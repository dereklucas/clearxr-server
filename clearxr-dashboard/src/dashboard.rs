//! Real ClearXR dashboard UI for the layer overlay.
//!
//! Ported from clearxr-space's dashboard, adapted for the layer host model.
//! This renders into the layer's overlay swapchain via EguiOverlayRenderer.

use std::collections::HashMap;

use crate::config::Config;
use crate::game_scanner::Game;
use crate::notifications::{Notification, NotificationQueue};

fn debug_rect(ui: &egui::Ui, rect: egui::Rect, color: egui::Color32, enabled: bool) {
    if enabled {
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, color), epaint::StrokeKind::Inside);
    }
}


/// Active tab in the dashboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DashboardTab {
    CurrentApp,
    Launcher,
    Desktop,
    Settings,
}

/// Actions the dashboard can produce each frame.
#[derive(Debug)]
pub enum DashboardAction {
    LaunchGame(u32),
    SaveConfig,
    Resume,
    QuitApp,
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
    /// Desktop capture texture — user-managed Vulkan texture registered with egui.
    /// Tuple of (TextureId, width, height). The renderer owns the actual GPU resources.
    desktop_texture_id: Option<(egui::TextureId, u32, u32)>,
    /// Error message if screen capture failed to initialize.
    desktop_error: Option<String>,
    /// Rect of the desktop image within the egui canvas (set during render).
    /// Used for mapping panel UV → screen UV for mouse injection.
    desktop_image_rect: Option<egui::Rect>,
    /// Cached game art textures, keyed by app_id.
    game_textures: HashMap<u32, egui::TextureHandle>,
    game_textures_loaded: bool,
    /// Name of the currently running app (shown as a tab). None = no app running.
    current_app: Option<String>,
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
            desktop_texture_id: None,
            desktop_error: None,
            desktop_image_rect: None,
            visuals_set: false,
            game_textures: HashMap::new(),
            game_textures_loaded: false,
            current_app: None,
        }
    }

    /// Set an error message for the desktop tab (if screen capture failed to init).
    pub fn set_desktop_error(&mut self, msg: String) {
        self.desktop_error = Some(msg);
    }

    /// Whether the Desktop tab is currently active.
    pub fn is_desktop_active(&self) -> bool {
        self.active_tab == DashboardTab::Desktop
    }

    /// The desktop image rect within the egui canvas (for UV mapping).
    pub fn desktop_image_rect(&self) -> Option<egui::Rect> {
        self.desktop_image_rect
    }

    /// Set the desktop texture ID (managed by HeadlessRenderer, not egui).
    pub fn set_desktop_texture_id(&mut self, id: egui::TextureId, width: u32, height: u32) {
        self.desktop_texture_id = Some((id, width, height));
    }

    /// Set the currently running app name. Pass None to clear.
    pub fn set_current_app(&mut self, name: Option<String>) {
        if name.is_some() && self.current_app.is_none() {
            self.active_tab = DashboardTab::CurrentApp;
        }
        self.current_app = name;
        if self.current_app.is_none() && self.active_tab == DashboardTab::CurrentApp {
            self.active_tab = DashboardTab::Launcher;
        }
    }

    /// Load game art textures from disk (called once on first render).
    fn load_game_textures(&mut self, ctx: &egui::Context) {
        for game in &self.games {
            if let Some(path) = &game.art_path {
                match image::open(path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let pixels = rgba.into_raw();
                        let color_image =
                            egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                        let handle = ctx.load_texture(
                            format!("game-{}", game.app_id),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.game_textures.insert(game.app_id, handle);
                    }
                    Err(e) => {
                        log::warn!("[Dashboard] Failed to load art for {}: {}", game.name, e);
                    }
                }
            }
        }
        log::info!(
            "[Dashboard] Loaded {} / {} game art textures",
            self.game_textures.len(),
            self.games.len()
        );
    }

    /// Render the full dashboard UI and return any actions produced this frame.
    pub fn render(&mut self, ctx: &egui::Context) -> Vec<DashboardAction> {
        let mut actions = Vec::new();

        // Load game art on first render (needs ctx for texture creation)
        if !self.game_textures_loaded {
            self.load_game_textures(ctx);
            self.game_textures_loaded = true;
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
        let mut desktop_image_rect_out: Option<egui::Rect> = None;
        let dbg = config.display.debug_borders;

        // Responsive scale: 1.0 at 1024px wide, scales linearly with window width.
        let s = (ctx.screen_rect().width() / 1024.0).clamp(0.7, 2.5);

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
            .exact_height(24.0 * s)
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let pill_rect = egui::Rect::from_center_size(
                    rect.center(),
                    egui::vec2(rect.width() * 0.12, 5.0 * s),
                );
                // Hit area is the full panel height, wider than the visual pill
                let hit_rect = egui::Rect::from_center_size(
                    rect.center(),
                    egui::vec2(rect.width() * 0.4, rect.height()),
                );
                let is_hovered = ui.rect_contains_pointer(hit_rect);
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
            .exact_height(44.0 * s)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(22, 22, 40, 245))
                    .inner_margin(egui::Margin::symmetric((16.0 * s) as i8, (6.0 * s) as i8)),
            )
            .show(ctx, |ui| {
                // Top edge highlight for visual separation from content
                let bar_rect = ui.max_rect();
                ui.painter().line_segment(
                    [bar_rect.left_top(), bar_rect.right_top()],
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 18)),
                );
                ui.horizontal_centered(|ui| {
                    // Build tab list — CurrentApp only shown when an app is running
                    let current_app_name = self.current_app.clone();
                    let mut tabs: Vec<(DashboardTab, String)> = Vec::new();
                    if let Some(ref name) = current_app_name {
                        tabs.push((DashboardTab::CurrentApp, name.to_uppercase()));
                    }
                    tabs.push((DashboardTab::Launcher, "LAUNCHER".into()));
                    tabs.push((DashboardTab::Desktop, "DESKTOP".into()));
                    tabs.push((DashboardTab::Settings, "SETTINGS".into()));

                    for (tab, label) in &tabs {
                        let is_active = active_tab == *tab;
                        let color = if is_active {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(120, 124, 148)
                        };
                        // Title case: first char uppercase, rest lowercase
                        let display_label = {
                            let mut c = label.chars();
                            match c.next() {
                                None => String::new(),
                                Some(f) => f.to_uppercase().to_string() + &c.as_str().to_lowercase(),
                            }
                        };
                        let btn = egui::Button::new(
                            egui::RichText::new(&display_label).size(13.0 * s).color(color),
                        )
                        .frame(false)
                        .min_size(egui::vec2(80.0 * s, 32.0 * s));
                        let btn_response = ui.add(btn);
                        // Active tab indicator — visionOS-style pill highlight behind active tab
                        if is_active {
                            let r = btn_response.rect;
                            let pill = r.shrink2(egui::vec2(6.0 * s, 4.0 * s));
                            ui.painter().rect_filled(
                                pill,
                                pill.height() / 2.0,
                                egui::Color32::from_rgba_premultiplied(74, 158, 255, 110),
                            );
                            ui.painter().rect_stroke(
                                pill,
                                pill.height() / 2.0,
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(100, 180, 255, 180)),
                                epaint::StrokeKind::Inside,
                            );
                        }
                        debug_rect(ui, btn_response.rect, egui::Color32::from_rgb(255, 128, 0), dbg);
                        if btn_response.clicked() {
                            new_tab = *tab;
                        }
                    }

                    // FPS counter + layer badge on right
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.0} fps", fps))
                                    .size(11.0 * s)
                                    .color(egui::Color32::from_rgb(100, 106, 130))
                                    .monospace(),
                            );
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
        let desktop_texture_id = self.desktop_texture_id;
        let desktop_error = &self.desktop_error;

        let game_textures = &self.game_textures;
        let current_app = &self.current_app;
        let mut resume_clicked = false;
        let mut quit_clicked = false;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(10, 10, 20, 250))
                    .inner_margin(egui::Margin::symmetric((24.0 * s) as i8, (16.0 * s) as i8)),
            )
            .show(ctx, |ui| match active_tab {
                DashboardTab::CurrentApp => {
                    render_current_app_content(ui, current_app, &mut resume_clicked, &mut quit_clicked, s);
                }
                DashboardTab::Launcher => {
                    render_launcher_content(ui, games, &mut search, &mut launch_id, game_textures, dbg, s);
                }
                DashboardTab::Desktop => {
                    desktop_image_rect_out = render_desktop_content(ui, desktop_texture_id, desktop_error, s);
                }
                DashboardTab::Settings => {
                    render_settings_content(ui, config, &mut save_clicked, s);
                }
            });

        // Store desktop image rect for mouse injection coordinate mapping
        self.desktop_image_rect = desktop_image_rect_out;

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

        // Handle current app actions
        if resume_clicked {
            actions.push(DashboardAction::Resume);
        }
        if quit_clicked {
            actions.push(DashboardAction::QuitApp);
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
    game_textures: &HashMap<u32, egui::TextureHandle>,
    dbg: bool,
    s: f32,
) {
    // Header
    ui.add_space(8.0 * s);
    let header_response = ui.horizontal(|ui| {
        debug_rect(ui, ui.available_rect_before_wrap(), egui::Color32::from_rgb(0, 255, 255), dbg);
        ui.vertical(|ui| {
            ui.heading(
                egui::RichText::new("ClearXR")
                    .size(26.0 * s)
                    .color(egui::Color32::from_rgb(74, 158, 255)),
            );
            ui.label(
                egui::RichText::new("Game Library")
                    .size(12.0 * s)
                    .color(egui::Color32::from_rgb(145, 150, 178)),
            );
        });
        ui.add_space(16.0 * s);
        let search_width = (ui.available_width() * 0.45).min(320.0 * s);
        let search_height = 34.0 * s;
        let search_cr = search_height / 2.0;
        let search_response = ui.add_sized(
            egui::vec2(search_width, search_height),
            egui::TextEdit::singleline(search_buf)
                .hint_text("Search games...")
                .text_color(egui::Color32::WHITE)
                .background_color(egui::Color32::from_rgba_premultiplied(20, 20, 42, 200)),
        );
        // Rounded inset border around search field
        ui.painter().rect_stroke(
            search_response.rect,
            search_cr,
            egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(255, 255, 255, 28)),
            epaint::StrokeKind::Inside,
        );
        debug_rect(ui, search_response.rect, egui::Color32::from_rgb(255, 0, 255), dbg);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{} games", games.len()))
                    .size(13.0 * s)
                    .color(egui::Color32::from_rgb(110, 114, 140)),
            );
        });
    });
    debug_rect(ui, header_response.response.rect, egui::Color32::from_rgb(0, 255, 255), dbg);

    ui.add_space(20.0 * s);

    // Filter
    let search_lower = search_buf.to_lowercase();
    let filtered: Vec<&Game> = games
        .iter()
        .filter(|g| search_buf.is_empty() || g.name.to_lowercase().contains(&search_lower))
        .collect();

    if filtered.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0 * s);
            if games.is_empty() {
                ui.label(
                    egui::RichText::new("No Steam games found")
                        .size(18.0 * s)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
                ui.label(
                    egui::RichText::new("Install games via Steam to see them here.")
                        .size(14.0 * s)
                        .color(egui::Color32::from_rgb(80, 80, 100)),
                );
            } else {
                ui.label(
                    egui::RichText::new("No matches")
                        .size(18.0 * s)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
            }
        });
    } else {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let spacing = 16.0 * s;
            let available_width = ui.available_width();
            let min_card_width = 200.0 * s;
            let cols = ((available_width + spacing) / (min_card_width + spacing)).max(1.0) as usize;
            let card_width = (available_width - spacing * (cols as f32 - 1.0)) / cols as f32;
            let art_height = (card_width / 2.15).round();
            let card_height = art_height + 42.0 * s;

            egui::Grid::new("game_grid")
                .min_col_width(card_width)
                .min_row_height(card_height)
                .spacing([spacing, spacing])
                .show(ui, |ui| {
                    for (i, game) in filtered.iter().enumerate() {
                        if i > 0 && i % cols == 0 {
                            ui.end_row();
                        }

                        let response = ui.allocate_ui_with_layout(
                            egui::vec2(card_width, card_height),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                let rect = ui.available_rect_before_wrap();
                                let is_hovered = ui.rect_contains_pointer(rect);

                                // Card background
                                let bg = if is_hovered {
                                    egui::Color32::from_rgb(38, 38, 72)
                                } else {
                                    egui::Color32::from_rgb(24, 24, 50)
                                };
                                let cr = 8.0 * s;
                                ui.painter().rect_filled(rect, cr, bg);

                                // Card border — visible at rest, accented on hover
                                let stroke_color = if is_hovered {
                                    egui::Color32::from_rgba_premultiplied(90, 170, 255, 160)
                                } else {
                                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 55)
                                };
                                ui.painter().rect_stroke(rect, cr, egui::Stroke::new(1.0, stroke_color), epaint::StrokeKind::Inside);

                                // Debug: card allocation rect (red)
                                debug_rect(ui, rect, egui::Color32::RED, dbg);

                                let art_rect = egui::Rect::from_min_size(
                                    rect.min,
                                    egui::vec2(rect.width(), art_height),
                                );
                                let cri = cr as u8;
                                let top_rounding = egui::CornerRadius {
                                    nw: cri,
                                    ne: cri,
                                    sw: 0,
                                    se: 0,
                                };

                                if let Some(tex) = game_textures.get(&game.app_id) {
                                    ui.painter().rect_filled(art_rect, top_rounding, egui::Color32::BLACK);
                                    ui.painter().with_clip_rect(art_rect).image(
                                        tex.id(),
                                        art_rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                } else {
                                    // Fallback: colored placeholder with initials
                                    let hash = game
                                        .name
                                        .bytes()
                                        .fold(0u32, |acc, b| {
                                            acc.wrapping_mul(31).wrapping_add(b as u32)
                                        });
                                    let hue = (hash % 360) as f32;
                                    let art_color = egui::Color32::from_rgb(
                                        (40.0 + 30.0 * (hue * 0.017).sin()) as u8,
                                        (30.0 + 20.0 * ((hue + 120.0) * 0.017).sin()) as u8,
                                        (50.0 + 30.0 * ((hue + 240.0) * 0.017).sin()) as u8,
                                    );
                                    ui.painter().rect_filled(art_rect, top_rounding, art_color);

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
                                        egui::FontId::proportional(24.0 * s),
                                        egui::Color32::from_white_alpha(60),
                                    );
                                }

                                // Debug: art rect (green)
                                debug_rect(ui, art_rect, egui::Color32::GREEN, dbg);

                                // Hover overlay on art (play icon)
                                if is_hovered {
                                    ui.painter().rect_filled(
                                        art_rect,
                                        top_rounding,
                                        egui::Color32::from_black_alpha(120),
                                    );
                                    let center = art_rect.center();
                                    let size = 20.0 * s;
                                    let points = vec![
                                        egui::pos2(center.x - size * 0.4, center.y - size * 0.5),
                                        egui::pos2(center.x - size * 0.4, center.y + size * 0.5),
                                        egui::pos2(center.x + size * 0.5, center.y),
                                    ];
                                    ui.painter().add(egui::Shape::convex_polygon(
                                        points,
                                        egui::Color32::WHITE,
                                        egui::Stroke::NONE,
                                    ));
                                }

                                // Title footer strip
                                let footer_rect = egui::Rect::from_min_max(
                                    egui::pos2(rect.min.x, rect.min.y + art_height),
                                    rect.max,
                                );
                                let cri_b = cr as u8;
                                let bottom_rounding = egui::CornerRadius {
                                    nw: 0, ne: 0,
                                    sw: cri_b, se: cri_b,
                                };
                                ui.painter().rect_filled(
                                    footer_rect,
                                    bottom_rounding,
                                    egui::Color32::from_rgba_premultiplied(22, 22, 44, 170),
                                );

                                // Title — vertically centered within the footer strip
                                debug_rect(ui, footer_rect, egui::Color32::YELLOW, dbg);
                                let title_inset = footer_rect.shrink2(egui::vec2(12.0 * s, 0.0));
                                let galley = ui.painter().layout_no_wrap(
                                    game.name.clone(),
                                    egui::FontId::proportional(13.0 * s),
                                    egui::Color32::from_rgb(210, 212, 225),
                                );
                                let text_pos = egui::pos2(
                                    title_inset.min.x,
                                    footer_rect.center().y - galley.size().y * 0.5,
                                );
                                ui.painter().with_clip_rect(title_inset).galley(
                                    text_pos,
                                    galley,
                                    egui::Color32::from_rgb(210, 212, 225),
                                );
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
// Current App tab
// ============================================================

fn render_current_app_content(
    ui: &mut egui::Ui,
    current_app: &Option<String>,
    resume_clicked: &mut bool,
    quit_clicked: &mut bool,
    s: f32,
) {
    let name = match current_app {
        Some(name) => name.as_str(),
        None => {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0 * s);
                ui.label(
                    egui::RichText::new("No app running")
                        .size(18.0 * s)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
            });
            return;
        }
    };

    ui.vertical_centered(|ui| {
        ui.add_space(60.0 * s);
        ui.label(
            egui::RichText::new(name)
                .size(24.0 * s)
                .color(egui::Color32::WHITE)
                .strong(),
        );
        ui.add_space(32.0 * s);
        if ui
            .add_sized(
                egui::vec2(200.0 * s, 40.0 * s),
                egui::Button::new(egui::RichText::new("Resume").size(16.0 * s)),
            )
            .clicked()
        {
            *resume_clicked = true;
        }
        ui.add_space(12.0 * s);
        if ui
            .add_sized(
                egui::vec2(200.0 * s, 40.0 * s),
                egui::Button::new(
                    egui::RichText::new("Quit")
                        .size(16.0 * s)
                        .color(egui::Color32::from_rgb(255, 100, 100)),
                ),
            )
            .clicked()
        {
            *quit_clicked = true;
        }
    });
}

// ============================================================
// Desktop tab
// ============================================================

fn render_desktop_content(
    ui: &mut egui::Ui,
    texture_id: Option<(egui::TextureId, u32, u32)>,
    error: &Option<String>,
    s: f32,
) -> Option<egui::Rect> {
    if let Some(err) = error {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0 * s);
            ui.label(
                egui::RichText::new("Desktop capture unavailable")
                    .size(18.0 * s)
                    .color(egui::Color32::from_rgb(255, 100, 100)),
            );
            ui.add_space(8.0 * s);
            ui.label(
                egui::RichText::new(err)
                    .size(13.0 * s)
                    .color(egui::Color32::from_rgb(180, 180, 200)),
            );
        });
        return None;
    }

    match texture_id {
        Some((tex_id, w, h)) => {
            let available = ui.available_size();
            let tex_size = egui::vec2(w as f32, h as f32);
            let scale = (available.x / tex_size.x).min(available.y / tex_size.y);
            let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);

            let mut image_rect = None;
            ui.vertical_centered(|ui| {
                let vertical_pad = (available.y - display_size.y) * 0.5;
                if vertical_pad > 0.0 {
                    ui.add_space(vertical_pad);
                }
                let response = ui.image(egui::load::SizedTexture::new(tex_id, display_size));
                image_rect = Some(response.rect);
            });
            image_rect
        }
        None => {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0 * s);
                ui.label(
                    egui::RichText::new("Connecting to desktop...")
                        .size(18.0 * s)
                        .color(egui::Color32::from_rgb(106, 112, 136)),
                );
                ui.spinner();
            });
            None
        }
    }
}

