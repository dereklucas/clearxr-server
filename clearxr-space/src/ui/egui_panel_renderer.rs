//! egui rendering to a Vulkan panel texture via the `egui-ash-renderer` crate.
//!
//! Thin wrapper that keeps the same public API as the old hand-rolled
//! `EguiGpuRenderer` but delegates all GPU work (pipeline, font atlas,
//! vertex/index buffers, draw recording) to `egui_ash_renderer::Renderer`.

use anyhow::Result;
use ash::vk;
use egui::{Context, Event, Pos2, PointerButton, RawInput, Rect, Vec2};

use crate::vk_backend::VkBackend;

/// GPU-accelerated egui renderer backed by `egui-ash-renderer`.
///
/// Renders egui directly into a target `VkImage` (the panel texture) using a
/// dedicated render pass. After each `run()` call the image is left in
/// `SHADER_READ_ONLY_OPTIMAL` layout, ready for sampling in the main scene
/// render pass.
pub struct EguiPanelRenderer {
    ctx: Context,
    renderer: egui_ash_renderer::Renderer,
    width: u32,
    height: u32,

    // Render pass owned by us (R8G8B8A8_SRGB, LOAD_CLEAR, final = COLOR_ATTACHMENT)
    render_pass: vk::RenderPass,

    // Framebuffer targeting the current panel texture
    framebuffer: vk::Framebuffer,
    framebuffer_view: vk::ImageView,
    framebuffer_target: vk::Image,

    // Per-frame command buffer + fence
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Pointer input state
    pointer_pos: Option<Pos2>,
    has_rendered: bool,

    // Button state tracking (for edge detection)
    prev_button_pressed: bool,
    prev_secondary_pressed: bool,
}

unsafe impl Send for EguiPanelRenderer {}
unsafe impl Sync for EguiPanelRenderer {}

impl EguiPanelRenderer {
    /// Create a new panel renderer.
    ///
    /// `width` and `height` are the panel dimensions in pixels.
    pub fn new(vk: &VkBackend, width: u32, height: u32) -> Result<Self> {
        let device = vk.device();
        let ctx = Context::default();
        ctx.set_pixels_per_point(1.0);
        ctx.set_visuals(egui::Visuals::dark());

        // ---- Render pass (color-only, LOAD_CLEAR, no depth) ----
        let render_pass = create_panel_render_pass(device)?;

        // ---- egui-ash-renderer ----
        let renderer = egui_ash_renderer::Renderer::with_default_allocator(
            vk.instance_ref(),
            vk.physical_device(),
            device.clone(),
            render_pass,
            egui_ash_renderer::Options {
                srgb_framebuffer: true, // target is R8G8B8A8_SRGB
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("egui-ash-renderer init failed: {e}"))?;

        // ---- Command buffer + fence ----
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(vk.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];

        let fence = unsafe {
            device.create_fence(
                &vk::FenceCreateInfo {
                    flags: vk::FenceCreateFlags::SIGNALED,
                    ..Default::default()
                },
                None,
            )?
        };

        Ok(Self {
            ctx,
            renderer,
            width,
            height,
            render_pass,
            framebuffer: vk::Framebuffer::null(),
            framebuffer_view: vk::ImageView::null(),
            framebuffer_target: vk::Image::null(),
            command_buffer,
            fence,
            pointer_pos: None,
            has_rendered: false,
            prev_button_pressed: false,
            prev_secondary_pressed: false,
        })
    }

    // ------------------------------------------------------------------ //
    //  Pointer input
    // ------------------------------------------------------------------ //

    /// Inject a pointer move event from VR ray-cast UV coordinates.
    /// `u`, `v` are in `[0, 1]` panel space.
    pub fn pointer_move(&mut self, u: f32, v: f32) {
        self.pointer_pos = Some(Pos2::new(u * self.width as f32, v * self.height as f32));
    }

    /// Clear pointer (controller not pointing at this panel).
    pub fn pointer_leave(&mut self) {
        self.pointer_pos = None;
    }

    /// Returns true if egui wants keyboard input (e.g. a TextEdit has focus).
    pub fn wants_keyboard_input(&self) -> bool {
        self.ctx.wants_keyboard_input()
    }

    // ------------------------------------------------------------------ //
    //  Frame rendering
    // ------------------------------------------------------------------ //

    /// Run an egui frame and render directly into the target `VkImage`.
    ///
    /// The target image must have `COLOR_ATTACHMENT` usage and `R8G8B8A8_SRGB`
    /// format. After this call the image is in `SHADER_READ_ONLY_OPTIMAL`.
    /// Returns `true` if the image was updated.
    ///
    /// `click` – true for one frame to send instant press+release (for egui buttons).
    /// `button_pressed` – true while the primary trigger is held (for drag/select).
    /// `secondary_pressed` – true while the grip/squeeze is held (right-click).
    /// `scroll_delta` – vertical scroll amount from thumbstick (pixels).
    /// `pending_text` – characters queued by the virtual keyboard.
    pub fn run(
        &mut self,
        vk: &VkBackend,
        target_image: vk::Image,
        target_view_format: vk::Format,
        click: bool,
        button_pressed: bool,
        secondary_pressed: bool,
        scroll_delta: f32,
        pending_text: &[String],
        build_ui: impl FnMut(&Context),
    ) -> bool {
        // ---- Build egui RawInput ----
        let mut raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            ..Default::default()
        };

        if let Some(pos) = self.pointer_pos {
            raw_input.events.push(Event::PointerMoved(pos));

            // Instant click: press+release in one frame (for egui buttons/tabs)
            if click {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }

            // Continuous primary button: send press/release on state change (for drag/select)
            if button_pressed != self.prev_button_pressed {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: button_pressed,
                    modifiers: Default::default(),
                });
            }

