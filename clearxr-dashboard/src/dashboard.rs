//! Real ClearXR dashboard UI for the layer overlay.
//!
//! Ported from clearxr-space's dashboard, adapted for the layer host model.
//! This renders into the layer's overlay swapchain via EguiOverlayRenderer.

use crate::config::Config;
use crate::game_scanner::Game;
use crate::notifications::{Notification, NotificationQueue};
use crate::screen_capture::CaptureFrame;

/// Active tab in the dashboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DashboardTab {
    Launcher,
    Desktop,
    Settings,
}

/// Actions the dashboard can produce each frame.
#[derive(Debug)]
pub enum DashboardAction {
    LaunchGame(u32),
    SaveConfig,
}

/// Layer-hosted dashboard state and rendering.
pub struct LayerDashboard {
    games: Vec<Game>,
    search: String,
    active_tab: DashboardTab,
    config: Config,
    notifications: NotificationQueue,
    keyboard_visible: bool,
    keyboard_shift: bool,
    pending_keys: Vec<String>,
    visuals_set: bool,
    fps_timer: std::time::Instant,
    fps_frame_count: u32,
    fps_current: f32,
    /// Desktop capture texture handle, re-uploaded each frame a new capture arrives.
    desktop_texture: Option<egui::TextureHandle>,
    /// Error message if screen capture failed to initialize.
    desktop_error: Option<String>,
    /// Pending desktop frame data (BGRA, raw from capture). Stored here so the
    /// background thread can set it outside of ctx.run, then we apply it inside.
    pending_desktop_frame: Option<CaptureFrame>,
}

impl LayerDashboard {
    pub fn new(games: Vec<Game>, config: Config) -> Self {
        let active_tab = match config.shell.default_view.as_str() {
            "desktop" => DashboardTab::Desktop,
            "settings" => DashboardTab::Settings,
            _ => DashboardTab::Launcher,
        };
        Self {
            games,
            search: String::new(),
            active_tab,
            config,
            notifications: NotificationQueue::new(3),
            keyboard_visible: false,
            keyboard_shift: false,
            pending_keys: Vec::new(),
            fps_timer: std::time::Instant::now(),
            fps_frame_count: 0,
            fps_current: 0.0,
            desktop_texture: None,
            desktop_error: None,
            pending_desktop_frame: None,
            visuals_set: false,
        }
    }

    /// Set an error message for the desktop tab (if screen capture failed to init).
    pub fn set_desktop_error(&mut self, msg: String) {
        self.desktop_error = Some(msg);
    }

    /// Store a desktop capture frame for later upload. Call this outside of ctx.run().
    pub fn update_desktop_frame_data(&mut self, frame: CaptureFrame) {
        self.pending_desktop_frame = Some(frame);
    }

    /// Upload the pending desktop frame to egui. Call this inside ctx.run().
    pub fn apply_pending_desktop_frame(&mut self, ctx: &egui::Context) {
        if let Some(frame) = self.pending_desktop_frame.take() {
            self.update_desktop_frame(ctx, frame);
            // Force repaint so the desktop tab refreshes even when
            // the pointer isn't aimed at the panel (no input events).
            ctx.request_repaint();
        }
    }

    /// Upload a pre-processed RGBA desktop frame as an egui texture.
    /// The capture thread already handles BGRA→RGBA conversion and downscaling.
    pub fn update_desktop_frame(&mut self, ctx: &egui::Context, frame: CaptureFrame) {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let pixel_count = w * h;

        // Zero-copy reinterpret Vec<u8> as Vec<Color32>.
        // Desktop capture is always opaque (a=255), so RGBA bytes are already
        // premultiplied and match Color32's in-memory layout exactly.
        // This avoids the 60ms+ per-pixel loop of from_rgba_unmultiplied in debug builds.
        let pixels: Vec<egui::Color32> = unsafe {
            let mut data = frame.data;
            assert_eq!(data.len(), pixel_count * 4, "desktop frame size mismatch");
            let ptr = data.as_mut_ptr() as *mut egui::Color32;
            let cap = data.capacity() / 4;
            std::mem::forget(data);
            Vec::from_raw_parts(ptr, pixel_count, cap)
        };
        let image = egui::ColorImage { size: [w, h], pixels };

        match &mut self.desktop_texture {
            Some(handle) => {
                handle.set(image, egui::TextureOptions::LINEAR);
            }
            None => {
                self.desktop_texture =
                    Some(ctx.load_texture("desktop_capture", image, egui::TextureOptions::LINEAR));
            }
        }
    }

