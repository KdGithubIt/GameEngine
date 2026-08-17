//! Runtime audio assets, authored playback components, and platform backend.

pub use crate::spatial_audio::{AudioRolloffMode, AudioVoiceId, StereoGains};

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use engine_assets::asset::{Assets, Handle};
use engine_ecs::{Query, Res, ResMut};

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{mpsc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

#[cfg(not(target_arch = "wasm32"))]
use rodio::Source;

#[cfg(not(target_arch = "wasm32"))]
const AUDIO_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(target_arch = "wasm32"))]
const MAX_ACTIVE_VOICES: usize = 256;

#[derive(Clone, Copy)]
enum AudioBus {
    BackgroundMusic,
    SoundEffects,
}

#[derive(Clone, Copy)]
struct AudioBusVolumes {
    master: f32,
    background_music: f32,
    sound_effects: f32,
}

impl Default for AudioBusVolumes {
    fn default() -> Self {
        Self {
            master: 1.0,
            background_music: 1.0,
            sound_effects: 1.0,
        }
    }
}

impl AudioBusVolumes {
    fn set(&mut self, bus: AudioBus, volume: f32) {
        let volume = sanitize_volume(volume);
        match bus {
            AudioBus::BackgroundMusic => self.background_music = volume,
            AudioBus::SoundEffects => self.sound_effects = volume,
        }
    }

    fn background_music(self) -> f32 {
        self.master * self.background_music
    }

    fn sound_effects(self) -> f32 {
        self.master * self.sound_effects
    }
}

fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn fade_duration(fade_seconds: f32) -> Duration {
    if fade_seconds.is_finite() && fade_seconds > 0.0 {
        Duration::from_secs_f32(fade_seconds)
    } else {
        Duration::ZERO
    }
}

fn crossfade_gains(
    elapsed: Duration,
    duration: Duration,
    outgoing_start_gain: f32,
) -> (f32, f32, bool) {
    if duration.is_zero() || elapsed >= duration {
        return (0.0, 1.0, true);
    }
    let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    (outgoing_start_gain * (1.0 - progress), progress, false)
}

/// Decoded metadata and encoded bytes for an audio file loaded into memory.
///
/// The encoded bytes are retained so each playback request can create a fresh
/// decoder without touching the file system during a gameplay event.
#[derive(Clone)]
pub struct AudioAsset {
    encoded: Arc<[u8]>,
    sample_rate: u32,
    channels: u16,
}

/// Observable lifecycle of an author-authored automatic playback request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoredAudioState {
    /// Waiting for the shared authored-audio system's first pass.
    Pending,
    /// The request reached the platform audio backend successfully.
    Playing,
    /// This host has no usable audio backend (for example a headless test).
    Unavailable,
    /// The backend rejected playback; the message is retained for runtime inspection.
    Failed(String),
}

/// Authorable sound emitter attached to a spatial entity.
pub struct AudioEmitter {
    /// Decoded clip played when autoplay is enabled.
    pub clip: Handle<AudioAsset>,
    /// Per-emitter gain reserved for the spatial mixer.
    pub volume: f32,
    /// Zero is 2D and one is fully positional.
    pub spatial_blend: f32,
    /// Distance at which attenuation begins.
    pub min_distance: f32,
    /// Distance at which attenuation reaches its floor.
    pub max_distance: f32,
    /// Distance attenuation curve selected by authoring.
    pub rolloff: AudioRolloffMode,
    /// Whether the managed voice repeats until stopped or despawned.
    pub looping: bool,
    /// Whether conversion should produce one automatic playback request.
    pub autoplay: bool,
    state: AuthoredAudioState,
}

impl AudioEmitter {
    /// Creates a validated pending emitter with the default spatial policy.
    pub fn new(
        clip: Handle<AudioAsset>,
        volume: f32,
        spatial_blend: f32,
        min_distance: f32,
        max_distance: f32,
        autoplay: bool,
    ) -> Self {
        Self {
            clip,
            volume,
            spatial_blend,
            min_distance,
            max_distance,
            rolloff: AudioRolloffMode::Linear,
            looping: false,
            autoplay,
            state: AuthoredAudioState::Pending,
        }
    }

