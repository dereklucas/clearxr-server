//! GPU-accelerated egui renderer using Vulkan render pass.
//!
//! Renders egui's tessellated meshes directly into a Vulkan image using a
//! dedicated render pass + pipeline, eliminating the CPU software rasterizer
//! bottleneck for large panels (1024x640 launcher = 655K pixels).
//!
//! Flow:
//! 1. Inject VR pointer events (from ray-cast UV coordinates)
//! 2. Run an egui frame with a layout closure (tessellation)
//! 3. Upload vertex/index data + font atlas to GPU buffers
//! 4. Execute a Vulkan render pass targeting the panel's texture directly
//! 5. Panel texture is now ready for sampling in the main scene render pass

use std::collections::HashMap;

use anyhow::Result;
use ash::vk;
use egui::{Context, Event, Pos2, PointerButton, RawInput, Rect, Vec2};
use epaint::{ImageData, ImageDelta, Primitive, TextureId};

use crate::renderer::load_spv;
use crate::vk_backend::VkBackend;

/// Push constants for the egui shaders (screen_size only).
#[repr(C)]
#[derive(Copy, Clone)]
struct EguiPushConstants {
    screen_size: [f32; 2],
}

/// A managed GPU texture (font atlas or user texture).
#[allow(dead_code)]
struct GpuTexture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    /// Whether the image has been transitioned out of UNDEFINED layout.
    initialized: bool,
}

/// GPU-accelerated egui renderer. Renders directly into a target VkImage.
pub struct EguiGpuRenderer {
    ctx: Context,
    width: u32,
    height: u32,

    // Pointer state
    pointer_pos: Option<Pos2>,
    has_rendered: bool,

    // Vulkan resources
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    sampler: vk::Sampler,

    // Font atlas and user textures on GPU
    textures: HashMap<TextureId, GpuTexture>,
    /// Descriptor sets keyed by TextureId.
    descriptor_sets: HashMap<TextureId, vk::DescriptorSet>,

    // Dynamic vertex/index buffers (host-visible for simplicity)
    vertex_buffer: vk::Buffer,
    vertex_memory: vk::DeviceMemory,
    vertex_mapped: *mut u8,
    vertex_capacity: usize, // in bytes

    index_buffer: vk::Buffer,
    index_memory: vk::DeviceMemory,
    index_mapped: *mut u8,
    index_capacity: usize, // in bytes

    // Staging buffer for texture uploads
    staging_buffer: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_mapped: *mut u8,
    staging_capacity: usize,

    // Per-frame command buffer + fence
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,

    // Framebuffer for the current target (recreated if target changes)
    framebuffer: vk::Framebuffer,
    framebuffer_view: vk::ImageView,
    framebuffer_target: vk::Image,
}

unsafe impl Send for EguiGpuRenderer {}
unsafe impl Sync for EguiGpuRenderer {}

/// egui vertex layout: pos (2 floats) + uv (2 floats) + color (4 u8 normalized).
/// Total = 20 bytes per vertex, matching epaint::Vertex.
const VERTEX_SIZE: usize = 20;

impl EguiGpuRenderer {
    /// Create a new GPU-accelerated egui renderer.
    ///
    /// `width` and `height` are the panel dimensions in pixels.
    /// The renderer creates its own render pass (color-only, no depth).
    pub fn new(vk: &VkBackend, width: u32, height: u32) -> Result<Self> {
        let device = vk.device();
        let ctx = Context::default();
        ctx.set_pixels_per_point(1.0);
        ctx.set_visuals(egui::Visuals::dark());

        // ---- Render pass (color-only, LOAD_OP_CLEAR, no depth) ----
        let render_pass = create_egui_render_pass(device)?;

        // ---- Sampler for font atlas ----
        let sampler = unsafe {
            device.create_sampler(
                &vk::SamplerCreateInfo {
                    mag_filter: vk::Filter::LINEAR,
                    min_filter: vk::Filter::LINEAR,
                    address_mode_u: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                    address_mode_v: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                    address_mode_w: vk::SamplerAddressMode::CLAMP_TO_EDGE,
                    ..Default::default()
                },
                None,
            )?
        };

        // ---- Descriptor set layout (one combined image sampler) ----
        let binding = vk::DescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
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

        // ---- Descriptor pool (allow up to 16 textures) ----
        let pool_size = vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 16,
        };
        let dp_ci = vk::DescriptorPoolCreateInfo {
            flags: vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET,
            max_sets: 16,
            pool_size_count: 1,
            p_pool_sizes: &pool_size,
            ..Default::default()
        };
        let descriptor_pool = unsafe { device.create_descriptor_pool(&dp_ci, None)? };

