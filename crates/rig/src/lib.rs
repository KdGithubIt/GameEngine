//! Low-level runtime rig primitives shared by engine systems.
//!
//! This crate owns transform hierarchy data, skeleton identity, skin binding,
//! layered rig poses, and imported rigid-body rig descriptions. It deliberately
//! excludes rendering, windowing, audio, model importers, and physics solvers so
//! changes to these primitives can compile and validate without the full
//! high-level `engine` dependency graph.

#![warn(missing_docs)]

/// Imported secondary-motion rigid-body rig data.
pub mod rigid_body_rig;
/// Layered local-space rig poses and deterministic world-space evaluation.
pub mod rig_pose;
/// Stable skeleton assets and bone identity.
pub mod skeleton_asset;
/// Skin binding, rig spawning, and joint palette computation.
pub mod skinning;
/// Local/global transforms and hierarchy propagation.
pub mod transform;
