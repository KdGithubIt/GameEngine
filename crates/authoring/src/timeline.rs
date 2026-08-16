//! Versioned Timeline authoring model and deterministic compilation (ADR 0126).
//!
//! Timeline owns sequence time, stable bindings, track/clip/marker identity,
//! validation, and conversion into the dependency-neutral `engine-timeline`
//! schedule. It does not sample animation clips, mutate runtime transforms,
//! choose render cameras, or dispatch project gameplay itself.

use crate::diagnostic::Diagnostic;
use crate::id::{
    AssetId, EntityId, MotionSlotId, TimelineClipId, TimelineId, TimelineMarkerId,
    TimelineTrackId,
};
use engine_timeline::{
    CompiledEntry, CompiledTimeline, CompiledTimelineError, SeekCapability, TimelineTick,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Current persisted `*.timeline.json` schema version.
pub const TIMELINE_SCHEMA_VERSION: u32 = 1;

/// Canonical file-name suffix for Timeline authoring documents.
pub const TIMELINE_FILE_SUFFIX: &str = ".timeline.json";

/// Rational display frame rate used for frame snapping and timecode.
///
/// This affects presentation only. Persisted clip and marker positions always
/// remain exact [`TimelineTick`] values on the 48,000 Hz canonical clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineDisplayRate {
    /// Display frames in `denominator` seconds.
    pub numerator: u32,
    /// Time base denominator. `1001` permits NTSC-style rates.
    pub denominator: u32,
}

impl Default for TimelineDisplayRate {
    fn default() -> Self {
        Self {
            numerator: 30,
            denominator: 1,
        }
    }
}

/// Blend/easing metadata for a clip boundary.
///
/// Not every track consumes blend metadata. Keeping it typed on the clip
/// timing contract allows a supporting adapter to opt in without changing the
/// canonical integer boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TimelineBlend {
    /// Blend-in duration in canonical ticks.
    pub in_ticks: TimelineTick,
    /// Blend-out duration in canonical ticks.
    pub out_ticks: TimelineTick,
}

/// Common stable timing carried by every interval clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineClipTiming {
    /// Stable clip identity that survives moves, trims, and renames.
    pub id: TimelineClipId,
    /// Inclusive first active Timeline tick.
    pub start_tick: TimelineTick,
    /// Exclusive end Timeline tick.
    pub end_tick: TimelineTick,
    /// Optional typed blend durations for adapters that support blending.
    #[serde(default)]
    pub blend: TimelineBlend,
}

impl TimelineClipTiming {
    /// Returns the exact interval length in ticks.
    pub fn duration_ticks(&self) -> i64 {
        self.end_tick.get().saturating_sub(self.start_tick.get())
    }
}

/// One Animation Track clip binding an entity to an Animation Set motion slot.
///
/// The payload deliberately references an [`AssetId`] Animation Set and stable
/// [`MotionSlotId`]. It never stores an imported clip path or runtime handle;
/// resolving and sampling the motion remains Animation's responsibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineAnimationClip {
    /// Stable Timeline interval.
    pub timing: TimelineClipTiming,
    /// Stable authored entity whose Animator/animation domain receives motion.
    pub target: EntityId,
    /// Stable Animation Set asset owning the motion-slot binding.
    pub animation_set: AssetId,
    /// Stable motion slot selected from the Animation Set contract.
    pub motion_slot: MotionSlotId,
    /// Clip-local source offset in the same 48,000 Hz tick domain.
    #[serde(default)]
    pub source_start_tick: TimelineTick,
}

/// One numeric Vec3 key in a Transform/Property clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimelineVec3Key {
    /// Tick relative to the containing clip's start.
    pub tick: TimelineTick,
    /// Typed XYZ value.
    pub value: [f32; 3],
}

/// One quaternion key in a Transform/Property clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimelineQuatKey {
    /// Tick relative to the containing clip's start.
    pub tick: TimelineTick,
    /// Typed XYZW quaternion value.
    pub value: [f32; 4],
}

/// Explicitly supported Transform property curve.
///
/// This closed enum is the initial safe Property Track surface. It does not
/// permit reflection into arbitrary ECS memory or string property paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "property", content = "keys", rename_all = "snake_case")]
pub enum TimelineTransformCurve {
    /// Local translation sampled from Vec3 keys.
    Translation(Vec<TimelineVec3Key>),
    /// Local rotation sampled from quaternion keys.
    Rotation(Vec<TimelineQuatKey>),
    /// Local scale sampled from Vec3 keys.
    Scale(Vec<TimelineVec3Key>),
}

/// Typed sampled value returned by [`TimelineTransformCurve::sample`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimelineTransformSample {
    /// Local translation value.
    Translation([f32; 3]),
    /// Normalized local quaternion rotation value.
    Rotation([f32; 4]),
    /// Local scale value.
    Scale([f32; 3]),
}

impl TimelineTransformCurve {
    /// Samples this typed curve at a clip-local tick.
    ///
    /// The function only interpolates authored curve values. Applying the
    /// result to the runtime Transform remains an engine
    /// composition concern and is intentionally not implemented here.
    pub fn sample(&self, tick: TimelineTick) -> Option<TimelineTransformSample> {
        match self {
            Self::Translation(keys) => sample_vec3(keys, tick).map(TimelineTransformSample::Translation),
            Self::Rotation(keys) => sample_quat(keys, tick).map(TimelineTransformSample::Rotation),
            Self::Scale(keys) => sample_vec3(keys, tick).map(TimelineTransformSample::Scale),
        }
    }

    fn validate(&self, clip: &TimelineClipTiming) -> Vec<Diagnostic> {
        match self {
            Self::Translation(keys) | Self::Scale(keys) => validate_vec3_keys(keys, clip),
            Self::Rotation(keys) => validate_quat_keys(keys, clip),
        }
    }
}

