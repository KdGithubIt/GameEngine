use super::{
    APPLIED_SEEK_GENERATION, PENDING_SEEK_GENERATION, PendingSeek, TimelineBindingDiagnostic,
    TimelineBindings, TimelineDiagnostics, TimelinePlayerComponent, set_play_owned,
};
use crate::transform::GlobalTransform;
use crate::vfx::VfxPlayer;
use engine_authoring::TimelineTick;
use engine_ecs::World;
use engine_timeline::{
    AdapterTokens, CompiledClipPayload, CompiledTimeline, TimelinePlayState, VfxAction,
};

pub(in crate::timeline) fn mark_seek(component: &mut TimelinePlayerComponent) {
    component
        .tokens
        .set(PENDING_SEEK_GENERATION, component.player.generation());
}

pub(in crate::timeline) fn pending_seek(
    component: &TimelinePlayerComponent,
) -> Option<PendingSeek> {
    let generation = component.player.generation();
    let pending = component.tokens.get(PENDING_SEEK_GENERATION);
    let applied = component.tokens.get(APPLIED_SEEK_GENERATION);
    (pending == Some(generation) && applied != Some(generation)).then_some(PendingSeek {
        generation,
        tick: component.player.tick(),
        state: component.player.state(),
    })
}

pub(in crate::timeline) fn mark_seek_applied(tokens: &mut AdapterTokens, seek: PendingSeek) {
    tokens.set(APPLIED_SEEK_GENERATION, seek.generation);
}

pub(in crate::timeline) fn apply_seek(
    timeline: &CompiledTimeline,
    seek: PendingSeek,
    world: &mut World,
    bindings: &TimelineBindings,
    tokens: &mut AdapterTokens,
    diagnostics: &mut TimelineDiagnostics,
) {
    for track in timeline.tracks.iter().filter(|track| track.enabled) {
        if !track
            .clips
            .iter()
            .any(|clip| matches!(&clip.payload, CompiledClipPayload::Vfx { .. }))
        {
            continue;
        }

        let state = replayed_state(track, seek.tick);
        let Some(authoring) = track.entity.as_ref() else {
            continue;
        };
        let Some(target) = bindings.resolve(authoring) else {
            diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
                authoring: authoring.as_stable_id().as_str().to_owned(),
            });
            set_play_owned(tokens, track, false);
            continue;
        };

        if seek.state == TimelinePlayState::Stopped || matches!(state, ReplayedState::Stopped) {
            match world.get_component_mut::<VfxPlayer>(target) {
                Some(player) => player.stop(),
                None => diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                    authoring: authoring.as_stable_id().as_str().to_owned(),
                    component: "VfxPlayer",
                }),
            }
            set_play_owned(tokens, track, false);
            continue;
        }

        let ReplayedState::Running {
            since,
            play_owned: seek_play_owned,
        } = state
        else {
            unreachable!("stopped replay state handled above");
        };
        let Some(global) = world.get_component::<GlobalTransform>(target) else {
            diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                authoring: authoring.as_stable_id().as_str().to_owned(),
                component: "GlobalTransform",
            });
            set_play_owned(tokens, track, false);
            continue;
        };
        let origin = global.matrix().col(3).truncate();

        let Some(player) = world.get_component_mut::<VfxPlayer>(target) else {
            diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                authoring: authoring.as_stable_id().as_str().to_owned(),
                component: "VfxPlayer",
            });
            set_play_owned(tokens, track, false);
            continue;
        };
        let elapsed = seek.tick.saturating_sub(since).as_seconds() as f32 * player.time_scale;
        player.instance_mut().seek_preview(elapsed, origin);
        match seek.state {
            TimelinePlayState::Playing => player.play(),
            TimelinePlayState::Paused => {
                player.play();
                player.pause();
            }
            TimelinePlayState::Stopped => unreachable!("stopped Timeline handled above"),
        }
        set_play_owned(tokens, track, seek_play_owned);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayedState {
    Stopped,
    Running {
        since: TimelineTick,
        play_owned: bool,
    },
}

fn replayed_state(track: &engine_timeline::CompiledTrack, target: TimelineTick) -> ReplayedState {
    let mut running_since = None;
    let mut play_until = None;

    for clip in &track.clips {
        if clip.start > target {
            break;
        }
        if play_until.is_some_and(|end| end <= clip.start) {
            running_since = None;
            play_until = None;
        }

        let CompiledClipPayload::Vfx { action, .. } = &clip.payload else {
            continue;
        };
        match action {
            VfxAction::Play => {
                if running_since.is_none() {
                    running_since = Some(clip.start);
                }
                play_until = Some(clip.end);
            }
            VfxAction::Stop => {
                running_since = None;
                play_until = None;
            }
            VfxAction::Restart => {
                running_since = Some(clip.start);
                play_until = None;
            }
        }
    }

    if play_until.is_some_and(|end| end <= target) {
        return ReplayedState::Stopped;
    }
    match running_since {
        Some(since) => ReplayedState::Running {
            since,
            play_owned: play_until.is_some(),
        },
        None => ReplayedState::Stopped,
    }
}
