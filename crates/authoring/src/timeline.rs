//! Versioned Timeline document, validation, persistence, and deterministic compilation.

use crate::id::{
    AssetId, EntityId, MotionSlotId, TimelineClipId, TimelineId, TimelineMarkerId, TimelineTrackId,
};
use engine_timeline::{CompiledEntry, CompiledTimeline, SeekCapability, TimelineTick};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

/// Current `*.timeline.json` schema.
pub const TIMELINE_SCHEMA_VERSION: u32 = 1;
/// Timeline asset suffix.
pub const TIMELINE_FILE_SUFFIX: &str = ".timeline.json";
/// Maximum UTF-8 byte length of one Timeline Event or marker event name.
pub const MAX_TIMELINE_EVENT_NAME_BYTES: usize = 256;
/// Maximum UTF-8 byte length of one Timeline Event clip payload.
pub const MAX_TIMELINE_EVENT_PAYLOAD_BYTES: usize = 16 * 1024;

/// Display-only frame rate. Canonical storage remains 48 kHz ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineDisplayRate {
    /// Frames numerator.
    pub numerator: u32,
    /// Frames denominator.
    pub denominator: u32,
}
impl Default for TimelineDisplayRate { fn default() -> Self { Self { numerator: 60, denominator: 1 } } }

/// Stable binding resolved by production runtime; display names are never fallbacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineBinding {
    /// Stable scene entity.
    Entity { /// Entity ID.
        entity: EntityId },
    /// Stable asset.
    Asset { /// Asset ID.
        asset: AssetId },
}

/// Value category accepted by one explicitly registered Timeline property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelinePropertyValueKind {
    /// Boolean value.
    Bool,
    /// Scalar number.
    Number,
    /// Three-component vector.
    Vec3,
    /// Quaternion xyzw.
    Quat,
}

/// Property value supported by first-release Transform/Property clips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TimelinePropertyValue {
    /// Boolean.
    Bool(bool),
    /// Scalar number.
    Number(f64),
    /// Three-component vector.
    Vec3([f64; 3]),
    /// Quaternion xyzw.
    Quat([f64; 4]),
}

impl TimelinePropertyValue {
    /// Returns the stable value category used by the property registry.
    pub const fn kind(&self) -> TimelinePropertyValueKind {
        match self {
            Self::Bool(_) => TimelinePropertyValueKind::Bool,
            Self::Number(_) => TimelinePropertyValueKind::Number,
            Self::Vec3(_) => TimelinePropertyValueKind::Vec3,
            Self::Quat(_) => TimelinePropertyValueKind::Quat,
        }
    }

    fn is_valid_numeric_value(&self) -> bool {
        match self {
            Self::Bool(_) => true,
            Self::Number(value) => value.is_finite(),
            Self::Vec3(value) => value.iter().all(|component| component.is_finite()),
            Self::Quat(value) => {
                value.iter().all(|component| component.is_finite())
                    && value.iter().map(|component| component * component).sum::<f64>()
                        > f64::EPSILON
            }
        }
    }
}

/// One explicitly supported Transform/Property target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelinePropertyDescriptor {
    /// Stable property identifier persisted in Timeline clips.
    pub type_id: &'static str,
    /// Editor-facing label.
    pub label: &'static str,
    /// Required value category for both clip endpoints.
    pub value_kind: TimelinePropertyValueKind,
}

/// Canonical first-release property registry. Arbitrary ECS reflection is not supported.
pub const TIMELINE_PROPERTY_REGISTRY: [TimelinePropertyDescriptor; 3] = [
    TimelinePropertyDescriptor {
        type_id: "engine.transform.translation",
        label: "Transform / Translation",
        value_kind: TimelinePropertyValueKind::Vec3,
    },
    TimelinePropertyDescriptor {
        type_id: "engine.transform.rotation",
        label: "Transform / Rotation",
        value_kind: TimelinePropertyValueKind::Quat,
    },
    TimelinePropertyDescriptor {
        type_id: "engine.transform.scale",
        label: "Transform / Scale",
        value_kind: TimelinePropertyValueKind::Vec3,
    },
];

