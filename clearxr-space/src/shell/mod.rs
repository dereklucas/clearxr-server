//! Shell orchestrator for the ClearXR VR environment.
//!
//! Thin wrapper around Dashboard. The Shell owns one Dashboard (which handles
//! all UI rendering, game scanning, screen capture, notifications, etc.) plus
//! an InputDispatcher for controller hit-testing.

pub mod boundary;
pub mod dashboard;
pub mod notifications;

use ash::vk;
use anyhow::Result;
use glam::Vec3;
use log::info;

use crate::app;
use crate::config::Config;
use crate::input::{ControllerState, InputDispatcher, InputEvent, Hand};
use crate::launcher_panel::LauncherPanel;
use crate::panel::{PanelAnchor, PanelId, PanelTransform};
use crate::shell::dashboard::{Dashboard, DashboardAction, DashboardTab, DASHBOARD_PANEL_ID, SCREEN_PANEL_ID};
use crate::shell::notifications::Notification;
use crate::vk_backend::VkBackend;

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
/// Thin wrapper around Dashboard. Owns the input dispatcher and delegates
/// all UI, rendering, and state management to the Dashboard.
pub struct Shell {
    /// The unified dashboard that owns all UI panels and state.
    pub dashboard: Dashboard,
    input: InputDispatcher,
    prev_menu_click: bool,
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
        let mut dashboard = Dashboard::new(config, vk, render_pass)?;
        // If --screen flag, start on desktop tab
        if use_screen_capture {
            dashboard.active_tab = DashboardTab::Desktop;
        }