    /// Render the full dashboard UI and return any actions produced this frame.
    pub fn render(&mut self, ctx: &egui::Context) -> Vec<DashboardAction> {
        let mut actions = Vec::new();

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

        // Drain pending keyboard text
        let pending_text: Vec<String> = self.pending_keys.drain(..).collect();
        let _ = pending_text; // consumed via egui text injection in EguiOverlayRenderer

        let active_tab = self.active_tab;
        let games = &self.games;
        let mut search = self.search.clone();
        let fps = self.fps_current;
        let config = &mut self.config;
        let notifications = &self.notifications;
        let mut new_tab = active_tab;
        let mut save_clicked = false;
        let mut launch_id: Option<u32> = None;
        let mut kb_visible = self.keyboard_visible;
        let mut kb_shift = self.keyboard_shift;
        let mut kb_keys: Vec<String> = Vec::new();

        // Set dark background (once — calling every frame defeats repaint skipping)
        if !self.visuals_set {
            ctx.set_visuals(egui::Visuals {
                panel_fill: egui::Color32::from_rgba_premultiplied(10, 10, 20, 250),
                window_fill: egui::Color32::from_rgba_premultiplied(10, 10, 20, 250),
                ..egui::Visuals::dark()
            });
            self.visuals_set = true;
        }

        // ---- Grab bar at very bottom ----
        egui::TopBottomPanel::bottom("grab_bar")
            .exact_height(24.0)
            .frame(egui::Frame::NONE)
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
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(18, 18, 36, 230))
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
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