/// Returns the explicit first-release Timeline property registry.
pub const fn timeline_property_registry() -> &'static [TimelinePropertyDescriptor] {
    &TIMELINE_PROPERTY_REGISTRY
}

fn timeline_property_descriptor(type_id: &str) -> Option<&'static TimelinePropertyDescriptor> {
    TIMELINE_PROPERTY_REGISTRY
        .iter()
        .find(|descriptor| descriptor.type_id == type_id)
}

/// First-release typed track family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineTrackKind {
    /// Animation Set motion-slot sampling.
    Animation,
    /// Entity transform or explicitly registered typed property interpolation.
    TransformProperty,
    /// Runtime-only camera selection override.
    CameraCut,
    /// Bounded Timeline gameplay event.
    Event,
    /// Production audio playback request.
    Audio,
    /// Production VFX playback request.
    Vfx,
}
impl TimelineTrackKind {
    /// Explicit discontinuous-time contract.
    pub const fn seek_capability(self) -> SeekCapability {
        match self {
            Self::Animation => SeekCapability::Seekable,
            Self::TransformProperty | Self::CameraCut | Self::Event => SeekCapability::Stateless,
            Self::Audio => SeekCapability::NonSeekable,
            Self::Vfx => SeekCapability::ReplayRequired,
        }
    }
}

/// Shared registry metadata for one Timeline track family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineTrackDescriptor {
    /// Stable type identifier consumed by authoring and adapter surfaces.
    pub type_id: &'static str,
    /// Typed persisted family.
    pub kind: TimelineTrackKind,
    /// Human-readable Editor label.
    pub label: &'static str,
    /// Discontinuous-time policy owned by this family.
    pub seek_capability: SeekCapability,
    /// Whether the first-release Sequencer exposes numeric curve editing.
    pub supports_curves: bool,
}

/// Canonical first-release Timeline track registry.
///
/// Editor menus and external authoring adapters consume this table instead of
/// maintaining independent hard-coded track-family lists.
pub const TIMELINE_TRACK_REGISTRY: [TimelineTrackDescriptor; 6] = [
    TimelineTrackDescriptor { type_id: "engine.timeline.animation", kind: TimelineTrackKind::Animation, label: "Animation", seek_capability: SeekCapability::Seekable, supports_curves: false },
    TimelineTrackDescriptor { type_id: "engine.timeline.transform_property", kind: TimelineTrackKind::TransformProperty, label: "Transform / Property", seek_capability: SeekCapability::Stateless, supports_curves: true },
    TimelineTrackDescriptor { type_id: "engine.timeline.camera_cut", kind: TimelineTrackKind::CameraCut, label: "Camera Cut", seek_capability: SeekCapability::Stateless, supports_curves: false },
    TimelineTrackDescriptor { type_id: "engine.timeline.event", kind: TimelineTrackKind::Event, label: "Event", seek_capability: SeekCapability::Stateless, supports_curves: false },
    TimelineTrackDescriptor { type_id: "engine.timeline.audio", kind: TimelineTrackKind::Audio, label: "Audio", seek_capability: SeekCapability::NonSeekable, supports_curves: false },
    TimelineTrackDescriptor { type_id: "engine.timeline.vfx", kind: TimelineTrackKind::Vfx, label: "VFX", seek_capability: SeekCapability::ReplayRequired, supports_curves: false },
];

/// Returns the canonical Timeline track registry.
pub const fn timeline_track_registry() -> &'static [TimelineTrackDescriptor] {
    &TIMELINE_TRACK_REGISTRY
}

