//! Layered local-space rig poses and deterministic world-space evaluation.
//!
//! This module separates authoritative skeletal pose data from joint
//! [`Transform`](crate::transform::Transform) components. Animation,
//! procedural constraints, and physics write distinct channel-masked layers.
//! [`RigPose`] composes those layers from the immutable rest pose and can
//! evaluate world matrices for any composition prefix.
//!
//! Pose buffers use [`SkeletonAsset`](crate::skeleton_asset::SkeletonAsset)
//! bone order. That order is parent-before-child for valid imported rigs,
//! allowing world-space forward kinematics to run in one forward pass.
//!
//! Joint `Transform` components remain the publication surface consumed by
//! hierarchy propagation, skinning, and bone attachments. Once the staged
//! migration in ADR 0106 is complete, runtime systems must not treat those
//! published transforms as authoritative animation input.

use engine_authoring::id::AssetId;
use glam::{Mat4, Quat, Vec3};
use hashbrown::HashMap;

use crate::skeleton_asset::SkeletonAsset;
use crate::skinning::Skeleton;
use crate::transform::Transform;

/// Identifies the last pose layer included in a composition or world
/// evaluation.
///
/// Variants are ordered by composition precedence. A stage includes every
/// earlier stage, beginning with the immutable rest pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum PoseStage {
    /// Includes only the imported rest pose.
    Rest = 0,
    /// Includes the rest pose and animation layer.
    Animation = 1,
    /// Includes rest, animation, and procedural constraint layers.
    Procedural = 2,
    /// Includes rest, animation, procedural, and physics layers.
    Physics = 3,
    /// Uses the stored final pose.
    ///
    /// [`RigPose::evaluate_world`] recomposes that pose before reading it,
    /// while [`RigPose::local_transform`] borrows immutably and therefore
    /// reports the result of the most recent [`RigPose::compose`] call
    /// without refreshing it.
    Final = 4,
}

impl PoseStage {
    /// Returns whether this stage includes `candidate`.
    #[must_use]
    const fn includes(self, candidate: Self) -> bool {
        self as u8 >= candidate as u8
    }
}

/// Controls how a pose layer combines its active channels with the result of
/// earlier layers.
///
/// The initial runtime migration uses [`Self::Replace`]. [`Self::Blend`]
/// reserves the composition contract required by later weighted pose and
/// layered-blend operations without changing the layer API.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PoseBlend {
    /// Active channels replace the value produced by earlier layers.
    Replace,
    /// Active channels interpolate from the earlier result toward this
    /// layer's value.
    ///
    /// Finite weights are clamped to `0.0..=1.0`. A non-finite weight is
    /// treated as zero so malformed runtime state cannot create a non-finite
    /// skeleton pose.
    Blend(f32),
}

impl PoseBlend {
    /// Returns the effective interpolation weight.
    ///
    /// `None` identifies exact replacement, avoiding unnecessary arithmetic
    /// and preserving the layer's value exactly.
    #[must_use]
    fn effective_weight(self) -> Option<f32> {
        match self {
            Self::Replace => None,
            Self::Blend(weight) if weight.is_finite() => Some(weight.clamp(0.0, 1.0)),
            Self::Blend(_) => Some(0.0),
        }
    }
}

/// Identifies which local transform channels a pose layer owns.
///
/// Channel ownership is independent for translation, rotation, and scale.
/// This is required by operations such as PMX mode 2 physics, which replaces
/// rotation while retaining animation-owned translation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoseChannels(u8);

impl PoseChannels {
    /// Selects no local transform channels.
    pub const NONE: Self = Self(0);
    /// Selects local translation.
    pub const TRANSLATION: Self = Self(1 << 0);
    /// Selects local rotation.
    pub const ROTATION: Self = Self(1 << 1);
    /// Selects local scale.
    pub const SCALE: Self = Self(1 << 2);
    /// Selects translation, rotation, and scale.
    pub const ALL: Self = Self(Self::TRANSLATION.0 | Self::ROTATION.0 | Self::SCALE.0);

    /// Returns a mask containing the channels selected by either value.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether every channel in `other` is selected.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns whether no channels are selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Stores one local transform per skeleton bone.
///
/// Indices are meaningful only together with the owning rig's skeleton.
/// Buffers constructed by [`Self::from_skeleton_rest`] follow
/// [`SkeletonAsset::bones`] order exactly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoseBuffer {
    /// Local transforms in stable skeleton order.
    local_transforms: Vec<Transform>,
}

