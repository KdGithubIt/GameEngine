//! Compatibility facade for platform audio plus project-command adaptation.

pub use engine_platform::audio::*;

use std::collections::{HashMap, HashSet, VecDeque};

use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::transform::GlobalTransform;
use engine_authoring::{AssetId, StableId};
use engine_ecs::{Entity, Query, Res, ResMut};
use engine_platform::spatial_audio::{
    spatial_stereo_gains, AudioEmitterPose, AudioListenerPose, AudioVoiceSpatialSettings,
};

/// Validated spatial policy for one project Rust sound request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GameSpatialAudioOptions {
    pub(crate) volume: f32,
    pub(crate) spatial_blend: f32,
    pub(crate) min_distance: f32,
    pub(crate) max_distance: f32,
    pub(crate) rolloff: AudioRolloffMode,
    pub(crate) looping: bool,
}

impl GameSpatialAudioOptions {
    fn settings(self) -> AudioVoiceSpatialSettings {
        AudioVoiceSpatialSettings {
            volume: self.volume,
            spatial_blend: self.spatial_blend,
            min_distance: self.min_distance,
            max_distance: self.max_distance,
            rolloff: self.rolloff,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GameSpatialVoice {
    source: Entity,
    voice_id: AudioVoiceId,
    options: GameSpatialAudioOptions,
}

/// Process-local ownership of active authored spatial-audio voices.
///
/// Runtime entity and backend voice identities never cross the authoring or
/// project-Rust boundaries.
#[derive(Debug, Default)]
pub(crate) struct SpatialAudioRuntime {
    voices: HashMap<Entity, AudioVoiceId>,
    game_voices: Vec<GameSpatialVoice>,
}

/// Selects the active listener and synchronizes authored emitters to managed voices.
///
/// This final-composition system is intentionally engine-owned: it reads rig
/// world transforms and platform audio contracts without making either lower
/// runtime domain depend on the other.
pub(crate) fn spatial_audio_system(
    mut emitters: Query<(&mut AudioEmitter, &GlobalTransform)>,
    mut listeners: Query<(&AudioListener, &GlobalTransform)>,
    assets: Res<Assets<AudioAsset>>,
    mut audio: Option<ResMut<AudioSystem>>,
    mut runtime: ResMut<SpatialAudioRuntime>,
) {
    let Some(audio) = audio.as_deref_mut() else {
        runtime.voices.clear();
        runtime.game_voices.clear();
        for (_, (emitter, _)) in &mut emitters {
            if emitter.autoplay && emitter.state() == &AuthoredAudioState::Pending {
                emitter.mark_playback_unavailable();
            }
        }
        return;
    };

    let completed = audio.drain_completed_voices().into_iter().collect::<HashSet<_>>();
    runtime
        .voices
        .retain(|_, voice_id| !completed.contains(voice_id));
    runtime
        .game_voices
        .retain(|voice| !completed.contains(&voice.voice_id));

    let listener = active_listener_pose(&mut listeners);
    let mut live_emitters = HashSet::new();

    for (entity, (emitter, transform)) in &mut emitters {
        live_emitters.insert(entity);
        let settings = AudioVoiceSpatialSettings {
            volume: emitter.volume,
            spatial_blend: emitter.spatial_blend,
            min_distance: emitter.min_distance,
            max_distance: emitter.max_distance,
            rolloff: emitter.rolloff,
        };
        let gains = emitter_gains(listener, emitter_pose(transform), settings);

        if let Some(voice_id) = runtime.voices.get(&entity).copied() {
            if let Err(error) = audio.update_voice(voice_id, gains) {
                log::error!("failed to update spatial audio voice for entity {entity}: {error}");
            }
            continue;
        }

        if !emitter.autoplay || emitter.state() != &AuthoredAudioState::Pending {
            continue;
        }
        let Some(asset) = assets.get(&emitter.clip) else {
            emitter.mark_playback_failed("decoded clip handle is missing");
            continue;
        };
        match audio.start_voice(asset, gains, emitter.looping) {
            Ok(voice_id) => {
                runtime.voices.insert(entity, voice_id);
                emitter.mark_playback_started();
            }
            Err(error) => emitter.mark_playback_failed(error.to_string()),
        }
    }

    let stale_entities = runtime
        .voices
        .keys()
        .filter(|entity| !live_emitters.contains(entity))
        .copied()
        .collect::<Vec<_>>();
    for entity in stale_entities {
        if let Some(voice_id) = runtime.voices.remove(&entity)
            && let Err(error) = audio.stop_voice(voice_id)
        {
            log::error!("failed to stop stale spatial audio voice for entity {entity}: {error}");
        }
    }
}

fn active_listener_pose(
    listeners: &mut Query<(&AudioListener, &GlobalTransform)>,
) -> Option<AudioListenerPose> {
    let mut selected: Option<(i64, Entity, AudioListenerPose)> = None;
    for (entity, (listener, transform)) in listeners {
        if !listener.enabled {
            continue;
        }
        let pose = listener_pose(transform);
        let should_select = match selected.as_ref() {
            Some((priority, selected_entity, _)) => {
                listener.priority > *priority
                    || (listener.priority == *priority && entity < *selected_entity)
            }
            None => true,
        };
        if should_select {
            selected = Some((listener.priority, entity, pose));
        }
    }
    selected.map(|(_, _, pose)| pose)
}

fn listener_pose(transform: &GlobalTransform) -> AudioListenerPose {
    let matrix = transform.matrix();
    AudioListenerPose {
        position: matrix.col(3).truncate().to_array(),
        right: matrix.col(0).truncate().to_array(),
    }
}

fn emitter_pose(transform: &GlobalTransform) -> AudioEmitterPose {
    AudioEmitterPose {
        position: transform.matrix().col(3).truncate().to_array(),
    }
}

fn emitter_gains(
    listener: Option<AudioListenerPose>,
    emitter: AudioEmitterPose,
    settings: AudioVoiceSpatialSettings,
) -> StereoGains {
    match listener {
        Some(listener) => spatial_stereo_gains(listener, emitter, settings),
        None => {
            let volume = finite_unit(settings.volume);
            let non_spatial = 1.0 - finite_unit(settings.spatial_blend);
            StereoGains {
                left: volume * non_spatial,
                right: volume * non_spatial,
            }
        }
    }
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Maximum pending project-Rust audio requests.
pub const MAX_GAME_AUDIO_COMMANDS: usize = 256;

/// One validated audio request queued by a project Rust callback.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GameAudioCommand {
    PlaySoundEffect { asset_id: String },
    PlaySpatialSoundEffect {
        asset_id: String,
        source: Entity,
        options: GameSpatialAudioOptions,
    },
    PlayBackgroundMusic { asset_id: String, fade_seconds: f32 },
    StopBackgroundMusic,
    SetMasterVolume(f32),
    SetBackgroundMusicVolume(f32),
    SetSoundEffectVolume(f32),
}

/// Bounded bridge between exclusive project callbacks and the audio backend.
#[derive(Debug, Default)]
pub(crate) struct GameAudioCommandQueue {
    commands: VecDeque<GameAudioCommand>,
}

impl GameAudioCommandQueue {
    pub(crate) fn len(&self) -> usize {
        self.commands.len()
    }

    pub(crate) fn push_preflighted(&mut self, command: GameAudioCommand) {
        assert!(
            self.commands.len() < MAX_GAME_AUDIO_COMMANDS,
            "game audio queue capacity must be checked during atomic preflight"
        );
        self.commands.push_back(command);
    }
}

/// Resolves and applies queued project-Rust audio requests.
pub(crate) fn game_audio_effect_system(
    mut queue: ResMut<GameAudioCommandQueue>,
    mut transforms: Query<&GlobalTransform>,
    mut listeners: Query<(&AudioListener, &GlobalTransform)>,
    mut runtime: ResMut<SpatialAudioRuntime>,
    mut audio_system: Option<ResMut<AudioSystem>>,
    mut asset_server: Option<ResMut<AssetServer>>,
    manifest: Option<Res<AssetManifest>>,
    mut audio_assets: Option<ResMut<Assets<AudioAsset>>>,
) {
    let listener = active_listener_pose(&mut listeners);
    let poses = (&mut transforms)
        .into_iter()
        .map(|(entity, transform)| (entity, emitter_pose(transform)))
        .collect::<HashMap<_, _>>();

    if let Some(audio) = audio_system.as_deref_mut() {
        runtime.game_voices.retain(|voice| {
            let Some(emitter) = poses.get(&voice.source).copied() else {
                if let Err(error) = audio.stop_voice(voice.voice_id) {
                    log::error!("failed to stop stale project spatial voice: {error}");
                }
                return false;
            };
            let gains = emitter_gains(listener, emitter, voice.options.settings());
            if let Err(error) = audio.update_voice(voice.voice_id, gains) {
                log::error!("failed to update project spatial voice: {error}");
            }
            true
        });
    } else {
        runtime.game_voices.clear();
    }

    for command in queue.commands.drain(..) {
        let result = match command {
            GameAudioCommand::StopBackgroundMusic => {
                audio_system.as_deref_mut().map(AudioSystem::stop_bgm)
            }
            GameAudioCommand::SetMasterVolume(volume) => audio_system
                .as_deref_mut()
                .map(|audio| audio.set_master_volume(volume)),
            GameAudioCommand::SetBackgroundMusicVolume(volume) => audio_system
                .as_deref_mut()
                .map(|audio| audio.set_bgm_volume(volume)),
            GameAudioCommand::SetSoundEffectVolume(volume) => audio_system
                .as_deref_mut()
                .map(|audio| audio.set_se_volume(volume)),
            GameAudioCommand::PlaySoundEffect { asset_id } => resolve_and_play(
                &asset_id,
                None,
                false,
                audio_system.as_deref_mut(),
                asset_server.as_deref_mut(),
                manifest.as_deref(),
                audio_assets.as_deref_mut(),
            ),
            GameAudioCommand::PlaySpatialSoundEffect {
                asset_id,
                source,
                options,
            } => {
                let Some(emitter) = poses.get(&source).copied() else {
                    log::warn!("project Rust spatial audio source {source} has no GlobalTransform");
                    continue;
                };
                let gains = emitter_gains(listener, emitter, options.settings());
                match resolve_and_start_spatial(
                    &asset_id,
                    gains,
                    options.looping,
                    audio_system.as_deref_mut(),
                    asset_server.as_deref_mut(),
                    manifest.as_deref(),
                    audio_assets.as_deref_mut(),
                ) {
                    Some(Ok(voice_id)) => {
                        runtime.game_voices.push(GameSpatialVoice {
                            source,
                            voice_id,
                            options,
                        });
                        Some(Ok(()))
                    }
                    Some(Err(error)) => Some(Err(error)),
                    None => None,
                }
            }
            GameAudioCommand::PlayBackgroundMusic {
                asset_id,
                fade_seconds,
            } => resolve_and_play(
                &asset_id,
                Some(fade_seconds),
                true,
                audio_system.as_deref_mut(),
                asset_server.as_deref_mut(),
                manifest.as_deref(),
                audio_assets.as_deref_mut(),
            ),
        };
        match result {
            Some(Err(error)) => log::error!("project Rust audio command failed: {error}"),
            None => log::warn!(
                "project Rust audio command skipped because audio host resources are unavailable"
            ),
            Some(Ok(())) => {}
        }
    }
}

fn resolve_and_start_spatial(
    asset_id: &str,
    gains: StereoGains,
    looping: bool,
    audio: Option<&mut AudioSystem>,
    server: Option<&mut AssetServer>,
    manifest: Option<&AssetManifest>,
    assets: Option<&mut Assets<AudioAsset>>,
) -> Option<Result<AudioVoiceId, AudioError>> {
    let stable = StableId::new(asset_id);
    let Ok(asset_id) = AssetId::from_stable_id(stable) else {
        log::error!("project Rust audio asset ID `{asset_id}` became invalid after preflight");
        return None;
    };
    let entry = manifest?.get(&asset_id)?;
    let server = server?;
    let assets = assets?;
    let audio = audio?;
    let handle = match server.load_audio(asset_id, &entry.path, assets) {
        Ok(handle) => handle,
        Err(error) => {
            return Some(Err(AudioError::Playback {
                message: error.to_string(),
            }));
        }
    };
    let asset = assets.get(&handle)?;
    Some(audio.start_voice(asset, gains, looping))
}

fn resolve_and_play(
    asset_id: &str,
    fade_seconds: Option<f32>,
    background_music: bool,
    audio: Option<&mut AudioSystem>,
    server: Option<&mut AssetServer>,
    manifest: Option<&AssetManifest>,
    assets: Option<&mut Assets<AudioAsset>>,
) -> Option<Result<(), AudioError>> {
    let stable = StableId::new(asset_id);
    let Ok(asset_id) = AssetId::from_stable_id(stable) else {
        log::error!("project Rust audio asset ID `{asset_id}` became invalid after preflight");
        return Some(Ok(()));
    };
    let entry = manifest?.get(&asset_id)?;
    let server = server?;
    let assets = assets?;
    let audio = audio?;
    let handle = match server.load_audio(asset_id, &entry.path, assets) {
        Ok(handle) => handle,
        Err(error) => {
            return Some(Err(AudioError::Playback {
                message: error.to_string(),
            }));
        }
    };
    let asset = assets.get(&handle)?;
    Some(if background_music {
        if let Some(fade_seconds) = fade_seconds.filter(|seconds| *seconds > 0.0) {
            audio.crossfade_bgm(asset, fade_seconds)
        } else {
            audio.play_bgm(asset)
        }
    } else {
        audio.play_se(asset)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_listener_selection_is_deterministic_and_ignores_disabled_listeners() {
        use crate::transform::GlobalTransform;
        use glam::{Mat4, Vec3};

        let mut world = engine_ecs::World::new();
        let first = world
            .spawn_with(AudioListener {
                enabled: true,
                priority: 0,
            })
            .expect("listener entity");
        world
            .add_component(
                first,
                GlobalTransform(Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0))),
            )
            .expect("listener transform");
        let second = world
            .spawn_with(AudioListener {
                enabled: true,
                priority: 10,
            })
            .expect("listener entity");
        world
            .add_component(
                second,
                GlobalTransform(Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0))),
            )
            .expect("listener transform");
        let disabled = world
            .spawn_with(AudioListener {
                enabled: false,
                priority: 100,
            })
            .expect("listener entity");
        world
            .add_component(
                disabled,
                GlobalTransform(Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0))),
            )
            .expect("listener transform");

