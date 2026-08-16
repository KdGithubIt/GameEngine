//! Runtime audio assets, authored playback components, and platform backend.

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use engine_assets::asset::Handle;

mod spatial;

pub use spatial::{
    attenuation_gain, spatial_stereo_gains, AudioEmitterPose, AudioListenerPose, AudioVoiceId,
    SpatialRolloff, StereoGains, VoiceSpatialParams,
};

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU32, Ordering};
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
const AUDIO_COMMAND_CAPACITY: usize = 256;

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
    /// Engine-owned distance attenuation curve.
    pub rolloff: SpatialRolloff,
    /// Whether this emitter repeats until it is stopped or despawned.
    pub looping: bool,
    /// Whether conversion should produce one automatic playback request.
    pub autoplay: bool,
    state: AuthoredAudioState,
}

impl AudioEmitter {
    /// Creates a validated pending emitter.
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
            rolloff: SpatialRolloff::Linear,
            looping: false,
            autoplay,
            state: AuthoredAudioState::Pending,
        }
    }

    /// Returns the latest automatic playback state.
    pub fn state(&self) -> &AuthoredAudioState {
        &self.state
    }

    /// Updates runtime-only autoplay state without exposing mutable storage.
    #[doc(hidden)]
    pub fn set_runtime_state(&mut self, state: AuthoredAudioState) {
        self.state = state;
    }
}

