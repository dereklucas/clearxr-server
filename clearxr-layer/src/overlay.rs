//! Thin dashboard overlay — reads pre-rendered frames from shared memory,
//! uploads to a swapchain image, and appends a quad composition layer.
//! No egui, no rendering, no background threads.

use crate::{opaque::SpatialControllerPacket, vk_backend::VkBackend, NextDispatch};
use ash::{vk, vk::Handle};
use openxr_sys as xr;
use shared_memory::{Shmem, ShmemConf};
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

const SHM_NAME: &str = "ClearXR_Dashboard_Frame";
const HEADER_SIZE: usize = 64;
const PIPE_NAME: &str = r"\\.\pipe\ClearXR_Controller_Input";

/// Minimal SHM header (must match clearxr-dashboard/src/shm.rs).
#[repr(C)]
struct ShmHeader {
    frame_counter: AtomicU32,
    width: u32,
    height: u32,
    flags: u32,
    panel_pos: [f32; 3],
    panel_orient: [f32; 4],
    panel_size: [f32; 2],
    _reserved: [u8; 12],
}

// Safety: DashboardOverlay is only accessed from the thread that calls xrEndFrame.
// The raw pointers (staging_ptr, pipe handle) are not shared.
unsafe impl Send for DashboardOverlay {}

pub struct DashboardOverlay {
    session: xr::Session,
    swapchain: xr::Swapchain,
    space: xr::Space,
    width: u32,
    height: u32,
    images: Vec<vk::Image>,
    image_layouts: Vec<vk::ImageLayout>,
    // Staging buffer for pixel upload (persistently mapped)
    staging_buffer: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_ptr: *mut u8,
    pixel_size: usize,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    vk: VkBackend,
    // SHM reader
    shmem: Option<Shmem>,
    last_frame_counter: u32,
    // Pipe client for controller input
    #[cfg(target_os = "windows")]
    pipe: Option<windows_sys::Win32::Foundation::HANDLE>,
    // State
    visible: bool,
    menu_was_down: bool,
    pose: xr::Posef,
    size: xr::Extent2Df,
}

impl DashboardOverlay {
    pub unsafe fn new(
        next: &NextDispatch,
        session: xr::Session,
        binding: &xr::GraphicsBindingVulkanKHR,
    ) -> Result<Self, String> {
        let vk = VkBackend::from_graphics_binding(binding)
            .map_err(|e| format!("VkBackend failed: {e}"))?;

        // Try to open SHM (dashboard process may not have created it yet)
        let shmem = match ShmemConf::new().os_id(SHM_NAME).open() {
            Ok(s) => {
                log::info!("[ClearXR Layer] SHM opened: {}", SHM_NAME);
                Some(s)
            }
            Err(e) => {
                log::warn!("[ClearXR Layer] SHM not available yet: {e}");
                None
            }
        };

        // Read dimensions from SHM or use defaults
        let (width, height) = if let Some(ref s) = shmem {
            let header = &*(s.as_ptr() as *const ShmHeader);
            (header.width, header.height)
        } else {
            (2048, 1280)
        };

        let format = pick_swapchain_format(next, session)?;
        let swapchain = create_swapchain(next, session, format, width, height)?;
        let images = enumerate_swapchain_images(next, swapchain)?
            .into_iter()
            .map(|img| vk::Image::from_raw(img.image as usize as u64))
            .collect::<Vec<_>>();
        let space = create_local_space(next, session)?;

        // Create staging buffer for pixel upload
        let pixel_size = (width * height * 4) as usize;
        let (staging_buffer, staging_memory, staging_ptr) =
            create_staging_buffer(&vk, pixel_size)?;

        // Command buffer + fence
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(vk.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = vk.device().allocate_command_buffers(&alloc_info)
            .map_err(|e| format!("alloc cmd buf: {e}"))?[0];
        let fence = vk.device().create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        ).map_err(|e| format!("create fence: {e}"))?;

        // Try to connect pipe
        #[cfg(target_os = "windows")]
        let pipe = connect_pipe();

        let image_count = images.len();
        Ok(Self {
            session,
            swapchain,
            space,
            width,
            height,
            images,
            image_layouts: vec![vk::ImageLayout::UNDEFINED; image_count],
            staging_buffer,
            staging_memory,
            staging_ptr,
            pixel_size,
            command_buffer,
            fence,
            vk,
            shmem,
            last_frame_counter: 0,
            #[cfg(target_os = "windows")]
            pipe,
            visible: true,
            menu_was_down: false,
            pose: xr::Posef {
                orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                position: xr::Vector3f { x: 0.0, y: 1.5, z: -2.5 },
            },
            size: xr::Extent2Df { width: 1.6, height: 1.0 },
        })
    }

