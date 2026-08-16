//! Compatibility facade for platform audio plus project-command adaptation.

pub use engine_platform::audio::*;

use std::collections::VecDeque;

use crate::asset::{AssetManifest, AssetServer, Assets};
use engine_authoring::{AssetId, StableId};
use engine_ecs::{Res, ResMut};

/// Maximum pending project-Rust audio requests.
pub const MAX_GAME_AUDIO_COMMANDS: usize = 256;

/// One validated audio request queued by a project Rust callback.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GameAudioCommand {
    PlaySoundEffect { asset_id: String },
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
    mut audio_system: Option<ResMut<AudioSystem>>,
    mut asset_server: Option<ResMut<AssetServer>>,
    manifest: Option<Res<AssetManifest>>,
    mut audio_assets: Option<ResMut<Assets<AudioAsset>>>,
) {
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
