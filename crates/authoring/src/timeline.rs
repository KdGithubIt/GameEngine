//! Timeline / Sequencer authoring document and tick math (ADR 0126).
//!
//! A Timeline is a versioned authoring asset with stable sub-object identity.
//! Persisted time is integer ticks, never floating-point seconds, so clip and
//! marker boundaries stay exact across long sequences and edge comparisons.
//! Runtime entity handles, animation handles, audio voices, GPU handles, and
//! Editor selection state are never part of this document.

use crate::{AssetId, EntityId, MotionSlotId, StableId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Persisted Timeline document schema version.
pub const TIMELINE_SCHEMA_VERSION: u32 = 1;

/// Canonical authoring and evaluation tick rate.
///
/// One second is exactly this many ticks. The value divides the common audio
/// and frame rates without remainder, so clip edges authored in frames or in
/// audio samples land on exact integers.
pub const TIMELINE_TICKS_PER_SECOND: i64 = 48_000;

macro_rules! stable_timeline_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable persisted identifier with prefix `", $prefix, "_`.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a new opaque stable identifier.
            pub fn generate() -> Self {
                Self(format!(concat!($prefix, "_{}"), ulid::Ulid::new()))
            }

            /// Parses and validates a persisted identifier.
            pub fn parse(value: impl Into<String>) -> Result<Self, TimelineIdError> {
                let value = value.into();
                let Some(suffix) = value.strip_prefix(concat!($prefix, "_")) else {
                    return Err(TimelineIdError::WrongPrefix(value));
                };
                if ulid::Ulid::from_string(suffix).is_err() {
                    return Err(TimelineIdError::InvalidUlid(value));
                }
                Ok(Self(value))
            }

            /// Returns the persisted opaque identifier.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

/// Stable Timeline identifier validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineIdError {
    /// The identifier did not use the expected typed prefix.
    WrongPrefix(String),
    /// The suffix after the typed prefix was not a valid ULID.
    InvalidUlid(String),
}

impl fmt::Display for TimelineIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix(value) => {
                write!(
                    formatter,
                    "`{value}` does not use the expected typed prefix"
                )
            }
            Self::InvalidUlid(value) => {
                write!(formatter, "`{value}` does not end with a valid ULID")
            }
        }
    }
}

impl std::error::Error for TimelineIdError {}

stable_timeline_id!(TimelineId, "timeline");
stable_timeline_id!(TimelineTrackId, "timeline_track");
stable_timeline_id!(TimelineClipId, "timeline_clip");
stable_timeline_id!(TimelineMarkerId, "timeline_marker");

/// Integer timeline time in [`TIMELINE_TICKS_PER_SECOND`] units.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TimelineTick(pub i64);

impl TimelineTick {
    /// The zero tick.
    pub const ZERO: Self = Self(0);

    /// Returns the raw tick count.
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Converts seconds to the nearest tick.
    ///
    /// Authoring stores ticks, so this conversion happens once at the boundary
    /// where a human types seconds, never repeatedly during playback.
    pub fn from_seconds(seconds: f64) -> Self {
        if !seconds.is_finite() {
            return Self::ZERO;
        }
        Self((seconds * TIMELINE_TICKS_PER_SECOND as f64).round() as i64)
    }

    /// Converts this tick to seconds for display.
    pub fn as_seconds(self) -> f64 {
        self.0 as f64 / TIMELINE_TICKS_PER_SECOND as f64
    }

    /// Adds a tick offset, saturating rather than wrapping.
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtracts a tick offset, saturating rather than wrapping.
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl fmt::Display for TimelineTick {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Rational display frame rate used for Editor snapping and timecode.
///
/// This is presentation only. Persisted clip and marker time is always ticks,
/// so changing the display rate never rewrites authored time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayFrameRate {
    /// Frames per second numerator, for example 30000 for 29.97 fps.
    pub numerator: u32,
    /// Frames per second denominator, for example 1001 for 29.97 fps.
    pub denominator: u32,
}

impl Default for DisplayFrameRate {
    fn default() -> Self {
        Self {
            numerator: 60,
            denominator: 1,
        }
    }
}

impl DisplayFrameRate {
    /// Whether this rate can be used for snapping and timecode.
    pub fn is_valid(self) -> bool {
        self.numerator > 0 && self.denominator > 0
    }

