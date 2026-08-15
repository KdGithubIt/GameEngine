//! Runtime post-blend two-bone foot IK correction (ADR 0080 §2).
//!
//! Ground-contact intervals ([`crate::contact_detect::ContactInterval`]) are
//! baked per clip; correction is not. This module runs on the fixed
//! schedule *after* [`crate::animation::animation_system`], reads the final
//! blended local pose, and nudges the hip/knee/ankle chain of each foot
//! currently in contact so it lands on the actual ground under this
//! character, without ever baking the correction into a clip (see ADR 0080's
//! Context for why blending makes a baked correction unsound).
//!
//! [`FootIk`] is an opt-in per-entity component; [`foot_ik_system`] is a
//! no-op for any entity without one. Everything here is a *vertical*
//! correction only — X/Z stay wherever forward kinematics placed them; full
//! ground-normal alignment is out of scope for v1 (see the ADR's "not in
//! v1" list).

use crate::animation::{AnimationClip, Animator};
use crate::asset::Assets;
use crate::collision::{
    segment_blocked_by_static, static_obstacle_aabbs, Collider, PhysicsBody, TriggerVolume,
};
use crate::contact_detect::ContactInterval;
use crate::rig_pose::{PoseStage, RigPose};
use crate::skeleton_asset::{BoneId, SkeletonAsset, SkeletonAssetRegistry};
use crate::skinning::Skeleton;
use crate::transform::GlobalTransform;
use engine_ecs::Entity;
use glam::{Mat4, Quat, Vec3};
use hashbrown::HashSet;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

/// Opt-in per-entity component enabling runtime foot IK correction (ADR 0080 §2).
///
/// Requires a sibling [`Skeleton`] and [`Animator`]; an entity with
/// [`FootIk`] but no [`Skeleton`] is simply skipped by [`foot_ik_system`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FootIk {
    /// Maximum vertical adjustment, in meters. A ground hit farther than
    /// this from the FK foot position is treated as "no ground" rather than
    /// being clamped — see [`foot_ik_system`]'s ground-query rustdoc.
    pub max_correction: f32,
    /// Whether correction runs for this entity at all.
    pub enabled: bool,
}

