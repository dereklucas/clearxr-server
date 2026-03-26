//! Unified Dashboard panel for ClearXR.
//!
//! Replaces 9 separate panels (launcher, desktop, toolbar, grab bar, FPS,
//! tray, settings, notification, keyboard) with ONE draggable panel that
//! renders ALL UI (tab bar, launcher grid, desktop chrome, settings, grab
//! bar, notifications) via a single EguiPanelRenderer into one LauncherPanel
//! Vulkan texture.

use ash::vk;
use glam::Vec3;
use anyhow::Result;
use log::info;

use crate::app::game_scanner::Game;
use crate::app::LaunchedApp;
use crate::capture::screen_capture::ScreenCapture;
use crate::config::Config;
use crate::launcher_panel::LauncherPanel;
use crate::panel::{PanelAnchor, PanelId, PanelTransform};
use crate::shell::boundary::Boundary;
use crate::shell::notifications::{Notification, NotificationQueue};
use crate::ui::egui_panel_renderer::EguiPanelRenderer;
use crate::vk_backend::VkBackend;

/// Panel ID for the main dashboard overlay.
pub const DASHBOARD_PANEL_ID: PanelId = PanelId::new(1);
/// Panel ID for the desktop screen-capture panel.
pub const SCREEN_PANEL_ID: PanelId = PanelId::new(2);

/// Active tab in the dashboard UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DashboardTab {
    /// Game launcher grid.
    Launcher,
    /// Desktop screen mirror.
    Desktop,
    /// Configuration settings.
    Settings,
}

/// Actions the dashboard can produce each frame.
pub enum DashboardAction {
    /// No action.
    None,
    /// Launch a Steam game by app ID.
    LaunchGame(u32),
    /// Toggle dashboard visibility.
    ToggleVisibility,
    /// Save the current configuration to disk.
    SaveConfig,
    /// Capture a screenshot.
    Screenshot,
    /// Cycle the panel anchor mode.
    CycleAnchor,
}

/// Unified VR dashboard: one egui frame rendering all UI (tabs, launcher, settings, grab bar).
pub struct Dashboard {
    /// Main dashboard panel (egui renders into this).
    pub panel: LauncherPanel,
    egui: EguiPanelRenderer,

    /// Screen capture panel (desktop tab background).
    pub screen_panel: LauncherPanel,
    /// Screen capture source for the desktop tab.
    pub screen_capture: ScreenCapture,

    /// Currently selected tab.
    pub active_tab: DashboardTab,
    /// Whether the dashboard is visible.
    pub visible: bool,
    click_pending: bool,

    /// Discovered games from Steam library scan.
    pub games: Vec<Game>,
    search: String,

    /// User-editable configuration (panel opacity, default view, etc.).
    pub config: Config,

    /// Grab offset from controller to panel center (None = not grabbed).
    pub grab_offset: Option<Vec3>,
    /// Index of the hand currently grabbing (0=left, 1=right), or None.
    pub grab_hand: Option<usize>,
    /// Panel yaw angle (radians) at grab start.
    pub grab_initial_yaw: f32,
    /// Panel pitch angle (radians) at grab start.
    pub grab_initial_pitch: f32,
    /// Panel distance from head at grab start.
    pub grab_initial_distance: f32,
    /// Controller aim yaw at grab start.
    pub grab_controller_start_yaw: f32,
    /// Controller aim pitch at grab start.
    pub grab_controller_start_pitch: f32,
    /// Controller distance from head at grab start.
    pub grab_controller_start_distance: f32,
    /// Panel width at grab start (for distance-based scaling).
    pub base_width: f32,
    /// Panel height at grab start (for distance-based scaling).
    pub base_height: f32,

    /// Notification toast queue.
    pub notifications: NotificationQueue,

    /// Currently launched game process, if any.
    pub launched_app: Option<LaunchedApp>,

    // FPS
    fps_timer: std::time::Instant,
    fps_frame_count: u32,
    fps_current: f32,

    /// Whether a screenshot was requested this frame.
    pub screenshot_requested: bool,
    /// Previous frame's both-triggers state (for edge detection).
    pub prev_both_triggers: bool,

