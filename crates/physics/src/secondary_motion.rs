//! Fixed-step engine-native secondary-motion simulation (ADR 0112).
//!
//! Each opted-in entity owns an isolated Rapier world. Secondary-motion
//! colliders therefore never enter gameplay collision events, hit tests,
//! character-controller queries, or gameplay impulse exchange.

use engine_core::time::FixedTime;
use engine_ecs::{Entity, Query, Res, ResMut};
use engine_rig::rig_pose::{PoseStage, RigPose};
use engine_rig::rigid_body_rig::{
    JointDef, RigidBodyDef, RigidBodyMode, RigidBodyShape, SecondaryMotion,
    SecondaryMotionRigAsset, SecondaryMotionRigRegistry,
};
use engine_rig::skeleton_asset::BoneId;
use engine_rig::skinning::Skeleton;
use engine_rig::transform::{GlobalTransform, Transform};
use glam::{Mat4, Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use rapier3d::prelude::{
    CoefficientCombineRule, ColliderBuilder, GenericJointBuilder, Group, InteractionGroups,
    InteractionTestMode, JointAxesMask, JointAxis, MotorModel, PhysicsWorld, Pose,
    RigidBodyBuilder, RigidBodyHandle, RigidBodyType, Rotation, SharedShape, Vector,
};

const STANDARD_GRAVITY_METERS_PER_SECOND_SQUARED: f32 = 9.81;

type RigQuery<'a> = (
    &'a SecondaryMotion,
    &'a Skeleton,
    Option<&'a GlobalTransform>,
    &'a mut RigPose,
);

type PresentationRigQuery<'a> = (&'a SecondaryMotion, &'a Skeleton);

/// Runtime-only collection of isolated per-character secondary-motion worlds.
#[derive(Default)]
pub struct SecondaryMotionWorlds {
    characters: HashMap<Entity, CharacterPhysicsWorld>,
}

impl SecondaryMotionWorlds {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of live isolated character solvers.
    pub fn simulated_character_count(&self) -> usize {
        self.characters.len()
    }
}

struct CharacterPhysicsWorld {
    rig_id: String,
    world: PhysicsWorld,
    body_handles: Vec<RigidBodyHandle>,
    bone_driven: Vec<bool>,
    presentation_previous: Vec<Transform>,
    presentation_current: Vec<Transform>,
}

/// Interpolates the latest two fixed-step secondary-motion poses for rendering.
///
/// This presentation-only system never mutates Rapier or [`RigPose`].
pub fn secondary_motion_presentation_system(
    fixed_time: Res<FixedTime>,
    worlds: Option<Res<SecondaryMotionWorlds>>,
    mut rigs: Query<PresentationRigQuery<'_>>,
    mut transforms: Query<&mut Transform>,
) {
    let Some(worlds) = worlds.as_deref() else {
        return;
    };
    let alpha = fixed_time.interpolation_alpha();
    let mut writes = HashMap::new();

    for (entity, (marker, skeleton)) in &mut rigs {
        let Some(state) = worlds.characters.get(&entity) else {
            continue;
        };
        if state.rig_id != marker.rig.as_str()
            || state.presentation_previous.len() != skeleton.joints.len()
            || state.presentation_current.len() != skeleton.joints.len()
        {
            continue;
        }

        writes.extend(
            skeleton
                .joints
                .iter()
                .copied()
                .zip(&state.presentation_previous)
                .zip(&state.presentation_current)
                .map(|((joint, previous), current)| {
                    (
                        joint,
                        interpolate_presentation_transform(previous, current, alpha),
                    )
                }),
        );
    }

    if writes.is_empty() {
        return;
    }
    for (entity, transform) in transforms.iter_mut() {
        if let Some(interpolated) = writes.get(&entity) {
            *transform = interpolated.clone();
        }
    }
}

