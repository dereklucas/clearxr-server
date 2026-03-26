//! Play space boundary visualization.
//!
//! Renders translucent walls when the user approaches the edge
//! of their configured play space.

use glam::Vec3;

/// Rectangular play space boundary.
#[derive(Clone, Debug)]
pub struct Boundary {
    /// Half-extents of the play space in X and Z (meters from center).
    pub half_width: f32,   // X axis
    pub half_depth: f32,   // Z axis
    /// Distance from the edge at which the boundary starts appearing.
    pub fade_distance: f32,
    /// Whether the boundary is enabled.
    pub enabled: bool,
}

impl Default for Boundary {
    fn default() -> Self {
        Self {
            half_width: 1.5,   // 3m x 3m default play space
            half_depth: 1.5,
            fade_distance: 0.8, // start showing at 0.8m from edge
            enabled: true,
        }
    }
}

/// Which walls are visible and their opacity.
#[derive(Clone, Debug, Default)]
pub struct BoundaryVisibility {
    pub left: f32,    // -X wall opacity [0, 1]
    pub right: f32,   // +X wall opacity
    pub front: f32,   // -Z wall opacity
    pub back: f32,    // +Z wall opacity
}

impl Boundary {
    /// Set the play space size from OpenXR bounds.
    pub fn set_bounds(&mut self, width: f32, depth: f32) {
        self.half_width = width / 2.0;
        self.half_depth = depth / 2.0;
    }

    /// Compute wall visibilities based on user position.
    /// Returns opacity for each wall (0 = invisible, 1 = fully visible).
    pub fn compute_visibility(&self, user_pos: Vec3) -> BoundaryVisibility {
        if !self.enabled {
            return BoundaryVisibility::default();
        }

        BoundaryVisibility {
            left: self.wall_opacity(user_pos.x + self.half_width),
            right: self.wall_opacity(self.half_width - user_pos.x),
            front: self.wall_opacity(user_pos.z + self.half_depth),
            back: self.wall_opacity(self.half_depth - user_pos.z),
        }
    }

    /// Compute opacity for a single wall based on signed distance from edge.
    /// distance_to_edge = how far inside the boundary (positive = inside, negative = outside).
    fn wall_opacity(&self, distance_to_edge: f32) -> f32 {
        if distance_to_edge >= self.fade_distance {
            0.0 // far from edge
        } else if distance_to_edge <= 0.0 {
            1.0 // at or past edge
        } else {
            1.0 - (distance_to_edge / self.fade_distance)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_center_invisible() {
        let b = Boundary::default(); // 3m x 3m, centered at origin
        let vis = b.compute_visibility(Vec3::ZERO);
        assert_eq!(vis.left, 0.0);
        assert_eq!(vis.right, 0.0);
        assert_eq!(vis.front, 0.0);
        assert_eq!(vis.back, 0.0);
    }

    #[test]
    fn boundary_near_edge_fades_in() {
        let b = Boundary::default(); // half_width = 1.5, fade = 0.8
        // At x = 1.0, distance to +X edge = 0.5 (within fade_distance)
        let vis = b.compute_visibility(Vec3::new(1.0, 0.0, 0.0));
        assert!(vis.right > 0.0, "Right wall should be visible near edge");
        assert!(vis.right < 1.0, "Right wall should not be fully opaque yet");
        assert_eq!(vis.left, 0.0, "Left wall should be invisible");
    }

    #[test]
    fn boundary_at_edge_fully_visible() {
        let b = Boundary::default();
        // At x = 1.5 (exactly at the boundary edge)
        let vis = b.compute_visibility(Vec3::new(1.5, 0.0, 0.0));
        assert!((vis.right - 1.0).abs() < 0.01, "Right wall should be fully visible at edge");
    }

    #[test]
    fn boundary_past_edge() {
        let b = Boundary::default();
        let vis = b.compute_visibility(Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(vis.right, 1.0, "Past edge should be fully visible");
    }

    #[test]
    fn boundary_disabled() {
        let mut b = Boundary::default();
        b.enabled = false;
        let vis = b.compute_visibility(Vec3::new(1.5, 0.0, 0.0));
        assert_eq!(vis.left, 0.0);
        assert_eq!(vis.right, 0.0);
        assert_eq!(vis.front, 0.0);
        assert_eq!(vis.back, 0.0);
    }

    #[test]
    fn boundary_set_bounds() {
        let mut b = Boundary::default();
        b.set_bounds(4.0, 6.0);
        assert_eq!(b.half_width, 2.0);
        assert_eq!(b.half_depth, 3.0);
    }

    #[test]
    fn boundary_corner_two_walls() {
        let b = Boundary::default();
        // Near the +X, -Z corner
        let vis = b.compute_visibility(Vec3::new(1.2, 0.0, -1.2));
        assert!(vis.right > 0.0, "Right wall visible in corner");
        assert!(vis.front > 0.0, "Front wall visible in corner");
        assert_eq!(vis.left, 0.0);
        assert_eq!(vis.back, 0.0);
    }

    #[test]
    fn boundary_negative_position() {
        let b = Boundary::default(); // half_width=1.5, half_depth=1.5
        // User at far negative X: should trigger left wall
        let vis = b.compute_visibility(Vec3::new(-1.2, 0.0, 0.0));
        assert!(vis.left > 0.0, "Left wall should be visible at negative X near edge");
        assert_eq!(vis.right, 0.0, "Right wall should not be visible");
    }

    #[test]
    fn boundary_far_outside() {
        let b = Boundary::default();
        let vis = b.compute_visibility(Vec3::new(-5.0, 0.0, 0.0));
        assert_eq!(vis.left, 1.0, "Far outside left should be fully visible");
    }
}