    /// Current panel anchor mode.
    pub anchor: PanelAnchor,

    /// Play-space boundary configuration.
    pub boundary: Boundary,
    /// Whether the boundary proximity warning has been shown.
    pub boundary_warning_shown: bool,
}

impl Dashboard {
    /// Create the dashboard with game scanning, screen capture, and egui renderer.
    pub fn new(config: Config, vk: &VkBackend, render_pass: vk::RenderPass) -> Result<Self> {
        let width = 2048u32;
        let height = 1280u32;

        let panel = LauncherPanel::new(vk, render_pass, width, height, vk::Format::R8G8B8A8_SRGB)?;
        let egui = EguiPanelRenderer::new(vk, width, height)?;

        // Screen capture for desktop tab
        let screen_capture = ScreenCapture::new()?;
        let screen_w = screen_capture.screen_width();
        let screen_h = screen_capture.screen_height();
        let screen_panel =
            LauncherPanel::new(vk, render_pass, screen_w, screen_h, vk::Format::B8G8R8A8_SRGB)?;

        // Scan games
        let mut games = crate::app::game_scanner::scan_all();
        games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        info!(
            "Dashboard initialized: {}x{}, {} games",
            width,
            height,
            games.len()
        );

        let active_tab = match config.shell.default_view.as_str() {
            "desktop" => DashboardTab::Desktop,
            _ => DashboardTab::Launcher,
        };

        Ok(Self {
            panel,
            egui,
            screen_panel,
            screen_capture,
            active_tab,
            visible: true,
            click_pending: false,
            games,
            search: String::new(),
            config,
            grab_offset: None,
            grab_hand: None,
            grab_initial_yaw: 0.0,
            grab_initial_pitch: 0.0,
            grab_initial_distance: 2.5,
            grab_controller_start_yaw: 0.0,
            grab_controller_start_pitch: 0.0,
            grab_controller_start_distance: 0.0,
            base_width: 1.6,
            base_height: 1.0,
            notifications: NotificationQueue::new(3),
            launched_app: None,
            fps_timer: std::time::Instant::now(),
            fps_frame_count: 0,
            fps_current: 0.0,
            screenshot_requested: false,
            prev_both_triggers: false,
            anchor: PanelAnchor::World,
            boundary: Boundary::default(),
            boundary_warning_shown: false,
        })
    }

