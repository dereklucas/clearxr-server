/// Per-frame rendering: swapchains, render pass, pipeline.
///
/// Shared rendering infrastructure (pipeline, shaders, push constants, UBO)
/// is used by both the OpenXR path and the desktop window path.

use anyhow::Result;
use ash::vk;
#[cfg(all(feature = "xr", target_os = "windows"))]
use ash::vk::Handle; // for from_raw on non-dispatchable handles
#[cfg(all(feature = "xr", target_os = "windows"))]
use glam::Mat4;
#[cfg(all(feature = "xr", target_os = "windows"))]
use log::info;
#[cfg(all(feature = "xr", target_os = "windows"))]
use openxr as xr;

#[cfg(all(feature = "xr", target_os = "windows"))]
use crate::mirror_window::MirrorWindow;
use crate::vk_backend::VkBackend;

// ----------------------------------------------------------------
// Hand tracking UBO – must match HandUBO in scene.frag exactly.
// 52*16 + 16 + 7*2*16 = 1072 bytes.
// ----------------------------------------------------------------
#[repr(C)]
#[derive(Copy, Clone)]
pub struct HandData {
    pub joints: [[f32; 4]; 52],        // [0..25] = left hand, [26..51] = right hand
    pub active: [f32; 4],              // x=left_hand, y=right_hand, z=left_ctrl, w=right_ctrl
    pub ctrl_grip: [[f32; 4]; 2],      // [0]=left grip pos+radius, [1]=right grip pos+radius
    pub ctrl_aim_pos: [[f32; 4]; 2],   // [0]=left aim pos, [1]=right aim pos
    pub ctrl_aim_dir: [[f32; 4]; 2],   // [0]=left aim direction, [1]=right aim direction
    pub ctrl_inputs: [[f32; 4]; 2],    // [trigger, squeeze, thumbstick_x, thumbstick_y]
    pub ctrl_buttons: [[f32; 4]; 2],   // [btn1_touch, btn2_touch, stick_click, menu_click]
    pub ctrl_clicks: [[f32; 4]; 2],    // [btn1_click, btn2_click, 0, 0]  (A/B or X/Y click)
    pub ctrl_touches: [[f32; 4]; 2],   // [trigger_touch, squeeze_touch, thumbstick_touch, 0]
    pub ctrl_grip_right: [[f32; 4]; 2],// grip pose right vector
    pub ctrl_grip_up: [[f32; 4]; 2],   // grip pose up vector
}

impl Default for HandData {
    fn default() -> Self {
        Self {
            joints: [[0.0; 4]; 52],
            active: [0.0; 4],
            ctrl_grip: [[0.0; 4]; 2],
            ctrl_aim_pos: [[0.0; 4]; 2],
            ctrl_aim_dir: [[0.0; 4]; 2],
            ctrl_inputs: [[0.0; 4]; 2],
            ctrl_buttons: [[0.0; 4]; 2],
            ctrl_clicks: [[0.0; 4]; 2],
            ctrl_touches: [[0.0; 4]; 2],
            ctrl_grip_right: [[0.0; 4]; 2],
            ctrl_grip_up: [[0.0; 4]; 2],
        }
    }
}

// ----------------------------------------------------------------
// Shared UBO + descriptor set setup for HandData
// ----------------------------------------------------------------
pub struct HandUbo {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub mapped: *mut HandData,
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    pub descriptor_set: vk::DescriptorSet,
}

// Safety: mapped pointer is only used from the thread that owns HandUbo.
unsafe impl Send for HandUbo {}
unsafe impl Sync for HandUbo {}