/// Advances every opted-in secondary-motion rig by one authoritative fixed step.
///
/// The system reads the resolved procedural pose, writes only the
/// [`RigPose`] physics layer, and reseats bodies on any timeline discontinuity.
/// It deliberately does not reconstruct animation history or pre-roll seeks.
pub fn secondary_motion_system(
    fixed_time: Res<FixedTime>,
    registry: Option<Res<SecondaryMotionRigRegistry>>,
    mut worlds: Option<ResMut<SecondaryMotionWorlds>>,
    mut rigs: Query<RigQuery<'_>>,
) {
    let Some(registry) = registry else {
        return;
    };
    let mut fallback_worlds = SecondaryMotionWorlds::new();
    let worlds = worlds.as_deref_mut().unwrap_or(&mut fallback_worlds);
    let fixed_delta = fixed_time.fixed_delta.max(f32::EPSILON);
    let mut live = HashSet::new();

    for (entity, (marker, skeleton, root_global, rig_pose)) in &mut rigs {
        let Some(rig) = registry.get(&marker.rig) else {
            continue;
        };
        if rig.dynamic_body_count() == 0 || !skeleton_matches(rig, skeleton) {
            continue;
        }
        live.insert(entity);

        let root_matrix = root_global
            .map(GlobalTransform::matrix)
            .unwrap_or(Mat4::IDENTITY);
        let joint_indices = joint_indices(skeleton);
        let procedural_worlds = rig_pose
            .evaluate_world(PoseStage::Procedural, root_matrix)
            .to_vec();

        let needs_rebuild = worlds
            .characters
            .get(&entity)
            .is_none_or(|state| state.rig_id != marker.rig.as_str());
        if needs_rebuild {
            let rest_worlds = rig_pose
                .evaluate_world(PoseStage::Rest, root_matrix)
                .to_vec();
            worlds.characters.insert(
                entity,
                build_character_world(
                    rig,
                    &joint_indices,
                    root_matrix,
                    &rest_worlds,
                    &procedural_worlds,
                ),
            );
        }

        let Some(state) = worlds.characters.get_mut(&entity) else {
            continue;
        };
        state.world.integration_parameters.dt = fixed_delta;
        let targets = resolved_body_poses(rig, &joint_indices, &procedural_worlds);
        let discontinuous = rig_pose.is_discontinuous();
        if discontinuous {
            reseat_bodies(state, &targets);
        }
        drive_follow_bodies(state, rig, &targets);
        state.world.step();
        write_simulated_bones(
            state,
            rig,
            &joint_indices,
            root_matrix,
            &procedural_worlds,
            rig_pose,
        );
        capture_presentation_pose(state, rig_pose, discontinuous);
    }

    worlds.characters.retain(|entity, _| live.contains(entity));
}

fn capture_presentation_pose(
    state: &mut CharacterPhysicsWorld,
    rig_pose: &mut RigPose,
    reset_history: bool,
) {
    rig_pose.compose();
    let current = rig_pose.final_pose().local_transforms();
    if reset_history
        || state.presentation_current.len() != current.len()
        || state.presentation_previous.len() != current.len()
    {
        state.presentation_previous.clear();
        state.presentation_previous.extend_from_slice(current);
        state.presentation_current.clear();
        state.presentation_current.extend_from_slice(current);
        return;
    }

    std::mem::swap(
        &mut state.presentation_previous,
        &mut state.presentation_current,
    );
    state.presentation_current.clear();
    state.presentation_current.extend_from_slice(current);
}

fn interpolate_presentation_transform(
    previous: &Transform,
    current: &Transform,
    alpha: f32,
) -> Transform {
    let alpha = alpha.clamp(0.0, 1.0);
    Transform {
        translation: previous.translation.lerp(current.translation, alpha),
        rotation: normalized_quat(previous.rotation.slerp(current.rotation, alpha)),
        scale: previous.scale.lerp(current.scale, alpha),
    }
}

fn skeleton_matches(rig: &SecondaryMotionRigAsset, skeleton: &Skeleton) -> bool {
    rig.skeleton
        .as_ref()
        .is_none_or(|expected| skeleton.asset.as_ref() == Some(expected))
}

fn joint_indices(skeleton: &Skeleton) -> HashMap<BoneId, usize> {
    skeleton
        .bone_ids
        .iter()
        .enumerate()
        .map(|(index, bone)| (*bone, index))
        .collect()
}