    /// Ticks covered by one display frame, rounded to the nearest tick.
    ///
    /// Non-integer rates such as 30000/1001 do not divide the tick rate evenly.
    /// Snapping therefore rounds per frame index rather than accumulating a
    /// per-frame remainder, which keeps frame N at the same tick regardless of
    /// how the playhead reached it.
    pub fn tick_of_frame(self, frame: i64) -> Option<TimelineTick> {
        if !self.is_valid() {
            return None;
        }
        let numerator = i128::from(TIMELINE_TICKS_PER_SECOND) * i128::from(frame);
        let denominator = i128::from(self.numerator);
        let scaled = numerator * i128::from(self.denominator);
        let ticks = div_round_nearest(scaled, denominator);
        i64::try_from(ticks).ok().map(TimelineTick)
    }

    /// Frame index containing one tick.
    pub fn frame_of_tick(self, tick: TimelineTick) -> Option<i64> {
        if !self.is_valid() {
            return None;
        }
        let numerator = i128::from(tick.get()) * i128::from(self.numerator);
        let denominator = i128::from(TIMELINE_TICKS_PER_SECOND) * i128::from(self.denominator);
        i64::try_from(numerator.div_euclid(denominator)).ok()
    }

    /// Snaps one tick to the nearest display frame boundary.
    pub fn snap(self, tick: TimelineTick) -> Option<TimelineTick> {
        let frame = self.frame_of_tick(tick)?;
        let current = self.tick_of_frame(frame)?;
        let next = self.tick_of_frame(frame + 1)?;
        let to_current = tick.get() - current.get();
        let to_next = next.get() - tick.get();
        Some(if to_next < to_current { next } else { current })
    }
}

fn div_round_nearest(numerator: i128, denominator: i128) -> i128 {
    if denominator == 0 {
        return 0;
    }
    let half = denominator / 2;
    if (numerator >= 0) == (denominator > 0) {
        (numerator + half) / denominator
    } else {
        (numerator - half) / denominator
    }
}

/// Stable track type identity used by the track registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineTrackKind {
    /// Emits sequence-level events for project gameplay, UI, and scene flow.
    Event,
    /// Selects an authored game camera for a clip interval.
    CameraCut,
    /// Plays an Animation Set motion slot on a bound entity.
    Animation,
    /// Animates one explicitly supported typed property with a curve.
    Property,
    /// Starts, stops, or fades an audio cue.
    Audio,
    /// Starts, stops, or restarts a VFX effect.
    Vfx,
}

impl TimelineTrackKind {
    /// Every track kind the first Timeline release defines.
    pub const ALL: [Self; 6] = [
        Self::Event,
        Self::CameraCut,
        Self::Animation,
        Self::Property,
        Self::Audio,
        Self::Vfx,
    ];

    /// Stable registry identifier persisted in track records.
    pub const fn type_id(self) -> &'static str {
        match self {
            Self::Event => "engine.timeline.event",
            Self::CameraCut => "engine.timeline.camera_cut",
            Self::Animation => "engine.timeline.animation",
            Self::Property => "engine.timeline.property",
            Self::Audio => "engine.timeline.audio",
            Self::Vfx => "engine.timeline.vfx",
        }
    }

    /// Human-readable label used by Editor presentation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Event => "Event",
            Self::CameraCut => "Camera Cut",
            Self::Animation => "Animation",
            Self::Property => "Property",
            Self::Audio => "Audio",
            Self::Vfx => "VFX",
        }
    }

    /// Resolves a kind from its stable registry identifier.
    pub fn from_type_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.type_id() == value)
    }
}

