//! Timeline playback semantics required by ADR 0126's verification list.

use engine_authoring::{
    EntityId, TimelineBinding, TimelineClip, TimelineClipId, TimelineClipPayload, TimelineDocument,
    TimelineMarker, TimelineMarkerId, TimelineProperty, TimelineTick, TimelineTrack,
    TimelineTrackId, TimelineTrackKind,
};
use engine_timeline::{
    CompiledTimeline, LoopRegion, TimelinePlayState, TimelinePlayer, TimelineSeek,
    TimelineTrackOutput, compile_timeline,
};

fn event_track(name: &str, clips: Vec<TimelineClip>) -> TimelineTrack {
    TimelineTrack {
        id: TimelineTrackId::generate(),
        kind: TimelineTrackKind::Event,
        name: name.to_owned(),
        enabled: true,
        binding: TimelineBinding::default(),
        clips,
    }
}

fn event_clip(event: &str, start: i64, end: i64) -> TimelineClip {
    TimelineClip {
        id: TimelineClipId::generate(),
        start: TimelineTick(start),
        end: TimelineTick(end),
        payload: TimelineClipPayload::Event {
            event: event.to_owned(),
        },
    }
}

fn property_track(name: &str, entity: EntityId, clips: Vec<TimelineClip>) -> TimelineTrack {
    TimelineTrack {
        id: TimelineTrackId::generate(),
        kind: TimelineTrackKind::Property,
        name: name.to_owned(),
        enabled: true,
        binding: TimelineBinding {
            entity: Some(entity),
            asset: None,
        },
        clips,
    }
}

fn ramp_clip(start: i64, end: i64, from: f32, to: f32) -> TimelineClip {
    TimelineClip {
        id: TimelineClipId::generate(),
        start: TimelineTick(start),
        end: TimelineTick(end),
        payload: TimelineClipPayload::Property {
            property: TimelineProperty::TranslationX,
            keys: vec![
                engine_authoring::TimelineKey {
                    tick: TimelineTick::ZERO,
                    value: from,
                    interpolation: engine_authoring::TimelineInterpolation::Linear,
                },
                engine_authoring::TimelineKey {
                    tick: TimelineTick(end - start),
                    value: to,
                    interpolation: engine_authoring::TimelineInterpolation::Linear,
                },
            ],
        },
    }
}

fn marker(event: &str, tick: i64) -> TimelineMarker {
    TimelineMarker {
        id: TimelineMarkerId::generate(),
        tick: TimelineTick(tick),
        name: event.to_owned(),
        event: event.to_owned(),
    }
}

fn one_second_timeline() -> CompiledTimeline {
    let mut document = TimelineDocument::new(TimelineTick(48_000));
    document.tracks.push(property_track(
        "Move",
        EntityId::generate(),
        vec![ramp_clip(0, 24_000, 0.0, 10.0)],
    ));
    document.markers.push(marker("half", 24_000));
    compile_timeline(&document).expect("compile")
}

#[test]
fn variable_frame_deltas_land_on_the_same_tick_as_one_combined_delta() {
    let timeline = one_second_timeline();
    let mut stepped = TimelinePlayer::new();
    let mut single = TimelinePlayer::new();
    stepped.play();
    single.play();

    // 1/60 second thirty times must equal half a second exactly, even though
    // one frame is 800 ticks and a float accumulation would drift.
    for _ in 0..30 {
        stepped.advance(&timeline, 1.0 / 60.0);
    }
    single.advance(&timeline, 0.5);

    assert_eq!(stepped.tick(), TimelineTick(24_000));
    assert_eq!(stepped.tick(), single.tick());
}

#[test]
fn a_marker_fires_exactly_once_when_the_playhead_crosses_it() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();
    player.play();

    let mut fired = 0;
    for _ in 0..60 {
        fired += player
            .advance(&timeline, 1.0 / 60.0)
            .events
            .iter()
            .filter(|event| event.event == "half")
            .count();
    }
    assert_eq!(fired, 1);
}

#[test]
fn a_loop_boundary_neither_loses_a_marker_nor_fires_it_twice() {
    let mut document = TimelineDocument::new(TimelineTick(48_000));
    document
        .tracks
        .push(event_track("Beats", vec![event_clip("beat", 0, 4_000)]));
    document.markers.push(marker("loop_marker", 8_000));
    let timeline = compile_timeline(&document).expect("compile");

    let mut player = TimelinePlayer::new();
    assert!(player.set_loop_region(Some(LoopRegion {
        start: TimelineTick(0),
        end: TimelineTick(12_000),
        count: Some(3),
    })));
    player.play();

    let mut markers = 0;
    let mut beats = 0;
    // Three loops of 0.25 s each; advance well past them in small steps.
    for _ in 0..120 {
        let evaluation = player.advance(&timeline, 1.0 / 120.0);
        markers += evaluation
            .events
            .iter()
            .filter(|event| event.event == "loop_marker")
            .count();
        beats += evaluation
            .events
            .iter()
            .filter(|event| event.event == "beat")
            .count();
    }
    assert_eq!(markers, 3, "one crossing per completed loop");
    assert_eq!(beats, 3, "the event clip is entered once per loop");
    assert_eq!(player.loops_completed(), 3);
}