/// One Transform/Property Track clip over the initial explicit Transform set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineTransformClip {
    /// Stable Timeline interval.
    pub timing: TimelineClipTiming,
    /// Stable authored entity whose existing Transform receives the sample.
    pub target: EntityId,
    /// Closed typed property curve.
    pub curve: TimelineTransformCurve,
}

/// One Camera Cut interval.
///
/// Camera selection is represented only as a stable camera binding plus an
/// explicit override priority. Runtime application must install a transient
/// override; this document never edits Camera3D enabled/priority fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCameraCutClip {
    /// Stable Timeline interval.
    pub timing: TimelineClipTiming,
    /// Stable authored game-camera entity.
    pub camera: EntityId,
    /// Explicit priority among overlapping Timeline camera overrides.
    pub override_priority: i32,
}

/// One Audio Track interval evaluated through the ADR 0122 tracked-voice path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineAudioClip {
    /// Stable Timeline interval.
    pub timing: TimelineClipTiming,
    /// Stable authored audio asset.
    pub audio_asset: AssetId,
    /// Optional stable scene entity supplying spatial position.
    pub emitter: Option<EntityId>,
    /// Linear clip gain. Must be finite and non-negative.
    pub volume: f32,
    /// ADR 0122 spatial blend in the inclusive range `[0, 1]`.
    pub spatial_blend: f32,
    /// Whether the tracked voice loops while the Timeline clip remains active.
    pub looping: bool,
}

/// One VFX Track interval evaluated through the ADR 0125 runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineVfxClip {
    /// Stable Timeline interval.
    pub timing: TimelineClipTiming,
    /// Stable authored VFX effect asset.
    pub effect_asset: AssetId,
    /// Stable scene entity receiving the effect instance.
    pub target: EntityId,
    /// Non-negative VFX simulation multiplier.
    pub time_scale: f32,
    /// Optional deterministic seed override.
    pub seed_override: Option<u32>,
}

/// One stable sequence-level Event marker.
///
/// `id` is the stable event identity used for ordering and source attribution.
/// `name` is the authored event name passed to the later bounded host adapter;
/// it is not used to resolve an entity, asset, or track implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEventMarker {
    /// Stable marker/event identity.
    pub id: TimelineMarkerId,
    /// Exact marker tick.
    pub tick: TimelineTick,
    /// Human/project-facing event name.
    pub name: String,
}

/// Persisted, typed track family.
///
/// Every built-in family is explicit; no opaque JSON/reflection escape hatch is
/// accepted by Timeline authoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TimelineTrackKind {
    /// Animation Set motion-slot clips.
    Animation {
        /// Ordered source clips; deterministic compilation reorders ties by
        /// exact time and stable clip ID rather than Vec iteration identity.
        clips: Vec<TimelineAnimationClip>,
    },
    /// Explicitly supported Transform property curves.
    TransformProperty {
        /// Typed transform clips.
        clips: Vec<TimelineTransformClip>,
    },
    /// Transient game-camera cuts.
    CameraCut {
        /// Camera cut intervals.
        clips: Vec<TimelineCameraCutClip>,
    },
    /// ADR 0122 tracked audio voices.
    Audio {
        /// Ordered typed audio clips.
        clips: Vec<TimelineAudioClip>,
    },
    /// ADR 0125 deterministic VFX playback.
    Vfx {
        /// Ordered typed VFX clips.
        clips: Vec<TimelineVfxClip>,
    },
    /// Sequence-level event marker lane.
    Event {
        /// Stable sequence Event markers.
        markers: Vec<TimelineEventMarker>,
    },
}

/// Stable built-in Timeline track type identifier.
///
/// This is a closed typed identifier, not a user-provided string. Future track
/// families extend it only when their authoring/runtime contract is explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimelineTrackType {
    /// Animation Set motion-slot track.
    Animation,
    /// Explicit Transform property track.
    TransformProperty,
    /// Camera Cut track.
    CameraCut,
    /// Audio track.
    Audio,
    /// VFX track.
    Vfx,
    /// Sequence Event track.
    Event,
}

impl TimelineTrackKind {
    /// Returns the stable typed family identifier.
    pub const fn track_type(&self) -> TimelineTrackType {
        match self {
            Self::Animation { .. } => TimelineTrackType::Animation,
            Self::TransformProperty { .. } => TimelineTrackType::TransformProperty,
            Self::CameraCut { .. } => TimelineTrackType::CameraCut,
            Self::Audio { .. } => TimelineTrackType::Audio,
            Self::Vfx { .. } => TimelineTrackType::Vfx,
            Self::Event { .. } => TimelineTrackType::Event,
        }
    }

    /// Returns the clip with `id`, when this family owns interval clips.
    pub fn clip(&self, id: &TimelineClipId) -> Option<TimelineClipRef<'_>> {
        match self {
            Self::Animation { clips } => clips
                .iter()
                .find(|clip| &clip.timing.id == id)
                .map(TimelineClipRef::Animation),
            Self::TransformProperty { clips } => clips
                .iter()
                .find(|clip| &clip.timing.id == id)
                .map(TimelineClipRef::TransformProperty),
            Self::CameraCut { clips } => clips
                .iter()
                .find(|clip| &clip.timing.id == id)
                .map(TimelineClipRef::CameraCut),
            Self::Audio { clips } => clips
                .iter()
                .find(|clip| &clip.timing.id == id)
                .map(TimelineClipRef::Audio),
            Self::Vfx { clips } => clips
                .iter()
                .find(|clip| &clip.timing.id == id)
                .map(TimelineClipRef::Vfx),
            Self::Event { .. } => None,
        }
    }
}

