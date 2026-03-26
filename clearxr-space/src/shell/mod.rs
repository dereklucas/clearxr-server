//! Shell orchestrator for the ClearXR VR environment.
//!
//! The Shell owns the main panel, toolbar, FPS counter, screen capture,
//! launcher UI, and input dispatcher. It provides a single `tick()` method
//! called per frame from the XR session loop, plus `panels_mut()` to hand
//! mutable panel references to the renderer.

pub mod boundary;
pub mod notifications;

use ash::vk;
use glam::Vec3;
use anyhow::Result;
use log::info;

use crate::app::{game_scanner, launch_steam_game, LaunchedApp};
use crate::capture::screen_capture::ScreenCapture;
use crate::config::Config;
use crate::input::{ControllerState, InputDispatcher, InputEvent, Hand};
use crate::launcher_panel::{LauncherPanel, ToolbarTab, generate_toolbar_pixels, generate_grab_bar_pixels, generate_grab_bar_pixels_highlighted, generate_fps_pixels, draw_text};
use crate::panel::{PanelAnchor, PanelId, PanelTransform};

// Stable PanelIds for InputDispatcher routing
const PANEL_ID_LAUNCHER: PanelId = PanelId::new(1);
const PANEL_ID_DESKTOP: PanelId = PanelId::new(2);
const PANEL_ID_TOOLBAR: PanelId = PanelId::new(3);
const PANEL_ID_GRAB_BAR: PanelId = PanelId::new(4);
const PANEL_ID_FPS: PanelId = PanelId::new(5);
const PANEL_ID_TRAY: PanelId = PanelId::new(6);
const PANEL_ID_SETTINGS: PanelId = PanelId::new(7);

/// Extract a PanelTransform from a LauncherPanel for InputDispatcher hit-testing.
fn panel_transform(panel: &LauncherPanel) -> PanelTransform {
    PanelTransform {
        center: panel.center,
        right_dir: panel.right_dir,
        up_dir: panel.up_dir,
        width: panel.width,
        height: panel.height,
        opacity: panel.opacity,
        anchor: PanelAnchor::World,
    }
}
use crate::shell::boundary::Boundary;
use crate::shell::notifications::{Notification, NotificationLevel, NotificationQueue};
use crate::ui::ui_renderer::UiRenderer;
use crate::vk_backend::VkBackend;

/// Which content the main panel is showing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewMode {
    /// Game launcher UI
    Launcher,
    /// Desktop screen capture
    Desktop,
    // Future: Game(pid) for running flat game capture
}

/// A single haptic vibration pulse request.
pub struct HapticPulse {
    pub duration_ms: u32,
    pub frequency: f32,
    pub amplitude: f32,
}

/// Per-frame output from Shell::tick().
pub struct ShellFrame {
    /// Distance from left controller to nearest panel hit (0 = no hit).
    pub left_ray_hit_dist: f32,
    /// Distance from right controller to nearest panel hit (0 = no hit).
    pub right_ray_hit_dist: f32,
    /// Haptic pulse request for the left hand (if any).
    pub haptic_left: Option<HapticPulse>,
    /// Haptic pulse request for the right hand (if any).
    pub haptic_right: Option<HapticPulse>,
}

/// Top-level VR shell state.
///
/// Owns every panel, their content sources (UI renderer, screen capture),
/// and the input dispatcher. The XR session loop calls `tick()` once per
/// frame and `panels_mut()` to collect the panels that should be drawn.
pub struct Shell {
    pub config: Config,
    pub active_view: ViewMode,

    // Panel content sources
    launcher_ui: UiRenderer,
    screen_capture: ScreenCapture,

    // Vulkan panels
    pub launcher_panel: LauncherPanel,
    pub screen_panel: LauncherPanel,
    pub toolbar_panel: LauncherPanel,
    pub grab_bar: LauncherPanel,
    pub fps_panel: LauncherPanel,

    // Input
    input: InputDispatcher,

    // Grab state
    grab_offset: Option<Vec3>,  // offset from grip to panel center when grab started
    grab_hand: Option<usize>,   // which hand (0=left, 1=right) is grabbing

    // Grab bar highlight state
    grab_bar_highlighted: bool,
    grab_bar_w: u32,
    grab_bar_h: u32,

    // System tray
    tray_panel: Option<LauncherPanel>,
    tray_ui: Option<UiRenderer>,
    prev_menu_click: bool, // edge detection for menu button
    tray_visible: bool,

    // Panel anchor
    pub anchor: PanelAnchor,

    // Vulkan render pass (needed for lazy panel creation)
    render_pass: vk::RenderPass,

    // FPS tracking
    fps_timer: std::time::Instant,
    fps_frame_count: u32,
    fps_current: f32,
    fps_w: u32,
    fps_h: u32,
    toolbar_w: u32,
    toolbar_h: u32,
    prev_hover_zone: Option<u8>,

    // Settings panel
    settings_panel: Option<LauncherPanel>,
    settings_ui: Option<UiRenderer>,
    settings_visible: bool,

    // Notifications
    pub notifications: NotificationQueue,
    notification_panel: Option<LauncherPanel>,

    // Virtual keyboard — disabled for MVP (not wired end-to-end; no text input target)
    // keyboard_panel: Option<LauncherPanel>,
    // keyboard_ui: Option<UiRenderer>,
    // keyboard_visible: bool,

    // Boundary
    pub boundary: Boundary,
    boundary_warning_shown: bool,

    // Screenshot
    pub screenshot_requested: bool,
    prev_both_triggers: bool,

    // Launched app tracking
    launched_app: Option<LaunchedApp>,

    // Scanned games (kept for launch lookups)
    games: Vec<game_scanner::Game>,
}