            // Secondary button (right-click): send press/release on state change
            if secondary_pressed != self.prev_secondary_pressed {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed: secondary_pressed,
                    modifiers: Default::default(),
                });
            }
        }
        self.prev_button_pressed = button_pressed;
        self.prev_secondary_pressed = secondary_pressed;

        // Thumbstick scroll
        if scroll_delta.abs() > 0.01 {
            raw_input.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, scroll_delta),
                modifiers: Default::default(),
            });
        }

        // Virtual keyboard text input
        for text in pending_text {
            if text == "\x08" {
                // Backspace sentinel from virtual keyboard
                raw_input.events.push(Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                    physical_key: None,
                });
                raw_input.events.push(Event::Key {
                    key: egui::Key::Backspace,
                    pressed: false,
                    repeat: false,
                    modifiers: Default::default(),
                    physical_key: None,
                });
            } else {
                raw_input.events.push(Event::Text(text.clone()));
            }
        }

        // ---- Run egui ----
        let full_output = self.ctx.run(raw_input, build_ui);

        let needs_repaint = full_output
            .viewport_output
            .values()
            .any(|vo| vo.repaint_delay == std::time::Duration::ZERO);

        // Apply texture deltas (font atlas, user textures) even when skipping paint.
        let textures_delta = full_output.textures_delta;
        if !textures_delta.set.is_empty() {
            if let Err(e) = self
                .renderer
                .set_textures(vk.queue(), vk.command_pool, &textures_delta.set)
                .map_err(|e| anyhow::anyhow!("{e}"))
            {
                log::error!("egui set_textures failed: {e}");
            }
        }

        if !needs_repaint && self.has_rendered {
            // Free any textures that were released this frame.
            if !textures_delta.free.is_empty() {
                if let Err(e) = self
                    .renderer
                    .free_textures(&textures_delta.free)
                    .map_err(|e| anyhow::anyhow!("{e}"))
                {
                    log::error!("egui free_textures failed: {e}");
                }
            }
            return false;
        }

        // ---- Tessellate ----
        let clipped_primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        // ---- Render to GPU ----
        if let Err(e) = self.render_to_target(vk, target_image, target_view_format, &clipped_primitives) {
            log::error!("egui panel render failed: {e}");
            return false;
        }

        // Free textures after rendering (they may have been sampled this frame).
        if !textures_delta.free.is_empty() {
            if let Err(e) = self
                .renderer
                .free_textures(&textures_delta.free)
                .map_err(|e| anyhow::anyhow!("{e}"))
            {
                log::error!("egui free_textures failed: {e}");
            }
        }

        self.has_rendered = true;
        true
    }

    // ------------------------------------------------------------------ //
    //  Internal: record + submit render commands
    // ------------------------------------------------------------------ //

    fn render_to_target(
        &mut self,
        vk: &VkBackend,
        target_image: vk::Image,
        target_format: vk::Format,
        clipped_primitives: &[egui::ClippedPrimitive],
    ) -> Result<()> {
        let device = vk.device();

        // Ensure framebuffer targets the correct image.
        self.ensure_framebuffer(device, target_image, target_format)?;

        // Wait for previous frame's commands to complete.
        unsafe {
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
            device.reset_fences(&[self.fence])?;
        }

        let cmd = self.command_buffer;
        let begin = vk::CommandBufferBeginInfo {
            flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
            ..Default::default()
        };

        unsafe {
            device.begin_command_buffer(cmd, &begin)?;

            // Transition target UNDEFINED/SHADER_READ_ONLY -> COLOR_ATTACHMENT_OPTIMAL.
            let barrier = vk::ImageMemoryBarrier {
                old_layout: vk::ImageLayout::UNDEFINED,
                new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: target_image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    level_count: 1,
                    layer_count: 1,
                    ..Default::default()
                },
                src_access_mask: vk::AccessFlags::empty(),
                dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                ..Default::default()
            };
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );

            // Begin render pass — clear to fully transparent.
            let clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 0.0],
                },
            };
            let rp_begin = vk::RenderPassBeginInfo {
                render_pass: self.render_pass,
                framebuffer: self.framebuffer,
                render_area: vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D {
                        width: self.width,
                        height: self.height,
                    },
                },
                clear_value_count: 1,
                p_clear_values: &clear_value,
                ..Default::default()
            };
            device.cmd_begin_render_pass(cmd, &rp_begin, vk::SubpassContents::INLINE);

            // Record egui draw commands via the crate.
            self.renderer
                .cmd_draw(
                    cmd,
                    vk::Extent2D {
                        width: self.width,
                        height: self.height,
                    },
                    1.0, // pixels_per_point
                    clipped_primitives,
                )
                .map_err(|e| anyhow::anyhow!("cmd_draw failed: {e}"))?;

            device.cmd_end_render_pass(cmd);

            // Transition COLOR_ATTACHMENT -> SHADER_READ_ONLY_OPTIMAL.
            let barrier2 = vk::ImageMemoryBarrier {
                old_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: target_image,
                subresource_range: vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    level_count: 1,
                    layer_count: 1,
                    ..Default::default()
                },
                src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                dst_access_mask: vk::AccessFlags::SHADER_READ,
                ..Default::default()
            };
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier2],
            );

            device.end_command_buffer(cmd)?;

            // Submit and wait.
            let submit = vk::SubmitInfo {
                command_buffer_count: 1,
                p_command_buffers: &cmd,
                ..Default::default()
            };
            device.queue_submit(vk.queue(), &[submit], self.fence)?;
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
        }

        Ok(())
    }

    /// Ensure the framebuffer targets the given image.
    fn ensure_framebuffer(
        &mut self,
        device: &ash::Device,
        target_image: vk::Image,
        format: vk::Format,
    ) -> Result<()> {
        if self.framebuffer_target == target_image && self.framebuffer != vk::Framebuffer::null() {
            return Ok(());
        }

        // Destroy old framebuffer/view.
        if self.framebuffer != vk::Framebuffer::null() {
            unsafe { device.destroy_framebuffer(self.framebuffer, None) };
            self.framebuffer = vk::Framebuffer::null();
        }
        if self.framebuffer_view != vk::ImageView::null() {
            unsafe { device.destroy_image_view(self.framebuffer_view, None) };
            self.framebuffer_view = vk::ImageView::null();
        }

        let view = unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(target_image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(
                        vk::ImageSubresourceRange::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .level_count(1)
                            .layer_count(1),
                    ),
                None,
            )?
        };

        let attachments = [view];
        let fb_ci = vk::FramebufferCreateInfo {
            render_pass: self.render_pass,
            attachment_count: 1,
            p_attachments: attachments.as_ptr(),
            width: self.width,
            height: self.height,
            layers: 1,
            ..Default::default()
        };
        let framebuffer = unsafe { device.create_framebuffer(&fb_ci, None)? };

        self.framebuffer = framebuffer;
        self.framebuffer_view = view;
        self.framebuffer_target = target_image;
        Ok(())
    }

    // ------------------------------------------------------------------ //
    //  Cleanup
    // ------------------------------------------------------------------ //

    /// Destroy all Vulkan resources.
    ///
    /// The inner `egui_ash_renderer::Renderer` is cleaned up via its `Drop`
    /// impl when this struct is dropped; we only need to destroy the resources
    /// we own directly.
    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.wait_for_fences(&[self.fence], true, u64::MAX).ok();
            device.destroy_fence(self.fence, None);

            if self.framebuffer != vk::Framebuffer::null() {
                device.destroy_framebuffer(self.framebuffer, None);
            }
            if self.framebuffer_view != vk::ImageView::null() {
                device.destroy_image_view(self.framebuffer_view, None);
            }

            device.destroy_render_pass(self.render_pass, None);
        }
    }
}

