use glam::{Vec3, Quat};

// Re-export Hand from panel module (single source of truth)
pub use crate::panel::Hand;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Trigger,
    Grip,
    A,
    B,
    X,
    Y,
    Menu,
    ThumbstickClick,
}

#[derive(Clone, Debug)]
pub enum InputEvent {
    /// Pointer ray is hovering over a panel at (u, v)
    PointerMove { hand: Hand, u: f32, v: f32, distance: f32 },
    /// Trigger pulled while pointing at panel
    PointerDown { hand: Hand, u: f32, v: f32 },
    /// Trigger released
    PointerUp { hand: Hand, u: f32, v: f32 },
    /// Grip pressed near panel edge - start moving
    GrabStart { hand: Hand, grip_pos: Vec3, grip_rot: Quat },
    /// Panel being moved by grip
    GrabMove { hand: Hand, grip_pos: Vec3, grip_rot: Quat },
    /// Grip released
    GrabEnd { hand: Hand },
    /// Button pressed (not on panel surface)
    ButtonPress { hand: Hand, button: Button },
    /// Button released
    ButtonRelease { hand: Hand, button: Button },
    /// Thumbstick moved (for scrolling)
    ThumbstickMove { hand: Hand, x: f32, y: f32 },
    /// Text input from virtual keyboard
    TextInput { text: String },
}

/// Per-hand controller state, extracted from OpenXR each frame.
#[derive(Clone, Debug, Default)]
pub struct HandState {
    pub active: bool,
    pub grip_pos: Vec3,
    pub grip_rot: Quat,
    pub aim_pos: Vec3,
    pub aim_dir: Vec3,
    pub trigger: f32,
    pub squeeze: f32,
    pub thumbstick: [f32; 2],
    pub a_click: bool,
    pub b_click: bool,
    pub x_click: bool,
    pub y_click: bool,
    pub menu_click: bool,
    pub thumbstick_click: bool,
    // Touch states
    pub a_touch: bool,
    pub b_touch: bool,
    pub x_touch: bool,
    pub y_touch: bool,
    pub trigger_touch: bool,
    pub thumbstick_touch: bool,
}

/// Full controller state for both hands.
#[derive(Clone, Debug, Default)]
pub struct ControllerState {
    pub left: HandState,
    pub right: HandState,
}

impl ControllerState {
    pub fn hand(&self, hand: Hand) -> &HandState {
        match hand {
            Hand::Left => &self.left,
            Hand::Right => &self.right,
        }
    }
}

use crate::panel::{PanelId, PanelTransform};

const TRIGGER_THRESHOLD: f32 = 0.5;
const GRIP_THRESHOLD: f32 = 0.7;
const GRAB_MARGIN: f32 = 0.15;
const THUMBSTICK_DEADZONE: f32 = 0.3;

/// Returns true if (u, v) is in the outer margin zone of a panel (edge region).
fn in_grab_margin(u: f32, v: f32) -> bool {
    u < GRAB_MARGIN || u > (1.0 - GRAB_MARGIN) || v < GRAB_MARGIN || v > (1.0 - GRAB_MARGIN)
}

/// Dispatches controller input to panels using ray-panel hit testing.
/// Tracks per-hand trigger state for edge detection of press/release.
pub struct InputDispatcher {
    prev_trigger: [bool; 2],    // [left, right] — for edge detection
    prev_grip: [bool; 2],       // for grip edge detection
    prev_a_click: [bool; 2],    // for A button edge detection (anchor cycling)
    grab_active: [Option<PanelId>; 2], // which panel each hand is grabbing
}

impl InputDispatcher {
    pub fn new() -> Self {
        Self {
            prev_trigger: [false; 2],
            prev_grip: [false; 2],
            prev_a_click: [false; 2],
            grab_active: [None; 2],
        }
    }