/// Typed payload persisted in one clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineClipPayload {
    /// Animation Set motion slot sampled at local clip time.
    Animation {
        /// Stable motion-slot identifier resolved through the target controller's Animation Set.
        motion_slot: MotionSlotId,
    },
    /// Entity property interpolation.
    TransformProperty {
        /// Stable property ID from [`TIMELINE_PROPERTY_REGISTRY`].
        property: String,
        /// Property value at the start of the clip.
        from: TimelinePropertyValue,
        /// Property value at the end of the clip.
        to: TimelinePropertyValue,
    },
    /// Runtime-only active-camera override.
    CameraCut,
    /// Bounded gameplay event.
    Event {
        /// Stable event name delivered through the host event path.
        name: String,
        /// Bounded serialized event payload.
        payload: String,
    },
    /// Production audio clip request.
    Audio {
        /// Stable audio asset identifier.
        clip: AssetId,
        /// Authored linear gain.
        volume: f32,
        /// Whether the requested voice loops while the clip is active.
        looping: bool,
    },
    /// Production VFX effect request.
    Vfx {
        /// Stable VFX asset identifier.
        effect: AssetId,
        /// Whether the effect loops while the clip is active.
        looping: bool,
    },
}
impl TimelineClipPayload {
    /// Family of this payload.
    pub const fn kind(&self) -> TimelineTrackKind {
        match self {
            Self::Animation { .. } => TimelineTrackKind::Animation,
            Self::TransformProperty { .. } => TimelineTrackKind::TransformProperty,
            Self::CameraCut => TimelineTrackKind::CameraCut,
            Self::Event { .. } => TimelineTrackKind::Event,
            Self::Audio { .. } => TimelineTrackKind::Audio,
            Self::Vfx { .. } => TimelineTrackKind::Vfx,
        }
    }
}

/// Persisted clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineClip {
    /// Stable clip ID.
    pub id: TimelineClipId,
    /// Display name.
    pub name: String,
    /// Inclusive start.
    pub start: TimelineTick,
    /// Positive duration.
    pub duration: TimelineTick,
    /// Source offset for seekable media.
    pub source_offset: TimelineTick,
    /// Typed payload.
    pub payload: TimelineClipPayload,
}
impl TimelineClip { /// Exclusive end.
    pub fn end(&self) -> TimelineTick { self.start.saturating_add(self.duration.get()) } }

/// Persisted track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineTrack {
    /// Stable track ID.
    pub id: TimelineTrackId,
    /// Display name.
    pub name: String,
    /// Typed family.
    pub kind: TimelineTrackKind,
    /// Persisted enabled state; editor mute/solo/lock stay transient.
    pub enabled: bool,
    /// Stable production binding.
    pub binding: Option<TimelineBinding>,
    /// Persisted clips.
    pub clips: Vec<TimelineClip>,
}

/// Persisted marker lane entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMarker {
    /// Stable marker ID.
    pub id: TimelineMarkerId,
    /// Display name.
    pub name: String,
    /// Exact tick.
    pub tick: TimelineTick,
    /// Optional bounded Event-track event name.
    pub event: Option<String>,
}

/// Versioned Timeline source of truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineDocument {
    /// Schema marker.
    pub schema_version: u32,
    /// Stable Timeline ID.
    pub id: TimelineId,
    /// Display name.
    pub name: String,
    /// Inclusive final tick.
    pub duration: TimelineTick,
    /// Display frame rate only.
    pub display_rate: TimelineDisplayRate,
    /// Ordered track list.
    pub tracks: Vec<TimelineTrack>,
    /// Marker lane.
    pub markers: Vec<TimelineMarker>,
}
impl TimelineDocument {
    /// Creates a new empty document.
    pub fn new(name: impl Into<String>, duration: TimelineTick) -> Self { Self { schema_version: TIMELINE_SCHEMA_VERSION, id: TimelineId::generate(), name: name.into(), duration, display_rate: TimelineDisplayRate::default(), tracks: Vec::new(), markers: Vec::new() } }
    /// Validates and compiles an immutable schedule.
    pub fn compile(&self) -> TimelineCompilation { compile_timeline(self) }
}