    /// Render ALL dashboard UI into the panel texture and return any actions.
    pub fn render(&mut self, vk: &VkBackend) -> Vec<DashboardAction> {
        let mut actions = Vec::new();

        // Update screen capture if desktop tab
        if self.active_tab == DashboardTab::Desktop {
            if let Some(frame) = self.screen_capture.try_get_frame() {
                self.screen_panel.stage_pixels(&frame.data).ok();
            }
        }

        // Tick notifications
        self.notifications.tick();

        // Update FPS
        self.fps_frame_count += 1;
        let fps_elapsed = self.fps_timer.elapsed().as_secs_f32();
        if fps_elapsed >= 0.5 {
            self.fps_current = self.fps_frame_count as f32 / fps_elapsed;
            self.fps_frame_count = 0;
            self.fps_timer = std::time::Instant::now();
        }

        let click = self.click_pending;
        self.click_pending = false;

        let active_tab = self.active_tab;
        let games = &self.games;
        let mut search = self.search.clone();
        let fps = self.fps_current;
        let config = &mut self.config;
        let notifications = &self.notifications;
        let mut new_tab = active_tab;
        let mut save_clicked = false;
        let mut launch_id: Option<u32> = None;

        let format = vk::Format::R8G8B8A8_SRGB;
        let is_desktop = active_tab == DashboardTab::Desktop;

        self.egui
            .run(vk, self.panel.texture, format, click, |ctx| {
                // Set transparent background for desktop tab compositing,
                // opaque dark for other tabs. This affects the render pass clear color.
                let bg = if is_desktop {
                    egui::Color32::TRANSPARENT
                } else {
                    egui::Color32::from_rgba_premultiplied(10, 10, 20, 255)
                };
                ctx.set_visuals(egui::Visuals {
                    panel_fill: bg,
                    window_fill: bg,
                    ..egui::Visuals::dark()
                });

                // Layout: [CONTENT on top] | [TAB BAR] | [GRAB BAR at bottom]
                // Bottom panels are declared first so CentralPanel gets the remaining space.

                // ---- Grab bar at very bottom ----
                egui::TopBottomPanel::bottom("grab_bar")
                    .exact_height(24.0)
                    .frame(egui::Frame::NONE) // transparent background, separate from tab bar
                    .show(ctx, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        let pill_rect = egui::Rect::from_center_size(
                            rect.center(),
                            egui::vec2(rect.width() * 0.25, 14.0),
                        );
                        let is_hovered = ui.rect_contains_pointer(pill_rect);
                        let color = if is_hovered {
                            egui::Color32::from_rgba_premultiplied(74, 158, 255, 220)
                        } else {
                            egui::Color32::from_rgba_premultiplied(96, 96, 112, 180)
                        };
                        ui.painter()
                            .rect_filled(pill_rect, pill_rect.height() / 2.0, color);
                    });

                // ---- Tab bar above grab bar ----
                egui::TopBottomPanel::bottom("tabs")
                    .exact_height(40.0)
                    .frame(egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(18, 18, 36, 230))
                        .inner_margin(egui::Margin::symmetric(12, 4)))
                    .show(ctx, |ui| {
                        ui.horizontal_centered(|ui| {
                            let tabs = [
                                (DashboardTab::Launcher, "LAUNCHER"),
                                (DashboardTab::Desktop, "DESKTOP"),
                                (DashboardTab::Settings, "SETTINGS"),
                            ];
                            for (tab, label) in tabs {
                                let color = if active_tab == tab {
                                    egui::Color32::WHITE
                                } else {
                                    egui::Color32::from_rgb(128, 128, 144)
                                };
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(label).size(14.0).color(color),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    new_tab = tab;
                                }
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{:.0} fps", fps))
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(0, 255, 96))
                                            .monospace(),
                                    );
                                },
                            );
                        });
                    });

                // ---- Notification overlay (if any) ----
                if let Some(notif) = notifications.visible().first() {
                    egui::Area::new(egui::Id::new("notification"))
                        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
                        .show(ctx, |ui| {
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgba_premultiplied(16, 24, 48, 230))
                                .corner_radius(6.0)
                                .inner_margin(8.0)
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(&notif.title)
                                            .size(14.0)
                                            .color(egui::Color32::WHITE)
                                            .strong(),
                                    );
                                    if !notif.body.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&notif.body).size(11.0).color(
                                                egui::Color32::from_rgb(180, 180, 200),
                                            ),
                                        );
                                    }
                                });
                        });
                }

                // ---- Active content (fills remaining space above tabs) ----
                egui::CentralPanel::default()
                    .frame(if is_desktop {
                        // Fully transparent for desktop — screen_panel shows through
                        egui::Frame::NONE
                    } else {
                        egui::Frame::NONE.fill(egui::Color32::from_rgba_premultiplied(10, 10, 20, 250))
                    })
                    .show(ctx, |ui| match active_tab {
                        DashboardTab::Launcher => {
                            render_launcher_content(ui, games, &mut search, &mut launch_id);
                        }
                        DashboardTab::Desktop => {
                            // Fully transparent — screen_panel behind shows through
                        }
                        DashboardTab::Settings => {
                            render_settings_content(ui, config, &mut save_clicked);
                        }
                    });
            });

        self.search = search;
        // Mark texture as initialized so record_draw() renders the panel
        self.panel.texture_initialized = true;

        // Apply tab change
        if new_tab != self.active_tab {
            self.active_tab = new_tab;
        }

        if save_clicked {
            self.config.save().ok();
            actions.push(DashboardAction::SaveConfig);
            self.notifications
                .push(Notification::success("Settings", "Saved"));
        }

        if let Some(app_id) = launch_id {
            actions.push(DashboardAction::LaunchGame(app_id));
        }

        actions
    }

    // ---- Pointer input ----

    /// Inject pointer position from controller ray-cast (UV coordinates).
    pub fn pointer_move(&mut self, u: f32, v: f32) {
        self.egui.pointer_move(u, v);
    }

    /// Clear pointer state (call at start of each frame).
    pub fn pointer_leave(&mut self) {
        self.egui.pointer_leave();
    }

    /// Signal a click on the dashboard.
    pub fn click(&mut self) {
        self.click_pending = true;
    }

    // ---- Panel access ----

    /// Returns mutable references to the panels that should be drawn this frame.
    /// screen_panel first (behind), then dashboard panel (in front, alpha-blended).
    pub fn panels_mut(&mut self) -> Vec<&mut LauncherPanel> {
        let mut panels = Vec::new();
        if self.visible {
            if self.active_tab == DashboardTab::Desktop {
                // Position screen_panel to match the CONTENT area of the dashboard
                // (above the tab bar + grab bar, which take up 64px out of 800px)
                let tab_grab_fraction = 64.0 / 1280.0; // tab(40) + grab(24)
                let content_height = self.panel.height * (1.0 - tab_grab_fraction);
                let content_offset_up = self.panel.height * tab_grab_fraction * 0.5;

                self.screen_panel.center = self.panel.center
                    + self.panel.up_dir * content_offset_up;
                self.screen_panel.width = self.panel.width;
                self.screen_panel.height = content_height;
                self.screen_panel.right_dir = self.panel.right_dir;
                self.screen_panel.up_dir = self.panel.up_dir;

                panels.push(&mut self.screen_panel);
            }
            panels.push(&mut self.panel);
        }
        panels
    }

    /// Returns a PanelTransform for InputDispatcher hit-testing.
    pub fn transform(&self) -> PanelTransform {
        PanelTransform {
            center: self.panel.center,
            right_dir: self.panel.right_dir,
            up_dir: self.panel.up_dir,
            width: self.panel.width,
            height: self.panel.height,
            opacity: self.panel.opacity,
            anchor: PanelAnchor::World,
            grabbable: false,
        }
    }

    /// Destroy all Vulkan resources.
    pub fn destroy(&mut self, vk: &crate::vk_backend::VkBackend) {
        let device = vk.device();
        self.panel.destroy(device, vk);
        self.screen_panel.destroy(device, vk);
        self.egui.destroy(device);
    }
}

