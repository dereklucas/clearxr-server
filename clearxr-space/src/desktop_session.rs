/// Desktop window mode – renders the scene to a native window using winit + Vulkan.
///
/// Bypasses OpenXR entirely. Uses WASD + mouse for camera control.

use anyhow::Result;
use ash::vk;
use glam::{Quat, Vec3};
use log::info;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::ffi::CString;
use std::sync::{atomic::AtomicBool, Arc};

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::game_scanner;
use crate::launcher_panel::LauncherPanel;
use crate::renderer::{
    create_pipeline, create_render_pass_with_layout, record_commands_open, HandData, HandUbo, PushConstants,
};
use crate::screen_capture::ScreenCapture;
use crate::ui_renderer::UiRenderer;
use crate::vk_backend::VkBackend;

/// What drives the panel texture.
enum PanelSource {
    /// HTML launcher UI via Ultralight
    Launcher {
        ui: UiRenderer,
    },
    /// Live desktop/window capture
    Screen {
        capture: ScreenCapture,
    },
}

pub fn run(keep_running: Arc<AtomicBool>, use_screen_capture: bool) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = DesktopApp::new(keep_running, use_screen_capture)?;
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct DesktopApp {
    keep_running: Arc<AtomicBool>,
    use_screen_capture: bool,
    state: Option<DesktopState>,
}

struct DesktopState {
    window: Window,
    vk: VkBackend,
    surface: vk::SurfaceKHR,
    surface_fn: ash::khr::surface::Instance,
    swapchain_fn: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_image_views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    extent: vk::Extent2D,
    color_format: vk::Format,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    hand_ubo: HandUbo,
    launcher: LauncherPanel,
    panel_source: PanelSource,
    // Camera state
    cam_pos: Vec3,
    cam_yaw: f32,   // radians
    cam_pitch: f32,  // radians
    keys_held: KeysHeld,
    mouse_captured: bool,
    mouse_clicking: bool,
    start_time: std::time::Instant,
    needs_swapchain_recreate: bool,
}

