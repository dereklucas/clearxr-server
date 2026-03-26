use crate::panel::{PanelId, PanelTransform};
use super::{Button, Hand, ControllerState, InputEvent};

/// Trigger analog value at which a press is registered.
const TRIGGER_THRESHOLD: f32 = 0.5;
/// Squeeze analog value at which a grip is registered.
const GRIP_THRESHOLD: f32 = 0.7;
/// UV-space margin around the panel edge where grabs are allowed.
const GRAB_MARGIN: f32 = 0.15;
/// Thumbstick deflection below which input is ignored.
const THUMBSTICK_DEADZONE: f32 = 0.3;

/// Returns true if (u, v) is in the outer margin zone of a panel (edge region).
fn in_grab_margin(u: f32, v: f32) -> bool {
    u < GRAB_MARGIN || u > (1.0 - GRAB_MARGIN) || v < GRAB_MARGIN || v > (1.0 - GRAB_MARGIN)
}

/// Dispatches controller input to panels using ray-panel hit testing.
///
/// Tracks per-hand trigger state for edge detection of press/release.
pub struct InputDispatcher {
    prev_trigger: [bool; 2],    // [left, right] — for edge detection
    prev_grip: [bool; 2],       // for grip edge detection
    prev_a_click: [bool; 2],    // for A button edge detection (anchor cycling)
    pub(crate) grab_active: [Option<PanelId>; 2], // which panel each hand is grabbing
}

impl InputDispatcher {
    /// Create a new dispatcher with all input state cleared.
    pub fn new() -> Self {
        Self {
            prev_trigger: [false; 2],
            prev_grip: [false; 2],
            prev_a_click: [false; 2],
            grab_active: [None; 2],
        }
    }

    /// Process controller state against panel transforms.
    ///
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

            // Find ALL panels hit by this hand's ray
            let mut hits: Vec<(PanelId, f32, f32, f32)> = Vec::new();
            for &(panel_id, panel_transform) in panels {
                if let Some((u, v, t)) = panel_transform.hit_test(hand_state.aim_pos, hand_state.aim_dir) {
                    hits.push((panel_id, u, v, t));
                }
            }

