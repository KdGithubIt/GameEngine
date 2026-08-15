//! Rig pose, skin binding, and joint palette computation (Phase 48,
//! ADR 0043; revised by ADR 0074, ADR 0077, and ADR 0086).
//!
//! [`Skeleton`] is the *rig*: one joint entity per bone of a
//! [`SkeletonAsset`], addressed by [`BoneId`]. [`SkinnedMesh`] is the
//! *binding* of one drawn mesh to that rig, carrying the skin-local joint
//! order and inverse bind matrices. Separating them lets several skins with
//! different joint orders share one joint hierarchy (ADR 0086).
//!
//! Joint world matrices come from the transform hierarchy
//! ([`crate::transform::transform_propagation_system`]), so register
//! [`joint_palette_system`] after transform propagation in the frame
//! schedule. The palette produced here is uploaded by the skinned render
//! path as a uniform array of [`MAX_JOINTS`] matrices.

use std::fmt;

use engine_authoring::id::AssetId;
use engine_ecs::{Entity, Query, World, WorldError};
use glam::{Mat4, Quat, Vec3};
use hashbrown::HashMap;

use crate::rig_pose::RigPose;
use crate::skeleton_asset::{BoneId, SkeletonAsset};
use crate::transform::{Children, GlobalTransform, Parent, Transform};

/// Maximum joints in one skin binding, and therefore in one uploaded palette.
///
/// 128 matrices occupy 8 KiB, within the WebGPU / WebGL2 minimum uniform
/// buffer size, so the WASM target needs no storage-buffer fallback
/// (ADR 0043). The cap bounds a single [`SkinnedMesh`] binding, not a
/// [`Skeleton`]: a rig may carry more bones than this as long as no one skin
/// binds more than [`MAX_JOINTS`] of them (ADR 0086 §4).
pub const MAX_JOINTS: usize = 128;

/// The runtime pose of one rig: a joint entity per bone of a
/// [`SkeletonAsset`] (ADR 0086).
///
/// `joints` and `bone_ids` share the asset's bone order (parent before
/// child), and cover *every* bone of the asset — including bones no skin
/// references, which exist so clips and attachments can address them.
///
/// Any number of [`SkinnedMesh`] bindings may reference the same rig entity;
/// each resolves its own skin joints through [`Self::bone_ids`], so skins
/// with different joint orders or joint subsets share one hierarchy.
pub struct Skeleton {
    /// Joint entities in [`SkeletonAsset::bones`] order.
    pub joints: Vec<Entity>,
    /// [`BoneId`] per joint, same order as [`Self::joints`] (ADR 0077).
    ///
    /// [`crate::animation::animation_system`] builds a `BoneId -> joint
    /// index` map from this list to resolve
    /// [`crate::animation::AnimChannel::target_bone`], and
    /// [`joint_palette_system`] resolves [`SkinnedMesh::joint_bones`]
    /// through it. A rig spawned without bone identity leaves it empty, and
    /// every binding against that rig then draws its bind pose.
    pub bone_ids: Vec<BoneId>,
    /// The [`SkeletonAsset`] these joints were spawned from, when known
    /// (ADR 0077).
    pub asset: Option<AssetId>,
}

impl Skeleton {
    /// Returns the joint entity driving `bone`, if this rig carries it.
    ///
    /// The only supported way for an engine system to turn a persisted
    /// [`BoneId`] into something it can read a transform from (ADR 0088 §3).
    /// Game modules do not get this: they declare an
    /// `engine.bone_attachment` and read that entity's transform instead.
    pub fn joint_of(&self, bone: BoneId) -> Option<Entity> {
        self.bone_ids
            .iter()
            .position(|candidate| *candidate == bone)
            .and_then(|index| self.joints.get(index).copied())
    }
}

/// Binds one entity to a bone of another entity's rig (ADR 0088 §1).
///
/// Conversion reparents the entity onto the joint entity for `bone`, so the
/// attachment costs one `Parent` link and no per-frame work: transform
/// propagation moves it, and its own descendants, with the bone. The
/// component stays on the entity as the record of why it was reparented.
pub struct BoneAttachment {
    /// Entity carrying the [`Skeleton`] this attachment follows.
    pub rig: Entity,
    /// The bone within that rig's skeleton asset.
    pub bone: BoneId,
}