impl PoseBuffer {
    /// Creates a pose buffer from local transforms already arranged in
    /// skeleton order.
    ///
    /// The caller must preserve the skeleton ordering contract. Runtime rig
    /// construction normally uses [`Self::from_skeleton_rest`] instead.
    #[must_use]
    pub fn new(local_transforms: Vec<Transform>) -> Self {
        Self { local_transforms }
    }

    /// Creates a complete pose from `skeleton`'s imported rest transforms.
    #[must_use]
    pub fn from_skeleton_rest(skeleton: &SkeletonAsset) -> Self {
        let local_transforms = skeleton
            .bones
            .iter()
            .map(|bone| Transform {
                translation: bone.rest_translation,
                rotation: bone.rest_rotation,
                scale: bone.rest_scale,
            })
            .collect();

        Self { local_transforms }
    }

    /// Returns the number of joints represented by this buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.local_transforms.len()
    }

    /// Returns whether this buffer contains no joints.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.local_transforms.is_empty()
    }

    /// Returns the local transform at `joint_index`.
    ///
    /// An invalid index returns `None` rather than performing an unchecked
    /// access.
    #[must_use]
    pub fn local_transform(&self, joint_index: usize) -> Option<&Transform> {
        self.local_transforms.get(joint_index)
    }

    /// Returns mutable access to the local transform at `joint_index`.
    ///
    /// Systems producing sparse overrides should use [`PoseLayer`] instead.
    /// This access exists for pure graph operations that produce a complete
    /// pose buffer.
    pub fn local_transform_mut(&mut self, joint_index: usize) -> Option<&mut Transform> {
        self.local_transforms.get_mut(joint_index)
    }

    /// Returns all local transforms in skeleton order.
    #[must_use]
    pub fn local_transforms(&self) -> &[Transform] {
        &self.local_transforms
    }

    /// Resizes this buffer to `joint_count`, keeping existing storage.
    ///
    /// New entries default to the identity transform. Values of surviving
    /// entries are left alone because every consumer of a resized buffer
    /// establishes ownership through channel masks before reading.
    pub fn resize(&mut self, joint_count: usize) {
        self.local_transforms
            .resize_with(joint_count, Transform::default);
    }
}

/// Stores sparse, channel-masked local transform overrides.
///
/// Storage remains contiguous even when only a few bones are active. This
/// avoids per-frame entity-keyed maps while channel masks determine which
/// values participate in composition.
#[derive(Debug, Clone, PartialEq)]
pub struct PoseLayer {
    /// Candidate transform values in skeleton order.
    values: PoseBuffer,
    /// Channels owned by each candidate value.
    channels: Vec<PoseChannels>,
    /// Composition behavior shared by this layer.
    blend: PoseBlend,
}

impl PoseLayer {
    /// Creates an inactive layer for `joint_count` joints.
    #[must_use]
    pub fn new(joint_count: usize, blend: PoseBlend) -> Self {
        Self {
            values: PoseBuffer::new(vec![Transform::default(); joint_count]),
            channels: vec![PoseChannels::NONE; joint_count],
            blend,
        }
    }

    /// Returns the number of joints represented by this layer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether this layer contains no joints.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns this layer's blend behavior.
    #[must_use]
    pub fn blend(&self) -> PoseBlend {
        self.blend
    }

    /// Changes how active channels combine with earlier layers.
    pub fn set_blend(&mut self, blend: PoseBlend) {
        self.blend = blend;
    }

    /// Clears channel ownership without reallocating pose storage.
    ///
    /// Procedural and physics layers are cleared at the start of each fixed
    /// step. The animation layer is persistent so pausing an animator retains
    /// its most recently sampled pose.
    pub fn clear(&mut self) {
        self.channels.fill(PoseChannels::NONE);
    }

    /// Returns the channels owned for `joint_index`.
    #[must_use]
    pub fn channels(&self, joint_index: usize) -> Option<PoseChannels> {
        self.channels.get(joint_index).copied()
    }

    /// Returns the stored candidate transform for `joint_index`.
    ///
    /// Callers must inspect [`Self::channels`] before consuming individual
    /// fields because inactive fields are only reusable storage and do not
    /// participate in composition.
    #[must_use]
    pub fn transform(&self, joint_index: usize) -> Option<&Transform> {
        self.values.local_transform(joint_index)
    }

