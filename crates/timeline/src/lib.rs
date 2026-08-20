//! Neutral Timeline scheduling and evaluation core (ADR 0126).
//!
//! This crate owns timeline time semantics and nothing else: tick traversal,
//! the immutable compiled schedule, per-player transient state, marker crossing,
//! and the typed outputs one evaluation produces. It deliberately does not
//! depend on audio, rendering, animation, physics, ECS, or Editor GUI, so a
//! timeline test compiles without a graphics or audio backend and the crate
//! cannot become a cross-domain dependency hub.
//!
//! Concrete adapters that apply an evaluation to a running world are registered
//! at the top-level `engine` composition layer.

#![deny(missing_docs)]

use engine_authoring::{
    AssetId, EntityId, TIMELINE_TICKS_PER_SECOND, TimelineClip, TimelineClipId,
    TimelineClipPayload, TimelineDocument, TimelineId, TimelineMarkerId, TimelineProperty,
    TimelineTick, TimelineTrackId, TimelineTrackKind,
};
use std::collections::BTreeMap;
use std::fmt;

mod evaluate;
mod player;
mod registry;

pub use evaluate::{
    ActiveClip, ClipTransition, FiredEvent, TimelineEvaluation, TimelineTrackOutput,
};
pub use player::{LoopRegion, TimelinePlayState, TimelinePlayer, TimelineSeek};
pub use registry::{TrackDescriptor, TrackRegistry, TrackSeekPolicy};

/// One compiled clip with pre-resolved payload data.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledClip {
    /// Stable authored clip identity.
    pub id: TimelineClipId,
    /// Inclusive clip start.
    pub start: TimelineTick,
    /// Exclusive clip end.
    pub end: TimelineTick,
    /// Pre-resolved payload.
    pub payload: CompiledClipPayload,
}

impl CompiledClip {
    /// Whether one tick lies inside the half-open clip interval.
    pub fn contains(&self, tick: TimelineTick) -> bool {
        tick >= self.start && tick < self.end
    }

    /// Clip length in ticks, never zero for a compiled clip.
    pub fn duration(&self) -> i64 {
        (self.end.get() - self.start.get()).max(1)
    }
}

/// Compiled clip payload.
///
/// Compilation resolves everything the evaluator needs without consulting the
/// authoring document again, and without holding a runtime handle of any kind.
#[derive(Debug, Clone, PartialEq)]
pub enum CompiledClipPayload {
    /// Emits one stable sequence event when the clip is entered.
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
        speed: f32,
        /// Whether the motion loops inside the clip interval.
        looping: bool,
    },
    /// Samples one supported typed property from a curve.
    Property {
        /// Property the curve drives.
        property: TimelineProperty,
        /// Curve keys sorted by tick, relative to the clip start.
        curve: CompiledCurve,
    },
    /// Starts or stops an audio cue.
    Audio {
        /// Audio cue asset.
        cue: AssetId,
        /// Whether the cue plays for the clip interval or stops at its start.
        play: bool,
        /// Fade duration applied at the clip boundaries.
        fade_ticks: i64,
    },
    /// Starts, stops, or restarts a VFX effect.
    Vfx {
        /// VFX effect asset.
        effect: AssetId,
        /// Action performed when the clip is entered.
        action: VfxAction,
    },
}

/// Compiled VFX action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfxAction {
    /// Plays the effect for the clip interval.
    Play,
    /// Stops the effect.
    Stop,
    /// Restarts the effect from its beginning.
    Restart,
}

/// One compiled curve key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledKey {
    /// Key offset from the clip start.
    pub offset: i64,
    /// Authored value at this key.
    pub value: f32,
    /// Interpolation from this key to the next.
    pub interpolation: CurveInterpolation,
}

/// Compiled interpolation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveInterpolation {
    /// Holds the key value until the next key.
    Step,
    /// Interpolates linearly to the next key.
    Linear,
    /// Interpolates with smooth ease-in and ease-out.
    Smooth,
}