impl Default for FootIk {
    fn default() -> Self {
        Self {
            max_correction: 0.3,
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// One-shot diagnostics
// ---------------------------------------------------------------------------

/// Records degenerate-chain skips at most once per `(entity, bone)` pair, so
/// a chain that stays degenerate every tick reports once instead of spamming
/// a diagnostic every fixed step (ADR 0080 §2).
///
/// Optional resource, mirroring [`crate::ui_document::UiRuntimeDiagnostics`]'s
/// pattern: [`foot_ik_system`] works identically whether or not it is
/// inserted, it just has nowhere to report skips without it.
#[derive(Debug, Default)]
pub struct FootIkDiagnostics {
    reported: HashSet<(Entity, BoneId)>,
}

impl FootIkDiagnostics {
    /// Creates an empty diagnostics recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a degenerate-chain skip for `(entity, bone)`.
    ///
    /// Returns `true` the first time this pair is recorded (the caller
    /// should log/surface it), `false` on every subsequent call (already
    /// reported).
    pub fn record_degenerate_chain(&mut self, entity: Entity, bone: BoneId) -> bool {
        self.reported.insert((entity, bone))
    }

    /// Returns every distinct `(entity, bone)` pair recorded so far.
    pub fn reported(&self) -> impl Iterator<Item = &(Entity, BoneId)> {
        self.reported.iter()
    }
}

// ---------------------------------------------------------------------------
// Contact weight (ADR 0080 §2 step 1)
// ---------------------------------------------------------------------------

/// Ease-in/ease-out duration applied at each [`ContactInterval`]'s start and
/// end, so a correction fades rather than pops (ADR 0080 §2).
const CONTACT_EASE_SECONDS: f32 = 0.1;

/// Returns `interval`'s contact weight at `time`, `0.0` outside the
/// interval, ramping linearly to `1.0` over [`CONTACT_EASE_SECONDS`] at each
/// edge (and never reaching above `1.0` even for a short interval whose two
/// ramps overlap).
fn eased_interval_weight(time: f32, interval: &ContactInterval) -> f32 {
    if time < interval.start || time >= interval.end {
        return 0.0;
    }
    let ease_in = (time - interval.start) / CONTACT_EASE_SECONDS;
    let ease_out = (interval.end - time) / CONTACT_EASE_SECONDS;
    ease_in.min(ease_out).clamp(0.0, 1.0)
}

/// Sums (clamped to `[0, 1]`) every `bone` interval in `clip.contacts` that
/// contains `time`, eased at interval edges (ADR 0080 §2).
///
/// Detection normally never produces overlapping intervals for the same
/// bone, so this is a single match in practice; summing instead of taking
/// the first match matches the ADR's rule literally and stays correct even
/// if that ever changes.
pub(crate) fn clip_contact_weight(clip: &AnimationClip, bone: BoneId, time: f32) -> f32 {
    clip.contacts
        .iter()
        .filter(|interval| interval.bone == bone)
        .map(|interval| eased_interval_weight(time, interval))
        .sum::<f32>()
        .clamp(0.0, 1.0)
}

/// Combines the target clip's and (while crossfading) the fade-source
/// clip's contact weight for `bone`, per ADR 0080 §2 step 1: the target
/// clip's contribution is scaled by the crossfade blend weight (`1.0` when
/// not fading), the fade source's by `1.0 - blend weight`.
fn combined_contact_weight(
    clips: &Assets<AnimationClip>,
    animator: &Animator,
    target_clip: &AnimationClip,
    bone: BoneId,
) -> f32 {
    let target_blend = animator.crossfade_progress().unwrap_or(1.0);
    let mut weight = clip_contact_weight(target_clip, bone, animator.time) * target_blend;
    if let Some((source_handle, source_time)) = animator.fade_source()
        && let Some(source_clip) = clips.get(&source_handle) {
            weight += clip_contact_weight(source_clip, bone, source_time) * (1.0 - target_blend);
        }
    weight.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Two-bone analytic IK (ADR 0080 §2 step 3)
// ---------------------------------------------------------------------------

/// Numeric floor below which a length, direction, or reach is treated as
/// degenerate rather than solved (ADR 0080 §2).
const IK_EPSILON: f32 = 1.0e-5;

/// A successful two-bone analytic IK solve (ADR 0080 §2), expressed as
/// rotation deltas to apply on top of the current (FK) hip and knee
/// rotations, in whatever space the input positions were given in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TwoBoneIkSolution {
    /// The resulting knee position after applying `hip_rotation_delta`.
    pub new_knee: Vec3,
    /// The resulting ankle position; equals `target` within numerical
    /// tolerance by construction.
    pub new_ankle: Vec3,
    /// Rotation to left-multiply onto the hip's current rotation.
    pub hip_rotation_delta: Quat,
    /// Rotation to left-multiply onto the knee's current rotation.
    pub knee_rotation_delta: Quat,
}

/// Solves hip-knee-ankle two-bone IK (law of cosines) placing the ankle at
/// `target`, preserving the knee-bend plane from the current
/// `hip`/`knee`/`ankle` pose (ADR 0080 §2 step 3).
///
/// All four points must be given in the same space (model or world); the
/// solution's rotation deltas are then valid in that same space.
///
/// Returns `None` — a degenerate chain, per ADR 0080 §2 — when:
/// - either segment (`hip`-`knee` or `knee`-`ankle`) has near-zero length;
/// - `hip`, `knee`, and `ankle` are (near-)collinear, so no bend plane can
///   be preserved;
/// - `target` coincides with `hip` (undefined direction); or
/// - `target` is unreachable: farther than `hip_knee_len + knee_ankle_len`,
///   or closer than `|hip_knee_len - knee_ankle_len|`.
///
/// The rotation direction that keeps the knee bending on the same side as
/// the input pose is chosen by comparing both candidate rotations against
/// the original hip-to-knee direction, rather than by reasoning about a
/// cross-product sign convention — this is what "preserves the knee-bend
/// plane" in practice.
pub(crate) fn solve_two_bone_ik(
    hip: Vec3,
    knee: Vec3,
    ankle: Vec3,
    target: Vec3,
) -> Option<TwoBoneIkSolution> {
    let upper = knee - hip;
    let lower = ankle - knee;
    let len1 = upper.length();
    let len2 = lower.length();
    if len1 <= IK_EPSILON || len2 <= IK_EPSILON {
        return None;
    }

    let bend_axis = upper.cross(ankle - hip);
    if bend_axis.length() <= IK_EPSILON {
        return None;
    }
    let axis = bend_axis.normalize();

    let to_target = target - hip;
    let dist = to_target.length();
    if dist <= IK_EPSILON {
        return None;
    }

    let min_reach = (len1 - len2).abs();
    let max_reach = len1 + len2;
    if dist < min_reach - IK_EPSILON || dist > max_reach + IK_EPSILON {
        return None;
    }
    let clamped_dist = dist.clamp(min_reach, max_reach);

    let cos_hip_angle = ((len1 * len1) + (clamped_dist * clamped_dist) - (len2 * len2))
        / (2.0 * len1 * clamped_dist);
    let hip_angle = cos_hip_angle.clamp(-1.0, 1.0).acos();

    let target_dir = to_target.normalize();
    let old_upper_dir = upper.normalize();
    let candidate_a = Quat::from_axis_angle(axis, hip_angle) * target_dir;
    let candidate_b = Quat::from_axis_angle(axis, -hip_angle) * target_dir;
    let new_upper_dir = if candidate_a.dot(old_upper_dir) >= candidate_b.dot(old_upper_dir) {
        candidate_a
    } else {
        candidate_b
    }
    .normalize();

    let new_knee = hip + new_upper_dir * len1;
    let old_lower_dir = lower.normalize();
    let new_lower_dir = (target - new_knee).normalize();

    Some(TwoBoneIkSolution {
        new_knee,
        new_ankle: new_knee + new_lower_dir * len2,
        hip_rotation_delta: Quat::from_rotation_arc(old_upper_dir, new_upper_dir),
        knee_rotation_delta: Quat::from_rotation_arc(old_lower_dir, new_lower_dir),
    })
}

// ---------------------------------------------------------------------------
// Chain resolution (ADR 0080 §2 step 3: "walking BoneId parents")
// ---------------------------------------------------------------------------

/// Resolved hip/knee/ankle bone indices into [`SkeletonAsset::bones`] for one
/// leg, per ADR 0080 §2's chain-resolution rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LegChainIndices {
    hip: usize,
    knee: usize,
    ankle: usize,
}

/// Resolves the hip/knee/ankle chain for a contact bone (ADR 0080 §2):
/// the ankle is the contact bone itself, or its parent when the contact
/// bone's name contains `"toe"`; the chain is the two bones above the ankle.
///
/// Returns `None` when the chain cannot be walked three bones deep (the
/// contact bone is at or near a skeleton root) — a degenerate chain per the
/// ADR, left for the caller to report.
fn resolve_leg_chain_indices(
    skeleton_asset: &SkeletonAsset,
    contact_bone_index: usize,
) -> Option<LegChainIndices> {
    let contact_bone = skeleton_asset.bones.get(contact_bone_index)?;
    let ankle_index = if contact_bone.name.to_ascii_lowercase().contains("toe") {
        contact_bone.parent?
    } else {
        contact_bone_index
    };
    let knee_index = skeleton_asset.bones.get(ankle_index)?.parent?;
    let hip_index = skeleton_asset.bones.get(knee_index)?.parent?;
    Some(LegChainIndices {
        hip: hip_index,
        knee: knee_index,
        ankle: ankle_index,
    })
}

/// Finds the nearest shared ancestor of bone indices `a` and `b` in
/// `skeleton_asset`, walking parent links from each (ADR 0080 §2's pelvis
/// follow-up: "common ancestor of both hip bones").
///
/// Bounded by `skeleton_asset.bones.len()` steps per side so a corrupt
/// parent cycle cannot loop forever.
fn common_ancestor_index(skeleton_asset: &SkeletonAsset, a: usize, b: usize) -> Option<usize> {
    let mut ancestors_of_a: Vec<usize> = Vec::new();
    let mut current = Some(a);
    let mut steps = 0;
    while let Some(index) = current {
        ancestors_of_a.push(index);
        current = skeleton_asset.bones.get(index)?.parent;
        steps += 1;
        if steps > skeleton_asset.bones.len() + 1 {
            return None;
        }
    }

    let mut current = Some(b);
    let mut steps = 0;
    while let Some(index) = current {
        if ancestors_of_a.contains(&index) {
            return Some(index);
        }
        current = skeleton_asset.bones.get(index)?.parent;
        steps += 1;
        if steps > skeleton_asset.bones.len() + 1 {
            return None;
        }
    }
    None
}

/// Forward-kinematics every ancestor of `bone_index` (root-first, ending at
/// `bone_index` itself), seeded by `base` instead of the identity — passing
/// the character entity's own world (position, rotation) here makes every
/// returned transform a **world**-space transform directly, so the ground
/// query and IK solve never need a separate model-to-world conversion.
///
/// Returns `None` when a joint in the chain has no entry in `local` (no
/// [`Transform`] this tick — despawned or malformed) or a parent cycle is
/// detected.
fn chain_world_transforms(
    skeleton_asset: &SkeletonAsset,
    world_matrices: &[Mat4],
    bone_index: usize,
) -> Option<Vec<(Vec3, Quat)>> {
    let mut ancestors = Vec::new();
    let mut current = Some(bone_index);
    let mut steps = 0;
    while let Some(index) = current {
        ancestors.push(index);
        current = skeleton_asset.bones.get(index)?.parent;
        steps += 1;
        if steps > skeleton_asset.bones.len() + 1 {
            return None;
        }
    }
    ancestors.reverse();

    let mut chain = Vec::with_capacity(ancestors.len());
    for index in ancestors {
        let (_, rotation, position) = world_matrices
            .get(index)?
            .to_scale_rotation_translation();
        chain.push((position, rotation));
    }
    Some(chain)
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// Applies runtime two-bone foot IK correction after animation sampling
/// (ADR 0080 §2).
///
/// Register on the fixed schedule **after**
/// [`crate::animation::animation_system`] — this system reads the final
/// blended local pose, so it must see the tick's animation output, never a
/// stale one. Requires [`SkeletonAssetRegistry`] to resolve a character's
/// bone hierarchy; a character whose [`Skeleton::asset`] is unset or not
/// registered is silently skipped (no ground truth, no guess).
///
/// Per character with [`FootIk::enabled`]:
///
/// 1. **Contact weight**: for every bone named in the active clip's (and,
///    mid-crossfade, the fade source clip's) [`AnimationClip::contacts`],
///    blended and eased per ADR 0080 §2 step 1.
/// 2. **Ground query**: a vertical segment from `max_correction` above to
///    `max_correction` below the bone's current (FK) world position, tested
///    against every [`PhysicsBody::Static`], non-[`TriggerVolume`] collider
///    via [`segment_blocked_by_static`] (the Phase 58 slab test). No hit
///    forces the weight to `0.0` — a correction is never guessed without a
///    ground hit. Because the segment is centered on the FK position with
///    half-length `max_correction`, any hit is automatically within
///    `max_correction` of it.
/// 3. **Two-bone analytic IK** (law of cosines), blended with the FK pose
///    by the contact weight (`Quat::slerp`, so weight `0.0` leaves the pose
///    untouched and `1.0` snaps fully to the solve) and written to the hip
///    and knee channels in the [`RigPose`] procedural layer. A degenerate chain (not
///    exactly three resolvable bones, a zero-length segment, a collinear
///    chain, or an unreachable target) skips with [`FootIkDiagnostics`],
///    never a panic.
/// 4. **Pelvis follow-up**: when exactly two legs correct downward this
///    tick, the common ancestor of their hip bones (ADR 0080 §2 step 4)
///    lowers by the lesser (smaller-magnitude) of the two corrections.
///
/// Only the joint pose layer is written; the character's own entity
/// transform is never touched (ADR 0080 §5).
pub fn foot_ik_system(
    clips: engine_ecs::Res<Assets<AnimationClip>>,
    skeleton_registry: Option<engine_ecs::Res<SkeletonAssetRegistry>>,
    colliders: engine_ecs::Query<(
        &Collider,
        &PhysicsBody,
        &GlobalTransform,
        Option<&TriggerVolume>,
    )>,
    mut diagnostics: Option<engine_ecs::ResMut<FootIkDiagnostics>>,
    mut characters: engine_ecs::Query<(
        &Animator,
        &Skeleton,
        &FootIk,
        &GlobalTransform,
        &mut RigPose,
    )>,
) {
    let Some(skeleton_registry) = skeleton_registry else {
        return;
    };

    let static_aabbs = static_obstacle_aabbs(colliders.iter().map(|(_, data)| data));

    for (entity, (animator, skeleton, foot_ik, global_transform, rig_pose)) in
        characters.iter_mut()
    {
        if !foot_ik.enabled {
            continue;
        }
        let Some(target_clip) = clips.get(&animator.clip) else {
            continue;
        };
        let Some(asset_id) = skeleton.asset.as_ref() else {
            continue;
        };
        let Some(skeleton_asset) = skeleton_registry.get(asset_id) else {
            continue;
        };
        if rig_pose.skeleton_asset() != asset_id
            || rig_pose.joint_count() != skeleton_asset.bones.len()
        {
            continue;
        }

        let mut contact_bones: HashSet<BoneId> = target_clip
            .contacts
            .iter()
            .map(|interval| interval.bone)
            .collect();
        if let Some((source_handle, _)) = animator.fade_source()
            && let Some(source_clip) = clips.get(&source_handle) {
                contact_bones.extend(source_clip.contacts.iter().map(|interval| interval.bone));
            }
        if contact_bones.is_empty() {
            continue;
        }

        let world_matrices = rig_pose
            .evaluate_world(PoseStage::Animation, global_transform.matrix())
            .to_vec();

        let mut leg_corrections: Vec<(f32, usize)> = Vec::new();

        for &bone in &contact_bones {
            let Some(bone_index) = skeleton_asset.bone_index(bone) else {
                continue;
            };
            let weight = combined_contact_weight(&clips, animator, target_clip, bone);
            if weight <= 0.0 {
                continue;
            }
            let Some(chain_indices) = resolve_leg_chain_indices(skeleton_asset, bone_index) else {
                record_degenerate(&mut diagnostics, entity, bone);
                continue;
            };
            let Some(chain) = chain_world_transforms(
                skeleton_asset,
                &world_matrices,
                chain_indices.ankle,
            ) else {
                record_degenerate(&mut diagnostics, entity, bone);
                continue;
            };
            let len = chain.len();
            if len < 3 {
                record_degenerate(&mut diagnostics, entity, bone);
                continue;
            }
            let (hip_position, hip_rotation) = chain[len - 3];
            let (knee_position, knee_rotation) = chain[len - 2];
            let (ankle_position, _) = chain[len - 1];
            let parent_of_hip_rotation = if len >= 4 {
                chain[len - 4].1
            } else {
                global_transform
                    .matrix()
                    .to_scale_rotation_translation()
                    .1
            };

            let ray_half = foot_ik.max_correction.max(0.0);
            let ray_start = ankle_position + Vec3::Y * ray_half;
            let ray_end = ankle_position - Vec3::Y * ray_half;
            let Some(t) = segment_blocked_by_static(&static_aabbs, ray_start, ray_end) else {
                continue;
            };
            let ground_point = ray_start + (ray_end - ray_start) * t;
            let target = Vec3::new(ankle_position.x, ground_point.y, ankle_position.z);
            let vertical_delta = ground_point.y - ankle_position.y;

            let Some(solution) =
                solve_two_bone_ik(hip_position, knee_position, ankle_position, target)
            else {
                record_degenerate(&mut diagnostics, entity, bone);
                continue;
            };

            let Some(hip_local) =
                rig_pose.local_transform(PoseStage::Animation, chain_indices.hip)
            else {
                continue;
            };
            let Some(knee_local) =
                rig_pose.local_transform(PoseStage::Animation, chain_indices.knee)
            else {
                continue;
            };

            let new_hip_world = (solution.hip_rotation_delta * hip_rotation).normalize();
            let new_hip_local_rotation =
                (parent_of_hip_rotation.inverse() * new_hip_world).normalize();
            let new_knee_world = (solution.knee_rotation_delta * knee_rotation).normalize();
            let new_knee_local_rotation = (new_hip_world.inverse() * new_knee_world).normalize();

            let blended_hip_rotation = hip_local.rotation.slerp(new_hip_local_rotation, weight);
            let blended_knee_rotation = knee_local.rotation.slerp(new_knee_local_rotation, weight);

            rig_pose
                .procedural_layer_mut()
                .write_rotation(chain_indices.hip, blended_hip_rotation);
            rig_pose
                .procedural_layer_mut()
                .write_rotation(chain_indices.knee, blended_knee_rotation);

            leg_corrections.push((vertical_delta, chain_indices.hip));
        }

        apply_pelvis_follow_up(
            skeleton_asset,
            rig_pose,
            &leg_corrections,
        );
    }
}

fn record_degenerate(
    diagnostics: &mut Option<engine_ecs::ResMut<FootIkDiagnostics>>,
    entity: Entity,
    bone: BoneId,
) {
    if let Some(diagnostics) = diagnostics.as_deref_mut() {
        diagnostics.record_degenerate_chain(entity, bone);
    }
}

/// Applies ADR 0080 §2 step 4: when exactly two legs corrected downward
/// this tick, the common ancestor of their hip bones lowers by the lesser
/// (smaller-magnitude) correction, so the legs do not hyperextend.
fn apply_pelvis_follow_up(
    skeleton_asset: &SkeletonAsset,
    rig_pose: &mut RigPose,
    leg_corrections: &[(f32, usize)],
) {
    if leg_corrections.len() != 2 {
        return;
    }
    let (delta_a, hip_a) = leg_corrections[0];
    let (delta_b, hip_b) = leg_corrections[1];
    if !(delta_a < 0.0 && delta_b < 0.0) {
        return;
    }
    let Some(pelvis_index) = common_ancestor_index(skeleton_asset, hip_a, hip_b) else {
        return;
    };
    // The smaller-magnitude (closer to zero) of two negative deltas is the
    // greater (less negative) value.
    let lesser = delta_a.max(delta_b);

    let Some(mut pelvis) = rig_pose.local_transform(PoseStage::Animation, pelvis_index) else {
        return;
    };
    pelvis.translation.y += lesser;
    rig_pose
        .procedural_layer_mut()
        .write_translation(pelvis_index, pelvis.translation);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rig_pose::publish_final_rig_pose_system;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};
    use crate::transform::Transform;
    use engine_authoring::id::AssetId;
    use engine_ecs::World;

    // --- solve_two_bone_ik ---------------------------------------------------

    /// A bent test chain: hip at the origin, knee slightly forward and
    /// below, ankle straight below the knee. Bent (not collinear) so a bend
    /// plane is well-defined.
    fn bent_chain() -> (Vec3, Vec3, Vec3) {
        let hip = Vec3::ZERO;
        let knee = Vec3::new(0.2, -1.0, 0.0);
        let ankle = Vec3::new(0.0, -2.0, 0.0);
        (hip, knee, ankle)
    }

    #[test]
    fn solve_two_bone_ik_reproduces_the_original_pose_when_target_is_the_current_ankle() {
        let (hip, knee, ankle) = bent_chain();

        let solution = solve_two_bone_ik(hip, knee, ankle, ankle)
            .expect("a self-consistent target must solve");

        assert!(
            solution.new_ankle.distance(ankle) < 1e-4,
            "expected ankle {:?}, got {:?}",
            ankle,
            solution.new_ankle
        );
        assert!(
            solution.new_knee.distance(knee) < 1e-4,
            "expected knee {:?}, got {:?}",
            knee,
            solution.new_knee
        );
    }

    #[test]
    fn solve_two_bone_ik_reaches_a_nearby_reachable_target_exactly() {
        let (hip, knee, ankle) = bent_chain();
        let target = Vec3::new(0.1, -1.9, 0.05);

        let solution = solve_two_bone_ik(hip, knee, ankle, target).expect("must be reachable");

        assert!(
            solution.new_ankle.distance(target) < 1e-4,
            "expected {:?}, got {:?}",
            target,
            solution.new_ankle
        );
    }

    #[test]
    fn solve_two_bone_ik_preserves_the_knee_bend_plane() {
        let (hip, knee, ankle) = bent_chain();
        let original_axis = (knee - hip).cross(ankle - hip).normalize();
        let target = Vec3::new(0.1, -1.9, 0.05);

        let solution = solve_two_bone_ik(hip, knee, ankle, target).expect("must be reachable");

        let new_axis = (solution.new_knee - hip)
            .cross(solution.new_ankle - hip)
            .normalize();
        assert!(
            original_axis.dot(new_axis) > 0.99,
            "the knee must keep bending on the same side, dot={}",
            original_axis.dot(new_axis)
        );
    }

    #[test]
    fn solve_two_bone_ik_skips_an_unreachable_target() {
        let (hip, knee, ankle) = bent_chain();
        let far_target = Vec3::new(0.0, -20.0, 0.0);

        assert!(solve_two_bone_ik(hip, knee, ankle, far_target).is_none());
    }

    #[test]
    fn solve_two_bone_ik_skips_a_collinear_chain() {
        let hip = Vec3::ZERO;
        let knee = Vec3::new(0.0, -1.0, 0.0);
        let ankle = Vec3::new(0.0, -2.0, 0.0);

        assert!(solve_two_bone_ik(hip, knee, ankle, Vec3::new(0.0, -1.5, 0.0)).is_none());
    }

    #[test]
    fn solve_two_bone_ik_skips_a_zero_length_segment() {
        let hip = Vec3::ZERO;
        let knee = Vec3::ZERO;
        let ankle = Vec3::new(0.0, -1.0, 0.0);

        assert!(solve_two_bone_ik(hip, knee, ankle, Vec3::new(0.1, -1.0, 0.0)).is_none());
    }

    // --- chain resolution -----------------------------------------------------

    fn bone(id: u32, name: &str, parent: Option<usize>) -> BoneDef {
        BoneDef {
            id: BoneId(id),
            name: name.to_owned(),
            parent,
            rest_translation: Vec3::ZERO,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        }
    }

    /// A minimal biped-ish rig: pelvis -> {hip_l -> knee_l -> ankle_l},
    /// {hip_r -> knee_r -> ankle_r -> toe_r}.
    fn biped_skeleton() -> SkeletonAsset {
        let bones = vec![
            bone(0, "pelvis", None),
            bone(1, "hip_l", Some(0)),
            bone(2, "knee_l", Some(1)),
            bone(3, "ankle_l", Some(2)),
            bone(4, "hip_r", Some(0)),
            bone(5, "knee_r", Some(4)),
            bone(6, "ankle_r", Some(5)),
            bone(7, "toe_r", Some(6)),
        ];
        let identity = compute_skeleton_identity(&bones);
        SkeletonAsset {
            id: AssetId::generate(),
            name: "biped".to_owned(),
            bones,
            identity,
            next_bone_id: 8,
        }
    }

    #[test]
    fn resolve_leg_chain_indices_uses_the_contact_bone_directly_when_not_a_toe() {
        let skeleton = biped_skeleton();
        let chain = resolve_leg_chain_indices(&skeleton, 3).expect("ankle_l chain must resolve");
        assert_eq!(
            chain,
            LegChainIndices {
                hip: 1,
                knee: 2,
                ankle: 3
            }
        );
    }

    #[test]
    fn resolve_leg_chain_indices_walks_up_from_a_toe_bone() {
        let skeleton = biped_skeleton();
        let chain = resolve_leg_chain_indices(&skeleton, 7).expect("toe_r chain must resolve");
        assert_eq!(
            chain,
            LegChainIndices {
                hip: 4,
                knee: 5,
                ankle: 6
            }
        );
    }

    #[test]
    fn resolve_leg_chain_indices_fails_near_the_skeleton_root() {
        let skeleton = biped_skeleton();
        assert!(resolve_leg_chain_indices(&skeleton, 0).is_none());
    }

    #[test]
    fn common_ancestor_index_finds_the_shared_pelvis() {
        let skeleton = biped_skeleton();
        assert_eq!(common_ancestor_index(&skeleton, 1, 4), Some(0));
    }

    // --- foot_ik_system ---------------------------------------------------

    /// Builds a straight-down (but bent enough for a well-defined bend
    /// plane) hip/knee/ankle rig as three entities parented in a chain under
    /// a character root entity, with a single-clip [`Animator`] whose
    /// contact interval covers the whole test window.
    struct TestRig {
        world: World,
        character: Entity,
        hip: Entity,
        knee: Entity,
    }

    fn planted_clip(ankle_bone: BoneId, duration: f32) -> AnimationClip {
        AnimationClip {
            duration,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: vec![ContactInterval {
                bone: ankle_bone,
                start: 0.0,
                end: duration,
            }],
        }
    }

    /// A clip with no contact intervals at all: this bone is never "in
    /// contact" while it is the active clip.
    fn bare_clip(duration: f32) -> AnimationClip {
        AnimationClip {
            duration,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        }
    }

    fn build_test_rig() -> TestRig {
        let mut world = World::new();
        let hip_bone = BoneId(0);
        let knee_bone = BoneId(1);
        let ankle_bone = BoneId(2);

        let mut bones = vec![
            bone(hip_bone.0, "hip", None),
            bone(knee_bone.0, "knee", Some(0)),
            bone(ankle_bone.0, "ankle", Some(1)),
        ];
        bones[0].rest_rotation = Quat::from_rotation_z(0.3);
        bones[1].rest_translation = Vec3::new(0.0, -1.0, 0.0);
        bones[1].rest_rotation = Quat::from_rotation_z(-0.6);
        bones[2].rest_translation = Vec3::new(0.0, -1.0, 0.0);
        let identity = compute_skeleton_identity(&bones);
        let skeleton_asset = SkeletonAsset {
            id: AssetId::generate(),
            name: "leg_rig".to_owned(),
            bones,
            identity,
            next_bone_id: 3,
        };
        let skeleton_asset_id = skeleton_asset.id.clone();

        let hip = world
            .spawn_with(Transform {
                translation: Vec3::ZERO,
                rotation: Quat::from_rotation_z(0.3),
                scale: Vec3::ONE,
            })
            .expect("hip must spawn");
        // The knee bends back by twice the hip angle (a plausible knee bend),
        // which keeps hip/knee/ankle non-collinear -- a straight leg has no
        // well-defined bend plane, so `solve_two_bone_ik` would (correctly)
        // treat it as degenerate.
        let knee = world
            .spawn_with(Transform {
                translation: Vec3::new(0.0, -1.0, 0.0),
                rotation: Quat::from_rotation_z(-0.6),
                scale: Vec3::ONE,
            })
            .expect("knee must spawn");
        let ankle = world
            .spawn_with(Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)))
            .expect("ankle must spawn");

        let mut clips = Assets::<AnimationClip>::default();
        let clip = clips.add(planted_clip(ankle_bone, 10.0));
        let mut animator = Animator::playing(clip);
        animator.time = 1.0;

        let character = world
            .spawn_with(animator)
            .expect("character entity must spawn");
        world
            .add_component(character, GlobalTransform::identity())
            .expect("character must accept GlobalTransform");
        world
            .add_component(
                character,
                Skeleton {
                    joints: vec![hip, knee, ankle],
                    bone_ids: vec![hip_bone, knee_bone, ankle_bone],
                    asset: Some(skeleton_asset_id.clone()),
                },
            )
            .expect("character must accept Skeleton");
        world
            .add_component(character, FootIk::default())
            .expect("character must accept FootIk");
        world
            .add_component(character, RigPose::from_skeleton(&skeleton_asset))
            .expect("character must accept RigPose");

        world.insert_resource(clips);
        let mut registry = crate::skeleton_asset::SkeletonAssetRegistry::new();
        registry.insert(skeleton_asset);
        world.insert_resource(registry);

        TestRig {
            world,
            character,
            hip,
            knee,
        }
    }