    /// Replaces the stored transform and exact channel mask at `joint_index`.
    ///
    /// This method intentionally replaces channel ownership, which is why it
    /// is named `set_transform`. Incremental channel writers such as
    /// [`Self::write_rotation`] preserve existing ownership bits.
    ///
    /// Returns `false` when `joint_index` is outside this layer.
    pub fn set_transform(
        &mut self,
        joint_index: usize,
        transform: Transform,
        channels: PoseChannels,
    ) -> bool {
        let Some(value) = self.values.local_transform_mut(joint_index) else {
            return false;
        };
        let Some(owned_channels) = self.channels.get_mut(joint_index) else {
            return false;
        };

        *value = transform;
        *owned_channels = channels;
        true
    }

    /// Exchanges values and channel masks with `source`, leaving each layer
    /// its own [`PoseBlend`].
    ///
    /// Whole-layer producers such as pose-graph evaluation use this instead
    /// of assigning through [`RigPose::animation_layer_mut`], which would
    /// also overwrite the blend behavior configured on the destination.
    /// Exchanging rather than moving hands this layer's previous storage back
    /// to the producer, so a pooled scratch buffer
    /// ([`PoseArena`](crate::pose_graph::PoseArena)) keeps allocations out of
    /// the steady-state frame.
    ///
    /// Returns `false` and leaves both layers untouched when they describe
    /// different joint counts. Adopting a mismatched layer would break the
    /// invariant that every stage of one [`RigPose`] has exactly one entry
    /// per skeleton bone, which composition relies on.
    pub fn swap_values(&mut self, source: &mut PoseLayer) -> bool {
        if source.len() != self.len() {
            return false;
        }

        std::mem::swap(&mut self.values, &mut source.values);
        std::mem::swap(&mut self.channels, &mut source.channels);
        true
    }

    /// Resizes this layer to `joint_count` and clears every channel.
    ///
    /// Existing allocations are reused when the size is unchanged, which is
    /// the case every frame for a pooled buffer. The blend behavior resets to
    /// [`PoseBlend::Replace`] so a recycled buffer cannot carry a previous
    /// node's weighting into an unrelated evaluation.
    pub fn reset(&mut self, joint_count: usize) {
        self.values.resize(joint_count);
        self.channels.clear();
        self.channels.resize(joint_count, PoseChannels::NONE);
        self.blend = PoseBlend::Replace;
    }

    /// Writes local translation and adds translation ownership.
    ///
    /// Other values and ownership bits remain unchanged.
    pub fn write_translation(&mut self, joint_index: usize, translation: Vec3) -> bool {
        let Some(value) = self.values.local_transform_mut(joint_index) else {
            return false;
        };
        let Some(owned_channels) = self.channels.get_mut(joint_index) else {
            return false;
        };

        value.translation = translation;
        *owned_channels = owned_channels.union(PoseChannels::TRANSLATION);
        true
    }

    /// Writes local rotation and adds rotation ownership.
    ///
    /// The producer is responsible for supplying a valid normalized
    /// quaternion. Composition does not silently alter simulated rotations.
    pub fn write_rotation(&mut self, joint_index: usize, rotation: Quat) -> bool {
        let Some(value) = self.values.local_transform_mut(joint_index) else {
            return false;
        };
        let Some(owned_channels) = self.channels.get_mut(joint_index) else {
            return false;
        };

        value.rotation = rotation;
        *owned_channels = owned_channels.union(PoseChannels::ROTATION);
        true
    }

    /// Writes local scale and adds scale ownership.
    ///
    /// Physics layers normally leave this channel inactive so animation
    /// remains the owner of skeletal scale.
    pub fn write_scale(&mut self, joint_index: usize, scale: Vec3) -> bool {
        let Some(value) = self.values.local_transform_mut(joint_index) else {
            return false;
        };
        let Some(owned_channels) = self.channels.get_mut(joint_index) else {
            return false;
        };

        value.scale = scale;
        *owned_channels = owned_channels.union(PoseChannels::SCALE);
        true
    }

