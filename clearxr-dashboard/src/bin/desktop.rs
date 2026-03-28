//! Desktop preview app for the ClearXR dashboard.
//!
//! Runs the same egui UI in a native window so you can iterate without a headset.
//! Launch with: cargo run --bin clearxr-dashboard-desktop --features desktop

use clearxr_dashboard::config::Config;
use clearxr_dashboard::dashboard::{DashboardAction, LayerDashboard};
use clearxr_dashboard::game_scanner;
use clearxr_dashboard::screen_capture::ScreenCapture;

struct DashboardApp {
    dashboard: LayerDashboard,
    screen_capture: Option<ScreenCapture>,
    desktop_texture: Option<egui::TextureHandle>,
}

impl DashboardApp {
    fn new() -> Self {
        let games = game_scanner::scan_all();
        let config = Config::load();
        let mut dashboard = LayerDashboard::new(games, config);

        let screen_capture = match ScreenCapture::new() {
            Ok(sc) => {
                log::info!("[Desktop] Screen capture initialized.");
                Some(sc)
            }
            Err(e) => {
                log::warn!("[Desktop] Screen capture failed: {}", e);
                dashboard.set_desktop_error(format!("Screen capture unavailable: {e}"));
                None
            }
        };

        Self {
            dashboard,
            screen_capture,
            desktop_texture: None,
        }
    }
}

impl eframe::App for DashboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll screen capture and upload new frames as an egui texture
        if let Some(ref mut sc) = self.screen_capture {
            if sc.poll() {
                if let Some(frame) = sc.take_latest_frame() {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.data,
                    );
                    match self.desktop_texture.as_mut() {
                        Some(handle) => {
                            handle.set(image, egui::TextureOptions::LINEAR);
                        }
                        None => {
                            let handle = ctx.load_texture(
                                "desktop-capture",
                                image,
                                egui::TextureOptions::LINEAR,
                            );
                            self.desktop_texture = Some(handle);
                        }
                    }
                }
            }
        }

        // Update dashboard's desktop texture reference
        if let Some(ref handle) = self.desktop_texture {
            let size = handle.size();
            self.dashboard.set_desktop_texture_id(
                handle.id(),
                size[0] as u32,
                size[1] as u32,
            );
        }

        // Keep repainting when desktop tab is active (for live capture updates)
        if self.dashboard.is_desktop_active() && self.desktop_texture.is_some() {
            ctx.request_repaint();
        }

        // Render the dashboard UI
        let actions = self.dashboard.render(ctx);

        // Handle actions
        for action in actions {
            match action {
                DashboardAction::LaunchGame(app_id) => {
                    log::info!("[Desktop] LaunchGame({})", app_id);
                    let url = format!("steam://rungameid/{app_id}");
                    if let Err(e) = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &url])
                        .spawn()
                    {
                        log::error!("[Desktop] Failed to open {}: {}", url, e);
                    }
                }
                DashboardAction::SaveConfig => {
                    log::info!("[Desktop] Config saved.");
                }
            }
        }
    }
}

fn main() -> eframe::Result {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClearXR Dashboard")
            .with_inner_size([1024.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "ClearXR Dashboard",
        options,
        Box::new(|_cc| Ok(Box::new(DashboardApp::new()))),
    )
}
