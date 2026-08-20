//! Stateful Audio Track composition for Timeline playback (ADR 0126 / ADR 0122).

use super::{
    CompiledClip, CompiledClipPayload, CompiledTimeline, TimelineBindingDiagnostic,
    TimelineBindings, TimelineDiagnostics, TimelineEvaluation, TimelinePlayState,
    TimelineTrackOutput,
};
use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::audio::{
    AudioAsset, AudioEmitter, AudioSystem, AudioVoiceId, AudioVoiceSpatialSettings,
    SpatialAudioRuntime, StereoGains,
};
use engine_authoring::{
    AssetId, EntityId, TIMELINE_TICKS_PER_SECOND, TimelineClipId, TimelineTick, TimelineTrackId,
};
use engine_ecs::{Entity, World};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TimelineAudioVoiceKey {
    source: Entity,
    track: TimelineTrackId,
    clip: TimelineClipId,
}

impl TimelineAudioVoiceKey {
    fn new(source: Entity, track: &TimelineTrackId, clip: &TimelineClipId) -> Self {
        Self {
            source,
            track: track.clone(),
            clip: clip.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineAudioVoice {
    voice_id: AudioVoiceId,
    generation: u64,
}

/// Process-local Audio Track ownership.
///
/// Voice IDs remain entirely runtime-local. The stable track/clip identities
/// are used only to associate a live managed voice with one Timeline player.
#[derive(Debug, Default)]
struct TimelineAudioRuntime {
    voices: HashMap<TimelineAudioVoiceKey, TimelineAudioVoice>,
    last_ticks: HashMap<Entity, TimelineTick>,
}

#[derive(Debug, Clone)]
struct DesiredAudioVoice {
    key: TimelineAudioVoiceKey,
    cue: AssetId,
    offset: i64,
    duration: i64,
    fade_ticks: i64,
    entity: Option<EntityId>,
    stop_fade: f32,
}

/// Applies Audio Track state for one Timeline player.
///
/// The adapter deliberately owns only Timeline-to-audio composition. Decoding,
/// backend voice lifetime, bus gain, and spatial mixing stay in ADR 0122's
/// `AudioSystem` / `SpatialAudioRuntime`.
pub(super) fn apply_audio_evaluation(
    source: Entity,
    timeline: &CompiledTimeline,
    evaluation: &TimelineEvaluation,
    state: TimelinePlayState,
    generation: u64,
    world: &mut World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) {
    let mut runtime = world
        .remove_resource::<TimelineAudioRuntime>()
        .unwrap_or_default();
    let rewound = runtime
        .last_ticks
        .get(&source)
        .is_some_and(|previous| evaluation.tick < *previous);
    runtime.last_ticks.insert(source, evaluation.tick);

    let Some(mut audio) = world.remove_resource::<AudioSystem>() else {
        // Backend voice IDs are meaningful only for the AudioSystem instance
        // that created them. If that owner disappears, discard the Timeline
        // side of the runtime association and rebuild it if audio returns.
        runtime.voices.retain(|key, _| key.source != source);
        world.insert_resource(runtime);
        return;
    };

    if state != TimelinePlayState::Playing {
        stop_source_voices(source, &mut runtime, &mut audio);
        world.insert_resource(audio);
        world.insert_resource(runtime);
        return;
    }

    let desired = desired_audio_voices(source, timeline, evaluation);
    let desired_keys = desired
        .iter()
        .map(|voice| voice.key.clone())
        .collect::<HashSet<_>>();
    stop_undesired_voices(
        source,
        &desired_keys,
        &mut runtime,
        &mut audio,
    );

    let entered = evaluation
        .entered
        .iter()
        .map(|transition| {
            TimelineAudioVoiceKey::new(source, &transition.track, &transition.clip)
        })
        .collect::<HashSet<_>>();

    for desired in desired {
        let restart = runtime
            .voices
            .get(&desired.key)
            .is_some_and(|voice| voice.generation != generation)
            || rewound
            || entered.contains(&desired.key);
        if restart {
            stop_voice(&desired.key, &mut runtime, &mut audio);
        }

        let Some(gains) = output_gains(
            desired.entity.as_ref(),
            fade_gain(desired.offset, desired.duration, desired.fade_ticks) * desired.stop_fade,
            world,
            bindings,
            diagnostics,
        ) else {
            stop_voice(&desired.key, &mut runtime, &mut audio);
            continue;
        };

        if let Some(voice) = runtime.voices.get(&desired.key).copied() {
            if let Err(error) = audio.update_voice(voice.voice_id, gains) {
                log::error!(
                    "failed to update Timeline audio voice for player {source}: {error}"
                );
            }
            continue;
        }

        match start_voice(
            &desired.cue,
            desired.offset,
            gains,
            world,
            &mut audio,
        ) {
            Ok(voice_id) => {
                runtime.voices.insert(
                    desired.key,
                    TimelineAudioVoice {
                        voice_id,
                        generation,
                    },
                );
            }
            Err(message) => {
                log::error!(
                    "failed to start Timeline audio cue `{}` for player {source}: {message}",
                    desired.cue.as_stable_id().as_str()
                );
            }
        }
    }

    world.insert_resource(audio);
    world.insert_resource(runtime);
}

/// Stops Timeline-owned voices whose source player no longer exists.
pub(super) fn cleanup_stale_sources(world: &mut World, live_sources: &[Entity]) {
    let Some(mut runtime) = world.remove_resource::<TimelineAudioRuntime>() else {
        return;
    };
    let live = live_sources.iter().copied().collect::<HashSet<_>>();
    runtime.last_ticks.retain(|source, _| live.contains(source));

    let stale = runtime
        .voices
        .keys()
        .filter(|key| !live.contains(&key.source))
        .cloned()
        .collect::<Vec<_>>();

    let Some(mut audio) = world.remove_resource::<AudioSystem>() else {
        runtime.voices.clear();
        world.insert_resource(runtime);
        return;
    };
    for key in stale {
        stop_voice(&key, &mut runtime, &mut audio);
    }
    world.insert_resource(audio);
    world.insert_resource(runtime);
}

fn stop_undesired_voices(
    source: Entity,
    desired: &HashSet<TimelineAudioVoiceKey>,
    runtime: &mut TimelineAudioRuntime,
    audio: &mut AudioSystem,
) {
    let stale = runtime
        .voices
        .keys()
        .filter(|key| key.source == source && !desired.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    for key in stale {
        stop_voice(&key, runtime, audio);
    }
}

fn stop_source_voices(
    source: Entity,
    runtime: &mut TimelineAudioRuntime,
    audio: &mut AudioSystem,
) {
    let keys = runtime
        .voices
        .keys()
        .filter(|key| key.source == source)
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        stop_voice(&key, runtime, audio);
    }
}

fn stop_voice(
    key: &TimelineAudioVoiceKey,
    runtime: &mut TimelineAudioRuntime,
    audio: &mut AudioSystem,
) {
    let Some(voice) = runtime.voices.remove(key) else {
        return;
    };
    if let Err(error) = audio.stop_voice(voice.voice_id) {
        log::error!(
            "failed to stop Timeline audio voice for player {}: {error}",
            key.source
        );
    }
}

fn desired_audio_voices(
    source: Entity,
    timeline: &CompiledTimeline,
    evaluation: &TimelineEvaluation,
) -> Vec<DesiredAudioVoice> {
    evaluation
        .active
        .iter()
        .filter_map(|active| {
            let TimelineTrackOutput::Audio {
                cue,
                play,
                fade_ticks,
                entity,
            } = &active.output
            else {
                return None;
            };
            if !*play {
                return None;
            }
            let clip = compiled_clip(timeline, &active.track, &active.clip)?;
            let stop_fade =
                stop_fade_gain(timeline, cue, clip.start, evaluation.tick);
            if stop_fade <= 0.0 {
                return None;
            }
            Some(DesiredAudioVoice {
                key: TimelineAudioVoiceKey::new(source, &active.track, &active.clip),
                cue: cue.clone(),
                offset: active.offset,
                duration: clip.duration(),
                fade_ticks: *fade_ticks,
                entity: entity.clone(),
                stop_fade,
            })
        })
        .collect()
}

fn compiled_clip<'a>(
    timeline: &'a CompiledTimeline,
    track: &TimelineTrackId,
    clip: &TimelineClipId,
) -> Option<&'a CompiledClip> {
    timeline
        .track(track)?
        .clips
        .iter()
        .find(|candidate| &candidate.id == clip)
}

fn stop_fade_gain(
    timeline: &CompiledTimeline,
    cue: &AssetId,
    play_start: TimelineTick,
    target: TimelineTick,
) -> f32 {
    let stop = timeline
        .tracks
        .iter()
        .filter(|track| track.enabled)
        .flat_map(|track| &track.clips)
        .filter_map(|clip| {
            let CompiledClipPayload::Audio {
                cue: stop_cue,
                play: false,
                fade_ticks,
            } = &clip.payload
            else {
                return None;
            };
            (stop_cue == cue && clip.start >= play_start && clip.start <= target)
                .then_some((clip.start, *fade_ticks))
        })
        .min_by_key(|(start, _)| *start);
    let Some((stop_start, fade_ticks)) = stop else {
        return 1.0;
    };
    if fade_ticks <= 0 {
        return 0.0;
    }
    let elapsed = target.get().saturating_sub(stop_start.get()).max(0);
    (1.0 - elapsed as f32 / fade_ticks as f32).clamp(0.0, 1.0)
}
fn output_gains(
    entity: Option<&EntityId>,
    fade: f32,
    world: &World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) -> Option<StereoGains> {
    let Some(authoring) = entity else {
        return Some(StereoGains {
            left: fade,
            right: fade,
        });
    };
    let Some(target) = bindings.resolve(authoring) else {
        diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
            authoring: authoring.as_stable_id().as_str().to_owned(),
        });
        return None;
    };
    let Some(emitter) = world.get_component::<AudioEmitter>(target) else {
        diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
            authoring: authoring.as_stable_id().as_str().to_owned(),
            component: "AudioEmitter",
        });
        return None;
    };
    let settings = AudioVoiceSpatialSettings {
        volume: emitter.volume,
        spatial_blend: emitter.spatial_blend,
        min_distance: emitter.min_distance,
        max_distance: emitter.max_distance,
        rolloff: emitter.rolloff,
    };
    let Some(gains) = world
        .get_resource::<SpatialAudioRuntime>()
        .and_then(|runtime| runtime.gains_for_entity(target, settings))
    else {
        log::warn!(
            "Timeline spatial audio target {target} has no copied ADR 0122 emitter pose"
        );
        return None;
    };
    Some(StereoGains {
        left: gains.left * fade,
        right: gains.right * fade,
    })
}