/// Marks the transform used as the positional-audio listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioListener {
    /// Disabled listeners remain authored but do not participate in selection.
    pub enabled: bool,
    /// Selection priority among enabled listeners; higher values win.
    pub priority: i32,
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

    /// Updates runtime-only autoplay state without exposing mutable storage.
    #[doc(hidden)]
    pub fn set_runtime_state(&mut self, state: AuthoredAudioState) {
        self.state = state;
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
    /// A voice completed before an update reached the backend.
    UnknownVoice {
        /// Runtime-only voice identifier that was no longer active.
        voice: AudioVoiceId,
    },
    /// The process-local voice identifier space was exhausted.
    VoiceIdExhausted,
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
            Self::UnknownVoice { voice } => write!(
                formatter,
                "audio voice {} is no longer active",
                voice.raw()
            ),
            Self::VoiceIdExhausted => formatter.write_str("audio voice identifier space exhausted"),
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
    command_sender: mpsc::SyncSender<AudioCommand>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    volumes: AudioBusVolumes,
    next_voice_id: u64,
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
        let (command_sender, command_receiver) = mpsc::sync_channel(AUDIO_COMMAND_CAPACITY);
        let (init_sender, init_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("engine-audio".into())
            .spawn(move || run_audio_thread(command_receiver, init_sender))
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
            worker: Mutex::new(Some(worker)),
            volumes: AudioBusVolumes::default(),
            next_voice_id: 1,
        })
    }

    /// Plays a one-shot sound effect from an already-loaded [`AudioAsset`].
    pub fn play_se(&self, asset: &AudioAsset) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::PlaySe {
            encoded: asset.encoded(),
            respond_to,
        })
    }

    /// Starts a spatially mixed sound-effect voice from an already-loaded asset.
    ///
    /// The returned identifier is runtime-only and exists solely so engine
    /// composition can update or stop this voice while the source entity moves.
    pub fn start_voice(
        &mut self,
        asset: &AudioAsset,
        looping: bool,
        params: VoiceSpatialParams,
    ) -> Result<AudioVoiceId, AudioError> {
        let voice = AudioVoiceId(self.next_voice_id);
        self.next_voice_id = self
            .next_voice_id
            .checked_add(1)
            .ok_or(AudioError::VoiceIdExhausted)?;
        let gains = spatial_stereo_gains(params);
        self.send_command(|respond_to| AudioCommand::StartVoice {
            voice,
            encoded: asset.encoded(),
            looping,
            gains,
            respond_to,
        })?;
        Ok(voice)
    }

    /// Updates an existing spatial voice without decoding or restarting it.
    pub fn update_voice(
        &self,
        voice: AudioVoiceId,
        params: VoiceSpatialParams,
    ) -> Result<(), AudioError> {
        let gains = spatial_stereo_gains(params);
        self.send_command(|respond_to| AudioCommand::UpdateVoice {
            voice,
            gains,
            respond_to,
        })
    }

    /// Explicitly stops a spatial voice. Stopping an already-retired voice is idempotent.
    pub fn stop_voice(&self, voice: AudioVoiceId) -> Result<(), AudioError> {
        self.send_command(|respond_to| AudioCommand::StopVoice { voice, respond_to })
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
        _looping: bool,
        _params: VoiceSpatialParams,
    ) -> Result<AudioVoiceId, AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn update_voice(
        &self,
        _voice: AudioVoiceId,
        _params: VoiceSpatialParams,
    ) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
    }
    /// Returns an error because this phase does not support WASM audio.
    pub fn stop_voice(&self, _voice: AudioVoiceId) -> Result<(), AudioError> {
        Err(AudioError::UnsupportedTarget)
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
        voice: AudioVoiceId,
        encoded: Arc<[u8]>,
        looping: bool,
        gains: StereoGains,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    UpdateVoice {
        voice: AudioVoiceId,
        gains: StereoGains,
        respond_to: mpsc::Sender<Result<(), AudioError>>,
    },
    StopVoice {
        voice: AudioVoiceId,
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
    let mut voices = HashMap::<AudioVoiceId, ActiveVoice>::new();
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
                voice,
                encoded,
                looping,
                gains,
                respond_to,
            }) => {
                let result = start_voice_on_thread(
                    &stream_handle,
                    encoded,
                    looping,
                    gains,
                    volumes.sound_effects(),
                )
                .map(|active| {
                    voices.insert(voice, active);
                });
                let _ = respond_to.send(result);
            }
            Some(AudioCommand::UpdateVoice {
                voice,
                gains,
                respond_to,
            }) => {
                let result = voices
                    .get(&voice)
                    .ok_or(AudioError::UnknownVoice { voice })
                    .map(|active| active.gains.set(gains));
                let _ = respond_to.send(result);
            }
            Some(AudioCommand::StopVoice { voice, respond_to }) => {
                if let Some(active) = voices.remove(&voice) {
                    active.sink.stop();
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
        voices.retain(|_, active| !active.sink.empty());
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
    for active in voices.values() {
        active.sink.set_volume(volumes.sound_effects());
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct VoiceGainControl {
    left: Arc<AtomicU32>,
    right: Arc<AtomicU32>,
}

#[cfg(not(target_arch = "wasm32"))]
impl VoiceGainControl {
    fn new(gains: StereoGains) -> Self {
        Self {
            left: Arc::new(AtomicU32::new(gains.left.to_bits())),
            right: Arc::new(AtomicU32::new(gains.right.to_bits())),
        }
    }

    fn set(&self, gains: StereoGains) {
        self.left.store(gains.left.to_bits(), Ordering::Relaxed);
        self.right.store(gains.right.to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> StereoGains {
        StereoGains {
            left: f32::from_bits(self.left.load(Ordering::Relaxed)),
            right: f32::from_bits(self.right.load(Ordering::Relaxed)),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct ActiveVoice {
    sink: rodio::Sink,
    gains: VoiceGainControl,
}

#[cfg(not(target_arch = "wasm32"))]
fn append_spatial_source<S>(sink: &rodio::Sink, source: S, gains: VoiceGainControl)
where
    S: Source + Send + 'static,
    f32: rodio::cpal::FromSample<S::Item>,
    S::Item: rodio::cpal::Sample + Send,
{
    let initial = gains.load();
    let updates = gains.clone();
    let source = rodio::source::ChannelVolume::new(source, vec![initial.left, initial.right])
        .periodic_access(AUDIO_POLL_INTERVAL, move |source| {
            let current = updates.load();
            source.set_volume(0, current.left);
            source.set_volume(1, current.right);
        });
    sink.append(source);
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
fn start_voice_on_thread(
    stream_handle: &rodio::OutputStreamHandle,
    encoded: Arc<[u8]>,
    looping: bool,
    gains: StereoGains,
    bus_volume: f32,
) -> Result<ActiveVoice, AudioError> {
    let decoder = decoder_for_encoded(encoded)?;
    let sink = rodio::Sink::try_new(stream_handle).map_err(|source| AudioError::Playback {
        message: source.to_string(),
    })?;
    sink.set_volume(bus_volume);
    let control = VoiceGainControl::new(gains);
    if looping {
        append_spatial_source(&sink, decoder.repeat_infinite(), control.clone());
    } else {
        append_spatial_source(&sink, decoder, control.clone());
    }
    Ok(ActiveVoice {
        sink,
        gains: control,
    })
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