    /// Applies this layer's active channels to one destination transform.
    fn apply_joint_to(&self, joint_index: usize, destination: &mut Transform) {
        let Some(source) = self.values.local_transform(joint_index) else {
            return;
        };
        let Some(channels) = self.channels(joint_index) else {
            return;
        };

        match self.blend.effective_weight() {
            None => {
                if channels.contains(PoseChannels::TRANSLATION) {
                    destination.translation = source.translation;
                }
                if channels.contains(PoseChannels::ROTATION) {
                    destination.rotation = source.rotation;
                }
                if channels.contains(PoseChannels::SCALE) {
                    destination.scale = source.scale;
                }
            }
            Some(weight) => {
                if channels.contains(PoseChannels::TRANSLATION) {
                    destination.translation = destination.translation.lerp(source.translation, weight);
                }
                if channels.contains(PoseChannels::ROTATION) {
                    destination.rotation = destination.rotation.slerp(source.rotation, weight);
                }
                if channels.contains(PoseChannels::SCALE) {
                    destination.scale = destination.scale.lerp(source.scale, weight);
                }
            }
        }
    }

    /// Applies every active joint to `target`.
    fn apply_to(&self, target: &mut PoseBuffer) {
        debug_assert_eq!(self.values.len(), target.len());
        debug_assert_eq!(self.channels.len(), target.len());

        for joint_index in 0..target.len() {
            let Some(destination) = target.local_transform_mut(joint_index) else {
                continue;
            };
            self.apply_joint_to(joint_index, destination);
        }
    }
}

/// Owns every local-pose stage and cached FK result for one runtime rig.
///
/// The component is created from the same [`SkeletonAsset`] used to spawn the
/// rig's joint entities. Animation writes the persistent animation layer,
/// world-aware constraints write the transient procedural layer, and stateful
/// solvers write the transient physics layer.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoseDiscontinuity {
    Reposition,
    AnimationSeek,
}

/// The previous final pose is never used as a composition input.
#[derive(Debug, Clone, PartialEq)]
pub struct RigPose {
    /// Skeleton asset defining bone identity and index order.
    skeleton_asset: AssetId,
    /// Parent index per joint in pose-buffer order.
    ///
    /// Imported skeletons use parent-before-child order. World evaluation
    /// also validates each cached index and treats an invalid or forward
    /// parent as a root boundary instead of indexing out of range.
    parents: Vec<Option<usize>>,
    /// Immutable baseline copied from the imported skeleton.
    rest_pose: PoseBuffer,
    /// Persistent output from animation sampling or a future pure Pose Graph.
    animation_layer: PoseLayer,
    /// Transient output from Foot IK and other world-aware constraints.
    procedural_layer: PoseLayer,
    /// Transient output from rigid-body or secondary-motion simulation.
    physics_layer: PoseLayer,
    /// Deterministic result of the most recent [`Self::compose`] call.
    final_pose: PoseBuffer,
    /// Reused storage for the most recent [`Self::evaluate_world`] result.
    world_matrices: Vec<Mat4>,
    /// Set for one fixed step when this pose does not continue the previous
    /// one. The reason is retained so MMD seek reconstruction is not applied
    /// to loop wraps, state switches, teleports, or scene loads.
    discontinuity: Option<PoseDiscontinuity>,
}

impl RigPose {
    /// Creates a complete layered pose from `skeleton`.
    ///
    /// All pose layers and FK storage receive exactly one entry per bone.
    /// Rest transforms and parent indices come from the same bone array,
    /// preventing ordering drift between local and world evaluation.
    #[must_use]
    pub fn from_skeleton(skeleton: &SkeletonAsset) -> Self {
        let rest_pose = PoseBuffer::from_skeleton_rest(skeleton);
        let joint_count = rest_pose.len();
        let parents = skeleton.bones.iter().map(|bone| bone.parent).collect();

        Self {
            skeleton_asset: skeleton.id.clone(),
            parents,
            final_pose: rest_pose.clone(),
            rest_pose,
            animation_layer: PoseLayer::new(joint_count, PoseBlend::Replace),
            procedural_layer: PoseLayer::new(joint_count, PoseBlend::Replace),
            physics_layer: PoseLayer::new(joint_count, PoseBlend::Replace),
            world_matrices: vec![Mat4::IDENTITY; joint_count],
            discontinuity: None,
        }
    }