impl HandUbo {
    pub fn new(vk: &VkBackend) -> Result<Self> {
        let device = vk.device();
        let ubo_size = std::mem::size_of::<HandData>() as vk::DeviceSize;

        let buf_ci = vk::BufferCreateInfo {
            size: ubo_size,
            usage: vk::BufferUsageFlags::UNIFORM_BUFFER,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };
        let buffer = unsafe { device.create_buffer(&buf_ci, None)? };
        let mem_reqs = unsafe { device.get_buffer_memory_requirements(buffer) };

        let mem_type_idx = vk
            .find_memory_type(
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or_else(|| anyhow::anyhow!("No suitable memory type for hand UBO"))?;

        let alloc_ci = vk::MemoryAllocateInfo {
            allocation_size: mem_reqs.size,
            memory_type_index: mem_type_idx,
            ..Default::default()
        };
        let memory = unsafe { device.allocate_memory(&alloc_ci, None)? };
        unsafe { device.bind_buffer_memory(buffer, memory, 0)? };

        let mapped = unsafe {
            device.map_memory(memory, 0, ubo_size, vk::MemoryMapFlags::empty())?
        } as *mut HandData;

        unsafe { std::ptr::write(mapped, HandData::default()) };

        // Descriptor set layout
        let binding = vk::DescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1,
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            ..Default::default()
        };
        let dsl_ci = vk::DescriptorSetLayoutCreateInfo {
            binding_count: 1,
            p_bindings: &binding,
            ..Default::default()
        };
        let descriptor_set_layout =
            unsafe { device.create_descriptor_set_layout(&dsl_ci, None)? };

        // Descriptor pool
        let pool_size = vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1,
        };
        let dp_ci = vk::DescriptorPoolCreateInfo {
            max_sets: 1,
            pool_size_count: 1,
            p_pool_sizes: &pool_size,
            ..Default::default()
        };
        let descriptor_pool = unsafe { device.create_descriptor_pool(&dp_ci, None)? };

        // Allocate descriptor set
        let ds_alloc = vk::DescriptorSetAllocateInfo {
            descriptor_pool,
            descriptor_set_count: 1,
            p_set_layouts: &descriptor_set_layout,
            ..Default::default()
        };
        let descriptor_set = unsafe { device.allocate_descriptor_sets(&ds_alloc) }?[0];

        // Write descriptor
        let buf_info = vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: ubo_size,
        };
        let write = vk::WriteDescriptorSet {
            dst_set: descriptor_set,
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
            p_buffer_info: &buf_info,
            ..Default::default()
        };
        unsafe { device.update_descriptor_sets(&[write], &[]) };

        Ok(Self {
            buffer,
            memory,
            mapped,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
        })
    }

    pub fn update(&mut self, data: &HandData) {
        unsafe { std::ptr::copy_nonoverlapping(data, self.mapped, 1) };
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.unmap_memory(self.memory);
            device.destroy_buffer(self.buffer, None);
            device.free_memory(self.memory, None);
        }
    }
}

// ----------------------------------------------------------------
// Push constants layout – must match scene.frag exactly.
// 80 bytes total; within the 128-byte Vulkan minimum guarantee.
// ----------------------------------------------------------------
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PushConstants {
    pub cam_pos:   [f32; 4], // xyz = world position,   w = time (seconds)
    pub cam_right: [f32; 4], // xyz = right vector,      w = eye index
    pub cam_up:    [f32; 4], // xyz = up vector,         w = unused
    pub cam_fwd:   [f32; 4], // xyz = forward vector,    w = unused
    pub fov:       [f32; 4], // x = tan(left), y = tan(right), z = tan(down), w = tan(up)
}

// ----------------------------------------------------------------
// Per-eye swapchain + its Vulkan resources (XR mode only)
// ----------------------------------------------------------------
#[cfg(all(feature = "xr", target_os = "windows"))]
pub struct EyeSwapchain {
    pub handle: xr::Swapchain<xr::Vulkan>,
    pub resolution: vk::Extent2D,
    pub images: Vec<vk::Image>,
    image_views: Vec<vk::ImageView>,
    // Depth
    pub depth_handle: Option<xr::Swapchain<xr::Vulkan>>,
    pub depth_images: Vec<vk::Image>,
    depth_image_views: Vec<vk::ImageView>,
    // Framebuffers (color + depth)
    framebuffers: Vec<vk::Framebuffer>,
}

// ----------------------------------------------------------------
// Top-level XR renderer
// ----------------------------------------------------------------
#[cfg(all(feature = "xr", target_os = "windows"))]
pub struct Renderer {
    pub swapchains: Vec<EyeSwapchain>, // 2 for stereo, 4 for quad (foveated)
    pub has_depth: bool,
    pub render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    // Hand tracking UBO resources
    hand_ubo: vk::Buffer,
    hand_ubo_memory: vk::DeviceMemory,
    hand_ubo_mapped: *mut HandData,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
}

// Safety: the mapped pointer is only used from the main thread that owns the Renderer.
#[cfg(all(feature = "xr", target_os = "windows"))]
unsafe impl Send for Renderer {}
#[cfg(all(feature = "xr", target_os = "windows"))]
unsafe impl Sync for Renderer {}

