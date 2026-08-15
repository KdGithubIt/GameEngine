//! Grid-backed navigation mesh and A* pathfinding (Phase 44, ADR 0039).
//!
//! ## Quick start
//!
//! 1. Build a [`NavMesh`] with [`bake_from_obstacles`] (or the ECS-aware
//!    [`bake_navmesh`] helper) and save it with [`save_navmesh`].
//! 2. Load it at runtime with [`load_navmesh`] and insert the result wrapped
//!    in a [`NavMeshQuery`] as a world resource.
//! 3. Add [`NavMeshAgent`] to entities you want to path-find; set
//!    [`NavMeshAgent::target`] to a world-space destination.
//! 4. Register [`nav_mesh_agent_system`] with
//!    [`engine_ecs::App::add_fixed_system`].

use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use glam::Vec3;
use serde::{Deserialize, Serialize};

use engine_ecs::{Query, Res};

use crate::time::FixedTime;
use crate::transform::Transform;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Configuration used when baking a [`NavMesh`] from scene geometry.
#[derive(Debug, Clone)]
pub struct NavMeshSettings {
    /// Size of each square grid cell in world units.
    pub cell_size: f32,
    /// How much obstacle AABBs are expanded before marking cells blocked.
    /// Set to at least the agent's physical radius.
    pub agent_radius: f32,
    /// World-space minimum corner of the navigation area (XZ plane).
    pub world_min: Vec3,
    /// World-space maximum corner of the navigation area (XZ plane).
    pub world_max: Vec3,
    /// Height of the walkable plane used by the grid bake.
    pub walkable_height: f32,
    /// Vertical clearance checked when deciding whether a collider is an obstacle.
    pub agent_height: f32,
}

impl Default for NavMeshSettings {
    fn default() -> Self {
        Self {
            cell_size: 0.5,
            agent_radius: 0.4,
            world_min: Vec3::new(-50.0, 0.0, -50.0),
            world_max: Vec3::new(50.0, 0.0, 50.0),
            walkable_height: 0.0,
            agent_height: 1.8,
        }
    }
}

// ---------------------------------------------------------------------------
// NavMesh
// ---------------------------------------------------------------------------

/// Grid-backed navigation mesh.
///
/// The grid is aligned to the XZ plane. The Y coordinate of path waypoints
/// is inherited from the query start position.
///
/// Use [`bake_from_obstacles`] to create one and [`save_navmesh`] /
/// [`load_navmesh`] to persist it across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavMesh {
    /// World-space X coordinate of the grid's minimum corner.
    pub origin_x: f32,
    /// World-space Z coordinate of the grid's minimum corner.
    pub origin_z: f32,
    /// Number of columns (X axis).
    pub cols: usize,
    /// Number of rows (Z axis).
    pub rows: usize,
    /// Cell edge length in world units.
    pub cell_size: f32,
    /// Row-major walkability flags. `walkable[row * cols + col]`.
    pub walkable: Vec<bool>,
}

impl NavMesh {
    fn cell_idx(&self, col: usize, row: usize) -> usize {
        row * self.cols + col
    }

    /// Returns whether one grid cell is inside the mesh and walkable.
    #[doc(hidden)]
    pub fn is_walkable(&self, col: usize, row: usize) -> bool {
        col < self.cols && row < self.rows && self.walkable[self.cell_idx(col, row)]
    }

    fn world_to_col(&self, x: f32) -> Option<usize> {
        let c = ((x - self.origin_x) / self.cell_size).floor();
        if c < 0.0 || c as usize >= self.cols {
            None
        } else {
            Some(c as usize)
        }
    }

    fn world_to_row(&self, z: f32) -> Option<usize> {
        let r = ((z - self.origin_z) / self.cell_size).floor();
        if r < 0.0 || r as usize >= self.rows {
            None
        } else {
            Some(r as usize)
        }
    }

    /// Returns the world-space center of one grid cell at `y`.
    #[doc(hidden)]
    pub fn cell_center(&self, col: usize, row: usize, y: f32) -> Vec3 {
        Vec3::new(
            self.origin_x + (col as f32 + 0.5) * self.cell_size,
            y,
            self.origin_z + (row as f32 + 0.5) * self.cell_size,
        )
    }
}

// ---------------------------------------------------------------------------
// Baking
// ---------------------------------------------------------------------------

