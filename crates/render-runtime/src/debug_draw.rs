//! Immediate-mode debug line drawing.
//!
//! Insert [`DebugLines`] as an ECS resource and call its helper methods each
//! frame to visualize transforms, bounding boxes, and arbitrary line segments.
//! The built-in render pipeline clears all lines after drawing them.

use glam::{Vec3, Vec4};

/// A single line segment drawn for one frame.
#[derive(Debug, Clone, Copy)]
pub struct DebugLine {
    /// World-space start position.
    pub from: Vec3,
    /// World-space end position.
    pub to: Vec3,
    /// RGBA color in `[0.0, 1.0]` per channel.
    pub color: [f32; 4],
}

/// An ECS resource that accumulates debug lines drawn during one frame.
///
/// All lines are cleared by the render pipeline after each frame so callers
/// must re-submit every frame they want the visualization to appear.
///
/// # Example
///
/// ```rust,no_run
/// use engine_ecs::ResMut;
/// use engine_render_runtime::debug_draw::DebugLines;
/// use glam::Vec3;
///
/// fn my_debug_system(mut lines: ResMut<DebugLines>) {
///     lines.line(Vec3::ZERO, Vec3::X, Vec3::new(1.0, 0.0, 0.0));
/// }
/// ```
#[derive(Debug)]
pub struct DebugLines {
    /// The accumulated line segments for the current frame.
    pub lines: Vec<DebugLine>,
    /// Whether debug lines are drawn this frame.
    ///
    /// When `false` the render pipeline skips drawing the accumulated lines
    /// but still clears them after each frame. Defaults to `true`.
    pub enabled: bool,
}

impl Default for DebugLines {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            enabled: true,
        }
    }
}

impl DebugLines {
    /// Creates an empty [`DebugLines`] resource with debug draw enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a single line segment with the given RGBA `color`.
    pub fn line(&mut self, from: Vec3, to: Vec3, color: Vec3) {
        self.lines.push(DebugLine {
            from,
            to,
            color: [color.x, color.y, color.z, 1.0],
        });
    }

    /// Appends a line segment with explicit RGBA color.
    pub fn line_rgba(&mut self, from: Vec3, to: Vec3, color: Vec4) {
        self.lines.push(DebugLine {
            from,
            to,
            color: color.to_array(),
        });
    }

    /// Appends twelve line segments forming an axis-aligned bounding box.
    pub fn aabb(&mut self, center: Vec3, half_extents: Vec3, color: Vec3) {
        let color = [color.x, color.y, color.z, 1.0];
        let he = half_extents;
        let corners = [
            center + Vec3::new(-he.x, -he.y, -he.z),
            center + Vec3::new(he.x, -he.y, -he.z),
            center + Vec3::new(he.x, he.y, -he.z),
            center + Vec3::new(-he.x, he.y, -he.z),
            center + Vec3::new(-he.x, -he.y, he.z),
            center + Vec3::new(he.x, -he.y, he.z),
            center + Vec3::new(he.x, he.y, he.z),
            center + Vec3::new(-he.x, he.y, he.z),
        ];
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        for (a, b) in edges {
            self.lines.push(DebugLine {
                from: corners[a],
                to: corners[b],
                color,
            });
        }
    }

    /// Appends three axis lines (X=red, Y=green, Z=blue) at `transform`.
    pub fn axes(&mut self, transform: &crate::transform::Transform, length: f32) {
        let mat = transform.to_matrix();
        let origin = mat.w_axis.truncate();
        let right = mat.x_axis.truncate().normalize_or_zero() * length;
        let up = mat.y_axis.truncate().normalize_or_zero() * length;
        let forward = mat.z_axis.truncate().normalize_or_zero() * length;
        self.lines.push(DebugLine {
            from: origin,
            to: origin + right,
            color: [1.0, 0.0, 0.0, 1.0],
        });
        self.lines.push(DebugLine {
            from: origin,
            to: origin + up,
            color: [0.0, 1.0, 0.0, 1.0],
        });
        self.lines.push(DebugLine {
            from: origin,
            to: origin + forward,
            color: [0.0, 0.0, 1.0, 1.0],
        });
    }