    pub fn is_for_session(&self, session: xr::Session) -> bool {
        self.session == session
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn update_menu_button(&mut self, menu_down: bool) -> bool {
        if menu_down && !self.menu_was_down {
            self.menu_was_down = menu_down;
            self.visible = !self.visible;
            return true;
        }
        self.menu_was_down = menu_down;
        false
    }

    /// Forward controller data to the dashboard process via named pipe.
    pub fn send_controller_input(&mut self, pkt: &SpatialControllerPacket) {
        static PIPE_LOG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let pipe_count = PIPE_LOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        #[cfg(target_os = "windows")]
        {
            if self.pipe.is_none() {
                self.pipe = connect_pipe();
                if pipe_count % 360 == 0 {
                    log::info!(
                        "[ClearXR Layer] Pipe connect attempt: {}",
                        if self.pipe.is_some() { "SUCCESS" } else { "not available yet" }
                    );
                }
            }
            if let Some(handle) = self.pipe {
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        pkt as *const SpatialControllerPacket as *const u8,
                        std::mem::size_of::<SpatialControllerPacket>(),
                    )
                };
                let mut written = 0u32;
                let ok = unsafe {
                    windows_sys::Win32::Storage::FileSystem::WriteFile(
                        handle,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        &mut written,
                        ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    // Pipe broken, try reconnecting next frame
                    unsafe {
                        windows_sys::Win32::Foundation::CloseHandle(handle);
                    }
                    self.pipe = None;
                }
            }
        }
    }