/// Stable binding from a track to authoring identity.
///
/// Bindings never carry display names: renaming a camera, entity, or asset
/// keeps a valid binding, and deleting the target produces a diagnostic instead
/// of a silent same-name retarget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineBinding {
    /// Bound authoring entity, when the track targets a scene entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityId>,
    /// Bound authoring asset, when the track targets an asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetId>,
}

impl TimelineBinding {
    /// Whether this binding names nothing at all.
    pub fn is_empty(&self) -> bool {
        self.entity.is_none() && self.asset.is_none()
    }
}

/// Explicitly supported animatable property.
///
/// Timeline property tracks animate this closed set. Arbitrary reflection into
/// component memory is deliberately not offered; a new animatable property is a
/// deliberate addition here plus an adapter that knows how to apply it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineProperty {
    /// Local translation on the X axis.
    TranslationX,
    /// Local translation on the Y axis.
    TranslationY,
    /// Local translation on the Z axis.
    TranslationZ,
    /// Local rotation around the X axis in degrees.
    RotationX,
    /// Local rotation around the Y axis in degrees.
    RotationY,
    /// Local rotation around the Z axis in degrees.
    RotationZ,
    /// Local scale on the X axis.
    ScaleX,
    /// Local scale on the Y axis.
    ScaleY,
    /// Local scale on the Z axis.
    ScaleZ,
}

/// How one curve segment interpolates to the next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineInterpolation {
    /// Holds the key value until the next key.
    Step,
    /// Interpolates linearly to the next key.
    #[default]
    Linear,
    /// Interpolates with smooth ease-in and ease-out.
    Smooth,
}

/// One authored curve key.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimelineKey {
    /// Key time relative to the clip start.
    pub tick: TimelineTick,
    /// Authored value at this key.
    pub value: f32,
    /// Interpolation from this key to the next.
    #[serde(default)]
    pub interpolation: TimelineInterpolation,
}

/// Audio clip action a track performs at its boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineAudioAction {
    /// Starts the cue at the clip start and stops it at the clip end.
    Play,
    /// Stops the cue at the clip start.
    Stop,
}

/// VFX clip action a track performs at its boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineVfxAction {
    /// Plays the effect for the clip interval.
    Play,
    /// Stops the effect at the clip start.
    Stop,
    /// Restarts the effect at the clip start.
    Restart,
}

/// Typed clip payload owned by the track's domain.
///
/// The payload variant and the track kind must agree, which keeps a track from
/// reaching into another domain through an untyped blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "payload", rename_all = "snake_case")]
pub enum TimelineClipPayload {
    /// Emits a stable sequence event while the clip interval is entered.
    Event {
        /// Stable project-visible event name.
        event: String,
    },
    /// Overrides camera selection for the clip interval.
    CameraCut {
        /// Authoring entity carrying the target camera.
        camera: EntityId,
    },
    /// Plays one Animation Set motion slot.
    Animation {
        /// Motion slot name inside the bound Animation Set.
        motion_slot: String,
        /// Playback rate multiplier.
        #[serde(default = "default_speed")]
        speed: f32,
        /// Whether the motion loops inside the clip interval.
        #[serde(default)]
        looping: bool,
    },
    /// Animates one supported typed property with a curve.
    Property {
        /// Property the curve drives.
        property: TimelineProperty,
        /// Curve keys relative to the clip start.
        keys: Vec<TimelineKey>,
    },
    /// Starts or stops an audio cue.
    Audio {
        /// Audio cue asset.
        cue: AssetId,
        /// Action performed at the clip boundaries.
        action: TimelineAudioAction,
        /// Fade duration applied at the clip boundaries.
        #[serde(default)]
        fade_ticks: TimelineTick,
    },
    /// Starts, stops, or restarts a VFX effect.
    Vfx {
        /// VFX effect asset.
        effect: AssetId,
        /// Action performed at the clip start.
        action: TimelineVfxAction,
    },
}

fn default_speed() -> f32 {
    1.0
}