#[cfg(all(feature = "xr", target_os = "windows"))]
impl Renderer {
    pub fn new(
        vk: &VkBackend,
        session: &xr::Session<xr::Vulkan>,
        view_configs: &[xr::ViewConfigurationView],
        depth_enabled: bool,
    ) -> Result<Self> {
        let device = vk.device();

        // ---- Choose swapchain format ----------------------------------------
        let formats = session.enumerate_swapchain_formats()?;
        let preferred: &[u32] = &[
            vk::Format::R8G8B8A8_SRGB.as_raw() as u32,
            vk::Format::B8G8R8A8_SRGB.as_raw() as u32,
            vk::Format::R8G8B8A8_UNORM.as_raw() as u32,
            vk::Format::B8G8R8A8_UNORM.as_raw() as u32,
        ];
        let fmt: u32 = preferred
            .iter()
            .find(|&&f| formats.contains(&f))
            .copied()
            .unwrap_or(formats[0]);
        let vk_format = vk::Format::from_raw(fmt as i32);
        info!("Swapchain format: {:?}", vk_format);

        // ---- Choose depth format (if depth enabled) -------------------------
        let depth_fmt = if depth_enabled {
            let depth_preferred: &[u32] = &[
                vk::Format::D32_SFLOAT.as_raw() as u32,
                vk::Format::D16_UNORM.as_raw() as u32,
            ];
            match depth_preferred.iter().find(|&&f| formats.contains(&f)) {
                Some(&f) => {
                    info!("Depth format: {:?}", vk::Format::from_raw(f as i32));
                    Some(f)
                }
                None => {
                    info!("No supported depth format found, disabling depth.");
                    None
                }
            }
        } else { None };
        let has_depth = depth_fmt.is_some();

        // ---- Render pass ----------------------------------------------------
        let depth_vk_format = depth_fmt.map(|f| vk::Format::from_raw(f as i32));
        let render_pass = create_render_pass(device, vk_format, depth_vk_format)?;

        // ---- Hand UBO -------------------------------------------------------
        let ubo_size = std::mem::size_of::<HandData>() as vk::DeviceSize;

        let buf_ci = vk::BufferCreateInfo {
            size: ubo_size,
            usage: vk::BufferUsageFlags::UNIFORM_BUFFER,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };
        let hand_ubo = unsafe { device.create_buffer(&buf_ci, None)? };
        let mem_reqs = unsafe { device.get_buffer_memory_requirements(hand_ubo) };

        let mem_type_idx = vk
            .find_memory_type(
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or_else(|| anyhow::anyhow!("No suitable memory type for hand UBO"))?;

        let alloc_ci = vk::MemoryAllocateInfo {
            allocation_size: mem_reqs.size,
            memory_type_index: mem_type_idx,
            ..Default::default()
        };
        let hand_ubo_memory = unsafe { device.allocate_memory(&alloc_ci, None)? };
        unsafe { device.bind_buffer_memory(hand_ubo, hand_ubo_memory, 0)? };

        let hand_ubo_mapped = unsafe {
            device.map_memory(hand_ubo_memory, 0, ubo_size, vk::MemoryMapFlags::empty())?
        } as *mut HandData;

        // Zero-initialise the UBO
        unsafe {
            std::ptr::write(hand_ubo_mapped, HandData::default());
        }

        // ---- Descriptor set layout ------------------------------------------
        let binding = vk::DescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1,
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            ..Default::default()
        };
        let dsl_ci = vk::DescriptorSetLayoutCreateInfo {
            binding_count: 1,
            p_bindings: &binding,
            ..Default::default()
        };
        let descriptor_set_layout =
            unsafe { device.create_descriptor_set_layout(&dsl_ci, None)? };

        // ---- Descriptor pool ------------------------------------------------
        let pool_size = vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: 1,
        };
        let dp_ci = vk::DescriptorPoolCreateInfo {
            max_sets: 1,
            pool_size_count: 1,
            p_pool_sizes: &pool_size,
            ..Default::default()
        };
        let descriptor_pool = unsafe { device.create_descriptor_pool(&dp_ci, None)? };

        // ---- Allocate descriptor set ----------------------------------------
        let ds_alloc = vk::DescriptorSetAllocateInfo {
            descriptor_pool,
            descriptor_set_count: 1,
            p_set_layouts: &descriptor_set_layout,
            ..Default::default()
        };
        let descriptor_set = unsafe { device.allocate_descriptor_sets(&ds_alloc) }?[0];