        // ---- Pipeline ----
        let (pipeline_layout, pipeline) =
            create_egui_pipeline(device, render_pass, descriptor_set_layout)?;

        // ---- Vertex buffer (initial 256KB) ----
        let vertex_capacity = 256 * 1024;
        let (vertex_buffer, vertex_memory, vertex_mapped) =
            create_host_visible_buffer(vk, vertex_capacity as u64, vk::BufferUsageFlags::VERTEX_BUFFER)?;

        // ---- Index buffer (initial 128KB) ----
        let index_capacity = 128 * 1024;
        let (index_buffer, index_memory, index_mapped) =
            create_host_visible_buffer(vk, index_capacity as u64, vk::BufferUsageFlags::INDEX_BUFFER)?;

        // ---- Staging buffer for texture uploads (initial 4MB) ----
        let staging_capacity = 4 * 1024 * 1024;
        let (staging_buffer, staging_memory, staging_mapped) =
            create_host_visible_buffer(vk, staging_capacity as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;

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
            width,
            height,
            pointer_pos: None,
            has_rendered: false,
            render_pass,
            pipeline_layout,
            pipeline,
            descriptor_set_layout,
            descriptor_pool,
            sampler,
            textures: HashMap::new(),
            descriptor_sets: HashMap::new(),
            vertex_buffer,
            vertex_memory,
            vertex_mapped,
            vertex_capacity,
            index_buffer,
            index_memory,
            index_mapped,
            index_capacity,
            staging_buffer,
            staging_memory,
            staging_mapped,
            staging_capacity,
            command_buffer,
            fence,
            framebuffer: vk::Framebuffer::null(),
            framebuffer_view: vk::ImageView::null(),
            framebuffer_target: vk::Image::null(),
        })
    }

    /// Force the next `run()` to render, even if egui says no repaint needed.
    pub fn force_repaint(&mut self) {
        self.has_rendered = false;
    }

    /// Inject a pointer move event from VR ray-cast UV coordinates.
    /// `u`, `v` are in `[0, 1]` panel space.
    pub fn pointer_move(&mut self, u: f32, v: f32) {
        self.pointer_pos = Some(Pos2::new(u * self.width as f32, v * self.height as f32));
    }

    /// Clear pointer (controller not pointing at this panel).
    pub fn pointer_leave(&mut self) {
        self.pointer_pos = None;
    }

    /// Width in pixels.
    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Run an egui frame and render directly into the target VkImage.
    ///
    /// The target image must have been created with COLOR_ATTACHMENT usage and
    /// R8G8B8A8_SRGB format. After this call, the image will be in
    /// SHADER_READ_ONLY_OPTIMAL layout, ready for sampling.
    ///
    /// Returns `true` if the image was updated.
    pub fn run(
        &mut self,
        vk: &VkBackend,
        target_image: vk::Image,
        target_view_format: vk::Format,
        click: bool,
        build_ui: impl FnMut(&Context),
    ) -> bool {
        let mut raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            ..Default::default()
        };

        // Inject pointer events.
        if let Some(pos) = self.pointer_pos {
            raw_input.events.push(Event::PointerMoved(pos));
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
        }

        let full_output = self.ctx.run(raw_input, build_ui);

        // Check if egui thinks a repaint is needed.
        let needs_repaint = full_output
            .viewport_output
            .values()
            .any(|vo| vo.repaint_delay == std::time::Duration::ZERO);
        if !needs_repaint && self.has_rendered {
            // Still apply texture deltas so the atlas stays up-to-date.
            if let Err(e) = self.apply_textures_delta(vk, &full_output.textures_delta) {
                log::error!("egui GPU texture delta failed: {}", e);
            }
            return false;
        }

        // Apply texture updates (font atlas, user textures).
        if let Err(e) = self.apply_textures_delta(vk, &full_output.textures_delta) {
            log::error!("egui GPU texture delta failed: {}", e);
            return false;
        }

        // Tessellate shapes into triangle meshes.
        let clipped_primitives =
            self.ctx
                .tessellate(full_output.shapes, full_output.pixels_per_point);

        // Render to GPU.
        if let Err(e) = self.render_to_target(vk, target_image, target_view_format, &clipped_primitives) {
            log::error!("egui GPU render failed: {}", e);
            return false;
        }

        self.has_rendered = true;
        true
    }

    /// Apply egui texture deltas (font atlas creation/updates, user textures).
    fn apply_textures_delta(
        &mut self,
        vk: &VkBackend,
        delta: &epaint::textures::TexturesDelta,
    ) -> Result<()> {
        let device = vk.device();

        for (id, image_delta) in &delta.set {
            self.apply_image_delta(vk, *id, image_delta)?;
        }
        for id in &delta.free {
            if let Some(tex) = self.textures.remove(id) {
                unsafe {
                    device.destroy_image_view(tex.view, None);
                    device.destroy_image(tex.image, None);
                    device.free_memory(tex.memory, None);
                }
            }
            if let Some(ds) = self.descriptor_sets.remove(id) {
                unsafe {
                    device.free_descriptor_sets(self.descriptor_pool, &[ds]).ok();
                }
            }
        }
        Ok(())
    }

    /// Apply a single texture delta (full or partial update).
    fn apply_image_delta(
        &mut self,
        vk: &VkBackend,
        id: TextureId,
        delta: &ImageDelta,
    ) -> Result<()> {
        let device = vk.device();

        let rgba_pixels: Vec<u8> = match &delta.image {
            ImageData::Color(color_image) => color_image
                .pixels
                .iter()
                .flat_map(|c| c.to_array())
                .collect(),
            ImageData::Font(font_image) => font_image
                .srgba_pixels(None)
                .flat_map(|c| c.to_array())
                .collect(),
        };

        let [w, h] = delta.image.size();
        let data_size = rgba_pixels.len();

        if delta.pos.is_some() && !self.textures.contains_key(&id) {
            // Partial update to non-existent texture — skip.
            return Ok(());
        }

        if delta.pos.is_none() {
            // Full update — (re)create the texture.
            // Remove old texture if it exists.
            if let Some(old) = self.textures.remove(&id) {
                unsafe {
                    device.destroy_image_view(old.view, None);
                    device.destroy_image(old.image, None);
                    device.free_memory(old.memory, None);
                }
            }
            if let Some(ds) = self.descriptor_sets.remove(&id) {
                unsafe {
                    device.free_descriptor_sets(self.descriptor_pool, &[ds]).ok();
                }
            }

            let (image, memory) = create_gpu_image(
                vk,
                w as u32,
                h as u32,
                vk::Format::R8G8B8A8_SRGB,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            )?;

            let view = unsafe {
                device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk::Format::R8G8B8A8_SRGB)
                        .subresource_range(
                            vk::ImageSubresourceRange::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .level_count(1)
                                .layer_count(1),
                        ),
                    None,
                )?
            };

            self.textures.insert(
                id,
                GpuTexture {
                    image,
                    memory,
                    view,
                    width: w as u32,
                    height: h as u32,
                    initialized: false,
                },
            );

            // Create descriptor set for this texture.
            let ds = self.allocate_descriptor_set(device, view)?;
            self.descriptor_sets.insert(id, ds);
        }

        // Ensure staging buffer is large enough.
        if data_size > self.staging_capacity {
            self.resize_staging_buffer(vk, data_size)?;
        }

        // Copy pixel data to staging buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(rgba_pixels.as_ptr(), self.staging_mapped, data_size);
        }

        // Upload via command buffer.
        let offset_x;
        let offset_y;
        if let Some(pos) = delta.pos {
            offset_x = pos[0] as u32;
            offset_y = pos[1] as u32;
        } else {
            offset_x = 0;
            offset_y = 0;
        }

        let tex = self.textures.get_mut(&id).unwrap();
        upload_texture_region(
            vk,
            self.staging_buffer,
            tex,
            offset_x,
            offset_y,
            w as u32,
            h as u32,
        )?;

        Ok(())
    }

    /// Allocate a descriptor set for a texture and write it.
    fn allocate_descriptor_set(
        &self,
        device: &ash::Device,
        view: vk::ImageView,
    ) -> Result<vk::DescriptorSet> {
        let ds_alloc = vk::DescriptorSetAllocateInfo {
            descriptor_pool: self.descriptor_pool,
            descriptor_set_count: 1,
            p_set_layouts: &self.descriptor_set_layout,
            ..Default::default()
        };
        let ds = unsafe { device.allocate_descriptor_sets(&ds_alloc) }?[0];

        let img_info = vk::DescriptorImageInfo {
            sampler: self.sampler,
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        };
        let write = vk::WriteDescriptorSet {
            dst_set: ds,
            dst_binding: 0,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            p_image_info: &img_info,
            ..Default::default()
        };
        unsafe { device.update_descriptor_sets(&[write], &[]) };

        Ok(ds)
    }

    /// Render tessellated egui primitives into the target image.
    fn render_to_target(
        &mut self,
        vk: &VkBackend,
        target_image: vk::Image,
        target_format: vk::Format,
        clipped_primitives: &[epaint::ClippedPrimitive],
    ) -> Result<()> {
        let device = vk.device();

        // Collect all vertices and indices, tracking offsets per mesh.
        let mut all_vertices: Vec<u8> = Vec::new();
        let mut all_indices: Vec<u32> = Vec::new();
        struct DrawCall {
            clip_rect: egui::Rect,
            texture_id: TextureId,
            vertex_offset: i32,
            index_offset: u32,
            index_count: u32,
        }
        let mut draw_calls: Vec<DrawCall> = Vec::new();

        for cp in clipped_primitives {
            let mesh = match &cp.primitive {
                Primitive::Mesh(m) => m,
                Primitive::Callback(_) => continue,
            };

            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }

            let vertex_offset = (all_vertices.len() / VERTEX_SIZE) as i32;
            let index_offset = all_indices.len() as u32;

            // Copy vertices in epaint::Vertex layout (pos2 + uv2 + color4u8 = 20 bytes)
            for v in &mesh.vertices {
                all_vertices.extend_from_slice(&v.pos.x.to_le_bytes());
                all_vertices.extend_from_slice(&v.pos.y.to_le_bytes());
                all_vertices.extend_from_slice(&v.uv.x.to_le_bytes());
                all_vertices.extend_from_slice(&v.uv.y.to_le_bytes());
                all_vertices.push(v.color.r());
                all_vertices.push(v.color.g());
                all_vertices.push(v.color.b());
                all_vertices.push(v.color.a());
            }

            all_indices.extend_from_slice(&mesh.indices);

            draw_calls.push(DrawCall {
                clip_rect: cp.clip_rect,
                texture_id: mesh.texture_id,
                vertex_offset,
                index_offset,
                index_count: mesh.indices.len() as u32,
            });
        }

        if draw_calls.is_empty() {
            // Nothing to draw — still need to clear the target.
            // We'll record a render pass with no draws.
        }

        let vtx_size = all_vertices.len();
        let idx_size = all_indices.len() * 4; // u32 indices

        // Resize buffers if needed.
        if vtx_size > self.vertex_capacity {
            self.resize_vertex_buffer(vk, vtx_size.next_power_of_two())?;
        }
        if idx_size > self.index_capacity {
            self.resize_index_buffer(vk, idx_size.next_power_of_two())?;
        }

        // Upload vertex data.
        if !all_vertices.is_empty() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    all_vertices.as_ptr(),
                    self.vertex_mapped,
                    vtx_size,
                );
            }
        }

        // Upload index data.
        if !all_indices.is_empty() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    all_indices.as_ptr() as *const u8,
                    self.index_mapped,
                    idx_size,
                );
            }
        }

        // Ensure framebuffer targets the correct image.
        self.ensure_framebuffer(device, target_image, target_format)?;

        // Wait for previous frame's commands to complete.
        unsafe {
            device.wait_for_fences(&[self.fence], true, u64::MAX)?;
            device.reset_fences(&[self.fence])?;
        }

        // Record command buffer.
        let cmd = self.command_buffer;
        let begin = vk::CommandBufferBeginInfo {
            flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
            ..Default::default()
        };

        unsafe {
            device.begin_command_buffer(cmd, &begin)?;

            // Transition target image to COLOR_ATTACHMENT_OPTIMAL.
            // The image may be in SHADER_READ_ONLY or UNDEFINED.
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

            // Begin render pass (clear to fully transparent so desktop can show through).
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

            // Set viewport and scissor.
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: self.width as f32,
                height: self.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            device.cmd_set_viewport(cmd, 0, &[viewport]);

            // Bind pipeline.
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);

            // Push constants (screen size).
            let push = EguiPushConstants {
                screen_size: [self.width as f32, self.height as f32],
            };
            let push_bytes = std::slice::from_raw_parts(
                &push as *const EguiPushConstants as *const u8,
                std::mem::size_of::<EguiPushConstants>(),
            );
            device.cmd_push_constants(
                cmd,
                self.pipeline_layout,
                vk::ShaderStageFlags::VERTEX,
                0,
                push_bytes,
            );

            // Bind vertex and index buffers.
            if !all_vertices.is_empty() {
                device.cmd_bind_vertex_buffers(cmd, 0, &[self.vertex_buffer], &[0]);
                device.cmd_bind_index_buffer(cmd, self.index_buffer, 0, vk::IndexType::UINT32);
            }

            // Draw each mesh.
            let mut current_texture = TextureId::default();
            let mut bound_texture = false;

            for dc in &draw_calls {
                // Bind texture descriptor set if changed.
                if !bound_texture || dc.texture_id != current_texture {
                    if let Some(&ds) = self.descriptor_sets.get(&dc.texture_id) {
                        device.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.pipeline_layout,
                            0,
                            &[ds],
                            &[],
                        );
                        current_texture = dc.texture_id;
                        bound_texture = true;
                    } else {
                        // Texture not uploaded yet — skip this draw.
                        continue;
                    }
                }

                // Set scissor from clip rect.
                let clip = &dc.clip_rect;
                let sx = (clip.min.x as i32).max(0);
                let sy = (clip.min.y as i32).max(0);
                let sw = ((clip.max.x as i32) - sx).max(0) as u32;
                let sh = ((clip.max.y as i32) - sy).max(0) as u32;
                if sw == 0 || sh == 0 {
                    continue;
                }
                let scissor = vk::Rect2D {
                    offset: vk::Offset2D { x: sx, y: sy },
                    extent: vk::Extent2D {
                        width: sw.min(self.width),
                        height: sh.min(self.height),
                    },
                };
                device.cmd_set_scissor(cmd, 0, &[scissor]);

                // Draw indexed.
                device.cmd_draw_indexed(
                    cmd,
                    dc.index_count,
                    1,
                    dc.index_offset,
                    dc.vertex_offset,
                    0,
                );
            }

            device.cmd_end_render_pass(cmd);

            // Transition target image to SHADER_READ_ONLY_OPTIMAL.
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

            // Submit.
            let submit = vk::SubmitInfo {
                command_buffer_count: 1,
                p_command_buffers: &cmd,
                ..Default::default()
            };
            device.queue_submit(vk.queue(), &[submit], self.fence)?;
            // Wait for completion so the panel texture is ready for the main render pass.
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

        // Destroy old framebuffer/view if they exist.
        if self.framebuffer != vk::Framebuffer::null() {
            unsafe { device.destroy_framebuffer(self.framebuffer, None) };
            self.framebuffer = vk::Framebuffer::null();
        }
        if self.framebuffer_view != vk::ImageView::null() {
            unsafe { device.destroy_image_view(self.framebuffer_view, None) };
            self.framebuffer_view = vk::ImageView::null();
        }

        // Create image view for the target.
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

        // Create framebuffer.
        let fb_ci = vk::FramebufferCreateInfo {
            render_pass: self.render_pass,
            attachment_count: 1,
            p_attachments: &view,
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

    /// Resize the vertex buffer (destroys old, creates new).
    fn resize_vertex_buffer(&mut self, vk: &VkBackend, new_capacity: usize) -> Result<()> {
        let device = vk.device();
        unsafe {
            device.unmap_memory(self.vertex_memory);
            device.destroy_buffer(self.vertex_buffer, None);
            device.free_memory(self.vertex_memory, None);
        }
        let (buf, mem, mapped) =
            create_host_visible_buffer(vk, new_capacity as u64, vk::BufferUsageFlags::VERTEX_BUFFER)?;
        self.vertex_buffer = buf;
        self.vertex_memory = mem;
        self.vertex_mapped = mapped;
        self.vertex_capacity = new_capacity;
        Ok(())
    }

    /// Resize the index buffer (destroys old, creates new).
    fn resize_index_buffer(&mut self, vk: &VkBackend, new_capacity: usize) -> Result<()> {
        let device = vk.device();
        unsafe {
            device.unmap_memory(self.index_memory);
            device.destroy_buffer(self.index_buffer, None);
            device.free_memory(self.index_memory, None);
        }
        let (buf, mem, mapped) =
            create_host_visible_buffer(vk, new_capacity as u64, vk::BufferUsageFlags::INDEX_BUFFER)?;
        self.index_buffer = buf;
        self.index_memory = mem;
        self.index_mapped = mapped;
        self.index_capacity = new_capacity;
        Ok(())
    }

    /// Resize the staging buffer.
    fn resize_staging_buffer(&mut self, vk: &VkBackend, new_capacity: usize) -> Result<()> {
        let device = vk.device();
        unsafe {
            device.unmap_memory(self.staging_memory);
            device.destroy_buffer(self.staging_buffer, None);
            device.free_memory(self.staging_memory, None);
        }
        let (buf, mem, mapped) =
            create_host_visible_buffer(vk, new_capacity as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
        self.staging_buffer = buf;
        self.staging_memory = mem;
        self.staging_mapped = mapped;
        self.staging_capacity = new_capacity;
        Ok(())
    }

    /// Destroy all Vulkan resources.
    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            // Wait for any in-flight work.
            device.wait_for_fences(&[self.fence], true, u64::MAX).ok();

            device.destroy_fence(self.fence, None);
            device.free_command_buffers(vk::CommandPool::null(), &[]); // no-op placeholder

            if self.framebuffer != vk::Framebuffer::null() {
                device.destroy_framebuffer(self.framebuffer, None);
            }
            if self.framebuffer_view != vk::ImageView::null() {
                device.destroy_image_view(self.framebuffer_view, None);
            }

            // Destroy GPU textures.
            for (_, tex) in self.textures.drain() {
                device.destroy_image_view(tex.view, None);
                device.destroy_image(tex.image, None);
                device.free_memory(tex.memory, None);
            }

            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_render_pass(self.render_pass, None);

            device.unmap_memory(self.vertex_memory);
            device.destroy_buffer(self.vertex_buffer, None);
            device.free_memory(self.vertex_memory, None);

            device.unmap_memory(self.index_memory);
            device.destroy_buffer(self.index_buffer, None);
            device.free_memory(self.index_memory, None);

            device.unmap_memory(self.staging_memory);
            device.destroy_buffer(self.staging_buffer, None);
            device.free_memory(self.staging_memory, None);
        }
    }
}

