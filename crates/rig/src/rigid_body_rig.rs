//! Engine-native secondary-motion rig data (ADR 0112).
//!
//! The types in this module are intentionally source-format and solver
//! independent. Importers may convert external rigid-body metadata into this
//! representation, while runtime simulation is owned by `engine-physics`.

use crate::skeleton_asset::{BoneId, SkeletonIdentity};
use engine_authoring::id::AssetId;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// Current serialized schema version of a [`SecondaryMotionRigAsset`].
pub const SECONDARY_MOTION_RIG_SCHEMA_VERSION: u32 = 1;

/// Collision volume attached to one secondary-motion body.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "camelCase")]
pub enum RigidBodyShape {
    /// A sphere of the given radius, in meters.
    Sphere {
        /// Sphere radius.
        radius: f32,
    },
    /// A box with the given half-extents, in meters, on the body's local axes.
    Box {
        /// Half-extent on each local axis.
        half_extents: [f32; 3],
    },
    /// A capsule aligned with the body's local Y axis.
    Capsule {
        /// Cross-section radius, in meters.
        radius: f32,
        /// Half the length of the cylindrical section, excluding caps.
        half_height: f32,
    },
}

/// How a secondary-motion body relates to its bound bone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RigidBodyMode {
    /// The resolved pre-physics pose drives the body.
    #[default]
    FollowBone,
    /// Simulation drives both position and rotation of the bound bone.
    Dynamic,
    /// Simulation drives rotation while the resolved pose keeps bone position.
    DynamicWithBonePosition,
}

/// One body in a [`SecondaryMotionRigAsset`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RigidBodyDef {
    /// Human-readable name for diagnostics and authoring UI.
    pub name: String,
    /// Stable bone identity within [`SecondaryMotionRigAsset::skeleton`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<BoneId>,
    /// Bone name captured when the binding was authored or imported.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bone_name: String,
    /// Collision volume.
    pub shape: RigidBodyShape,
    /// Rest position relative to the bound bone, in meters.
    pub bone_offset_translation: [f32; 3],
    /// Rest rotation relative to the bound bone, as `(x, y, z, w)`.
    pub bone_offset_rotation: [f32; 4],
    /// Mass in kilograms. Ignored by [`RigidBodyMode::FollowBone`].
    pub mass: f32,
    /// Non-negative linear velocity damping rate in inverse seconds.
    pub linear_damping: f32,
    /// Non-negative angular velocity damping rate in inverse seconds.
    pub angular_damping: f32,
    /// Multiplier applied to the secondary-motion world's gravity.
    pub gravity_scale: f32,
    /// Restitution in `0..=1`.
    pub restitution: f32,
    /// Coulomb friction coefficient.
    pub friction: f32,
    /// Relationship between simulation and the bound bone.
    pub mode: RigidBodyMode,
    /// Collision group index used only inside this rig's isolated world.
    pub group: u8,
    /// Bitmask of groups this body collides with inside the isolated world.
    pub collides_with: u16,
}

impl RigidBodyDef {
    /// Returns this body's rest offset from its bound bone as engine math types.
    pub fn bone_offset(&self) -> (Vec3, Quat) {
        (
            Vec3::from_array(self.bone_offset_translation),
            Quat::from_array(self.bone_offset_rotation),
        )
    }
}

/// One six-degree-of-freedom spring constraint between two bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JointDef {
    /// Human-readable constraint name.
    pub name: String,
    /// Index into [`SecondaryMotionRigAsset::bodies`] of the first body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_a: Option<usize>,
    /// Index into [`SecondaryMotionRigAsset::bodies`] of the second body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b: Option<usize>,
    /// Constraint-frame position in model space, in meters.
    pub translation: [f32; 3],
    /// Constraint-frame rotation as `(x, y, z, w)`.
    pub rotation: [f32; 4],
    /// Lower translation limit per axis, in meters.
    pub translation_lower: [f32; 3],
    /// Upper translation limit per axis, in meters.
    pub translation_upper: [f32; 3],
    /// Lower angular limit per axis, in radians.
    pub rotation_lower: [f32; 3],
    /// Upper angular limit per axis, in radians.
    pub rotation_upper: [f32; 3],
    /// Translation spring stiffness per axis; zero disables that spring.
    pub spring_translation: [f32; 3],
    /// Angular spring stiffness per axis; zero disables that spring.
    pub spring_rotation: [f32; 3],
}

