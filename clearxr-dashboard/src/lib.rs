//! ClearXR Dashboard — headless egui renderer that shares frames via SHM.
//!
//! This crate is imported by clearxr-streamer. It owns the dashboard UI,
//! game scanning, screen capture, and GPU rendering. The rendered frames
//! are written to shared memory for the clearxr-layer to display.

pub mod config;
pub mod dashboard;
pub mod game_scanner;
pub mod input_pipe;
pub mod notifications;
pub mod renderer;
pub mod screen_capture;
pub mod shm;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::config::Config;
use crate::dashboard::LayerDashboard;
use crate::input_pipe::InputPipeServer;
use crate::renderer::HeadlessRenderer;
use crate::screen_capture::ScreenCapture;
use crate::shm::ShmWriter;

const DASHBOARD_WIDTH: u32 = 2048;
const DASHBOARD_HEIGHT: u32 = 1280;

/// Handle to the running dashboard service. Drop to stop.
pub struct DashboardService {
    keep_running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DashboardService {
    /// Start the dashboard render loop on a background thread.
    pub fn start() -> Result<Self, String> {
        let keep_running = Arc::new(AtomicBool::new(true));
        let kr = keep_running.clone();

        let thread = std::thread::Builder::new()
            .name("dashboard-render".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render_loop(kr)
                }));
                match result {
                    Ok(Ok(())) => log::info!("[ClearXR Dashboard] Render loop exited normally."),
                    Ok(Err(e)) => log::error!("[ClearXR Dashboard] Render loop failed: {}", e),
                    Err(panic) => {
                        let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = panic.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "unknown panic".to_string()
                        };
                        log::error!("[ClearXR Dashboard] Render loop PANICKED: {}", msg);
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn dashboard thread: {e}"))?;

        log::info!("[ClearXR Dashboard] Service started.");
        Ok(Self {
            keep_running,
            thread: Some(thread),
        })
    }

    /// Stop the render loop and wait for the thread to finish.
    pub fn stop(&mut self) {
        self.keep_running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
        log::info!("[ClearXR Dashboard] Service stopped.");
    }
}