    /// Applies the authorable spatial playback policy without exposing backend voice state.
    pub fn with_spatial_playback(mut self, rolloff: AudioRolloffMode, looping: bool) -> Self {
        self.rolloff = rolloff;
        self.looping = looping;
        self
    }

    /// Returns the latest automatic playback state.
    pub fn state(&self) -> &AuthoredAudioState {
        &self.state
    }

    /// Records that the runtime host started this emitter successfully.
    pub fn mark_playback_started(&mut self) {
        self.state = AuthoredAudioState::Playing;
    }

    /// Records that this host cannot provide an audio backend.
    pub fn mark_playback_unavailable(&mut self) {
        self.state = AuthoredAudioState::Unavailable;
    }

    /// Records a runtime playback failure without exposing backend voice identity.
    pub fn mark_playback_failed(&mut self, message: impl Into<String>) {
        self.state = AuthoredAudioState::Failed(message.into());
    }
}

/// Marks the transform used as the positional-audio listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioListener {
    /// Disabled listeners remain authored but do not participate in selection.
    pub enabled: bool,
    /// Higher enabled priorities win; equal priorities use deterministic entity order.
    pub priority: i64,
}

impl Default for AudioListener {
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
        }
    }
}

/// Authorable background-music startup policy.
pub struct MusicController {
    /// Decoded music asset.
    pub clip: Handle<AudioAsset>,
    /// Background-music bus volume applied before autoplay.
    pub volume: f32,
    /// Fade-in/crossfade duration in seconds.
    pub fade_in_seconds: f32,
    /// Whether the controller starts music on the first system pass.
    pub autoplay: bool,
    state: AuthoredAudioState,
}

impl MusicController {
    /// Creates a validated pending music controller.
    pub fn new(
        clip: Handle<AudioAsset>,
        volume: f32,
        fade_in_seconds: f32,
        autoplay: bool,
    ) -> Self {
        Self {
            clip,
            volume,
            fade_in_seconds,
            autoplay,
            state: AuthoredAudioState::Pending,
        }
    }

    /// Returns the latest automatic playback state.
    pub fn state(&self) -> &AuthoredAudioState {
        &self.state
    }
}

/// Applies author-authored emitter and music autoplay requests once.
pub fn authored_audio_system(
    mut emitters: Query<&mut AudioEmitter>,
    mut music: Query<&mut MusicController>,
    assets: Res<Assets<AudioAsset>>,
    mut audio: Option<ResMut<AudioSystem>>,
) {
    for (_, emitter) in &mut emitters {
        if !emitter.autoplay || emitter.state != AuthoredAudioState::Pending {
            continue;
        }
        let Some(asset) = assets.get(&emitter.clip) else {
            emitter.state = AuthoredAudioState::Failed("decoded clip handle is missing".into());
            continue;
        };
        emitter.state = match audio.as_deref_mut() {
            Some(audio) => audio
                .play_se(asset)
                .map(|()| AuthoredAudioState::Playing)
                .unwrap_or_else(|error| AuthoredAudioState::Failed(error.to_string())),
            None => AuthoredAudioState::Unavailable,
        };
    }
    for (_, controller) in &mut music {
        if !controller.autoplay || controller.state != AuthoredAudioState::Pending {
            continue;
        }
        let Some(asset) = assets.get(&controller.clip) else {
            controller.state = AuthoredAudioState::Failed("decoded music handle is missing".into());
            continue;
        };
        controller.state = match audio.as_deref_mut() {
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
        };
    }
}

impl AudioAsset {
    /// Validates encoded WAV or OGG bytes and stores them for playback.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` cannot be decoded by the configured audio backend.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AudioError> {
        let encoded: Arc<[u8]> = Arc::from(bytes);
        let decoder = rodio::Decoder::new(Cursor::new(Arc::clone(&encoded))).map_err(|source| {
            AudioError::Decode {
                message: source.to_string(),
            }
        })?;
        Ok(Self {
            encoded,
            sample_rate: decoder.sample_rate(),
            channels: decoder.channels(),
        })
    }

    /// Stores encoded bytes without decoder validation.
    #[cfg(target_arch = "wasm32")]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AudioError> {
        Ok(Self {
            encoded: Arc::from(bytes),
            sample_rate: 0,
            channels: 0,
        })
    }

    /// Returns the encoded audio bytes.
    pub fn encoded(&self) -> Arc<[u8]> {
        Arc::clone(&self.encoded)
    }

    /// Returns the decoded sample rate reported by the audio file.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the number of channels reported by the audio file.
    pub fn channels(&self) -> u16 {
        self.channels
    }
}