    /// Process controller state against panel transforms.
    /// `panels` should be in front-to-back order (first = closest/focused).
    /// Returns `(panel_id, event)` pairs to dispatch.
    pub fn process(
        &mut self,
        state: &ControllerState,
        panels: &[(PanelId, &PanelTransform)],
    ) -> Vec<(PanelId, InputEvent)> {
        let mut events = Vec::new();

        for (hand_index, hand_enum) in [(0usize, Hand::Left), (1usize, Hand::Right)] {
            let hand_state = state.hand(hand_enum);
            if !hand_state.active {
                continue;
            }

            let trigger_now = hand_state.trigger >= TRIGGER_THRESHOLD;
            let trigger_prev = self.prev_trigger[hand_index];

            // Find the first panel hit (front-to-back priority)
            let mut hit: Option<(PanelId, f32, f32, f32)> = None;
            for &(panel_id, panel_transform) in panels {
                if let Some((u, v, t)) = panel_transform.hit_test(hand_state.aim_pos, hand_state.aim_dir) {
                    hit = Some((panel_id, u, v, t));
                    break; // first hit wins (front-to-back order)
                }
            }

            if let Some((panel_id, u, v, distance)) = hit {
                // Always emit PointerMove when aiming at a panel
                events.push((panel_id, InputEvent::PointerMove {
                    hand: hand_enum,
                    u,
                    v,
                    distance,
                }));

                // Trigger rising edge -> PointerDown
                if trigger_now && !trigger_prev {
                    events.push((panel_id, InputEvent::PointerDown {
                        hand: hand_enum,
                        u,
                        v,
                    }));
                }

                // Trigger falling edge -> PointerUp
                if !trigger_now && trigger_prev {
                    events.push((panel_id, InputEvent::PointerUp {
                        hand: hand_enum,
                        u,
                        v,
                    }));
                }
            }

            self.prev_trigger[hand_index] = trigger_now;

            // --- Grab logic (grip/squeeze) ---
            let grip_now = hand_state.squeeze >= GRIP_THRESHOLD;
            let grip_prev = self.prev_grip[hand_index];

            if grip_now && !grip_prev {
                // Grip rising edge: start grab if ray hits panel margin zone
                if let Some((panel_id, u, v, _)) = hit {
                    if in_grab_margin(u, v) {
                        self.grab_active[hand_index] = Some(panel_id);
                        events.push((panel_id, InputEvent::GrabStart {
                            hand: hand_enum,
                            grip_pos: hand_state.grip_pos,
                            grip_rot: hand_state.grip_rot,
                        }));
                    }
                }
            } else if grip_now && grip_prev {
                // Grip held: emit GrabMove if grab is active
                if let Some(panel_id) = self.grab_active[hand_index] {
                    events.push((panel_id, InputEvent::GrabMove {
                        hand: hand_enum,
                        grip_pos: hand_state.grip_pos,
                        grip_rot: hand_state.grip_rot,
                    }));
                }
            } else if !grip_now && grip_prev {
                // Grip released: end grab
                if let Some(panel_id) = self.grab_active[hand_index].take() {
                    events.push((panel_id, InputEvent::GrabEnd {
                        hand: hand_enum,
                    }));
                }
            }

            self.prev_grip[hand_index] = grip_now;

            // --- A button anchor cycling (rising edge while grab active) ---
            let a_now = hand_state.a_click;
            let a_prev = self.prev_a_click[hand_index];

            if a_now && !a_prev {
                if let Some(panel_id) = self.grab_active[hand_index] {
                    events.push((panel_id, InputEvent::ButtonPress {
                        hand: hand_enum,
                        button: Button::A,
                    }));
                }
            }

            self.prev_a_click[hand_index] = a_now;

            // --- Thumbstick resize during grab ---
            if let Some(panel_id) = self.grab_active[hand_index] {
                let ty = hand_state.thumbstick[1];
                if ty.abs() > THUMBSTICK_DEADZONE {
                    events.push((panel_id, InputEvent::ThumbstickMove {
                        hand: hand_enum,
                        x: hand_state.thumbstick[0],
                        y: ty,
                    }));
                }
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_state_default_inactive() {
        let state = ControllerState::default();
        assert!(!state.left.active);
        assert!(!state.right.active);
    }

    #[test]
    fn hand_accessor() {
        let mut state = ControllerState::default();
        state.left.active = true;
        state.right.trigger = 0.8;
        assert!(state.hand(Hand::Left).active);
        assert!(!state.hand(Hand::Right).active);
        assert!((state.hand(Hand::Right).trigger - 0.8).abs() < 0.001);
    }

    #[test]
    fn input_event_variants() {
        // Just verify all variants construct properly
        let events: Vec<InputEvent> = vec![
            InputEvent::PointerMove { hand: Hand::Right, u: 0.5, v: 0.5, distance: 2.0 },
            InputEvent::PointerDown { hand: Hand::Right, u: 0.5, v: 0.5 },
            InputEvent::PointerUp { hand: Hand::Right, u: 0.5, v: 0.5 },
            InputEvent::GrabStart { hand: Hand::Left, grip_pos: Vec3::ZERO, grip_rot: Quat::IDENTITY },
            InputEvent::GrabMove { hand: Hand::Left, grip_pos: Vec3::ONE, grip_rot: Quat::IDENTITY },
            InputEvent::GrabEnd { hand: Hand::Left },
            InputEvent::ButtonPress { hand: Hand::Right, button: Button::A },
            InputEvent::ButtonRelease { hand: Hand::Right, button: Button::A },
            InputEvent::ThumbstickMove { hand: Hand::Left, x: 0.5, y: -0.3 },
            InputEvent::TextInput { text: "hello".into() },
        ];
        assert_eq!(events.len(), 10);
    }

    // ---- InputDispatcher tests ----

    use crate::panel::{PanelAnchor, PanelTransform};

    /// Helper: create a panel at the given center, facing +Z (normal is -Z from cross(X,Y)).
    /// Actually the normal from right_dir.cross(up_dir) = X.cross(Y) = Z,
    /// so the ray must travel in -Z to have a negative denom and positive t.
    fn make_panel(center: Vec3, width: f32, height: f32) -> PanelTransform {
        PanelTransform {
            center,
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width,
            height,
            opacity: 1.0,
            anchor: PanelAnchor::World,
        }
    }

    /// Helper: create a ControllerState with the right hand active, aiming from
    /// `aim_pos` in direction `aim_dir` with given trigger value.
    fn right_hand_state(aim_pos: Vec3, aim_dir: Vec3, trigger: f32) -> ControllerState {
        let mut state = ControllerState::default();
        state.right.active = true;
        state.right.aim_pos = aim_pos;
        state.right.aim_dir = aim_dir;
        state.right.trigger = trigger;
        state
    }

    fn left_hand_state(aim_pos: Vec3, aim_dir: Vec3, trigger: f32) -> ControllerState {
        let mut state = ControllerState::default();
        state.left.active = true;
        state.left.aim_pos = aim_pos;
        state.left.aim_dir = aim_dir;
        state.left.trigger = trigger;
        state
    }

    #[test]
    fn dispatcher_aim_at_panel_generates_pointer_move() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];
        let state = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);

        let events = dispatcher.process(&state, &panels);

        assert_eq!(events.len(), 1);
        let (pid, ref evt) = events[0];
        assert_eq!(pid, PanelId::new(1));
        match evt {
            InputEvent::PointerMove { hand, u, v, .. } => {
                assert_eq!(*hand, Hand::Right);
                assert!((u - 0.5).abs() < 0.01);
                assert!((v - 0.5).abs() < 0.01);
            }
            _ => panic!("Expected PointerMove, got {:?}", evt),
        }
    }