impl TimelineClipPayload {
    /// Track kind this payload belongs to.
    pub const fn kind(&self) -> TimelineTrackKind {
        match self {
            Self::Event { .. } => TimelineTrackKind::Event,
            Self::CameraCut { .. } => TimelineTrackKind::CameraCut,
            Self::Animation { .. } => TimelineTrackKind::Animation,
            Self::Property { .. } => TimelineTrackKind::Property,
            Self::Audio { .. } => TimelineTrackKind::Audio,
            Self::Vfx { .. } => TimelineTrackKind::Vfx,
        }
    }
}

/// One authored clip on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineClip {
    /// Stable clip identity, preserved across moves and trims.
    pub id: TimelineClipId,
    /// Inclusive clip start.
    pub start: TimelineTick,
    /// Exclusive clip end.
    pub end: TimelineTick,
    /// Typed domain payload.
    #[serde(flatten)]
    pub payload: TimelineClipPayload,
}

impl TimelineClip {
    /// Clip length in ticks.
    pub fn duration(&self) -> TimelineTick {
        self.end.saturating_sub(self.start)
    }

    /// Whether one tick lies inside the half-open clip interval.
    pub fn contains(&self, tick: TimelineTick) -> bool {
        tick >= self.start && tick < self.end
    }
}

/// One authored track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineTrack {
    /// Stable track identity, preserved across renames and reordering.
    pub id: TimelineTrackId,
    /// Registry track kind.
    pub kind: TimelineTrackKind,
    /// Display name; never used as identity.
    pub name: String,
    /// Whether the track contributes to evaluation.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Stable binding to authoring identity.
    #[serde(default)]
    pub binding: TimelineBinding,
    /// Clips in authored order.
    #[serde(default)]
    pub clips: Vec<TimelineClip>,
}

fn default_true() -> bool {
    true
}

/// One sequence-level marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineMarker {
    /// Stable marker identity.
    pub id: TimelineMarkerId,
    /// Marker time.
    pub tick: TimelineTick,
    /// Display name; never used as identity.
    pub name: String,
    /// Stable event emitted when the playhead crosses this marker.
    pub event: String,
}

/// A versioned Timeline authoring document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineDocument {
    /// Persisted schema version.
    pub schema_version: u32,
    /// Stable document identity.
    pub id: TimelineId,
    /// Display frame rate used for Editor snapping and timecode.
    #[serde(default)]
    pub display_frame_rate: DisplayFrameRate,
    /// Authored sequence duration.
    pub duration: TimelineTick,
    /// Tracks in authored order; order is presentation and tie-break order.
    #[serde(default)]
    pub tracks: Vec<TimelineTrack>,
    /// Sequence-level markers.
    #[serde(default)]
    pub markers: Vec<TimelineMarker>,
}

impl TimelineDocument {
    /// Creates an empty Timeline of the given duration.
    pub fn new(duration: TimelineTick) -> Self {
        Self {
            schema_version: TIMELINE_SCHEMA_VERSION,
            id: TimelineId::generate(),
            display_frame_rate: DisplayFrameRate::default(),
            duration,
            tracks: Vec::new(),
            markers: Vec::new(),
        }
    }

    /// Parses a persisted Timeline document.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes canonical human-readable JSON with a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Resolves one track by stable identity.
    pub fn track(&self, id: &TimelineTrackId) -> Option<&TimelineTrack> {
        self.tracks.iter().find(|track| &track.id == id)
    }

    /// Resolves one mutable track by stable identity.
    pub fn track_mut(&mut self, id: &TimelineTrackId) -> Option<&mut TimelineTrack> {
        self.tracks.iter_mut().find(|track| &track.id == id)
    }

