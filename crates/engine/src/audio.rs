//! Compatibility facade for platform audio plus engine-owned spatial composition.

pub use engine_platform::audio::*;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::transform::GlobalTransform;
use engine_authoring::{AssetId, StableId};
use engine_ecs::{Entity, Query, Res, ResMut};
use glam::Vec3;

/// Maximum pending project-Rust audio requests.
pub const MAX_GAME_AUDIO_COMMANDS: usize = 256;

/// Options carried by one generation-checked project spatial-audio request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GameSpatialAudioOptions {
    pub(crate) volume: f32,
    pub(crate) spatial_blend: f32,
    pub(crate) min_distance: f32,
    pub(crate) max_distance: f32,
    pub(crate) rolloff: SpatialRolloff,
    pub(crate) looping: bool,
}

/// One validated audio request queued by a project Rust callback.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GameAudioCommand {
    PlaySoundEffect {
        asset_id: String,
    },
    PlaySpatialSoundEffect {
        asset_id: String,
        source: Entity,
        options: GameSpatialAudioOptions,
    },
    PlayBackgroundMusic {
        asset_id: String,
        fade_seconds: f32,
    },
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

/// Runtime-only mapping between authored emitters and backend voice IDs.
#[derive(Debug, Default)]
pub(crate) struct AuthoredAudioVoices {
    voices: BTreeMap<Entity, AudioVoiceId>,
}

#[derive(Debug, Clone, Copy)]
struct GameSpatialVoice {
    source: Entity,
    voice: AudioVoiceId,
    options: GameSpatialAudioOptions,
}

/// Runtime-only spatial voices created by project Rust commands.
#[derive(Debug, Default)]
pub(crate) struct GameSpatialAudioVoices {
    voices: Vec<GameSpatialVoice>,
}

/// Applies authored emitter/music autoplay and keeps spatial voices synchronized.
pub fn authored_audio_system(
    mut emitters: Query<(&mut AudioEmitter, &GlobalTransform)>,
    listeners: Query<(&AudioListener, &GlobalTransform)>,
    mut music: Query<&mut MusicController>,
    assets: Res<Assets<AudioAsset>>,
    mut audio: Option<ResMut<AudioSystem>>,
    mut active_voices: ResMut<AuthoredAudioVoices>,
) {
    let listener = active_listener_pose(&listeners);
    let mut live_emitters = BTreeSet::new();

    for (entity, (emitter, global)) in &mut emitters {
        live_emitters.insert(entity);
        if !emitter.autoplay {
            if let Some(voice) = active_voices.voices.remove(&entity)
                && let Some(audio) = audio.as_deref_mut()
            {
                let _ = audio.stop_voice(voice);
            }
            continue;
        }

        if matches!(emitter.state(), AuthoredAudioState::Pending) {
            let Some(asset) = assets.get(&emitter.clip) else {
                emitter.set_runtime_state(AuthoredAudioState::Failed(
                    "decoded clip handle is missing".into(),
                ));
                continue;
            };
            let Some(audio) = audio.as_deref_mut() else {
                emitter.set_runtime_state(AuthoredAudioState::Unavailable);
                continue;
            };
            let params = emitter_spatial_params(emitter, global, listener);
            match audio.start_voice(asset, emitter.looping, params) {
                Ok(voice) => {
                    active_voices.voices.insert(entity, voice);
                    emitter.set_runtime_state(AuthoredAudioState::Playing);
                }
                Err(error) => {
                    emitter.set_runtime_state(AuthoredAudioState::Failed(error.to_string()));
                }
            }
            continue;
        }

        let Some(voice) = active_voices.voices.get(&entity).copied() else {
            continue;
        };
        let Some(audio) = audio.as_deref_mut() else {
            continue;
        };
        let params = emitter_spatial_params(emitter, global, listener);
        match audio.update_voice(voice, params) {
            Ok(()) => {}
            Err(AudioError::UnknownVoice { .. }) => {
                active_voices.voices.remove(&entity);
            }
            Err(error) => {
                log::error!("authored spatial audio update failed: {error}");
            }
        }
    }

    let stale: Vec<(Entity, AudioVoiceId)> = active_voices
        .voices
        .iter()
        .filter(|(entity, _)| !live_emitters.contains(entity))
        .map(|(entity, voice)| (*entity, *voice))
        .collect();
    for (entity, voice) in stale {
        active_voices.voices.remove(&entity);
        if let Some(audio) = audio.as_deref_mut() {
            let _ = audio.stop_voice(voice);
        }
    }

    for (_, controller) in &mut music {
        if !controller.autoplay || !matches!(controller.state(), AuthoredAudioState::Pending) {
            continue;
        }
        let Some(asset) = assets.get(&controller.clip) else {
            controller.set_runtime_state(AuthoredAudioState::Failed(
                "decoded music handle is missing".into(),
            ));
            continue;
        };
        controller.set_runtime_state(match audio.as_deref_mut() {
            Some(audio) => {
                let result = audio.set_bgm_volume(controller.volume).and_then(|()| {
                    if controller.fade_in_seconds > 0.0 {
                        audio.crossfade_bgm(asset, controller.fade_in_seconds)
                    } else {
                        audio.play_bgm(asset)
                    }
                });
                result
                    .map(|()| AuthoredAudioState::Playing)
                    .unwrap_or_else(|error| AuthoredAudioState::Failed(error.to_string()))
            }
            None => AuthoredAudioState::Unavailable,
        });
    }
}