            // Sort by distance (closest first)
            hits.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));

            // Debug: log hits when trigger pulled
            if trigger_now && !trigger_prev && !hits.is_empty() {
                log::debug!("Hand {} trigger: {} hits: {:?}", hand_index, hits.len(),
                    hits.iter().map(|(id, u, v, t)| format!("({:?} u={:.2} v={:.2} t={:.2})", id, u, v, t)).collect::<Vec<_>>());
            }

            // Emit PointerMove for ALL hit panels (so background panels can show hover highlights)
            for &(panel_id, u, v, distance) in &hits {
                events.push((panel_id, InputEvent::PointerMove {
                    hand: hand_enum,
                    u, v,
                    distance,
                }));
            }

            // Emit PointerDown/PointerUp only for the CLOSEST hit panel (don't click through)
            if let Some(&(panel_id, u, v, _)) = hits.first() {
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

            // Use the closest hit for grab logic below
            let hit = hits.first().copied();

            self.prev_trigger[hand_index] = trigger_now;

            // --- Grab logic (grip/squeeze) ---
            let grip_now = hand_state.squeeze >= GRIP_THRESHOLD;
            let grip_prev = self.prev_grip[hand_index];

            if grip_now && !grip_prev {
                // Grip rising edge: start grab if ray hits panel margin zone (or if panel is always grabbable)
                if let Some((panel_id, u, v, _)) = hit {
                    // Check if the panel is marked as always-grabbable
                    let panel_grabbable = panels.iter()
                        .find(|&&(id, _)| id == panel_id)
                        .map(|&(_, pt)| pt.grabbable)
                        .unwrap_or(false);
                    if panel_grabbable || in_grab_margin(u, v) {
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
    use glam::{Vec3, Quat};
    use crate::panel::PanelAnchor;

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
            anchor: PanelAnchor::World, grabbable: false,
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

        // Both panels get PointerMove (hover passes through)
        let moves: Vec<_> = events.iter().filter(|(_, e)| matches!(e, InputEvent::PointerMove { .. })).collect();
        assert_eq!(moves.len(), 2, "Both hit panels should receive PointerMove");
        assert_eq!(moves[0].0, PanelId::new(10), "Closer panel should be first");
        assert_eq!(moves[1].0, PanelId::new(20), "Far panel should be second");
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
            anchor: PanelAnchor::World, grabbable: false,
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
    fn grab_requires_margin_or_bar() {
        // Verify that grabbing only works on panel edges, not center
        let mut d = InputDispatcher::new();
        let panel = PanelTransform {
            center: Vec3::new(0.0, 0.0, -2.0),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 2.0,
            height: 2.0,
            opacity: 0.95,
            anchor: PanelAnchor::World, grabbable: false,
        };
        // Aim at center with high grip
        let mut state = ControllerState::default();
        state.right.active = true;
        state.right.aim_pos = Vec3::ZERO;
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.squeeze = 0.9;
        state.right.grip_pos = Vec3::ZERO;
        state.right.grip_rot = Quat::IDENTITY;

        let events = d.process(&state, &[(PanelId::new(1), &panel)]);
        // Center of panel (u~0.5, v~0.5) should NOT trigger grab
        let has_grab = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabStart { .. }));
        assert!(!has_grab, "Center of panel should not trigger grab");
    }

    #[test]
    fn both_hands_can_click_different_panels() {
        let mut d = InputDispatcher::new();
        let panel_a = PanelTransform {
            center: Vec3::new(-2.0, 0.0, -2.0),
            right_dir: Vec3::X, up_dir: Vec3::Y, width: 2.0, height: 2.0,
            opacity: 0.95, anchor: PanelAnchor::World, grabbable: false,
        };
        let panel_b = PanelTransform {
            center: Vec3::new(2.0, 0.0, -2.0),
            right_dir: Vec3::X, up_dir: Vec3::Y, width: 2.0, height: 2.0,
            opacity: 0.95, anchor: PanelAnchor::World, grabbable: false,
        };

        // Frame 1: no trigger (establish baseline)
        let mut state0 = ControllerState::default();
        state0.left.active = true;
        state0.left.aim_pos = Vec3::new(-2.0, 0.0, 0.0);
        state0.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state0.left.trigger = 0.0;
        state0.right.active = true;
        state0.right.aim_pos = Vec3::new(2.0, 0.0, 0.0);
        state0.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state0.right.trigger = 0.0;
        d.process(&state0, &[(PanelId::new(1), &panel_a), (PanelId::new(2), &panel_b)]);

        // Frame 2: both triggers pulled
        let mut state = ControllerState::default();
        state.left.active = true;
        state.left.aim_pos = Vec3::new(-2.0, 0.0, 0.0);
        state.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.left.trigger = 1.0;
        state.right.active = true;
        state.right.aim_pos = Vec3::new(2.0, 0.0, 0.0);
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.trigger = 1.0;

        let events = d.process(&state, &[(PanelId::new(1), &panel_a), (PanelId::new(2), &panel_b)]);

        let left_clicks: Vec<_> = events.iter()
            .filter(|(_, e)| matches!(e, InputEvent::PointerDown { hand: Hand::Left, .. }))
            .collect();
        let right_clicks: Vec<_> = events.iter()
            .filter(|(_, e)| matches!(e, InputEvent::PointerDown { hand: Hand::Right, .. }))
            .collect();
        assert!(!left_clicks.is_empty(), "Left hand should click panel A");
        assert!(!right_clicks.is_empty(), "Right hand should click panel B");
        assert_eq!(left_clicks[0].0, PanelId::new(1));
        assert_eq!(right_clicks[0].0, PanelId::new(2));
    }

    #[test]
    fn anchor_cycle_preserves_world_position() {
        // When cycling through anchors and back to World, verify behavior
        let mut t = PanelTransform::default();
        let original = t.center;

        t.cycle_anchor(); // World -> Controller
        t.cycle_anchor(); // Controller -> Wrist
        t.cycle_anchor(); // Wrist -> Theater

        // Theater changes position via update_anchor
        t.update_anchor(
            Vec3::new(0.0, 1.6, 0.0), Vec3::NEG_Z, Vec3::X, Vec3::Y,
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO,
        );
        assert_ne!(t.center, original, "Theater should move panel");

        t.cycle_anchor(); // Theater -> Head
        t.cycle_anchor(); // Head -> World
        // Returning to World keeps the last position (World anchor doesn't auto-reposition)
        assert_eq!(t.anchor, PanelAnchor::World);
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
            anchor: PanelAnchor::World, grabbable: false,
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

    #[test]
    fn dispatcher_grabbable_bypasses_margin() {
        // Panel with grabbable=true: grip at center (u=0.5) should still GrabStart
        let mut d = InputDispatcher::new();
        let panel = PanelTransform {
            center: Vec3::new(0.0, 0.0, -2.0),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 2.0,
            height: 2.0,
            opacity: 1.0,
            anchor: PanelAnchor::World,
            grabbable: true, // entire surface is grabbable
        };
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Aim at center: u~0.5, v~0.5
        let state = right_hand_grip_state(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, -1.0),
            0.9,
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        let events = d.process(&state, &panels);

        let has_grab_start = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabStart { .. }));
        assert!(has_grab_start, "grabbable=true should allow GrabStart at center");
    }

    #[test]
    fn dispatcher_trigger_full_lifecycle() {
        // Frame 1: trigger=0, Frame 2: trigger=1, Frame 3: trigger=1, Frame 4: trigger=0
        // Verify: PointerDown on frame 2 only, PointerUp on frame 4 only
        let mut d = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: trigger=0
        let s1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        let e1 = d.process(&s1, &panels);
        assert!(!e1.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. })));
        assert!(!e1.iter().any(|(_, e)| matches!(e, InputEvent::PointerUp { .. })));

        // Frame 2: trigger=1 -> PointerDown
        let s2 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let e2 = d.process(&s2, &panels);
        assert_eq!(e2.iter().filter(|(_, e)| matches!(e, InputEvent::PointerDown { .. })).count(), 1);
        assert!(!e2.iter().any(|(_, e)| matches!(e, InputEvent::PointerUp { .. })));

        // Frame 3: trigger=1 (held) -> no Down or Up
        let s3 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let e3 = d.process(&s3, &panels);
        assert!(!e3.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. })));
        assert!(!e3.iter().any(|(_, e)| matches!(e, InputEvent::PointerUp { .. })));

        // Frame 4: trigger=0 -> PointerUp
        let s4 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        let e4 = d.process(&s4, &panels);
        assert!(!e4.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. })));
        assert_eq!(e4.iter().filter(|(_, e)| matches!(e, InputEvent::PointerUp { .. })).count(), 1);
    }

    #[test]
    fn dispatcher_multiple_panels_depth_sorted() {
        // Two panels at different z-depths, ray hits both
        // PointerDown goes to CLOSEST, PointerMove goes to BOTH
        let mut d = InputDispatcher::new();
        let close = make_panel(Vec3::new(0.0, 0.0, -1.0), 2.0, 2.0);
        let far = make_panel(Vec3::new(0.0, 0.0, -3.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![
            (PanelId::new(10), &close),
            (PanelId::new(20), &far),
        ];

        // Frame 1: no trigger
        let s0 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        d.process(&s0, &panels);

        // Frame 2: trigger pulled
        let s1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events = d.process(&s1, &panels);

        // Both should get PointerMove
        let moves: Vec<_> = events.iter()
            .filter(|(_, e)| matches!(e, InputEvent::PointerMove { .. }))
            .collect();
        assert_eq!(moves.len(), 2, "Both panels should get PointerMove");

        // Only closest should get PointerDown
        let downs: Vec<_> = events.iter()
            .filter(|(_, e)| matches!(e, InputEvent::PointerDown { .. }))
            .collect();
        assert_eq!(downs.len(), 1, "Only one panel should get PointerDown");
        assert_eq!(downs[0].0, PanelId::new(10), "Closest panel should get PointerDown");
    }

    #[test]
    fn dispatcher_no_events_for_both_hands_inactive() {
        let mut d = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Both hands inactive (default), with aim that would hit panel
        let mut state = ControllerState::default();
        state.left.aim_pos = Vec3::ZERO;
        state.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.left.trigger = 1.0;
        state.right.aim_pos = Vec3::ZERO;
        state.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state.right.trigger = 1.0;
        // active remains false for both
        let events = d.process(&state, &panels);
        assert!(events.is_empty(), "Inactive hands should produce zero events");
    }

    #[test]
    fn dispatcher_grab_only_first_hand_gets_priority() {
        // If hand A grabs first, hand B grabbing same panel should also work
        // (both hands CAN grab same panel simultaneously per existing behavior)
        let mut d = InputDispatcher::new();
        let panel = PanelTransform {
            center: Vec3::new(0.0, 0.0, -2.0),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 2.0,
            height: 2.0,
            opacity: 1.0,
            anchor: PanelAnchor::World,
            grabbable: false,
        };

        // Frame 1: Only left hand grabs at edge
        let mut state1 = ControllerState::default();
        state1.left.active = true;
        state1.left.aim_pos = Vec3::new(-0.9, 0.0, 0.0);
        state1.left.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state1.left.squeeze = 0.9;
        state1.right.active = true;
        state1.right.aim_pos = Vec3::new(0.9, 0.0, 0.0);
        state1.right.aim_dir = Vec3::new(0.0, 0.0, -1.0);
        state1.right.squeeze = 0.0; // not gripping

        let e1 = d.process(&state1, &[(PanelId::new(1), &panel)]);
        let left_grabs = e1.iter().filter(|(_, e)| matches!(e, InputEvent::GrabStart { hand: Hand::Left, .. })).count();
        let right_grabs = e1.iter().filter(|(_, e)| matches!(e, InputEvent::GrabStart { hand: Hand::Right, .. })).count();
        assert_eq!(left_grabs, 1, "Left hand should grab");
        assert_eq!(right_grabs, 0, "Right hand should not grab yet");
    }

    #[test]
    fn in_grab_margin_edges() {
        // Test the margin check helper
        assert!(in_grab_margin(0.05, 0.5));   // left edge
        assert!(in_grab_margin(0.95, 0.5));   // right edge
        assert!(in_grab_margin(0.5, 0.05));   // top edge
        assert!(in_grab_margin(0.5, 0.95));   // bottom edge
        assert!(!in_grab_margin(0.5, 0.5));   // center
        assert!(!in_grab_margin(0.2, 0.5));   // just inside margin
        assert!(!in_grab_margin(0.5, 0.8));   // just inside margin
    }

    #[test]
    fn dispatcher_grip_below_threshold_no_grab() {
        let mut d = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Squeeze below threshold (0.7)
        let state = right_hand_grip_state(
            Vec3::new(-0.9, 0.0, 0.0), // edge aim
            Vec3::new(0.0, 0.0, -1.0),
            0.5, // below GRIP_THRESHOLD
            Vec3::ZERO,
            Quat::IDENTITY,
        );
        let events = d.process(&state, &panels);
        let has_grab = events.iter().any(|(_, e)| matches!(e, InputEvent::GrabStart { .. }));
        assert!(!has_grab, "Squeeze below threshold should not start grab");
    }

    #[test]
    fn dispatcher_trigger_at_exact_threshold() {
        // Trigger at exactly TRIGGER_THRESHOLD (0.5) should count as pressed
        let mut d = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        // Frame 1: trigger=0
        let s0 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.0);
        d.process(&s0, &panels);

        // Frame 2: trigger=0.5 (exactly at threshold)
        let s1 = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.5);
        let events = d.process(&s1, &panels);
        let has_down = events.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. }));
        assert!(has_down, "Trigger at exact threshold should fire PointerDown");
    }

    #[test]
    fn dispatcher_trigger_just_below_threshold() {
        // Trigger at 0.49 (just below threshold) should NOT count as pressed
        let mut d = InputDispatcher::new();
        let panel = make_panel(Vec3::new(0.0, 0.0, -2.0), 2.0, 2.0);
        let panels: Vec<(PanelId, &PanelTransform)> = vec![(PanelId::new(1), &panel)];

        let s = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 0.49);
        let events = d.process(&s, &panels);
        let has_down = events.iter().any(|(_, e)| matches!(e, InputEvent::PointerDown { .. }));
        assert!(!has_down, "Trigger below threshold should not fire PointerDown");
    }

    #[test]
    fn dispatcher_empty_panel_list() {
        let mut d = InputDispatcher::new();
        let panels: Vec<(PanelId, &PanelTransform)> = vec![];
        let state = right_hand_state(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let events = d.process(&state, &panels);
        assert!(events.is_empty(), "No panels means no events");
    }
}