/// A compiled curve sampled by clip-local offset.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledCurve {
    keys: Vec<CompiledKey>,
}

impl CompiledCurve {
    /// Builds a curve from keys already sorted by offset.
    pub fn new(keys: Vec<CompiledKey>) -> Self {
        Self { keys }
    }

    /// Keys in authored order.
    pub fn keys(&self) -> &[CompiledKey] {
        &self.keys
    }

    /// Samples the curve at one clip-local offset.
    ///
    /// Sampling is a pure function of the offset, which is what lets property
    /// tracks declare themselves stateless for seeking.
    pub fn sample(&self, offset: i64) -> f32 {
        let Some(first) = self.keys.first() else {
            return 0.0;
        };
        if offset <= first.offset {
            return first.value;
        }
        let Some(last) = self.keys.last() else {
            return 0.0;
        };
        if offset >= last.offset {
            return last.value;
        }
        for window in self.keys.windows(2) {
            let (left, right) = (window[0], window[1]);
            if offset < left.offset || offset >= right.offset {
                continue;
            }
            let span = (right.offset - left.offset).max(1);
            let progress = (offset - left.offset) as f32 / span as f32;
            return match left.interpolation {
                CurveInterpolation::Step => left.value,
                CurveInterpolation::Linear => left.value + (right.value - left.value) * progress,
                CurveInterpolation::Smooth => {
                    let eased = progress * progress * (3.0 - 2.0 * progress);
                    left.value + (right.value - left.value) * eased
                }
            };
        }
        last.value
    }
}

/// One compiled track.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTrack {
    /// Stable authored track identity.
    pub id: TimelineTrackId,
    /// Registry track kind.
    pub kind: TimelineTrackKind,
    /// Whether the track contributes to evaluation.
    pub enabled: bool,
    /// Bound authoring entity, when the track targets one.
    pub entity: Option<EntityId>,
    /// Bound authoring asset, when the track targets one.
    pub asset: Option<AssetId>,
    /// Clips sorted by start tick.
    pub clips: Vec<CompiledClip>,
}

/// One compiled marker.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledMarker {
    /// Stable authored marker identity.
    pub id: TimelineMarkerId,
    /// Marker time.
    pub tick: TimelineTick,
    /// Stable event emitted when the playhead crosses this marker.
    pub event: String,
}

/// An immutable compiled schedule shared by every player of one Timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTimeline {
    /// Stable authored document identity.
    pub id: TimelineId,
    /// Authored sequence duration.
    pub duration: TimelineTick,
    /// Tracks in deterministic evaluation order.
    pub tracks: Vec<CompiledTrack>,
    /// Markers sorted by tick.
    pub markers: Vec<CompiledMarker>,
}

impl CompiledTimeline {
    /// Ticks per second this schedule is expressed in.
    pub const fn ticks_per_second() -> i64 {
        TIMELINE_TICKS_PER_SECOND
    }

    /// Resolves one compiled track by stable identity.
    pub fn track(&self, id: &TimelineTrackId) -> Option<&CompiledTrack> {
        self.tracks.iter().find(|track| &track.id == id)
    }
}

/// Why a Timeline document could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineCompileError {
    /// The authored document failed its own validation.
    InvalidDocument(Vec<String>),
}

impl fmt::Display for TimelineCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument(errors) => {
                write!(
                    formatter,
                    "invalid Timeline document: {}",
                    errors.join("; ")
                )
            }
        }
    }
}

impl std::error::Error for TimelineCompileError {}