    /// Declares that this step's pose does not continue the previous step's.
    ///
    /// Simulation that carries state across steps — secondary motion above
    /// all — has to know when the pose it is following was *repositioned*
    /// rather than *moved*, because a repositioning is not motion and must
    /// not become velocity. Only the code that performs the repositioning
    /// knows this; it cannot be recovered from the pose itself, since a fast
    /// animation and a seek reach the reader as the same large displacement.
    /// Inferring it from displacement is therefore unsound at any threshold,
    /// and reseating a rig mid-swing is what that inference costs.
    ///
    /// Raise this for a loop wrap, an instant clip switch, a teleported
    /// character, a respawn, or a scene load. Animation seeks are declared
    /// separately by the animation system so secondary motion may reconstruct
    /// their clip history. Do **not** raise it for fast motion, however fast,
    /// nor for a crossfade, whose blended pose is continuous by construction.
    ///
    /// The declaration lasts exactly one fixed step: it is cleared by
    /// [`rig_pose_clear_transient_system`] alongside the transient layers, so
    /// every step starts continuous unless something says otherwise.
    pub fn mark_discontinuous(&mut self) {
        self.discontinuity = Some(PoseDiscontinuity::Reposition);
    }

    /// Declares a clock seek whose animation history may be reconstructed.
    ///
    /// A previously declared generic repositioning wins because a teleport or
    /// state switch in the same step invalidates the historical clip path.
    #[doc(hidden)]
    pub fn mark_animation_seek(&mut self) {
        if self.discontinuity.is_none() {
            self.discontinuity = Some(PoseDiscontinuity::AnimationSeek);
        }
    }

    /// Whether this step's pose was declared a repositioning rather than
    /// motion. See [`Self::mark_discontinuous`].
    #[must_use]
    pub fn is_discontinuous(&self) -> bool {
        self.discontinuity.is_some()
    }

    /// Returns whether the current discontinuity is an animation clock seek.
    #[doc(hidden)]
    pub fn is_animation_seek(&self) -> bool {
        matches!(
            self.discontinuity,
            Some(PoseDiscontinuity::AnimationSeek)
        )
    }

    /// Returns the skeleton asset defining pose-buffer indices.
    #[must_use]
    pub fn skeleton_asset(&self) -> &AssetId {
        &self.skeleton_asset
    }

    /// Returns the number of joints represented by every pose stage.
    #[must_use]
    pub fn joint_count(&self) -> usize {
        self.rest_pose.len()
    }

    /// Returns parent indices in pose-buffer order.
    #[must_use]
    pub fn parents(&self) -> &[Option<usize>] {
        &self.parents
    }

    /// Returns the immutable imported rest pose.
    #[must_use]
    pub fn rest_pose(&self) -> &PoseBuffer {
        &self.rest_pose
    }

    /// Returns the persistent animation layer.
    #[must_use]
    pub fn animation_layer(&self) -> &PoseLayer {
        &self.animation_layer
    }

    /// Returns mutable access to the animation layer.
    pub fn animation_layer_mut(&mut self) -> &mut PoseLayer {
        &mut self.animation_layer
    }

    /// Returns the transient procedural layer.
    #[must_use]
    pub fn procedural_layer(&self) -> &PoseLayer {
        &self.procedural_layer
    }

    /// Returns mutable access to the procedural layer.
    pub fn procedural_layer_mut(&mut self) -> &mut PoseLayer {
        &mut self.procedural_layer
    }

    /// Returns the transient physics layer.
    #[must_use]
    pub fn physics_layer(&self) -> &PoseLayer {
        &self.physics_layer
    }

    /// Returns mutable access to the physics layer.
    pub fn physics_layer_mut(&mut self) -> &mut PoseLayer {
        &mut self.physics_layer
    }

    /// Returns the most recently composed final pose.
    #[must_use]
    pub fn final_pose(&self) -> &PoseBuffer {
        &self.final_pose
    }

    /// Returns the world matrices produced by the latest world evaluation.
    ///
    /// Before the first evaluation every entry is identity.
    #[must_use]
    pub fn world_matrices(&self) -> &[Mat4] {
        &self.world_matrices
    }

    /// Returns one local transform after composing through `stage`.
    ///
    /// This is the local-space counterpart of [`Self::evaluate_world`]. It
    /// lets ordered modifiers inspect their input stage without reading a
    /// previously published joint [`Transform`]. Invalid indices return
    /// `None`.
    ///
    /// [`PoseStage::Final`] reports the most recently composed pose rather
    /// than recomposing, because this method borrows immutably. Ordered
    /// modifiers request their own input stage instead, so that difference
    /// does not affect them.
    #[must_use]
    pub fn local_transform(&self, stage: PoseStage, joint_index: usize) -> Option<Transform> {
        (joint_index < self.joint_count())
            .then(|| self.local_transform_for_stage(joint_index, stage))
    }