/// Resolves and applies queued project-Rust audio requests.
pub(crate) fn game_audio_effect_system(
    mut queue: ResMut<GameAudioCommandQueue>,
    mut spatial_voices: ResMut<GameSpatialAudioVoices>,
    transforms: Query<&GlobalTransform>,
    listeners: Query<(&AudioListener, &GlobalTransform)>,
    mut audio_system: Option<ResMut<AudioSystem>>,
    mut asset_server: Option<ResMut<AssetServer>>,
    manifest: Option<Res<AssetManifest>>,
    mut audio_assets: Option<ResMut<Assets<AudioAsset>>>,
) {
    let listener = active_listener_pose(&listeners);
    let poses: BTreeMap<Entity, AudioEmitterPose> = transforms
        .iter()
        .map(|(entity, global)| (entity, emitter_pose(global)))
        .collect();

    if let Some(audio) = audio_system.as_deref_mut() {
        spatial_voices.voices.retain(|active| {
            let Some(emitter) = poses.get(&active.source).copied() else {
                let _ = audio.stop_voice(active.voice);
                return false;
            };
            let params = project_spatial_params(active.options, emitter, listener);
            match audio.update_voice(active.voice, params) {
                Ok(()) => true,
                Err(AudioError::UnknownVoice { .. }) => false,
                Err(error) => {
                    log::error!("project Rust spatial audio update failed: {error}");
                    true
                }
            }
        });
    } else {
        spatial_voices.voices.clear();
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
                    log::warn!(
                        "project Rust spatial audio skipped because source entity {}:{} has no GlobalTransform",
                        source.id(),
                        source.generation()
                    );
                    continue;
                };
                let params = project_spatial_params(options, emitter, listener);
                match resolve_and_start_spatial(
                    &asset_id,
                    options.looping,
                    params,
                    audio_system.as_deref_mut(),
                    asset_server.as_deref_mut(),
                    manifest.as_deref(),
                    audio_assets.as_deref_mut(),
                ) {
                    Some(Ok(voice)) => {
                        spatial_voices.voices.push(GameSpatialVoice {
                            source,
                            voice,
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

fn active_listener_pose(
    listeners: &Query<(&AudioListener, &GlobalTransform)>,
) -> Option<AudioListenerPose> {
    let mut selected: Option<(i32, Entity, AudioListenerPose)> = None;
    for (entity, (listener, global)) in listeners.iter() {
        if !listener.enabled {
            continue;
        }
        let candidate = (listener.priority, entity, listener_pose(global));
        let replace = match selected.as_ref() {
            None => true,
            Some((priority, selected_entity, _)) => {
                candidate.0 > *priority
                    || (candidate.0 == *priority && candidate.1 < *selected_entity)
            }
        };
        if replace {
            selected = Some(candidate);
        }
    }
    selected.map(|(_, _, pose)| pose)
}

fn listener_pose(global: &GlobalTransform) -> AudioListenerPose {
    let matrix = global.matrix();
    let forward = matrix
        .transform_vector3(Vec3::NEG_Z)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let up = matrix
        .transform_vector3(Vec3::Y)
        .try_normalize()
        .unwrap_or(Vec3::Y);
    AudioListenerPose {
        position: matrix.w_axis.truncate().to_array(),
        forward: forward.to_array(),
        up: up.to_array(),
    }
}

fn emitter_pose(global: &GlobalTransform) -> AudioEmitterPose {
    AudioEmitterPose {
        position: global.matrix().w_axis.truncate().to_array(),
    }
}

fn emitter_spatial_params(
    emitter: &AudioEmitter,
    global: &GlobalTransform,
    listener: Option<AudioListenerPose>,
) -> VoiceSpatialParams {
    project_spatial_params(
        GameSpatialAudioOptions {
            volume: emitter.volume,
            spatial_blend: emitter.spatial_blend,
            min_distance: emitter.min_distance,
            max_distance: emitter.max_distance,
            rolloff: emitter.rolloff,
            looping: emitter.looping,
        },
        emitter_pose(global),
        listener,
    )
}

fn project_spatial_params(
    options: GameSpatialAudioOptions,
    emitter: AudioEmitterPose,
    listener: Option<AudioListenerPose>,
) -> VoiceSpatialParams {
    let spatial_blend = if listener.is_some() {
        options.spatial_blend
    } else {
        0.0
    };
    VoiceSpatialParams {
        emitter,
        listener: listener.unwrap_or_default(),
        volume: options.volume,
        spatial_blend,
        min_distance: options.min_distance,
        max_distance: options.max_distance,
        rolloff: options.rolloff,
    }
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

fn resolve_and_start_spatial(
    asset_id: &str,
    looping: bool,
    params: VoiceSpatialParams,
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
    Some(audio.start_voice(asset, looping, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_audio_queue_drains_when_headless_audio_is_unavailable() {
        let mut app = engine_ecs::App::new();
        let mut queue = GameAudioCommandQueue::default();
        queue.push_preflighted(GameAudioCommand::StopBackgroundMusic);
        app.insert_resource(queue);
        app.insert_resource(GameSpatialAudioVoices::default());
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
