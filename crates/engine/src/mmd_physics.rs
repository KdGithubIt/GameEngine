//! Isolated Rapier secondary-motion worlds for imported MMD rigid-body rigs.
//!
//! Each entity carrying [`RigidBodyPhysics`](crate::rigid_body_rig::RigidBodyPhysics)
//! owns a separate solver. Consequently cosmetic hair/skirt bodies can never
//! enter gameplay [`CollisionEvents`](crate::collision::CollisionEvents),
//! collision layers, hit tests, or character-controller queries (ADR 0096 §5).

use engine_authoring::id::AssetId;
use engine_ecs::{Entity, Query, Res, ResMut};
use glam::{Mat4, Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use rapier3d::prelude::{
    CoefficientCombineRule, ColliderBuilder, GenericJointBuilder, Group, InteractionGroups,
    InteractionTestMode, JointAxesMask, JointAxis, MotorModel, PhysicsWorld, Pose,
    RigidBodyBuilder, RigidBodyHandle, RigidBodyType, Rotation, SharedShape, Vector,
};

use crate::animation::{AnimationClip, Animator, AnimatorState, RootMotionMode};
use crate::asset::{Assets, RuntimeAssetId};
use crate::pose_graph::PoseArena;
use crate::rig_pose::{PoseStage, RigPose};
use crate::rigid_body_rig::{
    JointDef, RigidBodyDef, RigidBodyMode, RigidBodyPhysics, RigidBodyRigAsset,
    RigidBodyRigRegistry, RigidBodyShape,
};
use crate::skeleton_asset::BoneId;
use crate::skinning::Skeleton;
use crate::time::{FixedTime, FIXED_DELTA_SECONDS};
use crate::transform::{GlobalTransform, Transform};

/// Runtime-only collection of isolated per-character Rapier worlds.
#[derive(Default)]
pub struct MmdPhysicsWorlds {
    characters: HashMap<Entity, CharacterPhysicsWorld>,
}

impl MmdPhysicsWorlds {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of characters whose secondary-motion solver is
    /// currently alive.
    pub fn simulated_character_count(&self) -> usize {
        self.characters.len()
    }
}

struct CharacterPhysicsWorld {
    rig_id: AssetId,
    world: PhysicsWorld,
    body_handles: Vec<RigidBodyHandle>,
    /// Whether each body's pose is fully determined by animation, either
    /// because PMX declared it so or because [`bone_driven_bodies`] proved it.
    bone_driven: Vec<bool>,
    /// Clip/time represented by this solver when it can be continued as
    /// historical seek input. `None` after ambiguous discontinuities.
    history_clip: Option<RuntimeAssetId>,
    history_time: f32,
    history_playback_speed: f32,
    /// Fixed step for which the Rapier damping rates were derived.
    damping_fixed_delta: f32,
    /// Complete post-physics local pose from the fixed step before
    /// [`Self::presentation_current`].
    ///
    /// These are presentation snapshots only. Rapier and [`RigPose`] remain
    /// authoritative at the fixed-step boundary.
    presentation_previous: Vec<Transform>,
    /// Complete post-physics local pose from the latest fixed step.
    presentation_current: Vec<Transform>,
}

/// How many meters one PMX authoring unit represents (ADR 0108).
///
/// Deliberately a second definition of [`crate::pmx_import::PMX_TO_METERS`]
/// rather than a use of it: the importer is desktop-only while this bridge
/// compiles on every runtime target (ADR 0096 §1). `pmx_import` owns a test
/// that pins the two together, so they cannot drift apart silently.
pub(crate) const PMX_AUTHORING_SCALE: f32 = 0.08;

/// MikuMikuDance's default gravity, in the PMX units a rig's constants were
/// tuned against: 9.8 under MMD's own ten-units-per-meter convention.
///
/// Every MMD runtime steps Bullet in these units, so this is the acceleration
/// the model's author actually saw their hair and skirt fall under.
const MMD_GRAVITY_PMX_UNITS: f32 = 9.8 * 10.0;

/// Converts an authored angular spring constant into Rapier's meter-space
/// torque per radian (ADR 0108 §3).
///
/// A PMX rotation spring is a torque per radian expressed as PMX force times
/// PMX length. Both factors carry one power of [`PMX_AUTHORING_SCALE`] when
/// the rig is simulated in meters, so the constant carries two. A translation
/// spring is a force per length, where the two conversions cancel exactly and
/// the authored number passes through unchanged.
const ANGULAR_STIFFNESS_SCALE: f32 = PMX_AUTHORING_SCALE * PMX_AUTHORING_SCALE;

/// Solver iterations per step, matching Bullet's own default
/// (`btContactSolverInfo::m_numIterations`) rather than Rapier's four.
///
/// An MMD rig is hundreds of near-rigid constraints deep, and its author tuned
/// it against a solver given this budget. On the project's PMX model the
/// difference is visible: at four iterations the locked axes hold to within
/// 88 mm under a sway, at ten to within 56 mm, and the rig swings noticeably
/// less of its own accord.
const MMD_SOLVER_ITERATIONS: usize = 10;

/// The damping rate standing in for Bullet's "erase the velocity" case.
///
/// No finite rate reproduces it exactly (see [`rapier_damping_rate`]), but at
/// any fixed step this engine runs, this one leaves under one percent of the
/// velocity per step, which is the behavior that reaches the screen.
const FULLY_DAMPED_RATE: f32 = 1.0e4;

/// A pre-roll may ignore earlier velocity once every simulated degree of
/// freedom retains less than this fraction of it.
///
/// ADR 0108 already treats "under one percent per step" as effectively erased
/// for fully damped PMX bodies. Reusing that physical tolerance here makes the
/// seek horizon depend on the rig's authored damping and the fixed timestep,
/// not on an arbitrary count of VMD frames.
const SEEK_PREROLL_RETAINED_VELOCITY: f32 = 0.01;

type RigQuery<'a> = (
    &'a RigidBodyPhysics,
    &'a Skeleton,
    Option<&'a GlobalTransform>,
    Option<&'a Animator>,
    &'a mut RigPose,
);

type PresentationRigQuery<'a> = (&'a RigidBodyPhysics, &'a Skeleton);

