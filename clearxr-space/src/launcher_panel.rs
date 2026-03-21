/// Renders a textured quad panel in 3D space.
///
/// Used to display the launcher UI (rendered from HTML) as a floating panel
/// in the VR/desktop scene. The panel has its own Vulkan pipeline with a
/// sampled texture, separate from the fullscreen scene pipeline.

use anyhow::Result;
use ash::vk;
use glam::Vec3;

use crate::renderer::{load_spv, PushConstants};
use crate::vk_backend::VkBackend;

/// Push constants for the panel shaders (must match panel.vert/frag).
/// Extends the scene PushConstants with panel placement data.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct PanelPushConstants {
    // First 80 bytes: camera data (same layout as scene PushConstants)
    pub cam_pos: [f32; 4],
    pub cam_right: [f32; 4],
    pub cam_up: [f32; 4],
    pub cam_fwd: [f32; 4],
    pub fov: [f32; 4],
    // Next 48 bytes: panel placement
    pub panel_center: [f32; 4], // xyz = world pos, w = opacity
    pub panel_right: [f32; 4],  // xyz = panel right axis (unit), w = unused
    pub panel_up: [f32; 4],     // xyz = panel up axis (unit), w = unused
}

pub struct LauncherPanel {
    // Texture
    pub texture: vk::Image,
    texture_memory: vk::DeviceMemory,
    texture_view: vk::ImageView,
    sampler: vk::Sampler,
    pub tex_width: u32,
    pub tex_height: u32,

    // Staging buffer for CPU -> GPU texture uploads
    staging_buffer: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_mapped: *mut u8,
    staging_size: vk::DeviceSize,

    // Descriptor set for the texture sampler
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,

    // Pipeline
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,

    // Panel placement in world space
    pub center: Vec3,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,

    // Has the texture been uploaded at least once?
    texture_initialized: bool,
}

unsafe impl Send for LauncherPanel {}
unsafe impl Sync for LauncherPanel {}