        // ---- Write descriptor -----------------------------------------------
        let buf_info = vk::DescriptorBufferInfo {
            buffer: hand_ubo,
            offset: 0,
            range: ubo_size,
        };
        let write = vk::WriteDescriptorSet {
            dst_set: descriptor_set,
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
            p_buffer_info: &buf_info,
            ..Default::default()
        };
        unsafe { device.update_descriptor_sets(&[write], &[]) };

        // ---- Pipeline -------------------------------------------------------
        let (pipeline_layout, pipeline) =
            create_pipeline(device, render_pass, descriptor_set_layout, has_depth)?;

        // ---- Swapchains (one per view: 2 for stereo, 4 for quad) ----
        let mut swapchains = Vec::with_capacity(view_configs.len());
        for (i, vc) in view_configs.iter().enumerate() {
            swapchains.push(create_eye_swapchain(vk, session, vc, fmt, depth_fmt, render_pass, i)?);
        }

        // ---- Command buffer -------------------------------------------------
        let alloc_info = vk::CommandBufferAllocateInfo {
            command_pool: vk.command_pool,
            level: vk::CommandBufferLevel::PRIMARY,
            command_buffer_count: 1,
            ..Default::default()
        };
        let command_buffer = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];

        // ---- Fence ----------------------------------------------------------
        let fence =
            unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };

        Ok(Self {
            swapchains,
            has_depth,
            render_pass,
            pipeline_layout,
            pipeline,
            command_buffer,
            fence,
            hand_ubo,
            hand_ubo_memory,
            hand_ubo_mapped,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
        })
    }

    /// Render all views (2 for stereo, 4 for quad/foveated).
    /// If `mirror` is provided, the first view is blitted to the desktop window.
    /// If `panels` are provided, they are drawn as overlays on top of the scene.
    /// Panels with pending uploads will have their GPU transfers recorded before the render pass.
    pub fn render_frame(
        &mut self,
        vk: &VkBackend,
        views: &[xr::View],
        time: f32,
        mut mirror: Option<&mut MirrorWindow>,
        panels: &mut [&mut crate::launcher_panel::LauncherPanel],
    ) -> Result<()> {
        let device = vk.device();

        for (view_idx, sw) in self.swapchains.iter_mut().enumerate() {
            let image_idx = sw.handle.acquire_image()? as usize;
            sw.handle.wait_image(xr::Duration::INFINITE)?;

            // Acquire depth swapchain image in lockstep
            let _depth_image_idx = if let Some(ref mut dh) = sw.depth_handle {
                let di = dh.acquire_image()? as usize;
                dh.wait_image(xr::Duration::INFINITE)?;
                Some(di)
            } else { None };

            let push = build_push_constants(&views[view_idx], view_idx as u32, time);
            let res = sw.resolution;
            let fb = sw.framebuffers[image_idx];

            // Begin command buffer
            let begin_info = vk::CommandBufferBeginInfo {
                flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
                ..Default::default()
            };
            unsafe { device.begin_command_buffer(self.command_buffer, &begin_info)? };

            // Record pending panel texture uploads BEFORE the render pass
            // (only on view 0 — the transfers only need to happen once per frame)
            if view_idx == 0 {
                for p in panels.iter_mut() {
                    p.record_upload(device, self.command_buffer);
                }
            }

            // Begin render pass + draw scene
            record_render_pass_open(
                device,
                self.command_buffer,
                self.render_pass,
                self.pipeline,
                self.pipeline_layout,
                fb,
                res,
                &push,
                self.descriptor_set,
                self.has_depth,
            );

            // Draw panel overlays
            for p in panels.iter() {
                p.record_draw(device, self.command_buffer, &push);
            }

            // Close render pass + command buffer
            unsafe {
                device.cmd_end_render_pass(self.command_buffer);
                device.end_command_buffer(self.command_buffer)?;
            }

            let submit_info = vk::SubmitInfo {
                command_buffer_count: 1,
                p_command_buffers: &self.command_buffer,
                ..Default::default()
            };
            unsafe {
                device.reset_fences(&[self.fence])?;
                device.queue_submit(vk.queue(), &[submit_info], self.fence)?;
                device.wait_for_fences(&[self.fence], true, u64::MAX)?;
            }

            // Blit the first view (left eye) to the desktop mirror window
            if view_idx == 0 {
                if let Some(ref mut m) = mirror {
                    let src_image = sw.images[image_idx];
                    if let Err(e) = m.blit_and_present(device, vk.queue(), src_image, res) {
                        log::debug!("Mirror blit skipped: {}", e);
                    }
                }
            }

            sw.handle.release_image()?;
            if let Some(ref mut dh) = sw.depth_handle {
                dh.release_image()?;
            }
        }

        Ok(())
    }

    /// Copy hand joint data into the persistently-mapped UBO.
    pub fn update_hand_data(&mut self, data: &HandData) {
        unsafe {
            std::ptr::copy_nonoverlapping(data, self.hand_ubo_mapped, 1);
        }
    }

    pub fn destroy(&mut self, vk: &VkBackend) {
        let device = vk.device();
        unsafe {
            device.destroy_fence(self.fence, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            for sw in &self.swapchains {
                for &fb in &sw.framebuffers {
                    device.destroy_framebuffer(fb, None);
                }
                for &iv in &sw.image_views {
                    device.destroy_image_view(iv, None);
                }
                for &div in &sw.depth_image_views {
                    device.destroy_image_view(div, None);
                }
            }
            device.destroy_render_pass(self.render_pass, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.unmap_memory(self.hand_ubo_memory);
            device.destroy_buffer(self.hand_ubo, None);
            device.free_memory(self.hand_ubo_memory, None);
        }
    }
}

// ============================================================
// Build push constants from an XR view pose + fov
// ============================================================
#[cfg(all(feature = "xr", target_os = "windows"))]
fn build_push_constants(view: &xr::View, eye_idx: u32, time: f32) -> PushConstants {
    let q = view.pose.orientation;
    let rot = Mat4::from_quat(glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));

    let right   =  rot.col(0).truncate();
    let up      =  rot.col(1).truncate();
    let forward = -rot.col(2).truncate(); // OpenXR: -Z is forward

    let p = view.pose.position;
    let fov = view.fov;

    // Pass all four asymmetric FOV angles as tangents.
    // angle_left and angle_down are typically negative.
    PushConstants {
        cam_pos:   [p.x, p.y, p.z, time],
        cam_right: [right.x, right.y, right.z, eye_idx as f32],
        cam_up:    [up.x, up.y, up.z, 0.0],
        cam_fwd:   [forward.x, forward.y, forward.z, 0.0],
        fov:       [
            fov.angle_left.tan(),   // negative
            fov.angle_right.tan(),  // positive
            fov.angle_down.tan(),   // negative
            fov.angle_up.tan(),     // positive
        ],
    }
}

