//! Thin Editor facade over the shared ADR 0124 production navigation bake service.
//!
//! The GUI owns workflow state only. Scene collection, deterministic fingerprints,
//! staleness, cancellation, baking, safe replacement, and statistics are owned by
//! `engine::navigation_bake` and `engine-physics`.

pub use engine::navigation_bake::{
    bake_scene_navmesh, collect_navigation_source, is_scene_navmesh_stale, prepare_scene_navmesh,
    NavMeshBakeDocument, NavMeshBakeError, NavMeshBakeResult, NavMeshBakeSettings,
    NavigationBakeOutput, NavigationBakeService, NavigationBakeServiceError, NavigationBakeStats,
    NavigationSource, NAVMESH_BAKE_SCHEMA_VERSION,
};
