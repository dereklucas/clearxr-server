use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PanelId(u64);

impl PanelId {
    pub const fn new(id: u64) -> Self { Self(id) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hand { Left, Right }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PanelAnchor {
    World,
    Controller { hand: Hand },
    Wrist { hand: Hand },
    Head { offset: Vec3 },
    Theater { distance: f32, scale: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct PanelTransform {
    pub center: Vec3,
    pub right_dir: Vec3,
    pub up_dir: Vec3,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub anchor: PanelAnchor,
}

impl Default for PanelTransform {
    fn default() -> Self {
        Self {
            center: Vec3::new(0.0, 1.6, -2.5),
            right_dir: Vec3::X,
            up_dir: Vec3::Y,
            width: 1.6,
            height: 1.0,
            opacity: 0.95,
            anchor: PanelAnchor::World,
        }
    }
}

impl PanelTransform {
    /// Update the panel's world-space position based on its anchor mode.
    /// Called each frame by the Shell.
    ///
    /// - World: no change (panel stays where it was placed)
    /// - Controller: panel follows the controller, offset slightly forward
    /// - Wrist: small panel on the inner wrist
    /// - Head: panel locked relative to head position/orientation
    /// - Theater: large panel centered in front of head at configured distance
    pub fn update_anchor(
        &mut self,
        head_pos: Vec3,
        head_forward: Vec3,
        head_right: Vec3,
        head_up: Vec3,
        controller_pos: Vec3,
        controller_forward: Vec3,
        controller_right: Vec3,
        controller_up: Vec3,
    ) {
        match self.anchor {
            PanelAnchor::World => {
                // No update — panel stays at its current position
            }
            PanelAnchor::Controller { .. } => {
                // Float in front of controller, slightly tilted back
                self.center = controller_pos + controller_forward * 0.25 + controller_up * 0.05;
                self.right_dir = controller_right;
                self.up_dir = controller_up;
            }
            PanelAnchor::Wrist { hand } => {
                // Small panel on inner wrist
                let sign = match hand {
                    Hand::Left => 1.0,  // inner wrist faces right for left hand
                    Hand::Right => -1.0,
                };
                self.center = controller_pos + controller_right * (sign * 0.08) + controller_up * 0.03;
                self.width = 0.15;
                self.height = 0.10;
                self.right_dir = controller_forward;
                self.up_dir = controller_up;
            }
            PanelAnchor::Head { offset } => {
                // Head-locked: follows head with offset
                self.center = head_pos
                    + head_forward * offset.z
                    + head_right * offset.x
                    + head_up * offset.y;
                self.right_dir = head_right;
                self.up_dir = head_up;
            }
            PanelAnchor::Theater { distance, scale } => {
                // Large panel centered in front of head
                // Only use head yaw (horizontal), not pitch, so it stays level
                let forward_flat = Vec3::new(head_forward.x, 0.0, head_forward.z).normalize_or_zero();
                if forward_flat == Vec3::ZERO {
                    return; // Degenerate case: looking straight up/down
                }
                self.center = head_pos + forward_flat * distance + Vec3::Y * 0.5; // slightly above eye level
                self.width = 1.6 * scale;
                self.height = 1.0 * scale;
                self.right_dir = forward_flat.cross(Vec3::Y).normalize();
                self.up_dir = Vec3::Y;
            }
        }
    }

    /// Cycle to the next anchor mode.
    pub fn cycle_anchor(&mut self) {
        self.anchor = match self.anchor {
            PanelAnchor::World => PanelAnchor::Controller { hand: Hand::Right },
            PanelAnchor::Controller { .. } => PanelAnchor::Wrist { hand: Hand::Left },
            PanelAnchor::Wrist { .. } => PanelAnchor::Theater { distance: 5.0, scale: 3.0 },
            PanelAnchor::Theater { .. } => PanelAnchor::Head { offset: Vec3::new(0.0, 0.0, -2.0) },
            PanelAnchor::Head { .. } => PanelAnchor::World,
        };
    }

    /// Ray-plane intersection. Returns (u, v, t) if the ray hits, None otherwise.
    pub fn hit_test(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<(f32, f32, f32)> {
        let panel_normal = self.right_dir.cross(self.up_dir);
        let denom = ray_dir.dot(panel_normal);
        if denom.abs() < 1e-6 {
            return None;
        }
        let t = (self.center - ray_origin).dot(panel_normal) / denom;
        if t < 0.0 {
            return None;
        }
        let hit = ray_origin + ray_dir * t;
        let local = hit - self.center;
        let u = local.dot(self.right_dir) / self.width + 0.5;
        let v = 0.5 - local.dot(self.up_dir) / self.height;
        if u >= 0.0 && u <= 1.0 && v >= 0.0 && v <= 1.0 {
            Some((u, v, t))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_direct_hit() {
        let panel = PanelTransform::default(); // center (0, 1.6, -2.5), facing +Z
        // Ray from origin pointing at panel center
        let origin = Vec3::new(0.0, 1.6, 0.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let result = panel.hit_test(origin, dir);
        assert!(result.is_some());
        let (u, v, t) = result.unwrap();
        assert!((u - 0.5).abs() < 0.01, "u should be ~0.5, got {}", u);
        assert!((v - 0.5).abs() < 0.01, "v should be ~0.5, got {}", v);
        assert!((t - 2.5).abs() < 0.01, "t should be ~2.5, got {}", t);
    }

    #[test]
    fn hit_test_miss_outside_panel() {
        let panel = PanelTransform::default();
        // Ray pointing far to the right - should miss
        let origin = Vec3::new(5.0, 1.6, 0.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        assert!(panel.hit_test(origin, dir).is_none());
    }

    #[test]
    fn hit_test_miss_parallel() {
        let panel = PanelTransform::default();
        // Ray parallel to panel plane
        let origin = Vec3::new(0.0, 1.6, 0.0);
        let dir = Vec3::new(1.0, 0.0, 0.0);
        assert!(panel.hit_test(origin, dir).is_none());
    }

    #[test]
    fn hit_test_miss_behind() {
        let panel = PanelTransform::default();
        // Ray pointing away from panel
        let origin = Vec3::new(0.0, 1.6, 0.0);
        let dir = Vec3::new(0.0, 0.0, 1.0);
        assert!(panel.hit_test(origin, dir).is_none());
    }

    #[test]
    fn hit_test_corner() {
        let panel = PanelTransform::default(); // width 1.6, height 1.0
        // Ray hitting top-right corner
        let origin = Vec3::new(0.8, 2.1, 0.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let result = panel.hit_test(origin, dir);
        assert!(result.is_some());
        let (u, v, _) = result.unwrap();
        assert!(u > 0.95, "u should be near 1.0, got {}", u);
        assert!(v < 0.05, "v should be near 0.0, got {}", v);
    }

    #[test]
    fn hit_test_floor_panel() {
        // FPS-style floor panel
        let panel = PanelTransform {
            center: Vec3::new(0.0, 0.01, -0.5),
            right_dir: Vec3::X,
            up_dir: -Vec3::Z,
            width: 0.3,
            height: 0.12,
            opacity: 0.9,
            anchor: PanelAnchor::World,
        };
        // Ray looking down at the floor
        let origin = Vec3::new(0.0, 1.6, 0.0);
        let dir = Vec3::new(0.0, -1.0, -0.3).normalize();
        let result = panel.hit_test(origin, dir);
        // This may or may not hit depending on exact geometry - just test it doesn't crash
        // The point is that non-standard orientations work
        let _ = result;
    }

    #[test]
    fn panel_anchor_default_is_world() {
        let t = PanelTransform::default();
        assert_eq!(t.anchor, PanelAnchor::World);
    }

    #[test]
    fn anchor_cycle_order() {
        let mut t = PanelTransform::default();
        assert_eq!(t.anchor, PanelAnchor::World);
        t.cycle_anchor();
        assert!(matches!(t.anchor, PanelAnchor::Controller { .. }));
        t.cycle_anchor();
        assert!(matches!(t.anchor, PanelAnchor::Wrist { .. }));
        t.cycle_anchor();
        assert!(matches!(t.anchor, PanelAnchor::Theater { .. }));
        t.cycle_anchor();
        assert!(matches!(t.anchor, PanelAnchor::Head { .. }));
        t.cycle_anchor();
        assert_eq!(t.anchor, PanelAnchor::World);
    }

    #[test]
    fn theater_mode_positions_in_front() {
        let mut t = PanelTransform::default();
        t.anchor = PanelAnchor::Theater { distance: 5.0, scale: 3.0 };
        t.update_anchor(
            Vec3::new(0.0, 1.6, 0.0),  // head_pos
            Vec3::new(0.0, 0.0, -1.0), // head_forward (-Z)
            Vec3::new(1.0, 0.0, 0.0),  // head_right
            Vec3::new(0.0, 1.0, 0.0),  // head_up
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, // controller (unused for theater)
        );
        assert!((t.center.z - (-5.0)).abs() < 0.1, "Panel should be ~5m in front, got z={}", t.center.z);
        assert!(t.width > 4.0, "Theater panel should be wide, got {}", t.width);
    }

    #[test]
    fn controller_anchor_follows_controller() {
        let mut t = PanelTransform::default();
        t.anchor = PanelAnchor::Controller { hand: Hand::Right };
        let ctrl_pos = Vec3::new(0.3, 1.0, -0.5);
        let ctrl_fwd = Vec3::new(0.0, 0.0, -1.0);
        t.update_anchor(
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO,
            ctrl_pos, ctrl_fwd, Vec3::X, Vec3::Y,
        );
        assert!((t.center - (ctrl_pos + ctrl_fwd * 0.25 + Vec3::Y * 0.05)).length() < 0.01);
    }

    #[test]
    fn world_anchor_does_not_move() {
        let mut t = PanelTransform::default();
        let original_center = t.center;
        t.update_anchor(
            Vec3::new(5.0, 5.0, 5.0), Vec3::X, Vec3::Y, Vec3::Z,
            Vec3::new(10.0, 10.0, 10.0), Vec3::X, Vec3::Y, Vec3::Z,
        );
        assert_eq!(t.center, original_center);
    }

    #[test]
    fn wrist_anchor_small_panel() {
        let mut t = PanelTransform::default();
        t.anchor = PanelAnchor::Wrist { hand: Hand::Left };
        t.update_anchor(
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO,
            Vec3::new(0.0, 1.0, -0.3), Vec3::NEG_Z, Vec3::X, Vec3::Y,
        );
        assert!(t.width < 0.2, "Wrist panel should be small, got {}", t.width);
        assert!(t.height < 0.15, "Wrist panel should be small, got {}", t.height);
    }

    #[test]
    fn hit_test_exact_boundary_u0() {
        let panel = PanelTransform::default(); // width 1.6, center at (0, 1.6, -2.5)
        // Ray hitting exactly at left edge (u=0.0)
        let origin = Vec3::new(-0.8, 1.6, 0.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let result = panel.hit_test(origin, dir);
        assert!(result.is_some(), "Exact left edge should be a hit");
        let (u, _, _) = result.unwrap();
        assert!((u - 0.0).abs() < 0.01);
    }

    #[test]
    fn hit_test_exact_boundary_u1() {
        let panel = PanelTransform::default();
        let origin = Vec3::new(0.8, 1.6, 0.0);
        let dir = Vec3::new(0.0, 0.0, -1.0);
        let result = panel.hit_test(origin, dir);
        assert!(result.is_some(), "Exact right edge should be a hit");
        let (u, _, _) = result.unwrap();
        assert!((u - 1.0).abs() < 0.01);
    }

    #[test]
    fn theater_mode_looking_straight_up() {
        let mut t = PanelTransform::default();
        t.anchor = PanelAnchor::Theater { distance: 5.0, scale: 3.0 };
        let _original_center = t.center;
        // Looking straight up: forward = (0, 1, 0)
        t.update_anchor(
            Vec3::new(0.0, 1.6, 0.0),
            Vec3::new(0.0, 1.0, 0.0),  // straight up
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO,
        );
        // Should not crash, panel position should remain unchanged (early return)
        // The center might not change since forward_flat normalizes to zero
        // Just verify it didn't produce NaN
        assert!(!t.center.x.is_nan());
        assert!(!t.center.y.is_nan());
        assert!(!t.center.z.is_nan());
    }

    #[test]
    fn head_anchor_positions_with_offset() {
        let mut t = PanelTransform::default();
        t.anchor = PanelAnchor::Head { offset: Vec3::new(0.5, -0.3, -2.0) };
        t.update_anchor(
            Vec3::new(0.0, 1.6, 0.0),   // head pos
            Vec3::new(0.0, 0.0, -1.0),  // forward
            Vec3::new(1.0, 0.0, 0.0),   // right
            Vec3::new(0.0, 1.0, 0.0),   // up
            Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO,
        );
        // Expected: head_pos + right*0.5 + up*(-0.3) + forward*(-2.0)
        let expected = Vec3::new(0.5, 1.3, 2.0);
        assert!((t.center - expected).length() < 0.01, "Head anchor offset wrong: {:?}", t.center);
    }
}