/// Interpolates MMD rig presentation between the two latest fixed samples.
///
/// Secondary motion remains fully fixed-step: this system writes only the
/// published joint [`Transform`] compatibility surface after fixed simulation
/// has finished for the frame. It never mutates Rapier or [`RigPose`], so
/// gameplay collision and the next physics step continue from authoritative
/// fixed state while rendering avoids exposing 60 Hz stair-steps directly on
/// higher-refresh displays.
pub fn mmd_rigid_body_presentation_system(
    fixed_time: Res<FixedTime>,
    worlds: Option<Res<MmdPhysicsWorlds>>,
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
        if state.rig_id != marker.rig
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

/// Advances every opted-in MMD secondary-motion rig and writes simulated
/// body poses back to the corresponding bone entities.
///
/// Register after animation, foot IK, and fixed transform propagation. A
/// second propagation pass must follow this system before rendering or
/// gameplay collision reads the corrected bones.
pub fn mmd_rigid_body_physics_system(
    fixed_time: Res<FixedTime>,
    registry: Option<Res<RigidBodyRigRegistry>>,
    clips: Option<Res<Assets<AnimationClip>>>,
    mut pose_arena: Option<ResMut<PoseArena>>,
    mut worlds: Option<ResMut<MmdPhysicsWorlds>>,
    mut rigs: Query<RigQuery<'_>>,
) {
    let Some(registry) = registry else {
        return;
    };
    let mut fallback_worlds = MmdPhysicsWorlds::new();
    let worlds = worlds.as_deref_mut().unwrap_or(&mut fallback_worlds);
    let mut fallback_arena = PoseArena::new();
    let pose_arena = pose_arena.as_deref_mut().unwrap_or(&mut fallback_arena);
    let fixed_delta = fixed_time.fixed_delta.max(f32::EPSILON);
    let mut live = HashSet::new();

    for (entity, (marker, skeleton, root_global, animator, rig_pose)) in &mut rigs {
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

        let mut pre_rolled = false;
        if rig_pose.is_animation_seek()
            && let (Some(animator), Some(clips)) = (animator, clips.as_deref())
            && let Some(clip) = clips.get(&animator.clip)
        {
            if let Some(state) = worlds.characters.get_mut(&entity) {
                pre_rolled = advance_cached_seek_world(
                    state,
                    rig,
                    &joint_indices,
                    root_matrix,
                    rig_pose,
                    animator,
                    clip,
                    fixed_delta,
                    pose_arena,
                );
            }
            if !pre_rolled
                && let Some(state) = build_seek_preroll_world(
                    rig,
                    &joint_indices,
                    root_matrix,
                    rig_pose,
                    animator,
                    clip,
                    fixed_delta,
                    pose_arena,
                )
            {
                worlds.characters.insert(entity, state);
                pre_rolled = true;
            }
        }

        let needs_rebuild = worlds
            .characters
            .get(&entity)
            .is_none_or(|state| state.rig_id != marker.rig);
        if needs_rebuild {
            // The rest pose is only needed to derive authored joint frames,
            // so it is evaluated when a solver is built rather than on every
            // fixed step.
            let rest_worlds = rig_pose
                .evaluate_world(PoseStage::Rest, root_matrix)
                .to_vec();
            let state = build_character_world(
                rig,
                &joint_indices,
                root_matrix,
                &rest_worlds,
                &procedural_worlds,
            );
            worlds.characters.insert(entity, state);
        }
        let Some(state) = worlds.characters.get_mut(&entity) else {
            continue;
        };
        sync_damping_timestep(state, rig, fixed_delta);
        state.world.integration_parameters.dt = fixed_delta;
        let targets = animated_body_poses(rig, &joint_indices, &procedural_worlds);

        // A successfully pre-rolled seek already stepped through the target
        // sample. Every other discontinuity keeps the established safe reseat
        // behavior: loop wraps, instant state switches and teleports have no
        // unique clip history to reconstruct.
        if !pre_rolled {
            if rig_pose.is_discontinuous() {
                reseat_bodies(state, &targets);
            }
            drive_follow_bodies(state, rig, &targets);
            state.world.step();
            update_history_cursor(state, rig_pose, animator, clips.as_deref());
        }

        write_simulated_bones(
            state,
            rig,
            &joint_indices,
            root_matrix,
            &procedural_worlds,
            rig_pose,
        );
        let reset_presentation = rig_pose.is_discontinuous() || pre_rolled;
        capture_presentation_pose(state, rig_pose, reset_presentation);
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

fn skeleton_matches(rig: &RigidBodyRigAsset, skeleton: &Skeleton) -> bool {
    rig.skeleton
        .as_ref()
        .is_none_or(|expected| skeleton.asset.as_ref() == Some(expected))
}

/// Maps every bone a rig carries to its pose-buffer index.
///
/// Built once per character per fixed step. Every rigid body resolves a
/// [`BoneId`] at least twice per step (driving and write-back), so scanning
/// [`Skeleton::bone_ids`] per body would cost body-count times bone-count
/// comparisons on a PMX character with hundreds of both.
fn joint_indices(skeleton: &Skeleton) -> HashMap<BoneId, usize> {
    skeleton
        .bone_ids
        .iter()
        .enumerate()
        .map(|(index, bone)| (*bone, index))
        .collect()
}

/// Reconstructs the isolated secondary-motion world for an animation seek.
///
/// Animation owns pose evaluation: this function only asks the pure pose
/// sampler for historical clip samples. Physics owns the fixed-step cadence
/// and never advances the live [`Animator`], emits animation events, or runs
/// gameplay/procedural systems while pre-rolling.
#[allow(clippy::too_many_arguments)]
fn build_seek_preroll_world(
    rig: &RigidBodyRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rig_pose: &mut RigPose,
    animator: &Animator,
    clip: &AnimationClip,
    fixed_delta: f32,
    pose_arena: &mut PoseArena,
) -> Option<CharacterPhysicsWorld> {
    if !seek_preroll_supported(animator, clip, rig_pose) {
        return None;
    }

    let target_time = animator.time.clamp(0.0, clip.duration.max(0.0));
    let clip_step = fixed_delta * animator.playback_speed;
    if target_time <= f32::EPSILON || !clip_step.is_finite() || clip_step <= f32::EPSILON {
        return None;
    }

    let bone_driven = bone_driven_bodies(rig);
    let available_steps = fixed_step_count_covering(target_time, clip_step);
    let step_count = seek_preroll_step_count(rig, &bone_driven, fixed_delta)
        .map_or(available_steps, |horizon| horizon.min(available_steps))
        .max(1);
    // Anchor the sample grid at the seek target. Repeated f32 addition can
    // leave a tiny remainder below `target_time`; treating that remainder as
    // another full Rapier fixed step would over-simulate the reconstructed
    // history by one frame.
    let first_time =
        (target_time - clip_step * step_count.saturating_sub(1) as f32).max(0.0);

    let first_worlds = sample_animation_worlds(
        clip,
        first_time,
        joint_indices,
        root_matrix,
        rig_pose,
        pose_arena,
    )?;
    let rest_worlds = rig_pose
        .evaluate_world(PoseStage::Rest, root_matrix)
        .to_vec();
    let mut state = build_character_world(
        rig,
        joint_indices,
        root_matrix,
        &rest_worlds,
        &first_worlds,
    );
    sync_damping_timestep(&mut state, rig, fixed_delta);
    state.world.integration_parameters.dt = fixed_delta;

    let mut targets = animated_body_poses(rig, joint_indices, &first_worlds);
    drive_follow_bodies(&mut state, rig, &targets);
    state.world.step();

    for step_index in 1..step_count {
        let next_time = if step_index + 1 == step_count {
            target_time
        } else {
            first_time + clip_step * step_index as f32
        };
        if !sample_animation_targets(
            rig,
            clip,
            next_time,
            joint_indices,
            root_matrix,
            rig_pose,
            pose_arena,
            &mut targets,
        ) {
            return None;
        }
        drive_follow_bodies(&mut state, rig, &targets);
        state.world.step();
    }

    state.history_clip = Some(animator.clip.id());
    state.history_time = target_time;
    state.history_playback_speed = animator.playback_speed;
    Some(state)
}

#[allow(clippy::too_many_arguments)]
fn advance_cached_seek_world(
    state: &mut CharacterPhysicsWorld,
    rig: &RigidBodyRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rig_pose: &mut RigPose,
    animator: &Animator,
    clip: &AnimationClip,
    fixed_delta: f32,
    pose_arena: &mut PoseArena,
) -> bool {
    if state.rig_id != rig.id
        || state.history_clip != Some(animator.clip.id())
        || state.history_playback_speed.to_bits() != animator.playback_speed.to_bits()
        || !seek_preroll_supported(animator, clip, rig_pose)
    {
        return false;
    }

    let target_time = animator.time.clamp(0.0, clip.duration.max(0.0));
    if target_time < state.history_time {
        return false;
    }
    let clip_step = fixed_delta * animator.playback_speed;
    if !clip_step.is_finite() || clip_step <= f32::EPSILON {
        return false;
    }

    let gap = target_time - state.history_time;
    let Some(step_count) = fixed_step_count_if_aligned(gap, clip_step) else {
        // A cached solver can only be continued by whole fixed steps. A
        // fractional clip-time gap would otherwise integrate a full Rapier
        // step for a partial animation interval, so rebuild a target-anchored
        // history instead.
        return false;
    };

    // Prefer the already-correct current solver only when reaching the target
    // from it costs no more history than a fresh damping-horizon rebuild.
    let bone_driven = bone_driven_bodies(rig);
    if let Some(horizon) = seek_preroll_step_count(rig, &bone_driven, fixed_delta)
        && step_count > horizon
    {
        return false;
    }

    sync_damping_timestep(state, rig, fixed_delta);
    state.world.integration_parameters.dt = fixed_delta;
    let mut targets = Vec::with_capacity(rig.bodies.len());
    for step_index in 1..=step_count {
        let next_time = if step_index == step_count {
            target_time
        } else {
            state.history_time + clip_step * step_index as f32
        };
        if !sample_animation_targets(
            rig,
            clip,
            next_time,
            joint_indices,
            root_matrix,
            rig_pose,
            pose_arena,
            &mut targets,
        ) {
            return false;
        }
        drive_follow_bodies(state, rig, &targets);
        state.world.step();
    }

    state.history_time = target_time;
    true
}

fn update_history_cursor(
    state: &mut CharacterPhysicsWorld,
    rig_pose: &RigPose,
    animator: Option<&Animator>,
    clips: Option<&Assets<AnimationClip>>,
) {
    if rig_pose.is_discontinuous() {
        state.history_clip = None;
        return;
    }
    let Some((animator, clip)) = animator.and_then(|animator| {
        clips?
            .get(&animator.clip)
            .map(|clip| (animator, clip))
    }) else {
        state.history_clip = None;
        return;
    };
    if !seek_preroll_supported(animator, clip, rig_pose) {
        state.history_clip = None;
        return;
    }
    state.history_clip = Some(animator.clip.id());
    state.history_time = animator.time;
    state.history_playback_speed = animator.playback_speed;
}

fn seek_preroll_supported(
    animator: &Animator,
    clip: &AnimationClip,
    rig_pose: &RigPose,
) -> bool {
    animator.state == AnimatorState::Playing
        && animator.root_motion_mode == RootMotionMode::Disabled
        && !animator.is_fading()
        && animator.playback_speed.is_finite()
        && animator.playback_speed > 0.0
        && animator.time.is_finite()
        && clip.duration.is_finite()
        // Entity-level animation moves the character root, whose historical
        // world transform is not stored in the clip-local RigPose.
        && clip.channels.iter().all(|channel| channel.target_bone.is_some())
        // World-aware modifiers cannot be reconstructed by a data-only clip
        // sample. Imported VMD IK is already baked into the AnimationClip
        // (ADR 0097), so ordinary MMD motion does not hit this fallback.
        && (0..rig_pose.joint_count()).all(|joint_index| {
            rig_pose
                .procedural_layer()
                .channels(joint_index)
                .is_none_or(|channels| channels.is_empty())
        })
}

/// Returns the finite number of fixed steps needed to forget prior velocity.
///
/// `None` means at least one simulated degree of freedom has zero damping, so
/// there is no finite forgetting horizon and the only defensible start is the
/// beginning of the clip.
fn seek_preroll_step_count(
    rig: &RigidBodyRigAsset,
    bone_driven: &[bool],
    fixed_delta: f32,
) -> Option<usize> {
    let mut slowest_retention = 0.0_f32;
    let mut simulated_body_count = 0_usize;

    for (index, body) in rig.bodies.iter().enumerate() {
        if bone_driven.get(index).copied().unwrap_or(false) {
            continue;
        }
        simulated_body_count += 1;
        for damping in [body.linear_damping, body.angular_damping] {
            let rate = rapier_damping_rate(damping, fixed_delta);
            if rate <= f32::EPSILON {
                return None;
            }
            let retention = 1.0 / (1.0 + fixed_delta * rate);
            slowest_retention = slowest_retention.max(retention);
        }
    }

    if simulated_body_count == 0 || slowest_retention <= SEEK_PREROLL_RETAINED_VELOCITY {
        return Some(1);
    }
    if slowest_retention >= 1.0 {
        return None;
    }

    let steps = (SEEK_PREROLL_RETAINED_VELOCITY.ln() / slowest_retention.ln()).ceil();
    Some(steps.max(1.0) as usize)
}

fn fixed_step_count_covering(duration: f32, clip_step: f32) -> usize {
    let ratio = duration / clip_step;
    let nearest = ratio.round();
    let tolerance = f32::EPSILON * ratio.abs().max(1.0) * 8.0;
    let steps = if (ratio - nearest).abs() <= tolerance {
        nearest
    } else {
        ratio.ceil()
    };
    steps.max(1.0) as usize
}

fn fixed_step_count_if_aligned(duration: f32, clip_step: f32) -> Option<usize> {
    if duration <= f32::EPSILON {
        return Some(0);
    }
    let ratio = duration / clip_step;
    let nearest = ratio.round();
    let tolerance = f32::EPSILON * ratio.abs().max(1.0) * 8.0;
    ((ratio - nearest).abs() <= tolerance && nearest >= 1.0).then_some(nearest as usize)
}

fn sample_animation_worlds(
    clip: &AnimationClip,
    sample_time: f32,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rig_pose: &mut RigPose,
    pose_arena: &mut PoseArena,
) -> Option<Vec<Mat4>> {
    let mut sample = pose_arena.acquire(rig_pose.joint_count());
    sample.sample_into(clip, sample_time, joint_indices);
    if !rig_pose
        .animation_layer_mut()
        .swap_values(&mut sample.joints)
    {
        pose_arena.release(sample);
        return None;
    }

    let worlds = rig_pose
        .evaluate_world(PoseStage::Animation, root_matrix)
        .to_vec();
    let restored = rig_pose
        .animation_layer_mut()
        .swap_values(&mut sample.joints);
    pose_arena.release(sample);
    restored.then_some(worlds)
}

#[allow(clippy::too_many_arguments)]
// Historical sampling stays allocation-free after the first pre-roll sample.
fn sample_animation_targets(
    rig: &RigidBodyRigAsset,
    clip: &AnimationClip,
    sample_time: f32,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rig_pose: &mut RigPose,
    pose_arena: &mut PoseArena,
    targets: &mut Vec<Option<Pose>>,
) -> bool {
    let mut sample = pose_arena.acquire(rig_pose.joint_count());
    sample.sample_into(clip, sample_time, joint_indices);
    if !rig_pose
        .animation_layer_mut()
        .swap_values(&mut sample.joints)
    {
        pose_arena.release(sample);
        return false;
    }

    targets.clear();
    {
        let worlds = rig_pose.evaluate_world(PoseStage::Animation, root_matrix);
        targets.extend(
            rig.bodies
                .iter()
                .map(|body| body_pose_from_bone_worlds(body, joint_indices, worlds)),
        );
    }
    let restored = rig_pose
        .animation_layer_mut()
        .swap_values(&mut sample.joints);
    pose_arena.release(sample);
    restored
}

fn build_character_world(
    rig: &RigidBodyRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    rest_worlds: &[Mat4],
    procedural_worlds: &[Mat4],
) -> CharacterPhysicsWorld {
    let mut world = PhysicsWorld::new();
    // The authored constants were tuned against MMD's gravity, not Earth's.
    // Expressed in this engine's meters it is the weaker of the two, which is
    // what keeps a chain's pendulum period matching the model's own tool.
    world.gravity = Vector::new(0.0, -MMD_GRAVITY_PMX_UNITS * PMX_AUTHORING_SCALE, 0.0);
    world.integration_parameters.num_solver_iterations = MMD_SOLVER_ITERATIONS;
    let bone_driven = bone_driven_bodies(rig);
    let mut body_handles = Vec::with_capacity(rig.bodies.len());
    let mut rest_body_poses = Vec::with_capacity(rig.bodies.len());

    for (index, body) in rig.bodies.iter().enumerate() {
        // PMX joint frames are authored relative to each rigid body's rest
        // pose. Keep that authored frame independent from whichever animation
        // happened to be playing when this runtime world was constructed.
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
            .linear_damping(rapier_damping_rate(
                body.linear_damping,
                FIXED_DELTA_SECONDS,
            ))
            .angular_damping(rapier_damping_rate(
                body.angular_damping,
                FIXED_DELTA_SECONDS,
            ))
            // MMD's Bullet worlds disable deactivation for secondary bodies.
            // Sleeping a settled Rapier chain would change when an animated
            // kinematic anchor can resume affecting it.
            .can_sleep(false)
            // The MMD Bullet bridge does not opt rigid bodies into CCD.
            .ccd_enabled(false);
        let handle = world.bodies.insert(builder);
        let collider = ColliderBuilder::new(rigid_body_shape(body.shape))
            // PMX mass is the body's total shape mass. Supplying it as
            // RigidBodyBuilder::additional_mass would add Rapier's default
            // density-derived collider mass and derive the wrong inertia.
            .mass(body.mass.max(0.0))
            .restitution(body.restitution.clamp(0.0, 1.0))
            .restitution_combine_rule(CoefficientCombineRule::Multiply)
            .friction(body.friction.max(0.0))
            .friction_combine_rule(CoefficientCombineRule::Multiply)
            .collision_groups(mmd_collision_groups(body))
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
        rig_id: rig.id.clone(),
        world,
        body_handles,
        bone_driven,
        history_clip: None,
        history_time: 0.0,
        history_playback_speed: 0.0,
        damping_fixed_delta: FIXED_DELTA_SECONDS,
        presentation_previous: Vec::new(),
        presentation_current: Vec::new(),
    }
}

fn sync_damping_timestep(
    state: &mut CharacterPhysicsWorld,
    rig: &RigidBodyRigAsset,
    fixed_delta: f32,
) {
    if state.damping_fixed_delta.to_bits() == fixed_delta.to_bits() {
        return;
    }

    for (index, body) in rig.bodies.iter().enumerate() {
        let Some(rigid_body) = state
            .body_handles
            .get(index)
            .and_then(|handle| state.world.bodies.get_mut(*handle))
        else {
            continue;
        };
        rigid_body.set_linear_damping(rapier_damping_rate(body.linear_damping, fixed_delta));
        rigid_body.set_angular_damping(rapier_damping_rate(body.angular_damping, fixed_delta));
    }
    state.damping_fixed_delta = fixed_delta;
}

/// Which bodies animation fully determines, so the solver is never asked to
/// discover a pose that is already known.
///
/// A PMX joint that locks all six axes welds its two bodies together. Weld a
/// body to one animation follows and it follows animation too: it has no
/// remaining degree of freedom, and no simulation can produce motion its
/// author did not authorise. Solving it anyway can only add error, and does:
/// this project's model welds each sleeve to the elbow that way, and driving
/// them as dynamic bodies broke the weld by 53 degrees in a single step
/// during an ordinary dance, which reached the screen as the sleeve snapping
/// away from the arm and back.
///
/// The relation is transitive — a body welded to a welded body is equally
/// determined — so this is the closure of [`RigidBodyMode::FollowBone`] under
/// fully locked joints.
fn bone_driven_bodies(rig: &RigidBodyRigAsset) -> Vec<bool> {
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

    // Each pass can only turn flags on, and there are at most as many bodies
    // to turn on as the rig has, so this terminates.
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

/// Whether a joint leaves its two bodies no freedom relative to each other.
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

/// The pose animation places each body at this step, indexed like
/// [`RigidBodyRigAsset::bodies`].
fn animated_body_poses(
    rig: &RigidBodyRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    procedural_worlds: &[Mat4],
) -> Vec<Option<Pose>> {
    rig.bodies
        .iter()
        .map(|body| body_pose_from_bone_worlds(body, joint_indices, procedural_worlds))
        .collect()
}

/// Places every body on the animated pose and clears the velocity it carried.
///
/// A discontinuity with no reconstructible history — a loop wrap, an instant
/// state switch, a teleported character, or a seek that cannot be pre-rolled —
/// is not motion. Driving kinematic bodies through it makes Rapier derive a
/// velocity from a displacement that never physically happened, and the
/// impulse joints then fling every attached hair or skirt body straight
/// through the limits meant to hold it. Reconstructible seeks use historical
/// fixed-step simulation instead; this remains the safe fallback.
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
            // Without this the next step would still derive its velocity
            // from the pose the body held before being reseated.
            rigid_body.set_next_kinematic_position(*target);
        }
        rigid_body.set_linvel(Vector::new(0.0, 0.0, 0.0), true);
        rigid_body.set_angvel(Vector::new(0.0, 0.0, 0.0), true);
    }
}

fn drive_follow_bodies(
    state: &mut CharacterPhysicsWorld,
    rig: &RigidBodyRigAsset,
    targets: &[Option<Pose>],
) {
    for index in 0..rig.bodies.len() {
        // Bodies animation determines are driven; the rest stay dynamic, where
        // teleporting either mode would make their joints apply an artificial
        // positional correction.
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
    rig: &RigidBodyRigAsset,
    joint_indices: &HashMap<BoneId, usize>,
    root_matrix: Mat4,
    procedural_worlds: &[Mat4],
    rig_pose: &mut RigPose,
) {
    // Resolve each solver-controlled bone in world space first. The complete
    // hierarchy is reconstructed in a second parent-before-child pass below;
    // considering only directly simulated parents is insufficient because a
    // non-simulated spacer bone still inherits a simulated ancestor.
    let mut desired_worlds = HashMap::<usize, (Mat4, RigidBodyMode)>::new();
    for (index, body) in rig.bodies.iter().enumerate() {
        // A bone-driven body reproduces the animated pose by construction, so
        // writing its solver result back would replace that pose with a
        // rounded copy of itself.
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
        let desired_world =
            Mat4::from_scale_rotation_translation(world_scale, rotation, translation);
        desired_worlds.insert(joint_index, (desired_world, body.mode));
    }

    // Snapshot the procedural local pose before mutating the physics layer.
    // These locals are also used to propagate non-physics spacer bones under
    // an already simulated ancestor.
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
            // PMX mode 2 owns rotation only. Its actual world position must
            // be reconstructed from the procedural local translation and the
            // resolved parent, not from the pre-physics procedural world.
            procedural_local.translation
        };
        let resolved_local = Transform {
            translation: resolved_translation,
            rotation,
            scale: procedural_local.scale,
        };
        resolved_worlds[joint_index] = parent_world * resolved_local.to_matrix();
    }

    // PMX mode 2 is more than omitting translation from bone write-back.
    // MMD updates the bone's rotation from Bullet, resolves the bone's kept
    // position, then copies that position back into the rigid body while
    // preserving the simulated body rotation. If the solver body is left at
    // its simulated translation instead, hidden positional error becomes the
    // starting point for the next step's contacts and joints and can drift
    // away from what MMD actually simulates.
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
            joint.spring_translation[axis].max(0.0),
        );
        let rotation = (
            [JointAxis::AngX, JointAxis::AngY, JointAxis::AngZ][axis],
            AuthoredLimit::from_pair(joint.rotation_lower[axis], joint.rotation_upper[axis]),
            joint.spring_rotation[axis].max(0.0) * ANGULAR_STIFFNESS_SCALE,
        );
        for (joint_axis, limit, stiffness) in [translation, rotation] {
            let is_locked = matches!(limit, AuthoredLimit::Locked(_));
            match limit {
                // A lock and a hard equality constraint are the same statement,
                // and expressing it as one is what frees the spring below from
                // having to hold the axis in place.
                AuthoredLimit::Locked(0.0) => locked |= joint_axis.into(),
                AuthoredLimit::Locked(offset) => {
                    builder = builder.limits(joint_axis, [offset, offset]);
                }
                AuthoredLimit::Ranged(range) => builder = builder.limits(joint_axis, range),
                AuthoredLimit::Free => {}
            }
            // Bullet's own solver lets an equality limit override the spring on
            // the same axis, so a spring authored on a locked axis never acts in
            // MMD. Most of this project's model's springs are authored that way;
            // driving them here would fight the lock rather than reproduce it.
            if stiffness > 0.0 && !is_locked {
                builder = builder
                    // Rapier's default acceleration-based motor divides the
                    // authored constant out by the body's own mass and inertia,
                    // which is exactly the quantity a PMX author tuned it
                    // against. Force-based keeps stiffness a force per unit of
                    // error, which is what Bullet applies.
                    .motor_model(joint_axis, MotorModel::ForceBased)
                    // No damping term: Bullet caps this motor's force at the
                    // Hooke force itself, so what a PMX spring actually applies
                    // is an undamped restoring force. Dissipation comes from the
                    // per-body damping the author tuned separately.
                    .motor_position(joint_axis, 0.0, stiffness, 0.0);
            }
        }
    }

    world
        .impulse_joints
        .insert(*handle_a, *handle_b, builder.locked_axes(locked), true);
}