/// Runtime-neutral typed payload emitted by authoring compile.
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledTimelinePayload {
    /// One clip payload with stable binding and source metadata.
    Clip {
        /// Stable source clip identifier.
        clip_id: TimelineClipId,
        /// Stable owning track identifier.
        track_id: TimelineTrackId,
        /// Stable authoring binding resolved by the production adapter.
        binding: Option<TimelineBinding>,
        /// Persisted clip duration in canonical ticks.
        duration: TimelineTick,
        /// Persisted source offset in canonical ticks.
        source_offset: TimelineTick,
        /// Typed source payload retained for composition-layer dispatch.
        payload: TimelineClipPayload,
    },
    /// Marker event.
    Marker {
        /// Stable source marker identifier.
        marker_id: TimelineMarkerId,
        /// Stable event name emitted for the marker.
        event: String,
    },
}

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TimelineDiagnosticSeverity {
    /// Non-fatal authoring limitation or advisory.
    Warning,
    /// Error that prevents a valid compiled schedule.
    Error,
}
/// Compile/validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineDiagnostic {
    /// Diagnostic severity.
    pub severity: TimelineDiagnosticSeverity,
    /// Stable machine-readable diagnostic code.
    pub code: &'static str,
    /// Stable Timeline sub-object identity associated with the problem.
    pub target: String,
    /// Human-readable diagnostic message.
    pub message: String,
}
/// Compile result; schedule is absent when any error exists.
#[derive(Debug, Clone)]
pub struct TimelineCompilation {
    /// Pure validation/compilation diagnostics.
    pub diagnostics: Vec<TimelineDiagnostic>,
    /// Immutable schedule when no error diagnostic was produced.
    pub schedule: Option<CompiledTimeline<CompiledTimelinePayload>>,
}

/// Validates and compiles deterministic track/clip/marker order.
pub fn compile_timeline(document: &TimelineDocument) -> TimelineCompilation {
    let mut diagnostics = validate_timeline(document);
    if diagnostics.iter().any(|d| d.severity == TimelineDiagnosticSeverity::Error) { return TimelineCompilation { diagnostics, schedule: None }; }
    let mut entries = Vec::new();
    for (track_index, track) in document.tracks.iter().enumerate().filter(|(_, t)| t.enabled) {
        let mut clips = track.clips.iter().collect::<Vec<_>>();
        clips.sort_by(|a,b| (a.start, a.id.as_str()).cmp(&(b.start, b.id.as_str())));
        for (item_index, clip) in clips.into_iter().enumerate() {
            let payload = CompiledTimelinePayload::Clip { clip_id: clip.id.clone(), track_id: track.id.clone(), binding: track.binding.clone(), duration: clip.duration, source_offset: clip.source_offset, payload: clip.payload.clone() };
            entries.push(CompiledEntry::interval(track_index as u32, item_index as u32, clip.start, clip.end(), track.kind.seek_capability(), payload));
        }
    }
    let mut markers = document.markers.iter().filter(|m| m.event.is_some()).collect::<Vec<_>>();
    markers.sort_by(|a,b| (a.tick, a.id.as_str()).cmp(&(b.tick, b.id.as_str())));
    for (i, marker) in markers.into_iter().enumerate() {
        entries.push(CompiledEntry::point(u32::MAX, i as u32, marker.tick, SeekCapability::Stateless, true, CompiledTimelinePayload::Marker { marker_id: marker.id.clone(), event: marker.event.clone().unwrap_or_default() }));
    }
    match CompiledTimeline::new(document.duration, entries) { Ok(schedule) => TimelineCompilation { diagnostics, schedule: Some(schedule) }, Err(error) => { diagnostics.push(error_diag("timeline.compile", document.id.as_str(), error.to_string())); TimelineCompilation { diagnostics, schedule: None } } }
}