// ====================================================================== //
//  Render pass creation
// ====================================================================== //

/// Create a render pass for egui panel rendering (R8G8B8A8_SRGB, LOAD_CLEAR).
fn create_panel_render_pass(device: &ash::Device) -> Result<vk::RenderPass> {
    let color_attachment = vk::AttachmentDescription {
        format: vk::Format::R8G8B8A8_SRGB,
        samples: vk::SampleCountFlags::TYPE_1,
        load_op: vk::AttachmentLoadOp::CLEAR,
        store_op: vk::AttachmentStoreOp::STORE,
        stencil_load_op: vk::AttachmentLoadOp::DONT_CARE,
        stencil_store_op: vk::AttachmentStoreOp::DONT_CARE,
        initial_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        final_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        ..Default::default()
    };

    let color_ref = vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    };

    let subpass = vk::SubpassDescription {
        pipeline_bind_point: vk::PipelineBindPoint::GRAPHICS,
        color_attachment_count: 1,
        p_color_attachments: &color_ref,
        ..Default::default()
    };

    let dependency = vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        src_access_mask: vk::AccessFlags::empty(),
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        ..Default::default()
    };

    let rp_ci = vk::RenderPassCreateInfo {
        attachment_count: 1,
        p_attachments: &color_attachment,
        subpass_count: 1,
        p_subpasses: &subpass,
        dependency_count: 1,
        p_dependencies: &dependency,
        ..Default::default()
    };

    Ok(unsafe { device.create_render_pass(&rp_ci, None)? })
}