// ============================================================
// Record one fullscreen draw into the framebuffer
// ============================================================
pub fn record_commands(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    framebuffer: vk::Framebuffer,
    resolution: vk::Extent2D,
    push: &PushConstants,
    descriptor_set: vk::DescriptorSet,
    has_depth: bool,
) -> Result<()> {
    record_commands_open(device, cmd, render_pass, pipeline, layout, framebuffer, resolution, push, descriptor_set, has_depth)?;
    unsafe {
        device.cmd_end_render_pass(cmd);
        device.end_command_buffer(cmd)?;
    }
    Ok(())
}

/// Start render pass, draw the scene — but leave the render pass open.
/// The command buffer must already be in recording state.
/// Caller must call cmd_end_render_pass + end_command_buffer when done.
pub fn record_render_pass_open(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    framebuffer: vk::Framebuffer,
    resolution: vk::Extent2D,
    push: &PushConstants,
    descriptor_set: vk::DescriptorSet,
    has_depth: bool,
) {
    let mut clears = vec![
        vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } },
    ];
    if has_depth {
        clears.push(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } });
    }

    let rp_begin = vk::RenderPassBeginInfo {
        render_pass,
        framebuffer,
        render_area: vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: resolution,
        },
        clear_value_count: clears.len() as u32,
        p_clear_values: clears.as_ptr(),
        ..Default::default()
    };

    let viewport = vk::Viewport {
        x: 0.0, y: 0.0,
        width: resolution.width as f32,
        height: resolution.height as f32,
        min_depth: 0.0, max_depth: 1.0,
    };

    let scissor = vk::Rect2D {
        offset: vk::Offset2D::default(),
        extent: resolution,
    };

    let push_bytes = unsafe {
        std::slice::from_raw_parts(
            push as *const PushConstants as *const u8,
            std::mem::size_of::<PushConstants>(),
        )
    };

    unsafe {
        device.cmd_begin_render_pass(cmd, &rp_begin, vk::SubpassContents::INLINE);
        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        device.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, layout, 0, &[descriptor_set], &[]);
        device.cmd_set_viewport(cmd, 0, &[viewport]);
        device.cmd_set_scissor(cmd, 0, &[scissor]);
        device.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::FRAGMENT, 0, push_bytes);
        device.cmd_draw(cmd, 3, 1, 0, 0);
    }
}