/// Borrowed typed interval clip returned by inspection helpers.
#[derive(Debug, Clone, Copy)]
pub enum TimelineClipRef<'a> {
    /// Animation clip.
    Animation(&'a TimelineAnimationClip),
    /// Transform/property clip.
    TransformProperty(&'a TimelineTransformClip),
    /// Camera Cut clip.
    CameraCut(&'a TimelineCameraCutClip),
    /// Audio clip.
    Audio(&'a TimelineAudioClip),
    /// VFX clip.
    Vfx(&'a TimelineVfxClip),
}

impl TimelineClipRef<'_> {
    /// Returns common stable timing.
    pub const fn timing(self) -> &TimelineClipTiming {
        match self {
            Self::Animation(clip) => &clip.timing,
            Self::TransformProperty(clip) => &clip.timing,
            Self::CameraCut(clip) => &clip.timing,
            Self::Audio(clip) => &clip.timing,
            Self::Vfx(clip) => &clip.timing,
        }
    }
}

/// One persisted Timeline track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineTrack {
    /// Stable track identity.
    pub id: TimelineTrackId,
    /// Human-readable track name; never used for binding resolution.
    pub name: String,
    /// Whether this track participates in compiled/runtime evaluation.
    pub enabled: bool,
    /// Explicit persisted ordering key. Equal keys remain deterministic via
    /// stable track ID but produce a validation warning.
    pub order: i32,
    /// Closed typed track payload.
    pub kind: TimelineTrackKind,
}

/// Canonical versioned Timeline authoring document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineDocument {
    /// Persisted schema version.
    pub schema_version: u32,
    /// Stable document identity.
    pub id: TimelineId,
    /// Human-readable Timeline name.
    pub name: String,
    /// Inclusive final playhead tick. Interval clips end at or before it.
    pub duration_ticks: TimelineTick,
    /// Rational display frame rate; does not change canonical tick time.
    pub display_rate: TimelineDisplayRate,
    /// Authored typed tracks.
    pub tracks: Vec<TimelineTrack>,
}

impl TimelineDocument {
    /// Creates an empty current-format Timeline document.
    pub fn new(id: TimelineId, name: impl Into<String>, duration_ticks: TimelineTick) -> Self {
        Self {
            schema_version: TIMELINE_SCHEMA_VERSION,
            id,
            name: name.into(),
            duration_ticks,
            display_rate: TimelineDisplayRate::default(),
            tracks: Vec::new(),
        }
    }

    /// Parses and structurally validates a current-format Timeline JSON file.
    ///
    /// # Errors
    ///
    /// Returns malformed JSON, an unsupported version, or deterministic
    /// structural diagnostics.
    pub fn from_json(json: &str) -> Result<Self, TimelineDocumentError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            schema_version: u32,
        }

        let version: VersionProbe =
            serde_json::from_str(json).map_err(TimelineDocumentError::Json)?;
        if version.schema_version != TIMELINE_SCHEMA_VERSION {
            return Err(TimelineDocumentError::UnsupportedVersion {
                found: version.schema_version,
            });
        }
        let document: Self = serde_json::from_str(json).map_err(TimelineDocumentError::Json)?;
        let diagnostics = document.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(TimelineDocumentError::Validation { diagnostics });
        }
        Ok(document)
    }

    /// Serializes deterministic pretty current-format JSON with a final newline.
    ///
    /// # Errors
    ///
    /// Returns structural validation or serialization failure.
    pub fn to_canonical_json(&self) -> Result<String, TimelineDocumentError> {
        let diagnostics = self.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(TimelineDocumentError::Validation { diagnostics });
        }
        let mut json = serde_json::to_string_pretty(self).map_err(TimelineDocumentError::Json)?;
        json.push('\n');
        Ok(json)
    }

    /// Returns deterministic structural diagnostics.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        if self.schema_version != TIMELINE_SCHEMA_VERSION {
            diagnostics.push(Diagnostic::error(
                "timeline.unsupported_version",
                format!(
                    "Timeline schema version {} is unsupported; expected {}",
                    self.schema_version, TIMELINE_SCHEMA_VERSION
                ),
            ));
        }
        if self.name.trim().is_empty() {
            diagnostics.push(Diagnostic::error(
                "timeline.blank_name",
                "Timeline name must not be blank",
            ));
        }
        if self.duration_ticks < TimelineTick::ZERO {
            diagnostics.push(Diagnostic::error(
                "timeline.negative_duration",
                format!("Timeline duration {} is negative", self.duration_ticks),
            ));
        }
        if self.display_rate.numerator == 0 || self.display_rate.denominator == 0 {
            diagnostics.push(Diagnostic::error(
                "timeline.invalid_display_rate",
                "Timeline display rate numerator and denominator must be non-zero",
            ));
        }

        let mut track_ids = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        let mut marker_ids = BTreeSet::new();
        let mut order_owners = BTreeMap::<i32, TimelineTrackId>::new();
        for track in self.tracks_in_evaluation_order() {
            if !track_ids.insert(track.id.clone()) {
                diagnostics.push(Diagnostic::error(
                    "timeline.duplicate_track_id",
                    format!("Timeline track ID `{}` is duplicated", track.id),
                ));
            }
            if track.name.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "timeline.blank_track_name",
                    format!("Timeline track `{}` has a blank name", track.id),
                ));
            }
            if let Some(previous) = order_owners.insert(track.order, track.id.clone()) {
                diagnostics.push(Diagnostic::warning(
                    "timeline.track_order_tie",
                    format!(
                        "Timeline tracks `{previous}` and `{}` share order {}; stable track ID breaks the tie deterministically",
                        track.id, track.order
                    ),
                ));
            }
            validate_track(
                track,
                self.duration_ticks,
                &mut clip_ids,
                &mut marker_ids,
                &mut diagnostics,
            );
        }
        diagnostics
    }

    /// Returns binding-existence diagnostics using a project/scene-owned
    /// resolver. No display-name fallback is attempted.
    pub fn validate_bindings(
        &self,
        resolver: &dyn TimelineBindingResolver,
    ) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for track in self.tracks_in_evaluation_order() {
            match &track.kind {
                TimelineTrackKind::Animation { clips } => {
                    let mut clips = clips.iter().collect::<Vec<_>>();
                    clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                    for clip in clips {
                        if !resolver.entity_exists(&clip.target) {
                            diagnostics.push(missing_entity_binding(
                                &track.id,
                                &clip.timing.id,
                                &clip.target,
                            ));
                        }
                        if !resolver.asset_exists(&clip.animation_set) {
                            diagnostics.push(Diagnostic::error(
                                "timeline.missing_asset_binding",
                                format!(
                                    "Timeline track `{}` clip `{}` references missing Animation Set asset `{}`",
                                    track.id, clip.timing.id, clip.animation_set
                                ),
                            ));
                        }
                    }
                }
                TimelineTrackKind::TransformProperty { clips } => {
                    let mut clips = clips.iter().collect::<Vec<_>>();
                    clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                    for clip in clips {
                        if !resolver.entity_exists(&clip.target) {
                            diagnostics.push(missing_entity_binding(
                                &track.id,
                                &clip.timing.id,
                                &clip.target,
                            ));
                        }
                    }
                }
                TimelineTrackKind::CameraCut { clips } => {
                    let mut clips = clips.iter().collect::<Vec<_>>();
                    clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                    for clip in clips {
                        if !resolver.entity_exists(&clip.camera) {
                            diagnostics.push(missing_entity_binding(
                                &track.id,
                                &clip.timing.id,
                                &clip.camera,
                            ));
                        }
                    }
                }
                TimelineTrackKind::Audio { clips } => {
                    for clip in clips {
                        if !resolver.asset_exists(&clip.audio_asset) {
                            diagnostics.push(Diagnostic::error(
                                "timeline.missing_asset_binding",
                                format!(
                                    "Timeline track `{}` clip `{}` references missing audio asset `{}`",
                                    track.id, clip.timing.id, clip.audio_asset
                                ),
                            ));
                        }
                        if let Some(emitter) = &clip.emitter {
                            if !resolver.entity_exists(emitter) {
                                diagnostics.push(missing_entity_binding(
                                    &track.id,
                                    &clip.timing.id,
                                    emitter,
                                ));
                            }
                        }
                    }
                }
                TimelineTrackKind::Vfx { clips } => {
                    for clip in clips {
                        if !resolver.asset_exists(&clip.effect_asset) {
                            diagnostics.push(Diagnostic::error(
                                "timeline.missing_asset_binding",
                                format!(
                                    "Timeline track `{}` clip `{}` references missing VFX asset `{}`",
                                    track.id, clip.timing.id, clip.effect_asset
                                ),
                            ));
                        }
                        if !resolver.entity_exists(&clip.target) {
                            diagnostics.push(missing_entity_binding(
                                &track.id,
                                &clip.timing.id,
                                &clip.target,
                            ));
                        }
                    }
                }
                TimelineTrackKind::Event { .. } => {}
            }
        }
        diagnostics
    }

    /// Returns one track by stable ID.
    pub fn track(&self, id: &TimelineTrackId) -> Option<&TimelineTrack> {
        self.tracks.iter().find(|track| &track.id == id)
    }

    /// Returns tracks in canonical deterministic evaluation order.
    pub fn tracks_in_evaluation_order(&self) -> Vec<&TimelineTrack> {
        let mut tracks = self.tracks.iter().collect::<Vec<_>>();
        tracks.sort_by_key(|track| (track.order, track.id.clone()));
        tracks
    }
}

