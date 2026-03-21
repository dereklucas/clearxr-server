/// Desktop mirror window – blits the left-eye VR view to a Win32 window.
///
/// Uses raw Win32 FFI (no external crate) + ash khr::surface / khr::swapchain.

use anyhow::Result;
use ash::vk;
use log::{info, warn};

// ============================================================
// Minimal Win32 FFI
// ============================================================

type HWND = isize;
type HINSTANCE = isize;
type LRESULT = isize;
type WPARAM = usize;
type LPARAM = isize;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF0000;
const WS_VISIBLE: u32 = 0x10000000;
const CW_USEDEFAULT: i32 = 0x80000000u32 as i32;
const PM_REMOVE: u32 = 0x0001;
const SW_SHOW: i32 = 5;
const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_QUIT: u32 = 0x0012;

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    wnd_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    cls_extra: i32,
    wnd_extra: i32,
    hinstance: HINSTANCE,
    hicon: isize,
    hcursor: isize,
    hbr_background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    hicon_sm: isize,
}

#[repr(C)]
#[derive(Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    time: u32,
    pt: POINT,
}

extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        ex_style: u32, class: *const u16, title: *const u16,
        style: u32, x: i32, y: i32, w: i32, h: i32,
        parent: HWND, menu: isize, inst: HINSTANCE, param: *const std::ffi::c_void,
    ) -> HWND;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const MSG) -> i32;
    fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    fn DefWindowProcW(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT;
    fn PostQuitMessage(exit_code: i32);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Encode a &str as a null-terminated UTF-16 Vec.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ============================================================
// MirrorWindow
// ============================================================

pub struct MirrorWindow {
    hwnd: HWND,
    surface: vk::SurfaceKHR,
    surface_fn: ash::khr::surface::Instance,
    swapchain: vk::SwapchainKHR,
    swapchain_fn: ash::khr::swapchain::Device,
    images: Vec<vk::Image>,
    extent: vk::Extent2D,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    should_close: bool,
    // Stored for swapchain recreation on resize
    physical_device: vk::PhysicalDevice,
    surface_format: vk::SurfaceFormatKHR,
    present_mode: vk::PresentModeKHR,
}

impl MirrorWindow {
    pub fn new(
        entry: &ash::Entry,
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: vk::PhysicalDevice,
        queue_family_index: u32,
        command_pool: vk::CommandPool,
    ) -> Result<Self> {
        // ---- Win32 window ----
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_name = wide("ClearXRMirror");

        let wc = WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            wnd_proc: Some(wnd_proc),
            cls_extra: 0,
            wnd_extra: 0,
            hinstance,
            hicon: 0,
            hcursor: 0,
            hbr_background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
            hicon_sm: 0,
        };
        unsafe { RegisterClassExW(&wc) };

        let title = wide("Clear XR Mirror");
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT, CW_USEDEFAULT, 1280, 720,
                0, 0, hinstance, std::ptr::null(),
            )
        };
        if hwnd == 0 {
            anyhow::bail!("Failed to create mirror window");
        }
        unsafe { ShowWindow(hwnd, SW_SHOW) };
        info!("Mirror window created.");

        // ---- Vulkan surface ----
        let win32_surface_fn = ash::khr::win32_surface::Instance::new(entry, instance);
        let surface_fn = ash::khr::surface::Instance::new(entry, instance);

        let surface_info = vk::Win32SurfaceCreateInfoKHR::default()
            .hinstance(hinstance as vk::HINSTANCE)
            .hwnd(hwnd as vk::HWND);

        let surface = unsafe { win32_surface_fn.create_win32_surface(&surface_info, None)? };

        let supported = unsafe {
            surface_fn.get_physical_device_surface_support(physical_device, queue_family_index, surface)?
        };
        if !supported {
            anyhow::bail!(
                "Queue family {} doesn't support presentation to mirror surface",
                queue_family_index
            );
        }

        // ---- Swapchain ----
        let caps = unsafe {
            surface_fn.get_physical_device_surface_capabilities(physical_device, surface)?
        };
        let formats = unsafe {
            surface_fn.get_physical_device_surface_formats(physical_device, surface)?
        };
        let present_modes = unsafe {
            surface_fn.get_physical_device_surface_present_modes(physical_device, surface)?
        };

        let format = formats
            .iter()
            .find(|f| {
                f.format == vk::Format::B8G8R8A8_SRGB
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or_else(|| {
                formats
                    .iter()
                    .find(|f| f.format == vk::Format::R8G8B8A8_SRGB)
            })
            .unwrap_or(&formats[0]);

        let present_mode = if present_modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else if present_modes.contains(&vk::PresentModeKHR::IMMEDIATE) {
            vk::PresentModeKHR::IMMEDIATE
        } else {
            vk::PresentModeKHR::FIFO
        };

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: 1280,
                height: 720,
            }
        };

        let image_count = (caps.min_image_count + 1).min(if caps.max_image_count > 0 {
            caps.max_image_count
        } else {
            u32::MAX
        });

        let swapchain_fn = ash::khr::swapchain::Device::new(instance, device);

        let sc_ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(present_mode)
            .clipped(true);

        let swapchain = unsafe { swapchain_fn.create_swapchain(&sc_ci, None)? };
        let images = unsafe { swapchain_fn.get_swapchain_images(swapchain)? };

        info!(
            "Mirror swapchain: {}x{}, {:?}, {} images, {:?}",
            extent.width,
            extent.height,
            format.format,
            images.len(),
            present_mode,
        );

        // ---- Command buffer + sync ----
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];

        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };
        let image_available =
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };
        let render_finished =
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };

        Ok(Self {
            hwnd,
            surface,
            surface_fn,
            swapchain,
            swapchain_fn,
            images,
            extent,
            command_buffer,
            fence,
            image_available,
            render_finished,
            should_close: false,
            physical_device,
            surface_format: *format,
            present_mode,
        })
    }

    /// Pump Win32 messages. Returns `false` if the window was closed.
    pub fn pump_events(&mut self) -> bool {
        let mut msg = MSG::default();
        // Use hwnd=0 so we also receive WM_QUIT (which is posted to the thread queue, not the window)
        while unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) } != 0 {
            if msg.message == WM_QUIT {
                info!("Mirror window closed by user.");
                self.should_close = true;
                return false;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        !self.should_close
    }

    pub fn is_closed(&self) -> bool {
        self.should_close
    }

    /// Recreate the swapchain after a resize or ERROR_OUT_OF_DATE_KHR.
    fn recreate_swapchain(&mut self, device: &ash::Device) -> Result<()> {
        unsafe { device.device_wait_idle()? };

        let caps = unsafe {
            self.surface_fn
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)?
        };

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            self.extent // keep old size as fallback
        };

        // Zero extent means minimized — skip recreation
        if extent.width == 0 || extent.height == 0 {
            return Ok(());
        }

        let image_count = (caps.min_image_count + 1).min(if caps.max_image_count > 0 {
            caps.max_image_count
        } else {
            u32::MAX
        });

        let old_swapchain = self.swapchain;

        let sc_ci = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(self.surface_format.format)
            .image_color_space(self.surface_format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(self.present_mode)
            .clipped(true)
            .old_swapchain(old_swapchain);

        self.swapchain = unsafe { self.swapchain_fn.create_swapchain(&sc_ci, None)? };
        self.images = unsafe { self.swapchain_fn.get_swapchain_images(self.swapchain)? };
        self.extent = extent;

        unsafe { self.swapchain_fn.destroy_swapchain(old_swapchain, None) };

        info!("Mirror swapchain recreated: {}x{}", extent.width, extent.height);
        Ok(())
    }

    /// Blit a VR eye image to the mirror window and present.
    ///
    /// The source image must be in `COLOR_ATTACHMENT_OPTIMAL` layout (as left
    /// by the XR render pass).  It is restored to that layout before return.
    pub fn blit_and_present(
        &mut self,
        device: &ash::Device,
        queue: vk::Queue,
        source_image: vk::Image,
        source_extent: vk::Extent2D,
    ) -> Result<()> {
        if self.should_close || self.extent.width == 0 {
            return Ok(());
        }

        // Acquire mirror swapchain image
        let (img_idx, suboptimal) = match unsafe {
            self.swapchain_fn.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available,
                vk::Fence::null(),
            )
        } {
            Ok(result) => result,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(device)?;
                return Ok(()); // skip this frame, next frame will use new swapchain
            }
            Err(e) => return Err(anyhow::anyhow!("Mirror acquire: {:?}", e)),
        };

        let dst_image = self.images[img_idx as usize];
        let cmd = self.command_buffer;

        let subresource = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        unsafe {
            device.begin_command_buffer(cmd, &begin)?;

            // Barriers: src → TRANSFER_SRC, dst → TRANSFER_DST
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[
                    vk::ImageMemoryBarrier::default()
                        .image(source_image)
                        .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .subresource_range(subresource),
                    vk::ImageMemoryBarrier::default()
                        .image(dst_image)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .subresource_range(subresource),
                ],
            );

            // Blit (handles scaling + format conversion)
            let blit = vk::ImageBlit {
                src_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                },
                src_offsets: [
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: source_extent.width as i32,
                        y: source_extent.height as i32,
                        z: 1,
                    },
                ],
                dst_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                },
                dst_offsets: [
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: self.extent.width as i32,
                        y: self.extent.height as i32,
                        z: 1,
                    },
                ],
            };

            device.cmd_blit_image(
                cmd,
                source_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );

            // Restore src → COLOR_ATTACHMENT, dst → PRESENT_SRC
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[
                    vk::ImageMemoryBarrier::default()
                        .image(source_image)
                        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .subresource_range(subresource),
                    vk::ImageMemoryBarrier::default()
                        .image(dst_image)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .subresource_range(subresource),
                ],
            );

            device.end_command_buffer(cmd)?;
        }

        // Submit: wait image_available, signal render_finished
        let wait_sems = [self.image_available];
        let wait_stages = [vk::PipelineStageFlags::TRANSFER];
        let cmds = [cmd];
        let signal_sems = [self.render_finished];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&wait_sems)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signal_sems);

        unsafe {
            device.reset_fences(&[self.fence])?;
            device.queue_submit(queue, &[submit], self.fence)?;
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        }

        // Present
        let present_sems = [self.render_finished];
        let swapchains = [self.swapchain];
        let indices = [img_idx];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&present_sems)
            .swapchains(&swapchains)
            .image_indices(&indices);

        let needs_recreate = suboptimal;
        match unsafe { self.swapchain_fn.queue_present(queue, &present) } {
            Ok(_) => {}
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR) => {
                self.recreate_swapchain(device)?;
                return Ok(());
            }
            Err(e) => warn!("Mirror present: {:?}", e),
        }

        if needs_recreate {
            self.recreate_swapchain(device)?;
        }

        Ok(())
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.device_wait_idle().ok();
            device.destroy_semaphore(self.render_finished, None);
            device.destroy_semaphore(self.image_available, None);
            device.destroy_fence(self.fence, None);
            self.swapchain_fn
                .destroy_swapchain(self.swapchain, None);
            self.surface_fn.destroy_surface(self.surface, None);
            if !self.should_close {
                // Window still alive (programmatic shutdown) — destroy it
                DestroyWindow(self.hwnd);
            }
        }
    }
}