    /// Validates the persisted Timeline contract.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != TIMELINE_SCHEMA_VERSION {
            errors.push(format!(
                "unsupported Timeline schema version {} (expected {TIMELINE_SCHEMA_VERSION})",
                self.schema_version
            ));
        }
        if !self.display_frame_rate.is_valid() {
            errors.push("display frame rate numerator and denominator must be positive".to_owned());
        }
        if self.duration.get() < 0 {
            errors.push("Timeline duration must not be negative".to_owned());
        }

        let mut track_ids = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        for track in &self.tracks {
            if !track_ids.insert(track.id.clone()) {
                errors.push(format!("duplicate TimelineTrackId `{}`", track.id.as_str()));
            }
            if track.name.trim().is_empty() {
                errors.push(format!(
                    "track `{}` must carry a non-empty display name",
                    track.id.as_str()
                ));
            }
            let mut sorted = track.clips.clone();
            sorted.sort_by_key(|clip| (clip.start, clip.end));
            let mut previous_end: Option<TimelineTick> = None;
            for clip in &sorted {
                if !clip_ids.insert(clip.id.clone()) {
                    errors.push(format!("duplicate TimelineClipId `{}`", clip.id.as_str()));
                }
                if clip.start.get() < 0 {
                    errors.push(format!(
                        "clip `{}` starts before tick zero",
                        clip.id.as_str()
                    ));
                }
                if clip.end <= clip.start {
                    errors.push(format!(
                        "clip `{}` must end after it starts",
                        clip.id.as_str()
                    ));
                }
                if clip.payload.kind() != track.kind {
                    errors.push(format!(
                        "clip `{}` carries a {} payload on a {} track",
                        clip.id.as_str(),
                        clip.payload.kind().label(),
                        track.kind.label()
                    ));
                }
                if let Some(end) = previous_end
                    && clip.start < end
                {
                    errors.push(format!(
                        "clip `{}` overlaps an earlier clip on track `{}`",
                        clip.id.as_str(),
                        track.id.as_str()
                    ));
                }
                previous_end = Some(clip.end);
                if clip.end > self.duration {
                    errors.push(format!(
                        "clip `{}` ends after the Timeline duration",
                        clip.id.as_str()
                    ));
                }
                errors.extend(validate_payload(clip));
            }
            errors.extend(validate_binding(track));
        }

        let mut marker_ids = BTreeSet::new();
        for marker in &self.markers {
            if !marker_ids.insert(marker.id.clone()) {
                errors.push(format!(
                    "duplicate TimelineMarkerId `{}`",
                    marker.id.as_str()
                ));
            }
            if marker.tick.get() < 0 || marker.tick > self.duration {
                errors.push(format!(
                    "marker `{}` lies outside the Timeline duration",
                    marker.id.as_str()
                ));
            }
            if marker.event.trim().is_empty() {
                errors.push(format!(
                    "marker `{}` must carry a non-empty event name",
                    marker.id.as_str()
                ));
            }
        }
        errors
    }
}

fn validate_binding(track: &TimelineTrack) -> Vec<String> {
    let mut errors = Vec::new();
    let requires_entity = matches!(
        track.kind,
        TimelineTrackKind::Animation | TimelineTrackKind::Property | TimelineTrackKind::Vfx
    );
    if requires_entity && track.binding.entity.is_none() && !track.clips.is_empty() {
        errors.push(format!(
            "track `{}` must bind an entity before it carries {} clips",
            track.id.as_str(),
            track.kind.label()
        ));
    }
    if track.kind == TimelineTrackKind::Animation
        && track.binding.asset.is_none()
        && !track.clips.is_empty()
    {
        errors.push(format!(
            "track `{}` must bind an Animation Set asset before it carries Animation clips",
            track.id.as_str()
        ));
    }
    errors
}

