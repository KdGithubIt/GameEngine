//! Animation assets, evaluation, pose graphs, retargeting, and morph state.

#![warn(missing_docs)]

/// Clip assets, animator state, events, and fixed-step sampling.
pub mod animation;
/// Compiled Animation Graph state-machine runtime and transition evaluation.
pub mod anim_graph;
/// Typed animation parameter storage and blend sampling.
pub mod animation_parameters;
/// Offline ground-contact detection over animation clips.
pub mod contact_detect;
/// Runtime post-blend two-bone foot IK correction.
///
/// This module is available with the default `physics` feature. Consumers that
/// only need animation data, sampling, retargeting, or import-time contracts
/// may disable default features to avoid compiling the physics backend.
#[cfg(feature = "physics")]
pub mod foot_ik;
/// Humanoid profile validation, portable motion, and target-specific baking (ADR 0110).
pub mod humanoid;
/// Skeleton-independent humanoid motion conversion and target-specific bake/cache (ADR 0110).
pub mod humanoid_motion;
/// Animation-side morph assets, bindings, and weights.
pub mod morph;
/// Pure pose sampling and blending.
pub mod pose_graph;
/// Skeleton retargeting and baked-clip cache integration.
pub mod retarget;
/// Cross-crate acceptance fixtures, compiled only for tests or opt-in test consumers.
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_fixtures;

/// Compatibility namespace for low-level asset contracts used by migrated modules.
#[doc(hidden)]
pub mod asset {
    pub use engine_assets::asset::*;
}
/// Compatibility namespace for physics collision queries used by foot IK.
#[cfg(feature = "physics")]
#[doc(hidden)]
pub mod collision {
    pub use engine_physics::collision::*;
}
/// Compatibility namespace for the derived asset cache.
#[doc(hidden)]
pub mod derived_cache {
    pub use engine_assets::derived_cache::*;
}
/// Compatibility namespace for fixed-step time.
#[doc(hidden)]
pub mod time {
    pub use engine_core::time::*;
}
/// Compatibility namespace for rig poses.
#[doc(hidden)]
pub mod rig_pose {
    pub use engine_rig::rig_pose::*;
}
/// Compatibility namespace for stable skeleton data.
#[doc(hidden)]
pub mod skeleton_asset {
    pub use engine_rig::skeleton_asset::*;
}
/// Compatibility namespace for skin binding and runtime skeleton components.
#[doc(hidden)]
pub mod skinning {
    pub use engine_rig::skinning::*;
}
/// Compatibility namespace for local/global transforms.
#[doc(hidden)]
pub mod transform {
    pub use engine_rig::transform::*;
}