// ====================================================================== //
//  Vulkan resource helpers                                                //
// ====================================================================== //

/// Upload a region of pixel data from a staging buffer to a GPU texture.
/// Standalone function to avoid borrow-checker issues with `&mut self`.
fn upload_texture_region(
    vk: &VkBackend,
    staging_buffer: vk::Buffer,
    tex: &mut GpuTexture,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    let device = vk.device();

    let alloc_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(vk.command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    let cmd = unsafe { device.allocate_command_buffers(&alloc_info) }?[0];

    let begin = vk::CommandBufferBeginInfo {
        flags: vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT,
        ..Default::default()
    };

    unsafe {
        device.begin_command_buffer(cmd, &begin)?;

        // Transition to TRANSFER_DST
        let old_layout = if tex.initialized {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        };
        let barrier = vk::ImageMemoryBarrier {
            old_layout,
            new_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            image: tex.image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            src_access_mask: vk::AccessFlags::empty(),
            dst_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            ..Default::default()
        };
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );

        // Copy staging buffer to image region
        let region = vk::BufferImageCopy {
            buffer_offset: 0,
            buffer_row_length: width,
            buffer_image_height: height,
            image_subresource: vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            },
            image_offset: vk::Offset3D {
                x: offset_x as i32,
                y: offset_y as i32,
                z: 0,
            },
            image_extent: vk::Extent3D {
                width,
                height,
                depth: 1,
            },
        };
        device.cmd_copy_buffer_to_image(
            cmd,
            staging_buffer,
            tex.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );

        // Transition to SHADER_READ_ONLY
        let barrier2 = vk::ImageMemoryBarrier {
            old_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
            image: tex.image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            dst_access_mask: vk::AccessFlags::SHADER_READ,
            ..Default::default()
        };
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier2],
        );

        device.end_command_buffer(cmd)?;

        let submit = vk::SubmitInfo {
            command_buffer_count: 1,
            p_command_buffers: &cmd,
            ..Default::default()
        };
        let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
        device.queue_submit(vk.queue(), &[submit], fence)?;
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        device.destroy_fence(fence, None);
        device.free_command_buffers(vk.command_pool, &[cmd]);
    }

    tex.initialized = true;
    Ok(())
}