/// Builds a [`NavMesh`] from a list of obstacle AABBs.
///
/// Each obstacle is a `(center, half_extents)` pair in world space. Obstacles
/// are expanded by [`NavMeshSettings::agent_radius`] before cells are marked
/// as blocked.
pub fn bake_from_obstacles(obstacles: &[(Vec3, Vec3)], settings: &NavMeshSettings) -> NavMesh {
    let cols = ((settings.world_max.x - settings.world_min.x) / settings.cell_size).ceil() as usize;
    let rows = ((settings.world_max.z - settings.world_min.z) / settings.cell_size).ceil() as usize;
    let cols = cols.max(1);
    let rows = rows.max(1);

    let mut walkable = vec![true; cols * rows];

    let origin_x = settings.world_min.x;
    let origin_z = settings.world_min.z;
    let r = settings.agent_radius;

    for &(center, half) in obstacles {
        let min_x = center.x - half.x - r;
        let max_x = center.x + half.x + r;
        let min_z = center.z - half.z - r;
        let max_z = center.z + half.z + r;

        let c_min = ((min_x - origin_x) / settings.cell_size).floor().max(0.0) as usize;
        let c_max = ((max_x - origin_x) / settings.cell_size).ceil() as usize;
        let r_min = ((min_z - origin_z) / settings.cell_size).floor().max(0.0) as usize;
        let r_max = ((max_z - origin_z) / settings.cell_size).ceil() as usize;

        for row in r_min..r_max.min(rows) {
            for col in c_min..c_max.min(cols) {
                walkable[row * cols + col] = false;
            }
        }
    }

    NavMesh {
        origin_x,
        origin_z,
        cols,
        rows,
        cell_size: settings.cell_size,
        walkable,
    }
}

/// Queries the ECS `world` for all [`crate::collision::Collider`] +
/// [`crate::transform::GlobalTransform`] pairs and bakes a [`NavMesh`],
/// then saves it to `output_path`.
///
/// # Errors
///
/// Returns [`NavMeshError::Io`] or [`NavMeshError::Json`] on save failure.
/// Not available on wasm32 (no filesystem).
#[cfg(not(target_arch = "wasm32"))]
pub fn bake_navmesh(
    world: &mut engine_ecs::World,
    settings: &NavMeshSettings,
    output_path: &Path,
) -> Result<NavMesh, NavMeshError> {
    use crate::collision::{Collider, PhysicsBody, TriggerVolume};
    use crate::transform::GlobalTransform;

    let obstacles: Vec<(Vec3, Vec3)> = if let Ok(query) = world.query::<(
        &GlobalTransform,
        &Collider,
        Option<&PhysicsBody>,
        Option<&TriggerVolume>,
    )>() {
        query
            .iter()
            .filter_map(|(_, (transform, collider, body, trigger))| {
                if body != Some(&PhysicsBody::Static) || trigger.is_some() {
                    return None;
                }
                let aabb = collider.world_aabb(transform);
                let min_y = aabb.center.y - aabb.half_extents.y;
                let max_y = aabb.center.y + aabb.half_extents.y;
                let clearance_top = settings.walkable_height + settings.agent_height;
                if max_y <= settings.walkable_height + 0.05 || min_y >= clearance_top {
                    return None;
                }
                Some((aabb.center, aabb.half_extents))
            })
            .collect()
    } else {
        Vec::new()
    };

    let nav_mesh = bake_from_obstacles(&obstacles, settings);
    save_navmesh(&nav_mesh, output_path)?;
    Ok(nav_mesh)
}

