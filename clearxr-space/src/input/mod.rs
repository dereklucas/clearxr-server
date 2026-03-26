//! Input types and dispatcher for VR controller interaction.

pub mod dispatcher;
pub use dispatcher::InputDispatcher;

use glam::{Vec3, Quat};

// Re-export Hand from panel module (single source of truth)
pub use crate::panel::Hand;

/// Controller button identifier.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // All variants are part of the input API; not all wired up yet
pub enum Button {
    /// Primary trigger (index finger).
    Trigger,
    /// Side grip / squeeze.
    Grip,
    /// A face button (right controller).
    A,
    /// B face button (right controller).
    B,
    /// X face button (left controller).
    X,
    /// Y face button (left controller).
    Y,
    /// Menu / system button.
    Menu,
    /// Thumbstick press.
    ThumbstickClick,
}

/// Input event produced by the InputDispatcher each frame.
#[derive(Clone, Debug)]
#[allow(dead_code)] // All variants/fields are part of the input API; not all consumed yet
pub enum InputEvent {
    /// Pointer ray is hovering over a panel at (u, v).
    PointerMove { hand: Hand, u: f32, v: f32, distance: f32 },
    /// Trigger pulled while pointing at panel.
    PointerDown { hand: Hand, u: f32, v: f32 },
    /// Trigger released.
    PointerUp { hand: Hand, u: f32, v: f32 },
    /// Grip pressed near panel edge -- start moving.
    GrabStart { hand: Hand, grip_pos: Vec3, grip_rot: Quat },
    /// Panel being moved by grip.
    GrabMove { hand: Hand, grip_pos: Vec3, grip_rot: Quat },
    /// Grip released.
    GrabEnd { hand: Hand },
    /// Button pressed (not on panel surface).
    ButtonPress { hand: Hand, button: Button },
    /// Button released.
    ButtonRelease { hand: Hand, button: Button },
    /// Thumbstick moved (for scrolling).
    ThumbstickMove { hand: Hand, x: f32, y: f32 },
    /// Text input from virtual keyboard.
    TextInput { text: String },
}

/// Per-hand controller state extracted from OpenXR each frame.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // All fields are part of the input API; not all consumed yet
pub struct HandState {
    /// Whether this hand's tracking is active.
    pub active: bool,
    /// Grip (palm) position in world space.
    pub grip_pos: Vec3,
    /// Grip (palm) orientation in world space.
    pub grip_rot: Quat,
    /// Aim ray origin in world space.
    pub aim_pos: Vec3,
    /// Aim ray direction (unit vector) in world space.
    pub aim_dir: Vec3,
    /// Trigger analog value (0.0 = released, 1.0 = fully pressed).
    pub trigger: f32,
    /// Squeeze / grip analog value (0.0 = released, 1.0 = fully pressed).
    pub squeeze: f32,
    /// Thumbstick deflection [x, y], each in -1.0..1.0.
    pub thumbstick: [f32; 2],
    /// A button pressed (right controller).
    pub a_click: bool,
    /// B button pressed (right controller).
    pub b_click: bool,
    /// X button pressed (left controller).
    pub x_click: bool,
    /// Y button pressed (left controller).
    pub y_click: bool,
    /// Menu / system button pressed.
    pub menu_click: bool,
    /// Thumbstick click pressed.
    pub thumbstick_click: bool,
    // Touch states
    /// A button touched.
    pub a_touch: bool,
    /// B button touched.
    pub b_touch: bool,
    /// X button touched.
    pub x_touch: bool,
    /// Y button touched.
    pub y_touch: bool,
    /// Trigger touched.
    pub trigger_touch: bool,
    /// Thumbstick touched.
    pub thumbstick_touch: bool,
}

/// Full controller state for both hands.
#[derive(Clone, Debug, Default)]
pub struct ControllerState {
    /// Left hand controller state.
    pub left: HandState,
    /// Right hand controller state.
    pub right: HandState,
}

impl ControllerState {
    /// Get the hand state for the given hand.
    pub fn hand(&self, hand: Hand) -> &HandState {
        match hand {
            Hand::Left => &self.left,
            Hand::Right => &self.right,
        }
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
}
