//! Render-side tracking for runtime morph vertex uploads.

/// Records which vertices a render-side morph blend changed this frame.
#[derive(Debug, Clone, Default)]
pub struct MorphDirtyVertices {
    /// Changed vertex indices, sorted ascending.
    pub changed: Vec<u32>,
}