impl Shell {
    /// Create the shell and all its panels.
    ///
    /// `render_pass` is the Vulkan render pass that the panel pipelines must be
    /// compatible with (comes from the Renderer).
    pub fn new(
        config: Config,
        use_screen_capture: bool,
        vk: &VkBackend,
        render_pass: vk::RenderPass,
    ) -> Result<Self> {
        let active_view = if use_screen_capture {
            ViewMode::Desktop
        } else {
            match config.shell.default_view.as_str() {
                "desktop" => ViewMode::Desktop,
                _ => ViewMode::Launcher,
            }
        };

        // ---- Launcher panel + UI renderer ----
        let launcher_tex_w = 1024u32;
        let launcher_tex_h = 640u32;
        let mut games = game_scanner::scan_all();
        games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        info!("Initializing launcher UI...");
        let html_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui/launcher-v2.html");
        let mut launcher_ui = UiRenderer::new(launcher_tex_w, launcher_tex_h, &html_path)?;
        if let Err(e) = launcher_ui.set_games(&games) {
            log::warn!("Failed to inject games into UI: {}", e);
        }
        info!(
            "Launcher UI initialized ({}x{}, {} games)",
            launcher_tex_w, launcher_tex_h, games.len()
        );

        let mut launcher_panel = LauncherPanel::new(
            vk, render_pass, launcher_tex_w, launcher_tex_h, vk::Format::R8G8B8A8_SRGB,
        )?;
        if let Some(pixels) = launcher_ui.update() {
            launcher_panel.upload_pixels(vk, pixels)?;
        }

        // ---- Screen capture panel ----
        let mut screen_capture = ScreenCapture::new()?;
        let screen_tex_w = screen_capture.screen_width();
        let screen_tex_h = screen_capture.screen_height();
        let mut screen_panel = LauncherPanel::new(
            vk, render_pass, screen_tex_w, screen_tex_h, vk::Format::B8G8R8A8_SRGB,
        )?;
        // Wider panel for 16:9 screen
        screen_panel.width = 2.4;
        screen_panel.height = 1.35;
        if let Some(frame) = screen_capture.try_get_frame() {
            screen_panel.upload_pixels(vk, &frame.data)?;
        }
        info!("Screen capture initialized: {}x{}", screen_tex_w, screen_tex_h);

        // ---- Toolbar panel ----
        let toolbar_w = 512u32;
        let toolbar_h = 48u32;
        let mut toolbar_panel = LauncherPanel::new(
            vk, render_pass, toolbar_w, toolbar_h, vk::Format::R8G8B8A8_SRGB,
        )?;
        toolbar_panel.width = 0.8;
        toolbar_panel.height = 0.1;
        toolbar_panel.opacity = 0.95;
        toolbar_panel.center = Vec3::new(0.0, 1.6 - 0.5 - 0.08, -2.5);
        let toolbar_tab = if use_screen_capture { ToolbarTab::Screen } else { ToolbarTab::Launcher };
        let toolbar_pixels = generate_toolbar_pixels(toolbar_w, toolbar_h, toolbar_tab, "world", None);
        toolbar_panel.upload_pixels(vk, &toolbar_pixels)?;

        // ---- Grab bar (visionOS-style pill below toolbar) ----
        let grab_bar_w = 128u32;
        let grab_bar_h = 24u32;
        let mut grab_bar = LauncherPanel::new(
            vk, render_pass, grab_bar_w, grab_bar_h, vk::Format::R8G8B8A8_SRGB,
        )?;
        grab_bar.width = 0.35;
        grab_bar.height = 0.05;
        grab_bar.opacity = 0.85;
        grab_bar.center = Vec3::new(0.0, 1.0, -2.5); // will be repositioned each frame
        let grab_pixels = generate_grab_bar_pixels(grab_bar_w, grab_bar_h);
        grab_bar.upload_pixels(vk, &grab_pixels)?;

        // ---- FPS counter panel (on the floor at feet) ----
        let fps_w = 128u32;
        let fps_h = 48u32;
        let mut fps_panel = LauncherPanel::new(
            vk, render_pass, fps_w, fps_h, vk::Format::R8G8B8A8_SRGB,
        )?;
        fps_panel.center = Vec3::new(0.0, 0.01, -0.5);
        fps_panel.width = 0.3;
        fps_panel.height = 0.12;
        fps_panel.opacity = 0.9;
        fps_panel.right_dir = Vec3::X;
        fps_panel.up_dir = -Vec3::Z;

        // Apply config opacity to content panels
        launcher_panel.opacity = config.panel.opacity;
        screen_panel.opacity = config.panel.opacity;

        Ok(Self {
            config,
            active_view,
            launcher_ui,
            screen_capture,
            launcher_panel,
            screen_panel,
            toolbar_panel,
            grab_bar,
            fps_panel,
            input: InputDispatcher::new(),
            grab_offset: None,
            grab_hand: None,
            grab_bar_highlighted: false,
            grab_bar_w,
            grab_bar_h,
            tray_panel: None,
            tray_ui: None,
            prev_menu_click: false,
            tray_visible: false,
            anchor: PanelAnchor::World,
            render_pass,
            fps_timer: std::time::Instant::now(),
            fps_frame_count: 0,
            fps_current: 0.0,
            fps_w,
            fps_h,
            toolbar_w,
            toolbar_h,
            prev_hover_zone: None,
            settings_panel: None,
            settings_ui: None,
            settings_visible: false,
            // keyboard disabled for MVP
            // keyboard_panel: None,
            // keyboard_ui: None,
            // keyboard_visible: false,
            notifications: NotificationQueue::new(3),
            notification_panel: None,
            boundary: Boundary::default(),
            boundary_warning_shown: false,
            screenshot_requested: false,
            prev_both_triggers: false,
            launched_app: None,
            games,
        })
    }