impl LauncherPanel {
    pub fn new(
        vk: &VkBackend,
        render_pass: vk::RenderPass,
        tex_width: u32,
        tex_height: u32,
    ) -> Result<Self> {
        let device = vk.device();

        // ---- Texture image ----
        let img_ci = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::R8G8B8A8_SRGB,
            extent: vk::Extent3D {
                width: tex_width,
                height: tex_height,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            initial_layout: vk::ImageLayout::UNDEFINED,
            ..Default::default()
        };
        let texture = unsafe { device.create_image(&img_ci, None)? };
        let mem_reqs = unsafe { device.get_image_memory_requirements(texture) };
        let mem_type = vk
            .find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .ok_or_else(|| anyhow::anyhow!("No device-local memory for panel texture"))?;
        let texture_memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo {
                    allocation_size: mem_reqs.size,
                    memory_type_index: mem_type,
                    ..Default::default()
                },
                None,
            )?
        };
        unsafe { device.bind_image_memory(texture, texture_memory, 0)? };

        // ---- Image view ----
        let texture_view = unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(texture)
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

        // ---- Sampler ----
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

        // ---- Staging buffer (host-visible, for CPU -> GPU copies) ----
        let staging_size = (tex_width * tex_height * 4) as vk::DeviceSize;
        let buf_ci = vk::BufferCreateInfo {
            size: staging_size,
            usage: vk::BufferUsageFlags::TRANSFER_SRC,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };
        let staging_buffer = unsafe { device.create_buffer(&buf_ci, None)? };
        let buf_reqs = unsafe { device.get_buffer_memory_requirements(staging_buffer) };
        let buf_mem_type = vk
            .find_memory_type(
                buf_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or_else(|| anyhow::anyhow!("No host-visible memory for staging buffer"))?;
        let staging_memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo {
                    allocation_size: buf_reqs.size,
                    memory_type_index: buf_mem_type,
                    ..Default::default()
                },
                None,
            )?
        };
        unsafe { device.bind_buffer_memory(staging_buffer, staging_memory, 0)? };
        let staging_mapped = unsafe {
            device.map_memory(staging_memory, 0, staging_size, vk::MemoryMapFlags::empty())?
        } as *mut u8;

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

        // ---- Descriptor pool ----
        let pool_size = vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
        };
        let dp_ci = vk::DescriptorPoolCreateInfo {
            max_sets: 1,
            pool_size_count: 1,
            p_pool_sizes: &pool_size,
            ..Default::default()
        };
        let descriptor_pool = unsafe { device.create_descriptor_pool(&dp_ci, None)? };

        // ---- Allocate + write descriptor ----
        let ds_alloc = vk::DescriptorSetAllocateInfo {
            descriptor_pool,
            descriptor_set_count: 1,
            p_set_layouts: &descriptor_set_layout,
            ..Default::default()
        };
        let descriptor_set = unsafe { device.allocate_descriptor_sets(&ds_alloc) }?[0];

        let img_info = vk::DescriptorImageInfo {
            sampler,
            image_view: texture_view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        };
        let write = vk::WriteDescriptorSet {
            dst_set: descriptor_set,
            dst_binding: 0,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            p_image_info: &img_info,
            ..Default::default()
        };
        unsafe { device.update_descriptor_sets(&[write], &[]) };

        // ---- Pipeline ----
        let (pipeline_layout, pipeline) =
            create_panel_pipeline(device, render_pass, descriptor_set_layout)?;

        Ok(Self {
            texture,
            texture_memory,
            texture_view,
            sampler,
            tex_width,
            tex_height,
            staging_buffer,
            staging_memory,
            staging_mapped,
            staging_size,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            pipeline_layout,
            pipeline,
            center: Vec3::new(0.0, 1.6, -2.5),
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            texture_initialized: false,
        })
    }

    /// Upload RGBA pixel data to the panel texture.
    /// `pixels` must be exactly tex_width * tex_height * 4 bytes (RGBA).
    pub fn upload_pixels(&mut self, vk: &VkBackend, pixels: &[u8]) -> Result<()> {
        let device = vk.device();
        let expected = (self.tex_width * self.tex_height * 4) as usize;
        if pixels.len() != expected {
            anyhow::bail!(
                "Panel pixel data size mismatch: got {}, expected {}",
                pixels.len(),
                expected
            );
        }

        // Copy to staging buffer
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.staging_mapped, pixels.len());
        }

        // Record and submit a transfer command
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

            // Transition image to TRANSFER_DST
            let barrier = vk::ImageMemoryBarrier {
                old_layout: if self.texture_initialized {
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                } else {
                    vk::ImageLayout::UNDEFINED
                },
                new_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: self.texture,
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

            // Copy buffer to image
            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D {
                    width: self.tex_width,
                    height: self.tex_height,
                    depth: 1,
                },
            };
            device.cmd_copy_buffer_to_image(
                cmd,
                self.staging_buffer,
                self.texture,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );

            // Transition to SHADER_READ_ONLY
            let barrier2 = vk::ImageMemoryBarrier {
                old_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image: self.texture,
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

            // Submit and wait
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

        self.texture_initialized = true;
        Ok(())
    }

    /// Record draw commands for the panel into an already-begun command buffer.
    /// Call this after the scene has been drawn (within the same render pass).
    pub fn record_draw(
        &self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        scene_push: &PushConstants,
    ) {
        if !self.texture_initialized {
            return;
        }

        // Panel is fixed in world space: faces +Z, right = +X, up = +Y
        let panel_right = Vec3::X;
        let panel_up = Vec3::Y;

        let push = PanelPushConstants {
            cam_pos: scene_push.cam_pos,
            cam_right: [
                scene_push.cam_right[0],
                scene_push.cam_right[1],
                scene_push.cam_right[2],
                self.width,
            ],
            cam_up: [
                scene_push.cam_up[0],
                scene_push.cam_up[1],
                scene_push.cam_up[2],
                self.height,
            ],
            cam_fwd: scene_push.cam_fwd,
            fov: scene_push.fov,
            panel_center: [self.center.x, self.center.y, self.center.z, self.opacity],
            panel_right: [panel_right.x, panel_right.y, panel_right.z, 0.0],
            panel_up: [panel_up.x, panel_up.y, panel_up.z, 0.0],
        };

        let push_bytes = unsafe {
            std::slice::from_raw_parts(
                &push as *const PanelPushConstants as *const u8,
                std::mem::size_of::<PanelPushConstants>(),
            )
        };

        unsafe {
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                cmd,
                self.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                push_bytes,
            );
            device.cmd_draw(cmd, 4, 1, 0, 0); // triangle strip quad
        }
    }

    /// Ray-plane intersection test: given a ray origin and direction,
    /// compute UV coordinates where the ray hits the panel.
    /// Returns Some((u, v)) in [0,1] if the ray hits, None otherwise.
    pub fn hit_test(&self, ray_origin: Vec3, ray_dir: Vec3, _cam_pos: Vec3) -> Option<(f32, f32)> {
        // Panel is fixed in world space: faces +Z, right = +X, up = +Y
        let panel_right = Vec3::X;
        let panel_up = Vec3::Y;
        let panel_normal = Vec3::Z; // faces toward +Z (toward default camera)

        // Ray-plane intersection: t = dot(center - origin, normal) / dot(dir, normal)
        let denom = ray_dir.dot(panel_normal);
        if denom.abs() < 1e-6 {
            return None; // Ray parallel to panel
        }

        let t = (self.center - ray_origin).dot(panel_normal) / denom;
        if t < 0.0 {
            return None; // Hit is behind the ray
        }

        let hit = ray_origin + ray_dir * t;
        let local = hit - self.center;

        // Project onto panel axes
        let u = local.dot(panel_right) / self.width + 0.5;
        let v = 0.5 - local.dot(panel_up) / self.height; // flip Y for top-left origin

        if u >= 0.0 && u <= 1.0 && v >= 0.0 && v <= 1.0 {
            Some((u, v))
        } else {
            None
        }
    }

    pub fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_image_view(self.texture_view, None);
            device.unmap_memory(self.staging_memory);
            device.destroy_buffer(self.staging_buffer, None);
            device.free_memory(self.staging_memory, None);
            device.destroy_image(self.texture, None);
            device.free_memory(self.texture_memory, None);
        }
    }
}