/// Create an egui-specific render pass (color-only, no depth).
fn create_egui_render_pass(device: &ash::Device) -> Result<vk::RenderPass> {
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

/// Create the egui graphics pipeline.
fn create_egui_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    descriptor_set_layout: vk::DescriptorSetLayout,
) -> Result<(vk::PipelineLayout, vk::Pipeline)> {
    let vert_spv = include_bytes!("../../shaders/egui.vert.spv");
    let frag_spv = include_bytes!("../../shaders/egui.frag.spv");

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

    // Push constants: screen_size (2 floats = 8 bytes)
    let push_range = vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::VERTEX,
        offset: 0,
        size: std::mem::size_of::<EguiPushConstants>() as u32,
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

    // Vertex input: pos (vec2), uv (vec2), color (vec4 as unorm8)
    let binding_desc = vk::VertexInputBindingDescription {
        binding: 0,
        stride: VERTEX_SIZE as u32,
        input_rate: vk::VertexInputRate::VERTEX,
    };
    let attr_descs = [
        // location 0: position (vec2, offset 0)
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: 0,
        },
        // location 1: uv (vec2, offset 8)
        vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: 8,
        },
        // location 2: color (vec4 unorm8, offset 16)
        vk::VertexInputAttributeDescription {
            location: 2,
            binding: 0,
            format: vk::Format::R8G8B8A8_UNORM,
            offset: 16,
        },
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo {
        vertex_binding_description_count: 1,
        p_vertex_binding_descriptions: &binding_desc,
        vertex_attribute_description_count: attr_descs.len() as u32,
        p_vertex_attribute_descriptions: attr_descs.as_ptr(),
        ..Default::default()
    };

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

    // Premultiplied alpha blending (egui outputs premultiplied alpha).
    let blend_attachment = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::ONE,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::RGBA,
    };
    let color_blend = vk::PipelineColorBlendStateCreateInfo {
        attachment_count: 1,
        p_attachments: &blend_attachment,
        ..Default::default()
    };

    let depth_stencil = vk::PipelineDepthStencilStateCreateInfo {
        depth_test_enable: vk::FALSE,
        depth_write_enable: vk::FALSE,
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
            .map_err(|(_, e)| anyhow::anyhow!("egui GPU pipeline creation failed: {:?}", e))?[0]
    };

    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    Ok((pipeline_layout, pipeline))
}