                    // FPS counter + layer badge on right
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.0} fps", fps))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(0, 255, 96))
                                    .monospace(),
                            );
                            ui.add_space(8.0);
                            // Layer badge — identifies this as layer-hosted
                            egui::Frame::new()
                                .fill(egui::Color32::from_rgba_premultiplied(255, 122, 48, 36))
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgb(255, 156, 88),
                                ))
                                .corner_radius(999.0)
                                .inner_margin(egui::Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("LAYER")
                                            .size(10.0)
                                            .color(egui::Color32::from_rgb(255, 240, 226)),
                                    );
                                });
                        },
                    );
                });
            });

        // ---- Virtual keyboard ----
        let focused_before_keyboard = ctx.memory(|m| m.focused());
        if kb_visible {
            egui::TopBottomPanel::bottom("keyboard")
                .exact_height(200.0)
                .frame(
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(24, 24, 48, 240))
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ctx, |ui| {
                    let rows: &[&str] =
                        &["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"];
                    for row in rows {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for ch in row.chars() {
                                let label = if kb_shift {
                                    ch.to_uppercase().to_string()
                                } else {
                                    ch.to_string()
                                };
                                let btn = ui.add_sized(
                                    egui::vec2(36.0, 36.0),
                                    egui::Button::new(
                                        egui::RichText::new(&label).size(16.0).monospace(),
                                    ),
                                );
                                if btn.clicked() {
                                    kb_keys.push(label);
                                    kb_shift = false;
                                }
                            }
                        });
                    }
                    // Bottom row: shift, space, backspace, hide
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if ui
                            .add_sized(
                                egui::vec2(60.0, 36.0),
                                egui::Button::new(egui::RichText::new("Shift").size(14.0)),
                            )
                            .clicked()
                        {
                            kb_shift = !kb_shift;
                        }
                        if ui
                            .add_sized(
                                egui::vec2(200.0, 36.0),
                                egui::Button::new(egui::RichText::new("SPACE").size(14.0)),
                            )
                            .clicked()
                        {
                            kb_keys.push(" ".into());
                        }
                        if ui
                            .add_sized(
                                egui::vec2(60.0, 36.0),
                                egui::Button::new(egui::RichText::new("Bksp").size(14.0)),
                            )
                            .clicked()
                        {
                            kb_keys.push("\x08".into());
                        }
                        if ui
                            .add_sized(
                                egui::vec2(60.0, 36.0),
                                egui::Button::new(egui::RichText::new("Hide").size(14.0)),
                            )
                            .clicked()
                        {
                            kb_visible = false;
                        }
                    });
                });
        }
        // Restore focus after keyboard clicks
        if !kb_keys.is_empty() {
            if let Some(id) = focused_before_keyboard {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }

        // ---- Notification overlay ----
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
                                    egui::RichText::new(&notif.body)
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(180, 180, 200)),
                                );
                            }
                        });
                });
        }

        // ---- Main content ----
        let desktop_texture = &self.desktop_texture;
        let desktop_error = &self.desktop_error;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgba_premultiplied(10, 10, 20, 250)))
            .show(ctx, |ui| match active_tab {
                DashboardTab::Launcher => {
                    render_launcher_content(ui, games, &mut search, &mut launch_id);
                }
                DashboardTab::Desktop => {
                    render_desktop_content(ui, desktop_texture, desktop_error);
                }
                DashboardTab::Settings => {
                    render_settings_content(ui, config, &mut save_clicked);
                }
            });

        // Apply state changes
        self.search = search;
        self.keyboard_visible = kb_visible;
        self.keyboard_shift = kb_shift;

        // Queue keyboard keys for next frame
        for key in kb_keys {
            self.pending_keys.push(key);
        }

        // Show keyboard when egui wants text input
        if ctx.wants_keyboard_input() {
            self.keyboard_visible = true;
        }

        // Apply tab change
        if new_tab != self.active_tab {
            self.active_tab = new_tab;
        }

        // Handle save
        if save_clicked {
            self.config.save().ok();
            actions.push(DashboardAction::SaveConfig);
            self.notifications
                .push(Notification::success("Settings", "Saved"));
        }

        // Handle game launch
        if let Some(app_id) = launch_id {
            if let Some(game) = self.games.iter().find(|g| g.app_id == app_id) {
                self.notifications
                    .push(Notification::info("Launching", &game.name));
            }
            actions.push(DashboardAction::LaunchGame(app_id));
        }

        actions
    }
}

// ============================================================
// Launcher tab
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
            ui.label(
                egui::RichText::new(format!("{} games", games.len()))
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

    // Filter
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

                                // Art placeholder
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

                                // Initials
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

                                // Play button on hover
                                if is_hovered {
                                    if ui.button("Play").clicked() {
                                        // Click handled by card response below
                                    }
                                }
                            },
                        );

                        if response.response.clicked() {
                            *launch_app_id = Some(game.app_id);
                        }
                    }
                });
        });
    }
}

// ============================================================
// Desktop tab
// ============================================================

fn render_desktop_content(
    ui: &mut egui::Ui,
    texture: &Option<egui::TextureHandle>,
    error: &Option<String>,
) {
    if let Some(err) = error {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(
                egui::RichText::new("Desktop capture unavailable")
                    .size(18.0)
                    .color(egui::Color32::from_rgb(255, 100, 100)),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .size(13.0)
                    .color(egui::Color32::from_rgb(180, 180, 200)),
            );
        });
        return;
    }

    match texture {
        Some(handle) => {
            let available = ui.available_size();
            let tex_size = handle.size_vec2();
            // Fit the image to available space, maintaining aspect ratio
            let scale = (available.x / tex_size.x).min(available.y / tex_size.y);
            let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);

            ui.vertical_centered(|ui| {
                // Center vertically
                let vertical_pad = (available.y - display_size.y) * 0.5;
                if vertical_pad > 0.0 {
                    ui.add_space(vertical_pad);
                }
                ui.image(egui::load::SizedTexture::new(handle.id(), display_size));
            });
        }
        None => {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(
                    egui::RichText::new("Connecting to desktop...")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
                ui.spinner();
            });
        }
    }
}

// ============================================================
// Settings tab
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
