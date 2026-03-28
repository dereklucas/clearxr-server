//! Headless GPU egui renderer with shared Vulkan image output.
//!
//! Creates its own Vulkan instance + device (independent of any app),
//! renders egui into an offscreen image exported via named Win32 handles
//! so the OpenXR layer can import it without GPU readback.

use anyhow::Result;
use ash::vk;
use egui::{Context, Event, Pos2, PointerButton, RawInput, Rect, Vec2};
use std::mem::ManuallyDrop;

// Named Win32 handles for cross-process sharing (UTF-16 with null terminator).
const IMAGE_HANDLE_NAME: &[u16] = &[
    b'C' as u16, b'l' as u16, b'e' as u16, b'a' as u16, b'r' as u16,
    b'X' as u16, b'R' as u16, b'_' as u16, b'D' as u16, b'a' as u16,
    b's' as u16, b'h' as u16, b'b' as u16, b'o' as u16, b'a' as u16,
    b'r' as u16, b'd' as u16, b'I' as u16, b'm' as u16, b'a' as u16,
    b'g' as u16, b'e' as u16, 0,
];
const SEMAPHORE_HANDLE_NAME: &[u16] = &[
    b'C' as u16, b'l' as u16, b'e' as u16, b'a' as u16, b'r' as u16,
    b'X' as u16, b'R' as u16, b'_' as u16, b'D' as u16, b'a' as u16,
    b's' as u16, b'h' as u16, b'b' as u16, b'o' as u16, b'a' as u16,
    b'r' as u16, b'd' as u16, b'S' as u16, b'e' as u16, b'm' as u16,
    b'a' as u16, b'p' as u16, b'h' as u16, b'o' as u16, b'r' as u16,
    b'e' as u16, 0,
];

/// User-managed Vulkan texture for the desktop capture image.
/// Bypasses egui-ash-renderer's set_textures (which does vkQueueWaitIdle)
/// by uploading directly via our own staging buffer + command buffer.
struct DesktopTexture {
    image: vk::Image,
    image_memory: vk::DeviceMemory,
    image_view: vk::ImageView,
    sampler: vk::Sampler,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    staging_buffer: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_ptr: *mut u8,
    width: u32,
    height: u32,
    texture_id: egui::TextureId,
    needs_upload: bool,
    initialized: bool,
}

/// Per-flight-frame GPU resources for double-buffering.
struct FlightFrame {
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Textures to free after this flight's fence is waited on.
    /// Deferred because free_textures destroys GPU resources that the
    /// in-flight command buffer may still reference.
    pending_free: Vec<egui::TextureId>,
}

pub struct HeadlessRenderer {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,

    // Double-buffered flight frames (eliminates GPU/CPU serialization)
    flights: [FlightFrame; 2],
    current_flight: usize,

    // Offscreen render target (exported via named Win32 handle)
    render_image: vk::Image,
    render_image_memory: vk::DeviceMemory,
    render_image_view: vk::ImageView,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,

    // Shared timeline semaphore for cross-process synchronization
    timeline_semaphore: vk::Semaphore,
    semaphore_counter: u64,

    width: u32,
    height: u32,

    // egui
    ctx: Context,
    egui_renderer: ManuallyDrop<egui_ash_renderer::Renderer>,
    pointer_pos: Option<Pos2>,
    prev_button: bool,
    prev_secondary: bool,
    has_rendered: bool,

    // User-managed desktop texture (bypasses set_textures bottleneck)
    desktop_texture: Option<DesktopTexture>,
}

unsafe impl Send for HeadlessRenderer {}

