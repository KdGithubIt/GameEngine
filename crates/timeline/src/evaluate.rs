//! Timeline evaluation and exact marker crossing (ADR 0126 §8).
//!
//! Evaluation is expressed over the half-open intervals a traversal actually
//! crossed. A marker fires exactly once when an interval contains its tick, so
//! a loop boundary can neither lose a marker nor fire it twice, and a seek that
//! crossed nothing fires nothing.

use crate::{CompiledClip, CompiledClipPayload, CompiledTimeline, VfxAction};
use engine_authoring::{
    AssetId, EntityId, MotionSlotId, TimelineClipId, TimelineMarkerId, TimelineProperty,
    TimelineTick, TimelineTrackId,
};

/// One clip that contains the evaluated tick.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveClip {
    /// Track the clip belongs to.
    pub track: TimelineTrackId,
    /// Stable clip identity.
    pub clip: TimelineClipId,
    /// Clip-local tick offset of the evaluated position.
    pub offset: i64,
    /// Normalized clip progress in `0.0..=1.0`.
    pub progress: f32,
    /// Typed output the composition layer applies.
    pub output: TimelineTrackOutput,
}

/// Whether a clip was entered or exited during this evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipTransition {
    /// Track the clip belongs to.
    pub track: TimelineTrackId,
    /// Stable clip identity.
    pub clip: TimelineClipId,
}

/// One event the traversal produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredEvent {
    /// Stable event name.
    pub event: String,
    /// Marker that produced the event, when it came from the marker lane.
    pub marker: Option<TimelineMarkerId>,
    /// Track that produced the event, when it came from an Event track clip.
    pub track: Option<TimelineTrackId>,
}

/// Typed output one active clip contributes.
///
/// The neutral core resolves what should happen; applying it to a camera, an
/// animation player, an audio voice, or a VFX instance belongs to the
/// composition layer, which is why no runtime handle appears here.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineTrackOutput {
    /// The Event track has no per-frame output; its events fire on entry.
    Event,
    /// Camera selection override for the clip interval.
    CameraCut {
        /// Authoring entity carrying the target camera.
        camera: EntityId,
        /// Bound track entity, when the track names one.
        binding: Option<EntityId>,
    },
    /// Motion slot playback for the clip interval.
    Animation {
        /// Bound authoring entity.
        entity: Option<EntityId>,
        /// Bound Animation Set asset.
        animation_set: Option<AssetId>,
        /// Stable motion slot.
        motion_slot: MotionSlotId,
        /// Playback rate multiplier.
        speed: f32,
        /// Whether the motion loops inside the clip interval.
        looping: bool,
    },
    /// Sampled property value for the evaluated tick.
    Property {
        /// Bound authoring entity.
        entity: Option<EntityId>,
        /// Property the value belongs to.
        property: TimelineProperty,
        /// Sampled value.
        value: f32,
    },
    /// Audio cue state for the clip interval.
    Audio {
        /// Audio cue asset.
        cue: AssetId,
        /// Whether the cue should be playing.
        play: bool,
        /// Fade duration applied at the clip boundaries.
        fade_ticks: i64,
        /// Bound authoring entity for spatial cues, when the track names one.
        entity: Option<EntityId>,
    },
    /// VFX effect state for the clip interval.
    Vfx {
        /// VFX effect asset.
        effect: AssetId,
        /// Action performed when the clip is entered.
        action: VfxAction,
        /// Bound authoring entity.
        entity: Option<EntityId>,
    },
}

/// The result of evaluating one Timeline player.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TimelineEvaluation {
    /// Evaluated playhead position.
    pub tick: TimelineTick,
    /// Clips containing the evaluated tick, in deterministic track order.
    pub active: Vec<ActiveClip>,
    /// Clips entered during this evaluation.
    pub entered: Vec<ClipTransition>,
    /// Clips exited during this evaluation.
    pub exited: Vec<ClipTransition>,
    /// Events produced by marker crossings and Event track entries.
    pub events: Vec<FiredEvent>,
}