fn build_character_world(
    rig: &SecondaryMotionRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rest_worlds: &[Mat4],
    procedural_worlds: &[Mat4],
) -> CharacterPhysicsWorld {
    let mut world = PhysicsWorld::new();
    world.gravity = Vector::new(
        0.0,
        -STANDARD_GRAVITY_METERS_PER_SECOND_SQUARED,
        0.0,
    );
    let bone_driven = bone_driven_bodies(rig);
    let mut body_handles = Vec::with_capacity(rig.bodies.len());
    let mut rest_body_poses = Vec::with_capacity(rig.bodies.len());

    for (index, body) in rig.bodies.iter().enumerate() {
        let rest_body_pose = body_pose_from_bone_worlds(body, joint_indices, rest_worlds)
            .unwrap_or_else(|| pose_from_matrix(root_matrix * offset_matrix(body)));
        let body_pose = body_pose_from_bone_worlds(body, joint_indices, procedural_worlds)
            .unwrap_or_else(|| pose_from_matrix(root_matrix * offset_matrix(body)));
        let body_type = if bone_driven[index] {
            RigidBodyType::KinematicPositionBased
        } else {
            RigidBodyType::Dynamic
        };
        let builder = RigidBodyBuilder::new(body_type)
            .pose(body_pose)
            .linear_damping(non_negative(body.linear_damping))
            .angular_damping(non_negative(body.angular_damping))
            .gravity_scale(finite_or(body.gravity_scale, 1.0))
            .can_sleep(false)
            .ccd_enabled(false);
        let handle = world.bodies.insert(builder);
        let collider = ColliderBuilder::new(rigid_body_shape(body.shape))
            .mass(non_negative(body.mass))
            .restitution(finite_or(body.restitution, 0.0).clamp(0.0, 1.0))
            .restitution_combine_rule(CoefficientCombineRule::Multiply)
            .friction(non_negative(body.friction))
            .friction_combine_rule(CoefficientCombineRule::Multiply)
            .collision_groups(secondary_motion_collision_groups(body))
            .build();
        world
            .colliders
            .insert_with_parent(collider, handle, &mut world.bodies);
        body_handles.push(handle);
        rest_body_poses.push(rest_body_pose);
    }

    for joint in &rig.joints {
        insert_joint(
            &mut world,
            &body_handles,
            &rest_body_poses,
            joint,
            root_matrix,
        );
    }

    CharacterPhysicsWorld {
        rig_id: rig.id.as_str().to_owned(),
        world,
        body_handles,
        bone_driven,
        presentation_previous: Vec::new(),
        presentation_current: Vec::new(),
    }
}

fn non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Returns bodies whose pose is fully determined by the resolved rig pose.
///
/// A fully locked constraint propagates follow intent transitively, avoiding
/// numerical drift in bodies that have no actual simulated degree of freedom.
fn bone_driven_bodies(rig: &SecondaryMotionRigAsset) -> Vec<bool> {
    let mut driven = rig
        .bodies
        .iter()
        .map(|body| body.mode == RigidBodyMode::FollowBone)
        .collect::<Vec<_>>();
    let welds = rig
        .joints
        .iter()
        .filter(|joint| joint_welds_its_bodies(joint))
        .filter_map(|joint| Some((joint.body_a?, joint.body_b?)))
        .filter(|(a, b)| *a < driven.len() && *b < driven.len())
        .collect::<Vec<_>>();

    let mut changed = true;
    while changed {
        changed = false;
        for (a, b) in &welds {
            if driven[*a] != driven[*b] {
                driven[*a] = true;
                driven[*b] = true;
                changed = true;
            }
        }
    }
    driven
}

fn joint_welds_its_bodies(joint: &JointDef) -> bool {
    (0..3).all(|axis| {
        matches!(
            AuthoredLimit::from_pair(joint.translation_lower[axis], joint.translation_upper[axis]),
            AuthoredLimit::Locked(_)
        ) && matches!(
            AuthoredLimit::from_pair(joint.rotation_lower[axis], joint.rotation_upper[axis]),
            AuthoredLimit::Locked(_)
        )
    })
}

fn resolved_body_poses(
    rig: &SecondaryMotionRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    procedural_worlds: &[Mat4],
) -> Vec<Option<Pose>> {
    rig.bodies
        .iter()
        .map(|body| body_pose_from_bone_worlds(body, joint_indices, procedural_worlds))
        .collect()
}

/// Reseats all bound bodies on a discontinuity and erases solver history.
fn reseat_bodies(state: &mut CharacterPhysicsWorld, targets: &[Option<Pose>]) {
    for (index, target) in targets.iter().enumerate() {
        let Some(target) = target else {
            continue;
        };
        let Some(rigid_body) = state
            .body_handles
            .get(index)
            .and_then(|handle| state.world.bodies.get_mut(*handle))
        else {
            continue;
        };
        rigid_body.set_position(*target, true);
        if rigid_body.is_kinematic() {
            rigid_body.set_next_kinematic_position(*target);
        }
        rigid_body.set_linvel(Vector::new(0.0, 0.0, 0.0), true);
        rigid_body.set_angvel(Vector::new(0.0, 0.0, 0.0), true);
    }
}