/// Binds one drawn mesh to a rig entity's [`Skeleton`] (ADR 0086).
///
/// The inverse bind matrices and joint order belong to the *skin*, not to
/// the rig, so they live here: two skins over one skeleton have different
/// joint counts and orders and must not overwrite each other.
pub struct SkinnedMesh {
    /// Entity holding the [`Skeleton`] this mesh is skinned to.
    ///
    /// A missing or despawned rig leaves the palette empty, which the render
    /// path draws as the bind pose.
    pub rig: Entity,
    /// The bone driving each skin joint, in skin joint order.
    ///
    /// Resolved through [`Skeleton::bone_ids`] every frame rather than
    /// stored as a joint index, so a binding stays meaningful against a rig
    /// it was not spawned with, and a bone the rig lacks degrades to the
    /// bind pose instead of indexing out of range.
    pub joint_bones: Vec<BoneId>,
    /// One inverse bind matrix per skin joint, same order as
    /// [`Self::joint_bones`].
    pub inverse_bind_matrices: Vec<Mat4>,
    /// The imported Skin sub-asset this binding was built from, when known.
    ///
    /// Diagnostics only; the palette never reads it.
    pub skin: Option<AssetId>,
}

/// The per-entity joint palette consumed by the skinned render path.
///
/// Entities with a [`SkinnedMesh`] must also carry this component;
/// [`joint_palette_system`] refreshes `matrices` every frame as
/// `bone_world * inverse_bind` per skin joint, capped at [`MAX_JOINTS`].
#[derive(Default)]
pub struct JointPalette {
    /// Skinning matrices in skin joint order.
    pub matrices: Vec<Mat4>,
}

/// Recomputes every [`JointPalette`] from rig joint world transforms.
///
/// Each [`Skeleton`] is resolved once into a `BoneId -> world matrix` map,
/// so meshes sharing a rig share that work; each binding then gathers its
/// own palette in its own skin joint order.
///
/// Every failure mode degrades to the bind pose instead of panicking: a
/// [`SkinnedMesh`] whose rig entity no longer exists gets an empty palette,
/// a joint that despawned or lost its [`GlobalTransform`] contributes an
/// identity world matrix, and a [`BoneId`] the rig does not carry does the
/// same.
pub fn joint_palette_system(
    joint_transforms: Query<&GlobalTransform>,
    rigs: Query<&Skeleton>,
    mut skins: Query<(&SkinnedMesh, &mut JointPalette)>,
) {
    let world_matrices: HashMap<Entity, Mat4> = joint_transforms
        .iter()
        .map(|(entity, global)| (entity, global.0))
        .collect();

    let bone_matrices: HashMap<Entity, HashMap<BoneId, Mat4>> = rigs
        .iter()
        .map(|(entity, rig)| {
            let bones = rig
                .bone_ids
                .iter()
                .zip(&rig.joints)
                .map(|(bone, joint)| {
                    let joint_world = world_matrices.get(joint).copied().unwrap_or(Mat4::IDENTITY);
                    (*bone, joint_world)
                })
                .collect();
            (entity, bones)
        })
        .collect();

    for (_, (skin, palette)) in skins.iter_mut() {
        palette.matrices.clear();
        let Some(bones) = bone_matrices.get(&skin.rig) else {
            continue;
        };
        palette.matrices.extend(
            skin.joint_bones
                .iter()
                .zip(&skin.inverse_bind_matrices)
                .take(MAX_JOINTS)
                .map(|(bone, inverse_bind)| {
                    let bone_world = bones.get(bone).copied().unwrap_or(Mat4::IDENTITY);
                    bone_world * *inverse_bind
                }),
        );
    }
}

/// A skeleton node description produced by model import.
///
/// `parent` indexes into the same node slice; `None` marks a root. The
/// importer produces one entry per document node and derives
/// [`SkeletonAsset::bones`] from the subset that a skin needs.
#[derive(Debug, Clone)]
pub struct SkeletonNodeDesc {
    /// Human-readable node name.
    pub name: String,
    /// Index of the parent node in the same slice, if any.
    pub parent: Option<usize>,
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation.
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
}

/// Reports why [`spawn_rig`] could not build a joint hierarchy.
#[derive(Debug)]
pub enum RigSpawnError {
    /// Runtime ECS mutation failed.
    World(WorldError),
    /// A bone's parent link broke the parent-before-child invariant that
    /// [`SkeletonAsset::bones`] guarantees, which only a corrupt asset can
    /// do.
    BoneParentOutOfOrder {
        /// Position of the offending bone.
        bone_index: usize,
        /// The parent index it declared.
        parent_index: usize,
    },
}

impl fmt::Display for RigSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::World(_) => formatter.write_str("world mutation failed while spawning a rig"),
            Self::BoneParentOutOfOrder {
                bone_index,
                parent_index,
            } => write!(
                formatter,
                "bone {bone_index} declares parent {parent_index}, which is not an earlier bone"
            ),
        }
    }
}