impl Drop for DashboardService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Main render loop — runs on the dashboard thread.
fn render_loop(keep_running: Arc<AtomicBool>) -> Result<(), String> {
    // Set Windows timer resolution to 1ms (default is ~15.6ms, ruins sleep accuracy)
    #[cfg(target_os = "windows")]
    {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(uPeriod: u32) -> u32;
        }
        unsafe { timeBeginPeriod(1); }
    }

    // Initialize GPU renderer
    let mut renderer = HeadlessRenderer::new(DASHBOARD_WIDTH, DASHBOARD_HEIGHT)
        .map_err(|e| format!("Renderer init failed: {e}"))?;

    // Create shared memory
    let shm = ShmWriter::create(DASHBOARD_WIDTH, DASHBOARD_HEIGHT)
        .map_err(|e| format!("SHM create failed: {e}"))?;

    // Create named pipe for controller input
    let mut pipe = InputPipeServer::create()
        .map_err(|e| format!("Pipe create failed: {e}"))?;

    // Initialize dashboard state
    let games = game_scanner::scan_all();
    let config = Config::load();
    let mut dashboard = LayerDashboard::new(games, config);

    // Screen capture
    let mut screen_capture: Option<ScreenCapture> = match ScreenCapture::new() {
        Ok(sc) => {
            log::info!("[ClearXR Dashboard] Screen capture initialized.");
            Some(sc)
        }
        Err(e) => {
            log::warn!("[ClearXR Dashboard] Screen capture failed: {}", e);
            dashboard.set_desktop_error("DXGI capture unavailable.".into());
            None
        }
    };

    // Input state — the layer computes UV hit via ray-quad intersection
    // and sends pre-computed results. Dashboard just feeds them to egui.
    let mut pointer_uv: Option<(f32, f32)> = None;
    let mut trigger = false;
    let mut secondary = false;
    let mut scroll_delta = 0.0f32;
    let mut raw_thumbstick_y = 0.0f32;
    let mut prev_trigger = false;
    let mut prev_secondary = false;

    let target_interval = std::time::Duration::from_micros(13_889); // ~72fps

    log::info!("[ClearXR Dashboard] Render loop starting. screen_capture={}",
        if screen_capture.is_some() { "ok" } else { "failed" }
    );

    while keep_running.load(Ordering::Relaxed) {
        let frame_start = std::time::Instant::now();

        // Read pre-computed input from the layer (UV + buttons, no spatial math needed)
        if let Some(pkt) = pipe.try_read() {
            if pkt.flags & 0x01 != 0 {
                pointer_uv = Some((pkt.pointer_u, pkt.pointer_v));
            } else {
                pointer_uv = None;
            }
            trigger = pkt.trigger > 0.5;
            secondary = pkt.grip > 0.5;
            raw_thumbstick_y = pkt.thumbstick_y;
            scroll_delta = if raw_thumbstick_y.abs() > 0.2 {
                raw_thumbstick_y * 20.0
            } else {
                0.0
            };
        }

        // Poll screen capture — upload directly to GPU staging buffer (bypasses egui's
        // set_textures which does vkQueueWaitIdle). The renderer owns the Vulkan texture.
        if let Some(ref mut sc) = screen_capture {
            if sc.poll() {
                if let Some(frame) = sc.take_latest_frame() {
                    let tex_id = renderer.update_desktop_pixels(&frame.data, frame.width, frame.height);
                    dashboard.set_desktop_texture_id(tex_id, frame.width, frame.height);
                }
            }
        }

        // Render egui frame
        let has_desktop_tex = renderer.desktop_texture_id().is_some();
        let mut actions = Vec::new();
        let result = renderer.render_frame(
            pointer_uv,
            trigger,
            secondary,
            scroll_delta,
            |ctx| {
                // Request repaint when desktop tab is active to keep the mirror updating
                if dashboard.is_desktop_active() && has_desktop_tex {
                    ctx.request_repaint();
                }
                actions = dashboard.render(ctx);
            },
        );
        scroll_delta = 0.0; // consumed

        match result {
            Ok(Some(pixels)) => shm.write_frame(pixels),
            Ok(None) => {}
            Err(e) => log::warn!("[ClearXR Dashboard] Render failed: {}", e),
        }

        // Mouse injection — when Desktop tab is active, map panel UV to screen
        // coordinates and inject native mouse events (same pattern as Space).
        if dashboard.is_desktop_active() {
            if let Some(ref sc) = screen_capture {
                if let (Some((pu, pv)), Some(img_rect)) = (pointer_uv, dashboard.desktop_image_rect()) {
                    // Convert panel UV to pixel coords within the egui canvas
                    let px = pu * DASHBOARD_WIDTH as f32;
                    let py = pv * DASHBOARD_HEIGHT as f32;

                    // Check if pointer is within the desktop image rect
                    if px >= img_rect.min.x && px <= img_rect.max.x
                        && py >= img_rect.min.y && py <= img_rect.max.y
                    {
                        // Normalize to [0,1] within the image (= screen UV)
                        let screen_u = (px - img_rect.min.x) / img_rect.width();
                        let screen_v = (py - img_rect.min.y) / img_rect.height();

                        sc.inject_mouse_move(screen_u, screen_v);

                        // Trigger → left click (edge-detect)
                        if trigger && !prev_trigger {
                            sc.inject_mouse_down(screen_u, screen_v);
                        } else if !trigger && prev_trigger {
                            sc.inject_mouse_up();
                        }
                        // Grip → right click (edge-detect)
                        if secondary && !prev_secondary {
                            sc.inject_right_mouse_down(screen_u, screen_v);
                        } else if !secondary && prev_secondary {
                            sc.inject_right_mouse_up();
                        }
                        // Thumbstick → scroll
                        if raw_thumbstick_y.abs() > 0.2 {
                            sc.inject_scroll(-(raw_thumbstick_y * 3.0) as i32);
                        }
                    }
                } else {
                    // Pointer left — release any held buttons
                    if prev_trigger {
                        sc.inject_mouse_up();
                    }
                    if prev_secondary {
                        sc.inject_right_mouse_up();
                    }
                }
            }
        }
        prev_trigger = trigger;
        prev_secondary = secondary;

        // Handle actions
        for action in actions {
            match action {
                dashboard::DashboardAction::LaunchGame(app_id) => {
                    log::info!("[ClearXR Dashboard] LaunchGame({})", app_id);
                }
                dashboard::DashboardAction::SaveConfig => {
                    log::info!("[ClearXR Dashboard] Config saved.");
                }
            }
        }

        // Sleep to target framerate
        let elapsed = frame_start.elapsed();
        if elapsed < target_interval {
            std::thread::sleep(target_interval - elapsed);
        }
    }

    log::info!("[ClearXR Dashboard] Render loop exiting.");

    #[cfg(target_os = "windows")]
    {
        #[link(name = "winmm")]
        extern "system" {
            fn timeEndPeriod(uPeriod: u32) -> u32;
        }
        unsafe { timeEndPeriod(1); }
    }

    Ok(())
}