    pub fn quad_layer(&self) -> xr::CompositionLayerQuad {
        xr::CompositionLayerQuad {
            ty: xr::CompositionLayerQuad::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: self.space,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: xr::SwapchainSubImage {
                swapchain: self.swapchain,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di {
                        width: self.width as i32,
                        height: self.height as i32,
                    },
                },
                image_array_index: 0,
            },
            pose: self.pose,
            size: self.size,
        }
    }

    pub unsafe fn render_frame(&mut self, next: &NextDispatch) -> Result<(), String> {
        // Try to open SHM if not connected yet
        if self.shmem.is_none() {
            if let Ok(s) = ShmemConf::new().os_id(SHM_NAME).open() {
                log::info!("[ClearXR Layer] SHM connected.");
                self.shmem = Some(s);
            } else {
                return Ok(()); // Dashboard not running yet
            }
        }

        let shmem = self.shmem.as_ref().unwrap();
        let header = &*(shmem.as_ptr() as *const ShmHeader);

        // Check for new frame
        let counter = header.frame_counter.load(Ordering::Acquire);
        if counter == self.last_frame_counter {
            return Ok(()); // No new frame
        }
        self.last_frame_counter = counter;

        // Update pose from SHM header
        self.visible = header.flags & 1 != 0;
        self.pose.position.x = header.panel_pos[0];
        self.pose.position.y = header.panel_pos[1];
        self.pose.position.z = header.panel_pos[2];
        self.pose.orientation.x = header.panel_orient[0];
        self.pose.orientation.y = header.panel_orient[1];
        self.pose.orientation.z = header.panel_orient[2];
        self.pose.orientation.w = header.panel_orient[3];
        self.size.width = header.panel_size[0];
        self.size.height = header.panel_size[1];

        if !self.visible {
            return Ok(());
        }

        // Copy pixels from SHM → staging buffer
        let src = shmem.as_ptr().add(HEADER_SIZE);
        ptr::copy_nonoverlapping(src, self.staging_ptr, self.pixel_size);

        // Acquire swapchain image
        let mut image_index = 0;
        let r = (next.acquire_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageAcquireInfo { ty: xr::SwapchainImageAcquireInfo::TYPE, next: ptr::null() },
            &mut image_index,
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("AcquireSwapchainImage: {:?}", r));
        }
        let r = (next.wait_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageWaitInfo { ty: xr::SwapchainImageWaitInfo::TYPE, next: ptr::null(), timeout: xr::Duration::INFINITE },
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("WaitSwapchainImage: {:?}", r));
        }

        let idx = image_index as usize;
        let image = self.images[idx];
        let old_layout = self.image_layouts[idx];

        // Record: transition → copy buffer to image → done
        let device = self.vk.device();
        device.wait_for_fences(&[self.fence], true, u64::MAX)
            .map_err(|e| format!("wait fence: {e}"))?;
        device.reset_fences(&[self.fence])
            .map_err(|e| format!("reset fence: {e}"))?;

        let cmd = self.command_buffer;
        device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
            .map_err(|e| format!("begin cmd: {e}"))?;

        // Transition image to TRANSFER_DST
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1).layer_count(1))
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        device.cmd_pipeline_barrier(cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(), &[], &[], &[barrier]);

        // Copy staging buffer → image
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1))
            .image_extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 });
        device.cmd_copy_buffer_to_image(cmd, self.staging_buffer, image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]);

        // Transition to COLOR_ATTACHMENT (compositor expects this)
        let barrier2 = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1).layer_count(1))
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_READ);
        device.cmd_pipeline_barrier(cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(), &[], &[], &[barrier2]);

        device.end_command_buffer(cmd)
            .map_err(|e| format!("end cmd: {e}"))?;

        let submit = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        device.queue_submit(self.vk.queue(), &[submit], self.fence)
            .map_err(|e| format!("queue submit: {e}"))?;

        self.image_layouts[idx] = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;

        // Release swapchain image
        let r = (next.release_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageReleaseInfo { ty: xr::SwapchainImageReleaseInfo::TYPE, next: ptr::null() },
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("ReleaseSwapchainImage: {:?}", r));
        }

        Ok(())
    }

    pub unsafe fn destroy(&mut self, next: &NextDispatch) {
        let device = self.vk.device();
        device.device_wait_idle().ok();
        device.unmap_memory(self.staging_memory);
        device.destroy_buffer(self.staging_buffer, None);
        device.free_memory(self.staging_memory, None);
        device.destroy_fence(self.fence, None);
        if self.space != xr::Space::NULL {
            (next.destroy_space)(self.space);
        }
        if self.swapchain != xr::Swapchain::NULL {
            (next.destroy_swapchain)(self.swapchain);
        }
        #[cfg(target_os = "windows")]
        if let Some(h) = self.pipe {
            windows_sys::Win32::Foundation::CloseHandle(h);
        }
    }
}

impl Drop for DashboardOverlay {
    fn drop(&mut self) {
        unsafe {
            self.vk.device().device_wait_idle().ok();
            self.vk.device().unmap_memory(self.staging_memory);
            self.vk.device().destroy_buffer(self.staging_buffer, None);
            self.vk.device().free_memory(self.staging_memory, None);
            self.vk.device().destroy_fence(self.fence, None);
            self.vk.destroy_command_pool();
        }
    }
}

// ============================================================
// Helpers
// ============================================================