impl HeadlessRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let entry = unsafe { ash::Entry::load()? };

        // ── Instance (with external memory/semaphore capability extensions) ──
        let app_info = vk::ApplicationInfo::default()
            .api_version(vk::make_api_version(0, 1, 2, 0));
        let instance_extensions: [*const std::ffi::c_char; 3] = [
            vk::KHR_EXTERNAL_MEMORY_CAPABILITIES_NAME.as_ptr(),
            vk::KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_NAME.as_ptr(),
            vk::KHR_GET_PHYSICAL_DEVICE_PROPERTIES2_NAME.as_ptr(),
        ];
        let instance_ci = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&instance_extensions);
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

        // ── Device (with external memory/semaphore + timeline semaphore extensions) ──
        let device_extensions: [*const std::ffi::c_char; 5] = [
            vk::KHR_EXTERNAL_MEMORY_NAME.as_ptr(),
            vk::KHR_EXTERNAL_MEMORY_WIN32_NAME.as_ptr(),
            vk::KHR_EXTERNAL_SEMAPHORE_NAME.as_ptr(),
            vk::KHR_EXTERNAL_SEMAPHORE_WIN32_NAME.as_ptr(),
            vk::KHR_TIMELINE_SEMAPHORE_NAME.as_ptr(),
        ];

        let mut timeline_features = vk::PhysicalDeviceTimelineSemaphoreFeatures::default()
            .timeline_semaphore(true);

        let queue_priorities = [1.0f32];
        let queue_ci = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&queue_priorities);
        let device_ci = vk::DeviceCreateInfo::default()
            .queue_create_infos(std::slice::from_ref(&queue_ci))
            .enabled_extension_names(&device_extensions)
            .push_next(&mut timeline_features);
        let device = unsafe { instance.create_device(physical_device, &device_ci, None)? };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

        // Command pool + double-buffered flight frames
        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { device.create_command_pool(&pool_ci, None)? };

        let alloc_ci = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(2);
        let command_buffers = unsafe { device.allocate_command_buffers(&alloc_ci)? };

        let fence0 = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?
        };
        let fence1 = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?
        };

        // ── Offscreen render image (exported via named Win32 handle) ──
        let format = vk::Format::R8G8B8A8_UNORM;

        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .push_next(&mut external_image_info);
        let render_image = unsafe { device.create_image(&image_ci, None)? };

        let mem_reqs = unsafe { device.get_image_memory_requirements(render_image) };
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let mem_type = find_memory_type(
            &mem_props,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or_else(|| anyhow::anyhow!("No suitable memory type for render image"))?;

        // Export memory with a named Win32 handle + dedicated allocation (required for external memory).
        let mut export_win32_info = vk::ExportMemoryWin32HandleInfoKHR::default()
            .name(IMAGE_HANDLE_NAME.as_ptr());
        let mut export_mem_info = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default()
            .image(render_image);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type)
            .push_next(&mut dedicated_info)
            .push_next(&mut export_mem_info)
            .push_next(&mut export_win32_info);
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

        // ── Timeline semaphore (exported via named Win32 handle) ──
        let mut sem_type_info = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let mut export_sem_info = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32);
        let mut export_sem_win32_info = vk::ExportSemaphoreWin32HandleInfoKHR::default()
            .name(SEMAPHORE_HANDLE_NAME.as_ptr());
        let sem_ci = vk::SemaphoreCreateInfo::default()
            .push_next(&mut sem_type_info)
            .push_next(&mut export_sem_info)
            .push_next(&mut export_sem_win32_info);
        let timeline_semaphore = unsafe { device.create_semaphore(&sem_ci, None)? };

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

        let flights = [
            FlightFrame {
                command_buffer: command_buffers[0],
                fence: fence0,
                pending_free: Vec::new(),
            },
            FlightFrame {
                command_buffer: command_buffers[1],
                fence: fence1,
                pending_free: Vec::new(),
            },
        ];

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
                // true: shader skips manual LINEARtoSRGB, so sRGB desktop pixels
                // pass through without double-encoding (fixes gamma crush).
                srgb_framebuffer: true,
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("egui-ash-renderer init failed: {e}"))?;

        log::info!(
            "[ClearXR Dashboard] Headless renderer initialized: {}x{}, Vulkan 1.2, shared image + timeline semaphore",
            width, height
        );

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            command_pool,
            flights,
            current_flight: 0,
            render_image,
            render_image_memory,
            render_image_view,
            render_pass,
            framebuffer,
            timeline_semaphore,
            semaphore_counter: 0,
            width,
            height,
            ctx,
            egui_renderer: ManuallyDrop::new(egui_renderer),
            pointer_pos: None,
            prev_button: false,
            prev_secondary: false,
            has_rendered: false,
            desktop_texture: None,
        })
    }

    /// Run one egui frame. Returns `Ok(true)` if a new frame was rendered and
    /// the timeline semaphore was signaled, `Ok(false)` if no repaint was needed.
    ///
    /// The layer imports the shared image + timeline semaphore by name and waits
    /// on `semaphore_counter()` before sampling.
    pub fn render_frame(
        &mut self,
        pointer_uv: Option<(f32, f32)>,
        trigger: bool,
        secondary: bool,
        scroll_delta: f32,
        build_ui: impl FnMut(&Context),
    ) -> Result<bool> {
        // 1. Build egui input
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
            if self.pointer_pos.is_some() {
                raw_input.events.push(Event::PointerGone);
            }
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

        // 2. Run egui
        let dot_pos = self.pointer_pos;
        let mut build_ui = build_ui;
        let full_output = self.ctx.run(raw_input, |ctx| {
            build_ui(ctx);

            // Draw pointer dot overlay
            if let Some(pos) = dot_pos {
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("pointer_dot"),
                ));
                painter.circle_filled(pos, 6.0, egui::Color32::from_rgba_premultiplied(74, 158, 255, 200));
                painter.circle_stroke(pos, 7.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
            }
        });

        let needs_repaint = full_output
            .viewport_output
            .values()
            .any(|vo| vo.repaint_delay == std::time::Duration::ZERO);

        // 3. Texture uploads — only font atlas now (desktop texture bypasses this)
        let textures_delta = full_output.textures_delta;
        if !textures_delta.set.is_empty() {
            let t0 = std::time::Instant::now();
            let count = textures_delta.set.len();
            self.egui_renderer
                .set_textures(self.queue, self.command_pool, &textures_delta.set)
                .map_err(|e| anyhow::anyhow!("set_textures failed: {e}"))?;
            let elapsed = t0.elapsed();
            if elapsed.as_millis() > 2 {
                log::warn!(
                    "[ClearXR Dashboard] set_textures took {:.1}ms for {} textures",
                    elapsed.as_secs_f64() * 1000.0, count
                );
            }
        }

        // 4. Skip GPU work if no repaint needed
        if !needs_repaint && self.has_rendered {
            // No GPU work submitted this frame, so free_textures is safe immediately.
            if !textures_delta.free.is_empty() {
                self.egui_renderer
                    .free_textures(&textures_delta.free)
                    .map_err(|e| anyhow::anyhow!("free_textures failed: {e}"))?;
            }
            return Ok(false);
        }

        // 5. Tessellate
        let clipped_primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        // 6. GPU render into current flight frame
        let slot = self.current_flight;
        let device = &self.device;

        unsafe {
            device.wait_for_fences(&[self.flights[slot].fence], true, u64::MAX)?;

            // Free textures deferred from the previous use of this flight slot
            if !self.flights[slot].pending_free.is_empty() {
                let to_free: Vec<_> = std::mem::take(&mut self.flights[slot].pending_free);
                self.egui_renderer
                    .free_textures(&to_free)
                    .map_err(|e| anyhow::anyhow!("free_textures failed: {e}"))?;
            }

            device.reset_fences(&[self.flights[slot].fence])?;

            let cmd = self.flights[slot].command_buffer;

            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            // Upload desktop texture (batched into this command buffer — no vkQueueWaitIdle)
            if let Some(ref mut dt) = self.desktop_texture {
                Self::record_desktop_upload(device, dt, cmd);
            }

            // Transition render image to COLOR_ATTACHMENT_OPTIMAL
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(if self.has_rendered {
                    vk::ImageLayout::GENERAL
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

            // Transition render image to GENERAL for cross-process sampling
            let barrier2 = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(self.render_image)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                )
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ);
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[], &[], &[barrier2],
            );

            device.end_command_buffer(cmd)?;

            // Signal the timeline semaphore with incremented counter
            self.semaphore_counter += 1;
            let signal_values = [self.semaphore_counter];
            let mut timeline_info = vk::TimelineSemaphoreSubmitInfo::default()
                .signal_semaphore_values(&signal_values);
            let signal_semaphores = [self.timeline_semaphore];
            let submit = vk::SubmitInfo::default()
                .command_buffers(std::slice::from_ref(&cmd))
                .signal_semaphores(&signal_semaphores)
                .push_next(&mut timeline_info);
            device.queue_submit(self.queue, &[submit], self.flights[slot].fence)?;
        }

        // Defer texture frees until this flight's fence is waited on (next use of this slot).
        // The GPU may still be referencing these textures in the just-submitted command buffer.
        self.flights[slot].pending_free = textures_delta.free;

        self.has_rendered = true;
        self.current_flight = 1 - self.current_flight;

        Ok(true)
    }

    /// Current timeline semaphore value. The layer waits for this value before
    /// sampling the shared image.
    pub fn semaphore_counter(&self) -> u64 {
        self.semaphore_counter
    }

    /// Upload desktop capture pixels to a user-managed Vulkan texture.
    /// Bypasses egui's set_textures entirely — no vkQueueWaitIdle, no per-pixel copy.
    /// Returns the TextureId for use in egui UI code.
    pub fn update_desktop_pixels(&mut self, data: &[u8], width: u32, height: u32) -> egui::TextureId {
        let pixel_count = (width * height * 4) as usize;
        assert_eq!(data.len(), pixel_count, "desktop pixel data size mismatch");

        // Recreate if size changed or first call
        if self.desktop_texture.as_ref().map_or(true, |dt| dt.width != width || dt.height != height) {
            // Clean up old
            if let Some(old) = self.desktop_texture.take() {
                self.egui_renderer.remove_user_texture(old.texture_id);
                unsafe { self.destroy_desktop_texture_resources(&old); }
            }
            // Create new
            let dt = unsafe { self.create_desktop_texture(width, height) }
                .expect("Failed to create desktop texture");
            log::info!(
                "[ClearXR Dashboard] Desktop texture created: {}x{}, TextureId={:?}",
                width, height, dt.texture_id
            );
            self.desktop_texture = Some(dt);
        }

        let dt = self.desktop_texture.as_mut().unwrap();

        // Copy pixels to persistently-mapped staging buffer (fast CPU memcpy)
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), dt.staging_ptr, pixel_count);
        }
        dt.needs_upload = true;

        dt.texture_id
    }

    /// Get the desktop texture's TextureId (if created).
    pub fn desktop_texture_id(&self) -> Option<egui::TextureId> {
        self.desktop_texture.as_ref().map(|dt| dt.texture_id)
    }

    /// Record GPU commands to upload the desktop staging buffer to the image.
    /// Called inside render_frame's command buffer recording, BEFORE the render pass.
    unsafe fn record_desktop_upload(device: &ash::Device, dt: &mut DesktopTexture, cmd: vk::CommandBuffer) {
        if !dt.needs_upload { return; }

        // Transition to TRANSFER_DST
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(if dt.initialized {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(dt.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )
            .src_access_mask(if dt.initialized {
                vk::AccessFlags::SHADER_READ
            } else {
                vk::AccessFlags::empty()
            })
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        device.cmd_pipeline_barrier(
            cmd,
            if dt.initialized { vk::PipelineStageFlags::FRAGMENT_SHADER }
            else { vk::PipelineStageFlags::TOP_OF_PIPE },
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[], &[], &[barrier],
        );

        // Copy staging buffer -> image
        let region = vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D { width: dt.width, height: dt.height, depth: 1 });
        device.cmd_copy_buffer_to_image(
            cmd, dt.staging_buffer, dt.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region],
        );

        // Transition to SHADER_READ_ONLY for sampling during egui render
        let barrier2 = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image(dt.image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            )
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[], &[], &[barrier2],
        );

        dt.initialized = true;
        dt.needs_upload = false;
    }

    unsafe fn create_desktop_texture(&mut self, width: u32, height: u32) -> Result<DesktopTexture> {
        let device = &self.device;
        let pixel_size = (width * height * 4) as usize;

        // Image (DEVICE_LOCAL, for sampling in egui render pass)
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width, height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED);
        let image = device.create_image(&image_ci, None)?;

        let mem_reqs = device.get_image_memory_requirements(image);
        let mem_props = self.instance.get_physical_device_memory_properties(self.physical_device);
        let mem_type = find_memory_type(&mem_props, mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .ok_or_else(|| anyhow::anyhow!("No DEVICE_LOCAL memory for desktop texture"))?;
        let image_memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default().allocation_size(mem_reqs.size).memory_type_index(mem_type), None)?;
        device.bind_image_memory(image, image_memory, 0)?;

        let image_view = device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                ),
            None,
        )?;

        // Sampler (LINEAR filtering, matching egui TextureOptions::LINEAR)
        let sampler = device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .mipmap_mode(vk::SamplerMipmapMode::LINEAR),
            None,
        )?;

        // Descriptor set (compatible with egui-ash-renderer's pipeline layout)
        let descriptor_set_layout = egui_ash_renderer::vulkan::create_vulkan_descriptor_set_layout(device)
            .map_err(|e| anyhow::anyhow!("desktop texture DSL: {e}"))?;
        let descriptor_pool = egui_ash_renderer::vulkan::create_vulkan_descriptor_pool(device, 1)
            .map_err(|e| anyhow::anyhow!("desktop texture pool: {e}"))?;
        let descriptor_set = egui_ash_renderer::vulkan::create_vulkan_descriptor_set(
            device, descriptor_set_layout, descriptor_pool, image_view, sampler,
        ).map_err(|e| anyhow::anyhow!("desktop texture descriptor set: {e}"))?;

        // Staging buffer (HOST_VISIBLE, persistently mapped)
        let buf_ci = vk::BufferCreateInfo::default()
            .size(pixel_size as u64)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        let staging_buffer = device.create_buffer(&buf_ci, None)?;
        let buf_reqs = device.get_buffer_memory_requirements(staging_buffer);
        let buf_mem_type = find_memory_type(
            &mem_props, buf_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ).ok_or_else(|| anyhow::anyhow!("No HOST_VISIBLE memory for desktop staging"))?;
        let staging_memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default().allocation_size(buf_reqs.size).memory_type_index(buf_mem_type), None)?;
        device.bind_buffer_memory(staging_buffer, staging_memory, 0)?;
        let staging_ptr = device.map_memory(staging_memory, 0, pixel_size as u64, vk::MemoryMapFlags::empty())?
            as *mut u8;

        // Register with egui-ash-renderer as a user texture
        let texture_id = self.egui_renderer.add_user_texture(descriptor_set);

        Ok(DesktopTexture {
            image, image_memory, image_view, sampler,
            descriptor_set_layout, descriptor_pool, descriptor_set,
            staging_buffer, staging_memory, staging_ptr,
            width, height, texture_id,
            needs_upload: false, initialized: false,
        })
    }

    unsafe fn destroy_desktop_texture_resources(&self, dt: &DesktopTexture) {
        let device = &self.device;
        device.unmap_memory(dt.staging_memory);
        device.destroy_buffer(dt.staging_buffer, None);
        device.free_memory(dt.staging_memory, None);
        device.destroy_descriptor_pool(dt.descriptor_pool, None);
        device.destroy_descriptor_set_layout(dt.descriptor_set_layout, None);
        device.destroy_sampler(dt.sampler, None);
        device.destroy_image_view(dt.image_view, None);
        device.destroy_image(dt.image, None);
        device.free_memory(dt.image_memory, None);
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

            // Destroy desktop texture before egui renderer (it holds a user texture ref)
            if let Some(ref dt) = self.desktop_texture {
                self.destroy_desktop_texture_resources(dt);
            }

            // CRITICAL: Drop the egui renderer FIRST — it uses the device internally.
            ManuallyDrop::drop(&mut self.egui_renderer);

            // Destroy timeline semaphore (named Win32 handle is reference-counted by the OS)
            self.device.destroy_semaphore(self.timeline_semaphore, None);

            // Destroy both flight frames (no staging buffers — only fences)
            for flight in &self.flights {
                self.device.destroy_fence(flight.fence, None);
            }

            self.device.destroy_framebuffer(self.framebuffer, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_image_view(self.render_image_view, None);
            self.device.destroy_image(self.render_image, None);
            self.device.free_memory(self.render_image_memory, None);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wide_string_image_handle_name() {
        let name: String = IMAGE_HANDLE_NAME.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from_u32(c as u32).unwrap())
            .collect();
        assert_eq!(name, "ClearXR_DashboardImage");
        assert_eq!(*IMAGE_HANDLE_NAME.last().unwrap(), 0u16, "must be null-terminated");
    }

    #[test]
    fn test_wide_string_semaphore_handle_name() {
        let name: String = SEMAPHORE_HANDLE_NAME.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from_u32(c as u32).unwrap())
            .collect();
        assert_eq!(name, "ClearXR_DashboardSemaphore");
        assert_eq!(*SEMAPHORE_HANDLE_NAME.last().unwrap(), 0u16, "must be null-terminated");
    }

    #[test]
    fn test_handle_names_match_shm_constants() {
        // The names in renderer.rs must match what the layer expects to import.
        // shm.rs defines the string versions; renderer.rs uses wide strings.
        use crate::shm;
        let image_name: String = IMAGE_HANDLE_NAME.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from_u32(c as u32).unwrap())
            .collect();
        assert_eq!(image_name, shm::IMAGE_HANDLE_NAME);

        let sem_name: String = SEMAPHORE_HANDLE_NAME.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from_u32(c as u32).unwrap())
            .collect();
        assert_eq!(sem_name, shm::SEMAPHORE_HANDLE_NAME);
    }
}