impl std::error::Error for RigSpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::World(source) => Some(source),
            Self::BoneParentOutOfOrder { .. } => None,
        }
    }
}

impl From<WorldError> for RigSpawnError {
    fn from(source: WorldError) -> Self {
        Self::World(source)
    }
}

/// The components one [`spawn_rig`] call produced for a rig entity.
///
/// Both must be added to the same entity. [`Skeleton`] alone leaves that rig
/// without pose storage, and every engine pose producer writes a [`RigPose`]
/// layer rather than joint transforms (ADR 0106), so the rig would silently
/// stop animating.
pub struct SpawnedRig {
    /// Joint entities and bone identity for the rig entity.
    pub skeleton: Skeleton,
    /// Layered pose storage sharing [`Self::skeleton`]'s bone order.
    pub pose: RigPose,
}

/// Spawns one joint entity per bone of `skeleton` and returns the components
/// its rig entity needs.
///
/// Every spawned entity receives [`Transform`] (the bone's rest TRS) and
/// [`GlobalTransform`], and hierarchy links use [`Parent`] / [`Children`] so
/// the existing transform propagation resolves joint world matrices. On
/// failure every entity spawned by this call is despawned again (best
/// effort).
///
/// The whole asset is spawned, not just the bones some skin references, so
/// attachment and helper bones are addressable (ADR 0086 §1).
///
/// The caller must add both returned components to one entity; see
/// [`SpawnedRig`] for why the pose cannot be omitted.
///
/// # Errors
///
/// Returns an error when the bone list is not in parent-before-child order
/// or ECS mutation fails.
pub fn spawn_rig(world: &mut World, skeleton: &SkeletonAsset) -> Result<SpawnedRig, RigSpawnError> {
    for (bone_index, bone) in skeleton.bones.iter().enumerate() {
        if let Some(parent_index) = bone.parent
            && parent_index >= bone_index {
                return Err(RigSpawnError::BoneParentOutOfOrder {
                    bone_index,
                    parent_index,
                });
            }
    }

    let mut joints: Vec<Entity> = Vec::with_capacity(skeleton.bones.len());
    if let Err(error) = spawn_rig_entities(world, skeleton, &mut joints) {
        for entity in joints.into_iter().rev() {
            let _ = world.despawn(entity);
        }
        return Err(error);
    }

    Ok(SpawnedRig {
        skeleton: Skeleton {
            bone_ids: skeleton.bones.iter().map(|bone| bone.id).collect(),
            joints,
            asset: Some(skeleton.id.clone()),
        },
        // Built from the same bone array the joints were spawned from, so
        // pose indices and joint indices cannot drift apart.
        pose: RigPose::from_skeleton(skeleton),
    })
}

