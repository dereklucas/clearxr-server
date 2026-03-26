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
use crate::input_pipe::{InputPipeServer, SpatialControllerPacket, SC_BTN_MENU};
use crate::renderer::HeadlessRenderer;
use crate::screen_capture::{CaptureFrame, ScreenCapture};
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

    // Input state
    let mut pointer_uv: Option<(f32, f32)> = None;
    let mut trigger = false;
    let mut secondary = false;
    let mut scroll_delta = 0.0f32;
    let mut menu_was_down = false;

    let target_interval = std::time::Duration::from_micros(13_889); // ~72fps

    log::info!("[ClearXR Dashboard] Render loop starting. screen_capture={}",
        if screen_capture.is_some() { "ok" } else { "failed" }
    );

    let mut diag_counter = 0u32;

    while keep_running.load(Ordering::Relaxed) {
        let frame_start = std::time::Instant::now();
        diag_counter += 1;

        // Read controller input from pipe
        let mut got_packet = false;
        if let Some(pkt) = pipe.try_read() {
            got_packet = true;
            if diag_counter <= 5 || diag_counter % 360 == 0 {
                let active = pkt.active_hands;
                let lx = pkt.left.pos_x; let ly = pkt.left.pos_y; let lz = pkt.left.pos_z;
                let rx = pkt.right.pos_x; let ry = pkt.right.pos_y; let rz = pkt.right.pos_z;
                log::info!(
                    "[ClearXR Dashboard] Pipe packet: active=0x{:02x} L_pos=[{:.2},{:.2},{:.2}] R_pos=[{:.2},{:.2},{:.2}]",
                    active, lx, ly, lz, rx, ry, rz,
                );
            }
            // Extract menu button for visibility toggle
            let left_menu = (pkt.active_hands & 0x01) != 0 && (pkt.left.buttons & SC_BTN_MENU) != 0;
            let right_menu = (pkt.active_hands & 0x02) != 0 && (pkt.right.buttons & SC_BTN_MENU) != 0;
            let menu_down = left_menu || right_menu;
            if menu_down && !menu_was_down {
                let visible = shm.header_ref().flags & 1 != 0;
                shm.set_visible(!visible);
            }
            menu_was_down = menu_down;

            // Ray-quad intersection for pointer input
            let result = ray_quad_from_packet(&pkt, &shm);
            pointer_uv = result.hit_uv;
            trigger = result.trigger;
            secondary = result.secondary;
            scroll_delta = result.scroll;

            if diag_counter <= 5 || diag_counter % 360 == 0 {
                let header = shm.header_ref();
                log::info!(
                    "[ClearXR Dashboard] Ray result: uv={:?} panel_pos=[{:.2},{:.2},{:.2}] panel_size=[{:.2},{:.2}]",
                    pointer_uv,
                    header.panel_pos[0], header.panel_pos[1], header.panel_pos[2],
                    header.panel_size[0], header.panel_size[1],
                );
            }
        }
        if !got_packet && diag_counter % 360 == 0 {
            log::info!("[ClearXR Dashboard] No pipe data this frame");
        }

        // Poll screen capture
        if let Some(ref mut sc) = screen_capture {
            if sc.poll() {
                if let Some(frame) = sc.latest_frame() {
                    let owned = CaptureFrame {
                        data: frame.data.clone(),
                        width: frame.width,
                        height: frame.height,
                    };
                    dashboard.update_desktop_frame_data(owned);
                }
            }
        }

        // Render egui frame
        let mut actions = Vec::new();
        let result = renderer.render_frame(
            pointer_uv,
            trigger,
            secondary,
            scroll_delta,
            |ctx| {
                dashboard.apply_pending_desktop_frame(ctx);
                actions = dashboard.render(ctx);
            },
        );
        scroll_delta = 0.0; // consumed

        match result {
            Ok(Some(pixels)) => {
                // New frame rendered — write to SHM
                shm.write_frame(pixels);
            }
            Ok(None) => {
                // No repaint needed, SHM content is still valid
            }
            Err(e) => {
                log::warn!("[ClearXR Dashboard] Render failed: {}", e);
            }
        }

        // Handle actions
        for action in actions {
            match action {
                dashboard::DashboardAction::LaunchGame(app_id) => {
                    log::info!("[ClearXR Dashboard] LaunchGame({})", app_id);
                    // TODO: signal streamer to launch via steam://rungameid/
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
    Ok(())
}

/// Result of ray-quad intersection against the dashboard panel.
struct RayResult {
    hit_uv: Option<(f32, f32)>,
    trigger: bool,
    secondary: bool,
    scroll: f32,
}

/// Perform ray-quad intersection using controller data from the opaque channel.
fn ray_quad_from_packet(pkt: &SpatialControllerPacket, shm: &ShmWriter) -> RayResult {
    let header = shm.header_ref();
    let panel_center = header.panel_pos;
    let q = header.panel_orient;
    let panel_size = header.panel_size;

    let panel_right = quat_rotate(&q, [1.0, 0.0, 0.0]);
    let panel_up = quat_rotate(&q, [0.0, 1.0, 0.0]);
    let panel_normal = cross(panel_right, panel_up);
    let half_w = panel_size[0] / 2.0;
    let half_h = panel_size[1] / 2.0;

    let mut best_hit: Option<(f32, f32, f32)> = None;
    let mut best_trigger = 0.0f32;
    let mut best_grip = 0.0f32;
    let mut best_thumbstick_y = 0.0f32;

    for (mask, hand) in [(0x01u8, pkt.left), (0x02u8, pkt.right)] {
        if pkt.active_hands & mask == 0 {
            continue;
        }
        let aim_pos = [hand.pos_x, hand.pos_y, hand.pos_z];
        let aim_rot = [hand.rot_x, hand.rot_y, hand.rot_z, hand.rot_w];
        let aim_dir = quat_rotate(&aim_rot, [0.0, 0.0, -1.0]);

        if let Some((u, v, t)) = ray_quad_hit(
            aim_pos, aim_dir, panel_center, panel_normal, panel_right, panel_up, half_w, half_h,
        ) {
            let is_closer = best_hit.map_or(true, |(_, _, prev_t)| t < prev_t);
            if is_closer {
                best_hit = Some((u, v, t));
                best_trigger = hand.trigger;
                best_grip = hand.grip;
                best_thumbstick_y = hand.thumbstick_y;
            }
        }
    }

    if let Some((u, v, _)) = best_hit {
        RayResult {
            hit_uv: Some((u, v)),
            trigger: best_trigger > 0.5,
            secondary: best_grip > 0.5,
            scroll: if best_thumbstick_y.abs() > 0.2 {
                best_thumbstick_y * 20.0
            } else {
                0.0
            },
        }
    } else {
        RayResult {
            hit_uv: None,
            trigger: false,
            secondary: false,
            scroll: 0.0,
        }
    }
}

// Add a helper to ShmWriter for reading the header back
impl ShmWriter {
    pub fn header_ref(&self) -> &shm::ShmHeader {
        unsafe { &*(self.ptr() as *const shm::ShmHeader) }
    }
}

// ============================================================
// Inline vector math (same as was in overlay.rs)
// ============================================================

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn quat_rotate(q: &[f32; 4], v: [f32; 3]) -> [f32; 3] {
    let qv = [q[0], q[1], q[2]];
    let w = q[3];
    let t = scale(cross(qv, v), 2.0);
    add(add(v, scale(t, w)), cross(qv, t))
}

fn ray_quad_hit(
    ray_origin: [f32; 3], ray_dir: [f32; 3],
    center: [f32; 3], normal: [f32; 3], right: [f32; 3], up: [f32; 3],
    half_w: f32, half_h: f32,
) -> Option<(f32, f32, f32)> {
    let denom = dot(ray_dir, normal);
    if denom.abs() < 1e-6 { return None; }
    let t = dot(sub(center, ray_origin), normal) / denom;
    if t < 0.0 { return None; }
    let hit = add(ray_origin, scale(ray_dir, t));
    let local = sub(hit, center);
    let u = dot(local, right) / (half_w * 2.0) + 0.5;
    let v = 0.5 - dot(local, up) / (half_h * 2.0);
    if u >= 0.0 && u <= 1.0 && v >= 0.0 && v <= 1.0 {
        Some((u, v, t))
    } else {
        None
    }
}
