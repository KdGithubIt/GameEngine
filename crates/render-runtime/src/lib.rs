//! Runtime rendering assets and presentation data above the low-level renderer.
//!
//! The default `gpu` feature exposes the complete runtime renderer. Consumers
//! that only need CPU mesh contracts and deterministic render limits may
//! disable default features; this keeps `wgpu` and the runtime presentation
//! stack out of their dependency graph.

#![warn(missing_docs)]

/// Camera projection, viewport, and active-camera selection contracts.
#[cfg(feature = "gpu")]
pub mod camera;
/// Immediate-mode runtime debug line presentation.
#[cfg(feature = "gpu")]
pub mod debug_draw;
#[cfg(feature = "gpu")]
mod bloom;
#[cfg(feature = "gpu")]
mod environment;
/// Runtime light resources and authored light mirroring.
#[cfg(feature = "gpu")]
pub mod light;
/// Level-of-detail selection and render-instancing statistics.
#[cfg(feature = "gpu")]
pub mod lod;
/// Runtime material, texture, and shading contracts.
#[cfg(feature = "gpu")]
pub mod material;
/// CPU/GPU mesh contracts used by render systems and import adapters.
#[cfg(feature = "gpu")]
pub mod mesh;
/// CPU mesh contracts used by importers without a GPU backend.
#[cfg(not(feature = "gpu"))]
#[path = "mesh_contract.rs"]
pub mod mesh;
// Compile the contract-only implementation in normal GPU builds as well so
// formatting, clippy, and type checking cannot silently rot behind cfg.
#[cfg(feature = "gpu")]
#[path = "mesh_contract.rs"]
#[allow(dead_code)]
mod mesh_contract_compile_check;
/// Render-side tracking for runtime morph vertex uploads.
#[cfg(feature = "gpu")]
pub mod morph;
/// CPU particle simulation rendered through runtime mesh instancing.
#[cfg(feature = "gpu")]
pub mod particles;
/// Backend-neutral compiled VFX CPU reference runtime (ADR 0125).
#[cfg(feature = "gpu")]
pub mod vfx;
/// HDR, bloom, tone-mapping, and color-grading settings.
#[cfg(feature = "gpu")]
pub mod postprocess;
/// Offscreen rendering helpers for editor integrations.
#[cfg(feature = "gpu")]
pub mod preview;
/// Runtime world renderer and tone-map facade.
#[cfg(feature = "gpu")]
pub mod renderer;
#[cfg(feature = "gpu")]
mod render_backend;
#[cfg(feature = "gpu")]
mod temporal;
/// Deterministic renderer and import-time content budgets.
pub mod render_limits;
/// Shadow mapping and environment-lighting runtime contracts.
#[cfg(feature = "gpu")]
pub mod shadow;

/// Compatibility namespace for low-level asset identity used by render assets.
#[cfg(feature = "gpu")]
#[doc(hidden)]
pub mod asset {
    pub use engine_assets::asset::*;
}
/// Compatibility namespace for frame timing used by presentation simulation.
#[cfg(feature = "gpu")]
#[doc(hidden)]
pub mod time {
    pub use engine_core::time::*;
}
/// Compatibility namespace for rig transforms used by presentation systems.
#[cfg(feature = "gpu")]
#[doc(hidden)]
pub mod transform {
    pub use engine_rig::transform::*;
}
