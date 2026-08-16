//! Format importers and format-independent model build pipeline.
//!
//! This crate owns source parsing and conversion into runtime asset contracts.
//! It depends on the lower asset/rig/animation/render domains and never on the
//! high-level `engine` composition crate.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Format-independent model intermediate representation.
pub mod model_ir;
/// Format-independent runtime-asset builder.
pub mod model_import;
/// Humanoid profiles and portable motion variants derived from canonical Native model data.
pub mod humanoid_import;
/// glTF / GLB parser and importer.
pub mod gltf_import;
/// Imported-model prefab generation.
pub mod gltf_prefab;
/// FBX parser and importer.
#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
pub mod fbx_import;
/// PMX model parser and importer.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub mod pmx_import;
/// VMD motion parser and bake pipeline.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub mod vmd_import;
/// Cross-crate acceptance fixtures, compiled only for tests or opt-in test consumers.
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_fixtures;

/// Compatibility namespace for animation contracts used by importers.
#[doc(hidden)]
pub mod animation {
    pub use engine_animation::animation::*;
}
/// Compatibility namespace for offline animation contact detection.
#[doc(hidden)]
pub mod contact_detect {
    pub use engine_animation::contact_detect::*;
}
/// Compatibility namespace for runtime asset metadata used by importers.
#[doc(hidden)]
pub mod asset {
    pub use engine_assets::asset::*;
}
/// Compatibility namespace for the derived-cache contract used by VMD baking.
#[doc(hidden)]
pub mod derived_cache {
    pub use engine_assets::derived_cache::*;
}
/// Compatibility namespace for CPU mesh contracts used by importers.
#[doc(hidden)]
pub mod mesh {
    pub use engine_render_runtime::mesh::*;
}
/// Compatibility namespace for animation-side morph assets.
#[doc(hidden)]
pub mod morph {
    pub use engine_animation::morph::*;
}
/// Compatibility namespace for renderer/import content budgets.
#[doc(hidden)]
pub mod render_limits {
    pub use engine_render_runtime::render_limits::*;
}
/// Compatibility namespace for animation retargeting used by VMD baking.
#[doc(hidden)]
pub mod retarget {
    pub use engine_animation::retarget::*;
}
/// Compatibility namespace for imported rigid-body rig data.
#[doc(hidden)]
pub mod rigid_body_rig {
    pub use engine_rig::rigid_body_rig::*;
}
/// Compatibility namespace for stable skeleton identity.
#[doc(hidden)]
pub mod skeleton_asset {
    pub use engine_rig::skeleton_asset::*;
}
/// Compatibility namespace for skinning limits and skeleton descriptors.
#[doc(hidden)]
pub mod skinning {
    pub use engine_rig::skinning::*;
}

/// Authoring component identifiers consumed by generated imported prefabs.
///
/// These strings are schema identifiers rather than runtime scene behavior;
/// keeping the compatibility namespace here prevents importers from depending
/// on the high-level scene bridge.
#[doc(hidden)]
pub mod scene_bridge {
    /// Neutral fallback material used when a primitive has no material.
    pub const BUILTIN_WHITE_MATERIAL_ASSET_ID: &str = "asset_01JP0000000000000000000203";
    /// Unified animation-controller authoring component ID.
    pub const ANIMATION_CONTROLLER_COMPONENT: &str = "engine.animation_controller";
    /// Static mesh renderer authoring component ID.
    pub const STATIC_MESH_RENDERER_COMPONENT: &str = "engine.static_mesh_renderer";
    /// Skinned mesh renderer authoring component ID.
    pub const SKINNED_MESH_RENDERER_COMPONENT: &str = "engine.skinned_mesh_renderer";
    /// Skinned model authoring component ID.
    pub const SKINNED_MODEL_COMPONENT: &str = "engine.skinned_model";
    /// Transform authoring component ID.
    pub const TRANSFORM_COMPONENT: &str = "engine.transform";
}

/// Temporary compatibility namespace for the PMX authoring-unit scale test.
///
/// The high-level facade retains a second test against the real physics bridge,
/// so moving the importer does not erase the cross-domain drift guard.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod mmd_physics {
    /// PMX authored-unit scale mirrored only for importer-local regression tests.
    #[doc(hidden)]
    pub const PMX_AUTHORING_SCALE: f32 = 0.08;
}