/// Begin command buffer, start render pass, draw the scene — but leave the
/// render pass open so additional draws (e.g. launcher panel) can be appended.
/// Caller must call cmd_end_render_pass + end_command_buffer when done.
pub fn record_commands_open(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    framebuffer: vk::Framebuffer,
    resolution: vk::Extent2D,
    push: &PushConstants,
    descriptor_set: vk::DescriptorSet,
    has_depth: bool,
) -> Result<()> {
    let begin_info = vk::CommandBufferBeginInfo {
        flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
        ..Default::default()
    };

    let mut clears = vec![
        vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } },
    ];
    if has_depth {
        clears.push(vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } });
    }

    let rp_begin = vk::RenderPassBeginInfo {
        render_pass,
        framebuffer,
        render_area: vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: resolution,
        },
        clear_value_count: clears.len() as u32,
        p_clear_values: clears.as_ptr(),
        ..Default::default()
    };

    let viewport = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width:  resolution.width  as f32,
        height: resolution.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };

    let scissor = vk::Rect2D {
        offset: vk::Offset2D::default(),
        extent: resolution,
    };

    let push_bytes = unsafe {
        std::slice::from_raw_parts(
            push as *const PushConstants as *const u8,
            std::mem::size_of::<PushConstants>(),
        )
    };

    unsafe {
        device.begin_command_buffer(cmd, &begin_info)?;
        device.cmd_begin_render_pass(cmd, &rp_begin, vk::SubpassContents::INLINE);
        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        device.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            0,
            &[descriptor_set],
            &[],
        );
        device.cmd_set_viewport(cmd, 0, &[viewport]);
        device.cmd_set_scissor(cmd, 0, &[scissor]);
        device.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::FRAGMENT, 0, push_bytes);
        device.cmd_draw(cmd, 3, 1, 0, 0); // fullscreen triangle, no vertex buffer
    }
    Ok(())
}

// ============================================================
// Render pass
// ============================================================
pub fn create_render_pass(
    device: &ash::Device,
    color_format: vk::Format,
    depth_format: Option<vk::Format>,
) -> Result<vk::RenderPass> {
    create_render_pass_with_layout(device, color_format, depth_format, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
}

pub fn create_render_pass_with_layout(
    device: &ash::Device,
    color_format: vk::Format,
    depth_format: Option<vk::Format>,
    color_final_layout: vk::ImageLayout,
) -> Result<vk::RenderPass> {
    let color_attachment = vk::AttachmentDescription {
        format: color_format,
        samples: vk::SampleCountFlags::TYPE_1,
        load_op:  vk::AttachmentLoadOp::CLEAR,
        store_op: vk::AttachmentStoreOp::STORE,
        stencil_load_op:  vk::AttachmentLoadOp::DONT_CARE,
        stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        final_layout:   color_final_layout,
        ..Default::default()
    };

    let depth_attachment = vk::AttachmentDescription {
        format: depth_format.unwrap_or(vk::Format::D32_SFLOAT),
        samples: vk::SampleCountFlags::TYPE_1,
        load_op:  vk::AttachmentLoadOp::CLEAR,
        store_op: vk::AttachmentStoreOp::STORE,
        stencil_load_op:  vk::AttachmentLoadOp::DONT_CARE,
        stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        final_layout:   vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        ..Default::default()
    };

    let color_ref = vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    };
    let depth_ref = vk::AttachmentReference {
        attachment: 1,
        layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
    };

    let subpass = vk::SubpassDescription {
        pipeline_bind_point: vk::PipelineBindPoint::GRAPHICS,
        color_attachment_count: 1,
        p_color_attachments: &color_ref,
        p_depth_stencil_attachment: if depth_format.is_some() { &depth_ref } else { std::ptr::null() },
        ..Default::default()
    };

    let dependency = vk::SubpassDependency {
        src_subpass:     vk::SUBPASS_EXTERNAL,
        dst_subpass:     0,
        src_stage_mask:  vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                       | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
        dst_stage_mask:  vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                       | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
        src_access_mask: vk::AccessFlags::empty(),
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                       | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        ..Default::default()
    };

    let attachments = if depth_format.is_some() {
        vec![color_attachment, depth_attachment]
    } else {
        vec![color_attachment]
    };

    let rp_ci = vk::RenderPassCreateInfo {
        attachment_count: attachments.len() as u32,
        p_attachments: attachments.as_ptr(),
        subpass_count: 1,
        p_subpasses: &subpass,
        dependency_count: 1,
        p_dependencies: &dependency,
        ..Default::default()
    };

    Ok(unsafe { device.create_render_pass(&rp_ci, None)? })
}