fn missing_entity_binding(
    track: &TimelineTrackId,
    clip: &TimelineClipId,
    entity: &EntityId,
) -> Diagnostic {
    Diagnostic::error(
        "timeline.missing_entity_binding",
        format!(
            "Timeline track `{track}` clip `{clip}` references missing entity `{entity}`"
        ),
    )
}

/// Project/scene-owned existence query used for stable Timeline bindings.
///
/// Implementations inspect stable authoring identity only. A resolver must not
/// search for same-name replacements when an ID is missing.
pub trait TimelineBindingResolver {
    /// Returns whether the stable authored entity currently exists.
    fn entity_exists(&self, entity: &EntityId) -> bool;
    /// Returns whether the stable authored asset currently exists.
    fn asset_exists(&self, asset: &AssetId) -> bool;
}

/// Error loading/saving a versioned Timeline document.
#[derive(Debug)]
pub enum TimelineDocumentError {
    /// Malformed JSON or an invalid typed stable ID.
    Json(serde_json::Error),
    /// Persisted schema version is unsupported.
    UnsupportedVersion {
        /// Version found in the document.
        found: u32,
    },
    /// Current-format structural validation failed.
    Validation {
        /// Deterministic diagnostics.
        diagnostics: Vec<Diagnostic>,
    },
}

impl fmt::Display for TimelineDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid Timeline JSON: {error}"),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "unsupported Timeline schema version {found}; expected {TIMELINE_SCHEMA_VERSION}"
            ),
            Self::Validation { diagnostics } => {
                let count = diagnostics.iter().filter(|item| item.is_blocking()).count();
                write!(formatter, "Timeline validation failed with {count} error(s)")
            }
        }
    }
}

impl std::error::Error for TimelineDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedVersion { .. } | Self::Validation { .. } => None,
        }
    }
}

/// Immutable payload compiled from the currently supported typed track set.
///
/// The neutral scheduler is generic over this closed typed payload set.
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledTimelinePayload {
    /// Animation Set motion-slot evaluation request.
    Animation(CompiledAnimationPayload),
    /// Typed Transform property curve evaluation request.
    TransformProperty(CompiledTransformPayload),
    /// Runtime-only game-camera override request.
    CameraCut(CompiledCameraCutPayload),
    /// ADR 0122 tracked-voice request.
    Audio(CompiledAudioPayload),
    /// ADR 0125 deterministic VFX request.
    Vfx(CompiledVfxPayload),
    /// Sequence-level Event marker request.
    Event(CompiledEventPayload),
}