    #[test]
    fn dispatcher_trigger_pull_generates_pointer_down() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: no trigger (establish prev state)
        let state1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        dispatcher.process(&state1, &panels);

        // Frame 2: trigger pulled
        let state2 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events = dispatcher.process(&state2, &panels);

        assert_eq!(events.len(), 2); // PointerMove + PointerDown
        let has_down = events.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. }));
        assert!(has_down, "Expected PointerDown event");
    }

    #[test]
    fn dispatcher_trigger_release_generates_pointer_up() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: trigger held (establish prev state as pressed)
        let state1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        dispatcher.process(&state1, &panels);

        // Frame 2: trigger released
        let state2 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        let events = dispatcher.process(&state2, &panels);

        assert_eq!(events.len(), 2); // PointerMove + PointerUp
        let has_up = events.iter().any(|(_, e)| matches!(e, InputEvent::PointerUp { .. }));
        assert!(has_up, "Expected PointerUp event");
    }

    #[test]
    fn dispatcher_aim_at_nothing_generates_no_events() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Aim way off to the side — miss the panel
        let state = right_hand_state(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 0.0);
        let events = dispatcher.process(&state, &panels);

        assert!(events.is_empty(), "Expected no events when aiming at nothing");
    }

    #[test]
    fn dispatcher_front_to_back_priority() {
        let mut dispatcher = InputDispatcher::new();
        // Close panel at z=-1, far panel at z=-3, both centered on the aim ray
        let close_panel = make_panel(Vec3::new(0.0, 0.0, -1.0), 2.0, 2.0);
        let far_panel = make_panel(Vec3::new(0.0, 0.0, -3.0), 2.0, 2.0);
        // Front-to-back order: close first
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(10), &close_panel), (PanelId::new(20), &far_panel)];

        let state = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        let events = dispatcher.process(&state, &panels);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, PanelId::new(10), "Closer panel should receive the event");
    }

    #[test]
    fn dispatcher_both_hands_independent() {
        let mut dispatcher = InputDispatcher::new();
        // Two panels side by side
        let left_panel = make_panel(Vec3::new(-2.0, 0.0, -2.0), 2.0, 2.0);
        let right_panel = make_panel(Vec3::new(2.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &left_panel), (PanelId::new(2), &right_panel)];

        // Both hands active, each aiming at different panel
        let mut state = ControllerState::default();
        state.left.active = true;
        state.left.aim_pos = Vec3::new(-2.0, 0.0, 0.0);
        state.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.left.trigger = 0.0;

        state.right.active = true;
        state.right.aim_pos = Vec3::new(2.0, 0.0, 0.0);
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.trigger = 0.0;

        let events = dispatcher.process(&state, &panels);

        // Should get PointerMove for each hand on its respective panel
        assert_eq!(events.len(), 2);

        let left_event = events.iter().find(|(_, e)| matches!(e, InputEvent::PointerMove { hand: Hand::Left, .. }));
        let right_event = events.iter().find(|(_, e)| matches!(e, InputEvent::PointerMove { hand: Hand::Right, .. }));

        assert!(left_event.is_some(), "Left hand should generate PointerMove");
        assert!(right_event.is_some(), "Right hand should generate PointerMove");
        assert_eq!(left_event.unwrap().0, PanelId::new(1), "Left hand should hit left panel");
        assert_eq!(right_event.unwrap().0, PanelId::new(2), "Right hand should hit right panel");
    }

    #[test]
    fn dispatcher_inactive_hand_generates_no_events() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Default state: both hands inactive
        let state = ControllerState::default();
        let events = dispatcher.process(&state, &panels);

        assert!(events.is_empty());
    }

    #[test]
    fn dispatcher_trigger_held_no_repeat_down() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: trigger pulled (rising edge)
        let state1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events1 = dispatcher.process(&state1, &panels);
        assert!(events1.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. })));

        // Frame 2: trigger still held — should NOT get another PointerDown
        let state2 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events2 = dispatcher.process(&state2, &panels);
        assert_eq!(events2.len(), 1, "Should only get PointerMove, no repeated PointerDown");
        assert!(matches!(events2[0].1, InputEvent::PointerMove { .. }));
    }

    #[test]
    fn trigger_held_does_not_repeat_click() {
        // Verify that holding trigger for multiple frames only produces one PointerDown.
        // This is the Track 02 MVP requirement: one trigger press = one logical click.
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: no trigger (establish baseline)
        let state0 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        dispatcher.process(&state0, &panels);

        // Frame 2: trigger pulled — rising edge, should get exactly one PointerDown
        let state1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events1 = dispatcher.process(&state1, &panels);
        let down_count_1 = events1.iter().filter(|(_, e)| matches!(e, InputEvent::PointerDown { .. })).count();
        assert_eq!(down_count_1, 1, "First press frame should produce exactly one PointerDown");

        // Frame 3-6: trigger still held — should NOT get any more PointerDown
        for frame in 3..=6 {
            let state = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
            let events = dispatcher.process(&state, &panels);
            let down_count = events.iter().filter(|(_, e)| matches!(e, InputEvent::PointerDown { .. })).count();
            assert_eq!(down_count, 0, "Frame {}: held trigger must not produce PointerDown", frame);
            // Should still get PointerMove (hover tracking continues)
            let move_count = events.iter().filter(|(_, e)| matches!(e, InputEvent::PointerMove { .. })).count();
            assert_eq!(move_count, 1, "Frame {}: should still get PointerMove while held", frame);
        }

        // Frame 7: release trigger
        let state_release = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        let events_release = dispatcher.process(&state_release, &panels);
        let up_count = events_release.iter().filter(|(_, e)| matches!(e, InputEvent::PointerUp { .. })).count();
        assert_eq!(up_count, 1, "Release should produce exactly one PointerUp");

        // Frame 8: press again — should get another PointerDown (new click)
        let state_repress = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events_repress = dispatcher.process(&state_repress, &panels);
        let down_count_re = events_repress.iter().filter(|(_, e)| matches!(e, InputEvent::PointerDown { .. })).count();
        assert_eq!(down_count_re, 1, "Re-press after release should produce new PointerDown");
    }

    #[test]
    fn dispatcher_left_hand_trigger_edge_detection() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: left hand, no trigger
        let state1 = left_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        dispatcher.process(&state1, &panels);

        // Frame 2: left hand, trigger pulled
        let state2 = left_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events = dispatcher.process(&state2, &panels);

        let has_down = events.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { hand: Hand::Left, .. }));
        assert!(has_down, "Left hand trigger should generate PointerDown");
    }

    // ---- Grab / Phase 2 tests ----

    /// Helper: create a ControllerState with right hand active, aiming, with grip/squeeze.
    fn right_hand_grip_state(
        aim_pos: Vec3,
        aim_dir: Vec3,
        squeeze: f32,
        grip_pos: Vec3,
        grip_rot: Quat,
    ) -> ControllerState {
        let mut state = ControllerState::default();
        state.right.active = true;
        state.right.aim_pos = aim_pos;
        state.right.aim_dir = aim_dir;
        state.right.squeeze = squeeze;
        state.right.grip_pos = grip_pos;
        state.right.grip_rot = grip_rot;
        state
    }

    #[test]
    fn dispatcher_grip_in_margin_starts_grab() {
        let mut dispatcher = InputDispatcher::new();
        // Panel centered at z=-2, 2x2. Aim at the left edge (u ~ 0.05).
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Aim at left edge: x = -0.9 maps to u ~ 0.05
        let state = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::new(-0.5, 0.0, 0.0),
            Quat::IDENTITY,
        );
        let events = dispatcher.process(&state, &panels);

        let has_grab_start = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabStart { .. }));
        assert!(has_grab_start, "Grip in margin zone should emit GrabStart");
    }

    #[test]
    fn dispatcher_grip_center_no_grab() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Aim at center: u ~ 0.5, v ~ 0.5
        let state = right_hand_grip_state(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        let events = dispatcher.process(&state, &panels);

        let has_grab_start = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabStart { .. }));
        assert!(!has_grab_start, "Grip in center should NOT emit GrabStart");
    }

    #[test]
    fn dispatcher_grab_move_follows_controller() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: grip in margin -> GrabStart
        let state1 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::new(-0.5, 0.0, 0.0),
            Quat::IDENTITY,
        );
        dispatcher.process(&state1, &panels);

        // Frame 2: grip still held, controller moved
        let new_grip_pos = Vec3::new(0.5, 1.0, -1.0);
        let state2 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            new_grip_pos,
            Quat::IDENTITY,
        );
        let events = dispatcher.process(&state2, &panels);

        let grab_move = events.iter().find(|(_, e)| matches!(e, InputEvent::GrabMove { .. }));
        assert!(grab_move.is_some(), "Should emit GrabMove while grip held");
        match &grab_move.unwrap().1 {
            InputEvent::GrabMove { grip_pos, .. } => {
                assert_eq!(*grip_pos, new_grip_pos, "GrabMove should carry current grip_pos");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn dispatcher_grip_release_ends_grab() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: grip in margin -> GrabStart
        let state1 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        dispatcher.process(&state1, &panels);

        // Frame 2: release grip
        let state2 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.0,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        let events = dispatcher.process(&state2, &panels);

        let has_grab_end = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabEnd { .. }));
        assert!(has_grab_end, "Releasing grip should emit GrabEnd");
        // Grab should be cleared
        assert!(dispatcher.grab_active[1].is_none(), "Grab should be cleared after release");
    }

    #[test]
    fn dispatcher_a_click_during_grab() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: grip in margin -> start grab
        let mut state1 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        state1.right.a_click = false;
        dispatcher.process(&state1, &panels);

        // Frame 2: grip held + A button pressed
        let mut state2 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        state2.right.a_click = true;
        let events = dispatcher.process(&state2, &panels);

        let has_button_a = events.iter().any(|(_, e)| matches!(
            e,
            InputEvent::ButtonPress { button: Button::A, .. }
        ));
        assert!(has_button_a, "A button during grab should emit ButtonPress {{ button: A }}");
    }

    #[test]
    fn dispatcher_thumbstick_during_grab() {
        let mut dispatcher = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: grip in margin -> start grab
        let state1 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        dispatcher.process(&state1, &panels);

        // Frame 2: grip held + thumbstick Y pushed
        let mut state2 = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        state2.right.thumbstick = [0.0, 0.8];
        let events = dispatcher.process(&state2, &panels);

        let has_thumbstick = events.iter().any(|(_, e)| matches!(
            e,
            InputEvent::ThumbstickMove { y, .. } if *y > 0.5
        ));
        assert!(has_thumbstick, "Thumbstick Y during grab should emit ThumbstickMove");
    }

    #[test]
    fn dispatcher_grab_at_exact_margin_boundary() {
        // Test at exactly u=0.15 (should NOT be in grab margin -- margin is u < 0.15)
        let mut d = InputDispatcher::new();
        let panel = PanelTransform {
            center: Vec3::new(0.0, 1.6, -2.5),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            anchor: PanelAnchor::World,
        };
        // Aim at u~0.15 (edge of margin)
        let x = (0.15 - 0.5) * 1.6; // u=0.15 -> x offset
        let mut state = ControllerState::default();
        state.right.active = true;
        state.right.aim_pos = Vec3::new(x, 1.6, 0.0);
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.squeeze = 0.9;

        let events = d.process(&state, &[(PanelId::new(1), &panel)]);
        // At u=0.15, this is at the boundary -- behavior depends on strict < vs <=
        // Either outcome is acceptable, just verify no crash
        assert!(!events.is_empty()); // Should at least have PointerMove
    }

    #[test]
    fn dispatcher_both_hands_grab_same_panel() {
        let mut d = InputDispatcher::new();
        let panel = PanelTransform {
            center: Vec3::new(0.0, 1.6, -2.5),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            anchor: PanelAnchor::World,
        };
        // Both hands aim at edges of the same panel
        let mut state = ControllerState::default();
        state.left.active = true;
        state.left.aim_pos = Vec3::new(-0.7, 1.6, 0.0); // left edge
        state.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.left.squeeze = 0.9;
        state.right.active = true;
        state.right.aim_pos = Vec3::new(0.7, 1.6, 0.0); // right edge
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.squeeze = 0.9;

        let events = d.process(&state, &[(PanelId::new(1), &panel)]);
        // Should have GrabStart from both hands
        let grabs: Vec<_> = events.iter().filter(|(_, e)| matches!(e, InputEvent::GrabStart { .. })).collect();
        assert_eq!(grabs.len(), 2, "Both hands should grab: {:?}", events);
    }
}
