//! Navigation runtime facade with the render-only debug adapter kept at composition level.

pub use engine_physics::navmesh::*;

use crate::debug_draw::DebugLines;
use crate::transform::GlobalTransform;
use engine_ecs::{Query, Res, ResMut};
use glam::Vec3;

/// Draws baked walkable cells and live agent paths for Scene/Play debugging.
pub fn nav_mesh_debug_draw_system(
    nav_mesh: Option<Res<NavMeshQuery>>,
    agents: Query<(&NavMeshAgent, &GlobalTransform)>,
    mut lines: Option<ResMut<DebugLines>>,
) {
    let (Some(nav_mesh), Some(lines)) = (nav_mesh, lines.as_deref_mut()) else {
        return;
    };
    let mesh = &nav_mesh.nav_mesh;
    let y = 0.03;
    let half = mesh.cell_size * 0.5;
    let blue = Vec3::new(0.1, 0.45, 1.0);
    for row in 0..mesh.rows {
        for col in 0..mesh.cols {
            if !mesh.is_walkable(col, row) {
                continue;
            }
            let center = mesh.cell_center(col, row, y);
            let first = center + Vec3::new(-half, 0.0, -half);
            let second = center + Vec3::new(half, 0.0, -half);
            let third = center + Vec3::new(half, 0.0, half);
            let fourth = center + Vec3::new(-half, 0.0, half);
            lines.line(first, second, blue);
            lines.line(second, third, blue);
            lines.line(third, fourth, blue);
            lines.line(fourth, first, blue);
        }
    }
    for (_, (agent, transform)) in &agents {
        let mut previous = transform.matrix().col(3).truncate();
        for waypoint in agent.path() {
            lines.line(previous, *waypoint + Vec3::Y * y, Vec3::new(1.0, 0.2, 1.0));
            previous = *waypoint + Vec3::Y * y;
        }
    }
}

