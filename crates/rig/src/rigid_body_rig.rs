//! Secondary-motion rigid-body rigs and the component that activates one
//! (ADR 0097 §6).
//!
//! An MMD character's hair, skirt, and accessories move because its author
//! tuned a rig of rigid bodies and six-degree-of-freedom spring joints in
//! their own tool. This module is where that authored rig lives once it has
//! been imported: a [`RigidBodyRigAsset`] is an engine-native, serializable
//! description of the bodies and constraints, produced by
//! [`crate::pmx_import`] at authoring time and readable on every target
//! afterwards.
//!
//! # Why the asset is engine-native
//!
//! ADR 0096 §1 draws the line this module sits on: the *importer* is
//! desktop-only and depends on an MMD parser, but the *physics bridge* that
//! consumes this data has to run everywhere the game does, wasm32 included.
//! Keeping the rig in an engine-native form — plain [`glam`] vectors,
//! [`crate::skeleton_asset::BoneId`] bone references, serde-serializable like
//! any other sub-asset — is what lets the bridge read it without ever
//! linking `mmd-anim-format`.
//!
//! # Runtime bridge
//!
//! This module owns the serialized data and activation marker. The isolated
//! Rapier solver lives in [`crate::mmd_physics`], which reads both values on
//! the fixed schedule and writes simulated poses back to skeleton joints.
//!
//! # Activation
//!
//! Simulation is opt-in through a single marker component, mirroring
//! [`crate::foot_ik::FootIk`]'s established "present = active, absent =
//! no-op" pattern:
//!
//! ```text
//! Skinned Model entity
//!   + engine.rigid_body_physics { rig: <RigidBodyRig sub-asset> }
//! ```
//!
//! One component, one reference field, and no further configuration for the
//! common case — the PMX author already tuned every body and joint, so
//! re-exposing mass and damping as editable engine state would duplicate an
//! authoring surface that already exists elsewhere for no user-visible gain
//! (ADR 0097's Alternatives).

use crate::skeleton_asset::{BoneId, SkeletonIdentity};
use engine_authoring::id::AssetId;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// Serialized schema version of a [`RigidBodyRigAsset`] document.
///
/// Bumped only by a change that older readers could not interpret
/// correctly; additive fields carry `#[serde(default)]` instead.
pub const RIGID_BODY_RIG_SCHEMA_VERSION: u32 = 1;

/// The collision volume of one [`RigidBodyDef`].
///
/// Serialized as an internally tagged enum so a document stays readable and
/// diffable, and so a future shape is additive rather than a numbering
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "camelCase")]
pub enum RigidBodyShape {
    /// A sphere of the given radius, in meters.
    Sphere {
        /// Sphere radius.
        radius: f32,
    },
    /// A box with the given half-extents, in meters, on the body's own axes.
    Box {
        /// Half-extent on each local axis.
        half_extents: [f32; 3],
    },
    /// A capsule aligned with the body's local Y axis.
    Capsule {
        /// Cross-section radius, in meters.
        radius: f32,
        /// Half the length of the cylindrical section, excluding the caps.
        half_height: f32,
    },
}

/// How one [`RigidBodyDef`] relates to the bone it rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RigidBodyMode {
    /// Animation drives the body: it follows its bone exactly and pushes
    /// other bodies without being pushed back.
    ///
    /// The default because it is the mode that cannot misbehave — a body
    /// that should have simulated simply stays rigid, rather than a
    /// character coming apart.
    #[default]
    FollowBone,
    /// The body simulates freely and writes its position and orientation back
    /// to its bone.
    Dynamic,
    /// The body simulates its rotation while the bone keeps its
    /// animation/procedural position.
    ///
    /// After write-back, the solver body's position is realigned to that bone
    /// position while its simulated rotation is retained. This is PMX mode 2:
    /// the position alignment affects the next physics step as well as the
    /// visible skeleton.
    DynamicWithBonePosition,
}