/// Editable engine-native secondary-motion rig.
///
/// Imported PMX rigid bodies and joints are merely one producer of this asset;
/// the runtime contract does not preserve source-format or solver identities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecondaryMotionRigAsset {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Stable asset identity.
    pub id: AssetId,
    /// Human-readable asset name.
    pub name: String,
    /// Skeleton whose stable [`BoneId`] values body bindings reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetId>,
    /// Skeleton structure identity captured when this rig was built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton_identity: Option<SkeletonIdentity>,
    /// Bodies in stable authoring order; constraints index this list.
    pub bodies: Vec<RigidBodyDef>,
    /// Constraints between pairs of [`Self::bodies`].
    pub joints: Vec<JointDef>,
}

impl SecondaryMotionRigAsset {
    /// Returns how many bodies have solver-owned motion.
    pub fn dynamic_body_count(&self) -> usize {
        self.bodies
            .iter()
            .filter(|body| body.mode != RigidBodyMode::FollowBone)
            .count()
    }
}

/// Opts one rigged entity into engine-native secondary motion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryMotion {
    /// Secondary-motion rig asset to simulate.
    pub rig: AssetId,
}

impl SecondaryMotion {
    /// Creates an activation component referencing `rig`.
    pub fn new(rig: AssetId) -> Self {
        Self { rig }
    }
}

/// Runtime lookup from stable asset IDs to secondary-motion rig definitions.
#[derive(Debug, Default)]
pub struct SecondaryMotionRigRegistry {
    by_id: hashbrown::HashMap<AssetId, SecondaryMotionRigAsset>,
}

impl SecondaryMotionRigRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces one rig definition.
    pub fn insert(&mut self, asset: SecondaryMotionRigAsset) {
        self.by_id.insert(asset.id.clone(), asset);
    }

    /// Removes and returns the rig registered under `id`.
    pub fn remove(&mut self, id: &AssetId) -> Option<SecondaryMotionRigAsset> {
        self.by_id.remove(id)
    }

    /// Returns the rig registered under `id`.
    pub fn get(&self, id: &AssetId) -> Option<&SecondaryMotionRigAsset> {
        self.by_id.get(id)
    }

    /// Returns the number of registered rigs.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Returns whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

// Temporary migration aliases removed by the ADR 0112 authoring-schema cutover.
#[doc(hidden)]
pub type RigidBodyRigAsset = SecondaryMotionRigAsset;
#[doc(hidden)]
pub type RigidBodyPhysics = SecondaryMotion;
#[doc(hidden)]
pub type RigidBodyRigRegistry = SecondaryMotionRigRegistry;
#[doc(hidden)]
pub const RIGID_BODY_RIG_SCHEMA_VERSION: u32 = SECONDARY_MOTION_RIG_SCHEMA_VERSION;

#[cfg(test)]
mod tests {
    use super::*;

    fn body(mode: RigidBodyMode) -> RigidBodyDef {
        RigidBodyDef {
            name: "hair".to_owned(),
            bone: Some(BoneId(7)),
            bone_name: "hair_01".to_owned(),
            shape: RigidBodyShape::Capsule {
                radius: 0.02,
                half_height: 0.05,
            },
            bone_offset_translation: [0.0, 0.1, 0.0],
            bone_offset_rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.5,
            linear_damping: 3.0,
            angular_damping: 4.0,
            gravity_scale: 1.0,
            restitution: 0.0,
            friction: 0.5,
            mode,
            group: 1,
            collides_with: u16::MAX,
        }
    }

    fn rig() -> SecondaryMotionRigAsset {
        SecondaryMotionRigAsset {
            schema_version: SECONDARY_MOTION_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "secondary motion".to_owned(),
            skeleton: Some(AssetId::generate()),
            skeleton_identity: Some(SkeletonIdentity(0x1234)),
            bodies: vec![
                body(RigidBodyMode::FollowBone),
                body(RigidBodyMode::Dynamic),
            ],
            joints: Vec::new(),
        }
    }

    #[test]
    fn current_secondary_motion_schema_round_trips() {
        let rig = rig();
        let json = serde_json::to_string(&rig).expect("rig must serialize");
        let restored: SecondaryMotionRigAsset =
            serde_json::from_str(&json).expect("rig must deserialize");
        assert_eq!(rig, restored);
    }

    #[test]
    fn missing_current_gravity_scale_is_rejected() {
        let rig = rig();
        let mut value = serde_json::to_value(rig).expect("rig must serialize");
        value["bodies"][0]
            .as_object_mut()
            .expect("body must be an object")
            .remove("gravityScale");
        assert!(serde_json::from_value::<SecondaryMotionRigAsset>(value).is_err());
    }

    #[test]
    fn dynamic_body_count_ignores_follow_bodies() {
        assert_eq!(rig().dynamic_body_count(), 1);
    }
}
