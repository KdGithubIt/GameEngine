//! Deterministic renderer and authoring budgets shared by import/runtime layers.
//!
//! These are content ceilings rather than device capability queries, so lower
//! crates can reject assets consistently without depending on the high-level
//! scene composition crate.

/// Largest accepted width or height for a decoded source texture.
pub const MAX_TEXTURE_DIMENSION: u32 = 8_192;
/// Maximum live pool authored on one particle emitter.
pub const MAX_PARTICLES_PER_EMITTER: usize = 65_536;
/// Maximum worst-case render instances across meshes and particle pools.
pub const MAX_RENDER_INSTANCES: usize = 100_000;
/// Material v2 exposes exactly these base/normal/emissive texture slots.
pub const MATERIAL_TEXTURE_SLOTS: usize = 3;
/// The renderer mirrors only one directional light resource.
pub const MAX_DIRECTIONAL_LIGHTS: usize = 1;
/// The renderer mirrors only one ambient light resource.
pub const MAX_AMBIENT_LIGHTS: usize = 1;