/// One rigid body in a [`RigidBodyRigAsset`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RigidBodyDef {
    /// Human-readable name from the source document, for diagnostics.
    pub name: String,
    /// The bone this body rides, as a stable identity within
    /// [`RigidBodyRigAsset::skeleton`].
    ///
    /// `None` for a body attached to no bone, which the source format may
    /// permit; such a body still collides but drives nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<BoneId>,
    /// The bone's name at import time. Diagnostics and Inspector display
    /// only — every binding resolves through [`Self::bone`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bone_name: String,
    /// Collision volume.
    pub shape: RigidBodyShape,
    /// Rest position relative to [`Self::bone`], in meters.
    pub bone_offset_translation: [f32; 3],
    /// Rest rotation relative to [`Self::bone`], as `(x, y, z, w)`.
    pub bone_offset_rotation: [f32; 4],
    /// Mass in kilograms. Ignored for [`RigidBodyMode::FollowBone`].
    pub mass: f32,
    /// Linear velocity damping factor.
    pub linear_damping: f32,
    /// Angular velocity damping factor.
    pub angular_damping: f32,
    /// Bounciness in `0..=1`.
    pub restitution: f32,
    /// Coulomb friction coefficient.
    pub friction: f32,
    /// How this body relates to its bone.
    #[serde(default)]
    pub mode: RigidBodyMode,
    /// Collision group index this body belongs to.
    pub group: u8,
    /// Bitmask of groups this body collides with; bit *n* set means it
    /// collides with group *n*.
    pub collides_with: u16,
}

impl RigidBodyDef {
    /// Returns this body's rest offset from its bone as engine types.
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
    /// Human-readable name from the source document, for diagnostics.
    pub name: String,
    /// Index into [`RigidBodyRigAsset::bodies`] of the first body, or `None`
    /// when the source referenced a body that does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_a: Option<usize>,
    /// Index into [`RigidBodyRigAsset::bodies`] of the second body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_b: Option<usize>,
    /// Constraint frame position in the model's space, in meters.
    pub translation: [f32; 3],
    /// Constraint frame rotation, as `(x, y, z, w)`.
    pub rotation: [f32; 4],
    /// Lower translation limit per axis, in meters.
    pub translation_lower: [f32; 3],
    /// Upper translation limit per axis, in meters.
    pub translation_upper: [f32; 3],
    /// Lower rotation limit per axis, in radians.
    pub rotation_lower: [f32; 3],
    /// Upper rotation limit per axis, in radians.
    pub rotation_upper: [f32; 3],
    /// Per-axis translation spring stiffness; zero disables that axis'
    /// spring.
    pub spring_translation: [f32; 3],
    /// Per-axis rotation spring stiffness; zero disables that axis' spring.
    pub spring_rotation: [f32; 3],
}

/// An imported secondary-motion rig: the bodies and constraints a model's
/// author tuned (ADR 0097 §6).
///
/// Produced as an imported sub-asset
/// ([`crate::asset::ImportedSubAssetKind::RigidBodyRig`]) of the model source
/// that declared it, so its ID is stable across reimports exactly like a
/// mesh's or a skeleton's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RigidBodyRigAsset {
    /// Serialized schema version ([`RIGID_BODY_RIG_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Imported sub-asset ID.
    pub id: AssetId,
    /// Human-readable name from the source document.
    pub name: String,
    /// The skeleton whose [`BoneId`]s [`RigidBodyDef::bone`] refers to.
    ///
    /// `None` only for a rig imported from a source with no skeleton at all,
    /// in which case no body drives a bone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetId>,
    /// That skeleton's structure hash when this rig was imported, so a stale
    /// binding (the rig was reimported and its bones changed) is detectable
    /// instead of silently driving the wrong bones — the same guard
    /// [`crate::animation::AnimationClip`] carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton_identity: Option<SkeletonIdentity>,
    /// Bodies in source order; [`JointDef`] endpoints index into this list.
    pub bodies: Vec<RigidBodyDef>,
    /// Constraints between pairs of [`Self::bodies`].
    pub joints: Vec<JointDef>,
}

impl RigidBodyRigAsset {
    /// Returns the number of bodies that actually simulate.
    ///
    /// A rig where this is zero is inert: every body follows its bone, so
    /// running a solver over it could not change the pose.
    pub fn dynamic_body_count(&self) -> usize {
        self.bodies
            .iter()
            .filter(|body| body.mode != RigidBodyMode::FollowBone)
            .count()
    }
}

/// Opts one Skinned Model entity into simulating its imported rigid-body rig
/// (ADR 0097 §6).
///
/// Present means "simulate this rig"; absent means no secondary motion at
/// all. That is the whole authoring surface, mirroring
/// [`crate::foot_ik::FootIk`]: the source's author already tuned every body
/// and joint, so there is nothing left to configure per entity.
///
/// [`crate::mmd_physics::mmd_rigid_body_physics_system`] reads this marker;
/// missing rigs or incompatible skeletons degrade to a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigidBodyPhysics {
    /// The [`RigidBodyRigAsset`] sub-asset to simulate.
    pub rig: AssetId,
}

