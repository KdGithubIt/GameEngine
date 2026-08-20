//! VFX Timeline adapter at the final engine composition boundary (ADR 0126).
//!
//! The neutral Timeline core reports stable VFX clip boundaries. This adapter
//! resolves those authoring bindings to live scene entities and drives the
//! ADR 0125 `VfxPlayer` interface without moving VFX simulation ownership out
//! of `engine-render-runtime`.

use super::{
    TimelineBindingDiagnostic, TimelineBindings, TimelineDiagnostics, TimelinePlayerComponent,
};
use crate::transform::GlobalTransform;
use crate::vfx::VfxPlayer;
use engine_authoring::TimelineTick;
use engine_ecs::World;
use engine_timeline::{
    AdapterTokens, CompiledClipPayload, CompiledTimeline, TimelineEvaluation, TimelinePlayState,
    VfxAction,
};

const PENDING_SEEK_GENERATION: &str = "engine.timeline.vfx.pending_seek_generation";
const APPLIED_SEEK_GENERATION: &str = "engine.timeline.vfx.applied_seek_generation";
const PLAY_TOKEN_PREFIX: &str = "engine.timeline.vfx.play:";

#[derive(Debug, Clone, Copy)]
pub(super) struct PendingSeek {
    generation: u64,
    tick: TimelineTick,
    state: TimelinePlayState,
}

/// Marks the current Timeline position for VFX reconstruction on the next
/// composition step.
///
/// The Timeline control path does not own authoring-to-runtime bindings, so it
/// cannot touch VFX state directly. The marker remains in the player's opaque
/// adapter token bag until `advance_timelines` reaches the composition layer.
pub(super) fn mark_seek(component: &mut TimelinePlayerComponent) {
    component.tokens.set(
        PENDING_SEEK_GENERATION,
        component.player.generation(),
    );
}

/// Returns the newest seek that has not yet been reconstructed.
pub(super) fn pending_seek(component: &TimelinePlayerComponent) -> Option<PendingSeek> {
    let generation = component.player.generation();
    let pending = component.tokens.get(PENDING_SEEK_GENERATION);
    let applied = component.tokens.get(APPLIED_SEEK_GENERATION);
    (pending == Some(generation) && applied != Some(generation)).then_some(PendingSeek {
        generation,
        tick: component.player.tick(),
        state: component.player.state(),
    })
}

/// Records that one pending VFX reconstruction has been consumed.
pub(super) fn mark_seek_applied(tokens: &mut AdapterTokens, seek: PendingSeek) {
    tokens.set(APPLIED_SEEK_GENERATION, seek.generation);
}

/// Applies VFX start/stop/restart boundaries reached by one Timeline step.
///
/// VFX actions are edge-triggered. In particular, a `Restart` clip must not
/// restart every frame while it remains active. `Play` owns its clip interval,
/// so leaving that interval stops the player even when the neutral evaluator
/// lands exactly on the exclusive end tick.
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
            if matches!(clip.payload, CompiledClipPayload::Vfx { .. }) {
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
                clip.payload,
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
            .any(|clip| matches!(clip.payload, CompiledClipPayload::Vfx { .. }))
        {
            continue;
        }
        let active_play = track.clips.iter().any(|clip| {
            clip.contains(evaluation.tick)
                && matches!(
                    clip.payload,
                    CompiledClipPayload::Vfx {
                        action: VfxAction::Play,
                        ..
                    }
                )
        });
        let owned = play_owned(tokens, track);
        match (owned, active_play) {
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

/// Reconstructs every VFX track at an exact seek target using ADR 0125's
/// deterministic preview-seek path.
///
/// This is deliberately VFX-domain reconstruction only. Checkpoint selection,
/// debounce, and cancellation remain the Sequencer/ReplayRequired coordinator's
/// responsibility defined by ADR 0126.
pub(super) fn apply_seek(
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
            .any(|clip| matches!(clip.payload, CompiledClipPayload::Vfx { .. }))
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
    timeline
        .tracks
        .iter()
        .enumerate()
        .find_map(|(track_index, track)| {
            (&track.id == track_id).then(|| {
                track
                    .clips
                    .iter()
                    .position(|clip| &clip.id == clip_id)
                    .map(|clip_index| (track_index, clip_index))
            })
        })
        .flatten()
}

fn apply_action(
    track: &engine_timeline::CompiledTrack,
    action: VfxAction,
    world: &mut World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) {
    let Some(authoring) = track.entity.as_ref() else {
        return;
    };
    let Some(target) = bindings.resolve(authoring) else {
        diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
            authoring: authoring.as_stable_id().as_str().to_owned(),
        });
        return;
    };
    let Some(player) = world.get_component_mut::<VfxPlayer>(target) else {
        diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
            authoring: authoring.as_stable_id().as_str().to_owned(),
            component: "VfxPlayer",
        });
        return;
    };
    match action {
        VfxAction::Play => player.play(),
        VfxAction::Stop => player.stop(),
        VfxAction::Restart => player.restart(),
    }
}