#[test]
fn a_scrub_samples_visual_state_and_suppresses_gameplay_events() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();

    let scrub = player.seek(&timeline, TimelineTick(24_000), TimelineSeek::Scrub);
    assert!(scrub.events.is_empty(), "scrub must not fire events");
    assert_eq!(scrub.tick, TimelineTick(24_000));

    let preview = player.seek(&timeline, TimelineTick(24_000), TimelineSeek::PreviewEvents);
    assert_eq!(preview.events.len(), 1, "event preview is explicit opt-in");
}

#[test]
fn a_scrub_inside_a_clip_samples_the_curve_at_that_tick() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();
    let evaluation = player.seek(&timeline, TimelineTick(12_000), TimelineSeek::Scrub);
    let active = evaluation.active.first().expect("active clip");
    match &active.output {
        TimelineTrackOutput::Property { value, .. } => assert!((value - 5.0).abs() < 0.001),
        other => panic!("unexpected output {other:?}"),
    }
    assert_eq!(active.offset, 12_000);
}

#[test]
fn overlapping_clips_on_different_tracks_evaluate_in_authored_track_order() {
    let entity = EntityId::generate();
    let mut document = TimelineDocument::new(TimelineTick(48_000));
    let first = property_track(
        "First",
        entity.clone(),
        vec![ramp_clip(0, 24_000, 0.0, 1.0)],
    );
    let second = property_track("Second", entity, vec![ramp_clip(0, 24_000, 5.0, 6.0)]);
    let first_id = first.id.clone();
    let second_id = second.id.clone();
    document.tracks.push(first);
    document.tracks.push(second);
    let timeline = compile_timeline(&document).expect("compile");

    let mut player = TimelinePlayer::new();
    let evaluation = player.seek(&timeline, TimelineTick(1_000), TimelineSeek::Scrub);
    let order = evaluation
        .active
        .iter()
        .map(|clip| clip.track.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, vec![first_id, second_id]);

    // The same schedule evaluated again produces the same order.
    let repeat = player.seek(&timeline, TimelineTick(1_000), TimelineSeek::Scrub);
    assert_eq!(evaluation.active, repeat.active);
}

#[test]
fn two_players_share_one_compiled_timeline_without_leaking_state() {
    let timeline = one_second_timeline();
    let mut left = TimelinePlayer::new();
    let mut right = TimelinePlayer::new();
    left.play();
    right.play();
    assert!(right.set_rate(2.0));

    for _ in 0..10 {
        left.advance(&timeline, 1.0 / 60.0);
        right.advance(&timeline, 1.0 / 60.0);
    }

    assert_eq!(left.tick(), TimelineTick(8_000));
    assert_eq!(right.tick(), TimelineTick(16_000));
    assert_eq!(left.rate(), 1.0);
}

#[test]
fn playback_holds_at_the_duration_instead_of_running_past_it() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();
    player.play();
    player.advance(&timeline, 2.0);
    assert_eq!(player.tick(), TimelineTick(48_000));
    assert_eq!(player.state(), TimelinePlayState::Paused);
}

#[test]
fn stopping_resets_the_playhead_and_advances_the_generation() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();
    player.play();
    player.advance(&timeline, 0.25);
    let generation = player.generation();
    player.stop();
    assert_eq!(player.tick(), TimelineTick::ZERO);
    assert_eq!(player.state(), TimelinePlayState::Stopped);
    assert_ne!(player.generation(), generation);
}

#[test]
fn a_negative_or_non_finite_rate_is_refused_rather_than_clamped() {
    let mut player = TimelinePlayer::new();
    assert!(!player.set_rate(-1.0));
    assert!(!player.set_rate(f32::NAN));
    assert_eq!(player.rate(), 1.0);
}

#[test]
fn a_disabled_track_contributes_nothing_to_evaluation() {
    let mut document = TimelineDocument::new(TimelineTick(48_000));
    let mut track = event_track("Beats", vec![event_clip("beat", 0, 4_000)]);
    track.enabled = false;
    document.tracks.push(track);
    let timeline = compile_timeline(&document).expect("compile");

    let mut player = TimelinePlayer::new();
    player.play();
    let evaluation = player.advance(&timeline, 0.05);
    assert!(evaluation.events.is_empty());
    assert!(evaluation.active.is_empty());
}

#[test]
fn a_stopped_player_reports_no_active_clip_at_tick_zero() {
    let timeline = one_second_timeline();
    let mut player = TimelinePlayer::new();
    // The property clip starts at tick zero, so a stopped player would report
    // it if evaluation ignored play state.
    let evaluation = player.advance(&timeline, 1.0 / 60.0);
    assert!(evaluation.active.is_empty());
    assert_eq!(player.state(), TimelinePlayState::Stopped);

    player.play();
    let playing = player.advance(&timeline, 1.0 / 60.0);
    assert_eq!(playing.active.len(), 1);
}
