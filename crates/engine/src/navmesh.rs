//! Production navigation facade with render-only debugging kept at composition level.

pub use engine_physics::navigation::*;

/// Legacy grid navigation retained for focused compatibility and migration utilities.
pub mod grid {
    pub use engine_physics::navmesh::*;
}

use crate::debug_draw::DebugLines;
use crate::transform::GlobalTransform;
use engine_ecs::{Query, Res, ResMut};
use glam::Vec3;

/// Draws the actual production polygons, off-mesh links, and live agent paths.
pub fn nav_mesh_debug_draw_system(
    nav_mesh: Option<Res<NavMeshQuery>>,
    agents: Query<(&NavMeshAgent, &GlobalTransform)>,
    mut lines: Option<ResMut<DebugLines>>,
) {
    let (Some(nav_mesh), Some(lines)) = (nav_mesh, lines.as_deref_mut()) else {
        return;
    };
    let blue = Vec3::new(0.1, 0.45, 1.0);
    let link_color = Vec3::new(0.95, 0.65, 0.1);
    let path_color = Vec3::new(1.0, 0.2, 1.0);
    let lift = Vec3::Y * 0.03;
    if let Some(profile) = nav_mesh.nav_mesh.profile(DEFAULT_NAVIGATION_PROFILE) {
        for tile in &profile.tiles {
            for polygon in &tile.polygons {
                let points = polygon
                    .vertices
                    .iter()
                    .map(|index| Vec3::from_array(tile.vertices[*index as usize]) + lift)
                    .collect::<Vec<_>>();
                for index in 0..points.len() {
                    lines.line(points[index], points[(index + 1) % points.len()], blue);
                }
            }
        }
        for link in &profile.links {
            lines.line(
                Vec3::from_array(link.start) + lift,
                Vec3::from_array(link.end) + lift,
                link_color,
            );
        }
    }
    for (_, (agent, transform)) in &agents {
        let mut previous = transform.matrix().col(3).truncate();
        for waypoint in agent.path() {
            lines.line(previous, *waypoint + lift, path_color);
            previous = *waypoint + lift;
        }
    }
}