/// Compiles one authored Timeline into an immutable schedule.
///
/// Track order follows the authored order and clip order follows start tick, so
/// two players of the same document always traverse the same schedule in the
/// same order. Overlapping clips on different tracks are resolved by that
/// deterministic order rather than by iteration accident.
pub fn compile_timeline(
    document: &TimelineDocument,
) -> Result<CompiledTimeline, TimelineCompileError> {
    let errors = document.validate();
    if !errors.is_empty() {
        return Err(TimelineCompileError::InvalidDocument(errors));
    }
    let mut tracks = Vec::with_capacity(document.tracks.len());
    for track in &document.tracks {
        let mut clips = track
            .clips
            .iter()
            .map(compile_clip)
            .collect::<Vec<CompiledClip>>();
        clips.sort_by_key(|clip| (clip.start, clip.end, clip.id.as_str().to_owned()));
        tracks.push(CompiledTrack {
            id: track.id.clone(),
            kind: track.kind,
            enabled: track.enabled,
            entity: track.binding.entity.clone(),
            asset: track.binding.asset.clone(),
            clips,
        });
    }
    let mut markers = document
        .markers
        .iter()
        .map(|marker| CompiledMarker {
            id: marker.id.clone(),
            tick: marker.tick,
            event: marker.event.clone(),
        })
        .collect::<Vec<_>>();
    markers.sort_by_key(|marker| (marker.tick, marker.id.as_str().to_owned()));
    Ok(CompiledTimeline {
        id: document.id.clone(),
        duration: document.duration,
        tracks,
        markers,
    })
}

fn compile_clip(clip: &TimelineClip) -> CompiledClip {
    let payload = match &clip.payload {
        TimelineClipPayload::Event { event } => CompiledClipPayload::Event {
            event: event.clone(),
        },
        TimelineClipPayload::CameraCut { camera } => CompiledClipPayload::CameraCut {
            camera: camera.clone(),
        },
        TimelineClipPayload::Animation {
            motion_slot,
            speed,
            looping,
        } => CompiledClipPayload::Animation {
            motion_slot: motion_slot.clone(),
            speed: *speed,
            looping: *looping,
        },
        TimelineClipPayload::Property { property, keys } => {
            let mut compiled = keys
                .iter()
                .map(|key| CompiledKey {
                    offset: key.tick.get(),
                    value: key.value,
                    interpolation: match key.interpolation {
                        engine_authoring::TimelineInterpolation::Step => CurveInterpolation::Step,
                        engine_authoring::TimelineInterpolation::Linear => {
                            CurveInterpolation::Linear
                        }
                        engine_authoring::TimelineInterpolation::Smooth => {
                            CurveInterpolation::Smooth
                        }
                    },
                })
                .collect::<Vec<_>>();
            compiled.sort_by_key(|key| key.offset);
            CompiledClipPayload::Property {
                property: *property,
                curve: CompiledCurve::new(compiled),
            }
        }
        TimelineClipPayload::Audio {
            cue,
            action,
            fade_ticks,
        } => CompiledClipPayload::Audio {
            cue: cue.clone(),
            play: matches!(action, engine_authoring::TimelineAudioAction::Play),
            fade_ticks: fade_ticks.get(),
        },
        TimelineClipPayload::Vfx { effect, action } => CompiledClipPayload::Vfx {
            effect: effect.clone(),
            action: match action {
                engine_authoring::TimelineVfxAction::Play => VfxAction::Play,
                engine_authoring::TimelineVfxAction::Stop => VfxAction::Stop,
                engine_authoring::TimelineVfxAction::Restart => VfxAction::Restart,
            },
        },
    };
    CompiledClip {
        id: clip.id.clone(),
        start: clip.start,
        end: clip.end,
        payload,
    }
}

/// Per-track transient tokens an adapter may keep between evaluations.
///
/// The neutral core stores these opaquely so a domain adapter can carry its own
/// handle without the timeline crate learning that domain's types.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AdapterTokens {
    tokens: BTreeMap<String, u64>,
}

impl AdapterTokens {
    /// Reads one adapter token.
    pub fn get(&self, key: &str) -> Option<u64> {
        self.tokens.get(key).copied()
    }