/// Pure document validation.
pub fn validate_timeline(document: &TimelineDocument) -> Vec<TimelineDiagnostic> {
    let mut out = Vec::new();
    if document.schema_version != TIMELINE_SCHEMA_VERSION {
        out.push(error_diag(
            "timeline.schema_version",
            document.id.as_str(),
            format!(
                "expected schema {}, got {}",
                TIMELINE_SCHEMA_VERSION, document.schema_version
            ),
        ));
    }
    if document.duration < TimelineTick::ZERO {
        out.push(error_diag(
            "timeline.duration",
            document.id.as_str(),
            "duration must be nonnegative",
        ));
    }
    if document.display_rate.numerator == 0 || document.display_rate.denominator == 0 {
        out.push(error_diag(
            "timeline.display_rate",
            document.id.as_str(),
            "display rate must be nonzero",
        ));
    }

    let mut ids = BTreeSet::new();
    for track in &document.tracks {
        if !ids.insert(track.id.as_str().to_owned()) {
            out.push(error_diag(
                "timeline.duplicate_id",
                track.id.as_str(),
                "duplicate stable ID",
            ));
        }
        match track.kind {
            TimelineTrackKind::Animation
            | TimelineTrackKind::TransformProperty
            | TimelineTrackKind::CameraCut
            | TimelineTrackKind::Vfx => {
                if !matches!(track.binding, Some(TimelineBinding::Entity { .. })) {
                    out.push(error_diag(
                        "timeline.binding.required_entity",
                        track.id.as_str(),
                        "track requires a stable entity binding",
                    ));
                }
            }
            TimelineTrackKind::Audio => {
                if track.binding.is_some()
                    && !matches!(track.binding, Some(TimelineBinding::Entity { .. }))
                {
                    out.push(error_diag(
                        "timeline.binding.audio_entity_only",
                        track.id.as_str(),
                        "audio binding must be absent for non-spatial playback or a stable entity for spatial playback",
                    ));
                }
            }
            TimelineTrackKind::Event => {
                if track.binding.is_some() {
                    out.push(error_diag(
                        "timeline.binding.event_targetless",
                        track.id.as_str(),
                        "first-release Timeline Event tracks are sequence-level and must not declare a target binding",
                    ));
                }
            }
        }

        for clip in &track.clips {
            if !ids.insert(clip.id.as_str().to_owned()) {
                out.push(error_diag(
                    "timeline.duplicate_id",
                    clip.id.as_str(),
                    "duplicate stable ID",
                ));
            }
            if clip.payload.kind() != track.kind {
                out.push(error_diag(
                    "timeline.clip.kind_mismatch",
                    clip.id.as_str(),
                    "clip payload does not match track family",
                ));
            }
            if clip.start < TimelineTick::ZERO
                || clip.duration <= TimelineTick::ZERO
                || clip.end() > document.duration
            {
                out.push(error_diag(
                    "timeline.clip.range",
                    clip.id.as_str(),
                    "clip range must be positive and inside the Timeline",
                ));
            }
            if clip.source_offset < TimelineTick::ZERO {
                out.push(error_diag(
                    "timeline.clip.source_offset",
                    clip.id.as_str(),
                    "source offset must be nonnegative",
                ));
            }

            match &clip.payload {
                TimelineClipPayload::TransformProperty { property, from, to } => {
                    let Some(descriptor) = timeline_property_descriptor(property) else {
                        out.push(error_diag(
                            "timeline.property.unsupported",
                            clip.id.as_str(),
                            format!("unsupported Timeline property `{property}`"),
                        ));
                        continue;
                    };
                    if from.kind() != descriptor.value_kind || to.kind() != descriptor.value_kind {
                        out.push(error_diag(
                            "timeline.property.type_mismatch",
                            clip.id.as_str(),
                            format!(
                                "property `{}` requires {:?} endpoints",
                                descriptor.type_id, descriptor.value_kind
                            ),
                        ));
                    }
                    if !from.is_valid_numeric_value() || !to.is_valid_numeric_value() {
                        out.push(error_diag(
                            "timeline.property.non_finite",
                            clip.id.as_str(),
                            "property endpoints must be finite and quaternions must be non-zero",
                        ));
                    }
                }
                TimelineClipPayload::Event { name, payload } => {
                    validate_event_name(name, clip.id.as_str(), &mut out);
                    if payload.len() > MAX_TIMELINE_EVENT_PAYLOAD_BYTES {
                        out.push(error_diag(
                            "timeline.event.payload_too_large",
                            clip.id.as_str(),
                            format!(
                                "event payload exceeds {MAX_TIMELINE_EVENT_PAYLOAD_BYTES} UTF-8 bytes"
                            ),
                        ));
                    }
                }
                TimelineClipPayload::Audio { volume, .. } => {
                    if !volume.is_finite() || !(0.0..=1.0).contains(volume) {
                        out.push(error_diag(
                            "timeline.audio.volume",
                            clip.id.as_str(),
                            "audio volume must be finite in [0,1]",
                        ));
                    }
                }
                TimelineClipPayload::Animation { .. }
                | TimelineClipPayload::CameraCut
                | TimelineClipPayload::Vfx { .. } => {}
            }
        }
    }

    for marker in &document.markers {
        if !ids.insert(marker.id.as_str().to_owned()) {
            out.push(error_diag(
                "timeline.duplicate_id",
                marker.id.as_str(),
                "duplicate stable ID",
            ));
        }
        if marker.tick < TimelineTick::ZERO || marker.tick > document.duration {
            out.push(error_diag(
                "timeline.marker.range",
                marker.id.as_str(),
                "marker must lie inside the Timeline",
            ));
        }
        if let Some(event) = marker.event.as_deref() {
            validate_event_name(event, marker.id.as_str(), &mut out);
        }
    }
    out
}