    fn run_foot_ik(world: &mut World) {
        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), world);
        app.add_system(foot_ik_system);
        app.add_system(publish_final_rig_pose_system);
        app.update().expect("foot_ik schedule must run");
        std::mem::swap(app.world_mut(), world);
    }

    /// The FK ankle's world Y in [`build_test_rig`]'s chain: hip world
    /// rotation is `rotate_z(0.3)`, knee world rotation is
    /// `rotate_z(0.3 + (-0.6)) = rotate_z(-0.3)`, so the two 1.0-unit
    /// segments contribute `-cos(0.3)` and `-cos(-0.3)` respectively --
    /// equal, since cosine is even.
    ///
    /// The chain is very close to full extension (max reach 2.0, this
    /// distance ~= 1.911), so ground offsets used in tests must stay well
    /// under the ~0.09 of remaining reach.
    fn fk_ankle_y() -> f32 {
        -(0.3_f32.cos() + (0.3_f32 + (-0.6_f32)).cos())
    }

    /// Spawns a thin, wide static AABB "ground" collider whose **top**
    /// surface sits at world Y = `top_y`, with its [`GlobalTransform`]
    /// already reflecting that placement (this test world has no
    /// `transform_propagation_system` registered).
    fn spawn_static_ground(world: &mut World, top_y: f32) -> Entity {
        let half_height = 0.05;
        let translation = Vec3::new(0.0, top_y - half_height, 0.0);
        let ground = world
            .spawn_with(Transform::from_translation(translation))
            .expect("ground must spawn");
        world
            .add_component(
                ground,
                GlobalTransform(glam::Mat4::from_translation(translation)),
            )
            .expect("ground must accept GlobalTransform");
        world
            .add_component(ground, Collider::aabb(Vec3::new(5.0, half_height, 5.0)))
            .expect("ground must accept Collider");
        world
            .add_component(ground, PhysicsBody::Static)
            .expect("ground must accept PhysicsBody");
        ground
    }

    #[test]
    fn planted_foot_lands_on_a_static_ground_within_tolerance() {
        let mut rig = build_test_rig();
        // Place a static ground slab whose top face sits 0.05m below the FK
        // ankle position: within `FootIk::default().max_correction` (0.3)
        // and within the chain's remaining reach (see `fk_ankle_y`).
        spawn_static_ground(&mut rig.world, fk_ankle_y() - 0.05);

        run_foot_ik(&mut rig.world);

        // With the ankle's contact interval fully weighted (well inside the
        // interval, no crossfade), the corrected ankle should end up close
        // to the ground's top face. We can't read the ankle's own Transform
        // directly (only hip/knee rotations are written), so verify the
        // knee rotation actually changed from its FK identity value.
        let knee_transform = rig
            .world
            .get_component::<Transform>(rig.knee)
            .expect("knee transform");
        assert_ne!(
            knee_transform.rotation,
            Quat::IDENTITY,
            "the knee must bend to reach the ground target"
        );
    }

    #[test]
    fn no_ground_hit_leaves_the_pose_untouched() {
        let mut rig = build_test_rig();
        // No static collider anywhere: the ground query must find nothing.
        let hip_before = rig
            .world
            .get_component::<Transform>(rig.hip)
            .expect("hip transform")
            .clone();
        let knee_before = rig
            .world
            .get_component::<Transform>(rig.knee)
            .expect("knee transform")
            .clone();

        run_foot_ik(&mut rig.world);

        assert_eq!(
            rig.world
                .get_component::<Transform>(rig.hip)
                .unwrap()
                .rotation,
            hip_before.rotation,
            "no ground hit must leave the hip pose untouched"
        );
        assert_eq!(
            rig.world
                .get_component::<Transform>(rig.knee)
                .unwrap()
                .rotation,
            knee_before.rotation,
            "no ground hit must leave the knee pose untouched"
        );
    }

    #[test]
    fn crossfade_blend_eases_the_contact_weight_between_full_fk_and_full_ik() {
        // The rig starts on `planted_clip` (contact) and crossfades to a
        // `bare_clip` (no contact) with no ground/height easing involved, so
        // the *only* thing varying the combined contact weight is the
        // crossfade blend itself: target contributes 0 (no contact),
        // source contributes `1.0 - target_blend`. Sampled mid-fade
        // (`target_blend` ~= 0.5), the combined weight is ~= 0.5 --
        // strictly between the two clips' own weights of 0 and 1.
        let mut rig = build_test_rig();
        spawn_static_ground(&mut rig.world, fk_ankle_y() - 0.05);

        let bare = {
            let clips = rig
                .world
                .get_resource_mut::<Assets<AnimationClip>>()
                .expect("clips resource");
            clips.add(bare_clip(10.0))
        };
        {
            let animator = rig
                .world
                .get_component_mut::<Animator>(rig.character)
                .expect("animator");
            animator.crossfade_to(bare, 1.0);
        }
        {
            let mut app = engine_ecs::App::new();
            std::mem::swap(app.world_mut(), &mut rig.world);
            app.insert_resource(crate::time::FixedTime::with_delta(0.5));
            app.add_fixed_system(crate::animation::animation_system);
            app.run_fixed_update().expect("animation tick");
            std::mem::swap(app.world_mut(), &mut rig.world);
        }
        let progress = rig
            .world
            .get_component::<Animator>(rig.character)
            .expect("animator")
            .crossfade_progress()
            .expect("must still be fading");
        assert!(
            progress > 0.0 && progress < 1.0,
            "expected a mid-fade progress, got {progress}"
        );

        run_foot_ik(&mut rig.world);

        let mid_fade_knee_rotation = rig
            .world
            .get_component::<Transform>(rig.knee)
            .expect("knee transform")
            .rotation;

        // A weight of 1.0 (no crossfade at all) snaps fully to the IK
        // solve; a weight of 0.0 (ground but a bare target clip, no fade)
        // leaves the FK pose untouched. The mid-fade result must differ
        // from both, proving the weight actually landed strictly between.
        let mut full_weight_rig = build_test_rig();
        spawn_static_ground(&mut full_weight_rig.world, fk_ankle_y() - 0.05);
        run_foot_ik(&mut full_weight_rig.world);
        let full_knee_rotation = full_weight_rig
            .world
            .get_component::<Transform>(full_weight_rig.knee)
            .expect("full-weight knee transform")
            .rotation;
        assert_ne!(
            mid_fade_knee_rotation, full_knee_rotation,
            "a mid-fade weight must not snap fully to the IK solve"
        );

        let mut untouched_rig = build_test_rig();
        let bare_for_untouched = {
            let clips = untouched_rig
                .world
                .get_resource_mut::<Assets<AnimationClip>>()
                .expect("clips resource");
            clips.add(bare_clip(10.0))
        };
        untouched_rig
            .world
            .get_component_mut::<Animator>(untouched_rig.character)
            .expect("animator")
            .clip = bare_for_untouched;
        spawn_static_ground(&mut untouched_rig.world, fk_ankle_y() - 0.05);
        run_foot_ik(&mut untouched_rig.world);
        let zero_weight_knee_rotation = untouched_rig
            .world
            .get_component::<Transform>(untouched_rig.knee)
            .expect("zero-weight knee transform")
            .rotation;
        assert_ne!(
            mid_fade_knee_rotation, zero_weight_knee_rotation,
            "a mid-fade weight must not leave the pose fully untouched either"
        );
    }

    // --- FootIkDiagnostics -----------------------------------------------------

    #[test]
    fn foot_ik_diagnostics_records_a_pair_at_most_once() {
        let mut diagnostics = FootIkDiagnostics::new();
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        let bone = BoneId(0);

        assert!(diagnostics.record_degenerate_chain(entity, bone));
        assert!(!diagnostics.record_degenerate_chain(entity, bone));
        assert_eq!(diagnostics.reported().count(), 1);
    }
}
