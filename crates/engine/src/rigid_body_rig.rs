//! Public rigid-body rig facade owned by `engine-rig` (ADR 0111).
//!
//! This path remains part of the supported `engine` umbrella API while the
//! concrete rig data lives in the lower `engine-rig` crate.

pub use engine_rig::rigid_body_rig::*;