fn stop_track(
    track: &engine_timeline::CompiledTrack,
    world: &mut World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) {
    apply_action(track, VfxAction::Stop, world, bindings, diagnostics);
}

fn play_token(track: &engine_timeline::CompiledTrack) -> String {
    format!("{PLAY_TOKEN_PREFIX}{}", track.id.as_str())
}

fn play_owned(tokens: &AdapterTokens, track: &engine_timeline::CompiledTrack) -> bool {
    tokens.get(&play_token(track)) == Some(1)
}

fn set_play_owned(
    tokens: &mut AdapterTokens,
    track: &engine_timeline::CompiledTrack,
    owned: bool,
) {
    tokens.set(play_token(track), u64::from(owned));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{
        TimelineCameraOverride, TimelineControl, TimelineEvents, advance_timelines,
        apply_timeline_control,
    };
    use crate::transform::{GlobalTransform, Transform};
    use crate::vfx::{VfxPlaybackState, VfxRestartPolicy};
    use engine_authoring::{
        AssetId, CompiledVfxEffect, EntityId, TimelineBinding, TimelineClip, TimelineClipId,
        TimelineClipPayload, TimelineDocument, TimelineTrack, TimelineTrackId, TimelineTrackKind,
        TimelineVfxAction, VfxCapabilityRequirements,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn vfx_document(
        entity: &EntityId,
        action: TimelineVfxAction,
        start: i64,
        end: i64,
    ) -> TimelineDocument {
        let mut document = TimelineDocument::new(TimelineTick(96_000));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Vfx,
            name: "Effect".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(entity.clone()),
                asset: None,
            },
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick(start),
                end: TimelineTick(end),
                payload: TimelineClipPayload::Vfx {
                    effect: AssetId::generate(),
                    action,
                },
            }],
        });
        document
    }

    fn test_player() -> VfxPlayer {
        VfxPlayer::new(
            CompiledVfxEffect {
                source_schema_version: 1,
                seed: 7,
                max_particles: 16,
                emitters: Vec::new(),
                capabilities: VfxCapabilityRequirements::default(),
            },
            false,
            false,
            VfxRestartPolicy::Manual,
            1.0,
            None,
            BTreeMap::new(),
        )
    }

    fn world_with_vfx() -> (World, EntityId, engine_ecs::Entity, TimelineBindings) {
        let mut world = World::new();
        let target = world.spawn().expect("target");
        world
            .add_component(target, Transform::default())
            .expect("transform");
        world
            .add_component(target, GlobalTransform::default())
            .expect("global transform");
        world
            .add_component(target, test_player())
            .expect("VFX player");
        let authoring = EntityId::generate();
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring, target);
        (world, authoring, target, bindings)
    }

    fn step(
        seconds: f32,
        world: &mut World,
        bindings: &TimelineBindings,
        camera: &mut TimelineCameraOverride,
        events: &mut TimelineEvents,
        diagnostics: &mut TimelineDiagnostics,
    ) {
        advance_timelines(
            seconds,
            world,
            bindings,
            camera,
            events,
            diagnostics,
        );
    }

    #[test]
    fn play_owns_exactly_the_clip_interval() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Play,
                0,
                24_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(source, component).expect("timeline");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();

        step(
            0.1,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(
            world
                .get_component::<VfxPlayer>(target)
                .expect("player")
                .playback_state(),
            VfxPlaybackState::Playing
        );

        step(
            0.4,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(
            world
                .get_component::<VfxPlayer>(target)
                .expect("player")
                .playback_state(),
            VfxPlaybackState::Stopped
        );
        assert!(diagnostics.iter().next().is_none());
    }

    #[test]
    fn restart_is_an_edge_action_not_an_active_clip_action() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Restart,
                0,
                48_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(source, component).expect("timeline");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        step(
            0.1,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );
        world
            .get_component_mut::<VfxPlayer>(target)
            .expect("player")
            .instance_mut()
            .step(0.05, glam::Vec3::ZERO);

        step(
            0.1,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );
        let elapsed = world
            .get_component::<VfxPlayer>(target)
            .expect("player")
            .instance()
            .elapsed_seconds();
        assert!((elapsed - 0.05).abs() < 0.0001);
    }

    #[test]
    fn stop_action_stops_when_its_start_is_crossed() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        world
            .get_component_mut::<VfxPlayer>(target)
            .expect("player")
            .play();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Stop,
                12_000,
                24_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(source, component).expect("timeline");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        step(
            0.3,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(
            world
                .get_component::<VfxPlayer>(target)
                .expect("player")
                .playback_state(),
            VfxPlaybackState::Stopped
        );
    }

    #[test]
    fn seek_replays_an_active_play_clip_to_clip_local_time() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Play,
                0,
                48_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        world
            .add_component(source, TimelinePlayerComponent::new(timeline))
            .expect("timeline");
        apply_timeline_control(&mut world, source, TimelineControl::Play).expect("play");
        apply_timeline_control(
            &mut world,
            source,
            TimelineControl::Seek {
                tick: TimelineTick(24_000),
            },
        )
        .expect("seek");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        step(
            0.0,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );

        let player = world.get_component::<VfxPlayer>(target).expect("player");
        assert!((player.instance().elapsed_seconds() - 0.5).abs() < 0.0001);
        assert_eq!(player.playback_state(), VfxPlaybackState::Playing);
        assert!(diagnostics.iter().next().is_none());
    }

    #[test]
    fn seek_after_restart_replays_from_the_restart_boundary() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Restart,
                12_000,
                18_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        world
            .add_component(source, TimelinePlayerComponent::new(timeline))
            .expect("timeline");
        apply_timeline_control(&mut world, source, TimelineControl::Play).expect("play");
        apply_timeline_control(
            &mut world,
            source,
            TimelineControl::Seek {
                tick: TimelineTick(24_000),
            },
        )
        .expect("seek");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        step(
            0.0,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );

        let player = world.get_component::<VfxPlayer>(target).expect("player");
        assert!((player.instance().elapsed_seconds() - 0.25).abs() < 0.0001);
        assert_eq!(player.playback_state(), VfxPlaybackState::Playing);
    }

    #[test]
    fn seek_past_a_play_interval_restores_stopped_state() {
        let (mut world, authoring, target, bindings) = world_with_vfx();
        let timeline = Arc::new(
            engine_timeline::compile_timeline(&vfx_document(
                &authoring,
                TimelineVfxAction::Play,
                0,
                12_000,
            ))
            .expect("compile"),
        );
        let source = world.spawn().expect("source");
        world
            .add_component(source, TimelinePlayerComponent::new(timeline))
            .expect("timeline");
        apply_timeline_control(&mut world, source, TimelineControl::Play).expect("play");
        apply_timeline_control(
            &mut world,
            source,
            TimelineControl::Seek {
                tick: TimelineTick(24_000),
            },
        )
        .expect("seek");

        let mut camera = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        step(
            0.0,
            &mut world,
            &bindings,
            &mut camera,
            &mut events,
            &mut diagnostics,
        );

        assert_eq!(
            world
                .get_component::<VfxPlayer>(target)
                .expect("player")
                .playback_state(),
            VfxPlaybackState::Stopped
        );
    }
}
