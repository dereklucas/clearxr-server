//! Headless GPU egui renderer.
//!
//! Creates its own Vulkan instance + device (independent of any app),
//! renders egui into an offscreen image, and reads back RGBA pixels.

use anyhow::Result;
use ash::vk;
use egui::{Context, Event, Pos2, PointerButton, RawInput, Rect, Vec2};
use std::mem::ManuallyDrop;

const MAX_TEXTURE_SIDE: usize = 2048;

pub struct HeadlessRenderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Offscreen render target
    render_image: vk::Image,
    render_image_memory: vk::DeviceMemory,
    render_image_view: vk::ImageView,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,

    // Readback staging buffer
    staging_buffer: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_ptr: *mut u8,

    width: u32,
    height: u32,
    pixel_size: usize,

    // egui
    ctx: Context,
    egui_renderer: ManuallyDrop<egui_ash_renderer::Renderer>,
    pointer_pos: Option<Pos2>,
    prev_button: bool,
    prev_secondary: bool,
    has_rendered: bool,
}

unsafe impl Send for HeadlessRenderer {}

impl HeadlessRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let entry = unsafe { ash::Entry::load()? };

        // Create minimal Vulkan instance (no extensions needed for offscreen).
        let app_info = vk::ApplicationInfo::default()
            .api_version(vk::make_api_version(0, 1, 0, 0));
        let instance_ci = vk::InstanceCreateInfo::default()
            .application_info(&app_info);
        let instance = unsafe { entry.create_instance(&instance_ci, None)? };

        // Pick first physical device.
        let physical_device = unsafe { instance.enumerate_physical_devices()? }
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No Vulkan physical device found"))?;

        // Find a graphics queue family.
        let queue_family = unsafe {
            instance.get_physical_device_queue_family_properties(physical_device)
        }
        .iter()
        .enumerate()
        .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|(i, _)| i as u32)
        .ok_or_else(|| anyhow::anyhow!("No graphics queue family found"))?;

        let queue_priorities = [1.0f32];
        let queue_ci = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&queue_priorities);
        let device_ci = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_ci));
        let device = unsafe { instance.create_device(physical_device, &device_ci, None)? };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        // Command pool + buffer + fence
        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&pool_ci, None)? };

        let alloc_ci = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = unsafe { device.allocate_command_buffers(&alloc_ci)? }[0];

        let fence = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?
        };

        // Offscreen render image (R8G8B8A8_UNORM for readback compatibility)
        let format = vk::Format::R8G8B8A8_UNORM;
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC);
        let render_image = unsafe { device.create_image(&image_ci, None)? };

        let mem_reqs = unsafe { device.get_image_memory_requirements(render_image) };
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let mem_type = find_memory_type(
            &mem_props,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or_else(|| anyhow::anyhow!("No suitable memory type for render image"))?;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type);
        let render_image_memory = unsafe { device.allocate_memory(&alloc_info, None)? };
        unsafe { device.bind_image_memory(render_image, render_image_memory, 0)? };

        let view_ci = vk::ImageViewCreateInfo::default()
            .image(render_image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        let render_image_view = unsafe { device.create_image_view(&view_ci, None)? };

        // Render pass
        let attachment = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let color_ref = vk::AttachmentReference::default()
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_ref));
        let rp_ci = vk::RenderPassCreateInfo::default()
            .attachments(std::slice::from_ref(&attachment))
            .subpasses(std::slice::from_ref(&subpass));
        let render_pass = unsafe { device.create_render_pass(&rp_ci, None)? };

        // Framebuffer
        let fb_ci = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(std::slice::from_ref(&render_image_view))
            .width(width)
            .height(height)
            .layers(1);
        let framebuffer = unsafe { device.create_framebuffer(&fb_ci, None)? };

        // Staging buffer for readback
        let pixel_size = (width * height * 4) as usize;
        let buf_ci = vk::BufferCreateInfo::default()
            .size(pixel_size as u64)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        let staging_buffer = unsafe { device.create_buffer(&buf_ci, None)? };

        let buf_reqs = unsafe { device.get_buffer_memory_requirements(staging_buffer) };
        let buf_mem_type = find_memory_type(
            &mem_props,
            buf_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .ok_or_else(|| anyhow::anyhow!("No suitable memory type for staging buffer"))?;

        let buf_alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(buf_reqs.size)
            .memory_type_index(buf_mem_type);
        let staging_memory = unsafe { device.allocate_memory(&buf_alloc, None)? };
        unsafe { device.bind_buffer_memory(staging_buffer, staging_memory, 0)? };

        let staging_ptr = unsafe {
            device.map_memory(staging_memory, 0, pixel_size as u64, vk::MemoryMapFlags::empty())?
                as *mut u8
        };

        // egui
        let ctx = Context::default();
        ctx.set_pixels_per_point(1.0);
        ctx.set_visuals(egui::Visuals::dark());

        let egui_renderer = egui_ash_renderer::Renderer::with_default_allocator(
            &instance,
            physical_device,
            device.clone(),
            render_pass,
            egui_ash_renderer::Options {
                srgb_framebuffer: false, // UNORM, not SRGB
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("egui-ash-renderer init failed: {e}"))?;

        log::info!(
            "[ClearXR Dashboard] Headless renderer initialized: {}x{}, Vulkan device ready",
            width, height
        );

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            command_pool,
            command_buffer,
            fence,
            render_image,
            render_image_memory,
            render_image_view,
            render_pass,
            framebuffer,
            staging_buffer,
            staging_memory,
            staging_ptr,
            width,
            height,
            pixel_size,
            ctx,
            egui_renderer: ManuallyDrop::new(egui_renderer),
            pointer_pos: None,
            prev_button: false,
            prev_secondary: false,
            has_rendered: false,
        })
    }

    /// Run one egui frame. Returns the RGBA pixel data if a repaint happened.
    pub fn render_frame(
        &mut self,
        pointer_uv: Option<(f32, f32)>,
        trigger: bool,
        secondary: bool,
        scroll_delta: f32,
        build_ui: impl FnMut(&Context),
    ) -> Result<Option<&[u8]>> {
        // Build input
        let mut raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            ..Default::default()
        };

        if let Some((u, v)) = pointer_uv {
            let pos = Pos2::new(u * self.width as f32, v * self.height as f32);
            self.pointer_pos = Some(pos);
            raw_input.events.push(Event::PointerMoved(pos));
            if trigger != self.prev_button {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: trigger,
                    modifiers: Default::default(),
                });
            }
            if secondary != self.prev_secondary {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: secondary,
                    modifiers: Default::default(),
                });
            }
        } else {
            self.pointer_pos = None;
        }
        self.prev_button = trigger;
        self.prev_secondary = secondary;

        if scroll_delta.abs() > 0.01 {
            raw_input.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, scroll_delta),
                modifiers: Default::default(),
            });
        }

        // Run egui
        let full_output = self.ctx.run(raw_input, build_ui);

        let needs_repaint = full_output
            .viewport_output
            .values()
            .any(|vo| vo.repaint_delay == std::time::Duration::ZERO);

        // Texture uploads
        let textures_delta = full_output.textures_delta;
        if !textures_delta.set.is_empty() {
            self.egui_renderer
                .set_textures(self.queue, self.command_pool, &textures_delta.set)
                .map_err(|e| anyhow::anyhow!("set_textures failed: {e}"))?;
        }

        if !needs_repaint && self.has_rendered {
            if !textures_delta.free.is_empty() {
                self.egui_renderer
                    .free_textures(&textures_delta.free)
                    .map_err(|e| anyhow::anyhow!("free_textures failed: {e}"))?;
            }
            return Ok(None);
        }

        // Tessellate
        let clipped_primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        // GPU render
        let device = &self.device;
        let cmd = self.command_buffer;

        unsafe {
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
            device.reset_fences(&[self.fence])?;

            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            // Transition to COLOR_ATTACHMENT
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(if self.has_rendered {
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL
                } else {
                    vk::ImageLayout::UNDEFINED
                })
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .image(self.render_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                )
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[], &[], &[barrier],
            );

            // Render pass
            let clear = vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] },
            };
            let rp_begin = vk::RenderPassBeginInfo::default()
                .render_pass(self.render_pass)
                .framebuffer(self.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width: self.width, height: self.height },
                })
                .clear_values(std::slice::from_ref(&clear));
            device.cmd_begin_render_pass(cmd, &rp_begin, vk::SubpassContents::INLINE);

            self.egui_renderer
                .cmd_draw(
                    cmd,
                    vk::Extent2D { width: self.width, height: self.height },
                    1.0,
                    &clipped_primitives,
                )
                .map_err(|e| anyhow::anyhow!("cmd_draw failed: {e}"))?;

            device.cmd_end_render_pass(cmd);

            // Transition to TRANSFER_SRC for readback
            let barrier2 = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(self.render_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                )
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[], &[], &[barrier2],
            );

            // Copy image to staging buffer
            let region = vk::BufferImageCopy::default()
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 });
            device.cmd_copy_image_to_buffer(
                cmd,
                self.render_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.staging_buffer,
                &[region],
            );

            device.end_command_buffer(cmd)?;

            let submit = vk::SubmitInfo::default()
                .command_buffers(std::slice::from_ref(&cmd));
            device.queue_submit(self.queue, &[submit], self.fence)?;
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        }

        // Free textures after render
        if !textures_delta.free.is_empty() {
            self.egui_renderer
                .free_textures(&textures_delta.free)
                .map_err(|e| anyhow::anyhow!("free_textures failed: {e}"))?;
        }

        self.has_rendered = true;

        // Return pointer to the mapped staging buffer
        let pixels = unsafe { std::slice::from_raw_parts(self.staging_ptr, self.pixel_size) };
        Ok(Some(pixels))
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for HeadlessRenderer {
    fn drop(&mut self) {
        unsafe {
            log::info!("[ClearXR Dashboard] HeadlessRenderer dropping...");
            self.device.device_wait_idle().ok();

            // CRITICAL: Drop the egui renderer FIRST — it uses the device internally.
            // If we destroy the device before the egui renderer drops, it's use-after-free.
            ManuallyDrop::drop(&mut self.egui_renderer);

            self.device.unmap_memory(self.staging_memory);
            self.device.destroy_buffer(self.staging_buffer, None);
            self.device.free_memory(self.staging_memory, None);
            self.device.destroy_framebuffer(self.framebuffer, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_image_view(self.render_image_view, None);
            self.device.destroy_image(self.render_image, None);
            self.device.free_memory(self.render_image_memory, None);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
            log::info!("[ClearXR Dashboard] HeadlessRenderer destroyed.");
        }
    }
}

fn find_memory_type(
    mem_props: &vk::PhysicalDeviceMemoryProperties,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..mem_props.memory_type_count).find(|&i| {
        (type_filter & (1 << i)) != 0
            && mem_props.memory_types[i as usize]
                .property_flags
                .contains(properties)
    })
}