    /// Writes one adapter token.
    pub fn set(&mut self, key: impl Into<String>, value: u64) {
        self.tokens.insert(key.into(), value);
    }

    /// Drops every token, as a stop or a rebind must.
    pub fn clear(&mut self) {
        self.tokens.clear();
    }

    /// Whether any token is held.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        TimelineBinding, TimelineClipId, TimelineInterpolation, TimelineKey, TimelineTrack,
    };

    pub(crate) fn property_clip(start: i64, end: i64, from: f32, to: f32) -> TimelineClip {
        TimelineClip {
            id: TimelineClipId::generate(),
            start: TimelineTick(start),
            end: TimelineTick(end),
            payload: TimelineClipPayload::Property {
                property: TimelineProperty::TranslationX,
                keys: vec![
                    TimelineKey {
                        tick: TimelineTick::ZERO,
                        value: from,
                        interpolation: TimelineInterpolation::Linear,
                    },
                    TimelineKey {
                        tick: TimelineTick(end - start),
                        value: to,
                        interpolation: TimelineInterpolation::Linear,
                    },
                ],
            },
        }
    }

    pub(crate) fn property_track(clips: Vec<TimelineClip>) -> TimelineTrack {
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
    fn compilation_rejects_a_document_its_own_validation_rejects() {
        let mut document = TimelineDocument::new(TimelineTick(1_000));
        document.schema_version = 99;
        assert!(matches!(
            compile_timeline(&document),
            Err(TimelineCompileError::InvalidDocument(_))
        ));
    }

    #[test]
    fn compiled_clips_and_markers_are_deterministically_ordered() {
        let mut document = TimelineDocument::new(TimelineTick(10_000));
        document.tracks.push(property_track(vec![
            property_clip(5_000, 6_000, 0.0, 1.0),
            property_clip(0, 1_000, 0.0, 1.0),
        ]));
        document.markers.push(engine_authoring::TimelineMarker {
            id: engine_authoring::TimelineMarkerId::generate(),
            tick: TimelineTick(9_000),
            name: "late".to_owned(),
            event: "late".to_owned(),
        });
        document.markers.push(engine_authoring::TimelineMarker {
            id: engine_authoring::TimelineMarkerId::generate(),
            tick: TimelineTick(1_000),
            name: "early".to_owned(),
            event: "early".to_owned(),
        });
        let compiled = compile_timeline(&document).expect("compile");
        assert_eq!(compiled.tracks[0].clips[0].start, TimelineTick(0));
        assert_eq!(compiled.tracks[0].clips[1].start, TimelineTick(5_000));
        assert_eq!(compiled.markers[0].event, "early");
        assert_eq!(compiled.markers[1].event, "late");
    }

    #[test]
    fn a_curve_samples_as_a_pure_function_of_its_offset() {
        let curve = CompiledCurve::new(vec![
            CompiledKey {
                offset: 0,
                value: 0.0,
                interpolation: CurveInterpolation::Linear,
            },
            CompiledKey {
                offset: 100,
                value: 10.0,
                interpolation: CurveInterpolation::Step,
            },
            CompiledKey {
                offset: 200,
                value: 20.0,
                interpolation: CurveInterpolation::Linear,
            },
        ]);
        assert_eq!(curve.sample(-10), 0.0);
        assert_eq!(curve.sample(50), 5.0);
        assert_eq!(curve.sample(150), 10.0);
        assert_eq!(curve.sample(500), 20.0);
        assert_eq!(curve.sample(50), curve.sample(50));
    }

    #[test]
    fn adapter_tokens_are_opaque_and_clearable() {
        let mut tokens = AdapterTokens::default();
        assert!(tokens.is_empty());
        tokens.set("voice", 7);
        assert_eq!(tokens.get("voice"), Some(7));
        tokens.clear();
        assert!(tokens.get("voice").is_none());
    }
}