    /// Appends a wireframe sphere approximation using latitude/longitude rings.
    pub fn sphere_wire(&mut self, center: Vec3, radius: f32, color: Vec3) {
        const SEGMENTS: usize = 16;
        let color = [color.x, color.y, color.z, 1.0];
        for axis in 0..3 {
            let (ax, ay) = match axis {
                0 => (Vec3::X, Vec3::Y),
                1 => (Vec3::X, Vec3::Z),
                _ => (Vec3::Y, Vec3::Z),
            };
            let mut prev = center + ax * radius;
            for i in 1..=SEGMENTS {
                let angle = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
                let next = center + (ax * angle.cos() + ay * angle.sin()) * radius;
                self.lines.push(DebugLine {
                    from: prev,
                    to: next,
                    color,
                });
                prev = next;
            }
        }
    }

    /// Appends an exact wireframe approximation of a Y-axis capsule.
    ///
    /// `top` and `bottom` are the endpoints of the capsule's core segment;
    /// the hemispherical caps extend one `radius` beyond those points.
    pub fn capsule_y_wire(&mut self, top: Vec3, bottom: Vec3, radius: f32, color: Vec3) {
        const SEGMENTS: usize = 16;
        const HALF_SEGMENTS: usize = SEGMENTS / 2;

        for ring_center in [top, bottom] {
            let mut previous = ring_center + Vec3::X * radius;
            for index in 1..=SEGMENTS {
                let angle = std::f32::consts::TAU * index as f32 / SEGMENTS as f32;
                let next = ring_center + (Vec3::X * angle.cos() + Vec3::Z * angle.sin()) * radius;
                self.line(previous, next, color);
                previous = next;
            }
        }

        for radial in [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
            self.line(bottom + radial * radius, top + radial * radius, color);
        }

        for radial in [Vec3::X, Vec3::Z] {
            let mut previous = top + radial * radius;
            for index in 1..=HALF_SEGMENTS {
                let angle = std::f32::consts::PI * index as f32 / HALF_SEGMENTS as f32;
                let next = top + (radial * angle.cos() + Vec3::Y * angle.sin()) * radius;
                self.line(previous, next, color);
                previous = next;
            }

            let mut previous = bottom - radial * radius;
            for index in 1..=HALF_SEGMENTS {
                let angle = std::f32::consts::PI * index as f32 / HALF_SEGMENTS as f32;
                let next = bottom + (-radial * angle.cos() - Vec3::Y * angle.sin()) * radius;
                self.line(previous, next, color);
                previous = next;
            }
        }
    }

    /// Removes all accumulated lines after the renderer consumes them.
    #[doc(hidden)]
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aabb_produces_twelve_edges() {
        let mut lines = DebugLines::new();
        lines.aabb(Vec3::ZERO, Vec3::ONE * 0.5, Vec3::ONE);
        assert_eq!(lines.lines.len(), 12, "AABB must have exactly 12 edges");
    }

    #[test]
    fn axes_produces_three_lines() {
        let mut lines = DebugLines::new();
        lines.axes(&crate::transform::Transform::default(), 1.0);
        assert_eq!(lines.lines.len(), 3, "axes must produce exactly 3 lines");
    }

    #[test]
    fn capsule_wire_produces_cap_rings_rails_and_profiles() {
        let mut lines = DebugLines::new();
        lines.capsule_y_wire(Vec3::Y, -Vec3::Y, 0.5, Vec3::ONE);
        assert_eq!(lines.lines.len(), 68);
    }

    #[test]
    fn clear_removes_all_lines() {
        let mut lines = DebugLines::new();
        lines.line(Vec3::ZERO, Vec3::X, Vec3::ONE);
        lines.clear();
        assert!(lines.lines.is_empty());
    }

    #[test]
    fn line_stores_correct_endpoints() {
        let mut lines = DebugLines::new();
        let from = Vec3::new(1.0, 2.0, 3.0);
        let to = Vec3::new(4.0, 5.0, 6.0);
        lines.line(from, to, Vec3::X);
        assert_eq!(lines.lines[0].from, from);
        assert_eq!(lines.lines[0].to, to);
    }
}
