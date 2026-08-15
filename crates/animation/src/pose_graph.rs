//! Pure skeletal clip sampling and pose blending.
//!
//! Evaluation uses stable skeleton bone indices and never reads or writes
//! ECS joint entities. Ordered world-aware modifiers consume the resulting
//! animation layer after this data-only stage.

use glam::{Quat, Vec3};
use hashbrown::{HashMap, HashSet};

use crate::animation::{lerp_channel, lerp_morph_channel, AnimProperty, AnimationClip};
use crate::rig_pose::{PoseBlend, PoseChannels, PoseLayer};
use crate::skeleton_asset::BoneId;

/// Sparse transform channels targeting the animator entity itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityPose {
    /// Sampled local translation, when owned by the graph.
    pub translation: Option<Vec3>,
    /// Sampled local rotation, when owned by the graph.
    pub rotation: Option<Quat>,
    /// Sampled local scale, when owned by the graph.
    pub scale: Option<Vec3>,
}

/// Data-only output of clip sampling or blending.
#[derive(Debug, Clone, PartialEq)]
pub struct PoseGraphOutput {
    /// Sparse skeletal channels in stable bone order.
    pub joints: PoseLayer,
    /// Sparse non-skeletal channels for the animator entity.
    pub entity: EntityPose,
    /// Logical morph weights keyed by imported name.
    pub morph_weights: HashMap<String, f32>,
}

/// Per-bone weight multipliers applied on top of a blend weight.
///
/// This is what turns a whole-pose crossfade into a layered blend per bone:
/// an upper-body clip can reach full weight from the spine outward while the
/// legs keep the locomotion pose. Weights outside `0.0..=1.0`, and non-finite
/// weights, are clamped when read so a malformed mask cannot produce a
/// non-finite skeleton pose.
#[derive(Debug, Clone, PartialEq)]
pub struct BoneMask {
    /// Per-joint weights in stable skeleton bone order.
    weights: Vec<f32>,
    /// Weight reported for joints outside [`Self::weights`].
    default_weight: f32,
}

impl BoneMask {
    /// Creates a mask giving every joint the same weight.
    #[must_use]
    pub fn uniform(joint_count: usize, weight: f32) -> Self {
        Self {
            weights: vec![weight; joint_count],
            default_weight: weight,
        }
    }

    /// Creates a mask that fully weights the subtree rooted at `root` and
    /// gives `outside` to every other joint.
    ///
    /// `parents` is [`RigPose::parents`](crate::rig_pose::RigPose::parents),
    /// which is parent-before-child, so subtree membership resolves in one
    /// forward pass. A parent index that points forward or outside the
    /// skeleton ends the chain rather than indexing out of range, matching
    /// the non-panicking hierarchy policy used elsewhere in the pose runtime.
    #[must_use]
    pub fn from_subtree(
        parents: &[Option<usize>],
        root: usize,
        inside: f32,
        outside: f32,
    ) -> Self {
        let mut weights = vec![outside; parents.len()];
        for index in 0..parents.len() {
            let in_subtree = index == root
                || parents
                    .get(index)
                    .copied()
                    .flatten()
                    .filter(|parent| *parent < index)
                    .is_some_and(|parent| weights[parent] == inside);
            if in_subtree {
                weights[index] = inside;
            }
        }
        Self {
            weights,
            default_weight: outside,
        }
    }

    /// Sets one joint's weight, returning `false` for an index outside the
    /// mask.
    pub fn set_weight(&mut self, joint_index: usize, weight: f32) -> bool {
        let Some(slot) = self.weights.get_mut(joint_index) else {
            return false;
        };
        *slot = weight;
        true
    }

