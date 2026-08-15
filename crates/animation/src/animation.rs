//! Animation clip asset and Animator component (Phase 37).
//!
//! Provides keyframe-based animation playback on a fixed-timestep schedule.
//! Only linear interpolation is supported in v1; step and cubic-spline are
//! deferred.  Animation tracks are imported from glTF files in Phase 36/37.
//!
//! Phase 59 adds [`Animator::crossfade_to`] (two-clip blending, see
//! [`animation_system`]'s composition layer), [`AnimationClip::events`]
//! (timeline markers fired into [`AnimationEvents`]), and the
//! [`crate::anim_graph`] state machine runtime built on top of both.

use crate::asset::{Assets, Handle};
use crate::contact_detect::ContactInterval;
use crate::morph::{MorphTargets, MorphWeights};
use crate::pose_graph::PoseArena;
use crate::rig_pose::RigPose;
use crate::skeleton_asset::{BoneId, SkeletonIdentity};
use crate::skinning::{Skeleton, SkinnedMesh};
use crate::time::FixedTime;
use crate::transform::Transform;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use glam::{Quat, Vec3};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Asset types
// ---------------------------------------------------------------------------

/// Which transform channel a keyframe sequence targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimProperty {
    /// Local translation (XYZ; W component of keyframe values is ignored).
    Translation,
    /// Local rotation (XYZW quaternion).
    Rotation,
    /// Local scale (XYZ; W component of keyframe values is ignored).
    Scale,
}

/// A single keyframe value in a sampler channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time in seconds from the start of the clip.
    pub time: f32,
    /// Interpolation value: XYZ for translation/scale, XYZW for rotation.
    pub value: [f32; 4],
}

/// A single channel of keyframe data targeting one transform property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimChannel {
    /// Which transform property this channel drives.
    pub property: AnimProperty,
    /// Which bone of the entity's [`crate::skinning::Skeleton`] this channel
    /// drives, resolved through [`Skeleton::bone_ids`] (ADR 0043, revised by
    /// ADR 0074 and ADR 0077).
    ///
    /// `None` drives the [`Animator`] entity's own [`Transform`] (Phase 37
    /// behavior, unchanged). An unresolvable [`BoneId`] (not present in the
    /// skeleton, or no skeleton at all) skips the channel instead of
    /// panicking. A channel with `target_bone: Some(_)` requires
    /// [`AnimationClip::skeleton`] to be set — see
    /// [`AnimationClip::validate`].
    pub target_bone: Option<BoneId>,
    /// Keyframe samples, sorted by `time` ascending.
    pub keyframes: Vec<Keyframe>,
}

/// One scalar animation channel targeting a logical model morph by name.
///
/// A PMX model can split one logical morph across several render meshes, so
/// an imported motion cannot bind this channel to one mesh-local sub-asset
/// ID. The animation system instead resolves the name against every
/// [`MorphTargets`] component attached to the animator's rig.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorphChannel {
    /// Decoded source morph name used for runtime target lookup.
    pub target_name: String,
    /// Scalar samples sorted by time, stored in [`Keyframe::value`]'s first
    /// element so transform and morph tracks share one serialized key type.
    pub keyframes: Vec<Keyframe>,
}

/// A clip asset containing one or more animation channels.
///
/// Serializable so it can be persisted as a derived cache entry (a baked
/// retarget output, ADR 0079 §3); this is never a hand-authored file format
/// in its own right, so it carries no `schema_version` field of its own —
/// callers that persist an `AnimationClip` wrap it in an envelope that does
/// (see `crate::retarget`'s baked-clip helpers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationClip {
    /// Total clip length in seconds.
    pub duration: f32,
    /// Per-property keyframe tracks.
    pub channels: Vec<AnimChannel>,
    /// Scalar model-morph tracks resolved by logical morph name.
    #[serde(default)]
    pub morph_channels: Vec<MorphChannel>,
    /// Named timeline markers fired into [`AnimationEvents`] as playback
    /// crosses their [`AnimEvent::time`] (Phase 59). See [`animation_system`]
    /// for the exact crossing rule, including loop-wrap handling.
    ///
    /// Empty for every glTF-imported clip: `.gltf` has no native event
    /// concept, so events are added by code after import.
    pub events: Vec<AnimEvent>,
    /// The skeleton this clip was sampled against (ADR 0077).
    ///
    /// Required whenever any channel has [`AnimChannel::target_bone`] set;
    /// see [`AnimationClip::validate`]. `None` for code-authored,
    /// single-entity clips (Phase 37 behavior, all targets `None`), which
    /// stay valid without a skeleton.
    pub skeleton: Option<AssetId>,
    /// The skeleton's canonical structure hash at the time this clip was
    /// sampled, so a stale binding (the skeleton was reimported and its
    /// bones changed) is detectable instead of silently wrong.
    pub skeleton_identity: Option<SkeletonIdentity>,
    /// The bone whose translation carries locomotion.
    ///
    /// Input to root-motion extraction (this module's private
    /// `extract_root_motion`) and, in a later phase, to the retargeting
    /// translation policy. Auto-detected at import as the topmost bone with
    /// any translation channel.
    pub root_bone: Option<BoneId>,
    /// Detected ground-contact spans, sorted by `(start, bone)` (ADR 0080 §1).
    ///
    /// Populated by [`crate::contact_detect::detect_contact_intervals`] at
    /// import time, and **re-detected** (never copied) against the target
    /// skeleton when [`crate::retarget::retarget_clip`] bakes a retargeted
    /// clip, since a bone's own proportions decide where it plants. Empty
    /// for clips authored before ADR 0080, code-authored clips with no foot
    /// bones, and any clip with no candidate bones at all.
    ///
    /// `#[serde(default)]` so a baked cache entry from before this field
    /// existed still deserializes (as an empty list) instead of failing —
    /// baked entries are disposable and simply get re-baked with contacts
    /// on the next miss.
    #[serde(default)]
    pub contacts: Vec<ContactInterval>,
}

/// Reports why several imported clips cannot form one logical motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipCompositionError {
    /// Every layer must target the same skeleton and skeleton identity.
    SkeletonMismatch,
}

impl fmt::Display for ClipCompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SkeletonMismatch => formatter.write_str(
                "animation layers target different skeletons and cannot be composed",
            ),
        }
    }
}

impl std::error::Error for ClipCompositionError {}

/// Combines a primary clip and ordered overlays into one runtime clip.
///
/// Non-overlapping transform and morph channels are unioned. When two layers
/// drive the same logical channel, the later overlay replaces the earlier
/// channel as a whole. This deterministic rule models synchronized body/face
/// VMD packages without introducing runtime layer weights or masks.
pub fn compose_animation_clips(
    primary: &AnimationClip,
    overlays: &[AnimationClip],
) -> Result<AnimationClip, ClipCompositionError> {
    let mut transform_channels = BTreeMap::<(u32, u8), AnimChannel>::new();
    let mut morph_channels = BTreeMap::<String, MorphChannel>::new();
    let mut duration = primary.duration;
    let mut events = primary.events.clone();

    insert_transform_channels(&mut transform_channels, &primary.channels);
    insert_morph_channels(&mut morph_channels, &primary.morph_channels);

    for overlay in overlays {
        if overlay.skeleton != primary.skeleton
            || overlay.skeleton_identity != primary.skeleton_identity
        {
            return Err(ClipCompositionError::SkeletonMismatch);
        }
        duration = duration.max(overlay.duration);
        insert_transform_channels(&mut transform_channels, &overlay.channels);
        insert_morph_channels(&mut morph_channels, &overlay.morph_channels);
        events.extend(overlay.events.iter().cloned());
    }
    events.sort_by(|left, right| {
        left.time
            .total_cmp(&right.time)
            .then(left.name.cmp(&right.name))
    });

    Ok(AnimationClip {
        duration,
        channels: transform_channels.into_values().collect(),
        morph_channels: morph_channels.into_values().collect(),
        events,
        skeleton: primary.skeleton.clone(),
        skeleton_identity: primary.skeleton_identity,
        root_bone: primary.root_bone,
        contacts: primary.contacts.clone(),
    })
}

fn insert_transform_channels(
    destination: &mut BTreeMap<(u32, u8), AnimChannel>,
    channels: &[AnimChannel],
) {
    for channel in channels {
        let target = channel
            .target_bone
            .map(|bone| bone.0)
            .unwrap_or(u32::MAX);
        destination.insert((target, channel.property as u8), channel.clone());
    }
}

fn insert_morph_channels(
    destination: &mut BTreeMap<String, MorphChannel>,
    channels: &[MorphChannel],
) {
    for channel in channels {
        destination.insert(channel.target_name.clone(), channel.clone());
    }
}

/// Diagnostic code reported by [`AnimationClip::validate`] when a channel
/// targets a bone but the clip has no skeleton reference (ADR 0077).
pub const CLIP_MISSING_SKELETON_DIAGNOSTIC: &str = "anim.clip_missing_skeleton";