/// Errors returned by audio asset validation and playback.
#[derive(Debug)]
pub enum AudioError {
    /// The platform could not provide a default output stream.
    OutputStream {
        /// Human-readable source error.
        message: String,
    },
    /// A playback sink or one-shot playback request could not be created.
    Playback {
        /// Human-readable source error.
        message: String,
    },
    /// Encoded audio bytes could not be decoded.
    Decode {
        /// Human-readable source error.
        message: String,
    },
    /// The audio worker thread stopped or could not accept a command.
    AudioThread {
        /// Human-readable source error.
        message: String,
    },
    /// Runtime audio playback is not available for this target.
    UnsupportedTarget,
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputStream { message } => write!(
                formatter,
                "failed to open default audio output stream: {message}"
            ),
            Self::Playback { message } => write!(formatter, "audio playback failed: {message}"),
            Self::Decode { message } => write!(formatter, "failed to decode audio: {message}"),
            Self::AudioThread { message } => write!(formatter, "audio thread failed: {message}"),
            Self::UnsupportedTarget => {
                formatter.write_str("runtime audio playback is not available for this target")
            }
        }
    }
}

impl std::error::Error for AudioError {}

/// Sends playback commands to the audio worker thread.
#[cfg(not(target_arch = "wasm32"))]
pub struct AudioSystem {
    command_sender: mpsc::Sender<AudioCommand>,
    completed_receiver: Mutex<mpsc::Receiver<AudioVoiceId>>,
    voice_gains: HashMap<AudioVoiceId, Arc<Mutex<[f32; 2]>>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    next_voice_id: u64,
    volumes: AudioBusVolumes,
}

/// Target stub for builds where desktop audio playback is unavailable.
#[cfg(target_arch = "wasm32")]
pub struct AudioSystem {
    volumes: AudioBusVolumes,
}

#[cfg(not(target_arch = "wasm32"))]
impl AudioSystem {
    /// Opens the platform default audio output stream.
    pub fn new() -> Result<Self, AudioError> {
        let (command_sender, command_receiver) = mpsc::channel();
        let (completed_sender, completed_receiver) = mpsc::channel();
        let (init_sender, init_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("engine-audio".into())
            .spawn(move || run_audio_thread(command_receiver, completed_sender, init_sender))
            .map_err(|source| AudioError::AudioThread {
                message: source.to_string(),
            })?;

        match init_receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(source) => {
                let _ = worker.join();
                return Err(AudioError::AudioThread {
                    message: source.to_string(),
                });
            }
        }

        Ok(Self {
            command_sender,
            completed_receiver: Mutex::new(completed_receiver),
            voice_gains: HashMap::new(),
            worker: Mutex::new(Some(worker)),
            next_voice_id: 1,
            volumes: AudioBusVolumes::default(),
        })
    }