fn start_voice(
    cue: &AssetId,
    offset_ticks: i64,
    gains: StereoGains,
    world: &mut World,
    audio: &mut AudioSystem,
) -> Result<AudioVoiceId, String> {
    let path = world
        .get_resource::<AssetManifest>()
        .and_then(|manifest| manifest.get(cue))
        .map(|entry| entry.path.clone())
        .ok_or_else(|| {
            format!(
                "asset `{}` is missing from the runtime manifest",
                cue.as_stable_id().as_str()
            )
        })?;

    let mut server = world
        .remove_resource::<AssetServer>()
        .ok_or_else(|| "AssetServer resource is unavailable".to_owned())?;
    let Some(mut assets) = world.remove_resource::<Assets<AudioAsset>>() else {
        world.insert_resource(server);
        return Err("audio asset storage resource is unavailable".to_owned());
    };

    let result = (|| {
        let handle = server
            .load_audio(cue.clone(), &path, &mut assets)
            .map_err(|error| error.to_string())?;
        let asset = assets
            .get(&handle)
            .ok_or_else(|| "decoded audio asset disappeared after loading".to_owned())?;
        audio
            .start_voice_at(asset, gains, ticks_to_duration(offset_ticks))
            .map_err(|error| error.to_string())
    })();

    world.insert_resource(assets);
    world.insert_resource(server);
    result
}