fn validate_event_name(name: &str, target: &str, out: &mut Vec<TimelineDiagnostic>) {
    if name.trim().is_empty()
        || name.len() > MAX_TIMELINE_EVENT_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        out.push(error_diag(
            "timeline.event.invalid_name",
            target,
            format!(
                "event name must be non-blank, contain no control characters, and fit within {MAX_TIMELINE_EVENT_NAME_BYTES} UTF-8 bytes"
            ),
        ));
    }
}
fn error_diag(code: &'static str, target: &str, message: impl Into<String>) -> TimelineDiagnostic { TimelineDiagnostic { severity: TimelineDiagnosticSeverity::Error, code, target: target.to_owned(), message: message.into() } }

/// Timeline persistence error.
#[derive(Debug)]
pub enum TimelineDocumentError {
    /// Filesystem operation failed.
    Io(std::io::Error),
    /// JSON serialization or deserialization failed.
    Json(serde_json::Error),
    /// Path did not use the canonical Timeline suffix.
    WrongSuffix,
    /// Document failed pure Timeline validation.
    Invalid(Vec<TimelineDiagnostic>),
}
impl fmt::Display for TimelineDocumentError { fn fmt(&self, f:&mut fmt::Formatter<'_>)->fmt::Result { match self { Self::Io(e)=>e.fmt(f), Self::Json(e)=>e.fmt(f), Self::WrongSuffix=>f.write_str("Timeline path must end in .timeline.json"), Self::Invalid(d)=>write!(f,"Timeline has {} validation diagnostics",d.len()) } } }
impl std::error::Error for TimelineDocumentError {}
/// Saves canonical pretty JSON after validation.
pub fn save_timeline(path:&Path, document:&TimelineDocument)->Result<(),TimelineDocumentError>{ if !path.to_string_lossy().ends_with(TIMELINE_FILE_SUFFIX){return Err(TimelineDocumentError::WrongSuffix)} let diagnostics=validate_timeline(document); if diagnostics.iter().any(|d|d.severity==TimelineDiagnosticSeverity::Error){return Err(TimelineDocumentError::Invalid(diagnostics))} let mut text=serde_json::to_string_pretty(document).map_err(TimelineDocumentError::Json)?; text.push('\n'); crate::replace_file_contents(path,&text).map_err(|e|TimelineDocumentError::Io(std::io::Error::other(e.to_string()))) }
/// Loads current-format-only Timeline JSON and validates it.
pub fn load_timeline(path:&Path)->Result<TimelineDocument,TimelineDocumentError>{ if !path.to_string_lossy().ends_with(TIMELINE_FILE_SUFFIX){return Err(TimelineDocumentError::WrongSuffix)} let text=std::fs::read_to_string(path).map_err(TimelineDocumentError::Io)?; let document:TimelineDocument=serde_json::from_str(&text).map_err(TimelineDocumentError::Json)?; let diagnostics=validate_timeline(&document); if diagnostics.iter().any(|d|d.severity==TimelineDiagnosticSeverity::Error){Err(TimelineDocumentError::Invalid(diagnostics))}else{Ok(document)} }

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(payload: TimelineClipPayload) -> TimelineClip {
        TimelineClip {
            id: TimelineClipId::generate(),
            name: "clip".into(),
            start: TimelineTick::ZERO,
            duration: TimelineTick::new(10),
            source_offset: TimelineTick::ZERO,
            payload,
        }
    }

    #[test]
    fn stable_ids_survive_json_roundtrip() {
        let doc = TimelineDocument::new("shot", TimelineTick::new(48_000));
        let json = serde_json::to_string(&doc).unwrap();
        let loaded: TimelineDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc.id, loaded.id);
        assert_eq!(doc.duration, loaded.duration);
    }

    #[test]
    fn first_release_track_registry_contains_exactly_the_six_adr_families() {
        let type_ids = timeline_track_registry()
            .iter()
            .map(|descriptor| descriptor.type_id)
            .collect::<Vec<_>>();
        assert_eq!(
            type_ids,
            vec![
                "engine.timeline.animation",
                "engine.timeline.transform_property",
                "engine.timeline.camera_cut",
                "engine.timeline.event",
                "engine.timeline.audio",
                "engine.timeline.vfx",
            ]
        );
    }

    #[test]
    fn animation_payload_persists_a_motion_slot_instead_of_an_importer_asset() {
        let motion_slot = MotionSlotId::generate();
        let payload = TimelineClipPayload::Animation {
            motion_slot: motion_slot.clone(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let loaded: TimelineClipPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded,
            TimelineClipPayload::Animation { motion_slot }
        );
        assert!(json.contains("motion_slot"));
    }

    #[test]
    fn unbound_audio_track_is_valid_for_non_spatial_playback() {
        let mut doc = TimelineDocument::new("audio", TimelineTick::new(100));
        doc.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "music".into(),
            kind: TimelineTrackKind::Audio,
            enabled: true,
            binding: None,
            clips: vec![clip(TimelineClipPayload::Audio {
                clip: AssetId::generate(),
                volume: 1.0,
                looping: false,
            })],
        });
        assert!(validate_timeline(&doc).is_empty());
    }

    #[test]
    fn unsupported_property_is_rejected_instead_of_using_reflection() {
        let mut doc = TimelineDocument::new("property", TimelineTick::new(100));
        doc.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "property".into(),
            kind: TimelineTrackKind::TransformProperty,
            enabled: true,
            binding: Some(TimelineBinding::Entity {
                entity: EntityId::generate(),
            }),
            clips: vec![clip(TimelineClipPayload::TransformProperty {
                property: "engine.private.arbitrary_field".into(),
                from: TimelinePropertyValue::Number(0.0),
                to: TimelinePropertyValue::Number(1.0),
            })],
        });
        let diagnostics = validate_timeline(&doc);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "timeline.property.unsupported"));
    }

    #[test]
    fn event_payloads_are_bounded() {
        let mut doc = TimelineDocument::new("event", TimelineTick::new(100));
        doc.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "events".into(),
            kind: TimelineTrackKind::Event,
            enabled: true,
            binding: None,
            clips: vec![clip(TimelineClipPayload::Event {
                name: "sequence.started".into(),
                payload: "x".repeat(MAX_TIMELINE_EVENT_PAYLOAD_BYTES + 1),
            })],
        });
        let diagnostics = validate_timeline(&doc);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "timeline.event.payload_too_large"));
    }

    #[test]
    fn mismatched_typed_clip_is_diagnostic() {
        let mut doc = TimelineDocument::new("x", TimelineTick::new(100));
        doc.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "x".into(),
            kind: TimelineTrackKind::Audio,
            enabled: true,
            binding: Some(TimelineBinding::Entity {
                entity: EntityId::generate(),
            }),
            clips: vec![clip(TimelineClipPayload::CameraCut)],
        });
        assert!(doc.compile().schedule.is_none());
    }
}