    /// Plays a one-shot sound effect from an already-loaded [`AudioAsset`].
    pub fn play_se(&self, asset: &AudioAsset) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::PlaySe {
            encoded: asset.encoded(),
            respond_to,
        })
    }

    /// Starts one tracked sound-effect voice with engine-computed stereo gains.
    pub fn start_voice(
        &mut self,
        asset: &AudioAsset,
        gains: StereoGains,
        looping: bool,
    ) -> Result<AudioVoiceId, AudioError> {
        if self.voice_gains.len() >= MAX_ACTIVE_VOICES {
            return Err(AudioError::Playback {
                message: format!("active audio voice limit ({MAX_ACTIVE_VOICES}) reached"),
            });
        }
        let voice_id = AudioVoiceId(self.next_voice_id);
        let next_voice_id = self.next_voice_id.checked_add(1).ok_or_else(|| {
            AudioError::Playback {
                message: "runtime audio voice ID space is exhausted".to_owned(),
            }
        })?;
        let gains = Arc::new(Mutex::new(sanitize_stereo_gains(gains)));
        self.send_command(|respond_to| AudioCommand::StartVoice {
            voice_id,
            encoded: asset.encoded(),
            gains: Arc::clone(&gains),
            looping,
            respond_to,
        })?;
        self.next_voice_id = next_voice_id;
        self.voice_gains.insert(voice_id, gains);
        Ok(voice_id)
    }

    /// Updates one active voice without decoding or restarting its source.
    pub fn update_voice(
        &self,
        voice_id: AudioVoiceId,
        gains: StereoGains,
    ) -> Result<(), AudioError> {
        let Some(shared) = self.voice_gains.get(&voice_id) else {
            return Ok(());
        };
        let mut stored = shared.lock().map_err(|_| AudioError::AudioThread {
            message: "managed voice gain state is poisoned".to_owned(),
        })?;
        *stored = sanitize_stereo_gains(gains);
        Ok(())
    }

    /// Stops one active voice and retires its process-local identity.
    pub fn stop_voice(&mut self, voice_id: AudioVoiceId) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::StopVoice {
            voice_id,
            respond_to,
        })?;
        self.voice_gains.remove(&voice_id);
        Ok(())
    }

    /// Drains naturally completed one-shot voice IDs and retires their controls.
    pub fn drain_completed_voices(&mut self) -> Vec<AudioVoiceId> {
        let completed = match self.completed_receiver.lock() {
            Ok(receiver) => receiver.try_iter().collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        for voice_id in &completed {
            self.voice_gains.remove(voice_id);
        }
        completed
    }

    /// Replaces the current background music with an infinitely looping asset.
    pub fn play_bgm(&mut self, asset: &AudioAsset) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::PlayBgm {
            encoded: asset.encoded(),
            fade_duration: Duration::ZERO,
            respond_to,
        })
    }

    /// Crossfades from the active BGM to a newly looping asset.
    pub fn crossfade_bgm(
        &mut self,
        asset: &AudioAsset,
        fade_seconds: f32,
    ) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::PlayBgm {
            encoded: asset.encoded(),
            fade_duration: fade_duration(fade_seconds),
            respond_to,
        })
    }

    /// Stops the active background music, if any.
    pub fn stop_bgm(&mut self) -> Result<(), AudioError> {
        self.send_command(AudioCommand::StopBgm)
    }

    /// Sets the master volume used by new sound effects and current BGM.
    pub fn set_master_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        let volume = sanitize_volume(volume);
        self.send_command(|respond_to| AudioCommand::SetMasterVolume { volume, respond_to })?;
        self.volumes.master = volume;
        Ok(())
    }

    /// Returns the current clamped master volume.
    pub fn master_volume(&self) -> f32 {
        self.volumes.master
    }

    /// Sets the BGM bus volume independently of the master volume.
    pub fn set_bgm_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        self.set_bus_volume(AudioBus::BackgroundMusic, volume)
    }

    /// Returns the current clamped BGM bus volume.
    pub fn bgm_volume(&self) -> f32 {
        self.volumes.background_music
    }

    /// Sets the sound-effect bus volume independently of the master volume.
    pub fn set_se_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        self.set_bus_volume(AudioBus::SoundEffects, volume)
    }

    /// Returns the current clamped sound-effect bus volume.
    pub fn se_volume(&self) -> f32 {
        self.volumes.sound_effects
    }

    fn set_bus_volume(&mut self, bus: AudioBus, volume: f32) -> Result<(), AudioError> {
        let volume = sanitize_volume(volume);
        self.send_command(|respond_to| AudioCommand::SetBusVolume {
            bus,
            volume,
            respond_to,
        })?;
        self.volumes.set(bus, volume);
        Ok(())
    }

    fn send_command(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<(), AudioError>>) -> AudioCommand,
    ) -> Result<(), AudioError> {
        let (respond_to, response) = mpsc::channel();
        self.command_sender
            .send(command(respond_to))
            .map_err(|source| AudioError::AudioThread {
                message: source.to_string(),
            })?;
        response.recv().map_err(|source| AudioError::AudioThread {
            message: source.to_string(),
        })?
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for AudioSystem {
    fn drop(&mut self) {
        let _ = self.command_sender.send(AudioCommand::Shutdown);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl AudioSystem {
    /// Returns an error because this phase does not support WASM audio.
    pub fn new() -> Result<Self, AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn play_se(&self, _asset: &AudioAsset) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn start_voice(
        &mut self,
        _asset: &AudioAsset,
        _gains: StereoGains,
        _looping: bool,
    ) -> Result<AudioVoiceId, AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn update_voice(
        &self,
        _voice_id: AudioVoiceId,
        _gains: StereoGains,
    ) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn stop_voice(&mut self, _voice_id: AudioVoiceId) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// The unsupported backend never owns managed voices.
    pub fn drain_completed_voices(&mut self) -> Vec<AudioVoiceId> {
        Vec::new()
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn play_bgm(&mut self, _asset: &AudioAsset) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn crossfade_bgm(
        &mut self,
        _asset: &AudioAsset,
        _fade_seconds: f32,
    ) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Stops background music. The WASM stub never has active playback.
    pub fn stop_bgm(&mut self) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Sets the clamped master volume retained by the stub.
    pub fn set_master_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        self.volumes.master = sanitize_volume(volume);
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns the current clamped master volume.
    pub fn master_volume(&self) -> f32 {
        self.volumes.master
    }
    /// Sets the clamped BGM bus volume retained by the stub.
    pub fn set_bgm_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        self.volumes.set(AudioBus::BackgroundMusic, volume);
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns the current clamped BGM bus volume.
    pub fn bgm_volume(&self) -> f32 {
        self.volumes.background_music
    }
    /// Sets the clamped sound-effect bus volume retained by the stub.
    pub fn set_se_volume(&mut self, volume: f32) -> Result<(), AudioError> {
        self.volumes.set(AudioBus::SoundEffects, volume);
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns the current clamped sound-effect bus volume.
    pub fn se_volume(&self) -> f32 {
        self.volumes.sound_effects
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum AudioCommand {
    PlaySe {
        encoded: Arc<[u8]>,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    StartVoice {
        voice_id: AudioVoiceId,
        encoded: Arc<[u8]>,
        gains: Arc<Mutex<[f32; 2]>>,
        looping: bool,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    StopVoice {
        voice_id: AudioVoiceId,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    PlayBgm {
        encoded: Arc<[u8]>,
        fade_duration: Duration,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    StopBgm(mpsc::Sender<Result<(), AudioError>>),
    SetMasterVolume {
        volume: f32,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    SetBusVolume {
        bus: AudioBus,
        volume: f32,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    Shutdown,
}

#[cfg(not(target_arch = "wasm32"))]
fn run_audio_thread(
    command_receiver: mpsc::Receiver<AudioCommand>,
    completed_sender: mpsc::Sender<AudioVoiceId>,
    init_sender: mpsc::Sender<Result<(), AudioError>>,
) {
    let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
        Ok(stream) => {
            let _ = init_sender.send(Ok(()));
            stream
        }
        Err(source) => {
            let _ = init_sender.send(Err(AudioError::OutputStream {
                message: source.to_string(),
            }));
            return;
        }
    };

    let mut bgm = BgmPlayback::Silent;
    let mut se_sinks = Vec::new();
    let mut voices = HashMap::new();
    let mut volumes = AudioBusVolumes::default();

    loop {
        let command = match command_receiver.recv_timeout(AUDIO_POLL_INTERVAL) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            Some(AudioCommand::PlaySe {
                encoded,
                respond_to,
            }) => {
                let result = play_se_on_thread(&stream_handle, encoded, volumes.sound_effects())
                    .map(|sink| se_sinks.push(sink));
                let _ = respond_to.send(result);
            }
            Some(AudioCommand::StartVoice {
                voice_id,
                encoded,
                gains,
                looping,
                respond_to,
            }) => {
                let result = if voices.len() >= MAX_ACTIVE_VOICES {
                    Err(AudioError::Playback {
                        message: format!(
                            "active audio voice limit ({MAX_ACTIVE_VOICES}) reached"
                        ),
                    })
                } else {
                    play_voice_on_thread(
                        &stream_handle,
                        encoded,
                        gains,
                        looping,
                        volumes.sound_effects(),
                    )
                    .map(|voice| {
                        voices.insert(voice_id, voice);
                    })
                };
                let _ = respond_to.send(result);
            }
            Some(AudioCommand::StopVoice {
                voice_id,
                respond_to,
            }) => {
                if let Some(voice) = voices.remove(&voice_id) {
                    voice.sink.stop();
                }
                let _ = respond_to.send(Ok(()));
            }
            Some(AudioCommand::PlayBgm {
                encoded,
                fade_duration,
                respond_to,
            }) => {
                let result = bgm.replace(
                    &stream_handle,
                    encoded,
                    fade_duration,
                    volumes.background_music(),
                );
                let _ = respond_to.send(result);
            }
            Some(AudioCommand::StopBgm(respond_to)) => {
                bgm.stop();
                let _ = respond_to.send(Ok(()));
            }
            Some(AudioCommand::SetMasterVolume { volume, respond_to }) => {
                volumes.master = sanitize_volume(volume);
                apply_bus_volumes(&mut bgm, &se_sinks, &voices, volumes);
                let _ = respond_to.send(Ok(()));
            }
            Some(AudioCommand::SetBusVolume {
                bus,
                volume,
                respond_to,
            }) => {
                volumes.set(bus, volume);
                apply_bus_volumes(&mut bgm, &se_sinks, &voices, volumes);
                let _ = respond_to.send(Ok(()));
            }
            Some(AudioCommand::Shutdown) => break,
            None => {}
        }

        bgm.tick(volumes.background_music());
        se_sinks.retain(|sink| !sink.empty());
        let completed = voices
            .iter()
            .filter_map(|(voice_id, voice)| voice.sink.empty().then_some(*voice_id))
            .collect::<Vec<_>>();
        for voice_id in completed {
            voices.remove(&voice_id);
            let _ = completed_sender.send(voice_id);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_bus_volumes(
    bgm: &mut BgmPlayback,
    se_sinks: &[rodio::Sink],
    voices: &HashMap<AudioVoiceId, ActiveVoice>,
    volumes: AudioBusVolumes,
) {
    bgm.apply_volume(volumes.background_music());
    for sink in se_sinks {
        sink.set_volume(volumes.sound_effects());
    }
    for voice in voices.values() {
        voice.sink.set_volume(volumes.sound_effects());
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct ActiveVoice {
    sink: rodio::Sink,
}

#[cfg(not(target_arch = "wasm32"))]
struct ActiveBgm {
    sink: rodio::Sink,
    gain: f32,
}

#[cfg(not(target_arch = "wasm32"))]
struct BgmCrossfade {
    outgoing: Option<ActiveBgm>,
    outgoing_start_gain: f32,
    incoming: ActiveBgm,
    started_at: Instant,
    duration: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
enum BgmPlayback {
    Silent,
    Playing(ActiveBgm),
    Crossfading(BgmCrossfade),
}

#[cfg(not(target_arch = "wasm32"))]
impl BgmPlayback {
    fn replace(
        &mut self,
        stream_handle: &rodio::OutputStreamHandle,
        encoded: Arc<[u8]>,
        fade_duration: Duration,
        bus_volume: f32,
    ) -> Result<(), AudioError> {
        let decoder = decoder_for_encoded(encoded)?;
        let sink = rodio::Sink::try_new(stream_handle).map_err(|source| AudioError::Playback {
            message: source.to_string(),
        })?;
        sink.append(decoder.repeat_infinite());

        if fade_duration.is_zero() {
            self.stop();
            sink.set_volume(bus_volume);
            *self = Self::Playing(ActiveBgm { sink, gain: 1.0 });
            return Ok(());
        }

        let outgoing = self.take_loudest_track();
        let outgoing_start_gain = outgoing.as_ref().map_or(0.0, |track| track.gain);
        sink.set_volume(0.0);
        *self = Self::Crossfading(BgmCrossfade {
            outgoing,
            outgoing_start_gain,
            incoming: ActiveBgm { sink, gain: 0.0 },
            started_at: Instant::now(),
            duration: fade_duration,
        });
        Ok(())
    }

    fn stop(&mut self) {
        let previous = std::mem::replace(self, Self::Silent);
        match previous {
            Self::Silent => {}
            Self::Playing(track) => track.sink.stop(),
            Self::Crossfading(fade) => {
                if let Some(track) = fade.outgoing {
                    track.sink.stop();
                }
                fade.incoming.sink.stop();
            }
        }
    }

    fn tick(&mut self, bus_volume: f32) {
        let previous = std::mem::replace(self, Self::Silent);
        *self = match previous {
            Self::Crossfading(mut fade) => {
                let (outgoing_gain, incoming_gain, is_complete) = crossfade_gains(
                    fade.started_at.elapsed(),
                    fade.duration,
                    fade.outgoing_start_gain,
                );
                if let Some(outgoing) = fade.outgoing.as_mut() {
                    outgoing.gain = outgoing_gain;
                    outgoing.sink.set_volume(bus_volume * outgoing_gain);
                }
                fade.incoming.gain = incoming_gain;
                fade.incoming.sink.set_volume(bus_volume * incoming_gain);
                if is_complete {
                    if let Some(outgoing) = fade.outgoing {
                        outgoing.sink.stop();
                    }
                    Self::Playing(fade.incoming)
                } else {
                    Self::Crossfading(fade)
                }
            }
            state => state,
        };
    }

    fn apply_volume(&self, bus_volume: f32) {
        match self {
            Self::Silent => {}
            Self::Playing(track) => track.sink.set_volume(bus_volume * track.gain),
            Self::Crossfading(fade) => {
                if let Some(outgoing) = &fade.outgoing {
                    outgoing.sink.set_volume(bus_volume * outgoing.gain);
                }
                fade.incoming
                    .sink
                    .set_volume(bus_volume * fade.incoming.gain);
            }
        }
    }

    fn take_loudest_track(&mut self) -> Option<ActiveBgm> {
        let previous = std::mem::replace(self, Self::Silent);
        match previous {
            Self::Silent => None,
            Self::Playing(track) => Some(track),
            Self::Crossfading(fade) => match fade.outgoing {
                Some(outgoing) if outgoing.gain > fade.incoming.gain => {
                    fade.incoming.sink.stop();
                    Some(outgoing)
                }
                Some(outgoing) => {
                    outgoing.sink.stop();
                    Some(fade.incoming)
                }
                None => Some(fade.incoming),
            },
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn play_se_on_thread(
    stream_handle: &rodio::OutputStreamHandle,
    encoded: Arc<[u8]>,
    bus_volume: f32,
) -> Result<rodio::Sink, AudioError> {
    let decoder = decoder_for_encoded(encoded)?;
    let sink = rodio::Sink::try_new(stream_handle).map_err(|source| AudioError::Playback {
        message: source.to_string(),
    })?;
    sink.set_volume(bus_volume);
    sink.append(decoder);
    Ok(sink)
}

#[cfg(not(target_arch = "wasm32"))]
fn play_voice_on_thread(
    stream_handle: &rodio::OutputStreamHandle,
    encoded: Arc<[u8]>,
    gains: Arc<Mutex<[f32; 2]>>,
    looping: bool,
    bus_volume: f32,
) -> Result<ActiveVoice, AudioError> {
    let decoder = decoder_for_encoded(encoded)?;
    let initial = gains.lock().map(|gains| *gains).unwrap_or([0.0, 0.0]);
    let shared = Arc::clone(&gains);
    let source = rodio::source::ChannelVolume::new(decoder, vec![initial[0], initial[1]])
        .periodic_access(AUDIO_POLL_INTERVAL, move |source| {
            if let Ok(gains) = shared.lock() {
                source.set_volume(0, gains[0]);
                source.set_volume(1, gains[1]);
            }
        });
    let sink = rodio::Sink::try_new(stream_handle).map_err(|source| AudioError::Playback {
        message: source.to_string(),
    })?;
    sink.set_volume(bus_volume);
    if looping {
        sink.append(source.repeat_infinite());
    } else {
        sink.append(source);
    }
    Ok(ActiveVoice { sink })
}

fn sanitize_stereo_gains(gains: StereoGains) -> [f32; 2] {
    [sanitize_volume(gains.left), sanitize_volume(gains.right)]
}

#[cfg(not(target_arch = "wasm32"))]
fn decoder_for_encoded(
    encoded: Arc<[u8]>,
) -> Result<rodio::Decoder<Cursor<Arc<[u8]>>>, AudioError> {
    rodio::Decoder::new(Cursor::new(encoded)).map_err(|source| AudioError::Decode {
        message: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_asset_reads_wav_metadata() {
        let asset = AudioAsset::from_bytes(test_wav_bytes()).expect("test WAV must decode");
        assert_eq!(asset.sample_rate(), 44_100);
        assert_eq!(asset.channels(), 1);
        assert!(!asset.encoded().is_empty());
    }

    #[test]
    fn audio_bus_volumes_are_clamped_and_combined_with_master_volume() {
        let mut volumes = AudioBusVolumes {
            master: 0.5,
            ..AudioBusVolumes::default()
        };
        volumes.set(AudioBus::BackgroundMusic, 0.4);
        volumes.set(AudioBus::SoundEffects, 2.0);
        assert!((volumes.background_music() - 0.2).abs() < f32::EPSILON);
        assert!((volumes.sound_effects() - 0.5).abs() < f32::EPSILON);
        volumes.set(AudioBus::SoundEffects, f32::NAN);
        assert_eq!(volumes.sound_effects(), 0.0);
    }

    #[test]
    fn crossfade_gains_progress_linearly_and_finish_at_target() {
        let duration = Duration::from_secs(2);
        let (outgoing, incoming, is_complete) =
            crossfade_gains(Duration::from_secs(1), duration, 0.8);
        assert!((outgoing - 0.4).abs() < f32::EPSILON);
        assert!((incoming - 0.5).abs() < f32::EPSILON);
        assert!(!is_complete);
        let (outgoing, incoming, is_complete) = crossfade_gains(duration, duration, 0.8);
        assert_eq!((outgoing, incoming, is_complete), (0.0, 1.0, true));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn managed_voice_update_reuses_existing_voice_state() {
        let (command_sender, _command_receiver) = mpsc::channel();
        let (_completed_sender, completed_receiver) = mpsc::channel();
        let voice_id = AudioVoiceId(7);
        let shared = Arc::new(Mutex::new([0.2, 0.8]));
        let mut voice_gains = HashMap::new();
        voice_gains.insert(voice_id, Arc::clone(&shared));
        let audio = AudioSystem {
            command_sender,
            completed_receiver: Mutex::new(completed_receiver),
            voice_gains,
            worker: Mutex::new(None),
            next_voice_id: 8,
            volumes: AudioBusVolumes::default(),
        };

        audio
            .update_voice(
                voice_id,
                StereoGains {
                    left: 0.6,
                    right: 0.4,
                },
            )
            .expect("managed gain update must not require an audio device");

        assert_eq!(*shared.lock().expect("test gain lock"), [0.6, 0.4]);
        assert_eq!(audio.voice_gains.len(), 1);
        assert!(audio.voice_gains.contains_key(&voice_id));
        assert_eq!(audio.next_voice_id, 8, "update must not allocate a new voice");
    }

    #[test]
    fn invalid_crossfade_durations_request_immediate_replacement() {
        assert_eq!(fade_duration(0.0), Duration::ZERO);
        assert_eq!(fade_duration(-1.0), Duration::ZERO);
        assert_eq!(fade_duration(f32::NAN), Duration::ZERO);
        assert_eq!(fade_duration(f32::INFINITY), Duration::ZERO);
    }

    fn test_wav_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&38_u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&44_100_u32.to_le_bytes());
        bytes.extend_from_slice(&88_200_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&0_i16.to_le_bytes());
        bytes
    }
}