unsafe fn create_staging_buffer(
    vk: &VkBackend,
    size: usize,
) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8), String> {
    let device = vk.device();
    let buf_ci = vk::BufferCreateInfo::default()
        .size(size as u64)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC);
    let buffer = device.create_buffer(&buf_ci, None)
        .map_err(|e| format!("create staging buffer: {e}"))?;

    let mem_reqs = device.get_buffer_memory_requirements(buffer);
    let mem_type = vk.find_memory_type(
        mem_reqs.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ).ok_or("No HOST_VISIBLE memory type")?;

    let alloc = vk::MemoryAllocateInfo::default()
        .allocation_size(mem_reqs.size)
        .memory_type_index(mem_type);
    let memory = device.allocate_memory(&alloc, None)
        .map_err(|e| format!("alloc staging memory: {e}"))?;
    device.bind_buffer_memory(buffer, memory, 0)
        .map_err(|e| format!("bind staging memory: {e}"))?;

    let ptr = device.map_memory(memory, 0, size as u64, vk::MemoryMapFlags::empty())
        .map_err(|e| format!("map staging memory: {e}"))? as *mut u8;

    Ok((buffer, memory, ptr))
}

unsafe fn pick_swapchain_format(next: &NextDispatch, session: xr::Session) -> Result<vk::Format, String> {
    let mut count = 0;
    (next.enumerate_swapchain_formats)(session, 0, &mut count, ptr::null_mut());
    let mut formats = vec![0i64; count as usize];
    (next.enumerate_swapchain_formats)(session, count, &mut count, formats.as_mut_ptr());

    // Prefer UNORM since dashboard renders in UNORM
    let preferred = [
        vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_UNORM,
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_SRGB,
    ];
    Ok(preferred.iter().copied()
        .find(|f| formats.iter().any(|c| *c == f.as_raw() as i64))
        .unwrap_or(vk::Format::from_raw(formats[0] as i32)))
}

unsafe fn create_swapchain(
    next: &NextDispatch, session: xr::Session, format: vk::Format, width: u32, height: u32,
) -> Result<xr::Swapchain, String> {
    let ci = xr::SwapchainCreateInfo {
        ty: xr::SwapchainCreateInfo::TYPE, next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_DST,
        format: format.as_raw() as i64,
        sample_count: 1, width, height, face_count: 1, array_size: 1, mip_count: 1,
    };
    let mut swapchain = xr::Swapchain::NULL;
    let r = (next.create_swapchain)(session, &ci, &mut swapchain);
    if r != xr::Result::SUCCESS {
        return Err(format!("CreateSwapchain: {:?}", r));
    }
    Ok(swapchain)
}

unsafe fn enumerate_swapchain_images(
    next: &NextDispatch, swapchain: xr::Swapchain,
) -> Result<Vec<xr::SwapchainImageVulkanKHR>, String> {
    let mut count = 0;
    (next.enumerate_swapchain_images)(swapchain, 0, &mut count, ptr::null_mut());
    let mut images: Vec<MaybeUninit<xr::SwapchainImageVulkanKHR>> = (0..count)
        .map(|_| xr::SwapchainImageVulkanKHR::out(ptr::null_mut()))
        .collect();
    (next.enumerate_swapchain_images)(swapchain, count, &mut count,
        images.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader);
    Ok(images.into_iter().map(|i| i.assume_init()).collect())
}

unsafe fn create_local_space(next: &NextDispatch, session: xr::Session) -> Result<xr::Space, String> {
    let ci = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE, next: ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::LOCAL,
        pose_in_reference_space: xr::Posef {
            orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
            position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
        },
    };
    let mut space = xr::Space::NULL;
    let r = (next.create_reference_space)(session, &ci, &mut space);
    if r != xr::Result::SUCCESS {
        return Err(format!("CreateReferenceSpace: {:?}", r));
    }
    Ok(space)
}

#[cfg(target_os = "windows")]
fn connect_pipe() -> Option<windows_sys::Win32::Foundation::HANDLE> {
    let name: Vec<u8> = format!("{}\0", PIPE_NAME).into_bytes();
    let handle = unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateFileA(
            name.as_ptr(),
            0x40000000, // GENERIC_WRITE
            0, // no sharing
            ptr::null(),
            3, // OPEN_EXISTING
            0,
            ptr::null_mut(), // no template
        )
    };
    if handle == -1isize as windows_sys::Win32::Foundation::HANDLE {
        None
    } else {
        log::info!("[ClearXR Layer] Pipe connected: {}", PIPE_NAME);
        Some(handle)
    }
}
