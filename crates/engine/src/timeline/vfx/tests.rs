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
    advance_timelines(seconds, world, bindings, camera, events, diagnostics);
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