/// What a PMX joint's limit pair asks of one degree of freedom.
///
/// Bullet — the solver every MMD runtime hands these numbers to — reads the
/// pair as three distinct states rather than as a range alone, so a bridge
/// that only ever builds a range cannot express two thirds of what a model
/// author wrote.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AuthoredLimit {
    /// `lower == upper`: the axis is pinned at that offset.
    Locked(f32),
    /// `lower > upper`: the axis is unconstrained.
    Free,
    /// `lower < upper`: the axis moves within the range.
    Ranged([f32; 2]),
}

impl AuthoredLimit {
    /// Classifies one authored `lower`/`upper` pair.
    ///
    /// A non-finite bound describes no reachable range, so it degrades to
    /// [`Self::Free`] rather than reaching the solver as a NaN limit.
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

/// Converts a PMX/Bullet damping factor into the damping rate Rapier expects.
///
/// The two solvers spell damping differently. Bullet removes a *fraction* of
/// the velocity per second (`v *= (1 - d)^dt`, with `d` clamped to `0..=1`),
/// while Rapier integrates a damping *rate* (`v *= 1 / (1 + dt * rate)`).
/// The rate therefore has to be derived for the actual fixed step:
/// `rate = ((1 - d)^(-dt) - 1) / dt`. The previous continuous-time
/// approximation `-ln(1 - d)` matched only as `dt -> 0`; at 60 Hz it left
/// substantially too much residual velocity for the 0.999/0.9999 damping
/// values common on this project's hair bodies.
fn rapier_damping_rate(pmx_damping: f32, fixed_delta: f32) -> f32 {
    let damping = if pmx_damping.is_finite() {
        pmx_damping.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if damping >= 1.0 {
        FULLY_DAMPED_RATE
    } else {
        let dt = if fixed_delta.is_finite() && fixed_delta > f32::EPSILON {
            fixed_delta
        } else {
            FIXED_DELTA_SECONDS
        };
        (-dt * (1.0 - damping).ln()).exp_m1() / dt
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

fn mmd_collision_groups(body: &RigidBodyDef) -> InteractionGroups {
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

/// A body's rest offset from its bone, as [`body_pose_from_bone_worlds`]
/// actually applied it.
///
/// That function multiplies the bone's *world* matrix by the rest offset, so
/// a scaled model stretches the offset by `world_scale` before the body is
/// placed. Recovering the bone from a simulated body therefore has to divide
/// out the offset at that same scale rather than the authored one.
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
    use crate::rig_pose::{publish_final_rig_pose_system, PoseChannels};
    use crate::rigid_body_rig::RIGID_BODY_RIG_SCHEMA_VERSION;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef, BoneId, SkeletonAsset};
    use crate::transform::{Parent, Transform};

    fn test_skeleton_asset(rest_translations: &[Vec3], parents: &[Option<usize>]) -> SkeletonAsset {
        let bones = rest_translations
            .iter()
            .zip(parents)
            .enumerate()
            .map(|(index, (translation, parent))| BoneDef {
                id: BoneId(index as u32),
                name: format!("bone_{index}"),
                parent: *parent,
                rest_translation: *translation,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            })
            .collect::<Vec<_>>();
        SkeletonAsset {
            id: AssetId::generate(),
            name: "test_skeleton".to_owned(),
            identity: compute_skeleton_identity(&bones),
            next_bone_id: bones.len() as u32,
            bones,
        }
    }

    /// Builds one body with the PMX-typical values a hair or skirt chain
    /// carries, leaving the caller to vary only what its case is about.
    fn chain_body(name: &str, bone: u32, mass: f32, mode: RigidBodyMode) -> RigidBodyDef {
        RigidBodyDef {
            name: name.to_owned(),
            bone: Some(BoneId(bone)),
            bone_name: name.to_owned(),
            shape: RigidBodyShape::Sphere { radius: 0.05 },
            bone_offset_translation: [0.0; 3],
            bone_offset_rotation: [0.0, 0.0, 0.0, 1.0],
            mass,
            linear_damping: 0.1,
            angular_damping: 0.1,
            restitution: 0.0,
            friction: 0.5,
            mode,
            group: 0,
            collides_with: u16::MAX,
        }
    }
    #[test]
    fn seek_preroll_matches_continuous_fixed_step_history() {
        let skeleton_asset = test_skeleton_asset(&[Vec3::Y], &[None]);
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let mut body = chain_body("hair", 0, 0.1, RigidBodyMode::Dynamic);
        body.linear_damping = 0.1;
        body.angular_damping = 0.1;
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "seek_history".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };
        let clip = AnimationClip {
            duration: 2.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let fixed_delta = 1.0 / 60.0;
        let steps = 12_usize;
        let target_time = fixed_delta * steps as f32;
        let indices = joint_indices(&skeleton);

        let mut normal_pose = RigPose::from_skeleton(&skeleton_asset);
        let mut normal_arena = PoseArena::new();
        let first_worlds = sample_animation_worlds(
            &clip,
            fixed_delta,
            &indices,
            Mat4::IDENTITY,
            &mut normal_pose,
            &mut normal_arena,
        )
        .expect("first animation sample");
        let rest_worlds = normal_pose
            .evaluate_world(PoseStage::Rest, Mat4::IDENTITY)
            .to_vec();
        let mut normal = build_character_world(
            &rig,
            &indices,
            Mat4::IDENTITY,
            &rest_worlds,
            &first_worlds,
        );
        normal.world.integration_parameters.dt = fixed_delta;
        let mut targets = animated_body_poses(&rig, &indices, &first_worlds);
        drive_follow_bodies(&mut normal, &rig, &targets);
        normal.world.step();
        for step in 2..=steps {
            assert!(sample_animation_targets(
                &rig,
                &clip,
                fixed_delta * step as f32,
                &indices,
                Mat4::IDENTITY,
                &mut normal_pose,
                &mut normal_arena,
                &mut targets,
            ));
            drive_follow_bodies(&mut normal, &rig, &targets);
            normal.world.step();
        }

        let mut clips = Assets::<AnimationClip>::new();
        let handle = clips.add(clip.clone());
        let mut animator = Animator::playing(handle);
        animator.time = target_time;
        let mut seek_pose = RigPose::from_skeleton(&skeleton_asset);
        let mut seek_arena = PoseArena::new();
        let seek = build_seek_preroll_world(
            &rig,
            &indices,
            Mat4::IDENTITY,
            &mut seek_pose,
            &animator,
            &clip,
            fixed_delta,
            &mut seek_arena,
        )
        .expect("seek must be reconstructible");

        let normal_body = normal
            .world
            .bodies
            .get(normal.body_handles[0])
            .expect("normal body");
        let seek_body = seek
            .world
            .bodies
            .get(seek.body_handles[0])
            .expect("seek body");
        assert!(
            (normal_body.position().translation.y - seek_body.position().translation.y).abs()
                < 1.0e-5,
            "seek must reach the same secondary-motion position as continuous playback"
        );
        assert!(
            (normal_body.linvel().y - seek_body.linvel().y).abs() < 1.0e-5,
            "seek must reconstruct the velocity carried into the target frame"
        );
    }

    #[test]
    fn seek_preroll_horizon_comes_from_authored_damping() {
        let mut body = chain_body("hair", 0, 0.1, RigidBodyMode::Dynamic);
        let rig_with = |body: RigidBodyDef| RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "damping_policy".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };

        body.linear_damping = 0.0;
        assert_eq!(
            seek_preroll_step_count(&rig_with(body.clone()), &[false], 1.0 / 60.0),
            None,
            "an undamped simulated degree of freedom has no finite history horizon"
        );

        body.linear_damping = 0.999;
        body.angular_damping = 0.999;
        let fast = seek_preroll_step_count(&rig_with(body.clone()), &[false], 1.0 / 60.0)
            .expect("damped body has a finite horizon");
        body.linear_damping = 0.1;
        body.angular_damping = 0.1;
        let slow = seek_preroll_step_count(&rig_with(body), &[false], 1.0 / 60.0)
            .expect("damped body has a finite horizon");
        assert!(
            fast < slow,
            "stronger authored damping must require fewer pre-roll fixed steps"
        );
    }

    #[test]
    fn seek_preroll_uses_only_whole_fixed_steps() {
        let fixed_delta = 1.0 / 60.0;
        let multiplied_target = fixed_delta * 12.0;
        assert_eq!(
            fixed_step_count_covering(multiplied_target, fixed_delta),
            12,
            "floating-point remainder must not create a thirteenth solver step"
        );

        let mut repeated_target = 0.0;
        for _ in 0..12 {
            repeated_target += fixed_delta;
        }
        assert_eq!(
            fixed_step_count_if_aligned(repeated_target, fixed_delta),
            Some(12),
            "normal accumulated playback time must remain cache-reusable"
        );
        assert_eq!(
            fixed_step_count_if_aligned(repeated_target + fixed_delta * 0.5, fixed_delta),
            None,
            "fractional fixed-step gaps must rebuild instead of over-integrating"
        );
    }

    #[test]
    fn unsupported_seek_contexts_do_not_pre_roll() {
        let skeleton_asset = test_skeleton_asset(&[Vec3::Y], &[None]);
        let mut rig_pose = RigPose::from_skeleton(&skeleton_asset);
        let clip = AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let mut clips = Assets::<AnimationClip>::new();
        let handle = clips.add(clip.clone());
        let alternate = clips.add(clip.clone());

        let supported = Animator::playing(handle);
        assert!(
            seek_preroll_supported(&supported, &clip, &rig_pose),
            "plain bone-local playback must be reconstructible"
        );

        let mut root_motion = Animator::playing(handle);
        root_motion.root_motion_mode = RootMotionMode::ExtractedOnly;
        assert!(
            !seek_preroll_supported(&root_motion, &clip, &rig_pose),
            "root motion history is not reproduced by clip-only pre-roll"
        );

        let mut fading = Animator::playing(handle);
        fading.crossfade_to(alternate, 0.25);
        assert!(
            !seek_preroll_supported(&fading, &clip, &rig_pose),
            "an active crossfade requires blended historical state"
        );

        let entity_level_clip = AnimationClip {
            channels: vec![crate::animation::AnimChannel {
                property: crate::animation::AnimProperty::Translation,
                target_bone: None,
                keyframes: Vec::new(),
            }],
            ..clip.clone()
        };
        assert!(
            !seek_preroll_supported(&supported, &entity_level_clip, &rig_pose),
            "entity-level motion needs historical root transforms"
        );

        assert!(
            rig_pose
                .procedural_layer_mut()
                .write_rotation(0, Quat::IDENTITY),
            "test rig must expose one procedural joint"
        );
        assert!(
            !seek_preroll_supported(&supported, &clip, &rig_pose),
            "world-aware procedural pose history cannot be reconstructed from a clip"
        );
    }

    #[test]
    fn mmd_body_properties_match_bullet_mass_contact_and_activation_semantics() {
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let mut body = chain_body("physics", 0, 2.5, RigidBodyMode::Dynamic);
        body.shape = RigidBodyShape::Sphere { radius: 0.5 };
        body.friction = 0.4;
        body.restitution = 0.6;
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "body_properties".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };
        let worlds = vec![Mat4::IDENTITY];
        let state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &worlds,
            &worlds,
        );
        let rigid_body = state
            .world
            .bodies
            .get(state.body_handles[0])
            .expect("the PMX body must exist");
        let collider = state
            .world
            .colliders
            .iter()
            .next()
            .expect("the PMX collider must exist")
            .1;

        assert!(
            (rigid_body.mass_properties().mass() - 2.5).abs() < 1.0e-6,
            "PMX mass is total mass, not mass added on top of collider density"
        );
        let body_inertia = rigid_body.mass_properties().local_mprops.principal_inertia();
        let collider_inertia = collider.mass_properties().principal_inertia();
        assert!(
            (body_inertia - collider_inertia).length() < 1.0e-6,
            "the body's inertia must come from the authored mass and PMX shape"
        );
        assert!(
            rigid_body.activation().normalized_linear_threshold < 0.0
                && rigid_body.activation().angular_threshold < 0.0,
            "MMD secondary bodies must not deactivate/sleep"
        );
        assert!(
            !rigid_body.is_ccd_enabled(),
            "the MMD Bullet bridge does not opt secondary bodies into CCD"
        );
        assert_eq!(
            collider.material().friction_combine_rule,
            CoefficientCombineRule::Multiply
        );
        assert_eq!(
            collider.material().restitution_combine_rule,
            CoefficientCombineRule::Multiply
        );
    }