/// Reports that filesystem-backed navmesh baking is unavailable on wasm32.
///
/// # Errors
///
/// Always returns [`NavMeshError::Io`] with [`io::ErrorKind::Unsupported`].
#[cfg(target_arch = "wasm32")]
pub fn bake_navmesh(
    _world: &mut engine_ecs::World,
    _settings: &NavMeshSettings,
    _output_path: &Path,
) -> Result<NavMesh, NavMeshError> {
    Err(unsupported_navmesh_io())
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Serialises `nav_mesh` to `path` (JSON, may be named `navmesh.bin`).
///
/// # Errors
///
/// Returns [`NavMeshError::Json`] on serialisation failure or
/// [`NavMeshError::Io`] on write failure.
/// Not available on wasm32 (no filesystem).
#[cfg(not(target_arch = "wasm32"))]
pub fn save_navmesh(nav_mesh: &NavMesh, path: &Path) -> Result<(), NavMeshError> {
    let json = serde_json::to_string(nav_mesh).map_err(NavMeshError::Json)?;
    std::fs::write(path, json).map_err(NavMeshError::Io)?;
    Ok(())
}

/// Reports that filesystem-backed navmesh persistence is unavailable on wasm32.
///
/// # Errors
///
/// Always returns [`NavMeshError::Io`] with [`io::ErrorKind::Unsupported`].
#[cfg(target_arch = "wasm32")]
pub fn save_navmesh(_nav_mesh: &NavMesh, _path: &Path) -> Result<(), NavMeshError> {
    Err(unsupported_navmesh_io())
}

/// Loads a [`NavMesh`] from `path`.
///
/// # Errors
///
/// Returns [`NavMeshError::Io`] on read failure, [`NavMeshError::Json`] on
/// parse failure, or [`NavMeshError::InvalidGrid`] when the walkable array
/// length does not match the declared grid dimensions.
/// Not available on wasm32 (no filesystem).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_navmesh(path: &Path) -> Result<NavMesh, NavMeshError> {
    let bytes = std::fs::read(path).map_err(NavMeshError::Io)?;
    let nav_mesh: NavMesh = serde_json::from_slice(&bytes).map_err(NavMeshError::Json)?;
    if nav_mesh.walkable.len() != nav_mesh.cols * nav_mesh.rows {
        return Err(NavMeshError::InvalidGrid);
    }
    Ok(nav_mesh)
}

/// Reports that filesystem-backed navmesh loading is unavailable on wasm32.
///
/// # Errors
///
/// Always returns [`NavMeshError::Io`] with [`io::ErrorKind::Unsupported`].
#[cfg(target_arch = "wasm32")]
pub fn load_navmesh(_path: &Path) -> Result<NavMesh, NavMeshError> {
    Err(unsupported_navmesh_io())
}

#[cfg(target_arch = "wasm32")]
fn unsupported_navmesh_io() -> NavMeshError {
    NavMeshError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "navmesh file IO is not available on wasm32",
    ))
}

// ---------------------------------------------------------------------------
// A*
// ---------------------------------------------------------------------------

const COST_CARDINAL: u32 = 10;
const COST_DIAGONAL: u32 = 14;

fn octile_distance(c0: usize, r0: usize, c1: usize, r1: usize) -> u32 {
    let dc = (c0 as i32 - c1 as i32).unsigned_abs();
    let dr = (r0 as i32 - r1 as i32).unsigned_abs();
    let (lo, hi) = if dc < dr { (dc, dr) } else { (dr, dc) };
    COST_DIAGONAL * lo + COST_CARDINAL * (hi - lo)
}

