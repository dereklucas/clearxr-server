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
    pub panel_right: [f32; 4],  // xyz = panel right axis (unit), w = dot_u (-1 = no dot)
    pub panel_up: [f32; 4],     // xyz = panel up axis (unit), w = dot_v
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
    _staging_size: vk::DeviceSize,

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
    /// Panel right axis in world space (default: +X)
    pub right_dir: Vec3,
    /// Panel up axis in world space (default: +Y)
    pub up_dir: Vec3,

    // Has the texture been uploaded at least once?
    pub texture_initialized: bool,

    // Has new pixel data been staged but not yet uploaded to the GPU?
    upload_pending: bool,

    // Vulkan format used for the texture image and view.
    _format: vk::Format,

    /// Pointer dot UV position on this panel (set per-frame, None = no dot).
    pub dot_uv: Option<(f32, f32)>,
}

unsafe impl Send for LauncherPanel {}
unsafe impl Sync for LauncherPanel {}

impl LauncherPanel {
    pub fn new(
        vk: &VkBackend,
        render_pass: vk::RenderPass,
        tex_width: u32,
        tex_height: u32,
        format: vk::Format,
    ) -> Result<Self> {
        let device = vk.device();

        // ---- Texture image ----
        let img_ci = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format,
            extent: vk::Extent3D {
                width: tex_width,
                height: tex_height,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT,
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
            _staging_size: staging_size,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            pipeline_layout,
            pipeline,
            center: Vec3::new(0.0, 1.6, -2.5),
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            texture_initialized: false,
            upload_pending: false,
            _format: format,
            dot_uv: None,
        })
    }

    /// Copy pixel data into the staging buffer (CPU side only, no GPU work).
    /// Call `record_upload` later to record the GPU transfer commands.
    pub fn stage_pixels(&mut self, pixels: &[u8]) -> Result<()> {
        let expected = (self.tex_width * self.tex_height * 4) as usize;
        if pixels.len() != expected {
            anyhow::bail!(
                "Panel pixel data size mismatch: got {}, expected {}",
                pixels.len(),
                expected
            );
        }
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.staging_mapped, pixels.len());
        }
        self.upload_pending = true;
        Ok(())
    }

    /// Record barrier + copy + barrier commands into an existing command buffer.
    /// Does nothing if no upload is pending. Does NOT allocate, submit, or wait.
    pub fn record_upload(&mut self, device: &ash::Device, cmd: vk::CommandBuffer) {
        if !self.upload_pending {
            return;
        }

        unsafe {
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
        }

        self.texture_initialized = true;
        self.upload_pending = false;
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

        let panel_right = self.right_dir;
        let panel_up = self.up_dir;

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
            panel_right: [panel_right.x, panel_right.y, panel_right.z, self.dot_uv.map_or(-1.0, |d| d.0)],
            panel_up: [panel_up.x, panel_up.y, panel_up.z, self.dot_uv.map_or(-1.0, |d| d.1)],
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
    /// Returns Some((u, v, t)) where u,v are in [0,1] and t is the ray distance, or None.
    #[allow(dead_code)] // Public API; InputDispatcher uses PanelTransform::hit_test instead
    pub fn hit_test(&self, ray_origin: Vec3, ray_dir: Vec3, _cam_pos: Vec3) -> Option<(f32, f32, f32)> {
        let panel_right = self.right_dir;
        let panel_up = self.up_dir;
        let panel_normal = panel_right.cross(panel_up); // derived from orientation

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
            Some((u, v, t))
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