// ============================================================
// Settings tab
// ============================================================

fn render_settings_content(ui: &mut egui::Ui, config: &mut Config, save_clicked: &mut bool, s: f32) {
    ui.add_space(12.0 * s);
    ui.heading(
        egui::RichText::new("Settings")
            .size(22.0 * s)
            .color(egui::Color32::WHITE),
    );
    ui.add_space(4.0 * s);
    ui.label(
        egui::RichText::new("Configure your ClearXR experience")
            .size(12.0 * s)
            .color(egui::Color32::from_rgb(120, 126, 155)),
    );
    ui.add_space(20.0 * s);

    egui::Grid::new("settings_grid")
        .num_columns(2)
        .spacing([24.0 * s, 16.0 * s])
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Default View")
                    .size(14.0 * s)
                    .color(egui::Color32::from_rgb(200, 200, 220)),
            );
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

            ui.label(
                egui::RichText::new("Panel Opacity")
                    .size(14.0 * s)
                    .color(egui::Color32::from_rgb(200, 200, 220)),
            );
            {
                let pct = format!("{:.0}%", config.panel.opacity * 100.0);
                ui.add(egui::Slider::new(&mut config.panel.opacity, 0.5..=1.0).text(pct));
            }
            ui.end_row();

            ui.label(
                egui::RichText::new("Show FPS")
                    .size(14.0 * s)
                    .color(egui::Color32::from_rgb(200, 200, 220)),
            );
            ui.checkbox(&mut config.display.show_fps, "");
            ui.end_row();

            ui.label(
                egui::RichText::new("Haptic Feedback")
                    .size(14.0 * s)
                    .color(egui::Color32::from_rgb(200, 200, 220)),
            );
            ui.checkbox(&mut config.shell.haptics_enabled, "");
            ui.end_row();

            ui.label(
                egui::RichText::new("Debug Borders")
                    .size(14.0 * s)
                    .color(egui::Color32::from_rgb(200, 200, 220)),
            );
            ui.checkbox(&mut config.display.debug_borders, "");
            ui.end_row();
        });

    ui.add_space(24.0 * s);
    let save_btn = ui.add_sized(
        egui::vec2(120.0 * s, 36.0 * s),
        egui::Button::new(
            egui::RichText::new("Save")
                .size(15.0 * s)
                .color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_rgb(44, 108, 210))
        .corner_radius(6.0 * s),
    );
    if save_btn.clicked() {
        *save_clicked = true;
    }
}