fn drive_follow_bodies(
    state: &mut CharacterPhysicsWorld,
    rig: &SecondaryMotionRigAsset,
    targets: &[Option<Pose>],
) {
    for index in 0..rig.bodies.len() {
        if !state.bone_driven.get(index).copied().unwrap_or(false) {
            continue;
        }
        let Some(Some(target)) = targets.get(index).copied() else {
            continue;
        };
        let Some(rigid_body) = state
            .body_handles
            .get(index)
            .and_then(|handle| state.world.bodies.get_mut(*handle))
        else {
            continue;
        };
        rigid_body.set_next_kinematic_position(target);
    }
}

fn write_simulated_bones(
    state: &mut CharacterPhysicsWorld,
    rig: &SecondaryMotionRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    procedural_worlds: &[Mat4],
    rig_pose: &mut RigPose,
) {
    let mut desired_worlds = HashMap::<usize, (Mat4, RigidBodyMode)>::new();
    for (index, body) in rig.bodies.iter().enumerate() {
        if state.bone_driven.get(index).copied().unwrap_or(false) {
            continue;
        }
        let Some(bone) = body.bone else {
            continue;
        };
        let Some(joint_index) = joint_indices.get(&bone).copied() else {
            continue;
        };
        let Some(rigid_body) = state
            .body_handles
            .get(index)
            .and_then(|handle| state.world.bodies.get(*handle))
        else {
            continue;
        };
        let Some(procedural_world) = procedural_worlds.get(joint_index).copied() else {
            continue;
        };

        let (world_scale, _, procedural_translation) =
            procedural_world.to_scale_rotation_translation();
        let posed = matrix_from_pose(*rigid_body.position())
            * scaled_offset_matrix(body, world_scale).inverse();
        let (_, rotation, simulated_translation) = posed.to_scale_rotation_translation();
        let translation = match body.mode {
            RigidBodyMode::Dynamic => simulated_translation,
            RigidBodyMode::DynamicWithBonePosition => procedural_translation,
            RigidBodyMode::FollowBone => continue,
        };
        desired_worlds.insert(
            joint_index,
            (
                Mat4::from_scale_rotation_translation(world_scale, rotation, translation),
                body.mode,
            ),
        );
    }

    let procedural_locals = (0..rig_pose.joint_count())
        .map(|joint_index| {
            rig_pose
                .local_transform(PoseStage::Procedural, joint_index)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let mut resolved_worlds = vec![root_matrix; rig_pose.joint_count()];

    for (joint_index, procedural_local) in procedural_locals.iter().enumerate() {
        let parent_world = rig_pose
            .parents()
            .get(joint_index)
            .copied()
            .flatten()
            .filter(|parent_index| *parent_index < joint_index)
            .and_then(|parent_index| resolved_worlds.get(parent_index).copied())
            .unwrap_or(root_matrix);

        let Some((desired_world, mode)) = desired_worlds.get(&joint_index).copied() else {
            resolved_worlds[joint_index] = parent_world * procedural_local.to_matrix();
            continue;
        };
        let local_matrix = parent_world.inverse() * desired_world;
        let (scale, rotation, translation) = local_matrix.to_scale_rotation_translation();
        if !scale.is_finite() || !rotation.is_finite() || !translation.is_finite() {
            resolved_worlds[joint_index] = parent_world * procedural_local.to_matrix();
            continue;
        }

        rig_pose
            .physics_layer_mut()
            .write_rotation(joint_index, rotation);
        let resolved_translation = if mode == RigidBodyMode::Dynamic {
            rig_pose
                .physics_layer_mut()
                .write_translation(joint_index, translation);
            translation
        } else {
            procedural_local.translation
        };
        resolved_worlds[joint_index] = parent_world
            * Transform {
                translation: resolved_translation,
                rotation,
                scale: procedural_local.scale,
            }
            .to_matrix();
    }

    // Rotation-only bodies are kept at the resolved bone position before the
    // next fixed step so hidden solver translation cannot accumulate.
    for (index, body) in rig.bodies.iter().enumerate() {
        if body.mode != RigidBodyMode::DynamicWithBonePosition
            || state.bone_driven.get(index).copied().unwrap_or(false)
        {
            continue;
        }
        let Some(bone) = body.bone else {
            continue;
        };
        let Some(joint_index) = joint_indices.get(&bone).copied() else {
            continue;
        };
        let Some(bone_world) = resolved_worlds.get(joint_index).copied() else {
            continue;
        };
        let target = pose_from_matrix(bone_world * offset_matrix(body));
        let Some(rigid_body) = state
            .body_handles
            .get(index)
            .and_then(|handle| state.world.bodies.get_mut(*handle))
        else {
            continue;
        };
        let mut pinned = *rigid_body.position();
        pinned.translation = target.translation;
        rigid_body.set_position(pinned, true);
    }
}

fn body_pose_from_bone_worlds(
    body: &RigidBodyDef,
    joint_indices: &HashMap<BoneId, usize>,
    bone_worlds: &[Mat4],
) -> Option<Pose> {
    let joint_index = *joint_indices.get(&body.bone?)?;
    let bone_world = *bone_worlds.get(joint_index)?;
    Some(pose_from_matrix(bone_world * offset_matrix(body)))
}

fn insert_joint(
    world: &mut PhysicsWorld,
    handles: &[RigidBodyHandle],
    rest_body_poses: &[Pose],
    joint: &JointDef,
    root_matrix: Mat4,
) {
    let (Some(index_a), Some(index_b)) = (joint.body_a, joint.body_b) else {
        return;
    };
    let (Some(handle_a), Some(handle_b)) = (handles.get(index_a), handles.get(index_b)) else {
        return;
    };
    let (Some(rest_body_a), Some(rest_body_b)) =
        (rest_body_poses.get(index_a), rest_body_poses.get(index_b))
    else {
        return;
    };
    let joint_world = pose_from_matrix(root_matrix * joint_matrix(joint));
    let mut builder = GenericJointBuilder::new(JointAxesMask::empty())
        .local_frame1(rest_body_a.inv_mul(&joint_world))
        .local_frame2(rest_body_b.inv_mul(&joint_world))
        .contacts_enabled(false);

    let mut locked = JointAxesMask::empty();
    for axis in 0..3 {
        let translation = (
            [JointAxis::LinX, JointAxis::LinY, JointAxis::LinZ][axis],
            AuthoredLimit::from_pair(joint.translation_lower[axis], joint.translation_upper[axis]),
            non_negative(joint.spring_translation[axis]),
        );
        let rotation = (
            [JointAxis::AngX, JointAxis::AngY, JointAxis::AngZ][axis],
            AuthoredLimit::from_pair(joint.rotation_lower[axis], joint.rotation_upper[axis]),
            non_negative(joint.spring_rotation[axis]),
        );
        for (joint_axis, limit, stiffness) in [translation, rotation] {
            let locked_axis = matches!(limit, AuthoredLimit::Locked(_));
            match limit {
                AuthoredLimit::Locked(0.0) => locked |= joint_axis.into(),
                AuthoredLimit::Locked(offset) => {
                    builder = builder.limits(joint_axis, [offset, offset]);
                }
                AuthoredLimit::Ranged(range) => builder = builder.limits(joint_axis, range),
                AuthoredLimit::Free => {}
            }
            if stiffness > 0.0 && !locked_axis {
                builder = builder
                    .motor_model(joint_axis, MotorModel::ForceBased)
                    .motor_position(joint_axis, 0.0, stiffness, 0.0);
            }
        }
    }

    world
        .impulse_joints
        .insert(*handle_a, *handle_b, builder.locked_axes(locked), true);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum AuthoredLimit {
    Locked(f32),
    Free,
    Ranged([f32; 2]),
}

impl AuthoredLimit {
    fn from_pair(lower: f32, upper: f32) -> Self {
        if !lower.is_finite() || !upper.is_finite() || lower > upper {
            Self::Free
        } else if lower == upper {
            Self::Locked(lower)
        } else {
            Self::Ranged([lower, upper])
        }
    }
}

fn rigid_body_shape(shape: RigidBodyShape) -> SharedShape {
    match shape {
        RigidBodyShape::Sphere { radius } => SharedShape::ball(radius.max(f32::EPSILON)),
        RigidBodyShape::Box { half_extents } => SharedShape::cuboid(
            half_extents[0].max(f32::EPSILON),
            half_extents[1].max(f32::EPSILON),
            half_extents[2].max(f32::EPSILON),
        ),
        RigidBodyShape::Capsule {
            radius,
            half_height,
        } => SharedShape::capsule_y(
            half_height.max(f32::EPSILON),
            radius.max(f32::EPSILON),
        ),
    }
}

fn secondary_motion_collision_groups(body: &RigidBodyDef) -> InteractionGroups {
    let membership = 1_u32.checked_shl(u32::from(body.group)).unwrap_or(0);
    InteractionGroups::new(
        Group::from_bits_truncate(membership),
        Group::from_bits_truncate(u32::from(body.collides_with)),
        InteractionTestMode::And,
    )
}

fn offset_matrix(body: &RigidBodyDef) -> Mat4 {
    let (translation, rotation) = body.bone_offset();
    Mat4::from_rotation_translation(normalized_quat(rotation), translation)
}

fn scaled_offset_matrix(body: &RigidBodyDef, world_scale: Vec3) -> Mat4 {
    let (translation, rotation) = body.bone_offset();
    Mat4::from_rotation_translation(normalized_quat(rotation), translation * world_scale)
}

fn joint_matrix(joint: &JointDef) -> Mat4 {
    Mat4::from_rotation_translation(
        normalized_quat(Quat::from_array(joint.rotation)),
        Vec3::from_array(joint.translation),
    )
}

fn normalized_quat(rotation: Quat) -> Quat {
    if rotation.is_finite() && rotation.length_squared() > f32::EPSILON {
        rotation.normalize()
    } else {
        Quat::IDENTITY
    }
}

fn pose_from_matrix(matrix: Mat4) -> Pose {
    let (_, rotation, translation) = matrix.to_scale_rotation_translation();
    Pose::from_parts(
        Vector::new(translation.x, translation.y, translation.z),
        Rotation::from_xyzw(rotation.x, rotation.y, rotation.z, rotation.w),
    )
}

fn matrix_from_pose(pose: Pose) -> Mat4 {
    Mat4::from_rotation_translation(
        Quat::from_xyzw(
            pose.rotation.x,
            pose.rotation.y,
            pose.rotation.z,
            pose.rotation.w,
        ),
        Vec3::new(pose.translation.x, pose.translation.y, pose.translation.z),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discontinuity_reseat_clears_linear_and_angular_velocity() {
        let mut world = PhysicsWorld::new();
        let handle = world.bodies.insert(
            RigidBodyBuilder::dynamic()
                .translation(Vector::new(4.0, 5.0, 6.0))
                .linvel(Vector::new(1.0, 2.0, 3.0))
                .angvel(Vector::new(3.0, 2.0, 1.0)),
        );
        let mut state = CharacterPhysicsWorld {
            rig_id: "test".to_owned(),
            world,
            body_handles: vec![handle],
            bone_driven: vec![false],
            presentation_previous: Vec::new(),
            presentation_current: Vec::new(),
        };
        let target = Pose::from_parts(Vector::new(9.0, 8.0, 7.0), Rotation::identity());

        reseat_bodies(&mut state, &[Some(target)]);

        let body = state.world.bodies.get(handle).expect("body must remain alive");
        assert_eq!(*body.linvel(), Vector::zeros());
        assert_eq!(*body.angvel(), Vector::zeros());
        assert_eq!(*body.position(), target);
    }

    #[test]
    fn presentation_interpolation_never_overshoots_fixed_samples() {
        let previous = Transform {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        };
        let current = Transform {
            translation: Vec3::new(10.0, 0.0, 0.0),
            rotation: Quat::from_rotation_y(1.0),
            scale: Vec3::splat(2.0),
        };
        assert_eq!(
            interpolate_presentation_transform(&previous, &current, -1.0).translation,
            Vec3::ZERO
        );
        assert_eq!(
            interpolate_presentation_transform(&previous, &current, 2.0).translation,
            current.translation
        );
    }

    #[test]
    fn invalid_limits_are_treated_as_free() {
        assert_eq!(AuthoredLimit::from_pair(1.0, -1.0), AuthoredLimit::Free);
        assert_eq!(
            AuthoredLimit::from_pair(f32::NAN, 1.0),
            AuthoredLimit::Free
        );
    }
}