    #[test]
    fn mode2_realigns_solver_position_to_the_kept_bone_position() {
        let skeleton_asset = test_skeleton_asset(&[Vec3::Y], &[None]);
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let mut body = chain_body(
            "mode2",
            0,
            1.0,
            RigidBodyMode::DynamicWithBonePosition,
        );
        body.linear_damping = 0.0;
        body.angular_damping = 0.0;
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "mode2_position".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };
        let procedural_worlds = vec![Mat4::from_translation(Vec3::Y)];
        let indices = joint_indices(&skeleton);
        let mut state = build_character_world(
            &rig,
            &indices,
            Mat4::IDENTITY,
            &procedural_worlds,
            &procedural_worlds,
        );
        state.world.integration_parameters.dt = 1.0 / 60.0;
        state
            .world
            .bodies
            .get_mut(state.body_handles[0])
            .expect("mode 2 body")
            .set_linvel(Vector::new(3.0, 0.0, 0.0), true);
        state.world.step();
        let carried_velocity = state
            .world
            .bodies
            .get(state.body_handles[0])
            .expect("mode 2 body")
            .linvel()
            .x;
        assert!(
            state.world.bodies[state.body_handles[0]].translation().x > 0.01,
            "the fixture must drift before mode 2 position alignment"
        );

        let mut rig_pose = RigPose::from_skeleton(&skeleton_asset);
        write_simulated_bones(
            &mut state,
            &rig,
            &indices,
            Mat4::IDENTITY,
            &procedural_worlds,
            &mut rig_pose,
        );

