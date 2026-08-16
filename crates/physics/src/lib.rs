//! Gameplay-physics primitives and optional solver backends.
//!
//! The default `solver` feature enables Rapier-backed collision and dynamics.
//! Contract-only consumers may disable default features to use collision event,
//! navigation, and character-state types without compiling Rapier.

#![warn(missing_docs)]

/// Multi-layer navigation and static triangle-geometry helpers.
pub mod advanced_geometry;
/// Kinematic character-controller state shared with high-level movement adapters.
#[cfg(feature = "solver")]
pub mod character_controller;
/// Kinematic character-controller contract for builds without a physics solver.
#[cfg(not(feature = "solver"))]
#[path = "character_controller_contract.rs"]
pub mod character_controller;
/// Collision primitives, filters, events, and world-space queries.
#[cfg(feature = "solver")]
pub mod collision;
/// Collision data contracts and AABB helpers for builds without a solver.
#[cfg(not(feature = "solver"))]
#[path = "collision_contract.rs"]
pub mod collision;
/// Production tiled polygon navigation assets, baking, queries, and agents.
pub mod navigation;
/// Legacy grid navigation retained for focused compatibility tests and layered utilities.
pub mod navmesh;
/// Rapier-backed gameplay rigid-body integration.
#[cfg(feature = "solver")]
pub mod physics;

// Keep the feature-off contract implementations in the normal compiler/Clippy
// path as well. Cargo feature unification can otherwise make an affected CI run
// exercise only the default solver implementation even though leaf consumers
// intentionally build these alternate contract modules.
#[cfg(feature = "solver")]
#[allow(dead_code)]
#[path = "character_controller_contract.rs"]
mod character_controller_contract_check;
#[cfg(feature = "solver")]
#[allow(dead_code)]
#[path = "collision_contract.rs"]
mod collision_contract_check;

/// Compatibility namespace for rig transforms used by migrated physics modules.
#[doc(hidden)]
pub mod transform {
    pub use engine_rig::transform::*;
}
/// Compatibility namespace for fixed-step time used by migrated physics modules.
#[doc(hidden)]
pub mod time {
    pub use engine_core::time::*;
}