fn astar(
    nav_mesh: &NavMesh,
    start_col: usize,
    start_row: usize,
    end_col: usize,
    end_row: usize,
) -> Option<Vec<(usize, usize)>> {
    if !nav_mesh.is_walkable(start_col, start_row) || !nav_mesh.is_walkable(end_col, end_row) {
        return None;
    }

    if start_col == end_col && start_row == end_row {
        return Some(vec![(start_col, start_row)]);
    }

    // Min-heap ordered by (f_cost, g_cost, col, row).
    let mut open: BinaryHeap<std::cmp::Reverse<(u32, u32, usize, usize)>> = BinaryHeap::new();
    let mut g_cost: HashMap<(usize, usize), u32> = HashMap::new();
    let mut came_from: HashMap<(usize, usize), (usize, usize)> = HashMap::new();

    let h0 = octile_distance(start_col, start_row, end_col, end_row);
    open.push(std::cmp::Reverse((h0, 0, start_col, start_row)));
    g_cost.insert((start_col, start_row), 0);

    while let Some(std::cmp::Reverse((_, g, col, row))) = open.pop() {
        if col == end_col && row == end_row {
            let mut path = vec![(col, row)];
            let mut cur = (col, row);
            while let Some(&prev) = came_from.get(&cur) {
                path.push(prev);
                cur = prev;
            }
            path.reverse();
            return Some(path);
        }

        if g_cost.get(&(col, row)).copied().unwrap_or(u32::MAX) < g {
            continue;
        }

        let neighbors: [(usize, usize, u32); 8] = [
            (col.wrapping_sub(1), row, COST_CARDINAL),
            (col + 1, row, COST_CARDINAL),
            (col, row.wrapping_sub(1), COST_CARDINAL),
            (col, row + 1, COST_CARDINAL),
            (col.wrapping_sub(1), row.wrapping_sub(1), COST_DIAGONAL),
            (col + 1, row.wrapping_sub(1), COST_DIAGONAL),
            (col.wrapping_sub(1), row + 1, COST_DIAGONAL),
            (col + 1, row + 1, COST_DIAGONAL),
        ];

        for (nc, nr, step) in neighbors {
            if !nav_mesh.is_walkable(nc, nr) {
                continue;
            }
            let new_g = g + step;
            if new_g < g_cost.get(&(nc, nr)).copied().unwrap_or(u32::MAX) {
                g_cost.insert((nc, nr), new_g);
                came_from.insert((nc, nr), (col, row));
                let f = new_g + octile_distance(nc, nr, end_col, end_row);
                open.push(std::cmp::Reverse((f, new_g, nc, nr)));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// NavMeshQuery resource
// ---------------------------------------------------------------------------

/// World resource that wraps a loaded [`NavMesh`] and exposes path queries.
///
/// Insert this resource to enable [`nav_mesh_agent_system`].
pub struct NavMeshQuery {
    /// The underlying navigation grid.
    pub nav_mesh: NavMesh,
}

/// Scene-owned reference to the baked navigation artifact used at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavMeshSurface {
    /// Resolved project asset path retained for Play inspection and diagnostics.
    pub source: PathBuf,
}

impl NavMeshQuery {
    /// Creates a query resource from a loaded navmesh.
    pub fn new(nav_mesh: NavMesh) -> Self {
        Self { nav_mesh }
    }

    /// Finds a path from `start` to `end` in world space.
    ///
    /// Returns `None` when either point is outside the grid, the cell is
    /// blocked, or no path exists. Otherwise returns a sequence of world-space
    /// waypoints (XZ-plane, Y inherited from `start`).
    pub fn find_path(&self, start: Vec3, end: Vec3) -> Option<Vec<Vec3>> {
        let nm = &self.nav_mesh;
        let start_col = nm.world_to_col(start.x)?;
        let start_row = nm.world_to_row(start.z)?;
        let end_col = nm.world_to_col(end.x)?;
        let end_row = nm.world_to_row(end.z)?;

        let cells = astar(nm, start_col, start_row, end_col, end_row)?;
        Some(
            cells
                .into_iter()
                .map(|(c, r)| nm.cell_center(c, r, start.y))
                .collect(),
        )
    }
}

// ---------------------------------------------------------------------------
// NavMeshAgent component
// ---------------------------------------------------------------------------

/// Drives an entity toward [`NavMeshAgent::target`] by following a path from
/// the [`NavMeshQuery`] resource.
///
/// Register [`nav_mesh_agent_system`] in the **fixed** update schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavMeshAgentStatus {
    /// The agent has no target and no path.
    Idle,
    /// A target exists but the runtime has no baked NavMesh resource.
    MissingNavMesh,
    /// No route could be found for the current target.
    NoPath,
    /// The agent is following a computed route.
    Moving,
    /// The agent reached the target's stopping distance.
    Arrived,
}

/// Drives an entity toward [`NavMeshAgent::target`] by following a path from
/// the [`NavMeshQuery`] resource.
pub struct NavMeshAgent {
    /// World-space destination. Assign to start or change pathfinding.
    pub target: Option<Vec3>,
    /// Movement speed in world units per second.
    pub speed: f32,
    /// Distance to the final waypoint at which the agent stops.
    pub stopping_distance: f32,
    /// Seconds between path refreshes while a target remains active.
    pub repath_interval: f32,
    /// Radius used for lightweight local agent separation.
    pub avoidance_radius: f32,
    /// Latest runtime navigation state for project Rust and Play inspection.
    pub status: NavMeshAgentStatus,
    current_path: Vec<Vec3>,
    path_index: usize,
    computed_target: Option<Vec3>,
    repath_remaining: f32,
}

impl NavMeshAgent {
    /// Creates an agent with the given speed and a 0.1-unit stopping distance.
    pub fn new(speed: f32) -> Self {
        Self {
            target: None,
            speed,
            stopping_distance: 0.1,
            repath_interval: 0.5,
            avoidance_radius: 0.4,
            status: NavMeshAgentStatus::Idle,
            current_path: Vec::new(),
            path_index: 0,
            computed_target: None,
            repath_remaining: 0.0,
        }
    }

    /// Returns the current planned path waypoints.
    pub fn path(&self) -> &[Vec3] {
        &self.current_path
    }

    /// Returns `true` when the agent has no remaining path to follow.
    pub fn is_idle(&self) -> bool {
        self.current_path.is_empty()
    }

    /// Replaces the destination and forces a path refresh on the next step.
    pub fn set_target(&mut self, target: Option<Vec3>) {
        self.target = target;
        self.repath_remaining = 0.0;
        if target.is_none() {
            self.current_path.clear();
            self.path_index = 0;
            self.status = NavMeshAgentStatus::Idle;
        }
    }
}

impl Default for NavMeshAgent {
    fn default() -> Self {
        Self::new(3.5)
    }
}

// ---------------------------------------------------------------------------
// ECS system
// ---------------------------------------------------------------------------

/// Advances every [`NavMeshAgent`] along its planned path.
///
/// When [`NavMeshAgent::target`] changes the path is recomputed via the
/// [`NavMeshQuery`] resource. If the resource is absent the system is a no-op.
///
/// Register in the **fixed** update schedule so movement is framerate-independent.
pub fn nav_mesh_agent_system(
    nav_mesh: Option<Res<NavMeshQuery>>,
    fixed_time: Res<FixedTime>,
    mut agents: Query<(&mut NavMeshAgent, &mut Transform)>,
) {
    let dt = fixed_time.fixed_delta;

    if nav_mesh.is_none() {
        for (_, (agent, _)) in agents.iter_mut() {
            agent.status = if agent.target.is_some() {
                NavMeshAgentStatus::MissingNavMesh
            } else {
                NavMeshAgentStatus::Idle
            };
        }
        return;
    }
    let Some(nav_mesh) = nav_mesh else {
        return;
    };

    for (_, (agent, transform)) in agents.iter_mut() {
        // Recompute path only when the target changes.
        agent.repath_remaining = (agent.repath_remaining - dt).max(0.0);
        let target_changed = agent.target != agent.computed_target;
        if target_changed || (agent.target.is_some() && agent.repath_remaining <= 0.0) {
            agent.computed_target = agent.target;
            agent.repath_remaining = agent.repath_interval.max(0.0);
            agent.current_path.clear();
            agent.path_index = 0;

            if let Some(tgt) = agent.target {
                if let Some(path) = nav_mesh.find_path(transform.translation, tgt) {
                    agent.current_path = path;
                    agent.status = NavMeshAgentStatus::Moving;
                } else {
                    agent.status = NavMeshAgentStatus::NoPath;
                }
            } else {
                agent.status = NavMeshAgentStatus::Idle;
            }
        }

        if agent.current_path.is_empty() {
            continue;
        }

        // Stop when close enough to the final destination.
        let final_wp = *agent.current_path.last().expect("non-empty");
        let to_final = Vec3::new(
            final_wp.x - transform.translation.x,
            0.0,
            final_wp.z - transform.translation.z,
        );
        if to_final.length() <= agent.stopping_distance {
            agent.current_path.clear();
            agent.path_index = 0;
            agent.status = NavMeshAgentStatus::Arrived;
            continue;
        }

        // Advance past any waypoints the agent is already on top of.
        while agent.path_index < agent.current_path.len() {
            let wp = agent.current_path[agent.path_index];
            let to_wp = Vec3::new(
                wp.x - transform.translation.x,
                0.0,
                wp.z - transform.translation.z,
            );
            if to_wp.length() < agent.speed * dt * 1.5 {
                agent.path_index += 1;
            } else {
                break;
            }
        }

        let Some(&wp) = agent.current_path.get(agent.path_index) else {
            continue;
        };

        let to_wp = Vec3::new(
            wp.x - transform.translation.x,
            0.0,
            wp.z - transform.translation.z,
        );
        let dist = to_wp.length();
        if dist > 1e-4 {
            let dir = to_wp / dist;
            let step = (agent.speed * dt).min(dist);
            transform.translation.x += dir.x * step;
            transform.translation.z += dir.z * step;
        }
    }

    // A small deterministic separation pass prevents proving-game agents
    // from converging on the same waypoint and occupying one position.
    let snapshots = agents
        .iter_mut()
        .map(|(entity, (agent, transform))| {
            (
                entity,
                transform.translation,
                agent.avoidance_radius.max(0.0),
            )
        })
        .collect::<Vec<_>>();
    let mut corrections = BTreeMap::new();
    for first_index in 0..snapshots.len() {
        for second_index in (first_index + 1)..snapshots.len() {
            let (first, first_position, first_radius) = snapshots[first_index];
            let (second, second_position, second_radius) = snapshots[second_index];
            let delta = Vec3::new(
                first_position.x - second_position.x,
                0.0,
                first_position.z - second_position.z,
            );
            let minimum = first_radius + second_radius;
            let distance = delta.length();
            if distance >= minimum || minimum <= 0.0 {
                continue;
            }
            let direction = if distance > 1e-5 {
                delta / distance
            } else if first <= second {
                Vec3::X
            } else {
                -Vec3::X
            };
            let correction = direction * ((minimum - distance) * 0.5);
            *corrections.entry(first).or_insert(Vec3::ZERO) += correction;
            *corrections.entry(second).or_insert(Vec3::ZERO) -= correction;
        }
    }
    for (entity, (_agent, transform)) in agents.iter_mut() {
        if let Some(correction) = corrections.get(&entity) {
            transform.translation += *correction;
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Describes why a [`NavMesh`] operation failed.
#[derive(Debug)]
pub enum NavMeshError {
    /// An I/O error during read or write.
    Io(io::Error),
    /// The navmesh JSON could not be parsed or serialised.
    Json(serde_json::Error),
    /// The loaded navmesh has an inconsistent cell count.
    InvalidGrid,
}

impl fmt::Display for NavMeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "navmesh I/O error: {e}"),
            Self::Json(e) => write!(f, "navmesh JSON error: {e}"),
            Self::InvalidGrid => write!(f, "navmesh cell count does not match declared dimensions"),
        }
    }
}

impl std::error::Error for NavMeshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::InvalidGrid => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_mesh(cols: usize, rows: usize) -> NavMesh {
        NavMesh {
            origin_x: 0.0,
            origin_z: 0.0,
            cols,
            rows,
            cell_size: 1.0,
            walkable: vec![true; cols * rows],
        }
    }

    fn mesh_with_wall() -> NavMesh {
        // 5x5 grid, column 3 blocked for rows 0-3
        // Layout (col 0..4, row 0..4):
        //   row 0: W W W X W
        //   row 1: W W W X W
        //   row 2: W W W X W
        //   row 3: W W W X W
        //   row 4: W W W W W   <- gap at bottom
        let cols = 5;
        let rows = 5;
        let mut walkable = vec![true; cols * rows];
        for row in 0..4 {
            walkable[row * cols + 3] = false;
        }
        NavMesh {
            origin_x: 0.0,
            origin_z: 0.0,
            cols,
            rows,
            cell_size: 1.0,
            walkable,
        }
    }

    #[test]
    fn bake_marks_obstacle_cells_blocked() {
        let settings = NavMeshSettings {
            cell_size: 1.0,
            agent_radius: 0.0,
            world_min: Vec3::new(0.0, 0.0, 0.0),
            world_max: Vec3::new(5.0, 0.0, 5.0),
            ..NavMeshSettings::default()
        };
        let obstacles = vec![(Vec3::new(2.5, 0.0, 2.5), Vec3::new(0.5, 0.0, 0.5))];
        let nm = bake_from_obstacles(&obstacles, &settings);
        assert_eq!(nm.cols, 5);
        assert_eq!(nm.rows, 5);
        assert!(!nm.is_walkable(2, 2), "center cell must be blocked");
        assert!(nm.is_walkable(0, 0), "corner must be walkable");
    }

    #[test]
    fn bake_expands_obstacle_by_agent_radius() {
        let settings = NavMeshSettings {
            cell_size: 1.0,
            agent_radius: 1.0,
            world_min: Vec3::new(0.0, 0.0, 0.0),
            world_max: Vec3::new(7.0, 0.0, 7.0),
            ..NavMeshSettings::default()
        };
        let obstacles = vec![(Vec3::new(3.5, 0.0, 3.5), Vec3::new(0.5, 0.0, 0.5))];
        let nm = bake_from_obstacles(&obstacles, &settings);
        assert!(!nm.is_walkable(3, 3), "obstacle cell blocked");
        assert!(!nm.is_walkable(2, 3), "expanded left cell blocked");
        assert!(!nm.is_walkable(4, 3), "expanded right cell blocked");
        assert!(nm.is_walkable(0, 0), "far corner walkable");
    }

    #[test]
    fn astar_finds_direct_path_on_open_grid() {
        let nm = flat_mesh(5, 5);
        let path = astar(&nm, 0, 0, 4, 4);
        assert!(path.is_some(), "path must exist on open grid");
        let path = path.unwrap();
        assert_eq!(*path.first().unwrap(), (0, 0));
        assert_eq!(*path.last().unwrap(), (4, 4));
    }

    #[test]
    fn astar_returns_single_cell_for_same_start_end() {
        let nm = flat_mesh(3, 3);
        let path = astar(&nm, 1, 1, 1, 1).unwrap();
        assert_eq!(path, vec![(1, 1)]);
    }

    #[test]
    fn astar_routes_around_wall() {
        let nm = mesh_with_wall();
        let path = astar(&nm, 0, 0, 4, 0);
        assert!(path.is_some(), "path must exist around the wall");
        let path = path.unwrap();
        // Path must not pass through col 3 for rows 0-3
        for &(col, row) in &path {
            if row < 4 {
                assert_ne!(col, 3, "path passed through blocked cell ({col},{row})");
            }
        }
        assert_eq!(*path.last().unwrap(), (4, 0));
    }

    #[test]
    fn astar_returns_none_when_fully_blocked() {
        let nm = NavMesh {
            origin_x: 0.0,
            origin_z: 0.0,
            cols: 3,
            rows: 1,
            cell_size: 1.0,
            walkable: vec![true, false, true],
        };
        // start=0, end=2, but the only path through col 1 is blocked
        let path = astar(&nm, 0, 0, 2, 0);
        assert!(path.is_none());
    }

    #[test]
    fn astar_returns_none_for_blocked_start() {
        let nm = NavMesh {
            origin_x: 0.0,
            origin_z: 0.0,
            cols: 2,
            rows: 1,
            cell_size: 1.0,
            walkable: vec![false, true],
        };
        assert!(astar(&nm, 0, 0, 1, 0).is_none());
    }

    #[test]
    fn find_path_translates_cells_to_world_positions() {
        let nm = flat_mesh(3, 3);
        let query = NavMeshQuery::new(nm);
        let path = query.find_path(Vec3::new(0.5, 5.0, 0.5), Vec3::new(2.5, 5.0, 2.5));
        let path = path.expect("path must exist");
        assert_eq!(path.first().unwrap().y, 5.0, "Y preserved from start");
        let last = *path.last().unwrap();
        assert!((last.x - 2.5).abs() < 1.0, "last waypoint near end X");
        assert!((last.z - 2.5).abs() < 1.0, "last waypoint near end Z");
    }

    #[test]
    fn find_path_returns_none_outside_grid() {
        let nm = flat_mesh(3, 3);
        let query = NavMeshQuery::new(nm);
        let path = query.find_path(Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.5, 0.0, 1.5));
        assert!(path.is_none(), "start outside grid must return None");
    }

    #[test]
    fn save_load_roundtrip() {
        let path = std::env::temp_dir().join(format!("engine_navmesh_roundtrip_{}.bin", std::process::id()));
        let nm = flat_mesh(4, 4);
        save_navmesh(&nm, &path).expect("save must succeed");
        let loaded = load_navmesh(&path).expect("load must succeed");
        assert_eq!(loaded.cols, 4);
        assert_eq!(loaded.rows, 4);
        assert_eq!(loaded.walkable, nm.walkable);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_mismatched_cell_count() {
        let path = std::env::temp_dir().join(format!("engine_navmesh_bad_{}.bin", std::process::id()));
        let json = r#"{"origin_x":0,"origin_z":0,"cols":3,"rows":3,"cell_size":1.0,"walkable":[true,false]}"#;
        std::fs::write(&path, json).unwrap();
        let err = load_navmesh(&path).expect_err("must reject mismatched cell count");
        assert!(matches!(err, NavMeshError::InvalidGrid));
        let _ = std::fs::remove_file(&path);
    }
}