    /// Clears layers whose values last for only one fixed step.
    ///
    /// Animation remains intact so a paused or stopped animator retains its
    /// most recently sampled pose. Procedural and physics outputs must be
    /// regenerated from the current animation result.
    pub fn clear_transient_layers(&mut self) {
        self.procedural_layer.clear();
        self.physics_layer.clear();
        self.discontinuity = None;
    }

    /// Rebuilds the final local pose from rest and every active layer.
    ///
    /// Composition always starts from the immutable rest pose. Calling this
    /// method repeatedly without changing a layer therefore produces the
    /// same result and never accumulates the previous final pose.
    pub fn compose(&mut self) {
        self.final_pose.clone_from(&self.rest_pose);
        self.animation_layer.apply_to(&mut self.final_pose);
        self.procedural_layer.apply_to(&mut self.final_pose);
        self.physics_layer.apply_to(&mut self.final_pose);
    }

    /// Evaluates world matrices for a composition prefix rooted at `root`.
    ///
    /// Root bones use `root` as their parent world matrix. Valid child bones
    /// use the already evaluated world matrix of their parent. A malformed
    /// parent index that points forward or outside the pose is treated as a
    /// root boundary, matching the engine's non-panicking hierarchy policy.
    ///
    /// Requesting [`PoseStage::Final`] first refreshes the stored final pose
    /// through [`Self::compose`].
    pub fn evaluate_world(&mut self, stage: PoseStage, root: Mat4) -> &[Mat4] {
        if stage == PoseStage::Final {
            self.compose();
        }

        for joint_index in 0..self.joint_count() {
            let local_transform = self.local_transform_for_stage(joint_index, stage);
            let parent_world = self
                .parents
                .get(joint_index)
                .copied()
                .flatten()
                .filter(|parent_index| *parent_index < joint_index)
                .and_then(|parent_index| self.world_matrices.get(parent_index).copied())
                .unwrap_or(root);

            if let Some(world_matrix) = self.world_matrices.get_mut(joint_index) {
                *world_matrix = parent_world * local_transform.to_matrix();
            }
        }

        &self.world_matrices
    }

    /// Builds one local transform for the requested composition prefix.
    fn local_transform_for_stage(&self, joint_index: usize, stage: PoseStage) -> Transform {
        if stage == PoseStage::Final {
            return self
                .final_pose
                .local_transform(joint_index)
                .cloned()
                .unwrap_or_default();
        }

        let mut result = self
            .rest_pose
            .local_transform(joint_index)
            .cloned()
            .unwrap_or_default();

        if stage.includes(PoseStage::Animation) {
            self.animation_layer.apply_joint_to(joint_index, &mut result);
        }
        if stage.includes(PoseStage::Procedural) {
            self.procedural_layer.apply_joint_to(joint_index, &mut result);
        }
        if stage.includes(PoseStage::Physics) {
            self.physics_layer.apply_joint_to(joint_index, &mut result);
        }

        result
    }
}

/// Clears procedural and physics output at the start of a fixed pose step.
///
/// The animation layer deliberately survives so paused animators retain
/// their sampled pose. Every transient modifier must reproduce its output in
/// the current step, preventing stale physics or IK overrides from leaking
/// into later composition.
pub fn rig_pose_clear_transient_system(mut rigs: engine_ecs::Query<&mut RigPose>) {
    for (_, rig_pose) in rigs.iter_mut() {
        rig_pose.clear_transient_layers();
    }
}