fn ticks_to_duration(ticks: i64) -> Duration {
    Duration::from_secs_f64(
        ticks.max(0) as f64 / TIMELINE_TICKS_PER_SECOND as f64,
    )
}

fn fade_gain(offset: i64, duration: i64, fade_ticks: i64) -> f32 {
    if fade_ticks <= 0 {
        return 1.0;
    }
    let fade_ticks = fade_ticks as f32;
    let fade_in = (offset.max(0) as f32 / fade_ticks).clamp(0.0, 1.0);
    let remaining = duration.saturating_sub(offset).max(0);
    let fade_out = (remaining as f32 / fade_ticks).clamp(0.0, 1.0);
    fade_in.min(fade_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        TimelineAudioAction, TimelineBinding, TimelineClip, TimelineClipPayload, TimelineDocument,
        TimelineTrack, TimelineTrackKind,
    };
    use engine_timeline::{TimelinePlayer, TimelineSeek, compile_timeline};

    fn audio_document(cue: &AssetId, stop_fade_ticks: Option<i64>) -> CompiledTimeline {
        let mut document = TimelineDocument::new(TimelineTick(200));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Audio,
            name: "Voice".to_owned(),
            enabled: true,
            binding: TimelineBinding::default(),
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick(200),
                payload: TimelineClipPayload::Audio {
                    cue: cue.clone(),
                    action: TimelineAudioAction::Play,
                    fade_ticks: TimelineTick(20),
                },
            }],
        });
        if let Some(stop_fade_ticks) = stop_fade_ticks {
            document.tracks.push(TimelineTrack {
                id: TimelineTrackId::generate(),
                kind: TimelineTrackKind::Audio,
                name: "Stop".to_owned(),
                enabled: true,
                binding: TimelineBinding::default(),
                clips: vec![TimelineClip {
                    id: TimelineClipId::generate(),
                    start: TimelineTick(100),
                    end: TimelineTick(101),
                    payload: TimelineClipPayload::Audio {
                        cue: cue.clone(),
                        action: TimelineAudioAction::Stop,
                        fade_ticks: TimelineTick(stop_fade_ticks),
                    },
                }],
            });
        }
        compile_timeline(&document).expect("audio Timeline")
    }

    #[test]
    fn fade_gain_uses_both_clip_boundaries() {
        assert_eq!(fade_gain(0, 100, 20), 0.0);
        assert!((fade_gain(10, 100, 20) - 0.5).abs() < f32::EPSILON);
        assert_eq!(fade_gain(20, 100, 20), 1.0);
        assert_eq!(fade_gain(80, 100, 20), 1.0);
        assert!((fade_gain(90, 100, 20) - 0.5).abs() < f32::EPSILON);
        assert_eq!(fade_gain(100, 100, 20), 0.0);
        assert_eq!(fade_gain(50, 100, 0), 1.0);
    }

    #[test]
    fn seek_restores_a_play_clip_at_its_clip_local_offset() {
        let cue = AssetId::generate();
        let timeline = audio_document(&cue, None);
        let mut player = TimelinePlayer::new();
        player.play();
        let evaluation = player.seek(&timeline, TimelineTick(75), TimelineSeek::Playback);
        let desired = desired_audio_voices(Entity::new(7, 0), &timeline, &evaluation);

        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].cue, cue);
        assert_eq!(desired[0].offset, 75);
        assert_eq!(desired[0].duration, 200);
    }

    #[test]
    fn a_stop_action_suppresses_an_earlier_play_during_seek_reconstruction() {
        let cue = AssetId::generate();
        let timeline = audio_document(&cue, Some(0));
        let mut player = TimelinePlayer::new();
        player.play();

        let before_stop =
            player.seek(&timeline, TimelineTick(75), TimelineSeek::Playback);
        assert_eq!(
            desired_audio_voices(Entity::new(7, 0), &timeline, &before_stop).len(),
            1
        );

        let after_stop =
            player.seek(&timeline, TimelineTick(150), TimelineSeek::Playback);
        assert!(
            desired_audio_voices(Entity::new(7, 0), &timeline, &after_stop).is_empty()
        );
    }

    #[test]
    fn a_stop_action_fades_the_existing_cue_before_retiring_it() {
        let cue = AssetId::generate();
        let timeline = audio_document(&cue, Some(20));

        assert_eq!(
            stop_fade_gain(&timeline, &cue, TimelineTick::ZERO, TimelineTick(99)),
            1.0
        );
        assert_eq!(
            stop_fade_gain(&timeline, &cue, TimelineTick::ZERO, TimelineTick(100)),
            1.0
        );
        assert!(
            (stop_fade_gain(&timeline, &cue, TimelineTick::ZERO, TimelineTick(110)) - 0.5).abs()
                < f32::EPSILON
        );
        assert_eq!(
            stop_fade_gain(&timeline, &cue, TimelineTick::ZERO, TimelineTick(120)),
            0.0
        );
    }

    #[test]
    fn tick_offsets_convert_to_audio_time_without_frame_rate_rounding() {
        assert_eq!(ticks_to_duration(TIMELINE_TICKS_PER_SECOND), Duration::from_secs(1));
        assert_eq!(
            ticks_to_duration(TIMELINE_TICKS_PER_SECOND / 2),
            Duration::from_millis(500)
        );
    }
}