    /// Per-frame update: input, content, FPS, ray clipping.
    ///
    /// The caller passes the current `ControllerState` (extracted from OpenXR)
    /// and the Vulkan backend (for texture uploads). Returns a `ShellFrame`
    /// with the ray-hit distance the caller should write into `HandData`.
    pub fn tick(&mut self, vk: &VkBackend, controller: &ControllerState) -> ShellFrame {
        let hands: [&crate::input::HandState; 2] = [&controller.left, &controller.right];
        let mut haptic: [Option<HapticPulse>; 2] = [None, None];

        // ------------------------------------------------------------------
        // 1. Toolbar: position below active panel
        // ------------------------------------------------------------------
        {
            let (active_p_center, active_p_up_dir, active_p_height, active_p_right_dir) =
                match self.active_view {
                    ViewMode::Launcher => (
                        self.launcher_panel.center,
                        self.launcher_panel.up_dir,
                        self.launcher_panel.height,
                        self.launcher_panel.right_dir,
                    ),
                    ViewMode::Desktop => (
                        self.screen_panel.center,
                        self.screen_panel.up_dir,
                        self.screen_panel.height,
                        self.screen_panel.right_dir,
                    ),
                };
            self.toolbar_panel.center = active_p_center
                - active_p_up_dir * (active_p_height * 0.5 + self.toolbar_panel.height * 0.5 + 0.02);
            self.toolbar_panel.right_dir = active_p_right_dir;
            self.toolbar_panel.up_dir = active_p_up_dir;

            // Position grab bar below toolbar (visionOS-style pill)
            self.grab_bar.center = self.toolbar_panel.center
                - active_p_up_dir * (self.toolbar_panel.height * 0.5 + self.grab_bar.height * 0.5 + 0.01);
            self.grab_bar.right_dir = active_p_right_dir;
            self.grab_bar.up_dir = active_p_up_dir;
        }

        // ------------------------------------------------------------------
        // 2. Build panel list and process input through InputDispatcher
        // ------------------------------------------------------------------
        // Build panel list in front-to-back priority order
        let mut input_panels: Vec<(PanelId, PanelTransform)> = Vec::new();
        if self.tray_visible {
            if let Some(ref tray) = self.tray_panel {
                input_panels.push((PANEL_ID_TRAY, panel_transform(tray)));
            }
        }
        if self.settings_visible {
            if let Some(ref settings) = self.settings_panel {
                input_panels.push((PANEL_ID_SETTINGS, panel_transform(settings)));
            }
        }
        // Active panel
        let active_id = match self.active_view {
            ViewMode::Launcher => PANEL_ID_LAUNCHER,
            ViewMode::Desktop => PANEL_ID_DESKTOP,
        };
        input_panels.push((active_id, panel_transform(self.active_panel())));
        input_panels.push((PANEL_ID_TOOLBAR, panel_transform(&self.toolbar_panel)));
        input_panels.push((PANEL_ID_GRAB_BAR, panel_transform(&self.grab_bar)));

        // Process all input through the dispatcher (hit-testing + edge detection)
        let panel_refs: Vec<(PanelId, &PanelTransform)> = input_panels.iter()
            .map(|(id, t)| (*id, t))
            .collect();
        let events = self.input.process(controller, &panel_refs);

        // ------------------------------------------------------------------
        // 2b. Dispatch events from InputDispatcher
        // ------------------------------------------------------------------
        let mut per_hand_ray = [0.0f32; 2]; // [left, right] ray hit distances
        let mut any_bar_hit = false;
        self.launcher_panel.dot_uv = None;
        self.screen_panel.dot_uv = None;
        self.grab_bar.dot_uv = None;

        for (panel_id, event) in &events {
            match event {
                InputEvent::PointerMove { hand, u, v, distance } => {
                    let hand_idx = match hand { Hand::Left => 0, Hand::Right => 1 };
                    if per_hand_ray[hand_idx] == 0.0 || *distance < per_hand_ray[hand_idx] {
                        per_hand_ray[hand_idx] = *distance;
                    }

                    match *panel_id {
                        id if id == PANEL_ID_LAUNCHER => {
                            self.launcher_panel.dot_uv = Some((*u, *v));
                            self.launcher_ui.mouse_move(*u, *v);
                        }
                        id if id == PANEL_ID_DESKTOP => {
                            // No dot on desktop — real cursor is drawn in capture
                            self.screen_capture.inject_mouse_move(*u, *v);
                        }
                        id if id == PANEL_ID_TOOLBAR => {
                            // Hover highlight
                            let zone = if *u < 0.35 { 0u8 }
                                else if *u < 0.70 { 1 }
                                else if *u < 0.80 { 2 }
                                else if *u < 0.90 { 3 }
                                else { 4 };
                            let current_hover_zone = Some(zone);
                            if current_hover_zone != self.prev_hover_zone {
                                self.prev_hover_zone = current_hover_zone;
                                self.update_toolbar(vk);
                            }
                        }
                        id if id == PANEL_ID_GRAB_BAR => {
                            any_bar_hit = true;
                        }
                        id if id == PANEL_ID_TRAY => {
                            if let Some(ref mut ui) = self.tray_ui {
                                if let Some(ref mut panel) = self.tray_panel {
                                    panel.dot_uv = Some((*u, *v));
                                    ui.mouse_move(*u, *v);
                                }
                            }
                        }
                        id if id == PANEL_ID_SETTINGS => {
                            if let Some(ref mut ui) = self.settings_ui {
                                if let Some(ref mut panel) = self.settings_panel {
                                    panel.dot_uv = Some((*u, *v));
                                    ui.mouse_move(*u, *v);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                InputEvent::PointerDown { hand, u, v } => {
                    let hand_idx = match hand { Hand::Left => 0, Hand::Right => 1 };

                    match *panel_id {
                        id if id == PANEL_ID_LAUNCHER => {
                            self.launcher_ui.mouse_click(*u, *v);
                        }
                        id if id == PANEL_ID_DESKTOP => {
                            self.screen_capture.inject_mouse_click(*u, *v);
                        }
                        id if id == PANEL_ID_TOOLBAR => {
                            self.handle_toolbar_click(*u, vk);
                        }
                        id if id == PANEL_ID_TRAY => {
                            if let Some(ref mut ui) = self.tray_ui {
                                ui.mouse_click(*u, *v);
                            }
                        }
                        id if id == PANEL_ID_SETTINGS => {
                            if let Some(ref mut ui) = self.settings_ui {
                                ui.mouse_click(*u, *v);
                            }
                        }
                        _ => {}
                    }
                    // Haptic feedback on any click
                    haptic[hand_idx] = Some(HapticPulse { duration_ms: 20, frequency: 200.0, amplitude: 0.2 });
                }
                InputEvent::GrabStart { hand, grip_pos, .. } => {
                    if *panel_id == PANEL_ID_GRAB_BAR || *panel_id == PANEL_ID_TOOLBAR {
                        // Only start grab if not already grabbing
                        if self.grab_offset.is_none() && self.grab_hand.is_none() {
                            let hand_idx = match hand { Hand::Left => 0, Hand::Right => 1 };
                            let active_center = self.active_panel().center;
                            self.grab_offset = Some(active_center - *grip_pos);
                            self.grab_hand = Some(hand_idx);
                            haptic[hand_idx] = Some(HapticPulse { duration_ms: 50, frequency: 200.0, amplitude: 0.6 });
                            info!("Panel grabbed by hand {}.", hand_idx);
                        }
                    }
                }
                InputEvent::ButtonPress { hand: _, button: crate::input::Button::A } => {
                    // A button during grab → cycle anchor
                    if self.grab_hand.is_some() {
                        self.anchor = cycle_anchor(self.anchor);
                        info!("Panel anchor: {:?}", self.anchor);
                        self.update_toolbar(vk);
                    }
                }
                _ => {}
            }
        }

        // If no panel was hit by toolbar hover this frame, clear hover zone
        let toolbar_hovered = events.iter().any(|(pid, evt)| {
            *pid == PANEL_ID_TOOLBAR && matches!(evt, InputEvent::PointerMove { .. })
        });
        if !toolbar_hovered && self.prev_hover_zone.is_some() {
            self.prev_hover_zone = None;
            self.update_toolbar(vk);
        }

        // Clear tray dot_uv if no tray hit this frame
        if self.tray_visible {
            let tray_hovered = events.iter().any(|(pid, evt)| {
                *pid == PANEL_ID_TRAY && matches!(evt, InputEvent::PointerMove { .. })
            });
            if !tray_hovered {
                if let Some(ref mut panel) = self.tray_panel {
                    panel.dot_uv = None;
                }
            }
        }

        // Update grab bar highlight based on hover or active grab
        let grabbing = self.grab_hand.is_some();
        let should_highlight = any_bar_hit || grabbing;
        if should_highlight != self.grab_bar_highlighted {
            self.grab_bar_highlighted = should_highlight;
            let pixels = if should_highlight {
                generate_grab_bar_pixels_highlighted(self.grab_bar_w, self.grab_bar_h)
            } else {
                generate_grab_bar_pixels(self.grab_bar_w, self.grab_bar_h)
            };
            self.grab_bar.upload_pixels(vk, &pixels).ok();
        }

        // Grab continue/release (outside event loop, only the grabbing hand matters)
        if let Some(hand_idx) = self.grab_hand {
            let grab_hand = hands[hand_idx];
            let still_holding = grab_hand.squeeze > 0.3 || grab_hand.trigger > 0.3;
            if still_holding {
                if let Some(offset) = self.grab_offset {
                    let new_center = grab_hand.grip_pos + offset;
                    self.active_panel_mut().center = new_center;
                }
            } else {
                // Haptic feedback for grab release: light pulse
                haptic[hand_idx] = Some(HapticPulse { duration_ms: 30, frequency: 150.0, amplitude: 0.3 });
                self.grab_offset = None;
                self.grab_hand = None;
                info!("Panel released.");
            }
        }

        let left_ray_hit_dist = per_hand_ray[0];
        let right_ray_hit_dist = per_hand_ray[1];

        // ------------------------------------------------------------------
        // 3. Update anchor each frame
        // ------------------------------------------------------------------
        // Use whichever hand is active for anchor positioning
        let anchor_hand = if controller.right.active { &controller.right } else { &controller.left };
        match self.anchor {
            PanelAnchor::Controller { .. } => {
                if anchor_hand.active {
                    let p = self.active_panel_mut();
                    p.center = anchor_hand.grip_pos + anchor_hand.aim_dir * 0.4;
                }
            }
            PanelAnchor::Theater { distance, scale } => {
                if anchor_hand.active {
                    let fwd = anchor_hand.aim_dir;
                    let fwd_flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
                    if fwd_flat != Vec3::ZERO {
                        let p = self.active_panel_mut();
                        p.center = anchor_hand.aim_pos + fwd_flat * distance + Vec3::Y * 0.5;
                        p.width = 1.6 * scale;
                        p.height = 1.0 * scale;
                        p.right_dir = fwd_flat.cross(Vec3::Y).normalize();
                        p.up_dir = Vec3::Y;
                    }
                }
            }
            _ => {}
        }

        // ------------------------------------------------------------------
        // 4. Content updates (per-frame, not per-hand)
        // ------------------------------------------------------------------
        match self.active_view {
            ViewMode::Launcher => {
                if let Some(pixels) = self.launcher_ui.update() {
                    if let Err(e) = self.launcher_panel.upload_pixels(vk, pixels) {
                        log::error!("Launcher texture upload failed: {}", e);
                    }
                }
            }
            ViewMode::Desktop => {
                if let Some(frame) = self.screen_capture.try_get_frame() {
                    if let Err(e) = self.screen_panel.stage_pixels(&frame.data) {
                        log::error!("Screen capture stage failed: {}", e);
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // 5. Menu button -> system tray toggle
        // ------------------------------------------------------------------
        {
            let menu_click = controller.left.menu_click || controller.right.menu_click;
            if menu_click && !self.prev_menu_click {
                self.tray_visible = !self.tray_visible;
                if self.tray_visible {
                    self.show_system_tray(vk);
                } else {
                    self.hide_system_tray();
                }
            }
            self.prev_menu_click = menu_click;
        }

        // ------------------------------------------------------------------
        // 5b. System tray positioning, texture update, and action polling
        //     (hit-testing is now handled by InputDispatcher above)
        // ------------------------------------------------------------------
        if self.tray_visible {
            if let (Some(ref mut tray_ui), Some(ref mut tray_panel)) =
                (&mut self.tray_ui, &mut self.tray_panel)
            {
                // Position tray near active controller
                if anchor_hand.active {
                    tray_panel.center = anchor_hand.grip_pos
                        + anchor_hand.aim_dir * 0.3
                        + Vec3::Y * 0.1;
                }

                // Update tray texture
                if let Some(pixels) = tray_ui.update() {
                    tray_panel.upload_pixels(vk, pixels).ok();
                }

                // Poll for tray actions
                if let Some(action) = tray_ui.evaluate_js(
                    "(function(){ var a = window.__clearxr_tray_pending || ''; window.__clearxr_tray_pending = ''; return a; })()"
                ) {
                    if !action.is_empty() {
                        match action.as_str() {
                            "home" => {
                                self.active_view = ViewMode::Launcher;
                                self.update_toolbar(vk);
                                self.hide_system_tray();
                            }
                            "desktop" => {
                                self.active_view = ViewMode::Desktop;
                                self.update_toolbar(vk);
                                self.hide_system_tray();
                            }
                            "settings" => {
                                self.hide_system_tray();
                                self.show_settings(vk);
                            }
                            "screenshot" => {
                                self.screenshot_requested = true;
                                self.hide_system_tray();
                                self.notifications.push(Notification::success(
                                    "Screenshot",
                                    "Saved to Pictures/ClearXR",
                                ));
                                info!("Screenshot requested via system tray.");
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // 5c. Virtual keyboard interactions — disabled for MVP
        //     (not wired end-to-end; no text input target)
        // ------------------------------------------------------------------

        // ------------------------------------------------------------------
        // 6. FPS counter update (every 0.5s)
        // ------------------------------------------------------------------
        self.fps_frame_count += 1;
        let fps_elapsed = self.fps_timer.elapsed().as_secs_f32();
        if fps_elapsed >= 0.5 {
            self.fps_current = self.fps_frame_count as f32 / fps_elapsed;
            self.fps_frame_count = 0;
            self.fps_timer = std::time::Instant::now();
            let fps_pixels = generate_fps_pixels(self.fps_w, self.fps_h, self.fps_current);
            if let Err(e) = self.fps_panel.upload_pixels(vk, &fps_pixels) {
                log::error!("FPS panel upload failed: {}", e);
            }
        }

        // ------------------------------------------------------------------
        // 7. Boundary proximity warning
        // ------------------------------------------------------------------
        if self.config.display.show_boundary {
            if anchor_hand.active {
                let vis = self.boundary.compute_visibility(anchor_hand.aim_pos);
                let any_visible = vis.left > 0.3 || vis.right > 0.3 || vis.front > 0.3 || vis.back > 0.3;
                if any_visible && !self.boundary_warning_shown {
                    self.notifications.push(Notification::warning("Boundary", "Near play space edge"));
                    self.boundary_warning_shown = true;
                } else if !any_visible {
                    self.boundary_warning_shown = false;
                }
            }
        }

        // ------------------------------------------------------------------
        // 8. Poll launcher UI for pending game launch
        // ------------------------------------------------------------------
        {
            if let Some(result) = self.launcher_ui.evaluate_js(
                "var _p = window.__clearxr_pending_launch || ''; window.__clearxr_pending_launch = ''; _p"
            ) {
                if let Ok(app_id) = result.parse::<u32>() {
                    self.launch_game(app_id, vk);
                }
            }
        }

        // ------------------------------------------------------------------
        // 8b. Check launched app status
        // ------------------------------------------------------------------
        {
            let mut exited = false;
            if let Some(ref mut app) = self.launched_app {
                use crate::app::AppStatus;
                match app.status() {
                    AppStatus::Running => {}
                    AppStatus::ExitedOk | AppStatus::Exited(_) => {
                        info!("Launched app '{}' has exited", app.name);
                        self.notifications.push(Notification::info(
                            "Game ended",
                            &format!("{}", app.name),
                        ));
                        exited = true;
                    }
                }
            }
            if exited {
                self.launched_app = None;
            }
        }

        // ------------------------------------------------------------------
        // 9. Settings panel content update and action polling
        //    (hit-testing is now handled by InputDispatcher above)
        // ------------------------------------------------------------------
        if self.settings_visible {
            if let (Some(ref mut panel), Some(ref mut ui)) =
                (&mut self.settings_panel, &mut self.settings_ui)
            {
                if let Some(pixels) = ui.update() {
                    if let Err(e) = panel.upload_pixels(vk, pixels) {
                        log::error!("Settings texture upload failed: {}", e);
                    }
                }

                // Poll settings UI for pending config save
                if let Some(json) = ui.evaluate_js(
                    "var _s = window.__clearxr_pending_save || ''; window.__clearxr_pending_save = ''; _s"
                ) {
                    self.apply_settings_json(&json);
                }
            }
        }

        // ------------------------------------------------------------------
        // 10. Screenshot trigger (both triggers pulled simultaneously)
        // ------------------------------------------------------------------
        {
            let both_triggers = controller.left.trigger > 0.8
                && controller.right.trigger > 0.8;
            if both_triggers && !self.prev_both_triggers {
                self.screenshot_requested = true;
                info!("Screenshot requested!");
                self.notifications.push(Notification::success(
                    "Screenshot",
                    "Saved to Pictures/ClearXR",
                ));
            }
            self.prev_both_triggers = both_triggers;
        }

        // ------------------------------------------------------------------
        // 11. Notification panel rendering
        // ------------------------------------------------------------------
        self.notifications.tick();

        if self.notifications.count() > 0 {
            // Create notification panel if it doesn't exist (larger: 384x80)
            if self.notification_panel.is_none() {
                let panel = LauncherPanel::new(vk, self.render_pass, 384, 80, vk::Format::R8G8B8A8_SRGB).ok();
                if let Some(mut p) = panel {
                    p.width = 0.5;
                    p.height = 0.12;
                    p.opacity = 0.9;
                    self.notification_panel = Some(p);
                }
            }

            // Position slightly above and to the right of the active panel center
            // Extract active panel properties before mutably borrowing notification_panel
            let (base_center, right_dir, up_dir, p_height, p_width) = {
                let active_p = self.active_panel();
                (active_p.center, active_p.right_dir, active_p.up_dir, active_p.height, active_p.width)
            };
            if let Some(ref mut panel) = self.notification_panel {
                // Place above-right of panel center
                panel.center = base_center
                    + up_dir * (p_height * 0.5 + panel.height * 0.5 + 0.04)
                    + right_dir * (p_width * 0.25);
                panel.right_dir = right_dir;
                panel.up_dir = up_dir;

                // Render notification text (show the first/newest)
                let notif = &self.notifications.visible()[0];
                let pixels = generate_notification_pixels(384, 80, &notif.title, &notif.body, notif.level);
                if let Err(e) = panel.upload_pixels(vk, &pixels) {
                    log::error!("Notification panel upload failed: {}", e);
                }
            }
        } else {
            // No notifications — hide the panel
            self.notification_panel = None;
        }

        // Gate haptic feedback on config toggle
        let (haptic_left, haptic_right) = if self.config.shell.haptics_enabled {
            let [hl, hr] = haptic;
            (hl, hr)
        } else {
            (None, None)
        };

        ShellFrame { left_ray_hit_dist, right_ray_hit_dist, haptic_left, haptic_right }
    }

    /// Returns a reference to whichever panel is currently active.
    fn active_panel(&self) -> &LauncherPanel {
        match self.active_view {
            ViewMode::Launcher => &self.launcher_panel,
            ViewMode::Desktop => &self.screen_panel,
        }
    }

    /// Returns a mutable reference to whichever panel is currently active.
    fn active_panel_mut(&mut self) -> &mut LauncherPanel {
        match self.active_view {
            ViewMode::Launcher => &mut self.launcher_panel,
            ViewMode::Desktop => &mut self.screen_panel,
        }
    }

    /// Handle a click on the toolbar at the given U coordinate.
    fn handle_toolbar_click(&mut self, u: f32, vk: &VkBackend) {
        if u < 0.35 {
            if self.active_view != ViewMode::Launcher {
                self.active_view = ViewMode::Launcher;
                info!("Switched to Launcher view.");
                self.update_toolbar(vk);
            }
        } else if u < 0.70 {
            if self.active_view != ViewMode::Desktop {
                self.active_view = ViewMode::Desktop;
                info!("Switched to Desktop view.");
                self.update_toolbar(vk);
            }
        } else if u < 0.80 {
            self.show_settings(vk);
            info!("Settings opened via toolbar.");
        } else if u < 0.90 {
            self.anchor = cycle_anchor(self.anchor);
            info!("Panel anchor: {:?}", self.anchor);
            self.update_toolbar(vk);
        } else {
            self.screenshot_requested = true;
            info!("Screenshot requested via toolbar.");
            self.notifications.push(Notification::success(
                "Screenshot",
                "Saved to Pictures/ClearXR",
            ));
        }
    }

    /// Lazy-create and show the system tray panel near the controller.
    fn show_system_tray(&mut self, vk: &VkBackend) {
        if self.tray_panel.is_some() { return; }

        let w = 300u32;
        let h = 300u32;
        let html_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui/system-tray.html");

        let ui = match UiRenderer::new(w, h, &html_path) {
            Ok(ui) => ui,
            Err(e) => {
                log::error!("Failed to create tray UI: {}", e);
                return;
            }
        };

        let mut panel = match LauncherPanel::new(vk, self.render_pass, w, h, vk::Format::R8G8B8A8_SRGB) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to create tray panel: {}", e);
                return;
            }
        };

        // Position at right controller location, small square
        panel.width = 0.3;
        panel.height = 0.3;
        panel.opacity = 0.95;
        // Center will be set in tick() based on controller position
        panel.center = Vec3::new(0.0, 1.4, -1.5);

        self.tray_ui = Some(ui);
        self.tray_panel = Some(panel);
        self.tray_visible = true;
        info!("System tray opened.");
    }

    /// Hide and destroy the system tray panel.
    fn hide_system_tray_with_device(&mut self, device: &ash::Device) {
        if let Some(ref mut panel) = self.tray_panel {
            panel.destroy(device);
        }
        self.tray_panel = None;
        self.tray_ui = None;
        self.tray_visible = false;
        info!("System tray closed.");
    }

    /// Hide the system tray (defers destruction to Shell::destroy).
    fn hide_system_tray(&mut self) {
        self.tray_visible = false;
        info!("System tray closed.");
    }

    /// Return a short string for the current anchor mode (used on toolbar button).
    fn anchor_str(&self) -> &'static str {
        match self.anchor {
            PanelAnchor::World => "world",
            PanelAnchor::Theater { .. } => "theater",
            _ => "world",
        }
    }

    /// Update the toolbar texture to reflect the current active view.
    fn update_toolbar(&mut self, vk: &VkBackend) {
        let tab = match self.active_view {
            ViewMode::Launcher => ToolbarTab::Launcher,
            ViewMode::Desktop => ToolbarTab::Screen,
        };
        let anchor = self.anchor_str();
        let pixels = generate_toolbar_pixels(self.toolbar_w, self.toolbar_h, tab, anchor, self.prev_hover_zone);
        self.toolbar_panel.upload_pixels(vk, &pixels).ok();
    }

    /// Show the settings panel to the right of the main panel.
    pub fn show_settings(&mut self, vk: &VkBackend) {
        if self.settings_panel.is_some() { return; }
        let w = 800u32;
        let h = 600u32;
        let html_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui/settings.html");
        let mut ui = match UiRenderer::new(w, h, &html_path) {
            Ok(ui) => ui,
            Err(e) => {
                log::error!("Failed to create settings UI: {}", e);
                return;
            }
        };

        // Inject current config values into the settings page
        let config_json = serde_json::json!({
            "default_view": self.config.shell.default_view,
            "panel_opacity": self.config.panel.opacity,
            "show_fps": self.config.display.show_fps,
            "haptics_enabled": self.config.shell.haptics_enabled,
        });
        ui.evaluate_js(&format!("window.CONFIG = {};", config_json));

        let mut panel = match LauncherPanel::new(vk, self.render_pass, w, h, vk::Format::R8G8B8A8_SRGB) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to create settings panel: {}", e);
                return;
            }
        };
        panel.center = Vec3::new(0.5, 1.6, -2.0); // offset to the right of main panel
        panel.width = 1.0;
        panel.height = 0.75;
        panel.opacity = 0.95;

        if let Some(pixels) = ui.update() {
            if let Err(e) = panel.upload_pixels(vk, pixels) {
                log::error!("Settings initial upload failed: {}", e);
            }
        }

        self.settings_panel = Some(panel);
        self.settings_ui = Some(ui);
        self.settings_visible = true;
        info!("Settings panel opened.");
    }

    /// Hide and destroy the settings panel.
    pub fn hide_settings(&mut self, device: &ash::Device) {
        if let Some(ref mut panel) = self.settings_panel {
            panel.destroy(device);
        }
        self.settings_panel = None;
        self.settings_ui = None;
        self.settings_visible = false;
        info!("Settings panel closed.");
    }

    /// Toggle the settings panel visibility.
    pub fn toggle_settings(&mut self, vk: &VkBackend) {
        if self.settings_visible {
            self.hide_settings(vk.device());
        } else {
            self.show_settings(vk);
        }
    }

    // Virtual keyboard methods disabled for MVP — not wired end-to-end
    // (no text input target, no user action triggers show_keyboard).
    // Kept as comments for future use.
    //
    // pub fn show_keyboard(&mut self, vk: &VkBackend) { ... }
    // pub fn hide_keyboard(&mut self, device: &ash::Device) { ... }

    /// Launch a game by Steam app_id, looked up from the scanned games list.
    fn launch_game(&mut self, app_id: u32, _vk: &VkBackend) {
        // Find the game in our scanned list
        let game = self.games.iter().find(|g| g.app_id == app_id);
        let name = game.map(|g| g.name.as_str()).unwrap_or("Unknown");

        // Prevent double-launch: if an app is already running, don't launch again
        if let Some(ref mut app) = self.launched_app {
            use crate::app::AppStatus;
            match app.status() {
                AppStatus::Running => {
                    self.notifications.push(Notification::info(
                        "Already running",
                        &format!("{}", app.name),
                    ));
                    return;
                }
                _ => {
                    // Previous app exited, clear it and proceed
                    info!("Previous app '{}' has exited, clearing state", app.name);
                }
            }
        }
        self.launched_app = None;

        info!("Launching game: {} (app_id: {})", name, app_id);

        match launch_steam_game(name, app_id) {
            Ok(app) => {
                self.notifications.push(Notification::info(
                    "Launching",
                    &format!("{}...", app.name),
                ));
                self.launched_app = Some(app);
            }
            Err(e) => {
                log::error!("Failed to launch game {}: {}", app_id, e);
                self.notifications.push(Notification::warning(
                    "Failed to launch",
                    &format!("{}", name),
                ));
            }
        }
    }

    /// Apply settings JSON received from the settings UI save button.
    fn apply_settings_json(&mut self, json: &str) {
        #[derive(serde::Deserialize)]
        struct SettingsPayload {
            #[serde(default)]
            default_view: Option<String>,
            #[serde(default)]
            panel_opacity: Option<f32>,
            #[serde(default)]
            show_fps: Option<bool>,
            #[serde(default)]
            haptics_enabled: Option<bool>,
        }

        let payload: SettingsPayload = match serde_json::from_str(json) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to parse settings JSON: {}", e);
                return;
            }
        };

        if let Some(view) = payload.default_view {
            self.config.shell.default_view = view;
        }
        if let Some(opacity) = payload.panel_opacity {
            let clamped = opacity.clamp(0.5, 1.0);
            self.config.panel.opacity = clamped;
            self.launcher_panel.opacity = clamped;
            self.screen_panel.opacity = clamped;
        }
        if let Some(show_fps) = payload.show_fps {
            self.config.display.show_fps = show_fps;
        }
        if let Some(haptics) = payload.haptics_enabled {
            self.config.shell.haptics_enabled = haptics;
        }

        if let Err(e) = self.config.save() {
            log::error!("Failed to save config: {}", e);
        } else {
            info!("Settings saved.");
        }
    }

    /// Returns mutable references to the panels that should be rendered this
    /// frame: the active main panel, the toolbar, the FPS counter, and
    /// optionally the system tray, settings panel, and notification panel.
    pub fn panels_mut(&mut self) -> Vec<&mut LauncherPanel> {
        // We need to borrow different fields simultaneously, so we use raw
        // pointers to satisfy the borrow checker. Each pointer points to a
        // distinct field of Self, which is safe.
        let active: *mut LauncherPanel = match self.active_view {
            ViewMode::Launcher => &mut self.launcher_panel,
            ViewMode::Desktop => &mut self.screen_panel,
        };
        let toolbar: *mut LauncherPanel = &mut self.toolbar_panel;
        let grab_bar: *mut LauncherPanel = &mut self.grab_bar;
        let fps: *mut LauncherPanel = &mut self.fps_panel;
        // SAFETY: active, toolbar, grab_bar, fps, etc. are distinct fields of self.
        let mut panels = unsafe {
            let mut v = vec![&mut *active, &mut *toolbar, &mut *grab_bar];
            if self.config.display.show_fps {
                v.push(&mut *fps);
            }
            v
        };
        if self.tray_visible {
            if let Some(ref mut tray) = self.tray_panel {
                panels.push(tray);
            }
        }
        if self.settings_visible {
            if let Some(ref mut settings) = self.settings_panel {
                panels.push(settings);
            }
        }
        // keyboard disabled for MVP
        // if self.keyboard_visible {
        //     if let Some(ref mut kb) = self.keyboard_panel {
        //         panels.push(kb);
        //     }
        // }
        if let Some(ref mut notif) = self.notification_panel {
            panels.push(notif);
        }
        panels
    }

    /// Record pending texture upload commands for all panels into `cmd`.
    /// Call this *before* the render pass begins.
    pub fn record_uploads(&mut self, device: &ash::Device, cmd: vk::CommandBuffer) {
        for p in self.panels_mut() {
            p.record_upload(device, cmd);
        }
    }

    /// Record draw commands for all panels into `cmd`.
    /// Call this *inside* the render pass, after the scene has been drawn.
    pub fn record_draws(&mut self, device: &ash::Device, cmd: vk::CommandBuffer, push: &crate::renderer::PushConstants) {
        for p in self.panels_mut() {
            p.record_draw(device, cmd, push);
        }
    }

    /// Destroy all Vulkan resources owned by the shell.
    pub fn destroy(&mut self, device: &ash::Device) {
        self.launcher_panel.destroy(device);
        self.screen_panel.destroy(device);
        self.toolbar_panel.destroy(device);
        self.grab_bar.destroy(device);
        self.fps_panel.destroy(device);
        if let Some(ref mut tray) = self.tray_panel {
            tray.destroy(device);
        }
        if let Some(ref mut settings) = self.settings_panel {
            settings.destroy(device);
        }
        // keyboard disabled for MVP
        // if let Some(ref mut kb) = self.keyboard_panel {
        //     kb.destroy(device);
        // }
        if let Some(ref mut notif) = self.notification_panel {
            notif.destroy(device);
        }
    }
}

/// Generate RGBA pixels for a notification toast panel.
///
/// Renders a colored background (based on notification level) with the title
/// and body text drawn using the bitmap font from `launcher_panel`.
fn generate_notification_pixels(width: u32, height: u32, title: &str, body: &str, level: NotificationLevel) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    // Background color based on level
    let (bg_r, bg_g, bg_b) = match level {
        NotificationLevel::Info => (0x10u8, 0x18u8, 0x30u8),
        NotificationLevel::Warning => (0x30u8, 0x28u8, 0x10u8),
        NotificationLevel::Success => (0x10u8, 0x30u8, 0x18u8),
    };

    // Colored left border based on level
    let (border_r, border_g, border_b) = match level {
        NotificationLevel::Info => (0x40u8, 0x80u8, 0xFFu8),
        NotificationLevel::Warning => (0xFFu8, 0xC0u8, 0x30u8),
        NotificationLevel::Success => (0x30u8, 0xE0u8, 0x60u8),
    };
    let border_width = 4u32;

    // Draw background with slight vertical gradient and colored left border
    for y in 0..height {
        // Gradient factor: slightly lighter at top, darker at bottom
        let grad = 1.0 - (y as f32 / height as f32) * 0.15;
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;

            if x < border_width {
                // Colored left border
                pixels[idx] = border_r;
                pixels[idx + 1] = border_g;
                pixels[idx + 2] = border_b;
                pixels[idx + 3] = 0xF0;
            } else {
                // Background with gradient
                pixels[idx] = (bg_r as f32 * grad).min(255.0) as u8;
                pixels[idx + 1] = (bg_g as f32 * grad).min(255.0) as u8;
                pixels[idx + 2] = (bg_b as f32 * grad).min(255.0) as u8;
                pixels[idx + 3] = 0xE0;
            }
        }
    }

    // Title at top (scale 2 for bigger text, bright white) — offset right of border
    let text_x = border_width + 6;
    let title_scale = 2u32;
    draw_text(&mut pixels, width, height, title, text_x, 8, title_scale, 0xFF, 0xFF, 0xFF);

    // Body below title (scale 1, lighter color)
    let body_y = 8 + 7 * title_scale + 6; // below title with small gap
    draw_text(&mut pixels, width, height, body, text_x, body_y, 1, 0xCC, 0xCC, 0xCC);

    pixels
}

/// Cycle through anchor modes: World -> Theater -> World.
/// Controller mode removed from visible cycle for MVP (functional but crude).
fn cycle_anchor(current: PanelAnchor) -> PanelAnchor {
    match current {
        PanelAnchor::World => PanelAnchor::Theater { distance: 5.0, scale: 3.0 },
        PanelAnchor::Theater { .. } => PanelAnchor::World,
        _ => PanelAnchor::World,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_mode_equality() {
        assert_eq!(ViewMode::Launcher, ViewMode::Launcher);
        assert_ne!(ViewMode::Launcher, ViewMode::Desktop);
    }

    #[test]
    fn shell_switch_view() {
        // Without Vulkan we cannot fully construct a Shell, but we can
        // verify the ViewMode toggle logic in isolation.
        let mut view = ViewMode::Launcher;
        assert_eq!(view, ViewMode::Launcher);

        // Simulate clicking the Desktop zone (0.35 <= u < 0.70)
        let u = 0.50;
        let new_view = if u < 0.35 { ViewMode::Launcher } else if u < 0.70 { ViewMode::Desktop } else { view };
        if new_view != view {
            view = new_view;
        }
        assert_eq!(view, ViewMode::Desktop);

        // Simulate clicking the Launcher zone (u < 0.35)
        let u = 0.20;
        let new_view = if u < 0.35 { ViewMode::Launcher } else if u < 0.70 { ViewMode::Desktop } else { view };
        if new_view != view {
            view = new_view;
        }
        assert_eq!(view, ViewMode::Launcher);
    }

    #[test]
    fn shell_frame_default_ray() {
        let frame = ShellFrame { left_ray_hit_dist: 0.0, right_ray_hit_dist: 0.0, haptic_left: None, haptic_right: None };
        assert_eq!(frame.right_ray_hit_dist, 0.0);
    }

    #[test]
    fn shell_frame_with_hit() {
        let frame = ShellFrame { left_ray_hit_dist: 0.0, right_ray_hit_dist: 2.5, haptic_left: None, haptic_right: None };
        assert!((frame.right_ray_hit_dist - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn generate_notification_pixels_info() {
        let pixels = generate_notification_pixels(256, 64, "Test", "Hello world", NotificationLevel::Info);
        assert_eq!(pixels.len(), 256 * 64 * 4);
        // First 4 pixels are the colored left border (Info blue: 0x40, 0x80, 0xFF)
        assert_eq!(pixels[0], 0x40);
        assert_eq!(pixels[1], 0x80);
        assert_eq!(pixels[2], 0xFF);
        assert_eq!(pixels[3], 0xF0);
        // Pixel past the border (x=5, y=0) should be background (Info: 0x10, 0x18, 0x30)
        let bg_idx = (5 * 4) as usize;
        assert_eq!(pixels[bg_idx], 0x10);
        assert_eq!(pixels[bg_idx + 1], 0x18);
        assert_eq!(pixels[bg_idx + 2], 0x30);
        assert_eq!(pixels[bg_idx + 3], 0xE0);
    }

    #[test]
    fn generate_notification_pixels_warning() {
        let pixels = generate_notification_pixels(256, 64, "Warn", "msg", NotificationLevel::Warning);
        assert_eq!(pixels.len(), 256 * 64 * 4);
        // Left border should be Warning amber (0xFF, 0xC0, 0x30)
        assert_eq!(pixels[0], 0xFF);
        assert_eq!(pixels[1], 0xC0);
        assert_eq!(pixels[2], 0x30);
        // Background past border
        let bg_idx = (5 * 4) as usize;
        assert_eq!(pixels[bg_idx], 0x30);
        assert_eq!(pixels[bg_idx + 1], 0x28);
        assert_eq!(pixels[bg_idx + 2], 0x10);
    }

    #[test]
    fn generate_notification_pixels_success() {
        let pixels = generate_notification_pixels(128, 32, "OK", "done", NotificationLevel::Success);
        assert_eq!(pixels.len(), 128 * 32 * 4);
        // Left border should be Success green (0x30, 0xE0, 0x60)
        assert_eq!(pixels[0], 0x30);
        assert_eq!(pixels[1], 0xE0);
        assert_eq!(pixels[2], 0x60);
        // Background past border
        let bg_idx = (5 * 4) as usize;
        assert_eq!(pixels[bg_idx], 0x10);
        assert_eq!(pixels[bg_idx + 1], 0x30);
        assert_eq!(pixels[bg_idx + 2], 0x18);
    }
}
