use super::{
    apply_action, play_owned, set_play_owned, stop_track, TimelineBindings, TimelineDiagnostics,
};
use engine_authoring::TimelineTick;
use engine_ecs::World;
use engine_timeline::{
    AdapterTokens, CompiledClipPayload, CompiledTimeline, TimelineEvaluation, VfxAction,
};

pub(super) fn apply_evaluation(
    timeline: &CompiledTimeline,
    evaluation: &TimelineEvaluation,
    previous_tick: TimelineTick,
    world: &mut World,
    bindings: &TimelineBindings,
    tokens: &mut AdapterTokens,
    diagnostics: &mut TimelineDiagnostics,
) {
    let wrapped = evaluation.tick < previous_tick;
    let mut boundaries = Vec::new();

    for transition in &evaluation.entered {
        if let Some((track_index, clip_index)) =
            find_transition(timeline, &transition.track, &transition.clip)
        {
            let clip = &timeline.tracks[track_index].clips[clip_index];
            if matches!(&clip.payload, CompiledClipPayload::Vfx { .. }) {
                boundaries.push(Boundary {
                    phase: boundary_phase(clip.start, previous_tick, wrapped),
                    tick: clip.start,
                    track_index,
                    clip_index,
                    edge: BoundaryEdge::Enter,
                });
            }
        }
    }

    for transition in &evaluation.exited {
        if let Some((track_index, clip_index)) =
            find_transition(timeline, &transition.track, &transition.clip)
        {
            let clip = &timeline.tracks[track_index].clips[clip_index];
            if matches!(
                &clip.payload,
                CompiledClipPayload::Vfx {
                    action: VfxAction::Play,
                    ..
                }
            ) {
                boundaries.push(Boundary {
                    phase: boundary_phase(clip.end, previous_tick, wrapped),
                    tick: clip.end,
                    track_index,
                    clip_index,
                    edge: BoundaryEdge::ExitPlay,
                });
            }
        }
    }

    boundaries.sort_by_key(|boundary| {
        (
            boundary.phase,
            boundary.tick,
            boundary.track_index,
            boundary.edge.order(),
            boundary.clip_index,
        )
    });

    for boundary in boundaries {
        let track = &timeline.tracks[boundary.track_index];
        let clip = &track.clips[boundary.clip_index];
        match boundary.edge {
            BoundaryEdge::Enter => {
                let CompiledClipPayload::Vfx { action, .. } = &clip.payload else {
                    continue;
                };
                apply_action(track, *action, world, bindings, diagnostics);
                set_play_owned(tokens, track, *action == VfxAction::Play);
            }
            BoundaryEdge::ExitPlay => {
                stop_track(track, world, bindings, diagnostics);
                set_play_owned(tokens, track, false);
            }
        }
    }

    // `ClipTransition::exited` uses crossing intervals, so a step that lands
    // exactly on the half-open clip end may report the exit on the next
    // traversal. Reconcile interval ownership at the sampled tick so VFX stops
    // at the authored exclusive end without adding a frame of emission.
    for track in timeline.tracks.iter().filter(|track| track.enabled) {
        if !track
            .clips
            .iter()
            .any(|clip| matches!(&clip.payload, CompiledClipPayload::Vfx { .. }))
        {
            continue;
        }
        let active_play = track.clips.iter().any(|clip| {
            clip.contains(evaluation.tick)
                && matches!(
                    &clip.payload,
                    CompiledClipPayload::Vfx {
                        action: VfxAction::Play,
                        ..
                    }
                )
        });
        match (play_owned(tokens, track), active_play) {
            (true, false) => {
                stop_track(track, world, bindings, diagnostics);
                set_play_owned(tokens, track, false);
            }
            (false, true) => {
                apply_action(track, VfxAction::Play, world, bindings, diagnostics);
                set_play_owned(tokens, track, true);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundaryEdge {
    ExitPlay,
    Enter,
}

impl BoundaryEdge {
    const fn order(self) -> u8 {
        match self {
            Self::ExitPlay => 0,
            Self::Enter => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Boundary {
    phase: u8,
    tick: TimelineTick,
    track_index: usize,
    clip_index: usize,
    edge: BoundaryEdge,
}

fn boundary_phase(tick: TimelineTick, previous_tick: TimelineTick, wrapped: bool) -> u8 {
    if wrapped && tick < previous_tick {
        1
    } else {
        0
    }
}

fn find_transition(
    timeline: &CompiledTimeline,
    track_id: &engine_authoring::TimelineTrackId,
    clip_id: &engine_authoring::TimelineClipId,
) -> Option<(usize, usize)> {
    for (track_index, track) in timeline.tracks.iter().enumerate() {
        if &track.id != track_id {
            continue;
        }
        if let Some(clip_index) = track.clips.iter().position(|clip| &clip.id == clip_id) {
            return Some((track_index, clip_index));
        }
    }
    None
}