// ============================================================
// SPIR-V shader module loader
// ============================================================
pub fn load_spv(device: &ash::Device, bytes: &[u8]) -> Result<vk::ShaderModule> {
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let ci = vk::ShaderModuleCreateInfo {
        code_size: words.len() * 4,
        p_code: words.as_ptr(),
        ..Default::default()
    };
    Ok(unsafe { device.create_shader_module(&ci, None)? })
}

// ============================================================
// Graphics pipeline
// ============================================================
pub fn create_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    descriptor_set_layout: vk::DescriptorSetLayout,
    depth_enabled: bool,
) -> Result<(vk::PipelineLayout, vk::Pipeline)> {
    let vert_spv = include_bytes!("../shaders/scene.vert.spv");
    let frag_spv = include_bytes!("../shaders/scene.frag.spv");

    let vert_module = load_spv(device, vert_spv)?;
    let frag_module = load_spv(device, frag_spv)?;

    let stages = [
        vk::PipelineShaderStageCreateInfo {
            stage: vk::ShaderStageFlags::VERTEX,
            module: vert_module,
            p_name: c"main".as_ptr(),
            ..Default::default()
        },
        vk::PipelineShaderStageCreateInfo {
            stage: vk::ShaderStageFlags::FRAGMENT,
            module: frag_module,
            p_name: c"main".as_ptr(),
            ..Default::default()
        },
    ];

    // 80 bytes of push constants in the fragment shader.
    let push_range = vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::FRAGMENT,
        offset: 0,
        size: std::mem::size_of::<PushConstants>() as u32,
    };
    let set_layouts = [descriptor_set_layout];
    let layout_ci = vk::PipelineLayoutCreateInfo {
        set_layout_count: set_layouts.len() as u32,
        p_set_layouts: set_layouts.as_ptr(),
        push_constant_range_count: 1,
        p_push_constant_ranges: &push_range,
        ..Default::default()
    };
    let pipeline_layout = unsafe { device.create_pipeline_layout(&layout_ci, None)? };

    // No vertex input: positions come from gl_VertexIndex in the vertex shader.
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo {
        topology: vk::PrimitiveTopology::TRIANGLE_LIST,
        ..Default::default()
    };

    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state = vk::PipelineDynamicStateCreateInfo {
        dynamic_state_count: dynamic_states.len() as u32,
        p_dynamic_states: dynamic_states.as_ptr(),
        ..Default::default()
    };

    let viewport_state = vk::PipelineViewportStateCreateInfo {
        viewport_count: 1,
        scissor_count: 1,
        ..Default::default()
    };

    let rasterizer = vk::PipelineRasterizationStateCreateInfo {
        polygon_mode: vk::PolygonMode::FILL,
        cull_mode: vk::CullModeFlags::NONE,
        front_face: vk::FrontFace::COUNTER_CLOCKWISE,
        line_width: 1.0,
        ..Default::default()
    };

    let multisample = vk::PipelineMultisampleStateCreateInfo {
        rasterization_samples: vk::SampleCountFlags::TYPE_1,
        ..Default::default()
    };

    let blend_attachment = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::FALSE,
        color_write_mask: vk::ColorComponentFlags::RGBA,
        ..Default::default()
    };
    let color_blend = vk::PipelineColorBlendStateCreateInfo {
        attachment_count: 1,
        p_attachments: &blend_attachment,
        ..Default::default()
    };

    // Depth-stencil: write depth from gl_FragDepth, no depth test (ray marcher computes depth)
    let depth_stencil = vk::PipelineDepthStencilStateCreateInfo {
        depth_test_enable:  if depth_enabled { vk::TRUE } else { vk::FALSE },
        depth_write_enable: if depth_enabled { vk::TRUE } else { vk::FALSE },
        depth_compare_op:   vk::CompareOp::ALWAYS, // always pass — the shader writes the depth
        ..Default::default()
    };

    let pipeline_ci = vk::GraphicsPipelineCreateInfo {
        stage_count: stages.len() as u32,
        p_stages: stages.as_ptr(),
        p_vertex_input_state: &vertex_input,
        p_input_assembly_state: &input_assembly,
        p_viewport_state: &viewport_state,
        p_rasterization_state: &rasterizer,
        p_multisample_state: &multisample,
        p_depth_stencil_state: &depth_stencil,
        p_color_blend_state: &color_blend,
        p_dynamic_state: &dynamic_state,
        layout: pipeline_layout,
        render_pass,
        subpass: 0,
        ..Default::default()
    };

    let pipeline = unsafe {
        device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_ci], None)
            .map_err(|(_, e)| anyhow::anyhow!("Pipeline creation failed: {:?}", e))?[0]
    };

    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    Ok((pipeline_layout, pipeline))
}