/// Create a host-visible, host-coherent buffer with persistent mapping.
fn create_host_visible_buffer(
    vk: &VkBackend,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8)> {
    let device = vk.device();

    let buf_ci = vk::BufferCreateInfo {
        size,
        usage,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        ..Default::default()
    };
    let buffer = unsafe { device.create_buffer(&buf_ci, None)? };
    let mem_reqs = unsafe { device.get_buffer_memory_requirements(buffer) };
    let mem_type = vk
        .find_memory_type(
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .ok_or_else(|| anyhow::anyhow!("No host-visible memory for egui buffer"))?;
    let memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo {
                allocation_size: mem_reqs.size,
                memory_type_index: mem_type,
                ..Default::default()
            },
            None,
        )?
    };
    unsafe { device.bind_buffer_memory(buffer, memory, 0)? };
    let mapped = unsafe {
        device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())?
    } as *mut u8;

    Ok((buffer, memory, mapped))
}

/// Create a GPU-local image.
fn create_gpu_image(
    vk: &VkBackend,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::DeviceMemory)> {
    let device = vk.device();

    let img_ci = vk::ImageCreateInfo {
        image_type: vk::ImageType::TYPE_2D,
        format,
        extent: vk::Extent3D {
            width,
            height,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: vk::SampleCountFlags::TYPE_1,
        tiling: vk::ImageTiling::OPTIMAL,
        usage,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        initial_layout: vk::ImageLayout::UNDEFINED,
        ..Default::default()
    };
    let image = unsafe { device.create_image(&img_ci, None)? };
    let mem_reqs = unsafe { device.get_image_memory_requirements(image) };
    let mem_type = vk
        .find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
        .ok_or_else(|| anyhow::anyhow!("No device-local memory for egui GPU texture"))?;
    let memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo {
                allocation_size: mem_reqs.size,
                memory_type_index: mem_type,
                ..Default::default()
            },
            None,
        )?
    };
    unsafe { device.bind_image_memory(image, memory, 0)? };

    Ok((image, memory))
}

// ====================================================================== //
//  Tests                                                                  //
// ====================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_egui_push_constants_size() {
        assert_eq!(std::mem::size_of::<EguiPushConstants>(), 8);
    }

    #[test]
    fn test_vertex_size_matches_epaint() {
        assert_eq!(std::mem::size_of::<epaint::Vertex>(), VERTEX_SIZE);
    }
}