        let aligned = state
            .world
            .bodies
            .get(state.body_handles[0])
            .expect("mode 2 body");
        assert!(aligned.translation().x.abs() < 1.0e-5);
        assert!((aligned.translation().y - 1.0).abs() < 1.0e-5);
        assert!(
            (aligned.linvel().x - carried_velocity).abs() < 1.0e-6,
            "position alignment must not erase the dynamic body's velocity"
        );
    }

    /// The solver reseats a rig only when the pose says it was repositioned,
    /// never because it moved a long way in one step.
    ///
    /// Fast is not the same as discontinuous, and no threshold separates
    /// them: a dance whips a hair tip an order of magnitude further than the
    /// body it hangs off — measured on this project's own motion, 30 of 504
    /// bones pass 0.30 m in a step while the median passes 0.031 m. Reseating
    /// on that reading snapped every body onto the animated pose mid-swing
    /// several times a second, including parts a model author had pinned
    /// rigid and which therefore could not move at all.
    #[test]
    fn a_fast_step_is_not_a_reseat_but_a_declared_one_is() {
        let skeleton = || Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "one_body".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![chain_body("hair", 0, 0.1, RigidBodyMode::Dynamic)],
            joints: Vec::new(),
        };
        let skeleton_asset = test_skeleton_asset(&[Vec3::Y], &[None]);

        let run = |declare: bool| {
            let mut app = engine_ecs::App::new();
            app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
            app.insert_resource(MmdPhysicsWorlds::new());
            let mut registry = RigidBodyRigRegistry::new();
            let rig_id = rig.id.clone();
            registry.insert(rig.clone());
            app.insert_resource(registry);
            let entity = app
                .world_mut()
                .spawn_with(RigidBodyPhysics::new(rig_id))
                .expect("rig marker");
            app.world_mut()
                .add_component(entity, skeleton())
                .expect("skeleton");
            app.world_mut()
                .add_component(entity, GlobalTransform::identity())
                .expect("root transform");
            app.world_mut()
                .add_component(entity, RigPose::from_skeleton(&skeleton_asset))
                .expect("rig pose");
            app.add_system(mmd_rigid_body_physics_system);
            app.update().expect("settling step");

            // Give the body a velocity the solver would carry across the
            // step, then move its bone a long way. A reseat erases that
            // velocity; ordinary integration keeps it.
            {
                let worlds = app
                    .world_mut()
                    .get_resource_mut::<MmdPhysicsWorlds>()
                    .expect("worlds");
                let state = worlds.characters.get_mut(&entity).expect("character");
                state
                    .world
                    .bodies
                    .get_mut(state.body_handles[0])
                    .expect("body")
                    .set_linvel(Vector::new(0.0, 0.0, 5.0), true);
            }
            {
                let rig_pose = app
                    .world_mut()
                    .get_component_mut::<RigPose>(entity)
                    .expect("rig pose");
                rig_pose
                    .animation_layer_mut()
                    .write_translation(0, Vec3::new(9.0, 1.0, 0.0));
                if declare {
                    rig_pose.mark_discontinuous();
                }
            }
            app.update().expect("post-jump step");

            let worlds = app
                .world()
                .get_resource::<MmdPhysicsWorlds>()
                .expect("worlds");
            let state = worlds.characters.get(&entity).expect("character");
            let body = state
                .world
                .bodies
                .get(state.body_handles[0])
                .expect("body");
            (
                Vec3::new(
                    body.position().translation.x,
                    body.position().translation.y,
                    body.position().translation.z,
                ),
                body.linvel().z,
            )
        };

        let (moved_position, moved_speed) = run(false);
        assert!(
            moved_position.x < 1.0,
            "an undeclared step must integrate, not teleport the body to {moved_position:?}"
        );
        assert!(
            moved_speed > 1.0,
            "an undeclared step must keep the body's velocity, not clear it"
        );

        let (reseated_position, reseated_speed) = run(true);
        assert!(
            (reseated_position.x - 9.0).abs() < 1.0e-3,
            "a declared repositioning must reseat the body, got {reseated_position:?}"
        );
        assert_eq!(
            reseated_speed, 0.0,
            "a declared repositioning must clear the velocity the jump would otherwise become"
        );
    }

    /// A body welded to one animation drives has no freedom left, so the
    /// solver is never asked to discover a pose that is already known.
    ///
    /// This is the regression the sleeve of this project's own model showed:
    /// PMX welds all five of its bodies to the elbow on every axis, giving
    /// them zero degrees of freedom, and Blender's reference playback moves
    /// them rigidly with the arm. Solving them as dynamic bodies instead
    /// broke the weld by 53 degrees in a single fixed step during an ordinary
    /// dance — four times what the elbow itself turned — which reached the
    /// screen as the sleeve snapping away from the arm and back.
    #[test]
    fn a_body_welded_to_a_bone_driven_one_is_bone_driven_too() {
        let weld = |name: &str, a: usize, b: usize| JointDef {
            name: name.to_owned(),
            body_a: Some(a),
            body_b: Some(b),
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation_lower: [0.0; 3],
            translation_upper: [0.0; 3],
            // PMX writes the negative zero this model actually contains.
            rotation_lower: [-0.0; 3],
            rotation_upper: [-0.0; 3],
            spring_translation: [0.0; 3],
            spring_rotation: [30.0; 3],
        };
        let mut hinge = weld("hinge", 3, 4);
        hinge.rotation_lower = [-0.3; 3];
        hinge.rotation_upper = [0.3; 3];

        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "welded".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![
                chain_body("arm", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body("sleeve", 1, 25.6, RigidBodyMode::DynamicWithBonePosition),
                // Reached only through the sleeve, so the closure has to be
                // transitive to find it.
                chain_body("cuff", 2, 6.4, RigidBodyMode::Dynamic),
                chain_body("skirt_top", 3, 10.0, RigidBodyMode::Dynamic),
                chain_body("skirt_hem", 4, 10.0, RigidBodyMode::Dynamic),
            ],
            joints: vec![weld("sleeve", 0, 1), weld("cuff", 1, 2), hinge],
        };

        assert_eq!(
            bone_driven_bodies(&rig),
            vec![true, true, true, false, false],
            "a weld chain from an animation-driven body is animation-driven; \
             a hinge is where freedom begins"
        );
    }

    /// Builds a two-body rig around `joint` and returns the Rapier joint the
    /// bridge produced for it, which is what every constraint-mapping case
    /// below inspects.
    fn built_joint(joint: JointDef) -> rapier3d::prelude::GenericJoint {
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0), BoneId(1)],
            asset: None,
        };
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "mapping".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![
                chain_body("anchor", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body("hair", 1, 0.1, RigidBodyMode::Dynamic),
            ],
            joints: vec![joint],
        };
        let worlds = vec![Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, -0.1, 0.0))];
        let state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &worlds,
            &worlds,
        );
        state
            .world
            .impulse_joints
            .iter()
            .next()
            .expect("the authored joint must be inserted")
            .1
            .data
    }

    /// A joint whose every axis is authored as a range, so a case can pin one
    /// axis to what it is about without inheriting another case's encoding.
    fn ranged_joint() -> JointDef {
        JointDef {
            name: "joint".to_owned(),
            body_a: Some(0),
            body_b: Some(1),
            translation: [0.0, -0.1, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation_lower: [-0.01; 3],
            translation_upper: [0.01; 3],
            rotation_lower: [-0.5; 3],
            rotation_upper: [0.5; 3],
            spring_translation: [0.0; 3],
            spring_rotation: [0.0; 3],
        }
    }

    /// PMX writes a pinned degree of freedom as an empty range, which Bullet
    /// reads as an equality constraint. Expressing that as a Rapier limit
    /// instead leaves the axis held only by whatever motor happens to sit on
    /// it — which is how an imported rig comes to depend on springs its
    /// author never intended to act (ADR 0108 §2).
    #[test]
    fn an_empty_pmx_range_locks_the_axis_instead_of_limiting_it() {
        let mut joint = ranged_joint();
        joint.translation_lower = [0.0; 3];
        joint.translation_upper = [0.0; 3];
        joint.rotation_lower[1] = 0.0;
        joint.rotation_upper[1] = 0.0;
        let built = built_joint(joint);

        assert!(built.locked_axes.contains(JointAxesMask::LIN_AXES));
        assert!(built.locked_axes.contains(JointAxesMask::ANG_Y));
        assert!(!built.limit_axes.contains(JointAxesMask::LIN_X));
        assert!(!built.limit_axes.contains(JointAxesMask::ANG_Y));
        // The axes this case did not pin must stay ranged rather than being
        // swept into the lock.
        assert!(!built.locked_axes.contains(JointAxesMask::ANG_X));
        assert!(built.limit_axes.contains(JointAxesMask::ANG_X));
    }

    /// PMX writes an unconstrained degree of freedom as an inverted range.
    /// Handing that to Rapier verbatim asks for a limit whose minimum exceeds
    /// its maximum, which constrains an axis its author left free.
    #[test]
    fn an_inverted_pmx_range_leaves_the_axis_unconstrained() {
        let mut joint = ranged_joint();
        joint.rotation_lower[2] = 0.5;
        joint.rotation_upper[2] = -0.5;
        let built = built_joint(joint);

        assert!(!built.locked_axes.contains(JointAxesMask::ANG_Z));
        assert!(!built.limit_axes.contains(JointAxesMask::ANG_Z));
    }

    #[test]
    fn a_proper_pmx_range_becomes_a_rapier_limit() {
        let built = built_joint(ranged_joint());

        assert!(built.limit_axes.contains(JointAxesMask::ANG_AXES));
        let limit = built
            .limits(JointAxis::AngX)
            .expect("a ranged axis must carry a limit");
        assert!((limit.min + 0.5).abs() < 1.0e-6);
        assert!((limit.max - 0.5).abs() < 1.0e-6);
    }

    /// A PMX spring is a force per unit of error, tuned against the body's
    /// own mass and inertia in PMX units. Rapier's default motor model
    /// divides that same mass back out, and its angular constant lives in
    /// meters, so passing the authored number through unconverted is two
    /// separate errors (ADR 0108 §3).
    #[test]
    fn an_authored_spring_becomes_a_force_based_motor_in_solver_units() {
        let mut joint = ranged_joint();
        joint.spring_translation = [36900.0; 3];
        joint.spring_rotation = [30.0; 3];
        let built = built_joint(joint);

        let linear = built
            .motor(JointAxis::LinX)
            .expect("a translation spring must motorize its axis");
        assert_eq!(linear.model, MotorModel::ForceBased);
        assert!((linear.stiffness - 36900.0).abs() < 1.0e-3);
        assert_eq!(linear.damping, 0.0);

        let angular = built
            .motor(JointAxis::AngX)
            .expect("a rotation spring must motorize its axis");
        assert_eq!(angular.model, MotorModel::ForceBased);
        let expected = 30.0 * PMX_AUTHORING_SCALE * PMX_AUTHORING_SCALE;
        assert!(
            (angular.stiffness - expected).abs() < 1.0e-6,
            "expected {expected} N m/rad, got {}",
            angular.stiffness
        );
    }

    /// Bullet resolves a locked axis through its equality limit and never
    /// through the spring sharing that axis, so a spring authored there is
    /// inert in MMD. Most of a real model's springs are authored exactly that
    /// way, and motorizing them fights the lock rather than reproducing it.
    #[test]
    fn a_spring_on_a_locked_axis_is_not_motorized() {
        let mut joint = ranged_joint();
        joint.rotation_lower[0] = 0.0;
        joint.rotation_upper[0] = 0.0;
        joint.spring_rotation = [30.0; 3];
        let built = built_joint(joint);

        assert!(built.locked_axes.contains(JointAxesMask::ANG_X));
        assert!(built.motor(JointAxis::AngX).is_none());
        assert!(built.motor(JointAxis::AngY).is_some());
    }

    /// A Bullet equality limit remains a locked axis even when the authored
    /// lock is at a non-zero coordinate. Rapier cannot express that offset in
    /// `locked_axes`, so it is represented as an equal min/max limit instead;
    /// that representation detail must not make the spring active again.
    #[test]
    fn a_spring_on_a_nonzero_locked_axis_is_not_motorized() {
        let mut joint = ranged_joint();
        joint.translation_lower[0] = 0.02;
        joint.translation_upper[0] = 0.02;
        joint.spring_translation[0] = 100.0;
        let built = built_joint(joint);

        assert!(!built.locked_axes.contains(JointAxesMask::LIN_X));
        let limit = built
            .limits(JointAxis::LinX)
            .expect("a non-zero equality lock must keep its offset");
        assert!((limit.min - 0.02).abs() < 1.0e-6);
        assert!((limit.max - 0.02).abs() < 1.0e-6);
        assert!(built.motor(JointAxis::LinX).is_none());
    }

    /// Bullet and Rapier use different discrete damping formulas, so the
    /// conversion has to agree at each actual fixed step rather than only in
    /// the continuous-time limit.
    #[test]
    fn pmx_damping_matches_bullet_at_the_solver_timestep() {
        let dt = 1.0 / 60.0;
        assert_eq!(rapier_damping_rate(0.0, dt), 0.0);

        for damping in [0.1, 0.9, 0.999, 0.9999] {
            let rate = rapier_damping_rate(damping, dt);
            let rapier_retention = 1.0 / (1.0 + dt * rate);
            let bullet_retention = (1.0 - damping).powf(dt);
            assert!(
                (rapier_retention - bullet_retention).abs() < 1.0e-6,
                "damping {damping} retained {rapier_retention} in Rapier, \
                 but Bullet retains {bullet_retention}"
            );
        }

        // Bullet clamps the authored factor to one and then erases the
        // velocity outright, which no finite rate reproduces.
        assert_eq!(rapier_damping_rate(1.0, dt), FULLY_DAMPED_RATE);
        assert_eq!(rapier_damping_rate(2.0, dt), FULLY_DAMPED_RATE);
        assert_eq!(rapier_damping_rate(f32::NAN, dt), FULLY_DAMPED_RATE);
        assert_eq!(rapier_damping_rate(-1.0, dt), 0.0);
    }

    #[test]
    fn presentation_transform_interpolates_all_local_channels() {
        let previous = Transform::default();
        let current = Transform {
            translation: Vec3::new(2.0, 4.0, 6.0),
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(3.0),
        };

        let halfway = interpolate_presentation_transform(&previous, &current, 0.5);
        assert!((halfway.translation - Vec3::new(1.0, 2.0, 3.0)).length() < 1.0e-6);
        assert!((halfway.scale - Vec3::splat(2.0)).length() < 1.0e-6);
        let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
        assert!(halfway.rotation.dot(expected).abs() > 1.0 - 1.0e-5);
    }

    /// The authored constants were tuned under MMD's gravity, which is weaker
    /// than Earth's once expressed at this engine's import scale. Simulating
    /// under Rapier's default instead shortens every chain's pendulum period.
    #[test]
    fn a_rig_falls_under_mmd_gravity_rather_than_rapiers_default() {
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let mut body = chain_body("free", 0, 1.0, RigidBodyMode::Dynamic);
        body.linear_damping = 0.0;
        body.angular_damping = 0.0;
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "falling".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };
        let worlds = vec![Mat4::IDENTITY];
        let mut state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &worlds,
            &worlds,
        );
        state.world.integration_parameters.dt = 1.0 / 60.0;
        for _ in 0..60 {
            state.world.step();
        }

        let speed = -state
            .world
            .bodies
            .get(state.body_handles[0])
            .expect("the falling body must exist")
            .linvel()
            .y;
        let expected = 9.8 * 10.0 * PMX_AUTHORING_SCALE;
        assert!(
            (speed - expected).abs() < 0.2,
            "one second of free fall reached {speed} m/s, but MMD gravity is {expected} m/s^2"
        );
    }

    /// A PMX hair chain, authored the way a real model authors one — every
    /// translation axis pinned, two rotation axes pinned, springs sitting on
    /// pinned axes, and the per-body damping a hair chain really carries —
    /// must stay within its own reach while the character sways underneath it.
    ///
    /// This guards the assembled behavior, not the individual mappings ADR
    /// 0108 defines; those have their own cases above. A chain this short
    /// cannot reproduce what a whole rig does — the divergence that motivated
    /// the ADR needed all 284 bodies and 318 joints of a real model — so the
    /// bounds here are divergence checks rather than fidelity ones.
    #[test]
    fn a_pmx_authored_chain_stays_bounded_while_its_character_sways() {
        const LINKS: usize = 8;
        const SPACING: f32 = 0.06;
        const ANCHOR_HEIGHT: f32 = 1.4;

        let mut rest_translations = vec![Vec3::new(0.0, ANCHOR_HEIGHT, 0.0)];
        let mut parents = vec![None];
        for index in 1..=LINKS {
            rest_translations.push(Vec3::new(0.0, -SPACING, 0.0));
            parents.push(Some(index - 1));
        }
        let skeleton_asset = test_skeleton_asset(&rest_translations, &parents);

        let bodies = (0..=LINKS)
            .map(|index| {
                let mode = if index == 0 {
                    RigidBodyMode::FollowBone
                } else {
                    RigidBodyMode::Dynamic
                };
                // The masses a hair chain actually carries: the project's own
                // model keeps adjacent dynamic bodies within about 1.5x of
                // each other, so an unrepresentative ratio is not what this
                // case is testing.
                let mut body = chain_body(&format!("link{index}"), index as u32, 0.25, mode);
                body.shape = RigidBodyShape::Sphere { radius: 0.024 };
                // The damping a PMX author actually writes for hair: almost
                // all velocity gone within a step under MMD's convention.
                body.linear_damping = 0.999;
                body.angular_damping = 0.9999;
                body
            })
            .collect::<Vec<_>>();
        let joints = (1..=LINKS)
            .map(|index| JointDef {
                name: format!("link{index}"),
                body_a: Some(index - 1),
                body_b: Some(index),
                translation: [0.0, ANCHOR_HEIGHT - SPACING * index as f32, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                // Swing about Z only; X and Y are pinned, which is where a
                // real model puts most of its rotation springs.
                rotation_lower: [0.0, 0.0, -0.3],
                rotation_upper: [0.0, 0.0, 0.3],
                spring_translation: [36900.0; 3],
                spring_rotation: [30.0; 3],
            })
            .collect::<Vec<_>>();

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(MmdPhysicsWorlds::new());
        let rig_id = AssetId::generate();
        let mut registry = RigidBodyRigRegistry::new();
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "hair".to_owned(),
            skeleton: Some(skeleton_asset.id.clone()),
            skeleton_identity: Some(skeleton_asset.identity),
            bodies,
            joints,
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: Vec::new(),
                    bone_ids: (0..=LINKS).map(|index| BoneId(index as u32)).collect(),
                    asset: Some(skeleton_asset.id.clone()),
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(rig_entity, GlobalTransform::identity())
            .expect("rig global transform");
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");
        app.add_system(crate::rig_pose::rig_pose_clear_transient_system);
        app.add_system(mmd_rigid_body_physics_system);

        let chain_length = SPACING * LINKS as f32;
        let mut worst_reach = 0.0_f32;
        let mut worst_stretch = 0.0_f32;
        for step in 0..900 {
            // A whole-body sway, which is the motion a dance clip applies to
            // every secondary-motion chain at once.
            let angle = (step as f32 / 60.0 * std::f32::consts::TAU).sin() * 0.5;
            let root = Mat4::from_rotation_z(angle);
            app.world_mut()
                .get_component_mut::<GlobalTransform>(rig_entity)
                .expect("rig global transform")
                .0 = root;
            app.update().expect("secondary-motion step");

            let rig_pose = app
                .world_mut()
                .get_component_mut::<RigPose>(rig_entity)
                .expect("rig pose must remain alive");
            let worlds = rig_pose.evaluate_world(PoseStage::Physics, root).to_vec();
            let anchor = worlds[0].w_axis.truncate();
            for (index, world) in worlds.iter().enumerate().skip(1) {
                let position = world.w_axis.truncate();
                assert!(
                    position.is_finite(),
                    "the chain produced a non-finite pose at step {step}"
                );
                worst_reach = worst_reach.max((position - anchor).length());
                let spacing = (position - worlds[index - 1].w_axis.truncate()).length();
                worst_stretch = worst_stretch.max((spacing - SPACING).abs());
            }
        }

        assert!(
            worst_reach < chain_length * 1.5,
            "the chain reached {worst_reach} m from its anchor, but its links total {chain_length} m"
        );
        // Every translation axis of every joint is pinned, so a link that
        // doubles or collapses means the constraint holding it stopped acting.
        // Rapier resolves these iteratively, so the bound is a divergence
        // check rather than a tolerance on the solver's residual.
        assert!(
            worst_stretch < SPACING,
            "link spacing moved {worst_stretch} m from the authored {SPACING} m"
        );
    }

    /// The whole fixed-step pose pipeline — clear, animate, propagate,
    /// simulate, publish, propagate — must stay bounded while a looping clip
    /// drives the anchor bone.
    ///
    /// This is the regression the layered pose runtime exists for. Before
    /// ADR 0106 the physics write-back landed in the same joint `Transform`
    /// the next frame's animation read back, and an unkeyed hair bone
    /// therefore accumulated its own correction until the skinned mesh drew
    /// as a spike. Simulating physics alone cannot reproduce that; the loop
    /// has to include animation and publication.
    #[test]
    fn the_full_pose_pipeline_stays_bounded_while_animating() {
        use crate::animation::{
            animation_system, AnimChannel, AnimProperty, AnimationClip, Animator, Keyframe,
        };
        use crate::asset::Assets;
        use crate::rig_pose::rig_pose_clear_transient_system;

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(MmdPhysicsWorlds::new());

        // The anchor swings a full radian back and forth once a second, so
        // every loop wrap is also a large pose jump for the hair body.
        let mut clips = Assets::<AnimationClip>::new();
        let clip = clips.add(AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Rotation,
                target_bone: Some(BoneId(0)),
                keyframes: vec![
                    Keyframe {
                        time: 0.0,
                        value: Quat::IDENTITY.to_array(),
                    },
                    Keyframe {
                        time: 0.5,
                        value: Quat::from_rotation_z(1.0).to_array(),
                    },
                    Keyframe {
                        time: 1.0,
                        value: Quat::IDENTITY.to_array(),
                    },
                ],
            }],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        app.insert_resource(clips);

        let anchor_bone = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::Y))
            .expect("anchor bone");
        app.world_mut()
            .add_component(anchor_bone, GlobalTransform(Mat4::from_translation(Vec3::Y)))
            .expect("anchor global transform");
        let hair_bone = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::new(0.0, -0.1, 0.0)))
            .expect("hair bone");
        app.world_mut()
            .add_component(
                hair_bone,
                GlobalTransform(Mat4::from_translation(Vec3::new(0.0, 0.9, 0.0))),
            )
            .expect("hair global transform");
        app.world_mut()
            .add_component(hair_bone, Parent(anchor_bone))
            .expect("hair parent");

        let skeleton_asset =
            test_skeleton_asset(&[Vec3::Y, Vec3::new(0.0, -0.1, 0.0)], &[None, Some(0)]);
        let rig_id = AssetId::generate();
        let mut registry = RigidBodyRigRegistry::new();
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "animated_chain".to_owned(),
            skeleton: Some(skeleton_asset.id.clone()),
            skeleton_identity: Some(skeleton_asset.identity),
            bodies: vec![
                chain_body("anchor", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body("hair", 1, 0.1, RigidBodyMode::DynamicWithBonePosition),
            ],
            joints: vec![JointDef {
                name: "hair".to_owned(),
                body_a: Some(0),
                body_b: Some(1),
                translation: [0.0, 0.9, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                rotation_lower: [-0.5; 3],
                rotation_upper: [0.5; 3],
                spring_translation: [0.0; 3],
                spring_rotation: [0.0; 3],
            }],
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: vec![anchor_bone, hair_bone],
                    bone_ids: vec![BoneId(0), BoneId(1)],
                    asset: Some(skeleton_asset.id.clone()),
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(rig_entity, GlobalTransform::identity())
            .expect("rig global transform");
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");
        app.world_mut()
            .add_component(rig_entity, Animator::playing(clip))
            .expect("rig animator");

        // The production fixed-step order from ADR 0106.
        app.add_system(rig_pose_clear_transient_system);
        app.add_system(animation_system);
        app.add_system(crate::transform::transform_propagation_system);
        app.add_system(mmd_rigid_body_physics_system);
        app.add_system(publish_final_rig_pose_system);
        app.add_system(crate::transform::transform_propagation_system);

        let mut worst_separation = 0.0_f32;
        for _ in 0..900 {
            app.update().expect("fixed pose step");
            let world_of = |entity| {
                app.world()
                    .get_component::<GlobalTransform>(entity)
                    .expect("bone global transform")
                    .matrix()
                    .w_axis
                    .truncate()
            };
            let separation = (world_of(hair_bone) - world_of(anchor_bone)).length();
            assert!(
                separation.is_finite(),
                "the animated chain produced a non-finite pose"
            );
            worst_separation = worst_separation.max(separation);
        }

        // The bones rest 0.1 m apart. A drifting write-back grows this without
        // bound, so any generous constant fails once accumulation returns.
        assert!(
            worst_separation < 0.5,
            "the hair bone drifted {worst_separation} m from its anchor, which rests 0.1 m away"
        );

        let rig_pose = app
            .world()
            .get_component::<RigPose>(rig_entity)
            .expect("rig pose must remain alive");
        assert!(
            !rig_pose
                .physics_layer()
                .channels(1)
                .expect("mode 2 pose channels")
                .contains(PoseChannels::TRANSLATION),
            "PMX mode 2 must leave translation to the animation pose"
        );
    }

    /// A chain of secondary-motion bones must stay near the anchor its joint
    /// binds it to. A solver that diverges pulls the bone arbitrarily far,
    /// which reaches the screen as a skinned mesh stretched into a spike.
    #[test]
    fn a_joined_mode2_chain_stays_bounded_for_600_steps() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(MmdPhysicsWorlds::new());

        let anchor_bone = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::Y))
            .expect("anchor bone");
        app.world_mut()
            .add_component(anchor_bone, GlobalTransform(Mat4::from_translation(Vec3::Y)))
            .expect("anchor global transform");

        let hair_bone = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::new(0.0, -0.1, 0.0)))
            .expect("hair bone");
        app.world_mut()
            .add_component(
                hair_bone,
                GlobalTransform(Mat4::from_translation(Vec3::new(0.0, 0.9, 0.0))),
            )
            .expect("hair global transform");
        app.world_mut()
            .add_component(hair_bone, Parent(anchor_bone))
            .expect("hair parent");

        let rig_id = AssetId::generate();
        let mut registry = RigidBodyRigRegistry::new();
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "chain".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![
                chain_body("anchor", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body(
                    "hair",
                    1,
                    0.1,
                    RigidBodyMode::DynamicWithBonePosition,
                ),
            ],
            joints: vec![JointDef {
                name: "hair".to_owned(),
                body_a: Some(0),
                body_b: Some(1),
                translation: [0.0, 0.9, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                // PMX hair and skirt joints pin translation outright and let
                // the bone swing only within a rotation range.
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                rotation_lower: [-0.5; 3],
                rotation_upper: [0.5; 3],
                spring_translation: [0.0; 3],
                spring_rotation: [0.0; 3],
            }],
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: vec![anchor_bone, hair_bone],
                    bone_ids: vec![BoneId(0), BoneId(1)],
                    asset: None,
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(rig_entity, GlobalTransform::identity())
            .expect("rig global transform");
        let skeleton_asset =
            test_skeleton_asset(&[Vec3::Y, Vec3::new(0.0, -0.1, 0.0)], &[None, Some(0)]);
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");

        app.add_system(mmd_rigid_body_physics_system);
        app.add_system(publish_final_rig_pose_system);
        app.add_system(crate::transform::transform_propagation_system);
        for _ in 0..600 {
            app.update().expect("secondary-motion step");
        }

        let world_of = |entity| {
            app.world()
                .get_component::<GlobalTransform>(entity)
                .expect("bone global transform")
                .matrix()
                .w_axis
                .truncate()
        };
        let separation = (world_of(hair_bone) - world_of(anchor_bone)).length();
        assert!(
            separation < 0.5,
            "the joined bone drifted {separation} m from its anchor, which rests 0.1 m away"
        );
        let rig_pose = app
            .world()
            .get_component::<RigPose>(rig_entity)
            .expect("rig pose must remain alive");
        let mode2_channels = rig_pose
            .physics_layer()
            .channels(1)
            .expect("mode 2 pose channels");
        assert!(mode2_channels.contains(PoseChannels::ROTATION));
        assert!(
            !mode2_channels.contains(PoseChannels::TRANSLATION),
            "PMX mode 2 must retain the animation/procedural bone position"
        );
        let worlds = app
            .world()
            .get_resource::<MmdPhysicsWorlds>()
            .expect("physics worlds");
        let state = worlds
            .characters
            .get(&rig_entity)
            .expect("character physics world");
        let simulated = state
            .world
            .bodies
            .get(state.body_handles[1])
            .expect("mode 2 body")
            .position();
        assert!(simulated.translation.is_finite());
        assert!(simulated.rotation.is_finite());
    }

    /// A chain deeper than one link, under a scaled model root, must keep
    /// its authored bone spacing. Resolving a physics-driven parent through
    /// a scaleless pose while the propagation pass reapplies the root scale
    /// multiplies every link, which reaches the screen as a mesh drawn out
    /// into a spike that lengthens toward the tip.
    #[test]
    fn a_chain_under_a_scaled_root_keeps_its_bone_spacing() {
        const ROOT_SCALE: f32 = 2.0;
        const LOCAL_SPACING: f32 = 0.1;
        let world_spacing = LOCAL_SPACING * ROOT_SCALE;

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(MmdPhysicsWorlds::new());

        let root = app
            .world_mut()
            .spawn_with(Transform {
                scale: Vec3::splat(ROOT_SCALE),
                ..Transform::default()
            })
            .expect("model root");
        app.world_mut()
            .add_component(root, GlobalTransform(Mat4::from_scale(Vec3::splat(ROOT_SCALE))))
            .expect("root global transform");

        // Three bones: an animation-driven anchor and two simulated links,
        // so the deepest one resolves its parent through the physics path.
        let mut bones = Vec::new();
        let mut parent = root;
        let mut parent_world = Mat4::from_scale(Vec3::splat(ROOT_SCALE));
        for index in 0..3 {
            let local = if index == 0 {
                Vec3::new(0.0, 0.5, 0.0)
            } else {
                Vec3::new(0.0, -LOCAL_SPACING, 0.0)
            };
            let world = parent_world * Mat4::from_translation(local);
            let bone = app
                .world_mut()
                .spawn_with(Transform::from_translation(local))
                .expect("bone");
            app.world_mut()
                .add_component(bone, GlobalTransform(world))
                .expect("bone global transform");
            app.world_mut()
                .add_component(bone, Parent(parent))
                .expect("bone parent");
            bones.push(bone);
            parent = bone;
            parent_world = world;
        }

        let rig_id = AssetId::generate();
        let mut registry = RigidBodyRigRegistry::new();
        let joint_between = |a: usize, b: usize, height: f32| JointDef {
            name: format!("link{b}"),
            body_a: Some(a),
            body_b: Some(b),
            translation: [0.0, height, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation_lower: [0.0; 3],
            translation_upper: [0.0; 3],
            rotation_lower: [-0.5; 3],
            rotation_upper: [0.5; 3],
            spring_translation: [0.0; 3],
            spring_rotation: [0.0; 3],
        };
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "skirt".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![
                chain_body("anchor", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body("link1", 1, 0.5, RigidBodyMode::Dynamic),
                chain_body("link2", 2, 0.5, RigidBodyMode::Dynamic),
            ],
            joints: vec![
                joint_between(0, 1, 1.0 - world_spacing),
                joint_between(1, 2, 1.0 - world_spacing * 2.0),
            ],
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: bones.clone(),
                    bone_ids: vec![BoneId(0), BoneId(1), BoneId(2)],
                    asset: None,
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(
                rig_entity,
                GlobalTransform(Mat4::from_scale(Vec3::splat(ROOT_SCALE))),
            )
            .expect("rig global transform");
        let skeleton_asset = test_skeleton_asset(
            &[
                Vec3::new(0.0, 0.5, 0.0),
                Vec3::new(0.0, -LOCAL_SPACING, 0.0),
                Vec3::new(0.0, -LOCAL_SPACING, 0.0),
            ],
            &[None, Some(0), Some(1)],
        );
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");

        app.add_system(mmd_rigid_body_physics_system);
        app.add_system(publish_final_rig_pose_system);
        app.add_system(crate::transform::transform_propagation_system);
        for _ in 0..60 {
            app.update().expect("secondary-motion step");
        }

        let world_of = |entity| {
            app.world()
                .get_component::<GlobalTransform>(entity)
                .expect("bone global transform")
                .matrix()
                .w_axis
                .truncate()
        };
        let deep_spacing = (world_of(bones[2]) - world_of(bones[1])).length();
        assert!(
            (deep_spacing - world_spacing).abs() < world_spacing * 0.5,
            "the deepest link sits {deep_spacing} m from its parent, but the rig authored {world_spacing} m"
        );
    }

    #[test]
    fn mmd_groups_preserve_pmx_membership_and_mask() {
        let body = RigidBodyDef {
            name: "hair".to_owned(),
            bone: Some(BoneId(0)),
            bone_name: "hair".to_owned(),
            shape: RigidBodyShape::Sphere { radius: 0.1 },
            bone_offset_translation: [0.0; 3],
            bone_offset_rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 1.0,
            linear_damping: 0.1,
            angular_damping: 0.1,
            restitution: 0.0,
            friction: 0.5,
            mode: RigidBodyMode::Dynamic,
            group: 3,
            collides_with: 1 << 5,
        };
        let groups = mmd_collision_groups(&body);
        assert_eq!(groups.memberships.bits(), 1 << 3);
        assert_eq!(groups.filter.bits(), 1 << 5);

        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "test".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        };
        assert_eq!(rig.dynamic_body_count(), 1);
    }

    #[test]
    fn dynamic_with_bone_position_body_is_not_teleported_to_its_bone() {
        let mut world = engine_ecs::World::new();
        let joint = world
            .spawn_with(Transform::from_translation(Vec3::Y))
            .expect("joint must spawn");
        world
            .add_component(
                joint,
                GlobalTransform(Mat4::from_translation(Vec3::Y)),
            )
            .expect("joint must accept GlobalTransform");

        let skeleton = Skeleton {
            joints: vec![joint],
            bone_ids: vec![BoneId(0)],
            asset: None,
        };
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "mode2".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![chain_body(
                "hair",
                0,
                0.1,
                RigidBodyMode::DynamicWithBonePosition,
            )],
            joints: Vec::new(),
        };

        let procedural_worlds = vec![Mat4::from_translation(Vec3::Y)];
        let mut state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &procedural_worlds,
            &procedural_worlds,
        );
        let simulated_pose = Pose::from_parts(
            Vector::new(0.0, -2.0, 0.0),
            Rotation::IDENTITY,
        );
        let handle = state.body_handles[0];
        state
            .world
            .bodies
            .get_mut(handle)
            .expect("mode 2 body must exist")
            .set_position(simulated_pose, true);

        drive_follow_bodies(
            &mut state,
            &rig,
            &animated_body_poses(&rig, &joint_indices(&skeleton), &procedural_worlds),
        );

        let resulting_y = state
            .world
            .bodies
            .get(handle)
            .expect("mode 2 body must remain alive")
            .position()
            .translation
            .y;
        assert!((resulting_y + 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn joint_frames_are_derived_from_rest_pose_not_initial_animation() {
        let skeleton_asset = test_skeleton_asset(
            &[Vec3::ZERO, Vec3::new(0.0, -0.1, 0.0)],
            &[None, Some(0)],
        );
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0), BoneId(1)],
            asset: Some(skeleton_asset.id.clone()),
        };
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "animated_chain".to_owned(),
            skeleton: Some(skeleton_asset.id.clone()),
            skeleton_identity: Some(skeleton_asset.identity),
            bodies: vec![
                chain_body("anchor", 0, 1.0, RigidBodyMode::FollowBone),
                chain_body("hair", 1, 0.1, RigidBodyMode::Dynamic),
            ],
            joints: vec![JointDef {
                name: "hair_joint".to_owned(),
                body_a: Some(0),
                body_b: Some(1),
                translation: [0.0, -0.1, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                rotation_lower: [-0.5; 3],
                rotation_upper: [0.5; 3],
                spring_translation: [0.0; 3],
                spring_rotation: [0.0; 3],
            }],
        };
        let mut rig_pose = RigPose::from_skeleton(&skeleton_asset);
        let rest_worlds = rig_pose
            .evaluate_world(PoseStage::Rest, Mat4::IDENTITY)
            .to_vec();

        // Start the solver from a translated and articulated animation pose.
        // The joint's local frames must nevertheless remain the compact PMX
        // rest-pose offsets instead of pointing back toward the rest model.
        rig_pose
            .animation_layer_mut()
            .write_translation(0, Vec3::new(10.0, 0.0, 0.0));
        rig_pose
            .animation_layer_mut()
            .write_rotation(0, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        let procedural_worlds = rig_pose
            .evaluate_world(PoseStage::Procedural, Mat4::IDENTITY)
            .to_vec();
        let state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &rest_worlds,
            &procedural_worlds,
        );

        let (_, joint) = state
            .world
            .impulse_joints
            .iter()
            .next()
            .expect("the authored joint must be inserted");
        let frame_a_translation = matrix_from_pose(joint.data.local_frame1)
            .w_axis
            .truncate();
        let frame_b_translation = matrix_from_pose(joint.data.local_frame2)
            .w_axis
            .truncate();
        assert!(
            frame_a_translation.distance(Vec3::new(0.0, -0.1, 0.0)) < 1.0e-5,
            "anchor frame must use its rest-pose offset, got {frame_a_translation:?}"
        );
        assert!(
            frame_b_translation.length() < 1.0e-5,
            "child frame must remain at the child's rest origin, got {frame_b_translation:?}"
        );
    }

    #[test]
    fn dynamic_child_localizes_against_a_physics_ancestor_through_a_spacer_bone() {
        let skeleton_asset = test_skeleton_asset(
            &[Vec3::ZERO, Vec3::Y, Vec3::Y],
            &[None, Some(0), Some(1)],
        );
        let skeleton = Skeleton {
            joints: Vec::new(),
            bone_ids: vec![BoneId(0), BoneId(1), BoneId(2)],
            asset: Some(skeleton_asset.id.clone()),
        };
        let rig = RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: AssetId::generate(),
            name: "spacer_chain".to_owned(),
            skeleton: Some(skeleton_asset.id.clone()),
            skeleton_identity: Some(skeleton_asset.identity),
            bodies: vec![
                chain_body("physics_root", 0, 1.0, RigidBodyMode::Dynamic),
                chain_body("physics_child", 2, 1.0, RigidBodyMode::Dynamic),
            ],
            joints: Vec::new(),
        };
        let mut rig_pose = RigPose::from_skeleton(&skeleton_asset);
        let procedural_worlds = rig_pose
            .evaluate_world(PoseStage::Procedural, Mat4::IDENTITY)
            .to_vec();
        let mut state = build_character_world(
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &procedural_worlds,
            &procedural_worlds,
        );
        state
            .world
            .bodies
            .get_mut(state.body_handles[0])
            .expect("root body")
            .set_position(
                Pose::from_parts(Vector::new(5.0, 0.0, 0.0), Rotation::IDENTITY),
                true,
            );
        state
            .world
            .bodies
            .get_mut(state.body_handles[1])
            .expect("child body")
            .set_position(
                Pose::from_parts(Vector::new(5.0, 2.0, 0.0), Rotation::IDENTITY),
                true,
            );

        write_simulated_bones(
            &mut state,
            &rig,
            &joint_indices(&skeleton),
            Mat4::IDENTITY,
            &procedural_worlds,
            &mut rig_pose,
        );

        let child_local = rig_pose
            .physics_layer()
            .transform(2)
            .expect("physics child local transform");
        assert!(
            child_local.translation.distance(Vec3::Y) < 1.0e-5,
            "the inherited ancestor correction must not be written into the child local translation: {:?}",
            child_local.translation
        );
        let child_world = rig_pose
            .evaluate_world(PoseStage::Physics, Mat4::IDENTITY)
            .get(2)
            .copied()
            .expect("physics child world")
            .to_scale_rotation_translation()
            .2;
        assert!(child_world.distance(Vec3::new(5.0, 2.0, 0.0)) < 1.0e-5);
    }

    #[test]
    fn isolated_solver_writes_dynamic_translation_to_the_physics_layer() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 30.0));
        app.insert_resource(MmdPhysicsWorlds::new());

        let joint = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::Y))
            .expect("joint");
        app.world_mut()
            .add_component(
                joint,
                GlobalTransform(Mat4::from_translation(Vec3::Y)),
            )
            .expect("joint global transform");

        let rig_id = AssetId::generate();
        let body = RigidBodyDef {
            name: "hair".to_owned(),
            bone: Some(BoneId(0)),
            bone_name: "hair".to_owned(),
            shape: RigidBodyShape::Sphere { radius: 0.1 },
            bone_offset_translation: [0.0; 3],
            bone_offset_rotation: [0.0, 0.0, 0.0, 1.0],
            mass: 1.0,
            linear_damping: 0.0,
            angular_damping: 0.0,
            restitution: 0.0,
            friction: 0.5,
            mode: RigidBodyMode::Dynamic,
            group: 0,
            collides_with: u16::MAX,
        };
        let mut registry = RigidBodyRigRegistry::new();
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "test".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies: vec![body],
            joints: Vec::new(),
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: vec![joint],
                    bone_ids: vec![BoneId(0)],
                    asset: None,
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(rig_entity, GlobalTransform::identity())
            .expect("rig global transform");
        let skeleton_asset = test_skeleton_asset(&[Vec3::Y], &[None]);
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");
        app.add_system(mmd_rigid_body_physics_system);
        app.update().expect("secondary-motion step");

        let rig_pose = app
            .world()
            .get_component::<RigPose>(rig_entity)
            .expect("physics-driven rig must keep its pose");
        assert!(
            rig_pose
                .physics_layer()
                .channels(0)
                .expect("bone channel")
                .contains(PoseChannels::TRANSLATION),
            "PMX mode 1 must own local translation in the physics layer"
        );
        assert_eq!(
            app.world()
                .get_resource::<MmdPhysicsWorlds>()
                .unwrap()
                .simulated_character_count(),
            1
        );
    }

    /// A looping clip repositions the whole skeleton in a single fixed step
    /// when it wraps back to its first frame. Feeding that jump to the solver
    /// as motion gives every kinematic body a velocity no animation could
    /// produce, and the impulse joints throw the attached hair and skirt
    /// chains straight through the limits that should hold them — which is
    /// what reaches the screen as a skinned mesh stretched into spikes.
    #[test]
    fn an_animation_pose_jump_does_not_fling_a_jointed_chain() {
        // A PMX hair chain is many short links deep, which is what lets one
        // oversized impulse accumulate along it; a single link absorbs the
        // same jump without ever leaving its limits.
        const LINKS: usize = 8;
        const SPACING: f32 = 0.1;
        const ANCHOR_HEIGHT: f32 = 1.0;
        const JUMP: f32 = 3.0;

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(MmdPhysicsWorlds::new());

        let mut bones = Vec::new();
        let mut rest_translations = Vec::new();
        let mut parents = Vec::new();
        for index in 0..=LINKS {
            let local = if index == 0 {
                Vec3::new(0.0, ANCHOR_HEIGHT, 0.0)
            } else {
                Vec3::new(0.0, -SPACING, 0.0)
            };
            let world = Vec3::new(0.0, ANCHOR_HEIGHT - SPACING * index as f32, 0.0);
            let bone = app
                .world_mut()
                .spawn_with(Transform::from_translation(local))
                .expect("bone");
            app.world_mut()
                .add_component(bone, GlobalTransform(Mat4::from_translation(world)))
                .expect("bone global transform");
            if index > 0 {
                app.world_mut()
                    .add_component(bone, Parent(bones[index - 1]))
                    .expect("bone parent");
            }
            bones.push(bone);
            rest_translations.push(local);
            parents.push(index.checked_sub(1));
        }

        let rig_id = AssetId::generate();
        let mut registry = RigidBodyRigRegistry::new();
        let bodies = (0..=LINKS)
            .map(|index| {
                let mode = if index == 0 {
                    RigidBodyMode::FollowBone
                } else {
                    RigidBodyMode::Dynamic
                };
                chain_body(&format!("link{index}"), index as u32, 0.125, mode)
            })
            .collect::<Vec<_>>();
        let joints = (1..=LINKS)
            .map(|index| JointDef {
                name: format!("link{index}"),
                body_a: Some(index - 1),
                body_b: Some(index),
                translation: [0.0, ANCHOR_HEIGHT - SPACING * index as f32, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                // PMX hair joints pin translation outright and let each link
                // swing only within a narrow rotation range.
                translation_lower: [0.0; 3],
                translation_upper: [0.0; 3],
                rotation_lower: [-0.5; 3],
                rotation_upper: [0.5; 3],
                spring_translation: [0.0; 3],
                spring_rotation: [0.0; 3],
            })
            .collect::<Vec<_>>();
        registry.insert(RigidBodyRigAsset {
            schema_version: RIGID_BODY_RIG_SCHEMA_VERSION,
            id: rig_id.clone(),
            name: "hair".to_owned(),
            skeleton: None,
            skeleton_identity: None,
            bodies,
            joints,
        });
        app.insert_resource(registry);

        let rig_entity = app
            .world_mut()
            .spawn_with(RigidBodyPhysics::new(rig_id))
            .expect("rig marker");
        app.world_mut()
            .add_component(
                rig_entity,
                Skeleton {
                    joints: bones.clone(),
                    bone_ids: (0..=LINKS).map(|index| BoneId(index as u32)).collect(),
                    asset: None,
                },
            )
            .expect("skeleton");
        app.world_mut()
            .add_component(rig_entity, GlobalTransform::identity())
            .expect("rig global transform");
        let skeleton_asset = test_skeleton_asset(&rest_translations, &parents);
        app.world_mut()
            .add_component(rig_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("rig pose");

        app.add_system(mmd_rigid_body_physics_system);
        app.add_system(publish_final_rig_pose_system);
        app.add_system(crate::transform::transform_propagation_system);
        for _ in 0..60 {
            app.update().expect("settling step");
        }

        // Wrap the clip: one step later the skeleton stands somewhere else
        // entirely, exactly as frame 0 does after the last frame plays. The
        // wrap declares itself, which is what lets the solver tell this apart
        // from motion that merely happens to be fast.
        {
            let rig_pose = app
                .world_mut()
                .get_component_mut::<RigPose>(rig_entity)
                .expect("rig pose must remain alive");
            rig_pose
                .animation_layer_mut()
                .write_translation(0, Vec3::new(JUMP, ANCHOR_HEIGHT, 0.0));
            rig_pose.mark_discontinuous();
        }
        app.update().expect("the step that absorbs the jump");
        // The declaration lasts one step, exactly as
        // `rig_pose_clear_transient_system` makes it in production; the rest
        // of this run is ordinary continuous motion.
        app.world_mut()
            .get_component_mut::<RigPose>(rig_entity)
            .expect("rig pose must remain alive")
            .clear_transient_layers();
        for _ in 0..119 {
            app.update().expect("post-jump step");
        }

        let world_of = |entity| {
            app.world()
                .get_component::<GlobalTransform>(entity)
                .expect("bone global transform")
                .matrix()
                .w_axis
                .truncate()
        };
        let anchor = world_of(bones[0]);
        assert!(
            anchor.distance(Vec3::new(JUMP, ANCHOR_HEIGHT, 0.0)) < 1.0e-4,
            "the anchor bone must follow the repositioned animation pose, got {anchor:?}"
        );
        let chain_length = SPACING * LINKS as f32;
        let separation = (world_of(bones[LINKS]) - anchor).length();
        assert!(
            separation < chain_length * 1.5,
            "the chain tip trails {separation} m behind its anchor, but its links total {chain_length} m"
        );
    }
}