/// Generate a placeholder launcher UI as RGBA pixels.
/// Dark background with colored game card rectangles and text placeholders.
pub fn generate_placeholder_ui(width: u32, height: u32, games: &[crate::game_scanner::Game]) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    // Dark background: #1a1a2e
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx] = 0x1a;     // R
            pixels[idx + 1] = 0x1a; // G
            pixels[idx + 2] = 0x2e; // B
            pixels[idx + 3] = 0xff; // A
        }
    }

    // Title bar area: slightly lighter
    let title_h = height / 10;
    for y in 0..title_h {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx] = 0x16;
            pixels[idx + 1] = 0x21;
            pixels[idx + 2] = 0x3e;
            pixels[idx + 3] = 0xff;
        }
    }

    // "ClearXR" accent line under title
    let accent_y = title_h;
    for x in 0..width {
        let idx = ((accent_y * width + x) * 4) as usize;
        pixels[idx] = 0x00;     // R - cyan accent
        pixels[idx + 1] = 0xe5; // G
        pixels[idx + 2] = 0xff; // B
        pixels[idx + 3] = 0xff;
    }

    // Game cards: grid of colored rectangles
    let card_cols = 4u32;
    let margin = width / 30;
    let card_w = (width - margin * (card_cols + 1)) / card_cols;
    let card_h = card_w * 3 / 4; // 4:3 aspect
    let start_y = title_h + margin * 2;

    let card_colors: &[[u8; 3]] = &[
        [0x0f, 0x3d, 0x6b], // deep blue
        [0x53, 0x1c, 0x4c], // purple
        [0x1b, 0x4d, 0x3e], // teal
        [0x4a, 0x35, 0x0a], // amber
        [0x3d, 0x0c, 0x0c], // red
        [0x0c, 0x2d, 0x48], // navy
        [0x2d, 0x1b, 0x4e], // violet
        [0x1a, 0x3a, 0x1a], // green
    ];

    let num_games = if games.is_empty() { 8 } else { games.len().min(12) };

    for i in 0..num_games {
        let col = (i as u32) % card_cols;
        let row = (i as u32) / card_cols;
        let x0 = margin + col * (card_w + margin);
        let y0 = start_y + row * (card_h + margin);

        let color = card_colors[i % card_colors.len()];

        // Card background
        for y in y0..(y0 + card_h).min(height) {
            for x in x0..(x0 + card_w).min(width) {
                let idx = ((y * width + x) * 4) as usize;
                // Slight gradient: lighter at top
                let t = (y - y0) as f32 / card_h as f32;
                let brighten = 1.0 + (1.0 - t) * 0.3;
                pixels[idx] = (color[0] as f32 * brighten).min(255.0) as u8;
                pixels[idx + 1] = (color[1] as f32 * brighten).min(255.0) as u8;
                pixels[idx + 2] = (color[2] as f32 * brighten).min(255.0) as u8;
                pixels[idx + 3] = 0xff;
            }
        }

        // "Title" bar at bottom of card (darker strip)
        let label_h = card_h / 5;
        let label_y = y0 + card_h - label_h;
        for y in label_y..(y0 + card_h).min(height) {
            for x in x0..(x0 + card_w).min(width) {
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx] = (color[0] as f32 * 0.5) as u8;
                pixels[idx + 1] = (color[1] as f32 * 0.5) as u8;
                pixels[idx + 2] = (color[2] as f32 * 0.5) as u8;
                pixels[idx + 3] = 0xff;
            }
        }
    }

    pixels
}

// ============================================================
// Panel graphics pipeline
// ============================================================

fn create_panel_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    descriptor_set_layout: vk::DescriptorSetLayout,
) -> Result<(vk::PipelineLayout, vk::Pipeline)> {
    let vert_spv = include_bytes!("../shaders/panel.vert.spv");
    let frag_spv = include_bytes!("../shaders/panel.frag.spv");

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

    // Push constants: 128 bytes (PanelPushConstants)
    let push_range = vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        offset: 0,
        size: std::mem::size_of::<PanelPushConstants>() as u32,
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

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo {
        topology: vk::PrimitiveTopology::TRIANGLE_STRIP,
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

    // Alpha blending: panel over scene
    let blend_attachment = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ZERO,
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
            .map_err(|(_, e)| anyhow::anyhow!("Panel pipeline creation failed: {:?}", e))?[0]
    };

    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    Ok((pipeline_layout, pipeline))
}