impl AnimationClip {
    /// Validates the invariant that any [`AnimChannel::target_bone`] requires
    /// [`Self::skeleton`] to be set (ADR 0077).
    ///
    /// Returns a [`CLIP_MISSING_SKELETON_DIAGNOSTIC`] error diagnostic when
    /// the invariant is violated; `None` otherwise, including for
    /// code-authored clips whose channels all target the animator's own
    /// [`Transform`] (`target_bone: None`).
    pub fn validate(&self) -> Option<Diagnostic> {
        if self.skeleton.is_some() {
            return None;
        }
        let has_bone_target = self
            .channels
            .iter()
            .any(|channel| channel.target_bone.is_some());
        has_bone_target.then(|| {
            Diagnostic::error(
                CLIP_MISSING_SKELETON_DIAGNOSTIC,
                "animation clip has a channel targeting a bone but declares no skeleton reference",
            )
        })
    }
}

/// A single named marker on an [`AnimationClip`] timeline (Phase 59).
///
/// Delivered to the firing entity's script as `on_event(ctx, name)` — see
/// [`AnimationEvents`] for the full dispatch contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimEvent {
    /// The clip-local time in seconds at which this event fires.
    pub time: f32,
    /// The event name delivered to `on_event(ctx, name)`.
    pub name: String,
}

// ---------------------------------------------------------------------------
// Component types
// ---------------------------------------------------------------------------

/// Playback state of an [`Animator`] component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimatorState {
    /// The animator is not advancing.  Call [`Animator::play`] to resume.
    Stopped,
    /// The animator is advancing each fixed tick.
    Playing,
    /// The animator is paused.  Call [`Animator::play`] to resume from the
    /// current position.
    Paused,
}

/// How translation authored on an animation's root track is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMotionMode {
    /// Sample the clip normally and do not extract a gameplay displacement.
    Disabled,
    /// Remove root translation from the pose and expose its per-step delta.
    ExtractedOnly,
    /// Remove root translation and submit the delta to the character motor.
    AppliedToMotor,
}

/// Per-character displacement produced by animation and consumed by the
/// kinematic controller in the same fixed step.
#[derive(Debug, Clone, Copy, Default)]
pub struct RootMotionRequest {
    /// World-space displacement waiting to be collision-resolved.
    pub delta: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimatorDiscontinuity {
    Reposition,
    Seek,
}

/// An ECS component that plays an [`AnimationClip`] on an entity's
/// [`Transform`].
pub struct Animator {
    /// The clip to play.
    pub clip: Handle<AnimationClip>,
    /// Current playback state.
    pub state: AnimatorState,
    /// Current playback position in seconds.
    pub time: f32,
    /// Whether the clip restarts when it reaches [`AnimationClip::duration`].
    pub looping: bool,
    /// Playback multiplier. Zero holds the pose; negative or non-finite
    /// values are rejected by [`Animator::set_playback_speed`].
    pub playback_speed: f32,
    /// Entity-local markers layered over events embedded in the clip asset.
    pub events: Vec<AnimEvent>,
    /// Markers restricted to one resolved runtime clip.
    clip_events: HashMap<crate::asset::RuntimeAssetId, Vec<AnimEvent>>,
    /// Event emitted once when a non-looping playback reaches its end.
    pub completion_event: Option<String>,
    /// Root translation policy used by [`animation_system`].
    pub root_motion_mode: RootMotionMode,
    root_motion_delta: Vec3,
    /// Active crossfade, if any (Phase 59). See [`Animator::crossfade_to`].
    fade: Option<CrossfadeState>,
    /// Pending continuity break. A seek is kept distinct because stateful
    /// secondary motion can reconstruct its history, while a loop wrap or
    /// instant state switch has no single historical path to reconstruct.
    discontinuity: Option<AnimatorDiscontinuity>,
}

/// The clip being faded out of, tracked while [`Animator::fade`] is active.
///
/// Sampled and advanced by [`animation_system`] alongside the target clip
/// (`Animator::clip`/`Animator::time`) so a fade source in motion (e.g. a
/// walk cycle) does not freeze its feet mid-fade.
struct CrossfadeState {
    /// The clip being faded out of.
    clip: Handle<AnimationClip>,
    /// The fade source's own playback position, advanced independently of
    /// the target clip's `Animator::time`.
    time: f32,
    /// Whether the fade source loops, captured from `Animator::looping` at
    /// the moment the fade started.
    looping: bool,
    /// Seconds elapsed since [`Animator::crossfade_to`] was called.
    elapsed: f32,
    /// Total fade duration in seconds, as passed to [`Animator::crossfade_to`].
    duration: f32,
}

impl Animator {
    /// Creates an [`Animator`] in the [`AnimatorState::Playing`] state.
    pub fn playing(clip: Handle<AnimationClip>) -> Self {
        Self {
            clip,
            state: AnimatorState::Playing,
            time: 0.0,
            looping: false,
            playback_speed: 1.0,
            events: Vec::new(),
            clip_events: HashMap::new(),
            completion_event: Some("animation.completed".to_owned()),
            root_motion_mode: RootMotionMode::Disabled,
            root_motion_delta: Vec3::ZERO,
            fade: None,
            discontinuity: None,
        }
    }

    /// Jumps playback to `seconds`, declaring a seek discontinuity rather
    /// than motion.
    ///
    /// A seek moves the clock, not the character: the pose it lands on is not
    /// something the character travelled to, so simulation following this rig
    /// must rebuild its state rather than derive a velocity from the jump.
    /// Unlike other discontinuities, a seek also identifies a historical clip
    /// path that secondary motion may pre-roll. Writing [`Self::time`] directly
    /// skips that declaration, which is correct only when the caller is
    /// scrubbing a pose nothing simulates.
    pub fn seek(&mut self, seconds: f32) {
        if !seconds.is_finite() {
            return;
        }
        self.time = seconds.max(0.0);
        if self.discontinuity != Some(AnimatorDiscontinuity::Reposition) {
            self.discontinuity = Some(AnimatorDiscontinuity::Seek);
        }
    }

    /// Takes the pending discontinuity, if any, leaving the animator
    /// continuous.
    ///
    /// Consumed by [`animation_system`] once per step so the declaration
    /// reaches [`crate::rig_pose::RigPose`] exactly once, even when several
    /// playback changes happen between two fixed steps.
    fn take_discontinuity(&mut self) -> Option<AnimatorDiscontinuity> {
        self.discontinuity.take()
    }

    /// Resumes playback.
    pub fn play(&mut self) {
        self.state = AnimatorState::Playing;
    }

    /// Stops playback and resets the position to zero.
    pub fn stop(&mut self) {
        self.state = AnimatorState::Stopped;
        self.time = 0.0;
        self.root_motion_delta = Vec3::ZERO;
        self.fade = None;
    }

    /// Pauses playback without resetting the position.
    pub fn pause(&mut self) {
        self.state = AnimatorState::Paused;
    }

    /// Sets whether the clip loops at the end.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Sets a finite, non-negative playback multiplier.
    ///
    /// Returns `false` and preserves the previous value for invalid input.
    pub fn set_playback_speed(&mut self, speed: f32) -> bool {
        if !speed.is_finite() || speed < 0.0 {
            return false;
        }
        self.playback_speed = speed;
        true
    }

    /// Replaces markers that fire only while `clip` is the active target clip.
    ///
    /// The runtime handle is session-local and is never serialized. Authoring
    /// conversion resolves persisted clip names before calling this method.
    pub fn set_clip_events(&mut self, clip: Handle<AnimationClip>, events: Vec<AnimEvent>) {
        self.clip_events.insert(clip.id(), events);
    }