/// Composes every rig and publishes its final local pose to joint entities.
///
/// Joint [`Transform`] components are a compatibility surface for hierarchy
/// propagation, skinning, and attachments. They are never read back into a
/// pose layer. A malformed rig whose joint count differs from its pose count
/// publishes the common prefix and leaves unmatched entities unchanged.
pub fn publish_final_rig_pose_system(
    mut rigs: engine_ecs::Query<(&Skeleton, &mut RigPose)>,
    mut transforms: engine_ecs::Query<&mut Transform>,
) {
    // Keyed by joint entity rather than scanned as a list: the destination
    // query visits every transform in the world, so a linear lookup here is
    // quadratic in (scene entities x rig joints) for characters the size of
    // a PMX model.
    let mut writes: HashMap<engine_ecs::Entity, Transform> = HashMap::new();
    for (_, (skeleton, rig_pose)) in rigs.iter_mut() {
        rig_pose.compose();
        writes.extend(
            skeleton
                .joints
                .iter()
                .copied()
                .zip(rig_pose.final_pose().local_transforms().iter().cloned()),
        );
    }
    if writes.is_empty() {
        return;
    }

    for (entity, transform) in transforms.iter_mut() {
        if let Some(published) = writes.get(&entity) {
            *transform = published.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef, BoneId};

    fn test_skeleton() -> SkeletonAsset {
        let bones = vec![
            BoneDef {
                id: BoneId(0),
                name: "root".to_owned(),
                parent: None,
                rest_translation: Vec3::new(1.0, 2.0, 3.0),
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            },
            BoneDef {
                id: BoneId(1),
                name: "child".to_owned(),
                parent: Some(0),
                rest_translation: Vec3::Y,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            },
        ];
        let identity = compute_skeleton_identity(&bones);

        SkeletonAsset {
            id: AssetId::generate(),
            name: "test".to_owned(),
            bones,
            identity,
            next_bone_id: 2,
        }
    }

    fn translation(matrix: Mat4) -> Vec3 {
        matrix.w_axis.truncate()
    }

    #[test]
    fn rig_pose_starts_at_the_skeleton_rest_pose() {
        let skeleton = test_skeleton();
        let pose = RigPose::from_skeleton(&skeleton);

        assert_eq!(pose.joint_count(), 2);
        assert_eq!(pose.parents(), &[None, Some(0)]);
        assert_eq!(pose.rest_pose(), pose.final_pose());
    }

    #[test]
    fn empty_layers_compose_to_the_rest_pose() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);

        pose.compose();

        assert_eq!(pose.final_pose(), pose.rest_pose());
    }

    #[test]
    fn composition_is_idempotent() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);

        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::new(0.0, 4.0, 0.0)));
        assert!(pose
            .physics_layer_mut()
            .write_rotation(1, Quat::from_rotation_z(0.5)));

        pose.compose();
        let first = pose.final_pose().clone();
        pose.compose();

        assert_eq!(pose.final_pose(), &first);
    }

    #[test]
    fn composition_applies_layers_in_declared_precedence() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);
        let animation_rotation = Quat::from_rotation_x(0.25);
        let procedural_rotation = Quat::from_rotation_y(0.5);
        let physics_rotation = Quat::from_rotation_z(0.75);

        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::new(4.0, 5.0, 6.0)));
        assert!(pose
            .animation_layer_mut()
            .write_rotation(1, animation_rotation));
        assert!(pose
            .procedural_layer_mut()
            .write_rotation(1, procedural_rotation));
        assert!(pose
            .physics_layer_mut()
            .write_rotation(1, physics_rotation));

        pose.compose();

        let child = pose
            .final_pose()
            .local_transform(1)
            .expect("child must exist");
        assert_eq!(child.translation, Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(child.rotation, physics_rotation);
        assert_eq!(child.scale, Vec3::ONE);
    }

    #[test]
    fn rotation_only_physics_preserves_animation_translation_and_scale() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);
        let physics_rotation = Quat::from_rotation_y(1.0);

        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::new(0.0, 3.0, 0.0)));
        assert!(pose
            .animation_layer_mut()
            .write_scale(1, Vec3::splat(1.5)));
        assert!(pose
            .physics_layer_mut()
            .write_rotation(1, physics_rotation));

        pose.compose();

        let child = pose
            .final_pose()
            .local_transform(1)
            .expect("child must exist");
        assert_eq!(child.translation, Vec3::new(0.0, 3.0, 0.0));
        assert_eq!(child.rotation, physics_rotation);
        assert_eq!(child.scale, Vec3::splat(1.5));
    }

    #[test]
    fn set_transform_replaces_previous_channel_ownership() {
        let mut layer = PoseLayer::new(1, PoseBlend::Replace);

        assert!(layer.write_rotation(0, Quat::from_rotation_x(0.5)));
        assert!(layer.set_transform(
            0,
            Transform::from_translation(Vec3::Y),
            PoseChannels::TRANSLATION,
        ));

        assert_eq!(layer.channels(0), Some(PoseChannels::TRANSLATION));
    }

    #[test]
    fn swap_values_keeps_each_layer_its_own_blend_behavior() {
        let mut destination = PoseLayer::new(2, PoseBlend::Blend(0.5));
        assert!(destination.write_scale(0, Vec3::splat(2.0)));
        let mut source = PoseLayer::new(2, PoseBlend::Replace);
        assert!(source.write_translation(1, Vec3::new(0.0, 6.0, 0.0)));

        assert!(destination.swap_values(&mut source));

        assert_eq!(destination.blend(), PoseBlend::Blend(0.5));
        assert_eq!(destination.channels(1), Some(PoseChannels::TRANSLATION));
        // The producer receives the destination's previous storage, which is
        // what lets a pooled buffer avoid reallocating next frame.
        assert_eq!(source.blend(), PoseBlend::Replace);
        assert_eq!(source.channels(0), Some(PoseChannels::SCALE));
    }

    #[test]
    fn swap_values_rejects_a_layer_built_for_another_skeleton() {
        let mut destination = PoseLayer::new(2, PoseBlend::Replace);
        assert!(destination.write_translation(0, Vec3::X));
        let mut source = PoseLayer::new(0, PoseBlend::Replace);

        assert!(!destination.swap_values(&mut source));

        assert_eq!(destination.len(), 2);
        assert_eq!(destination.channels(0), Some(PoseChannels::TRANSLATION));
        assert_eq!(source.len(), 0);
    }

    #[test]
    fn resetting_a_layer_clears_ownership_and_resizes() {
        let mut layer = PoseLayer::new(2, PoseBlend::Blend(0.25));
        assert!(layer.write_rotation(1, Quat::from_rotation_x(0.5)));

        layer.reset(3);

        assert_eq!(layer.len(), 3);
        assert_eq!(layer.blend(), PoseBlend::Replace);
        for index in 0..3 {
            assert_eq!(layer.channels(index), Some(PoseChannels::NONE));
        }
    }

    #[test]
    fn blend_interpolates_active_channels() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);

        pose.animation_layer_mut()
            .set_blend(PoseBlend::Blend(0.25));
        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::new(0.0, 5.0, 0.0)));

        pose.compose();

        let child = pose
            .final_pose()
            .local_transform(1)
            .expect("child must exist");
        assert_eq!(child.translation, Vec3::new(0.0, 2.0, 0.0));
    }

    #[test]
    fn clearing_transient_layers_preserves_animation_ownership() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);

        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::Y));
        assert!(pose
            .procedural_layer_mut()
            .write_rotation(1, Quat::from_rotation_x(0.5)));
        assert!(pose
            .physics_layer_mut()
            .write_rotation(1, Quat::from_rotation_z(0.5)));

        pose.clear_transient_layers();

        assert_eq!(
            pose.animation_layer().channels(1),
            Some(PoseChannels::TRANSLATION)
        );
        assert_eq!(
            pose.procedural_layer().channels(1),
            Some(PoseChannels::NONE)
        );
        assert_eq!(
            pose.physics_layer().channels(1),
            Some(PoseChannels::NONE)
        );
    }

    #[test]
    fn world_evaluation_accumulates_parent_before_child_transforms() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);
        let root_world = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));

        let matrices = pose.evaluate_world(PoseStage::Rest, root_world);

        assert_eq!(translation(matrices[0]), Vec3::new(11.0, 2.0, 3.0));
        assert_eq!(translation(matrices[1]), Vec3::new(11.0, 3.0, 3.0));
    }

    #[test]
    fn world_evaluation_respects_the_requested_pose_stage() {
        let skeleton = test_skeleton();
        let mut pose = RigPose::from_skeleton(&skeleton);

        assert!(pose
            .animation_layer_mut()
            .write_translation(1, Vec3::new(0.0, 2.0, 0.0)));
        assert!(pose
            .procedural_layer_mut()
            .write_translation(1, Vec3::new(0.0, 4.0, 0.0)));

        let animation_world = pose
            .evaluate_world(PoseStage::Animation, Mat4::IDENTITY)
            .to_vec();
        let procedural_world = pose
            .evaluate_world(PoseStage::Procedural, Mat4::IDENTITY)
            .to_vec();

        assert_eq!(
            translation(animation_world[1]),
            Vec3::new(1.0, 4.0, 3.0)
        );
        assert_eq!(
            translation(procedural_world[1]),
            Vec3::new(1.0, 6.0, 3.0)
        );
    }

    #[test]
    fn out_of_range_layer_writes_fail_without_modification() {
        let mut layer = PoseLayer::new(1, PoseBlend::Replace);

        assert!(!layer.write_translation(4, Vec3::ONE));
        assert!(!layer.write_rotation(4, Quat::IDENTITY));
        assert!(!layer.write_scale(4, Vec3::ONE));
        assert_eq!(layer.channels(0), Some(PoseChannels::NONE));
    }
}