        let mut query = engine_ecs::Query::new(&mut world);
        let pose = active_listener_pose(&mut query).expect("enabled listener");
        assert_eq!(pose.position, [2.0, 0.0, 0.0]);
        assert!(first != second);
    }

    #[test]
    fn listener_pose_uses_world_rotation_for_stereo_right_axis() {
        use crate::transform::GlobalTransform;
        use glam::{Mat4, Quat, Vec3};

        let transform = GlobalTransform(Mat4::from_rotation_translation(
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            Vec3::new(4.0, 5.0, 6.0),
        ));
        let pose = listener_pose(&transform);

        assert_eq!(pose.position, [4.0, 5.0, 6.0]);
        assert!(pose.right[0].abs() < 1.0e-6);
        assert!((pose.right[2] + 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn missing_listener_preserves_only_the_non_spatial_mix() {
        let gains = emitter_gains(
            None,
            AudioEmitterPose {
                position: [100.0, 0.0, 0.0],
            },
            AudioVoiceSpatialSettings {
                volume: 0.8,
                spatial_blend: 0.25,
                min_distance: 1.0,
                max_distance: 10.0,
                rolloff: AudioRolloffMode::Linear,
            },
        );

        assert!((gains.left - 0.6).abs() < 1.0e-6);
        assert!((gains.right - 0.6).abs() < 1.0e-6);
    }

    #[test]
    fn game_audio_queue_drains_when_headless_audio_is_unavailable() {
        let mut app = engine_ecs::App::new();
        let mut queue = GameAudioCommandQueue::default();
        queue.push_preflighted(GameAudioCommand::StopBackgroundMusic);
        app.insert_resource(queue);
        app.add_system(game_audio_effect_system);

        app.update().expect("headless audio system must run");

        assert_eq!(
            app.world()
                .get_resource::<GameAudioCommandQueue>()
                .unwrap()
                .len(),
            0
        );
    }
}