    /// Returns markers registered specifically for `clip`.
    pub fn clip_events(&self, clip: Handle<AnimationClip>) -> &[AnimEvent] {
        self.clip_events
            .get(&clip.id())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Repoints all runtime-only state from one resolved clip handle to
    /// another while preserving playback time and authored event markers.
    ///
    /// Scene conversion calls this when an imported clip is replaced by a
    /// target-skeleton-specific retarget bake. Handles are never persisted,
    /// so this operation does not modify authoring data.
    pub fn replace_clip_handle(&mut self, old: Handle<AnimationClip>, new: Handle<AnimationClip>) {
        if self.clip == old {
            self.clip = new;
        }
        if let Some(events) = self.clip_events.remove(&old.id()) {
            self.clip_events.insert(new.id(), events);
        }
        if let Some(fade) = &mut self.fade
            && fade.clip == old {
                fade.clip = new;
            }
    }

    /// Returns the root displacement extracted during the latest fixed step.
    pub fn root_motion_delta(&self) -> Vec3 {
        self.root_motion_delta
    }

    /// Starts playing `clip` from time zero, crossfading out of the
    /// currently playing clip over `duration` seconds (Phase 59).
    ///
    /// The clip and time this [`Animator`] was playing at the moment of this
    /// call become the fade source; [`animation_system`] samples both clips
    /// each tick and blends them (see its rustdoc for the exact rule) until
    /// `duration` seconds have elapsed, then drops the fade source.
    ///
    /// A non-finite or non-positive `duration` switches instantly: no fade
    /// source is recorded, any fade already in progress is discarded, and
    /// this call behaves like directly overwriting `clip`/`time`/`state`.
    ///
    /// Calling this again while a fade is already active does **not** stack
    /// or preserve the in-progress blend as a frozen pose. It simply treats
    /// the clip and time this [`Animator`] was about to play (the previous
    /// call's target) as the new fade source and discards the older fade
    /// source outright. This is a deliberate v1 simplification: a rapid
    /// series of re-crossfades degrades to always fading from the
    /// most-recently-requested clip, not a smooth chain of blends.
    pub fn crossfade_to(&mut self, clip: Handle<AnimationClip>, duration: f32) {
        if !duration.is_finite() || duration <= 0.0 {
            self.clip = clip;
            self.time = 0.0;
            self.state = AnimatorState::Playing;
            self.fade = None;
            // An instant switch replaces the pose outright, unlike the faded
            // path below, whose blend is continuous by construction. There is
            // no previous interval of the target clip to reconstruct.
            self.discontinuity = Some(AnimatorDiscontinuity::Reposition);
            return;
        }

        self.fade = Some(CrossfadeState {
            clip: self.clip,
            time: self.time,
            looping: self.looping,
            elapsed: 0.0,
            duration,
        });
        self.clip = clip;
        self.time = 0.0;
        self.state = AnimatorState::Playing;
    }

    /// Returns `true` while a crossfade started by [`Animator::crossfade_to`]
    /// is still blending toward the target clip.
    pub fn is_fading(&self) -> bool {
        self.fade.is_some()
    }

    /// Returns normalized crossfade progress while a fade is active.
    pub fn crossfade_progress(&self) -> Option<f32> {
        self.fade
            .as_ref()
            .map(|fade| (fade.elapsed / fade.duration).clamp(0.0, 1.0))
    }

    /// Returns the fade-source clip and its own (independently advancing)
    /// playback time while a crossfade started by [`Animator::crossfade_to`]
    /// is in progress, or `None` when no fade is active.
    ///
    /// A read-only accessor over the private fade-source state, added for
    /// [`crate::foot_ik::foot_ik_system`] (ADR 0080 §2), which needs to know
    /// which *second* clip is contributing to the blended pose so it can
    /// weight ground-contact intervals from both the target and fade-source
    /// clips. Pairs with [`Animator::crossfade_progress`] for the blend
    /// weight: that weight applies to the *target* clip
    /// ([`Animator::clip`] / [`Animator::time`]); `1.0 - weight` applies to
    /// this fade source. See [`animation_system`]'s crossfade rustdoc for the
    /// full composition rule this mirrors.
    pub(crate) fn fade_source(&self) -> Option<(Handle<AnimationClip>, f32)> {
        self.fade.as_ref().map(|fade| (fade.clip, fade.time))
    }
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

/// Returns the interpolated value for `channel` at time `t`, or `None` when
/// the channel has no keyframes.
pub fn lerp_channel(channel: &AnimChannel, t: f32) -> Option<[f32; 4]> {
    let kfs = &channel.keyframes;
    if kfs.is_empty() {
        return None;
    }
    if kfs.len() == 1 {
        return Some(kfs[0].value);
    }
    let last = kfs.last().unwrap();
    if t >= last.time {
        return Some(last.value);
    }
    let first = &kfs[0];
    if t <= first.time {
        return Some(first.value);
    }

    let idx = kfs.partition_point(|k| k.time <= t);
    let kf_a = &kfs[idx - 1];
    let kf_b = &kfs[idx];
    let span = kf_b.time - kf_a.time;
    let factor = if span > f32::EPSILON {
        (t - kf_a.time) / span
    } else {
        0.0
    };

    let a = kf_a.value;
    let b = kf_b.value;
    Some(match channel.property {
        AnimProperty::Rotation => Quat::from_array(a)
            .slerp(Quat::from_array(b), factor)
            .to_array(),
        AnimProperty::Translation | AnimProperty::Scale => [
            a[0] + (b[0] - a[0]) * factor,
            a[1] + (b[1] - a[1]) * factor,
            a[2] + (b[2] - a[2]) * factor,
            1.0,
        ],
    })
}

/// Returns the linearly interpolated scalar value for a morph channel.
pub(crate) fn lerp_morph_channel(channel: &MorphChannel, t: f32) -> Option<f32> {
    let keyframes = &channel.keyframes;
    if keyframes.is_empty() {
        return None;
    }
    if keyframes.len() == 1 || t <= keyframes[0].time {
        return Some(keyframes[0].value[0]);
    }
    let last = keyframes.last()?;
    if t >= last.time {
        return Some(last.value[0]);
    }

    let index = keyframes.partition_point(|keyframe| keyframe.time <= t);
    let left = &keyframes[index - 1];
    let right = &keyframes[index];
    let span = right.time - left.time;
    let factor = if span > f32::EPSILON {
        (t - left.time) / span
    } else {
        0.0
    };
    Some(left.value[0] + (right.value[0] - left.value[0]) * factor)
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One firing of an [`AnimEvent`], recorded for a specific entity (Phase 59).
#[derive(Debug, Clone)]
pub struct AnimationEventRecord {
    /// The entity whose [`Animator`] fired the event.
    pub entity: engine_ecs::Entity,
    /// The event name, copied from [`AnimEvent::name`].
    pub name: String,
    /// Target-clip time after the step that emitted this event.
    pub clip_time: f32,
}

/// ECS resource holding every animation event fired during the last
/// fixed-update [`animation_system`] pass (Phase 59).
///
/// Lifecycle mirrors [`crate::collision::CollisionEvents`]: cleared at the
/// start of each `animation_system` pass, then filled with that pass's
/// firings, so events are only valid within the fixed-update step they were
/// generated in.
///
/// This resource is optional. `animation_system` accepts it as
/// `Option<ResMut<AnimationEvents>>` — crossfade and sampling both work
/// identically whether or not it is present; a game that never inserts it
/// simply never records events. Insert it before registering
/// `animation_system` to enable event delivery.
///
/// Delivery differs from `UiEvents`' broadcast model: an event only reaches
/// the *firing* entity's own script (see
/// [`crate::scripting::scripting_update_system`]'s rustdoc), never every
/// enabled script in the scene. For same-tick delivery, register
/// `animation_system` before `scripting_update_system`.
#[derive(Default)]
pub struct AnimationEvents {
    events: Vec<AnimationEventRecord>,
    generation: u64,
}

impl AnimationEvents {
    /// Iterates every animation event fired during the last pass.
    pub fn iter(&self) -> impl Iterator<Item = &AnimationEventRecord> {
        self.events.iter()
    }

    /// Returns the animation-system update generation.
    ///
    /// Consumers use this instead of the fixed-step clock so a system ordered
    /// before animation sampling cannot accidentally mark stale events as
    /// already observed for the new step.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn clear(&mut self) {
        self.events.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    fn push(&mut self, record: AnimationEventRecord) {
        self.events.push(record);
    }
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// Advances all active [`Animator`] components by one fixed timestep and
/// applies the interpolated transform values.
///
/// Channels with [`AnimChannel::target_bone`] set drive the matching joint of
/// the [`Skeleton`] on the animator's own entity, resolved through
/// [`Skeleton::bone_ids`] (ADR 0077); channels without a target drive the
/// entity's own [`Transform`]. A [`BoneId`] absent from the skeleton, a
/// missing skeleton, and a resolved joint without a [`Transform`] are all
/// skipped instead of panicking.
///
/// # Crossfade composition (Phase 59)
///
/// While [`Animator::is_fading`] is true, both the target clip
/// (`Animator::clip`/`Animator::time`) and the fade source clip are advanced
/// and sampled every tick — the source keeps playing so a fade out of a
/// moving cycle (e.g. walk → run) never freezes. The two samples are
/// combined per `(target entity, property)`: when **both** clips drive that
/// property, the values are linearly blended by `weight = elapsed / duration`
/// (clamped to `[0, 1]`, `0` = fully the source clip, `1` = fully the target
/// clip); rotations use a normalized lerp with sign correction (the source
/// quaternion is negated first when its dot product with the target is
/// negative, so the blend always takes the shorter path). When **only one**
/// clip drives a property, that value is applied directly, unweighted — a
/// deliberate v1 simplification (see the phase 59 design doc). This
/// composition layer sits above [`lerp_channel`], which is unchanged.
///
/// # Events (Phase 59)
///
/// When an [`AnimationEvents`] resource is present, it is cleared at the
/// start of this call, then filled with every event on the *target* clip
/// (never the fade source) whose `time` falls in `(previous_time, new_time]`
/// this tick — or, on a looping wrap, `(previous_time, duration]` followed by
/// `(0, new_time]` (see `fire_events` for why those two intervals are kept
/// separate rather than merged into one global sort). Multiple events firing
/// in the same tick are recorded in ascending `time` order within each
/// interval. A large `dt` that steps entirely over an event's `time` still
/// fires it, since this is an interval check, not a sample check. Animators
/// that are [`AnimatorState::Stopped`] or [`AnimatorState::Paused`] fire
/// nothing.
///
/// Register with `app.add_fixed_system(animation_system)` to run on the
/// fixed-update schedule.
pub fn animation_system(
    time: engine_ecs::Res<FixedTime>,
    clips: engine_ecs::Res<Assets<AnimationClip>>,
    mut events: Option<engine_ecs::ResMut<AnimationEvents>>,
    mut arena: Option<engine_ecs::ResMut<PoseArena>>,
    mut animators: engine_ecs::Query<(&mut Animator, Option<&Skeleton>, Option<&mut RigPose>)>,
    mut transforms: engine_ecs::Query<&mut Transform>,
    mut morph_renderers: engine_ecs::Query<(&SkinnedMesh, &MorphTargets, &mut MorphWeights)>,
) {
    if let Some(events) = events.as_deref_mut() {
        events.clear();
    }
    // A host that never inserted the pool still animates; it just allocates
    // its scratch poses per frame instead of recycling them.
    let mut fallback_arena = PoseArena::new();
    let arena = arena.as_deref_mut().unwrap_or(&mut fallback_arena);

    let dt = time.fixed_delta;
    let mut poses: HashMap<engine_ecs::Entity, PoseWrite> = HashMap::new();
    let mut rig_morph_weights = HashMap::<engine_ecs::Entity, HashMap<String, f32>>::new();

    for (entity, (animator, skeleton, rig_pose)) in animators.iter_mut() {
        animator.root_motion_delta = Vec3::ZERO;
        if animator.state != AnimatorState::Playing {
            continue;
        }
        let handle = animator.clip;
        let Some(clip) = clips.get(&handle) else {
            continue;
        };

        let prev_time = animator.time;
        let scaled_dt = dt * animator.playback_speed;
        let (new_time, wrapped) =
            advance_time(prev_time, scaled_dt, clip.duration, animator.looping);
        animator.time = new_time;
        let completed = !animator.looping && prev_time < clip.duration && new_time >= clip.duration;
        if !animator.looping && new_time >= clip.duration {
            animator.state = AnimatorState::Stopped;
        }

        if let Some(events) = events.as_deref_mut() {
            fire_events(
                events,
                entity,
                &clip.events,
                prev_time,
                new_time,
                clip.duration,
                wrapped,
            );
            fire_events(
                events,
                entity,
                &animator.events,
                prev_time,
                new_time,
                clip.duration,
                wrapped,
            );
            fire_events(
                events,
                entity,
                animator.clip_events(handle),
                prev_time,
                new_time,
                clip.duration,
                wrapped,
            );
            if completed
                && let Some(name) = animator
                    .completion_event
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                {
                    events.push(AnimationEventRecord {
                        entity,
                        name: name.to_owned(),
                        clip_time: new_time,
                    });
                }
        }

        let bone_index = bone_index_map(skeleton);
        let joint_count = skeleton.map_or(0, |skeleton| skeleton.bone_ids.len());
        let mut current_pose = arena.acquire(joint_count);
        current_pose.sample_into(clip, new_time, &bone_index);
        if animator.root_motion_mode != RootMotionMode::Disabled
            && let Some((target, start, delta)) = extract_root_motion(
                clip,
                prev_time,
                new_time,
                wrapped,
                entity,
                skeleton,
                &bone_index,
            ) {
                animator.root_motion_delta = delta;
                // The root stays at its authored first-frame position; only
                // the extracted delta may move the character body.
                if target == entity {
                    current_pose.entity.translation = Some(start);
                } else if let Some(index) = skeleton.and_then(|skeleton| {
                    skeleton.joints.iter().position(|joint| *joint == target)
                }) {
                    current_pose.joints.write_translation(index, start);
                }
            }

        let target_pose = match &mut animator.fade {
            Some(fade) => {
                let source_clip = clips.get(&fade.clip);
                let source_duration = source_clip.map(|c| c.duration).unwrap_or(0.0);
                let (source_time, _) =
                    advance_time(fade.time, scaled_dt, source_duration, fade.looping);
                fade.time = source_time;
                fade.elapsed += scaled_dt;

                let weight = if fade.duration > f32::EPSILON {
                    (fade.elapsed / fade.duration).clamp(0.0, 1.0)
                } else {
                    1.0
                };

                let blended_pose = match source_clip {
                    Some(source_clip) => {
                        let mut source_pose = arena.acquire(joint_count);
                        source_pose.sample_into(source_clip, source_time, &bone_index);
                        let mut blended = arena.acquire(joint_count);
                        blended.blend_into(&source_pose, &current_pose, weight, None);
                        arena.release(source_pose);
                        arena.release(current_pose);
                        blended
                    }
                    None => current_pose,
                };

                if fade.elapsed >= fade.duration {
                    animator.fade = None;
                }
                blended_pose
            }
            None => current_pose,
        };

        // A normal loop wrap has no interval before the first sample to
        // reconstruct, so it remains a plain reseat. An explicit seek keeps
        // its reason even when `advance_time` normalizes a looping target:
        // normal playback also reseats at the wrap, so the post-wrap history
        // from clip start to that local target is well-defined.
        let discontinuity = match animator.take_discontinuity() {
            Some(AnimatorDiscontinuity::Seek) => Some(AnimatorDiscontinuity::Seek),
            Some(AnimatorDiscontinuity::Reposition) => Some(AnimatorDiscontinuity::Reposition),
            None if wrapped => Some(AnimatorDiscontinuity::Reposition),
            None => None,
        };

        let mut target_pose = target_pose;
        if let Some(rig_pose) = rig_pose {
            // A sampled layer sized for a different bone count belongs to
            // another rig, so adopting it would leave composition and the
            // joint entities disagreeing about which slot drives which bone.
            // Exchanging rather than moving returns the layer's previous
            // storage to the pool below.
            rig_pose
                .animation_layer_mut()
                .swap_values(&mut target_pose.joints);
            match discontinuity {
                Some(AnimatorDiscontinuity::Reposition) => rig_pose.mark_discontinuous(),
                Some(AnimatorDiscontinuity::Seek) => rig_pose.mark_animation_seek(),
                None => {}
            }
        }
        merge_pose(
            &mut poses,
            entity,
            PoseWrite {
                translation: target_pose.entity.translation,
                rotation: target_pose.entity.rotation,
                scale: target_pose.entity.scale,
            },
        );
        rig_morph_weights.insert(entity, std::mem::take(&mut target_pose.morph_weights));
        arena.release(target_pose);
    }

    for (entity, transform) in transforms.iter_mut() {
        let Some(pose) = poses.get(&entity) else {
            continue;
        };
        if let Some(translation) = pose.translation {
            transform.translation = translation;
        }
        if let Some(rotation) = pose.rotation {
            transform.rotation = rotation;
        }
        if let Some(scale) = pose.scale {
            transform.scale = scale;
        }
    }

    for (_, (skinned_mesh, targets, weights)) in morph_renderers.iter_mut() {
        let Some(sampled) = rig_morph_weights.get(&skinned_mesh.rig) else {
            continue;
        };
        weights.clear();
        for (name, weight) in sampled {
            if let Some(index) = targets.index_of(name) {
                weights.set(index, *weight);
            }
        }
    }
}

/// Advances a playback position by `dt`, applying the same looping/clamping
/// rule [`animation_system`] has always used for `Animator::time`.
///
/// Returns the new time and whether a looping wrap occurred (`raw >=
/// duration` while looping). A non-looping clip clamps to `duration` instead
/// of overshooting it.
fn advance_time(prev: f32, dt: f32, duration: f32, looping: bool) -> (f32, bool) {
    let raw = prev + dt;
    if looping && duration > f32::EPSILON {
        (raw % duration, raw >= duration)
    } else if raw >= duration {
        (duration, false)
    } else {
        (raw, false)
    }
}

/// Fires every event in `clip_events` crossed while advancing from
/// `prev_time` to `new_time`, in ascending `time` order.
///
/// On a looping wrap, the two crossed intervals — `(prev_time, duration]`
/// (the tail of the lap being left) and `(0, new_time]` (the head of the new
/// lap) — are each processed in ascending `time` order, and the tail's
/// events are pushed before the head's. They are **not** merged into one
/// global ascending sort: a global sort would put a just-after-wrap event
/// (small `time`) before a just-before-wrap event (`time` near `duration`),
/// which is backwards from playback order.
///
/// See [`animation_system`]'s "Events" rustdoc section for the crossing rule.
fn fire_events(
    events: &mut AnimationEvents,
    entity: engine_ecs::Entity,
    clip_events: &[AnimEvent],
    prev_time: f32,
    new_time: f32,
    duration: f32,
    wrapped: bool,
) {
    if wrapped {
        push_events_in_range(events, entity, clip_events, prev_time, duration, new_time);
        push_events_in_range(events, entity, clip_events, 0.0, new_time, new_time);
    } else {
        push_events_in_range(events, entity, clip_events, prev_time, new_time, new_time);
    }
}

/// Pushes every event in `clip_events` whose `time` falls in
/// `(from_exclusive, to_inclusive]`, in ascending `time` order.
fn push_events_in_range(
    events: &mut AnimationEvents,
    entity: engine_ecs::Entity,
    clip_events: &[AnimEvent],
    from_exclusive: f32,
    to_inclusive: f32,
    clip_time: f32,
) {
    let mut matched: Vec<&AnimEvent> = clip_events
        .iter()
        .filter(|e| e.time > from_exclusive && e.time <= to_inclusive)
        .collect();
    matched.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for event in matched {
        events.push(AnimationEventRecord {
            entity,
            name: event.name.clone(),
            clip_time,
        });
    }
}

/// Builds a `BoneId -> joint index` lookup from `skeleton.bone_ids`, mirroring
/// the per-skeleton map [`crate::skinning::joint_palette_system`] builds for
/// joint world matrices (ADR 0077). Rebuilt once per animator per tick;
/// skeletons are small (at most [`crate::skinning::MAX_JOINTS`]) so this
/// stays cheap.
fn bone_index_map(skeleton: Option<&Skeleton>) -> HashMap<BoneId, usize> {
    let Some(skeleton) = skeleton else {
        return HashMap::new();
    };
    skeleton
        .bone_ids
        .iter()
        .enumerate()
        .map(|(index, &bone_id)| (bone_id, index))
        .collect()
}

/// Resolves `target_bone` to a joint entity through `bone_index` and
/// `skeleton.joints`, or `None` when the bone is not present in the skeleton
/// (ADR 0077).
fn resolve_bone_target(
    target_bone: BoneId,
    skeleton: Option<&Skeleton>,
    bone_index: &HashMap<BoneId, usize>,
) -> Option<engine_ecs::Entity> {
    let index = *bone_index.get(&target_bone)?;
    skeleton?.joints.get(index).copied()
}

/// Finds the canonical root translation track and returns its first-frame
/// position plus the displacement crossed during this step.
///
/// The channel targeting [`AnimationClip::root_bone`] is preferred for
/// skinned clips (ADR 0077); an entity-level translation track
/// (`target_bone: None`) is the fallback used by simple transform animation.
fn extract_root_motion(
    clip: &AnimationClip,
    previous_time: f32,
    new_time: f32,
    wrapped: bool,
    entity: engine_ecs::Entity,
    skeleton: Option<&Skeleton>,
    bone_index: &HashMap<BoneId, usize>,
) -> Option<(engine_ecs::Entity, Vec3, Vec3)> {
    let channel = clip
        .channels
        .iter()
        .find(|channel| {
            channel.property == AnimProperty::Translation
                && clip.root_bone.is_some()
                && channel.target_bone == clip.root_bone
        })
        .or_else(|| {
            clip.channels.iter().find(|channel| {
                channel.property == AnimProperty::Translation && channel.target_bone.is_none()
            })
        })?;
    let target = channel
        .target_bone
        .and_then(|bone_id| resolve_bone_target(bone_id, skeleton, bone_index))
        .unwrap_or(entity);
    let sample =
        |time| lerp_channel(channel, time).map(|value| Vec3::new(value[0], value[1], value[2]));
    let start = sample(0.0)?;
    let previous = sample(previous_time)?;
    let current = sample(new_time)?;
    let delta = if wrapped {
        let end = sample(clip.duration)?;
        (end - previous) + (current - start)
    } else {
        current - previous
    };
    Some((target, start, delta))
}

/// Converts local extracted motion into a world-space request that the
/// character controller consumes later in the same fixed step.
pub fn root_motion_motor_system(
    mut query: engine_ecs::Query<(&Animator, &Transform, &mut RootMotionRequest)>,
) {
    for (_, (animator, transform, request)) in query.iter_mut() {
        request.delta = if animator.root_motion_mode == RootMotionMode::AppliedToMotor {
            transform.rotation * animator.root_motion_delta
        } else {
            Vec3::ZERO
        };
    }
}

/// Overwrites `poses[target]`'s populated fields from `pose`, matching the
/// last-writer-wins behavior [`animation_system`] has always applied when
/// multiple animators or channels drive the same entity.
fn merge_pose(
    poses: &mut HashMap<engine_ecs::Entity, PoseWrite>,
    target: engine_ecs::Entity,
    pose: PoseWrite,
) {
    let entry = poses.entry(target).or_default();
    if let Some(translation) = pose.translation {
        entry.translation = Some(translation);
    }
    if let Some(rotation) = pose.rotation {
        entry.rotation = Some(rotation);
    }
    if let Some(scale) = pose.scale {
        entry.scale = Some(scale);
    }
}

/// Pending transform writes produced while evaluating animator channels.
#[derive(Default)]
struct PoseWrite {
    translation: Option<Vec3>,
    rotation: Option<Quat>,
    scale: Option<Vec3>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::Assets;
    use crate::pose_graph::{EntityPose, PoseGraphOutput};
    use crate::rig_pose::{PoseBlend, PoseLayer};

    /// A skeleton-free pose graph output that only drives the animator
    /// entity's own rotation.
    fn entity_rotation_pose(rotation: Quat) -> PoseGraphOutput {
        PoseGraphOutput {
            joints: PoseLayer::new(0, PoseBlend::Replace),
            entity: EntityPose {
                rotation: Some(rotation),
                ..EntityPose::default()
            },
            morph_weights: HashMap::new(),
        }
    }

    /// A skeleton-free pose graph output carrying exactly one morph weight.
    fn entity_morph_pose(name: &str, weight: f32) -> PoseGraphOutput {
        PoseGraphOutput {
            joints: PoseLayer::new(0, PoseBlend::Replace),
            entity: EntityPose::default(),
            morph_weights: HashMap::from([(name.to_owned(), weight)]),
        }
    }

    fn translation_channel(pairs: &[(f32, f32)]) -> AnimChannel {
        AnimChannel {
            property: AnimProperty::Translation,
            target_bone: None,
            keyframes: pairs
                .iter()
                .map(|&(t, x)| Keyframe {
                    time: t,
                    value: [x, 0.0, 0.0, 1.0],
                })
                .collect(),
        }
    }

    #[test]
    fn lerp_channel_empty_returns_none() {
        let ch = AnimChannel {
            property: AnimProperty::Translation,
            target_bone: None,
            keyframes: vec![],
        };
        assert!(lerp_channel(&ch, 0.5).is_none());
    }

    #[test]
    fn lerp_channel_single_keyframe_returns_value() {
        let ch = translation_channel(&[(0.0, 3.0)]);
        let v = lerp_channel(&ch, 999.0).unwrap();
        assert!((v[0] - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lerp_channel_midpoint() {
        let ch = translation_channel(&[(0.0, 0.0), (2.0, 4.0)]);
        let v = lerp_channel(&ch, 1.0).unwrap();
        assert!(
            (v[0] - 2.0).abs() < 1e-5,
            "expected midpoint x=2.0, got {}",
            v[0]
        );
    }

    #[test]
    fn lerp_channel_clamps_before_first() {
        let ch = translation_channel(&[(1.0, 5.0), (2.0, 10.0)]);
        let v = lerp_channel(&ch, 0.0).unwrap();
        assert!(
            (v[0] - 5.0).abs() < f32::EPSILON,
            "must clamp to first keyframe"
        );
    }

    #[test]
    fn lerp_channel_clamps_past_last() {
        let ch = translation_channel(&[(0.0, 0.0), (1.0, 8.0)]);
        let v = lerp_channel(&ch, 100.0).unwrap();
        assert!(
            (v[0] - 8.0).abs() < f32::EPSILON,
            "must clamp to last keyframe"
        );
    }

    #[test]
    fn animation_system_drives_skeleton_joints_by_bone_id() {
        use crate::rig_pose::publish_final_rig_pose_system;
        use crate::skeleton_asset::{compute_skeleton_identity, BoneDef, BoneId, SkeletonAsset};
        use crate::skinning::Skeleton;
        use engine_ecs::World;

        let mut world = World::new();
        let joint = world
            .spawn_with(Transform::default())
            .expect("joint must spawn");

        let mut clips = Assets::<AnimationClip>::default();
        let clip = clips.add(AnimationClip {
            duration: 1.0,
            channels: vec![
                AnimChannel {
                    property: AnimProperty::Translation,
                    target_bone: Some(BoneId(7)),
                    keyframes: vec![
                        Keyframe {
                            time: 0.0,
                            value: [0.0, 0.0, 0.0, 1.0],
                        },
                        Keyframe {
                            time: 1.0,
                            value: [4.0, 0.0, 0.0, 1.0],
                        },
                    ],
                },
                // A BoneId absent from Skeleton::bone_ids is skipped, not
                // panicked on.
                AnimChannel {
                    property: AnimProperty::Scale,
                    target_bone: Some(BoneId(9999)),
                    keyframes: vec![Keyframe {
                        time: 0.0,
                        value: [3.0, 3.0, 3.0, 1.0],
                    }],
                },
            ],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });

        let animator_entity = world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");
        let bones = vec![BoneDef {
            id: BoneId(7),
            name: "joint".to_owned(),
            parent: None,
            rest_translation: Vec3::ZERO,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        }];
        let skeleton_asset = SkeletonAsset {
            id: AssetId::generate(),
            name: "animation_test".to_owned(),
            identity: compute_skeleton_identity(&bones),
            next_bone_id: 8,
            bones,
        };
        world
            .add_component(
                animator_entity,
                Skeleton {
                    joints: vec![joint],
                    // BoneId(7) is joint index 0; BoneId(9999) is deliberately
                    // absent so the mismatched channel above is exercised.
                    bone_ids: vec![BoneId(7)],
                    asset: None,
                },
            )
            .expect("animator must accept Skeleton");
        world
            .add_component(animator_entity, RigPose::from_skeleton(&skeleton_asset))
            .expect("animator must accept RigPose");

        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), &mut world);
        app.insert_resource(FixedTime::with_delta(0.5));
        app.insert_resource(clips);
        app.add_system(animation_system);
        app.add_system(publish_final_rig_pose_system);
        app.update().expect("animation schedule must run");
        std::mem::swap(app.world_mut(), &mut world);

        let joint_transform = world
            .get_component::<Transform>(joint)
            .expect("joint must keep Transform");
        assert!(
            (joint_transform.translation.x - 2.0).abs() < 1.0e-5,
            "joint must follow the clip at t=0.5, got {}",
            joint_transform.translation.x
        );
        assert_eq!(joint_transform.scale, Vec3::ONE);
    }

    #[test]
    fn lerp_channel_rotation_is_normalized() {
        let ch = AnimChannel {
            property: AnimProperty::Rotation,
            target_bone: None,
            keyframes: vec![
                Keyframe {
                    time: 0.0,
                    value: [0.0, 0.0, 0.0, 1.0],
                },
                Keyframe {
                    time: 1.0,
                    value: [0.0, 1.0, 0.0, 0.0],
                },
            ],
        };
        let v = lerp_channel(&ch, 0.5).unwrap();
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
        assert!(
            (len - 1.0).abs() < 1e-5,
            "interpolated quaternion must be unit length"
        );
    }

    #[test]
    fn animator_state_transitions() {
        let mut store = Assets::<AnimationClip>::new();
        let handle = store.add(AnimationClip {
            duration: 1.0,
            channels: vec![],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let mut a = Animator::playing(handle);
        assert_eq!(a.state, AnimatorState::Playing);
        a.pause();
        assert_eq!(a.state, AnimatorState::Paused);
        a.play();
        assert_eq!(a.state, AnimatorState::Playing);
        a.stop();
        assert_eq!(a.state, AnimatorState::Stopped);
        assert!(a.time.abs() < f32::EPSILON, "stop must reset time to zero");
    }

    #[test]
    fn animator_set_looping() {
        let mut store = Assets::<AnimationClip>::new();
        let handle = store.add(AnimationClip {
            duration: 1.0,
            channels: vec![],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let mut a = Animator::playing(handle);
        assert!(!a.looping);
        a.set_looping(true);
        assert!(a.looping);
    }

    #[test]
    fn seek_is_distinct_from_instant_switch_and_crossfade() {
        let mut clips = Assets::<AnimationClip>::new();
        let source = clips.add(clip(1.0, Vec::new()));
        let target = clips.add(clip(1.0, Vec::new()));
        let mut animator = Animator::playing(source);

        animator.seek(0.5);
        assert_eq!(
            animator.take_discontinuity(),
            Some(AnimatorDiscontinuity::Seek)
        );

        animator.crossfade_to(target, 0.0);
        assert_eq!(
            animator.take_discontinuity(),
            Some(AnimatorDiscontinuity::Reposition)
        );

        let mut reposition_then_seek = Animator::playing(source);
        reposition_then_seek.crossfade_to(target, 0.0);
        reposition_then_seek.seek(0.75);
        assert_eq!(
            reposition_then_seek.take_discontinuity(),
            Some(AnimatorDiscontinuity::Reposition),
            "a pending hard reposition must take precedence over a later seek"
        );

        animator.crossfade_to(source, 0.25);
        assert_eq!(
            animator.take_discontinuity(),
            None,
            "a positive-duration crossfade is continuous"
        );
    }

    #[test]
    fn stop_discards_crossfade_and_pending_root_motion() {
        let mut clips = Assets::<AnimationClip>::new();
        let source = clips.add(clip(1.0, Vec::new()));
        let target = clips.add(clip(1.0, Vec::new()));
        let mut animator = Animator::playing(source);
        animator.root_motion_delta = Vec3::new(1.0, 0.0, 0.0);
        animator.crossfade_to(target, 0.5);

        animator.stop();

        assert_eq!(animator.state, AnimatorState::Stopped);
        assert_eq!(animator.time, 0.0);
        assert_eq!(animator.root_motion_delta(), Vec3::ZERO);
        assert!(!animator.is_fading());
    }

    // --- Crossfade (Phase 59) -------------------------------------------------

    fn scale_channel(value: f32) -> AnimChannel {
        AnimChannel {
            property: AnimProperty::Scale,
            target_bone: None,
            keyframes: vec![Keyframe {
                time: 0.0,
                value: [value, value, value, 1.0],
            }],
        }
    }

    fn clip(duration: f32, channels: Vec<AnimChannel>) -> AnimationClip {
        AnimationClip {
            duration,
            channels,
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        }
    }

    fn run_animation_system(world: &mut engine_ecs::World, dt: f32, clips: Assets<AnimationClip>) {
        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), world);
        app.insert_resource(FixedTime::with_delta(dt));
        app.insert_resource(clips);
        app.add_system(animation_system);
        app.update().expect("animation schedule must run");
        std::mem::swap(app.world_mut(), world);
    }

    #[test]
    fn crossfade_duration_zero_switches_instantly() {
        let mut store = Assets::<AnimationClip>::new();
        let a = store.add(clip(1.0, vec![translation_channel(&[(0.0, 0.0)])]));
        let b = store.add(clip(1.0, vec![translation_channel(&[(0.0, 9.0)])]));

        let mut animator = Animator::playing(a);
        animator.time = 0.4;
        animator.crossfade_to(b, 0.0);

        assert_eq!(animator.clip, b);
        assert!(animator.time.abs() < f32::EPSILON, "time must reset to 0");
        assert_eq!(animator.state, AnimatorState::Playing);
        assert!(
            !animator.is_fading(),
            "a non-positive duration must not record a fade source"
        );
    }

    #[test]
    fn crossfade_mid_fade_blends_translation_by_weight() {
        let mut store = Assets::<AnimationClip>::new();
        // Constant (single-keyframe) clips so the blended value only depends
        // on `weight`, not on sampling position within the clip.
        let source = store.add(clip(10.0, vec![translation_channel(&[(0.0, 0.0)])]));
        let target = store.add(clip(10.0, vec![translation_channel(&[(0.0, 10.0)])]));

        let mut animator = Animator::playing(source);
        animator.crossfade_to(target, 1.0);

        let mut world = engine_ecs::World::new();
        let entity = world
            .spawn_with(animator)
            .expect("animator entity must spawn");
        world
            .add_component(entity, Transform::default())
            .expect("entity must accept Transform");

        // dt = 0.5 of a 1.0s fade => weight = 0.5 => halfway between 0 and 10.
        run_animation_system(&mut world, 0.5, store);

        let transform = world
            .get_component::<Transform>(entity)
            .expect("transform must exist");
        assert!(
            (transform.translation.x - 5.0).abs() < 1e-4,
            "expected the midpoint 5.0, got {}",
            transform.translation.x
        );
    }

    #[test]
    fn crossfade_after_duration_uses_only_the_target_clip() {
        let mut store = Assets::<AnimationClip>::new();
        let source = store.add(clip(10.0, vec![translation_channel(&[(0.0, 0.0)])]));
        let target = store.add(clip(10.0, vec![translation_channel(&[(0.0, 10.0)])]));

        let mut animator = Animator::playing(source);
        animator.crossfade_to(target, 1.0);

        let mut world = engine_ecs::World::new();
        let entity = world
            .spawn_with(animator)
            .expect("animator entity must spawn");
        world
            .add_component(entity, Transform::default())
            .expect("entity must accept Transform");

        // dt overshoots the 1.0s fade duration in a single tick.
        run_animation_system(&mut world, 1.5, store);

        let transform = world
            .get_component::<Transform>(entity)
            .expect("transform must exist");
        assert!(
            (transform.translation.x - 10.0).abs() < 1e-4,
            "expected only the target clip's value 10.0, got {}",
            transform.translation.x
        );
        let animator = world
            .get_component::<Animator>(entity)
            .expect("animator must exist");
        assert!(
            !animator.is_fading(),
            "the fade must be dropped once elapsed reaches duration"
        );
    }

    #[test]
    fn crossfade_property_present_in_only_one_clip_passes_through_unchanged() {
        let mut store = Assets::<AnimationClip>::new();
        // Source drives only scale; target drives only translation.
        let source = store.add(clip(10.0, vec![scale_channel(2.0)]));
        let target = store.add(clip(10.0, vec![translation_channel(&[(0.0, 5.0)])]));

        let mut animator = Animator::playing(source);
        animator.crossfade_to(target, 1.0);

        let mut world = engine_ecs::World::new();
        let entity = world
            .spawn_with(animator)
            .expect("animator entity must spawn");
        world
            .add_component(entity, Transform::default())
            .expect("entity must accept Transform");

        // weight = 0.3, but pass-through properties must be unweighted.
        run_animation_system(&mut world, 0.3, store);

        let transform = world
            .get_component::<Transform>(entity)
            .expect("transform must exist");
        assert!(
            (transform.scale.x - 2.0).abs() < 1e-4,
            "scale-only-in-source must pass through unweighted, got {}",
            transform.scale.x
        );
        assert!(
            (transform.translation.x - 5.0).abs() < 1e-4,
            "translation-only-in-target must pass through unweighted, got {}",
            transform.translation.x
        );
    }

    #[test]
    fn crossfade_source_keeps_advancing_during_the_fade() {
        let mut store = Assets::<AnimationClip>::new();
        // Source ramps 0 -> 10 over 10 seconds; target drives nothing, so the
        // ramp passes through unweighted and its value tracks the source's
        // own advancing time, not the time the fade started.
        let source = store.add(clip(
            10.0,
            vec![translation_channel(&[(0.0, 0.0), (10.0, 10.0)])],
        ));
        let target = store.add(clip(10.0, vec![]));

        let mut animator = Animator::playing(source);
        animator.crossfade_to(target, 100.0);

        let mut world = engine_ecs::World::new();
        let entity = world
            .spawn_with(animator)
            .expect("animator entity must spawn");
        world
            .add_component(entity, Transform::default())
            .expect("entity must accept Transform");

        let mut clips = store;
        {
            let mut app = engine_ecs::App::new();
            std::mem::swap(app.world_mut(), &mut world);
            app.insert_resource(FixedTime::with_delta(1.0));
            app.insert_resource(std::mem::take(&mut clips));
            app.add_system(animation_system);
            app.update().expect("first tick must run");
            app.update().expect("second tick must run");
            clips = app
                .world_mut()
                .remove_resource::<Assets<AnimationClip>>()
                .unwrap_or_default();
            std::mem::swap(app.world_mut(), &mut world);
        }
        let _ = clips;

        let transform = world
            .get_component::<Transform>(entity)
            .expect("transform must exist");
        assert!(
            (transform.translation.x - 2.0).abs() < 1e-4,
            "after two 1.0s ticks the source must have advanced to t=2.0, got {}",
            transform.translation.x
        );
    }

    #[test]
    fn pose_blending_takes_the_short_path_between_equivalent_rotations() {
        let a = Quat::from_xyzw(0.0, 0.0, 0.0, 1.0);
        // Same rotation as `a`, but encoded with the opposite sign, so
        // `a.dot(b) < 0`.
        let b = Quat::from_xyzw(0.0, 0.0, 0.0, -1.0);

        let blended = PoseGraphOutput::blend(&entity_rotation_pose(a), &entity_rotation_pose(b), 0.5)
            .entity
            .rotation
            .expect("both sides drive the entity rotation");

        assert!(
            blended.is_finite(),
            "sign correction must avoid a degenerate zero-length lerp"
        );
        assert!(
            (blended.w.abs() - 1.0).abs() < 1e-4,
            "shortest path between equivalent rotations must stay near identity, got {blended:?}"
        );
    }

    // --- Events (Phase 59) -----------------------------------------------------

    fn clip_with_events(duration: f32, events: Vec<AnimEvent>) -> AnimationClip {
        AnimationClip {
            duration,
            channels: vec![],
            morph_channels: Vec::new(),
            events,
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        }
    }

    fn event(time: f32, name: &str) -> AnimEvent {
        AnimEvent {
            time,
            name: name.to_string(),
        }
    }

    fn run_with_events(
        world: &mut engine_ecs::World,
        dt: f32,
        clips: Assets<AnimationClip>,
    ) -> Vec<AnimationEventRecord> {
        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), world);
        app.insert_resource(FixedTime::with_delta(dt));
        app.insert_resource(clips);
        app.insert_resource(AnimationEvents::default());
        app.add_system(animation_system);
        app.update().expect("animation schedule must run");
        let recorded = app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("AnimationEvents must exist")
            .iter()
            .cloned()
            .collect();
        std::mem::swap(app.world_mut(), world);
        recorded
    }

    #[test]
    fn event_fires_exactly_once_when_time_crosses_it() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(2.0, vec![event(0.5, "hit")]));
        let mut world = engine_ecs::World::new();
        world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");

        let recorded = run_with_events(&mut world, 0.6, store);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].name, "hit");
    }

    #[test]
    fn event_does_not_refire_once_already_crossed() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(2.0, vec![event(0.5, "hit")]));
        let mut world = engine_ecs::World::new();
        world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");

        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), &mut world);
        app.insert_resource(FixedTime::with_delta(0.6));
        app.insert_resource(store);
        app.insert_resource(AnimationEvents::default());
        app.add_system(animation_system);
        app.update().expect("first tick must run"); // 0.0 -> 0.6: fires
        app.update().expect("second tick must run"); // 0.6 -> 1.2: no refire
        let recorded: Vec<AnimationEventRecord> = app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("AnimationEvents must exist")
            .iter()
            .cloned()
            .collect();
        std::mem::swap(app.world_mut(), &mut world);

        assert!(
            recorded.is_empty(),
            "AnimationEvents must be cleared and not refire an already-crossed event"
        );
    }

    #[test]
    fn clip_specific_event_fires_only_for_its_bound_clip() {
        let mut store = Assets::<AnimationClip>::new();
        let idle = store.add(clip_with_events(1.0, Vec::new()));
        let attack = store.add(clip_with_events(1.0, Vec::new()));
        let mut animator = Animator::playing(idle);
        animator.set_clip_events(attack, vec![event(0.25, "attack.active")]);
        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(0.3));
        app.insert_resource(store);
        app.insert_resource(AnimationEvents::default());
        let entity = app
            .world_mut()
            .spawn_with(animator)
            .expect("animator must spawn");
        app.add_system(animation_system);

        app.update().expect("idle tick must run");
        assert!(app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("animation events")
            .iter()
            .next()
            .is_none());

        app.world_mut()
            .get_component_mut::<Animator>(entity)
            .expect("animator")
            .crossfade_to(attack, 0.0);
        app.update().expect("attack tick must run");
        let names = app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("animation events")
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["attack.active"]);
    }

    #[test]
    fn multiple_events_in_one_tick_fire_in_ascending_time_order() {
        let mut store = Assets::<AnimationClip>::new();
        // Declared out of order on purpose.
        let clip = store.add(clip_with_events(
            2.0,
            vec![
                event(0.8, "third"),
                event(0.2, "first"),
                event(0.5, "second"),
            ],
        ));
        let mut world = engine_ecs::World::new();
        world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");

        let recorded = run_with_events(&mut world, 1.0, store);
        let names: Vec<&str> = recorded.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    #[test]
    fn loop_wrap_fires_both_intervals_in_playback_order() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(
            1.0,
            vec![event(0.9, "near_end"), event(0.1, "near_start")],
        ));
        let mut world = engine_ecs::World::new();
        let mut animator = Animator::playing(clip);
        animator.set_looping(true);
        animator.time = 0.8;
        world.spawn_with(animator).expect("animator must spawn");

        // dt = 0.3: 0.8 -> 1.1, wraps to 0.1. Crosses (0.8, 1.0] then (0, 0.1].
        let recorded = run_with_events(&mut world, 0.3, store);
        let names: Vec<&str> = recorded.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["near_end", "near_start"],
            "the tail of the old lap must fire before the head of the new lap"
        );
    }

    #[test]
    fn stopped_animator_fires_nothing() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(2.0, vec![event(0.5, "hit")]));
        let mut world = engine_ecs::World::new();
        let mut animator = Animator::playing(clip);
        animator.stop();
        world.spawn_with(animator).expect("animator must spawn");

        let recorded = run_with_events(&mut world, 0.6, store);
        assert!(recorded.is_empty(), "a stopped animator must fire nothing");
    }

    #[test]
    fn large_dt_step_over_an_event_still_fires_it() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(1.0, vec![event(0.5, "hit")]));
        let mut world = engine_ecs::World::new();
        world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");

        // dt = 5.0 jumps straight past the clip's 1.0s duration; the event at
        // 0.5 must still fire via the interval check, not a sample check.
        let recorded = run_with_events(&mut world, 5.0, store);
        let names: Vec<&str> = recorded.iter().map(|record| record.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["hit", "animation.completed"],
            "the crossed marker must fire before the non-looping clip's completion event"
        );
    }

    #[test]
    fn non_looping_completion_event_is_emitted_once() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip_with_events(0.5, Vec::new()));
        let mut world = engine_ecs::World::new();
        world
            .spawn_with(Animator::playing(clip))
            .expect("animator must spawn");
        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), &mut world);
        app.insert_resource(FixedTime::with_delta(0.5));
        app.insert_resource(store);
        app.insert_resource(AnimationEvents::default());
        app.add_system(animation_system);

        app.update().expect("completion tick must run");
        let first = app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("animation events")
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first, vec!["animation.completed"]);

        app.update().expect("stopped tick must run");
        assert!(app
            .world()
            .get_resource::<AnimationEvents>()
            .expect("animation events")
            .iter()
            .next()
            .is_none());
    }

    #[test]
    fn extracted_root_motion_is_removed_from_pose_and_forwarded_to_motor_request() {
        let mut store = Assets::<AnimationClip>::new();
        let clip = store.add(clip(
            1.0,
            vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: None,
                keyframes: vec![
                    Keyframe {
                        time: 0.0,
                        value: [0.0, 0.0, 0.0, 0.0],
                    },
                    Keyframe {
                        time: 1.0,
                        value: [2.0, 0.0, 0.0, 0.0],
                    },
                ],
            }],
        ));
        let mut animator = Animator::playing(clip);
        animator.root_motion_mode = RootMotionMode::AppliedToMotor;

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(0.25));
        app.insert_resource(store);
        app.insert_resource(AnimationEvents::default());
        let entity = app
            .world_mut()
            .spawn_with(Transform::default())
            .expect("animated entity");
        app.world_mut()
            .add_component(entity, animator)
            .expect("animator");
        app.world_mut()
            .add_component(entity, RootMotionRequest::default())
            .expect("root motion request");
        app.add_system(animation_system);
        app.add_system(root_motion_motor_system);

        app.update().expect("animation and root motion systems");

        assert_eq!(
            app.world()
                .get_component::<Transform>(entity)
                .expect("transform")
                .translation,
            Vec3::ZERO,
            "the animated root pose must stay at its first-frame position"
        );
        let request = app
            .world()
            .get_component::<RootMotionRequest>(entity)
            .expect("root motion request");
        assert!((request.delta.x - 0.5).abs() < 1e-5);
        assert_eq!(request.delta.y, 0.0);
        assert_eq!(request.delta.z, 0.0);
    }

    // --- AnimationClip::validate (ADR 0077) -----------------------------------

    #[test]
    fn validate_reports_a_bone_target_without_a_skeleton() {
        use crate::skeleton_asset::BoneId;

        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: Some(BoneId(0)),
                keyframes: vec![],
            }],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };

        let diagnostic = clip
            .validate()
            .expect("a bone target without a skeleton must be reported");
        assert_eq!(diagnostic.code, CLIP_MISSING_SKELETON_DIAGNOSTIC);
        assert!(diagnostic.is_blocking());
    }

    #[test]
    fn validate_accepts_a_bone_target_with_a_skeleton() {
        use crate::skeleton_asset::BoneId;

        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: Some(BoneId(0)),
                keyframes: vec![],
            }],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: Some(engine_authoring::id::AssetId::generate()),
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };

        assert!(clip.validate().is_none());
    }

    #[test]
    fn validate_accepts_a_code_authored_clip_with_no_bone_targets_and_no_skeleton() {
        let clip = clip(1.0, vec![translation_channel(&[(0.0, 1.0)])]);

        assert!(clip.validate().is_none());
    }

    #[test]
    fn composition_unions_channels_and_later_overlay_wins_conflicts() {
        let mut primary = clip(1.0, vec![translation_channel(&[(0.0, 1.0)])]);
        primary.morph_channels.push(MorphChannel {
            target_name: "blink".to_owned(),
            keyframes: vec![Keyframe {
                time: 0.0,
                value: [0.25, 0.0, 0.0, 0.0],
            }],
        });
        let mut overlay = clip(2.0, vec![translation_channel(&[(0.0, 3.0)])]);
        overlay.morph_channels.extend([
            MorphChannel {
                target_name: "blink".to_owned(),
                keyframes: vec![Keyframe {
                    time: 0.0,
                    value: [0.75, 0.0, 0.0, 0.0],
                }],
            },
            MorphChannel {
                target_name: "smile".to_owned(),
                keyframes: vec![Keyframe {
                    time: 0.0,
                    value: [1.0, 0.0, 0.0, 0.0],
                }],
            },
        ]);

        let composed = compose_animation_clips(&primary, &[overlay]).expect("layers must compose");
        assert_eq!(composed.duration, 2.0);
        assert_eq!(lerp_channel(&composed.channels[0], 0.0).unwrap()[0], 3.0);
        assert_eq!(composed.morph_channels.len(), 2);
        let blink = composed
            .morph_channels
            .iter()
            .find(|channel| channel.target_name == "blink")
            .expect("blink channel must remain");
        assert_eq!(lerp_morph_channel(blink, 0.0), Some(0.75));
    }

    #[test]
    fn morph_crossfade_treats_missing_channel_as_zero() {
        let blended = PoseGraphOutput::blend(
            &entity_morph_pose("blink", 1.0),
            &entity_morph_pose("smile", 0.8),
            0.25,
        )
        .morph_weights;

        assert_eq!(blended.get("blink"), Some(&0.75));
        assert!((blended["smile"] - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn animation_system_fans_one_logical_morph_out_to_all_rig_render_parts() {
        use crate::morph::MorphAsset;

        let mut clips = Assets::<AnimationClip>::new();
        let mut morph_clip = clip(1.0, Vec::new());
        morph_clip.morph_channels.push(MorphChannel {
            target_name: "blink".to_owned(),
            keyframes: vec![
                Keyframe {
                    time: 0.0,
                    value: [0.0, 0.0, 0.0, 0.0],
                },
                Keyframe {
                    time: 1.0,
                    value: [1.0, 0.0, 0.0, 0.0],
                },
            ],
        });
        let handle = clips.add(morph_clip);
        let mut world = engine_ecs::World::new();
        let rig = world
            .spawn_with(Animator::playing(handle))
            .expect("rig animator must spawn");
        let mut renderers = Vec::new();
        for _ in 0..2 {
            let targets = MorphTargets::new(
                vec![MorphAsset {
                    source_index: 0,
                    id: AssetId::generate(),
                    name: "blink".to_owned(),
                    vertex_deltas: Vec::new(),
                    material_offsets: Vec::new(),
                }],
                Vec::new(),
            );
            let renderer = world
                .spawn_with(SkinnedMesh {
                    rig,
                    joint_bones: Vec::new(),
                    inverse_bind_matrices: Vec::new(),
                    skin: None,
                })
                .expect("render part must spawn");
            world
                .add_component(renderer, MorphWeights::for_targets(&targets))
                .expect("render part must accept weights");
            world
                .add_component(renderer, targets)
                .expect("render part must accept targets");
            renderers.push(renderer);
        }

        run_animation_system(&mut world, 0.5, clips);

        for renderer in renderers {
            let weights = world
                .get_component::<MorphWeights>(renderer)
                .expect("render part must retain weights");
            assert!((weights.get(0) - 0.5).abs() < 1.0e-5);
        }
    }

    /// Simulation that carries state across steps has to be told when the
    /// pose it follows was repositioned. Playback knows exactly when that
    /// happens; nothing downstream can recover it from the pose, because a
    /// fast clip and a seek arrive as the same large displacement.
    #[test]
    fn playback_declares_a_repositioning_but_never_mere_speed() {
        use crate::skeleton_asset::{compute_skeleton_identity, BoneDef, SkeletonAsset};

        fn run(build: impl FnOnce(&mut Animator), steps: usize) -> bool {
            let mut clips = Assets::<AnimationClip>::new();
            // One bone travelling 100 m over one second: far faster than any
            // threshold on displacement would tolerate, yet continuous.
            let clip = clips.add(AnimationClip {
                duration: 1.0,
                channels: vec![AnimChannel {
                    property: AnimProperty::Translation,
                    target_bone: Some(BoneId(0)),
                    keyframes: vec![
                        Keyframe {
                            time: 0.0,
                            value: [0.0, 0.0, 0.0, 0.0],
                        },
                        Keyframe {
                            time: 1.0,
                            value: [100.0, 0.0, 0.0, 0.0],
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

            let bones = vec![BoneDef {
                id: BoneId(0),
                name: "bone".to_owned(),
                parent: None,
                rest_translation: Vec3::ZERO,
                rest_rotation: Quat::IDENTITY,
                rest_scale: Vec3::ONE,
            }];
            let skeleton_asset = SkeletonAsset {
                id: AssetId::generate(),
                name: "continuity".to_owned(),
                identity: compute_skeleton_identity(&bones),
                next_bone_id: 1,
                bones,
            };

            let mut app = engine_ecs::App::new();
            // A quarter second divides the clip exactly, so the wrap lands on a
            // known step instead of on however the accumulation rounds.
            app.insert_resource(FixedTime::with_delta(0.25));
            app.insert_resource(clips);
            let entity = app
                .world_mut()
                .spawn_with(Transform::default())
                .expect("animator entity");
            let mut animator = Animator::playing(clip);
            build(&mut animator);
            app.world_mut()
                .add_component(entity, animator)
                .expect("animator");
            app.world_mut()
                .add_component(
                    entity,
                    Skeleton {
                        joints: Vec::new(),
                        bone_ids: vec![BoneId(0)],
                        asset: None,
                    },
                )
                .expect("skeleton");
            app.world_mut()
                .add_component(entity, RigPose::from_skeleton(&skeleton_asset))
                .expect("rig pose");
            app.add_system(crate::rig_pose::rig_pose_clear_transient_system);
            app.add_system(animation_system);
            for _ in 0..steps {
                app.update().expect("fixed step");
            }
            app.world()
                .get_component::<RigPose>(entity)
                .expect("rig pose")
                .is_discontinuous()
        }

        // Three steps of a one-second clip: 75 m travelled, no wrap.
        assert!(
            !run(|_| {}, 3),
            "speed alone is never a repositioning, however extreme"
        );
        // The fourth step of a looping one-second clip reaches its end.
        assert!(
            run(|animator| animator.looping = true, 4),
            "a loop wrap restarts the clock and must declare itself"
        );
        // ...and the step after the wrap is ordinary motion again.
        assert!(
            !run(|animator| animator.looping = true, 5),
            "the declaration lasts exactly one step"
        );
        assert!(
            run(|animator| animator.seek(0.7), 1),
            "a seek moves the clock, not the character"
        );

        // A crossfade blends, so its pose stays continuous; an instant switch
        // replaces it outright and does not.
        let mut clips = Assets::<AnimationClip>::new();
        let bare = || AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let first = clips.add(bare());
        let second = clips.add(bare());
        let third = clips.add(bare());
        let mut animator = Animator::playing(first);
        animator.crossfade_to(second, 0.2);
        assert_eq!(
            animator.take_discontinuity(),
            None,
            "a crossfade blends, so its pose is continuous by construction"
        );
        animator.crossfade_to(third, 0.0);
        assert_eq!(
            animator.take_discontinuity(),
            Some(AnimatorDiscontinuity::Reposition),
            "an instant switch replaces the pose outright"
        );
    }
}