fn spawn_rig_entities(
    world: &mut World,
    skeleton: &SkeletonAsset,
    joints: &mut Vec<Entity>,
) -> Result<(), RigSpawnError> {
    for bone in &skeleton.bones {
        let entity = world.spawn_with(Transform {
            translation: bone.rest_translation,
            rotation: bone.rest_rotation,
            scale: bone.rest_scale,
        })?;
        joints.push(entity);
        world.add_component(entity, GlobalTransform::default())?;
    }

    let mut children_by_bone: Vec<Vec<Entity>> = vec![Vec::new(); skeleton.bones.len()];
    for (bone_index, bone) in skeleton.bones.iter().enumerate() {
        let Some(parent_index) = bone.parent else {
            continue;
        };
        // Validated as an earlier bone by `spawn_rig`.
        let parent_entity = joints[parent_index];
        world.add_component(joints[bone_index], Parent(parent_entity))?;
        children_by_bone[parent_index].push(joints[bone_index]);
    }
    for (bone_index, children) in children_by_bone.into_iter().enumerate() {
        if children.is_empty() {
            continue;
        }
        world.add_component(joints[bone_index], Children(children))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};

    fn bone(id: u32, name: &str, parent: Option<usize>, translation: Vec3) -> BoneDef {
        BoneDef {
            id: BoneId(id),
            name: name.to_owned(),
            parent,
            rest_translation: translation,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        }
    }

    fn skeleton_asset(bones: Vec<BoneDef>) -> SkeletonAsset {
        let identity = compute_skeleton_identity(&bones);
        let next_bone_id = bones.iter().map(|bone| bone.id.0 + 1).max().unwrap_or(0);
        SkeletonAsset {
            id: AssetId::generate(),
            name: "rig".to_owned(),
            bones,
            identity,
            next_bone_id,
        }
    }

    fn spawn_joint(world: &mut World, translation: Vec3) -> Entity {
        let entity = world
            .spawn_with(Transform::from_translation(translation))
            .expect("joint must spawn");
        world
            .add_component(entity, GlobalTransform(Mat4::from_translation(translation)))
            .expect("joint must accept GlobalTransform");
        entity
    }

    /// Spawns a rig entity carrying `joints` under `bone_ids`, plus one mesh
    /// entity bound to it through `joint_bones`.
    fn spawn_rig_and_mesh(
        world: &mut World,
        joints: Vec<Entity>,
        bone_ids: Vec<BoneId>,
        joint_bones: Vec<BoneId>,
        inverse_bind_matrices: Vec<Mat4>,
    ) -> (Entity, Entity) {
        let rig = world
            .spawn_with(Skeleton {
                joints,
                bone_ids,
                asset: None,
            })
            .expect("rig entity must spawn");
        let skinned = bind_mesh(world, rig, joint_bones, inverse_bind_matrices);
        (rig, skinned)
    }

    fn bind_mesh(
        world: &mut World,
        rig: Entity,
        joint_bones: Vec<BoneId>,
        inverse_bind_matrices: Vec<Mat4>,
    ) -> Entity {
        let skinned = world
            .spawn_with(SkinnedMesh {
                rig,
                joint_bones,
                inverse_bind_matrices,
                skin: None,
            })
            .expect("skinned entity must spawn");
        world
            .add_component(skinned, JointPalette::default())
            .expect("skinned entity must accept JointPalette");
        skinned
    }

    fn run_palette(world: &mut World) {
        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), world);
        app.add_system(joint_palette_system);
        app.update().expect("palette schedule must run");
        std::mem::swap(app.world_mut(), world);
    }

    fn palette_of(world: &World, entity: Entity) -> Vec<Mat4> {
        world
            .get_component::<JointPalette>(entity)
            .expect("palette must exist")
            .matrices
            .clone()
    }

    #[test]
    fn palette_combines_bone_world_and_inverse_bind() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::new(2.0, 0.0, 0.0));
        let inverse_bind = Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0));
        let (_, skinned) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(0)],
            vec![inverse_bind],
        );

        run_palette(&mut world);

        let matrices = palette_of(&world, skinned);
        assert_eq!(matrices.len(), 1);
        let expected = Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0));
        assert!((matrices[0] - expected)
            .to_cols_array()
            .iter()
            .all(|difference| difference.abs() < 1.0e-6));
    }

    #[test]
    fn missing_joint_entity_falls_back_to_inverse_bind_only() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::new(5.0, 0.0, 0.0));
        let inverse_bind = Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0));
        let (_, skinned) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(0)],
            vec![inverse_bind],
        );
        world.despawn(joint).expect("joint must despawn");

        run_palette(&mut world);

        assert_eq!(palette_of(&world, skinned), vec![inverse_bind]);
    }

    #[test]
    fn a_bone_the_rig_does_not_carry_falls_back_to_inverse_bind_only() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::new(5.0, 0.0, 0.0));
        let inverse_bind = Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0));
        // The binding names a bone that is absent from the rig's bone list.
        let (_, skinned) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(9)],
            vec![inverse_bind],
        );

        run_palette(&mut world);

        assert_eq!(palette_of(&world, skinned), vec![inverse_bind]);
    }

    #[test]
    fn spawn_rig_links_hierarchy_and_covers_every_bone() {
        let mut world = World::new();
        let asset = skeleton_asset(vec![
            bone(0, "root", None, Vec3::new(1.0, 0.0, 0.0)),
            bone(1, "child", Some(0), Vec3::new(0.0, 2.0, 0.0)),
            // A bone no skin references still gets a joint entity.
            bone(2, "attach", Some(1), Vec3::new(0.0, 0.5, 0.0)),
        ]);

        let rig = spawn_rig(&mut world, &asset).expect("valid skeleton must spawn");

        assert_eq!(rig.skeleton.joints.len(), 3);
        assert_eq!(rig.skeleton.bone_ids, vec![BoneId(0), BoneId(1), BoneId(2)]);
        assert_eq!(rig.skeleton.asset, Some(asset.id.clone()));
        let parent = world
            .get_component::<Parent>(rig.skeleton.joints[1])
            .expect("child joint must have Parent");
        assert_eq!(parent.0, rig.skeleton.joints[0]);
        let children = world
            .get_component::<Children>(rig.skeleton.joints[0])
            .expect("root joint must have Children");
        assert_eq!(children.0, vec![rig.skeleton.joints[1]]);

        crate::transform::transform_propagation_system(Query::new(&mut world));
        let child_world = world
            .get_component::<GlobalTransform>(rig.skeleton.joints[1])
            .expect("child joint must have GlobalTransform")
            .matrix()
            .col(3)
            .truncate();
        assert_eq!(child_world, Vec3::new(1.0, 2.0, 0.0));
    }

    #[test]
    fn spawn_rig_returns_a_pose_aligned_with_its_joints() {
        let mut world = World::new();
        let asset = skeleton_asset(vec![
            bone(0, "root", None, Vec3::new(1.0, 0.0, 0.0)),
            bone(1, "child", Some(0), Vec3::new(0.0, 2.0, 0.0)),
        ]);

        let rig = spawn_rig(&mut world, &asset).expect("valid skeleton must spawn");

        // Pose producers address bones by pose index and the publisher zips
        // that pose against `Skeleton::joints`, so the two must agree in
        // length, order, and originating asset.
        assert_eq!(rig.pose.joint_count(), rig.skeleton.joints.len());
        assert_eq!(rig.pose.skeleton_asset(), &asset.id);
        assert_eq!(rig.pose.parents(), &[None, Some(0)]);
        assert_eq!(
            rig.pose
                .rest_pose()
                .local_transform(1)
                .expect("child rest transform")
                .translation,
            Vec3::new(0.0, 2.0, 0.0)
        );
    }

    #[test]
    fn spawn_rig_rejects_out_of_order_parents_without_leaking_entities() {
        let mut world = World::new();
        let asset = skeleton_asset(vec![
            bone(0, "root", Some(1), Vec3::ZERO),
            bone(1, "child", None, Vec3::ZERO),
        ]);

        let result = spawn_rig(&mut world, &asset);

        assert!(matches!(
            result,
            Err(RigSpawnError::BoneParentOutOfOrder {
                bone_index: 0,
                parent_index: 1,
            })
        ));
        assert_eq!(world.entity_count(), 0);
    }

    #[test]
    fn mismatched_joint_and_bind_lengths_use_shorter_list() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::ZERO);
        let (_, skinned) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(0), BoneId(0)],
            vec![Mat4::IDENTITY],
        );

        run_palette(&mut world);

        assert_eq!(palette_of(&world, skinned).len(), 1);
    }

    #[test]
    fn two_skins_over_one_rig_each_get_their_own_joint_order() {
        let mut world = World::new();
        let root = spawn_joint(&mut world, Vec3::new(3.0, 0.0, 0.0));
        let tip = spawn_joint(&mut world, Vec3::new(7.0, 0.0, 0.0));
        // The rig carries both bones; each skin binds a different subset in a
        // different order, which is exactly what one Skeleton per skin could
        // not express.
        let (rig, body) = spawn_rig_and_mesh(
            &mut world,
            vec![root, tip],
            vec![BoneId(0), BoneId(1)],
            vec![BoneId(0), BoneId(1)],
            vec![Mat4::IDENTITY, Mat4::IDENTITY],
        );
        let hair = bind_mesh(&mut world, rig, vec![BoneId(1)], vec![Mat4::IDENTITY]);

        run_palette(&mut world);

        assert_eq!(
            palette_of(&world, body),
            vec![
                Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)),
                Mat4::from_translation(Vec3::new(7.0, 0.0, 0.0)),
            ]
        );
        assert_eq!(
            palette_of(&world, hair),
            vec![Mat4::from_translation(Vec3::new(7.0, 0.0, 0.0))]
        );
    }

    #[test]
    fn meshes_sharing_a_rig_and_a_skin_receive_the_same_palette() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::new(3.0, 0.0, 0.0));
        let (rig, first) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(0)],
            vec![Mat4::IDENTITY],
        );
        let second = bind_mesh(&mut world, rig, vec![BoneId(0)], vec![Mat4::IDENTITY]);

        run_palette(&mut world);

        let expected = vec![Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0))];
        assert_eq!(palette_of(&world, first), expected);
        assert_eq!(palette_of(&world, second), expected);
    }

    #[test]
    fn despawned_rig_leaves_an_empty_palette() {
        let mut world = World::new();
        let joint = spawn_joint(&mut world, Vec3::ZERO);
        let (rig, skinned) = spawn_rig_and_mesh(
            &mut world,
            vec![joint],
            vec![BoneId(0)],
            vec![BoneId(0)],
            vec![Mat4::IDENTITY],
        );
        world.despawn(rig).expect("rig must despawn");

        run_palette(&mut world);

        assert!(palette_of(&world, skinned).is_empty());
    }
}