        Ok(Self {
            dashboard,
            input: InputDispatcher::new(),
            prev_menu_click: false,
        })
    }

    /// Per-frame update: input, content, FPS, ray clipping.
    ///
    /// The caller passes the current `ControllerState` (extracted from OpenXR)
    /// and the Vulkan backend (for texture uploads). Returns a `ShellFrame`
    /// with the ray-hit distance the caller should write into `HandData`.
    pub fn tick(&mut self, vk: &VkBackend, controller: &ControllerState) -> ShellFrame {
        let mut haptic: [Option<HapticPulse>; 2] = [None, None];

        // 1. Clear pointer state + release stuck buttons if not pointing at anything
        self.dashboard.pointer_leave();
        self.dashboard.panel.dot_uv = None;

        // 2. System button toggle (menu button OR Y button on left controller)
        let menu = controller.left.menu_click || controller.right.menu_click
            || controller.left.y_click;
        if menu && !self.prev_menu_click {
            self.dashboard.visible = !self.dashboard.visible;
            info!(
                "Dashboard {}",
                if self.dashboard.visible { "shown" } else { "hidden" }
            );
        }
        self.prev_menu_click = menu;

        if !self.dashboard.visible {
            return ShellFrame {
                left_ray_hit_dist: 0.0,
                right_ray_hit_dist: 0.0,
                haptic_left: None,
                haptic_right: None,
            };
        }

        // 3. Build input panel list
        let dashboard_transform = self.dashboard.transform();
        let mut input_panels = vec![(DASHBOARD_PANEL_ID, dashboard_transform)];
        // Add screen panel if desktop tab
        if self.dashboard.active_tab == DashboardTab::Desktop {
            let screen_transform = PanelTransform {
                center: self.dashboard.screen_panel.center,
                right_dir: self.dashboard.screen_panel.right_dir,
                up_dir: self.dashboard.screen_panel.up_dir,
                width: self.dashboard.screen_panel.width,
                height: self.dashboard.screen_panel.height,
                opacity: self.dashboard.screen_panel.opacity,
                anchor: PanelAnchor::World,
                grabbable: false,
            };
            // Screen panel behind dashboard -- insert first
            input_panels.insert(0, (SCREEN_PANEL_ID, screen_transform));
        }

        let panel_refs: Vec<(PanelId, &PanelTransform)> = input_panels
            .iter()
            .map(|(id, t)| (*id, t))
            .collect();
        let events = self.input.process(controller, &panel_refs);

        // 4. Dispatch events
        let mut per_hand_ray = [0.0f32; 2];
        for (panel_id, event) in &events {
            match event {
                InputEvent::PointerMove {
                    hand,
                    u,
                    v,
                    distance,
                } => {
                    let hand_idx = match hand {
                        Hand::Left => 0,
                        Hand::Right => 1,
                    };
                    if per_hand_ray[hand_idx] == 0.0 || *distance < per_hand_ray[hand_idx] {
                        per_hand_ray[hand_idx] = *distance;
                    }
                    if *panel_id == DASHBOARD_PANEL_ID {
                        self.dashboard.pointer_move(*u, *v);
                        // Don't show pointer dot on desktop tab (OS cursor is visible)
                        if self.dashboard.active_tab != DashboardTab::Desktop {
                            self.dashboard.panel.dot_uv = Some((*u, *v));
                        }
                    } else if *panel_id == SCREEN_PANEL_ID {
                        self.dashboard.screen_capture.inject_mouse_move(*u, *v);
                        self.dashboard.last_screen_uv = (*u, *v);
                    }
                }
                InputEvent::PointerDown { hand, u, v } => {
                    let hand_idx = match hand {
                        Hand::Left => 0,
                        Hand::Right => 1,
                    };
                    haptic[hand_idx] = Some(HapticPulse {
                        duration_ms: 20,
                        frequency: 200.0,
                        amplitude: 0.2,
                    });
                    if *panel_id == DASHBOARD_PANEL_ID {
                        self.dashboard.click();        // instant for egui buttons
                        self.dashboard.trigger_down(); // continuous for drag/select
                    } else if *panel_id == SCREEN_PANEL_ID {
                        self.dashboard.screen_capture.inject_mouse_down(*u, *v);
                    }
                }
                InputEvent::PointerUp { hand: _, u: _, v: _ } => {
                    if *panel_id == DASHBOARD_PANEL_ID {
                        self.dashboard.trigger_up();
                    } else if *panel_id == SCREEN_PANEL_ID {
                        self.dashboard.screen_capture.inject_mouse_up();
                    }
                }
                InputEvent::GrabStart {
                    hand, grip_pos, ..
                } => {
                    if *panel_id == DASHBOARD_PANEL_ID {
                        let center = self.dashboard.panel.center;
                        self.dashboard.grab_offset = Some(center - *grip_pos);
                        self.dashboard.grab_hand = Some(match hand {
                            Hand::Left => 0,
                            Hand::Right => 1,
                        });

                        // Orbital drag: store initial angles
                        // Use the controller's AIM direction for angular tracking
                        // (more stable than position-based atan2 which is noisy at close range)
                        let head = Vec3::new(0.0, 1.6, 0.0);
                        let to_panel = center - head;
                        let dist = to_panel.length().max(0.5);
                        self.dashboard.grab_initial_distance = dist;
                        self.dashboard.grab_initial_yaw = to_panel.x.atan2(-to_panel.z);
                        self.dashboard.grab_initial_pitch = (to_panel.y / dist).asin();

                        // Store the controller's aim direction angles and distance at grab start
                        let hand_state = match hand { Hand::Left => &controller.left, Hand::Right => &controller.right };
                        let aim = hand_state.aim_dir;
                        let grip_dist = (hand_state.grip_pos - head).length().max(0.1);
                        self.dashboard.grab_controller_start_distance = grip_dist;
                        self.dashboard.base_width = self.dashboard.panel.width;
                        self.dashboard.base_height = self.dashboard.panel.height;
                        self.dashboard.grab_controller_start_yaw = aim.x.atan2(-aim.z);
                        self.dashboard.grab_controller_start_pitch = aim.y.asin();

                        let hi = match hand {
                            Hand::Left => 0,
                            Hand::Right => 1,
                        };
                        haptic[hi] = Some(HapticPulse {
                            duration_ms: 50,
                            frequency: 200.0,
                            amplitude: 0.6,
                        });
                    }
                }
                _ => {}
            }
        }

        // 4b. Safety: release trigger/secondary if no hand is actually pressing
        // (prevents stuck buttons when pointer leaves all panels)
        let any_trigger = (controller.left.active && controller.left.trigger > 0.5)
            || (controller.right.active && controller.right.trigger > 0.5);
        if !any_trigger && self.dashboard.trigger_pressed {
            self.dashboard.trigger_up();
        }
        let any_grip = (controller.left.active && controller.left.squeeze > 0.5)
            || (controller.right.active && controller.right.squeeze > 0.5);
        if !any_grip && self.dashboard.secondary_pressed {
            if self.dashboard.active_tab == DashboardTab::Desktop {
                self.dashboard.screen_capture.inject_right_mouse_up();
            }
            self.dashboard.secondary_up();
        }
        // Also safety-release left mouse on desktop
        if !any_trigger && self.dashboard.active_tab == DashboardTab::Desktop {
            self.dashboard.screen_capture.inject_mouse_up();
        }

        // 5. Grab continue/release (orbital drag around user's head)
        if let Some(hand_idx) = self.dashboard.grab_hand {
            let hands = [&controller.left, &controller.right];
            let hand = hands[hand_idx];
            let holding = hand.squeeze > 0.3 || hand.trigger > 0.3;
            if holding {
                let head = Vec3::new(0.0, 1.6, 0.0);
                // Use aim direction for smooth angular tracking
                let aim = hand.aim_dir;
                let grip_yaw = aim.x.atan2(-aim.z);
                let grip_pitch = aim.y.asin();

                // Angular delta from grab start
                let dyaw = grip_yaw - self.dashboard.grab_controller_start_yaw;
                let dpitch = grip_pitch - self.dashboard.grab_controller_start_pitch;

                // Distance scaling: hand distance from head relative to grab start
                // Amplify the ratio so small hand movements produce noticeable distance changes
                let grip_dist = (hand.grip_pos - head).length().max(0.1);
                let raw_ratio = grip_dist / self.dashboard.grab_controller_start_distance.max(0.1);
                // Amplify: small hand movements produce large distance changes (8x sensitivity)
                let amplified = 1.0 + (raw_ratio - 1.0) * 8.0;
                let new_dist = (self.dashboard.grab_initial_distance * amplified).clamp(0.8, 10.0);

                // Apply to initial panel angles
                let new_yaw = self.dashboard.grab_initial_yaw + dyaw;
                let new_pitch = self.dashboard.grab_initial_pitch + dpitch;

                // Spherical to Cartesian
                let new_center = head + Vec3::new(
                    new_dist * new_pitch.cos() * new_yaw.sin(),
                    new_dist * new_pitch.sin(),
                    -new_dist * new_pitch.cos() * new_yaw.cos(),
                );

                self.dashboard.panel.center = new_center;
                self.dashboard.screen_panel.center = new_center;

                // Scale panel size proportionally with distance
                let scale = new_dist / self.dashboard.grab_initial_distance.max(0.1);
                self.dashboard.panel.width = self.dashboard.base_width * scale;
                self.dashboard.panel.height = self.dashboard.base_height * scale;

                // Panel always faces user
                let fwd = (new_center - head).normalize();
                self.dashboard.panel.right_dir = fwd.cross(Vec3::Y).normalize();
                self.dashboard.panel.up_dir = Vec3::Y;
                self.dashboard.screen_panel.right_dir = self.dashboard.panel.right_dir;
                self.dashboard.screen_panel.up_dir = Vec3::Y;
            } else {
                self.dashboard.grab_offset = None;
                self.dashboard.grab_hand = None;
                haptic[hand_idx] = Some(HapticPulse {
                    duration_ms: 30,
                    frequency: 150.0,
                    amplitude: 0.3,
                });
            }
        }

        // 5b. Grip right-click (when not grabbing)
        if self.dashboard.grab_hand.is_none() {
            let is_desktop = self.dashboard.active_tab == DashboardTab::Desktop;
            for (_hand_idx, hand) in [(0usize, &controller.left), (1usize, &controller.right)] {
                if hand.active && hand.squeeze >= 0.5 {
                    if is_desktop {
                        // Right-click on desktop via screen capture
                        // Use the last known pointer UV for positioning
                        let (u, v) = self.dashboard.last_screen_uv;
                        self.dashboard.screen_capture.inject_right_mouse_down(u, v);
                    }
                    self.dashboard.secondary_down();
                } else if hand.active && hand.squeeze < 0.3 && self.dashboard.secondary_pressed {
                    if is_desktop {
                        self.dashboard.screen_capture.inject_right_mouse_up();
                    }
                    self.dashboard.secondary_up();
                }
            }
        }

        // 5c. Thumbstick scroll (when not grabbing)
        if self.dashboard.grab_hand.is_none() {
            let is_desktop = self.dashboard.active_tab == DashboardTab::Desktop;
            let hands = [&controller.left, &controller.right];
            for hand in hands {
                if hand.active && (hand.thumbstick[0].abs() > 0.2 || hand.thumbstick[1].abs() > 0.2) {
                    if is_desktop {
                        // Scroll on desktop via screen capture
                        let dy = -(hand.thumbstick[1] * 3.0) as i32;
                        let dx = -(hand.thumbstick[0] * 3.0) as i32;
                        self.dashboard.screen_capture.inject_scroll(dx, dy);
                    }
                    // Also send to egui for dashboard scroll
                    self.dashboard.scroll_delta = hand.thumbstick[1] * 20.0;
                }
            }
        }

        // 6. Render dashboard
        let actions = self.dashboard.render(vk);

        // 7. Handle actions
        for action in actions {
            match action {
                DashboardAction::LaunchGame(app_id) => {
                    if let Some(game) = self.dashboard.games.iter().find(|g| g.app_id == app_id) {
                        let game_name = game.name.clone();
                        match app::launch_steam_game(&game_name, app_id) {
                            Ok(launched_app) => {
                                self.dashboard
                                    .notifications
                                    .push(Notification::info("Launching", &game_name));
                                self.dashboard.launched_app = Some(launched_app);
                            }
                            Err(e) => {
                                self.dashboard
                                    .notifications
                                    .push(Notification::warning("Failed", &e));
                            }
                        }
                    }
                }
                DashboardAction::SaveConfig => {}
                DashboardAction::Screenshot => {
                    self.dashboard.screenshot_requested = true;
                }
                DashboardAction::CycleAnchor => {
                    // future
                }
                DashboardAction::None => {}
                DashboardAction::ToggleVisibility => {
                    self.dashboard.visible = !self.dashboard.visible;
                }
            }
        }

        // 8. Monitor launched app
        if let Some(ref mut launched_app) = self.dashboard.launched_app {
            match launched_app.status() {
                crate::app::AppStatus::Running => {}
                _ => {
                    let name = launched_app.name.clone();
                    self.dashboard
                        .notifications
                        .push(Notification::info("Game ended", &name));
                    self.dashboard.launched_app = None;
                }
            }
        }

        // 9. Screenshot trigger (both triggers)
        let both = controller.left.trigger > 0.8 && controller.right.trigger > 0.8;
        if both && !self.dashboard.prev_both_triggers {
            self.dashboard.screenshot_requested = true;
            self.dashboard.notifications.push(Notification::success(
                "Screenshot",
                "Saved to Pictures/ClearXR",
            ));
        }
        self.dashboard.prev_both_triggers = both;

        // 10. Gate haptics on config
        if !self.dashboard.config.shell.haptics_enabled {
            haptic = [None, None];
        }

        ShellFrame {
            left_ray_hit_dist: per_hand_ray[0],
            right_ray_hit_dist: per_hand_ray[1],
            haptic_left: haptic[0].take(),
            haptic_right: haptic[1].take(),
        }
    }

    /// Returns mutable references to the panels that should be rendered this
    /// frame. Delegates to Dashboard.
    pub fn panels_mut(&mut self) -> Vec<&mut LauncherPanel> {
        self.dashboard.panels_mut()
    }

    /// Record pending texture upload commands for all panels into `cmd`.
    /// Call this *before* the render pass begins.
    #[allow(dead_code)] // Public API for XR render loop
    pub fn record_uploads(&mut self, device: &ash::Device, cmd: vk::CommandBuffer) {
        for p in self.panels_mut() {
            p.record_upload(device, cmd);
        }
    }

    /// Record draw commands for all panels into `cmd`.
    /// Call this *inside* the render pass, after the scene has been drawn.
    #[allow(dead_code)] // Public API for XR render loop
    pub fn record_draws(
        &mut self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        push: &crate::renderer::PushConstants,
    ) {
        for p in self.panels_mut() {
            p.record_draw(device, cmd, push);
        }
    }

    /// Destroy all Vulkan resources owned by the shell.
    pub fn destroy(&mut self, vk: &crate::vk_backend::VkBackend) {
        self.dashboard.destroy(vk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::dashboard::DashboardTab;

    #[test]
    fn dashboard_tab_equality() {
        assert_eq!(DashboardTab::Launcher, DashboardTab::Launcher);
        assert_ne!(DashboardTab::Launcher, DashboardTab::Desktop);
    }

    #[test]
    fn shell_frame_default_ray() {
        let frame = ShellFrame {
            left_ray_hit_dist: 0.0,
            right_ray_hit_dist: 0.0,
            haptic_left: None,
            haptic_right: None,
        };
        assert_eq!(frame.right_ray_hit_dist, 0.0);
    }

    #[test]
    fn shell_frame_with_hit() {
        let frame = ShellFrame {
            left_ray_hit_dist: 0.0,
            right_ray_hit_dist: 2.5,
            haptic_left: None,
            haptic_right: None,
        };
        assert!((frame.right_ray_hit_dist - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn settings_save_round_trip() {
        // Create a Config, modify values, serialize to TOML, deserialize back
        let mut config = Config::default();
        config.panel.opacity = 0.75;
        config.shell.default_view = "desktop".into();
        config.display.show_fps = false;
        let toml_str = toml::to_string(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.panel.opacity, 0.75);
        assert_eq!(loaded.shell.default_view, "desktop");
        assert!(!loaded.display.show_fps);
    }

    #[test]
    fn dashboard_tab_toggle() {
        // Verify switching between Launcher and Desktop
        let mut tab = DashboardTab::Launcher;
        assert_eq!(tab, DashboardTab::Launcher);
        tab = DashboardTab::Desktop;
        assert_eq!(tab, DashboardTab::Desktop);
        assert_ne!(DashboardTab::Launcher, DashboardTab::Desktop);
    }

    #[test]
    fn spherical_round_trip() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let panel_pos = Vec3::new(1.0, 2.0, -2.0);
        let to_panel = panel_pos - head;
        let dist = to_panel.length();
        let yaw = to_panel.x.atan2(-to_panel.z);
        let pitch = (to_panel.y / dist).asin();

        // Convert back (same formula used in Shell::tick orbital drag)
        let reconstructed = head + Vec3::new(
            dist * pitch.cos() * yaw.sin(),
            dist * pitch.sin(),
            -dist * pitch.cos() * yaw.cos(),
        );

        assert!(
            (reconstructed - panel_pos).length() < 0.01,
            "Round-trip failed: {:?} vs {:?}", reconstructed, panel_pos
        );
    }

    #[test]
    fn spherical_round_trip_negative_x() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let panel_pos = Vec3::new(-1.5, 1.0, -3.0);
        let to_panel = panel_pos - head;
        let dist = to_panel.length();
        let yaw = to_panel.x.atan2(-to_panel.z);
        let pitch = (to_panel.y / dist).asin();

        let reconstructed = head + Vec3::new(
            dist * pitch.cos() * yaw.sin(),
            dist * pitch.sin(),
            -dist * pitch.cos() * yaw.cos(),
        );

        assert!(
            (reconstructed - panel_pos).length() < 0.01,
            "Round-trip failed for negative X: {:?} vs {:?}", reconstructed, panel_pos
        );
    }

    #[test]
    fn distance_amplification_8x() {
        let start_dist: f32 = 0.6;
        let new_dist: f32 = 0.66; // 10% increase
        let raw_ratio = new_dist / start_dist;
        let amplified = 1.0 + (raw_ratio - 1.0) * 8.0;
        // 10% raw -> 80% amplified -> ratio of 1.8
        assert!((amplified - 1.8).abs() < 0.01,
            "Expected ~1.8 amplified ratio, got {}", amplified);
    }

    #[test]
    fn distance_amplification_no_change() {
        let start_dist: f32 = 0.6;
        let new_dist: f32 = 0.6; // no change
        let raw_ratio = new_dist / start_dist;
        let amplified = 1.0 + (raw_ratio - 1.0) * 8.0;
        assert!((amplified - 1.0).abs() < 0.001,
            "No hand movement should produce ratio 1.0, got {}", amplified);
    }

    #[test]
    fn distance_amplification_decrease() {
        let start_dist: f32 = 0.6;
        let new_dist: f32 = 0.54; // 10% decrease
        let raw_ratio = new_dist / start_dist;
        let amplified = 1.0 + (raw_ratio - 1.0) * 8.0;
        // 10% decrease -> 80% decrease -> ratio of 0.2
        assert!((amplified - 0.2).abs() < 0.01,
            "10% closer should produce ~0.2 ratio, got {}", amplified);
    }

    #[test]
    fn panel_faces_user_after_orbital_move() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let new_center = Vec3::new(2.0, 2.0, -3.0);
        let fwd = (new_center - head).normalize();
        let right = fwd.cross(Vec3::Y).normalize();
        // right should be perpendicular to forward and Y
        assert!(right.dot(fwd).abs() < 0.01, "right not perpendicular to forward");
        assert!(right.dot(Vec3::Y).abs() < 0.01, "right not perpendicular to Y");
    }

    #[test]
    fn panel_faces_user_directly_ahead() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let new_center = Vec3::new(0.0, 1.6, -3.0); // directly ahead
        let fwd = (new_center - head).normalize();
        let right = fwd.cross(Vec3::Y).normalize();
        // Should be pure +X when looking straight ahead
        assert!((right - Vec3::X).length() < 0.01,
            "Facing straight ahead, right should be +X, got {:?}", right);
    }

    #[test]
    fn haptic_pulse_struct() {
        let pulse = HapticPulse {
            duration_ms: 50,
            frequency: 200.0,
            amplitude: 0.6,
        };
        assert_eq!(pulse.duration_ms, 50);
        assert!((pulse.frequency - 200.0).abs() < f32::EPSILON);
        assert!((pulse.amplitude - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn shell_frame_haptic_fields() {
        let frame = ShellFrame {
            left_ray_hit_dist: 1.5,
            right_ray_hit_dist: 2.5,
            haptic_left: Some(HapticPulse { duration_ms: 20, frequency: 200.0, amplitude: 0.2 }),
            haptic_right: None,
        };
        assert!(frame.haptic_left.is_some());
        assert!(frame.haptic_right.is_none());
        assert!((frame.left_ray_hit_dist - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn spherical_round_trip_behind_head() {
        // Panel behind the user (positive Z)
        let head = Vec3::new(0.0, 1.6, 0.0);
        let panel_pos = Vec3::new(0.5, 2.0, 2.0);
        let to_panel = panel_pos - head;
        let dist = to_panel.length();
        let yaw = to_panel.x.atan2(-to_panel.z);
        let pitch = (to_panel.y / dist).asin();

        let reconstructed = head + Vec3::new(
            dist * pitch.cos() * yaw.sin(),
            dist * pitch.sin(),
            -dist * pitch.cos() * yaw.cos(),
        );

        assert!(
            (reconstructed - panel_pos).length() < 0.01,
            "Round-trip for behind-head failed: {:?} vs {:?}", reconstructed, panel_pos
        );
    }

    #[test]
    fn distance_clamp_range() {
        // Verify the clamping behavior used in grab logic
        let grab_initial_distance = 2.0;
        let amplified_min = 0.01_f32; // very small ratio
        let amplified_max = 100.0_f32; // very large ratio

        let clamped_min = (grab_initial_distance * amplified_min).clamp(0.8, 10.0);
        let clamped_max = (grab_initial_distance * amplified_max).clamp(0.8, 10.0);

        assert!((clamped_min - 0.8).abs() < 0.01, "Minimum should clamp to 0.8m");
        assert!((clamped_max - 10.0).abs() < 0.01, "Maximum should clamp to 10.0m");
    }
}