/// Evaluates a compiled Timeline at `tick` over the traversed `intervals`.
pub(crate) fn evaluate_intervals(
    timeline: &CompiledTimeline,
    tick: TimelineTick,
    intervals: &[(TimelineTick, TimelineTick)],
) -> TimelineEvaluation {
    let mut evaluation = TimelineEvaluation {
        tick,
        ..TimelineEvaluation::default()
    };
    for track in &timeline.tracks {
        if !track.enabled {
            continue;
        }
        for clip in &track.clips {
            for (from, to) in intervals {
                // A clip is entered when the traversal interval contains its
                // start tick. Continuing inside a clip crosses nothing, and a
                // loop wrap that re-enters at the clip start crosses it again,
                // which is why the entry test is the crossing and not the
                // containment of the interval start.
                if crosses(clip.start, *from, *to) {
                    evaluation.entered.push(ClipTransition {
                        track: track.id.clone(),
                        clip: clip.id.clone(),
                    });
                    if let CompiledClipPayload::Event { event } = &clip.payload {
                        evaluation.events.push(FiredEvent {
                            event: event.clone(),
                            marker: None,
                            track: Some(track.id.clone()),
                        });
                    }
                }
                if crosses_exit(clip.end, *from, *to) {
                    evaluation.exited.push(ClipTransition {
                        track: track.id.clone(),
                        clip: clip.id.clone(),
                    });
                }
            }
            if clip.contains(tick) {
                let offset = tick.get() - clip.start.get();
                evaluation.active.push(ActiveClip {
                    track: track.id.clone(),
                    clip: clip.id.clone(),
                    offset,
                    progress: (offset as f32 / clip.duration() as f32).clamp(0.0, 1.0),
                    output: track_output(track.entity.as_ref(), track.asset.as_ref(), clip, offset),
                });
            }
        }
    }
    for marker in &timeline.markers {
        for (from, to) in intervals {
            if crosses(marker.tick, *from, *to) {
                evaluation.events.push(FiredEvent {
                    event: marker.event.clone(),
                    marker: Some(marker.id.clone()),
                    track: None,
                });
            }
        }
    }
    evaluation
}

/// Whether a half-open traversal `[from, to)` contains `boundary`.
fn crosses(boundary: TimelineTick, from: TimelineTick, to: TimelineTick) -> bool {
    boundary >= from && boundary < to
}

/// Whether a forward traversal reaches an exclusive clip end.
///
/// Clip containment ends at `to`, so exits belong to the step that lands on
/// the boundary instead of a later step that may never run.
fn crosses_exit(boundary: TimelineTick, from: TimelineTick, to: TimelineTick) -> bool {
    boundary > from && boundary <= to
}

fn track_output(
    entity: Option<&EntityId>,
    asset: Option<&AssetId>,
    clip: &CompiledClip,
    offset: i64,
) -> TimelineTrackOutput {
    match &clip.payload {
        CompiledClipPayload::Event { .. } => TimelineTrackOutput::Event,
        CompiledClipPayload::CameraCut { camera } => TimelineTrackOutput::CameraCut {
            camera: camera.clone(),
            binding: entity.cloned(),
        },
        CompiledClipPayload::Animation {
            motion_slot,
            speed,
            looping,
        } => TimelineTrackOutput::Animation {
            entity: entity.cloned(),
            animation_set: asset.cloned(),
            motion_slot: motion_slot.clone(),
            speed: *speed,
            looping: *looping,
        },
        CompiledClipPayload::Property { property, curve } => TimelineTrackOutput::Property {
            entity: entity.cloned(),
            property: *property,
            value: curve.sample(offset),
        },
        CompiledClipPayload::Audio {
            cue,
            play,
            fade_ticks,
        } => TimelineTrackOutput::Audio {
            cue: cue.clone(),
            play: *play,
            fade_ticks: *fade_ticks,
            entity: entity.cloned(),
        },
        CompiledClipPayload::Vfx { effect, action } => TimelineTrackOutput::Vfx {
            effect: effect.clone(),
            action: *action,
            entity: entity.cloned(),
        },
    }
}