    /// Returns the clamped weight for `joint_index`.
    #[must_use]
    pub fn weight(&self, joint_index: usize) -> f32 {
        let weight = self
            .weights
            .get(joint_index)
            .copied()
            .unwrap_or(self.default_weight);
        if weight.is_finite() {
            weight.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Returns the number of joints this mask carries explicit weights for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    /// Returns whether the mask carries no explicit weights.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }
}

/// Reusable scratch buffers for pose graph evaluation.
///
/// A graph with several nodes evaluates into intermediate poses that are
/// discarded within the same fixed step. Recycling their storage keeps a
/// steady-state frame free of per-bone allocations, which matters at PMX
/// bone counts where every node owns two vectors sized by the skeleton.
#[derive(Debug, Default)]
pub struct PoseArena {
    free: Vec<PoseGraphOutput>,
}

impl PoseArena {
    /// Creates an empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hands out a buffer reset to `joint_count`.
    ///
    /// A pooled buffer keeps its allocation when the joint count is
    /// unchanged, which is the case for every rig on every frame.
    pub fn acquire(&mut self, joint_count: usize) -> PoseGraphOutput {
        let mut output = self.free.pop().unwrap_or_default();
        output.reset(joint_count);
        output
    }

    /// Returns a buffer to the pool for reuse.
    pub fn release(&mut self, output: PoseGraphOutput) {
        self.free.push(output);
    }

    /// Returns how many buffers are currently pooled.
    #[must_use]
    pub fn pooled(&self) -> usize {
        self.free.len()
    }
}

impl Default for PoseGraphOutput {
    fn default() -> Self {
        Self {
            joints: PoseLayer::new(0, PoseBlend::Replace),
            entity: EntityPose::default(),
            morph_weights: HashMap::new(),
        }
    }
}

impl PoseGraphOutput {
    /// Resizes this output to `joint_count` and clears every channel.
    ///
    /// Storage is reused, so a pooled buffer costs no allocation once it has
    /// been sized for a rig.
    pub fn reset(&mut self, joint_count: usize) {
        self.joints.reset(joint_count);
        self.entity = EntityPose::default();
        self.morph_weights.clear();
    }

    /// Samples one clip without resolving any ECS entity.
    ///
    /// `joint_count` sizes the produced layer and must match the destination
    /// [`RigPose`](crate::rig_pose::RigPose); `bone_index` resolves each
    /// channel's [`BoneId`] to a slot in that layer. The caller supplies a
    /// prepared map rather than a bone list because a clip drives hundreds
    /// of channels against hundreds of bones, and rebuilding or scanning the
    /// mapping per channel is quadratic in the bone count.
    ///
    /// Prefer [`Self::sample_into`] with a [`PoseArena`] buffer on the
    /// per-frame path; this constructor allocates.
    #[must_use]
    pub fn sample(
        clip: &AnimationClip,
        time: f32,
        joint_count: usize,
        bone_index: &HashMap<BoneId, usize>,
    ) -> Self {
        let mut output = Self::default();
        output.reset(joint_count);
        output.sample_into(clip, time, bone_index);
        output
    }

    /// Samples one clip into this already sized output.
    ///
    /// Every channel is cleared first, so the result depends only on `clip`
    /// and `time` and never on what this buffer previously held.
    pub fn sample_into(
        &mut self,
        clip: &AnimationClip,
        time: f32,
        bone_index: &HashMap<BoneId, usize>,
    ) {
        let joint_count = self.joints.len();
        self.reset(joint_count);
        let output = self;
        for channel in &clip.channels {
            let Some(value) = lerp_channel(channel, time) else {
                continue;
            };
            let joint_index = channel
                .target_bone
                .and_then(|bone| bone_index.get(&bone).copied());
            match (joint_index, channel.target_bone, channel.property) {
                (Some(index), _, AnimProperty::Translation) => {
                    output
                        .joints
                        .write_translation(index, Vec3::new(value[0], value[1], value[2]));
                }
                (Some(index), _, AnimProperty::Rotation) => {
                    output
                        .joints
                        .write_rotation(index, Quat::from_array(value));
                }
                (Some(index), _, AnimProperty::Scale) => {
                    output
                        .joints
                        .write_scale(index, Vec3::new(value[0], value[1], value[2]));
                }
                (None, None, AnimProperty::Translation) => {
                    output.entity.translation = Some(Vec3::new(value[0], value[1], value[2]));
                }
                (None, None, AnimProperty::Rotation) => {
                    output.entity.rotation = Some(Quat::from_array(value));
                }
                (None, None, AnimProperty::Scale) => {
                    output.entity.scale = Some(Vec3::new(value[0], value[1], value[2]));
                }
                // An unresolved bone channel is skipped, never redirected to
                // the model entity.
                (None, Some(_), _) => {}
            }
        }
        output.morph_weights.extend(
            clip.morph_channels
                .iter()
                .filter_map(|channel| {
                    lerp_morph_channel(channel, time)
                        .map(|weight| (channel.target_name.clone(), weight))
                }),
        );
    }

    /// Blends two evaluated outputs without consulting runtime state.
    ///
    /// Transform channels present on only one side pass through unchanged.
    /// Morph channels use zero as their neutral value.
    ///
    /// Prefer [`Self::blend_into`] with a [`PoseArena`] buffer on the
    /// per-frame path; this constructor allocates.
    #[must_use]
    pub fn blend(source: &Self, target: &Self, weight: f32) -> Self {
        let mut output = Self::default();
        output.blend_into(source, target, weight, None);
        output
    }

    /// Blends `source` toward `target` into this output.
    ///
    /// `mask` scales `weight` per joint, turning a whole-pose crossfade into
    /// a layered blend per bone; `None` weights every joint equally. The mask
    /// applies only to skeletal channels: the animator entity's own transform
    /// and logical morph weights are not bones and have no mask slot.
    pub fn blend_into(
        &mut self,
        source: &Self,
        target: &Self,
        weight: f32,
        mask: Option<&BoneMask>,
    ) {
        let weight = clamp_weight(weight);
        let joint_count = source.joints.len().max(target.joints.len());
        self.reset(joint_count);

        for index in 0..joint_count {
            let joint_weight = weight * mask.map_or(1.0, |mask| mask.weight(index));
            if let Some(value) = blend_vec3(
                active_translation(&source.joints, index),
                active_translation(&target.joints, index),
                joint_weight,
            ) {
                self.joints.write_translation(index, value);
            }
            if let Some(value) = blend_rotation(
                active_rotation(&source.joints, index),
                active_rotation(&target.joints, index),
                joint_weight,
            ) {
                self.joints.write_rotation(index, value);
            }
            if let Some(value) = blend_vec3(
                active_scale(&source.joints, index),
                active_scale(&target.joints, index),
                joint_weight,
            ) {
                self.joints.write_scale(index, value);
            }
        }

        self.entity = EntityPose {
            translation: blend_vec3(source.entity.translation, target.entity.translation, weight),
            rotation: blend_rotation(source.entity.rotation, target.entity.rotation, weight),
            scale: blend_vec3(source.entity.scale, target.entity.scale, weight),
        };
        for name in morph_names(source, target) {
            let from = source.morph_weights.get(name).copied().unwrap_or(0.0);
            let to = target.morph_weights.get(name).copied().unwrap_or(0.0);
            self.morph_weights
                .insert(name.to_owned(), from + (to - from) * weight);
        }
    }

    /// Builds the additive difference of `pose` relative to `reference` into
    /// this output.
    ///
    /// The delta is expressed the way [`Self::apply_additive_into`] consumes
    /// it: translation as an offset, rotation as a parent-relative rotation,
    /// and scale as a ratio.
    ///
    /// A skeletal channel contributes only when both inputs drive it. An
    /// additive difference against a channel the reference does not drive
    /// would need the rig's rest pose, which is deliberately not visible to
    /// this data-only stage. Morph weights have an unambiguous neutral value
    /// of zero, so a one-sided morph does produce a delta.
    pub fn additive_delta_into(&mut self, pose: &Self, reference: &Self) {
        let joint_count = pose.joints.len().max(reference.joints.len());
        self.reset(joint_count);

        for index in 0..joint_count {
            if let (Some(pose_value), Some(reference_value)) = (
                active_translation(&pose.joints, index),
                active_translation(&reference.joints, index),
            ) {
                self.joints
                    .write_translation(index, pose_value - reference_value);
            }
            if let (Some(pose_value), Some(reference_value)) = (
                active_rotation(&pose.joints, index),
                active_rotation(&reference.joints, index),
            ) {
                self.joints
                    .write_rotation(index, (reference_value.inverse() * pose_value).normalize());
            }
            if let (Some(pose_value), Some(reference_value)) = (
                active_scale(&pose.joints, index),
                active_scale(&reference.joints, index),
            ) {
                self.joints
                    .write_scale(index, scale_ratio(pose_value, reference_value));
            }
        }

        self.entity = EntityPose {
            translation: match (pose.entity.translation, reference.entity.translation) {
                (Some(pose_value), Some(reference_value)) => Some(pose_value - reference_value),
                _ => None,
            },
            rotation: match (pose.entity.rotation, reference.entity.rotation) {
                (Some(pose_value), Some(reference_value)) => {
                    Some((reference_value.inverse() * pose_value).normalize())
                }
                _ => None,
            },
            scale: match (pose.entity.scale, reference.entity.scale) {
                (Some(pose_value), Some(reference_value)) => {
                    Some(scale_ratio(pose_value, reference_value))
                }
                _ => None,
            },
        };
        for name in morph_names(pose, reference) {
            let pose_weight = pose.morph_weights.get(name).copied().unwrap_or(0.0);
            let reference_weight = reference.morph_weights.get(name).copied().unwrap_or(0.0);
            self.morph_weights
                .insert(name.to_owned(), pose_weight - reference_weight);
        }
    }

    /// Applies an additive `delta` on top of `base` into this output.
    ///
    /// `weight` scales the delta and `mask` scales it further per joint. A
    /// channel driven only by `base` passes through unchanged. A skeletal
    /// channel driven only by `delta` is dropped for the same reason
    /// [`Self::additive_delta_into`] gives: there is no pose to add it to
    /// without the rig's rest pose. Morph weights treat a missing base as
    /// zero.
    pub fn apply_additive_into(
        &mut self,
        base: &Self,
        delta: &Self,
        weight: f32,
        mask: Option<&BoneMask>,
    ) {
        let weight = clamp_weight(weight);
        let joint_count = base.joints.len().max(delta.joints.len());
        self.reset(joint_count);

        for index in 0..joint_count {
            let joint_weight = weight * mask.map_or(1.0, |mask| mask.weight(index));
            if let Some(base_value) = active_translation(&base.joints, index) {
                let added = active_translation(&delta.joints, index)
                    .map_or(base_value, |delta_value| base_value + delta_value * joint_weight);
                self.joints.write_translation(index, added);
            }
            if let Some(base_value) = active_rotation(&base.joints, index) {
                let added = active_rotation(&delta.joints, index).map_or(base_value, |delta_value| {
                    (base_value * Quat::IDENTITY.slerp(delta_value, joint_weight)).normalize()
                });
                self.joints.write_rotation(index, added);
            }
            if let Some(base_value) = active_scale(&base.joints, index) {
                let added = active_scale(&delta.joints, index).map_or(base_value, |delta_value| {
                    base_value * Vec3::ONE.lerp(delta_value, joint_weight)
                });
                self.joints.write_scale(index, added);
            }
        }

        self.entity = EntityPose {
            translation: base.entity.translation.map(|base_value| {
                delta
                    .entity
                    .translation
                    .map_or(base_value, |delta_value| base_value + delta_value * weight)
            }),
            rotation: base.entity.rotation.map(|base_value| {
                delta.entity.rotation.map_or(base_value, |delta_value| {
                    (base_value * Quat::IDENTITY.slerp(delta_value, weight)).normalize()
                })
            }),
            scale: base.entity.scale.map(|base_value| {
                delta
                    .entity
                    .scale
                    .map_or(base_value, |delta_value| base_value * Vec3::ONE.lerp(delta_value, weight))
            }),
        };
        for name in morph_names(base, delta) {
            let base_weight = base.morph_weights.get(name).copied().unwrap_or(0.0);
            let delta_weight = delta.morph_weights.get(name).copied().unwrap_or(0.0);
            self.morph_weights
                .insert(name.to_owned(), base_weight + delta_weight * weight);
        }
    }
}

/// Clamps a blend weight, treating a non-finite value as zero so malformed
/// runtime state cannot produce a non-finite pose.
fn clamp_weight(weight: f32) -> f32 {
    if weight.is_finite() {
        weight.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The union of two outputs' morph names, in unspecified order.
fn morph_names<'a>(left: &'a PoseGraphOutput, right: &'a PoseGraphOutput) -> HashSet<&'a str> {
    left.morph_weights
        .keys()
        .chain(right.morph_weights.keys())
        .map(String::as_str)
        .collect()
}

/// The component-wise ratio `pose / reference`, treating a degenerate
/// reference component as no change rather than producing a non-finite scale.
fn scale_ratio(pose: Vec3, reference: Vec3) -> Vec3 {
    Vec3::new(
        safe_ratio(pose.x, reference.x),
        safe_ratio(pose.y, reference.y),
        safe_ratio(pose.z, reference.z),
    )
}

fn safe_ratio(pose: f32, reference: f32) -> f32 {
    if reference.abs() > f32::EPSILON {
        pose / reference
    } else {
        1.0
    }
}

/// The layer's translation at `joint_index`, or `None` when that channel is
/// not owned.
fn active_translation(layer: &PoseLayer, joint_index: usize) -> Option<Vec3> {
    active_channel(layer, joint_index, PoseChannels::TRANSLATION)
        .map(|transform| transform.translation)
}

fn active_rotation(layer: &PoseLayer, joint_index: usize) -> Option<Quat> {
    active_channel(layer, joint_index, PoseChannels::ROTATION).map(|transform| transform.rotation)
}

fn active_scale(layer: &PoseLayer, joint_index: usize) -> Option<Vec3> {
    active_channel(layer, joint_index, PoseChannels::SCALE).map(|transform| transform.scale)
}

fn active_channel(
    layer: &PoseLayer,
    joint_index: usize,
    channel: PoseChannels,
) -> Option<&crate::transform::Transform> {
    layer
        .channels(joint_index)
        .filter(|channels| channels.contains(channel))
        .and_then(|_| layer.transform(joint_index))
}

fn blend_vec3(source: Option<Vec3>, target: Option<Vec3>, weight: f32) -> Option<Vec3> {
    match (source, target) {
        (Some(source), Some(target)) => Some(source.lerp(target, weight)),
        (Some(source), None) => Some(source),
        (None, Some(target)) => Some(target),
        (None, None) => None,
    }
}

fn blend_rotation(source: Option<Quat>, target: Option<Quat>, weight: f32) -> Option<Quat> {
    match (source, target) {
        (Some(source), Some(mut target)) => {
            if source.dot(target) < 0.0 {
                target = -target;
            }
            Some(source.lerp(target, weight).normalize())
        }
        (Some(source), None) => Some(source),
        (None, Some(target)) => Some(target),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{AnimChannel, Keyframe};
    use crate::transform::Transform;

    #[test]
    fn sampling_resolves_bones_without_joint_entities() {
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: Some(BoneId(7)),
                keyframes: vec![Keyframe {
                    time: 0.0,
                    value: [2.0, 3.0, 4.0, 1.0],
                }],
            }],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let bone_index = HashMap::from_iter([(BoneId(7), 0)]);
        let output = PoseGraphOutput::sample(&clip, 0.0, 1, &bone_index);
        assert_eq!(
            output.joints.transform(0).unwrap().translation,
            Vec3::new(2.0, 3.0, 4.0)
        );
        assert!(output.entity.translation.is_none());
    }

    #[test]
    fn sampling_skips_a_bone_channel_the_skeleton_does_not_carry() {
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: Some(BoneId(99)),
                keyframes: vec![Keyframe {
                    time: 0.0,
                    value: [2.0, 3.0, 4.0, 1.0],
                }],
            }],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let bone_index = HashMap::from_iter([(BoneId(7), 0)]);
        let output = PoseGraphOutput::sample(&clip, 0.0, 1, &bone_index);

        // An unresolved bone must not fall through to the model entity.
        assert_eq!(output.joints.channels(0), Some(PoseChannels::NONE));
        assert!(output.entity.translation.is_none());
    }

    #[test]
    fn blending_preserves_one_sided_channels() {
        let mut source = PoseGraphOutput {
            joints: PoseLayer::new(1, PoseBlend::Replace),
            entity: EntityPose::default(),
            morph_weights: HashMap::new(),
        };
        source.joints.write_translation(0, Vec3::X);
        let mut target = source.clone();
        target.joints.clear();
        target.joints.write_rotation(0, Quat::from_rotation_y(1.0));
        let output = PoseGraphOutput::blend(&source, &target, 0.5);
        let channels = output.joints.channels(0).unwrap();
        assert!(channels.contains(PoseChannels::TRANSLATION));
        assert!(channels.contains(PoseChannels::ROTATION));
        assert_eq!(output.joints.transform(0).unwrap().translation, Vec3::X);
    }

    /// A pose driving one joint's rotation, translation, and scale.
    fn joint_pose(joint_count: usize, index: usize, transform: Transform) -> PoseGraphOutput {
        let mut output = PoseGraphOutput::default();
        output.reset(joint_count);
        output
            .joints
            .set_transform(index, transform, PoseChannels::ALL);
        output
    }

    #[test]
    fn a_bone_mask_scales_the_blend_weight_per_joint() {
        let source = joint_pose(2, 0, Transform::from_translation(Vec3::ZERO));
        let mut target = joint_pose(2, 0, Transform::from_translation(Vec3::new(0.0, 4.0, 0.0)));
        target
            .joints
            .set_transform(
                1,
                Transform::from_translation(Vec3::new(0.0, 4.0, 0.0)),
                PoseChannels::ALL,
            );
        let mut masked_source = source.clone();
        masked_source.joints.set_transform(
            1,
            Transform::from_translation(Vec3::ZERO),
            PoseChannels::ALL,
        );

        let mut mask = BoneMask::uniform(2, 1.0);
        assert!(mask.set_weight(1, 0.0));
        let mut output = PoseGraphOutput::default();
        output.blend_into(&masked_source, &target, 1.0, Some(&mask));

        assert_eq!(
            output.joints.transform(0).unwrap().translation,
            Vec3::new(0.0, 4.0, 0.0)
        );
        // The masked-out joint keeps the source pose even at full weight.
        assert_eq!(output.joints.transform(1).unwrap().translation, Vec3::ZERO);
    }

    #[test]
    fn a_subtree_mask_covers_descendants_and_nothing_else() {
        // 0 -> 1 -> 2, plus an unrelated sibling 3 under the root.
        let parents = [None, Some(0), Some(1), Some(0)];

        let mask = BoneMask::from_subtree(&parents, 1, 1.0, 0.0);

        assert_eq!(mask.weight(0), 0.0);
        assert_eq!(mask.weight(1), 1.0);
        assert_eq!(mask.weight(2), 1.0);
        assert_eq!(mask.weight(3), 0.0);
    }

    #[test]
    fn an_additive_delta_round_trips_through_apply() {
        let reference = joint_pose(
            1,
            0,
            Transform {
                translation: Vec3::new(1.0, 0.0, 0.0),
                rotation: Quat::from_rotation_z(0.25),
                scale: Vec3::splat(2.0),
            },
        );
        let pose = joint_pose(
            1,
            0,
            Transform {
                translation: Vec3::new(1.0, 3.0, 0.0),
                rotation: Quat::from_rotation_z(0.75),
                scale: Vec3::splat(4.0),
            },
        );

        let mut delta = PoseGraphOutput::default();
        delta.additive_delta_into(&pose, &reference);
        let mut applied = PoseGraphOutput::default();
        applied.apply_additive_into(&reference, &delta, 1.0, None);

        let result = applied.joints.transform(0).expect("joint must be driven");
        assert!(result.translation.distance(pose.joints.transform(0).unwrap().translation) < 1.0e-5);
        assert!(result.rotation.angle_between(Quat::from_rotation_z(0.75)) < 1.0e-4);
        assert!((result.scale.x - 4.0).abs() < 1.0e-5);
    }

    #[test]
    fn a_zero_weight_additive_leaves_the_base_pose_untouched() {
        let base = joint_pose(
            1,
            0,
            Transform {
                translation: Vec3::Y,
                rotation: Quat::from_rotation_x(0.5),
                scale: Vec3::splat(1.5),
            },
        );
        let mut delta = PoseGraphOutput::default();
        delta.reset(1);
        delta.joints.set_transform(
            0,
            Transform {
                translation: Vec3::new(9.0, 9.0, 9.0),
                rotation: Quat::from_rotation_y(1.0),
                scale: Vec3::splat(8.0),
            },
            PoseChannels::ALL,
        );

        let mut output = PoseGraphOutput::default();
        output.apply_additive_into(&base, &delta, 0.0, None);

        let result = output.joints.transform(0).expect("joint must be driven");
        assert!(result.translation.distance(Vec3::Y) < 1.0e-6);
        assert!(result.rotation.angle_between(Quat::from_rotation_x(0.5)) < 1.0e-5);
        assert!((result.scale.x - 1.5).abs() < 1.0e-6);
    }

    #[test]
    fn an_additive_channel_without_a_base_is_dropped() {
        let mut base = PoseGraphOutput::default();
        base.reset(1);
        let delta = joint_pose(1, 0, Transform::from_translation(Vec3::X));

        let mut output = PoseGraphOutput::default();
        output.apply_additive_into(&base, &delta, 1.0, None);

        // Adding to an undriven channel would need the rig rest pose, which
        // this data-only stage deliberately cannot see.
        assert_eq!(output.joints.channels(0), Some(PoseChannels::NONE));
    }

    #[test]
    fn an_arena_recycles_buffers_instead_of_allocating() {
        let mut arena = PoseArena::new();
        assert_eq!(arena.pooled(), 0);

        let mut buffer = arena.acquire(4);
        assert!(buffer.joints.write_translation(2, Vec3::X));
        arena.release(buffer);
        assert_eq!(arena.pooled(), 1);

        buffer = arena.acquire(4);

        assert_eq!(arena.pooled(), 0);
        assert_eq!(buffer.joints.len(), 4);
        // A recycled buffer must not leak the previous evaluation's channels.
        assert_eq!(buffer.joints.channels(2), Some(PoseChannels::NONE));
    }
}
