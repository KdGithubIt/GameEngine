//! Advanced geometry facade with screen-space query adapters kept at composition level.

pub use engine_physics::advanced_geometry::*;

#[doc(hidden)]
mod core {
    pub use engine_physics::advanced_geometry::{StaticTriangleMesh, TriangleMeshRayHit};
}
mod spatial_query;

pub use spatial_query::*;