/// Pre-resolved stable Animation Track payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAnimationPayload {
    /// Stable source track.
    pub track: TimelineTrackId,
    /// Stable source clip.
    pub clip: TimelineClipId,
    /// Stable target entity.
    pub target: EntityId,
    /// Stable Animation Set asset.
    pub animation_set: AssetId,
    /// Stable Animation Set motion slot.
    pub motion_slot: MotionSlotId,
    /// Clip-local source start in canonical ticks.
    pub source_start_tick: TimelineTick,
}

impl CompiledAnimationPayload {
    /// Returns the source-domain sample position for a Timeline local tick.
    pub fn source_tick(&self, local_tick: TimelineTick) -> TimelineTick {
        self.source_start_tick.saturating_add(local_tick.get())
    }
}

/// Pre-resolved stable Transform/Property Track payload.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTransformPayload {
    /// Stable source track.
    pub track: TimelineTrackId,
    /// Stable source clip.
    pub clip: TimelineClipId,
    /// Stable target entity.
    pub target: EntityId,
    /// Immutable typed property curve.
    pub curve: TimelineTransformCurve,
}

impl CompiledTransformPayload {
    /// Samples the immutable typed curve at a clip-local tick.
    pub fn sample(&self, local_tick: TimelineTick) -> Option<TimelineTransformSample> {
        self.curve.sample(local_tick)
    }
}

/// Pre-resolved stable Camera Cut payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCameraCutPayload {
    /// Stable source track.
    pub track: TimelineTrackId,
    /// Stable source clip.
    pub clip: TimelineClipId,
    /// Stable authored camera entity.
    pub camera: EntityId,
    /// Explicit priority for transient Timeline camera arbitration.
    pub override_priority: i32,
}

/// Pre-resolved stable Event Track marker payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledEventPayload {
    /// Stable source track.
    pub track: TimelineTrackId,
    /// Stable marker/event identity.
    pub marker: TimelineMarkerId,
    /// Authored sequence-level event name.
    pub name: String,
}

/// Error compiling a validated authoring Timeline to the immutable schedule.
#[derive(Debug)]
pub enum TimelineCompileError {
    /// Authoring validation produced blocking diagnostics.
    Validation {
        /// Deterministic diagnostics.
        diagnostics: Vec<Diagnostic>,
    },
    /// The neutral schedule rejected a span after authoring validation.
    Schedule(CompiledTimelineError),
}

impl TimelineCompileError {
    /// Returns structural diagnostics when compilation was blocked by authoring.
    pub fn diagnostics(&self) -> Option<&[Diagnostic]> {
        match self {
            Self::Validation { diagnostics } => Some(diagnostics),
            Self::Schedule(_) => None,
        }
    }
}

impl fmt::Display for TimelineCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { diagnostics } => write!(
                formatter,
                "Timeline compilation blocked by {} validation diagnostic(s)",
                diagnostics.len()
            ),
            Self::Schedule(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TimelineCompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Schedule(error) => Some(error),
            Self::Validation { .. } => None,
        }
    }
}

/// Compiles structural authoring data into a deterministic immutable schedule.
///
/// Stable entity/asset references are retained as typed IDs. A composition
/// layer resolves them to runtime entities/handles and delegates actual motion,
/// Transform, camera, and event application to the owning domains.
pub fn compile_timeline(
    document: &TimelineDocument,
) -> Result<CompiledTimeline<CompiledTimelinePayload>, TimelineCompileError> {
    let diagnostics = document.validate();
    if diagnostics.iter().any(Diagnostic::is_blocking) {
        return Err(TimelineCompileError::Validation { diagnostics });
    }

    let mut entries = Vec::new();
    for (track_order, track) in document.tracks_in_evaluation_order().into_iter().enumerate() {
        if !track.enabled {
            continue;
        }
        let track_order = u32::try_from(track_order).unwrap_or(u32::MAX);
        match &track.kind {
            TimelineTrackKind::Animation { clips } => {
                let mut clips = clips.iter().collect::<Vec<_>>();
                clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                for (item_order, clip) in clips.into_iter().enumerate() {
                    entries.push(CompiledEntry::interval(
                        track_order,
                        u32::try_from(item_order).unwrap_or(u32::MAX),
                        clip.timing.start_tick,
                        clip.timing.end_tick,
                        SeekCapability::Seekable,
                        CompiledTimelinePayload::Animation(CompiledAnimationPayload {
                            track: track.id.clone(),
                            clip: clip.timing.id.clone(),
                            target: clip.target.clone(),
                            animation_set: clip.animation_set.clone(),
                            motion_slot: clip.motion_slot.clone(),
                            source_start_tick: clip.source_start_tick,
                        }),
                    ));
                }
            }
            TimelineTrackKind::TransformProperty { clips } => {
                let mut clips = clips.iter().collect::<Vec<_>>();
                clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                for (item_order, clip) in clips.into_iter().enumerate() {
                    entries.push(CompiledEntry::interval(
                        track_order,
                        u32::try_from(item_order).unwrap_or(u32::MAX),
                        clip.timing.start_tick,
                        clip.timing.end_tick,
                        SeekCapability::Stateless,
                        CompiledTimelinePayload::TransformProperty(CompiledTransformPayload {
                            track: track.id.clone(),
                            clip: clip.timing.id.clone(),
                            target: clip.target.clone(),
                            curve: clip.curve.clone(),
                        }),
                    ));
                }
            }
            TimelineTrackKind::CameraCut { clips } => {
                let mut clips = clips.iter().collect::<Vec<_>>();
                clips.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
                for (item_order, clip) in clips.into_iter().enumerate() {
                    entries.push(CompiledEntry::interval(
                        track_order,
                        u32::try_from(item_order).unwrap_or(u32::MAX),
                        clip.timing.start_tick,
                        clip.timing.end_tick,
                        SeekCapability::Stateless,
                        CompiledTimelinePayload::CameraCut(CompiledCameraCutPayload {
                            track: track.id.clone(),
                            clip: clip.timing.id.clone(),
                            camera: clip.camera.clone(),
                            override_priority: clip.override_priority,
                        }),
                    ));
                }
            }
            TimelineTrackKind::Event { markers } => {
                let mut markers = markers.iter().collect::<Vec<_>>();
                markers.sort_by_key(|marker| (marker.tick, marker.id.clone()));
                for (item_order, marker) in markers.into_iter().enumerate() {
                    entries.push(CompiledEntry::point(
                        track_order,
                        u32::try_from(item_order).unwrap_or(u32::MAX),
                        marker.tick,
                        SeekCapability::Stateless,
                        true,
                        CompiledTimelinePayload::Event(CompiledEventPayload {
                            track: track.id.clone(),
                            marker: marker.id.clone(),
                            name: marker.name.clone(),
                        }),
                    ));
                }
            }
        }
    }

    CompiledTimeline::new(document.duration_ticks, entries).map_err(TimelineCompileError::Schedule)
}

