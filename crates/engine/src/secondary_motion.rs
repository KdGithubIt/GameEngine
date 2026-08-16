//! Engine-native secondary-motion facade (ADR 0112).

pub use engine_physics::secondary_motion::{
    secondary_motion_presentation_system, secondary_motion_system, SecondaryMotionWorlds,
};
pub use engine_rig::rigid_body_rig::{
    JointDef, RigidBodyDef, RigidBodyMode, RigidBodyShape, SecondaryMotion,
    SecondaryMotionRigAsset, SecondaryMotionRigRegistry, SECONDARY_MOTION_RIG_SCHEMA_VERSION,
};