fn validate_payload(clip: &TimelineClip) -> Vec<String> {
    let mut errors = Vec::new();
    match &clip.payload {
        TimelineClipPayload::Event { event } => {
            if event.trim().is_empty() {
                errors.push(format!(
                    "clip `{}` must carry a non-empty event name",
                    clip.id.as_str()
                ));
            }
        }
        TimelineClipPayload::Animation {
            motion_slot, speed, ..
        } => {
            if MotionSlotId::from_stable_id(StableId::new(motion_slot.trim())).is_err() {
                errors.push(format!(
                    "clip `{}` motion slot must be a stable motion_<ULID> identifier",
                    clip.id.as_str()
                ));
            }
            if !speed.is_finite() || *speed <= 0.0 {
                errors.push(format!(
                    "clip `{}` playback speed must be finite and positive",
                    clip.id.as_str()
                ));
            }
        }
        TimelineClipPayload::Property { keys, .. } => {
            if keys.is_empty() {
                errors.push(format!(
                    "clip `{}` must carry at least one curve key",
                    clip.id.as_str()
                ));
            }
            let mut previous: Option<TimelineTick> = None;
            for key in keys {
                if !key.value.is_finite() {
                    errors.push(format!(
                        "clip `{}` carries a non-finite curve value",
                        clip.id.as_str()
                    ));
                }
                if key.tick.get() < 0 || key.tick > clip.duration() {
                    errors.push(format!(
                        "clip `{}` carries a curve key outside the clip interval",
                        clip.id.as_str()
                    ));
                }
                if let Some(previous) = previous
                    && key.tick <= previous
                {
                    errors.push(format!(
                        "clip `{}` curve keys must be strictly increasing",
                        clip.id.as_str()
                    ));
                }
                previous = Some(key.tick);
            }
        }
        TimelineClipPayload::Audio { fade_ticks, .. } => {
            if fade_ticks.get() < 0 {
                errors.push(format!(
                    "clip `{}` fade duration must not be negative",
                    clip.id.as_str()
                ));
            }
        }
        TimelineClipPayload::CameraCut { .. } | TimelineClipPayload::Vfx { .. } => {}
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn property_clip(start: i64, end: i64) -> TimelineClip {
        TimelineClip {
            id: TimelineClipId::generate(),
            start: TimelineTick(start),
            end: TimelineTick(end),
            payload: TimelineClipPayload::Property {
                property: TimelineProperty::TranslationX,
                keys: vec![TimelineKey {
                    tick: TimelineTick::ZERO,
                    value: 0.0,
                    interpolation: TimelineInterpolation::Linear,
                }],
            },
        }
    }

    fn property_track(clips: Vec<TimelineClip>) -> TimelineTrack {
        TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Property,
            name: "Move".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(EntityId::generate()),
                asset: None,
            },
            clips,
        }
    }

    #[test]
    fn one_second_is_an_exact_tick_count_in_both_directions() {
        assert_eq!(TimelineTick::from_seconds(1.0).get(), 48_000);
        assert_eq!(TimelineTick::from_seconds(0.5).get(), 24_000);
        assert_eq!(TimelineTick(48_000).as_seconds(), 1.0);
        // A long sequence keeps exact boundaries where float seconds would drift.
        assert_eq!(TimelineTick::from_seconds(3_600.0).get(), 172_800_000);
    }

    #[test]
    fn integer_frame_rates_land_on_exact_ticks() {
        let rate = DisplayFrameRate {
            numerator: 30,
            denominator: 1,
        };
        assert_eq!(rate.tick_of_frame(1), Some(TimelineTick(1_600)));
        assert_eq!(rate.tick_of_frame(30), Some(TimelineTick(48_000)));
        assert_eq!(rate.frame_of_tick(TimelineTick(1_600)), Some(1));
        assert_eq!(rate.frame_of_tick(TimelineTick(1_599)), Some(0));
    }

    #[test]
    fn a_drop_frame_rate_snaps_without_accumulating_a_remainder() {
        let rate = DisplayFrameRate {
            numerator: 30_000,
            denominator: 1_001,
        };
        let frame_100 = rate.tick_of_frame(100).expect("frame tick");
        // 100 frames at 29.97 fps is 3.337 seconds; the tick is rounded once,
        // from the frame index, rather than accumulated per frame.
        assert_eq!(frame_100, TimelineTick(160_160));
        assert_eq!(rate.tick_of_frame(100), Some(frame_100));
        let snapped = rate.snap(TimelineTick(160_100)).expect("snap");
        assert_eq!(snapped, frame_100);
    }

    #[test]
    fn snapping_prefers_the_nearer_frame_boundary() {
        let rate = DisplayFrameRate {
            numerator: 60,
            denominator: 1,
        };
        assert_eq!(rate.snap(TimelineTick(0)), Some(TimelineTick(0)));
        assert_eq!(rate.snap(TimelineTick(399)), Some(TimelineTick(0)));
        assert_eq!(rate.snap(TimelineTick(401)), Some(TimelineTick(800)));
        assert_eq!(rate.snap(TimelineTick(1)), Some(TimelineTick(0)));
    }

    #[test]
    fn a_valid_document_reports_no_errors_and_round_trips() {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        document
            .tracks
            .push(property_track(vec![property_clip(0, 1_000)]));
        document.markers.push(TimelineMarker {
            id: TimelineMarkerId::generate(),
            tick: TimelineTick(500),
            name: "Hit".to_owned(),
            event: "cutscene.hit".to_owned(),
        });
        assert!(document.validate().is_empty(), "{:?}", document.validate());

        let json = document.to_canonical_json().expect("canonical json");
        let parsed = TimelineDocument::from_json(&json).expect("round trip");
        assert_eq!(parsed, document);
    }

    #[test]
    fn overlapping_clips_on_one_track_are_rejected() {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        document.tracks.push(property_track(vec![
            property_clip(0, 1_000),
            property_clip(900, 2_000),
        ]));
        assert!(
            document
                .validate()
                .iter()
                .any(|error| error.contains("overlaps an earlier clip"))
        );
    }

    #[test]
    fn a_payload_that_does_not_match_its_track_kind_is_rejected() {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        let mut track = property_track(Vec::new());
        track.kind = TimelineTrackKind::Event;
        track.clips.push(property_clip(0, 1_000));
        document.tracks.push(track);
        assert!(
            document
                .validate()
                .iter()
                .any(|error| error.contains("payload on a Event track"))
        );
    }

    #[test]
    fn a_clip_reaching_past_the_duration_is_rejected() {
        let mut document = TimelineDocument::new(TimelineTick(500));
        document
            .tracks
            .push(property_track(vec![property_clip(0, 1_000)]));
        assert!(
            document
                .validate()
                .iter()
                .any(|error| error.contains("ends after the Timeline duration"))
        );
    }

    #[test]
    fn track_kind_identity_is_stable_across_the_registry_lookup() {
        for kind in TimelineTrackKind::ALL {
            assert_eq!(TimelineTrackKind::from_type_id(kind.type_id()), Some(kind));
        }
        assert_eq!(
            TimelineTrackKind::CameraCut.type_id(),
            "engine.timeline.camera_cut"
        );
        assert_eq!(
            TimelineTrackKind::from_type_id("engine.timeline.unknown"),
            None
        );
    }

    #[test]
    fn animation_tracks_require_a_set_binding_and_stable_motion_slot() {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        let motion_slot = MotionSlotId::generate();
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Animation,
            name: "Motion".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(EntityId::generate()),
                asset: None,
            },
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick(48_000),
                payload: TimelineClipPayload::Animation {
                    motion_slot: motion_slot.as_str().to_owned(),
                    speed: 1.0,
                    looping: false,
                },
            }],
        });
        assert!(
            document
                .validate()
                .iter()
                .any(|error| error.contains("must bind an Animation Set asset"))
        );

        document.tracks[0].binding.asset = Some(AssetId::generate());
        let TimelineClipPayload::Animation { motion_slot, .. } =
            &mut document.tracks[0].clips[0].payload
        else {
            panic!("fixture must remain an Animation clip");
        };
        *motion_slot = "walk".to_owned();
        assert!(
            document
                .validate()
                .iter()
                .any(|error| error.contains("stable motion_<ULID> identifier"))
        );
    }

    #[test]
    fn identifiers_reject_a_foreign_prefix() {
        let track = TimelineTrackId::generate();
        assert!(TimelineTrackId::parse(track.as_str()).is_ok());
        assert!(matches!(
            TimelineClipId::parse(track.as_str()),
            Err(TimelineIdError::WrongPrefix(_))
        ));
    }
}