fn validate_track(
    track: &TimelineTrack,
    timeline_duration: TimelineTick,
    clip_ids: &mut BTreeSet<TimelineClipId>,
    marker_ids: &mut BTreeSet<TimelineMarkerId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &track.kind {
        TimelineTrackKind::Animation { clips } => {
            let mut ordered = clips.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
            for clip in ordered {
                validate_clip_timing(&track.id, &clip.timing, timeline_duration, clip_ids, diagnostics);
                if clip.source_start_tick < TimelineTick::ZERO {
                    diagnostics.push(Diagnostic::error(
                        "timeline.animation_negative_source_tick",
                        format!(
                            "Animation clip `{}` has negative source start tick {}",
                            clip.timing.id, clip.source_start_tick
                        ),
                    ));
                }
            }
        }
        TimelineTrackKind::TransformProperty { clips } => {
            let mut ordered = clips.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
            for clip in ordered {
                validate_clip_timing(&track.id, &clip.timing, timeline_duration, clip_ids, diagnostics);
                diagnostics.extend(clip.curve.validate(&clip.timing));
            }
        }
        TimelineTrackKind::CameraCut { clips } => {
            let mut ordered = clips.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
            for clip in ordered {
                validate_clip_timing(&track.id, &clip.timing, timeline_duration, clip_ids, diagnostics);
            }
            warn_camera_cut_priority_ties(track, clips, diagnostics);
        }
        TimelineTrackKind::Event { markers } => {
            let mut ordered = markers.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|marker| (marker.tick, marker.id.clone()));
            for marker in ordered {
                if !marker_ids.insert(marker.id.clone()) {
                    diagnostics.push(Diagnostic::error(
                        "timeline.duplicate_marker_id",
                        format!("Timeline marker ID `{}` is duplicated", marker.id),
                    ));
                }
                if marker.tick < TimelineTick::ZERO || marker.tick > timeline_duration {
                    diagnostics.push(Diagnostic::error(
                        "timeline.marker_out_of_range",
                        format!(
                            "Event marker `{}` tick {} is outside Timeline [0, {}]",
                            marker.id, marker.tick, timeline_duration
                        ),
                    ));
                }
                if marker.name.trim().is_empty() {
                    diagnostics.push(Diagnostic::error(
                        "timeline.blank_event_name",
                        format!("Event marker `{}` has a blank event name", marker.id),
                    ));
                }
            }
        }
    }
}

fn validate_clip_timing(
    track: &TimelineTrackId,
    clip: &TimelineClipTiming,
    timeline_duration: TimelineTick,
    clip_ids: &mut BTreeSet<TimelineClipId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !clip_ids.insert(clip.id.clone()) {
        diagnostics.push(Diagnostic::error(
            "timeline.duplicate_clip_id",
            format!("Timeline clip ID `{}` is duplicated", clip.id),
        ));
    }
    if clip.start_tick < TimelineTick::ZERO
        || clip.start_tick >= clip.end_tick
        || clip.end_tick > timeline_duration
    {
        diagnostics.push(Diagnostic::error(
            "timeline.invalid_clip_range",
            format!(
                "Timeline track `{track}` clip `{}` interval [{}, {}) is outside [0, {}]",
                clip.id, clip.start_tick, clip.end_tick, timeline_duration
            ),
        ));
    }
    let duration = clip.duration_ticks();
    if clip.blend.in_ticks < TimelineTick::ZERO
        || clip.blend.out_ticks < TimelineTick::ZERO
        || clip.blend.in_ticks.get().saturating_add(clip.blend.out_ticks.get()) > duration
    {
        diagnostics.push(Diagnostic::error(
            "timeline.invalid_clip_blend",
            format!(
                "Timeline clip `{}` blend durations do not fit its {} tick interval",
                clip.id, duration
            ),
        ));
    }
}

fn warn_camera_cut_priority_ties(
    track: &TimelineTrack,
    clips: &[TimelineCameraCutClip],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut ordered = clips.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|clip| (clip.timing.start_tick, clip.timing.id.clone()));
    for (index, left) in ordered.iter().enumerate() {
        for right in ordered.iter().skip(index + 1) {
            if right.timing.start_tick >= left.timing.end_tick {
                break;
            }
            if left.override_priority == right.override_priority {
                diagnostics.push(Diagnostic::warning(
                    "timeline.camera_override_priority_tie",
                    format!(
                        "Camera Cut track `{}` clips `{}` and `{}` overlap with override priority {}; compiled order remains deterministic",
                        track.id, left.timing.id, right.timing.id, left.override_priority
                    ),
                ));
            }
        }
    }
}