// ============================================================
// Per-eye swapchain + image views + framebuffers
// ============================================================
#[cfg(all(feature = "xr", target_os = "windows"))]
fn create_eye_swapchain(
    vk: &VkBackend,
    session: &xr::Session<xr::Vulkan>,
    config: &xr::ViewConfigurationView,
    color_format: u32,
    depth_format: Option<u32>,
    render_pass: vk::RenderPass,
    eye_idx: usize,
) -> Result<EyeSwapchain> {
    let device = vk.device();
    let w = config.recommended_image_rect_width;
    let h = config.recommended_image_rect_height;
    let resolution = vk::Extent2D { width: w, height: h };
    info!("Eye {} swapchain: {}×{}", eye_idx, w, h);

    // ---- Color swapchain ----
    let handle = session.create_swapchain(&xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
            | xr::SwapchainUsageFlags::SAMPLED
            | xr::SwapchainUsageFlags::TRANSFER_SRC,
        format: color_format,
        sample_count: 1,
        width: w, height: h,
        face_count: 1, array_size: 1, mip_count: 1,
    })?;

    let raw_images = handle.enumerate_images()?;
    let vk_color_format = vk::Format::from_raw(color_format as i32);
    let images: Vec<vk::Image> = raw_images.iter().map(|&r| vk::Image::from_raw(r)).collect();

    let mut image_views = Vec::with_capacity(images.len());
    for &image in &images {
        let iv = unsafe { device.create_image_view(&vk::ImageViewCreateInfo {
            image,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk_color_format,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                level_count: 1, layer_count: 1,
                ..Default::default()
            },
            ..Default::default()
        }, None)? };
        image_views.push(iv);
    }

    // ---- Depth swapchain (optional) ----
    let (depth_handle, depth_images, depth_image_views) = if let Some(dfmt) = depth_format {
        let dh = session.create_swapchain(&xr::SwapchainCreateInfo {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            format: dfmt,
            sample_count: 1,
            width: w, height: h,
            face_count: 1, array_size: 1, mip_count: 1,
        })?;
        let raw_depth = dh.enumerate_images()?;
        let vk_depth_format = vk::Format::from_raw(dfmt as i32);
        let dimages: Vec<vk::Image> = raw_depth.iter().map(|&r| vk::Image::from_raw(r)).collect();
        let mut divs = Vec::with_capacity(dimages.len());
        for &dimg in &dimages {
            let div = unsafe { device.create_image_view(&vk::ImageViewCreateInfo {
                image: dimg,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk_depth_format,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::DEPTH,
                    level_count: 1, layer_count: 1,
                    ..Default::default()
                },
                ..Default::default()
            }, None)? };
            divs.push(div);
        }
        info!("  Depth swapchain: {} images, {:?}", dimages.len(), vk_depth_format);
        (Some(dh), dimages, divs)
    } else {
        (None, Vec::new(), Vec::new())
    };

    // ---- Framebuffers (color + optional depth) ----
    // With depth, we pair color[i] with depth[i] (they should have the same count).
    let mut framebuffers = Vec::with_capacity(images.len());
    for i in 0..images.len() {
        let attachments: Vec<vk::ImageView> = if depth_format.is_some() && i < depth_image_views.len() {
            vec![image_views[i], depth_image_views[i]]
        } else {
            vec![image_views[i]]
        };
        let fb = unsafe { device.create_framebuffer(&vk::FramebufferCreateInfo {
            render_pass,
            attachment_count: attachments.len() as u32,
            p_attachments: attachments.as_ptr(),
            width: w, height: h, layers: 1,
            ..Default::default()
        }, None)? };
        framebuffers.push(fb);
    }

    Ok(EyeSwapchain {
        handle, resolution, images, image_views,
        depth_handle, depth_images, depth_image_views,
        framebuffers,
    })
}
