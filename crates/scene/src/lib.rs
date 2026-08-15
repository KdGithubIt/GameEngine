//! Scene persistence, replay, and runtime scene lifecycle contracts.

#![warn(missing_docs)]

/// Versioned deterministic virtual-input recording and playback.
pub mod replay;
/// Save data, slot persistence, and queued persistence effects.
pub mod save;
/// Authoring-scene loading from project-relative asset paths.
pub mod scene_loader;
/// Runtime scene-switch request state and lifecycle bookkeeping.
pub mod scene_manager;