impl RigidBodyPhysics {
    /// Creates a marker referencing `rig`.
    pub fn new(rig: AssetId) -> Self {
        Self { rig }
    }
}

/// Runtime lookup from a rig's stable [`AssetId`] to its full
/// [`RigidBodyRigAsset`].
///
/// [`RigidBodyPhysics`] stores only the stable ID because it is
/// scene-persisted, so a system holding one needs somewhere to reach the
/// body and joint lists. This registry is that lookup, kept deliberately
/// narrow rather than folded into `Assets<RigidBodyRigAsset>` — exactly the
/// shape and rationale of [`crate::skeleton_asset::SkeletonAssetRegistry`].
#[derive(Debug, Default)]
pub struct RigidBodyRigRegistry {
    by_id: hashbrown::HashMap<AssetId, RigidBodyRigAsset>,
}

impl RigidBodyRigRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces the entry for `asset.id`.
    pub fn insert(&mut self, asset: RigidBodyRigAsset) {
        self.by_id.insert(asset.id.clone(), asset);
    }

    /// Removes the entry for `id`, returning it.
    ///
    /// Used to roll back entries added during a scene conversion that later
    /// fails atomically (`crate::scene_bridge::BridgeAssetState`).
    pub fn remove(&mut self, id: &AssetId) -> Option<RigidBodyRigAsset> {
        self.by_id.remove(id)
    }

    /// Returns the rig registered under `id`, if any.
    pub fn get(&self, id: &AssetId) -> Option<&RigidBodyRigAsset> {
        self.by_id.get(id)
    }

    /// Returns the number of registered rigs.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Returns `true` when no rigs are registered.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(name: &str, mode: RigidBodyMode) -> RigidBodyDef {
        RigidBodyDef {
            name: name.to_owned(),
            bone: Some(BoneId(7)),
            bone_name: "hair_01".to_owned(),
            shape: RigidBodyShape::Capsule {
                radius: 0.02,
                half_height: 0.05,
            },
            bone_offset_translation: [0.0, 0.1, 0.0],
            bone_offset_rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 0.5,
            linear_damping: 0.9,
            angular_damping: 0.9,
            restitution: 0.0,
            friction: 0.5,
            mode,
            group: 1,
            collides_with: 0xFFFF,
        }
    }

    fn rig() -> RigidBodyRigAsset {
        RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "character".to_owned(),
            skeleton: Some(AssetId::generate()),
            skeleton_identity: Some(SkeletonIdentity(0x1234)),
            bodies: vec![
                body("anchor", RigidBodyMode::FollowBone),
                body("hair", RigidBodyMode::Dynamic),
            ],
            joints: vec![JointDef {
                name: "hair_joint".to_owned(),
                body_a: Some(0),
                body_b: Some(1),
                translation: [0.0, 1.2, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                rotation_lower: [-0.5; 3],
                rotation_upper: [0.5; 3],
                spring_translation: [0.0; 3],
                spring_rotation: [10.0; 3],
            }],
        }
    }

    #[test]
    fn a_rig_round_trips_through_json() {
        let rig = rig();
        let json = serde_json::to_string(&rig).expect("rig must serialize");
        let restored: RigidBodyRigAsset =
            serde_json::from_str(&json).expect("rig must deserialize");
        assert_eq!(rig, restored);
    }

    #[test]
    fn only_simulating_bodies_are_counted_as_dynamic() {
        // A rig whose bodies all follow their bones cannot change a pose, so
        // a bridge can skip it entirely rather than build a solver for it.
        assert_eq!(rig().dynamic_body_count(), 1);
    }

    #[test]
    fn the_default_mode_is_the_one_that_cannot_misbehave() {
        // Guards the serde default: a document written before `mode` existed,
        // or one that omits it, must not silently start simulating.
        let json = serde_json::to_value(rig()).expect("rig must serialize");
        let mut body = json["bodies"][0].clone();
        body.as_object_mut().expect("body object").remove("mode");
        let restored: RigidBodyDef =
            serde_json::from_value(body).expect("body must deserialize without a mode");
        assert_eq!(restored.mode, RigidBodyMode::FollowBone);
    }

    #[test]
    fn the_registry_returns_and_removes_rigs_by_id() {
        let rig = rig();
        let id = rig.id.clone();
        let mut registry = RigidBodyRigRegistry::new();
        assert!(registry.is_empty());
        registry.insert(rig.clone());
        assert_eq!(registry.get(&id), Some(&rig));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.remove(&id), Some(rig));
        assert!(registry.is_empty());
    }
}