// ============================================================
// Helper: Launcher tab content
// ============================================================

fn render_launcher_content(
    ui: &mut egui::Ui,
    games: &[Game],
    search_buf: &mut String,
    launch_app_id: &mut Option<u32>,
) {
    // Header
    ui.horizontal(|ui| {
        ui.heading(
            egui::RichText::new("ClearXR")
                .size(22.0)
                .color(egui::Color32::from_rgb(74, 158, 255)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let count = games.len();
            ui.label(
                egui::RichText::new(format!("{} games", count))
                    .size(13.0)
                    .color(egui::Color32::from_rgb(106, 112, 136)),
            );
        });
    });

    // Search bar
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Search:")
                .size(14.0)
                .color(egui::Color32::from_rgb(160, 160, 176)),
        );
        ui.add_sized(
            egui::vec2(ui.available_width(), 32.0),
            egui::TextEdit::singleline(search_buf).hint_text("Search games..."),
        );
    });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // Filter games by search
    let search_lower = search_buf.to_lowercase();
    let filtered: Vec<&Game> = games
        .iter()
        .filter(|g| search_buf.is_empty() || g.name.to_lowercase().contains(&search_lower))
        .collect();

    if filtered.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            if games.is_empty() {
                ui.label(
                    egui::RichText::new("No Steam games found")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
                ui.label(
                    egui::RichText::new("Install games via Steam to see them here.")
                        .size(14.0)
                        .color(egui::Color32::from_rgb(80, 80, 100)),
                );
            } else {
                ui.label(
                    egui::RichText::new("No matches")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
            }
        });
    } else {
        // Scrollable game grid
        egui::ScrollArea::vertical().show(ui, |ui| {
            let available_width = ui.available_width();
            let card_width = 200.0_f32;
            let cols = ((available_width / card_width) as usize).max(1);

            egui::Grid::new("game_grid")
                .num_columns(cols)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    for (i, game) in filtered.iter().enumerate() {
                        if i > 0 && i % cols == 0 {
                            ui.end_row();
                        }

                        // Game card
                        let response = ui.allocate_ui_with_layout(
                            egui::vec2(card_width - 12.0, 120.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                let rect = ui.available_rect_before_wrap();
                                let is_hovered = ui.rect_contains_pointer(rect);
                                let bg = if is_hovered {
                                    egui::Color32::from_rgb(30, 30, 56)
                                } else {
                                    egui::Color32::from_rgb(19, 19, 42)
                                };
                                ui.painter().rect_filled(rect, 8.0, bg);

                                // Game art placeholder
                                let hash = game
                                    .name
                                    .bytes()
                                    .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
                                let hue = (hash % 360) as f32;
                                let art_color = egui::Color32::from_rgb(
                                    (40.0 + 30.0 * (hue * 0.017).sin()) as u8,
                                    (30.0 + 20.0 * ((hue + 120.0) * 0.017).sin()) as u8,
                                    (50.0 + 30.0 * ((hue + 240.0) * 0.017).sin()) as u8,
                                );
                                let art_rect = egui::Rect::from_min_size(
                                    rect.min,
                                    egui::vec2(rect.width(), 70.0),
                                );
                                ui.painter().rect_filled(
                                    art_rect,
                                    egui::CornerRadius {
                                        nw: 8,
                                        ne: 8,
                                        sw: 0,
                                        se: 0,
                                    },
                                    art_color,
                                );

                                // Game initials in art area
                                let initials: String = game
                                    .name
                                    .split_whitespace()
                                    .take(2)
                                    .map(|w| w.chars().next().unwrap_or(' '))
                                    .collect();
                                ui.painter().text(
                                    art_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    &initials,
                                    egui::FontId::proportional(24.0),
                                    egui::Color32::from_white_alpha(60),
                                );

                                // Title
                                ui.add_space(74.0);
                                ui.label(
                                    egui::RichText::new(&game.name)
                                        .size(13.0)
                                        .color(egui::Color32::WHITE),
                                );

                                // Launch button on hover
                                if is_hovered {
                                    if ui.button("Play").clicked() {
                                        // Signal launch via the card click below
                                    }
                                }
                            },
                        );

                        // Detect click on the card
                        if response.response.clicked() {
                            *launch_app_id = Some(game.app_id);
                        }
                    }
                });
        });
    }
}