fn validate_vec3_keys(keys: &[TimelineVec3Key], clip: &TimelineClipTiming) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if keys.is_empty() {
        diagnostics.push(Diagnostic::error(
            "timeline.empty_transform_curve",
            format!("Transform clip `{}` has no curve keys", clip.id),
        ));
        return diagnostics;
    }
    let max_tick = TimelineTick::new(clip.duration_ticks());
    let mut previous = None;
    for key in keys {
        if key.tick < TimelineTick::ZERO || key.tick > max_tick {
            diagnostics.push(Diagnostic::error(
                "timeline.transform_key_out_of_range",
                format!(
                    "Transform clip `{}` key tick {} is outside [0, {}]",
                    clip.id, key.tick, max_tick
                ),
            ));
        }
        if previous.is_some_and(|tick| key.tick <= tick) {
            diagnostics.push(Diagnostic::error(
                "timeline.transform_keys_not_strictly_ordered",
                format!("Transform clip `{}` keys must be strictly increasing", clip.id),
            ));
        }
        if key.value.iter().any(|component| !component.is_finite()) {
            diagnostics.push(Diagnostic::error(
                "timeline.non_finite_transform_value",
                format!("Transform clip `{}` contains a non-finite Vec3 value", clip.id),
            ));
        }
        previous = Some(key.tick);
    }
    diagnostics
}

fn validate_quat_keys(keys: &[TimelineQuatKey], clip: &TimelineClipTiming) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if keys.is_empty() {
        diagnostics.push(Diagnostic::error(
            "timeline.empty_transform_curve",
            format!("Transform clip `{}` has no rotation keys", clip.id),
        ));
        return diagnostics;
    }
    let max_tick = TimelineTick::new(clip.duration_ticks());
    let mut previous = None;
    for key in keys {
        if key.tick < TimelineTick::ZERO || key.tick > max_tick {
            diagnostics.push(Diagnostic::error(
                "timeline.transform_key_out_of_range",
                format!(
                    "Transform clip `{}` rotation key tick {} is outside [0, {}]",
                    clip.id, key.tick, max_tick
                ),
            ));
        }
        if previous.is_some_and(|tick| key.tick <= tick) {
            diagnostics.push(Diagnostic::error(
                "timeline.transform_keys_not_strictly_ordered",
                format!("Transform clip `{}` rotation keys must be strictly increasing", clip.id),
            ));
        }
        if key.value.iter().any(|component| !component.is_finite()) {
            diagnostics.push(Diagnostic::error(
                "timeline.non_finite_transform_value",
                format!("Transform clip `{}` contains a non-finite quaternion", clip.id),
            ));
        } else {
            let length_squared = key.value.iter().map(|value| value * value).sum::<f32>();
            if length_squared <= f32::EPSILON {
                diagnostics.push(Diagnostic::error(
                    "timeline.zero_rotation_quaternion",
                    format!("Transform clip `{}` contains a zero quaternion", clip.id),
                ));
            }
        }
        previous = Some(key.tick);
    }
    diagnostics
}

fn sample_vec3(keys: &[TimelineVec3Key], tick: TimelineTick) -> Option<[f32; 3]> {
    let first = keys.first()?;
    if tick <= first.tick || keys.len() == 1 {
        return Some(first.value);
    }
    let last = keys.last()?;
    if tick >= last.tick {
        return Some(last.value);
    }
    let right = keys.partition_point(|key| key.tick <= tick);
    let left_key = &keys[right - 1];
    let right_key = &keys[right];
    let factor = interpolation_factor(left_key.tick, right_key.tick, tick);
    Some([
        lerp(left_key.value[0], right_key.value[0], factor),
        lerp(left_key.value[1], right_key.value[1], factor),
        lerp(left_key.value[2], right_key.value[2], factor),
    ])
}

fn sample_quat(keys: &[TimelineQuatKey], tick: TimelineTick) -> Option<[f32; 4]> {
    let first = keys.first()?;
    if tick <= first.tick || keys.len() == 1 {
        return normalize_quat(first.value);
    }
    let last = keys.last()?;
    if tick >= last.tick {
        return normalize_quat(last.value);
    }
    let right = keys.partition_point(|key| key.tick <= tick);
    let left_key = &keys[right - 1];
    let right_key = &keys[right];
    let factor = interpolation_factor(left_key.tick, right_key.tick, tick);
    let mut right_value = right_key.value;
    let dot = left_key
        .value
        .iter()
        .zip(right_value.iter())
        .map(|(left, right)| left * right)
        .sum::<f32>();
    if dot < 0.0 {
        for value in &mut right_value {
            *value = -*value;
        }
    }
    normalize_quat([
        lerp(left_key.value[0], right_value[0], factor),
        lerp(left_key.value[1], right_value[1], factor),
        lerp(left_key.value[2], right_value[2], factor),
        lerp(left_key.value[3], right_value[3], factor),
    ])
}

fn interpolation_factor(left: TimelineTick, right: TimelineTick, tick: TimelineTick) -> f32 {
    let span = right.get().saturating_sub(left.get());
    if span <= 0 {
        return 0.0;
    }
    let offset = tick.get().saturating_sub(left.get());
    (offset as f64 / span as f64).clamp(0.0, 1.0) as f32
}

fn lerp(left: f32, right: f32, factor: f32) -> f32 {
    left + (right - left) * factor
}