#[derive(Default)]
struct KeysHeld {
    forward: bool,
    back: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

impl DesktopApp {
    fn new(keep_running: Arc<AtomicBool>, use_screen_capture: bool) -> Result<Self> {
        Ok(Self {
            keep_running,
            use_screen_capture,
            state: None,
        })
    }
}

impl ApplicationHandler for DesktopApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match DesktopState::new(event_loop, self.use_screen_capture) {
            Ok(state) => self.state = Some(state),
            Err(e) => {
                log::error!("Failed to initialize desktop mode: {}", e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        match event {
            WindowEvent::CloseRequested => {
                self.keep_running.store(false, std::sync::atomic::Ordering::SeqCst);
                event_loop.exit();
            }
            WindowEvent::Resized(_) => {
                state.needs_swapchain_recreate = true;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(key) = event.physical_key {
                    match key {
                        KeyCode::KeyW => state.keys_held.forward = pressed,
                        KeyCode::KeyS => state.keys_held.back = pressed,
                        KeyCode::KeyA => state.keys_held.left = pressed,
                        KeyCode::KeyD => state.keys_held.right = pressed,
                        KeyCode::Space => state.keys_held.up = pressed,
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => state.keys_held.down = pressed,
                        KeyCode::Escape if pressed => {
                            if state.mouse_captured {
                                state.mouse_captured = false;
                                let _ = state.window.set_cursor_grab(winit::window::CursorGrabMode::None);
                            } else {
                                self.keep_running.store(false, std::sync::atomic::Ordering::SeqCst);
                                event_loop.exit();
                            }
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseInput { state: btn_state, button: MouseButton::Left, .. } => {
                if !state.mouse_captured && btn_state == ElementState::Pressed {
                    state.mouse_captured = true;
                    // Note: set_cursor_visible(false) crashes on macOS due to a winit bug
                    // in invisible_cursor::new_invisible. Just grab the cursor instead.
                    let _ = state.window.set_cursor_grab(winit::window::CursorGrabMode::Locked)
                        .or_else(|_| state.window.set_cursor_grab(winit::window::CursorGrabMode::Confined));
                } else if state.mouse_captured {
                    state.mouse_clicking = btn_state == ElementState::Pressed;
                }
            }
            WindowEvent::RedrawRequested => {
                state.update_camera();
                if let Err(e) = state.render_frame() {
                    log::error!("Render failed: {}", e);
                }
                state.window.request_redraw();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device_id: winit::event::DeviceId, event: DeviceEvent) {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            if state.mouse_captured {
                let sensitivity = 0.003;
                state.cam_yaw -= dx as f32 * sensitivity;
                state.cam_pitch = (state.cam_pitch - dy as f32 * sensitivity)
                    .clamp(-std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::FRAC_PI_2 - 0.01);
            }
        }
    }
}

impl DesktopState {
    fn new(event_loop: &ActiveEventLoop, use_screen_capture: bool) -> Result<Self> {
        let window_attrs = Window::default_attributes()
            .with_title("Clear XR – Desktop Mode")
            .with_inner_size(winit::dpi::PhysicalSize::new(1280u32, 720u32));
        let window = event_loop.create_window(window_attrs)?;

        // Get required Vulkan instance extensions for this window
        let required_extensions = ash_window::enumerate_required_extensions(
            window.display_handle()?.as_raw(),
        )?;
        let ext_cstrings: Vec<CString> = required_extensions
            .iter()
            .map(|&ext| unsafe { std::ffi::CStr::from_ptr(ext) }.to_owned())
            .collect();

        let vk = VkBackend::new_standalone(&ext_cstrings)?;

        // Create surface
        let surface = unsafe {
            ash_window::create_surface(
                vk.entry(),
                vk.instance_ref(),
                window.display_handle()?.as_raw(),
                window.window_handle()?.as_raw(),
                None,
            )?
        };
        let surface_fn = ash::khr::surface::Instance::new(vk.entry(), vk.instance_ref());
        let swapchain_fn = ash::khr::swapchain::Device::new(vk.instance_ref(), vk.device());

        // Verify surface support
        let supported = unsafe {
            surface_fn.get_physical_device_surface_support(
                vk.physical_device(),
                vk.queue_family_index(),
                surface,
            )?
        };
        if !supported {
            anyhow::bail!("Queue family does not support presentation");
        }

        // Create swapchain
        let (swapchain, swapchain_images, extent, color_format) =
            create_swapchain(&vk, &surface_fn, &swapchain_fn, surface, vk::SwapchainKHR::null())?;

        let render_pass = create_render_pass_with_layout(
            vk.device(), color_format, None, vk::ImageLayout::PRESENT_SRC_KHR,
        )?;

        // Create image views + framebuffers
        let (swapchain_image_views, framebuffers) =
            create_image_views_and_framebuffers(vk.device(), &swapchain_images, color_format, render_pass, extent)?;

        // Pipeline
        let hand_ubo = HandUbo::new(&vk)?;
        let (pipeline_layout, pipeline) =
            create_pipeline(vk.device(), render_pass, hand_ubo.descriptor_set_layout, false)?;

        // Panel texture source
        let (tex_w, tex_h, mut panel_source) = if use_screen_capture {
            let tex_w = 1920;
            let tex_h = 1080;
            let capture = ScreenCapture::new(tex_w, tex_h)?;
            info!("Screen capture mode: {}x{}", tex_w, tex_h);
            (tex_w, tex_h, PanelSource::Screen { capture })
        } else {
            let tex_w = 1024;
            let tex_h = 640;
            let games = game_scanner::scan_all();
            info!("Initializing launcher UI...");
            let html_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/launcher.html");
            let mut ui = UiRenderer::new(tex_w, tex_h, &html_path)?;
            if let Err(e) = ui.set_games(&games) {
                log::warn!("Failed to inject games into UI: {}", e);
            }
            info!("Launcher UI initialized ({}x{}, {} games)", tex_w, tex_h, games.len());
            (tex_w, tex_h, PanelSource::Launcher { ui })
        };

        let mut launcher = LauncherPanel::new(&vk, render_pass, tex_w, tex_h)?;

        // Initial texture upload
        match &mut panel_source {
            PanelSource::Launcher { ui } => {
                if let Some(pixels) = ui.update() {
                    launcher.upload_pixels(&vk, pixels)?;
                }
            }
            PanelSource::Screen { capture } => {
                if let Some(pixels) = capture.capture() {
                    launcher.upload_pixels(&vk, pixels)?;
                }
            }
        };

        // Command buffer
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(vk.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = unsafe { vk.device().allocate_command_buffers(&alloc_info) }?[0];

        let fence = unsafe { vk.device().create_fence(&vk::FenceCreateInfo::default(), None)? };
        let image_available =
            unsafe { vk.device().create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };
        let render_finished =
            unsafe { vk.device().create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? };

        info!("Desktop mode initialized: {}x{}, {:?}", extent.width, extent.height, color_format);

        window.request_redraw();

        Ok(Self {
            window,
            vk,
            surface,
            surface_fn,
            swapchain_fn,
            swapchain,
            swapchain_images,
            swapchain_image_views,
            framebuffers,
            extent,
            color_format,
            render_pass,
            pipeline_layout,
            pipeline,
            command_buffer,
            fence,
            image_available,
            render_finished,
            hand_ubo,
            launcher,
            panel_source,
            cam_pos: Vec3::new(0.0, 1.6, 0.0), // eye height
            cam_yaw: 0.0,
            cam_pitch: 0.0,
            keys_held: KeysHeld::default(),
            mouse_captured: false,
            mouse_clicking: false,
            start_time: std::time::Instant::now(),
            needs_swapchain_recreate: false,
        })
    }

    fn update_camera(&mut self) {
        let speed = 0.05_f32;
        let rot = Quat::from_euler(glam::EulerRot::YXZ, self.cam_yaw, self.cam_pitch, 0.0);
        let forward = rot * -Vec3::Z;
        let right = rot * Vec3::X;
        let up = Vec3::Y;

        let mut move_dir = Vec3::ZERO;
        if self.keys_held.forward { move_dir += forward; }
        if self.keys_held.back { move_dir -= forward; }
        if self.keys_held.right { move_dir += right; }
        if self.keys_held.left { move_dir -= right; }
        if self.keys_held.up { move_dir += up; }
        if self.keys_held.down { move_dir -= up; }

        if move_dir.length_squared() > 0.0 {
            self.cam_pos += move_dir.normalize() * speed;
        }
    }

    fn render_frame(&mut self) -> Result<()> {
        if self.needs_swapchain_recreate {
            self.needs_swapchain_recreate = false;
            self.recreate_swapchain()?;
        }

        if self.extent.width == 0 || self.extent.height == 0 {
            return Ok(());
        }

        let device = self.vk.device();

        // Acquire swapchain image
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
                self.recreate_swapchain()?;
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("Acquire image: {:?}", e)),
        };

        // Build push constants from camera
        let rot = Quat::from_euler(glam::EulerRot::YXZ, self.cam_yaw, self.cam_pitch, 0.0);
        let right = rot * Vec3::X;
        let up = rot * Vec3::Y;
        let forward = rot * -Vec3::Z;
        let time = self.start_time.elapsed().as_secs_f32();

        // Symmetric perspective FOV: 90 degrees vertical, aspect-adjusted horizontal
        let aspect = self.extent.width as f32 / self.extent.height as f32;
        let vfov_half = 45.0_f32.to_radians();
        let hfov_half = (vfov_half.tan() * aspect).atan();

        let push = PushConstants {
            cam_pos: [self.cam_pos.x, self.cam_pos.y, self.cam_pos.z, time],
            cam_right: [right.x, right.y, right.z, 0.0],
            cam_up: [up.x, up.y, up.z, 0.0],
            cam_fwd: [forward.x, forward.y, forward.z, 0.0],
            fov: [
                -hfov_half.tan(), // left (negative)
                hfov_half.tan(),  // right
                -vfov_half.tan(), // down (negative)
                vfov_half.tan(),  // up
            ],
        };

        // ---- Panel source update ----
        match &mut self.panel_source {
            PanelSource::Launcher { ui } => {
                // UI interaction: aim ray → panel hit test
                if let Some((u, v)) =
                    self.launcher.hit_test(self.cam_pos, forward, self.cam_pos)
                {
                    ui.mouse_move(u, v);
                    if self.mouse_clicking {
                        ui.mouse_click(u, v);
                    }
                }
                // Update UI (hot-reload check + re-render if dirty)
                if let Some(pixels) = ui.update() {
                    if let Err(e) = self.launcher.upload_pixels(&self.vk, pixels) {
                        log::error!("Panel texture upload failed: {}", e);
                    }
                }
            }
            PanelSource::Screen { capture } => {
                // Input injection: aim ray → panel hit → move/click real mouse
                if let Some((u, v)) =
                    self.launcher.hit_test(self.cam_pos, forward, self.cam_pos)
                {
                    capture.inject_mouse_move(u, v);
                    if self.mouse_clicking {
                        capture.inject_mouse_click(u, v);
                    }
                }
                // Grab a new frame from the desktop
                if let Some(pixels) = capture.capture() {
                    if let Err(e) = self.launcher.upload_pixels(&self.vk, pixels) {
                        log::error!("Screen capture upload failed: {}", e);
                    }
                }
            }
        }

        // Simulate a right controller held in front of camera, pointing forward
        let mut hand_data = HandData::default();
        // Position controller slightly below and to the right of camera
        let ctrl_pos = self.cam_pos + forward * 0.3 + right * 0.15 - up * 0.15;
        hand_data.active[3] = 1.0; // right controller active
        hand_data.ctrl_grip[1] = [ctrl_pos.x, ctrl_pos.y, ctrl_pos.z, 0.02];
        hand_data.ctrl_aim_pos[1] = [ctrl_pos.x, ctrl_pos.y, ctrl_pos.z, 0.0];
        hand_data.ctrl_aim_dir[1] = [forward.x, forward.y, forward.z, 0.0];
        hand_data.ctrl_grip_right[1] = [right.x, right.y, right.z, 0.0];
        hand_data.ctrl_grip_up[1] = [up.x, up.y, up.z, 0.0];
        // Map mouse click to trigger
        if self.mouse_clicking {
            hand_data.ctrl_inputs[1] = [1.0, 0.0, 0.0, 0.0]; // trigger pulled
        }
        self.hand_ubo.update(&hand_data);

        let fb = self.framebuffers[img_idx as usize];

        // Draw scene background (leaves render pass open)
        record_commands_open(
            device,
            self.command_buffer,
            self.render_pass,
            self.pipeline,
            self.pipeline_layout,
            fb,
            self.extent,
            &push,
            self.hand_ubo.descriptor_set,
            false,
        )?;

        // Draw launcher panel on top of scene
        self.launcher.record_draw(device, self.command_buffer, &push);

        // Close render pass + command buffer
        unsafe {
            device.cmd_end_render_pass(self.command_buffer);
            device.end_command_buffer(self.command_buffer)?;
        }

        // Submit
        let wait_sems = [self.image_available];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let cmds = [self.command_buffer];
        let signal_sems = [self.render_finished];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&wait_sems)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signal_sems);

        unsafe {
            device.reset_fences(&[self.fence])?;
            device.queue_submit(self.vk.queue(), &[submit], self.fence)?;
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

        match unsafe { self.swapchain_fn.queue_present(self.vk.queue(), &present) } {
            Ok(_) => {}
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR) => {
                self.recreate_swapchain()?;
            }
            Err(e) => log::warn!("Present: {:?}", e),
        }

        if suboptimal {
            self.recreate_swapchain()?;
        }

        Ok(())
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        let device = self.vk.device();
        unsafe { device.device_wait_idle()? };

        // Destroy old framebuffers and image views
        for &fb in &self.framebuffers {
            unsafe { device.destroy_framebuffer(fb, None) };
        }
        for &iv in &self.swapchain_image_views {
            unsafe { device.destroy_image_view(iv, None) };
        }

        let old_swapchain = self.swapchain;
        let (swapchain, images, extent, _format) =
            create_swapchain(&self.vk, &self.surface_fn, &self.swapchain_fn, self.surface, old_swapchain)?;

        unsafe { self.swapchain_fn.destroy_swapchain(old_swapchain, None) };

        let (image_views, framebuffers) =
            create_image_views_and_framebuffers(device, &images, self.color_format, self.render_pass, extent)?;

        self.swapchain = swapchain;
        self.swapchain_images = images;
        self.swapchain_image_views = image_views;
        self.framebuffers = framebuffers;
        self.extent = extent;

        info!("Desktop swapchain recreated: {}x{}", extent.width, extent.height);
        Ok(())
    }
}

impl Drop for DesktopState {
    fn drop(&mut self) {
        unsafe { self.vk.device().device_wait_idle().ok() };
        let device = self.vk.device();
        unsafe {
            device.destroy_semaphore(self.render_finished, None);
            device.destroy_semaphore(self.image_available, None);
            device.destroy_fence(self.fence, None);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            for &fb in &self.framebuffers {
                device.destroy_framebuffer(fb, None);
            }
            for &iv in &self.swapchain_image_views {
                device.destroy_image_view(iv, None);
            }
            device.destroy_render_pass(self.render_pass, None);
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.surface_fn.destroy_surface(self.surface, None);
        }
        self.launcher.destroy(device);
        self.hand_ubo.destroy(device);
    }
}

// ============================================================
// Helpers
// ============================================================

fn create_swapchain(
    vk: &VkBackend,
    surface_fn: &ash::khr::surface::Instance,
    swapchain_fn: &ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    old_swapchain: vk::SwapchainKHR,
) -> Result<(vk::SwapchainKHR, Vec<vk::Image>, vk::Extent2D, vk::Format)> {
    let caps = unsafe {
        surface_fn.get_physical_device_surface_capabilities(vk.physical_device(), surface)?
    };
    let formats = unsafe {
        surface_fn.get_physical_device_surface_formats(vk.physical_device(), surface)?
    };
    let present_modes = unsafe {
        surface_fn.get_physical_device_surface_present_modes(vk.physical_device(), surface)?
    };

    let format = formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_SRGB
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .or_else(|| formats.iter().find(|f| f.format == vk::Format::R8G8B8A8_SRGB))
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
        vk::Extent2D { width: 1280, height: 720 }
    };

    let image_count = (caps.min_image_count + 1).min(if caps.max_image_count > 0 {
        caps.max_image_count
    } else {
        u32::MAX
    });

    let sc_ci = vk::SwapchainCreateInfoKHR::default()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(format.format)
        .image_color_space(format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(caps.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true)
        .old_swapchain(old_swapchain);

    let swapchain = unsafe { swapchain_fn.create_swapchain(&sc_ci, None)? };
    let images = unsafe { swapchain_fn.get_swapchain_images(swapchain)? };

    Ok((swapchain, images, extent, format.format))
}

fn create_image_views_and_framebuffers(
    device: &ash::Device,
    images: &[vk::Image],
    format: vk::Format,
    render_pass: vk::RenderPass,
    extent: vk::Extent2D,
) -> Result<(Vec<vk::ImageView>, Vec<vk::Framebuffer>)> {
    let mut image_views = Vec::with_capacity(images.len());
    let mut framebuffers = Vec::with_capacity(images.len());

    for &image in images {
        let iv = unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
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
        image_views.push(iv);

        let attachments = [iv];
        let fb = unsafe {
            device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(&attachments)
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            )?
        };
        framebuffers.push(fb);
    }

    Ok((image_views, framebuffers))
}