// ============================================================
// Helper: Settings tab content
// ============================================================

fn render_settings_content(ui: &mut egui::Ui, config: &mut Config, save_clicked: &mut bool) {
    ui.heading("Settings");
    ui.separator();

    egui::Grid::new("settings_grid")
        .num_columns(2)
        .spacing([20.0, 12.0])
        .show(ui, |ui| {
            ui.label("Default View");
            egui::ComboBox::from_id_salt("view")
                .selected_text(if config.shell.default_view == "desktop" {
                    "Desktop"
                } else {
                    "Launcher"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut config.shell.default_view,
                        "launcher".into(),
                        "Launcher",
                    );
                    ui.selectable_value(
                        &mut config.shell.default_view,
                        "desktop".into(),
                        "Desktop",
                    );
                });
            ui.end_row();

            ui.label("Panel Opacity");
            {
                let pct = format!("{:.0}%", config.panel.opacity * 100.0);
                ui.add(egui::Slider::new(&mut config.panel.opacity, 0.5..=1.0).text(pct));
            }
            ui.end_row();

            ui.label("Show FPS");
            ui.checkbox(&mut config.display.show_fps, "");
            ui.end_row();

            ui.label("Haptic Feedback");
            ui.checkbox(&mut config.shell.haptics_enabled, "");
            ui.end_row();
        });

    ui.separator();
    if ui.button("Save").clicked() {
        *save_clicked = true;
    }
}