fn normalize_quat(value: [f32; 4]) -> Option<[f32; 4]> {
    let length_squared = value.iter().map(|component| component * component).sum::<f32>();
    if !length_squared.is_finite() || length_squared <= f32::EPSILON {
        return None;
    }
    let inverse = length_squared.sqrt().recip();
    Some([
        value[0] * inverse,
        value[1] * inverse,
        value[2] * inverse,
        value[3] * inverse,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(start: i64, end: i64) -> TimelineClipTiming {
        TimelineClipTiming {
            id: TimelineClipId::generate(),
            start_tick: TimelineTick::new(start),
            end_tick: TimelineTick::new(end),
            blend: TimelineBlend::default(),
        }
    }

    fn base_document() -> TimelineDocument {
        TimelineDocument::new(
            TimelineId::generate(),
            "intro",
            TimelineTick::new(48_000 * 10),
        )
    }

    #[test]
    fn json_roundtrip_preserves_exact_ticks_and_stable_ids() {
        let mut document = base_document();
        let track = TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "events".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::Event {
                markers: vec![TimelineEventMarker {
                    id: TimelineMarkerId::generate(),
                    tick: TimelineTick::new(144_001),
                    name: "cutscene.open_door".into(),
                }],
            },
        };
        let marker_id = match &track.kind {
            TimelineTrackKind::Event { markers } => markers[0].id.clone(),
            _ => unreachable!(),
        };
        let track_id = track.id.clone();
        let document_id = document.id.clone();
        document.tracks.push(track);

        let json = document.to_canonical_json().expect("serialize");
        let loaded = TimelineDocument::from_json(&json).expect("load");
        assert_eq!(loaded.id, document_id);
        assert_eq!(loaded.tracks[0].id, track_id);
        match &loaded.tracks[0].kind {
            TimelineTrackKind::Event { markers } => {
                assert_eq!(markers[0].id, marker_id);
                assert_eq!(markers[0].tick, TimelineTick::new(144_001));
            }
            _ => panic!("expected event track"),
        }
    }

    #[test]
    fn compile_order_uses_track_order_then_stable_ties() {
        let mut document = base_document();
        let later_id = TimelineTrackId::generate();
        let earlier_id = TimelineTrackId::generate();
        let (first_id, second_id) = if earlier_id < later_id {
            (earlier_id, later_id)
        } else {
            (later_id, earlier_id)
        };
        document.tracks.push(TimelineTrack {
            id: second_id.clone(),
            name: "second".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::Event {
                markers: vec![TimelineEventMarker {
                    id: TimelineMarkerId::generate(),
                    tick: TimelineTick::new(100),
                    name: "second".into(),
                }],
            },
        });
        document.tracks.push(TimelineTrack {
            id: first_id.clone(),
            name: "first".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::Event {
                markers: vec![TimelineEventMarker {
                    id: TimelineMarkerId::generate(),
                    tick: TimelineTick::new(100),
                    name: "first".into(),
                }],
            },
        });

        let compiled = compile_timeline(&document).expect("warning-only order tie compiles");
        match compiled.entries()[0].payload() {
            CompiledTimelinePayload::Event(payload) => assert_eq!(payload.track, first_id),
            _ => panic!("expected event payload"),
        }
    }

    #[test]
    fn animation_track_compiles_stable_set_and_motion_slot_without_runtime_handle() {
        let mut document = base_document();
        let animation_set = AssetId::generate();
        let motion_slot = MotionSlotId::generate();
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "animation".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::Animation {
                clips: vec![TimelineAnimationClip {
                    timing: timing(0, 48_000),
                    target: EntityId::generate(),
                    animation_set: animation_set.clone(),
                    motion_slot: motion_slot.clone(),
                    source_start_tick: TimelineTick::new(12_000),
                }],
            },
        });

        let compiled = compile_timeline(&document).expect("compile");
        match compiled.entries()[0].payload() {
            CompiledTimelinePayload::Animation(payload) => {
                assert_eq!(payload.animation_set, animation_set);
                assert_eq!(payload.motion_slot, motion_slot);
                assert_eq!(payload.source_tick(TimelineTick::new(24_000)), TimelineTick::new(36_000));
            }
            _ => panic!("expected animation payload"),
        }
    }

    #[test]
    fn transform_curve_sampling_is_stateless_and_typed() {
        let curve = TimelineTransformCurve::Translation(vec![
            TimelineVec3Key {
                tick: TimelineTick::ZERO,
                value: [0.0, 0.0, 0.0],
            },
            TimelineVec3Key {
                tick: TimelineTick::new(48_000),
                value: [10.0, 20.0, 30.0],
            },
        ]);
        assert_eq!(
            curve.sample(TimelineTick::new(24_000)),
            Some(TimelineTransformSample::Translation([5.0, 10.0, 15.0]))
        );
    }

    #[test]
    fn missing_binding_is_reported_by_stable_id_without_name_retarget() {
        struct Missing;
        impl TimelineBindingResolver for Missing {
            fn entity_exists(&self, _entity: &EntityId) -> bool {
                false
            }
            fn asset_exists(&self, _asset: &AssetId) -> bool {
                false
            }
        }

        let mut document = base_document();
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "camera".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::CameraCut {
                clips: vec![TimelineCameraCutClip {
                    timing: timing(0, 10),
                    camera: EntityId::generate(),
                    override_priority: 10,
                }],
            },
        });
        let diagnostics = document.validate_bindings(&Missing);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "timeline.missing_entity_binding");
    }

    #[test]
    fn invalid_transform_key_order_blocks_compile() {
        let mut document = base_document();
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "transform".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::TransformProperty {
                clips: vec![TimelineTransformClip {
                    timing: timing(0, 100),
                    target: EntityId::generate(),
                    curve: TimelineTransformCurve::Scale(vec![
                        TimelineVec3Key {
                            tick: TimelineTick::new(10),
                            value: [1.0; 3],
                        },
                        TimelineVec3Key {
                            tick: TimelineTick::new(10),
                            value: [2.0; 3],
                        },
                    ]),
                }],
            },
        });
        let error = compile_timeline(&document).expect_err("duplicate key tick must fail");
        let diagnostics = error.diagnostics().expect("validation diagnostics");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "timeline.transform_keys_not_strictly_ordered"
        }));
    }
}
