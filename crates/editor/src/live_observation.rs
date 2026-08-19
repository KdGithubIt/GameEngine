//! Bounded engine-native live observation sessions for Remote AI Studio.
//!
//! This module owns transient media-session state and encoding above the
//! renderer-owned `FrameCapture` boundary. Live samples are never persisted as
//! Agent Host captured-frame evidence and never carry authoring authority.

use crate::FrameCapture;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const DEFAULT_LIVE_OBSERVATION_FPS: u8 = 4;
pub(crate) const MAX_LIVE_OBSERVATION_FPS: u8 = 8;
const MAX_LIVE_SESSIONS: usize = 4;
const MAX_OUTPUT_WIDTH: u32 = 1280;
const MAX_OUTPUT_HEIGHT: u32 = 720;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveObservationSource {
    GameView,
}

impl LiveObservationSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GameView => "game_view",
        }
    }
}

#[derive(Debug)]
pub(crate) enum LiveObservationError {
    InvalidFps,
    TooManySessions,
    NotFound,
    Unauthorized,
    Random(String),
    InvalidFrame(String),
    Encode(String),
}

impl fmt::Display for LiveObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFps => write!(
                formatter,
                "live observation max_fps must be between 1 and {MAX_LIVE_OBSERVATION_FPS}"
            ),
            Self::TooManySessions => formatter.write_str("too many live observation sessions are active"),
            Self::NotFound => formatter.write_str("live observation media session or frame was not found"),
            Self::Unauthorized => formatter.write_str("live observation media authentication failed"),
            Self::Random(error) => write!(formatter, "could not create live observation credential: {error}"),
            Self::InvalidFrame(error) => write!(formatter, "live observation frame is invalid: {error}"),
            Self::Encode(error) => write!(formatter, "live observation frame encoding failed: {error}"),
        }
    }
}

impl std::error::Error for LiveObservationError {}

#[derive(Debug, Clone)]
pub(crate) struct LiveObservationStarted {
    pub(crate) media_session_id: String,
    pub(crate) media_token: String,
    pub(crate) run_id: String,
    pub(crate) max_fps: u8,
}

#[derive(Debug)]
struct EncodedFrame {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

trait LiveMediaEncoder {
    fn codec(&self) -> &'static str;
    fn encode(&self, capture: &FrameCapture) -> Result<EncodedFrame, LiveObservationError>;
}

struct PngLiveMediaEncoder;

impl LiveMediaEncoder for PngLiveMediaEncoder {
    fn codec(&self) -> &'static str {
        "png"
    }

    fn encode(&self, capture: &FrameCapture) -> Result<EncodedFrame, LiveObservationError> {
        let (width, height, rgba8) = bounded_rgba8(capture)?;
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|error| LiveObservationError::Encode(error.to_string()))?;
            writer
                .write_image_data(&rgba8)
                .map_err(|error| LiveObservationError::Encode(error.to_string()))?;
        }
        Ok(EncodedFrame {
            width,
            height,
            bytes,
        })
    }
}

#[derive(Debug)]
struct LiveObservationSample {
    sequence: u64,
    width: u32,
    height: u32,
    produced_unix_ms: u64,
    readback_micros: u64,
    encode_micros: u64,
    end_to_end_micros: u64,
    bytes: Arc<Vec<u8>>,
}

#[derive(Debug)]
struct LiveObservationSession {
    id: String,
    run_id: String,
    media_token: String,
    source: LiveObservationSource,
    max_fps: u8,
    created_unix_ms: u64,
    last_capture_attempt: Option<Instant>,
    latest: Option<Arc<LiveObservationSample>>,
    last_error: Option<&'static str>,
    capture_count: u64,
    total_readback_micros: u64,
    total_encode_micros: u64,
    total_end_to_end_micros: u64,
    max_readback_micros: u64,
}

impl LiveObservationSession {
    fn capture_interval(&self) -> Duration {
        Duration::from_micros(1_000_000 / u64::from(self.max_fps))
    }

    fn capture_due(&self, now: Instant) -> bool {
        self.last_capture_attempt
            .is_none_or(|last| now.duration_since(last) >= self.capture_interval())
    }

    fn status_json(&self, codec: &str) -> Value {
        let latest = self.latest.as_ref();
        let average = |total: u64| {
            (self.capture_count > 0).then(|| total / self.capture_count)
        };
        json!({
            "media_session_id": self.id,
            "run_id": self.run_id,
            "source": self.source.as_str(),
            "codec": codec,
            "max_fps": self.max_fps,
            "created_unix_ms": self.created_unix_ms,
            "latest_sequence": latest.map(|sample| sample.sequence),
            "latest_width": latest.map(|sample| sample.width),
            "latest_height": latest.map(|sample| sample.height),
            "latest_produced_unix_ms": latest.map(|sample| sample.produced_unix_ms),
            "latest_bytes": latest.map(|sample| sample.bytes.len()),
            "capture_count": self.capture_count,
            "latest_readback_micros": latest.map(|sample| sample.readback_micros),
            "latest_encode_micros": latest.map(|sample| sample.encode_micros),
            "latest_end_to_end_micros": latest.map(|sample| sample.end_to_end_micros),
            "average_readback_micros": average(self.total_readback_micros),
            "average_encode_micros": average(self.total_encode_micros),
            "average_end_to_end_micros": average(self.total_end_to_end_micros),
            "max_readback_micros": (self.capture_count > 0).then_some(self.max_readback_micros),
            "last_error": self.last_error,
        })
    }
}

#[derive(Default)]
pub(crate) struct LiveObservationManager {
    sessions: BTreeMap<String, LiveObservationSession>,
    pending_session_ids: Vec<String>,
    pending_started_at: Option<Instant>,
    next_sequence: u64,
}

impl LiveObservationManager {
    pub(crate) fn start(
        &mut self,
        run_id: &str,
        max_fps: u8,
    ) -> Result<LiveObservationStarted, LiveObservationError> {
        if max_fps == 0 || max_fps > MAX_LIVE_OBSERVATION_FPS {
            return Err(LiveObservationError::InvalidFps);
        }
        self.remove_run(run_id);
        if self.sessions.len() >= MAX_LIVE_SESSIONS {
            return Err(LiveObservationError::TooManySessions);
        }
        let id = random_hex("media", 16)?;
        let media_token = random_hex("media-token", 32)?;
        let session = LiveObservationSession {
            id: id.clone(),
            run_id: run_id.to_owned(),
            media_token: media_token.clone(),
            source: LiveObservationSource::GameView,
            max_fps,
            created_unix_ms: unix_ms(),
            last_capture_attempt: None,
            latest: None,
            last_error: None,
            capture_count: 0,
            total_readback_micros: 0,
            total_encode_micros: 0,
            total_end_to_end_micros: 0,
            max_readback_micros: 0,
        };
        self.sessions.insert(id.clone(), session);
        Ok(LiveObservationStarted {
            media_session_id: id,
            media_token,
            run_id: run_id.to_owned(),
            max_fps,
        })
    }

    pub(crate) fn stop(
        &mut self,
        media_session_id: &str,
        media_token: &str,
    ) -> Result<(), LiveObservationError> {
        self.authenticate(media_session_id, media_token)?;
        self.sessions.remove(media_session_id);
        self.pending_session_ids
            .retain(|id| id != media_session_id);
        Ok(())
    }

    pub(crate) fn remove_run(&mut self, run_id: &str) {
        let removed = self
            .sessions
            .iter()
            .filter(|(_, session)| session.run_id == run_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in removed {
            self.sessions.remove(&id);
            self.pending_session_ids.retain(|pending| pending != &id);
        }
    }

    pub(crate) fn run_ids(&self) -> Vec<String> {
        self.sessions
            .values()
            .map(|session| session.run_id.clone())
            .collect()
    }

    pub(crate) fn status_json(
        &self,
        media_session_id: &str,
        media_token: &str,
    ) -> Result<Value, LiveObservationError> {
        let session = self.authenticate(media_session_id, media_token)?;
        Ok(session.status_json(PngLiveMediaEncoder.codec()))
    }

    pub(crate) fn frame_bytes(
        &self,
        media_session_id: &str,
        media_token: &str,
        sequence: u64,
    ) -> Result<Vec<u8>, LiveObservationError> {
        let session = self.authenticate(media_session_id, media_token)?;
        let sample = session.latest.as_ref().ok_or(LiveObservationError::NotFound)?;
        if sample.sequence != sequence {
            return Err(LiveObservationError::NotFound);
        }
        Ok(sample.bytes.as_ref().clone())
    }

    pub(crate) fn begin_capture(&mut self) -> bool {
        if !self.pending_session_ids.is_empty() {
            return false;
        }
        let now = Instant::now();
        self.pending_session_ids = self
            .sessions
            .iter_mut()
            .filter_map(|(id, session)| {
                if session.capture_due(now) {
                    session.last_capture_attempt = Some(now);
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        if self.pending_session_ids.is_empty() {
            return false;
        }
        self.pending_started_at = Some(now);
        true
    }

    pub(crate) fn report_capture(
        &mut self,
        capture: &FrameCapture,
        readback: Duration,
    ) -> Result<(), LiveObservationError> {
        if self.pending_session_ids.is_empty() {
            return Ok(());
        }
        let encode_started = Instant::now();
        let encoded = match PngLiveMediaEncoder.encode(capture) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.finish_failure();
                return Err(error);
            }
        };
        let encode = encode_started.elapsed();
        let end_to_end = self
            .pending_started_at
            .map_or(readback + encode, |started| started.elapsed());
        self.next_sequence = self.next_sequence.saturating_add(1).max(1);
        let sample = Arc::new(LiveObservationSample {
            sequence: self.next_sequence,
            width: encoded.width,
            height: encoded.height,
            produced_unix_ms: unix_ms(),
            readback_micros: duration_micros(readback),
            encode_micros: duration_micros(encode),
            end_to_end_micros: duration_micros(end_to_end),
            bytes: Arc::new(encoded.bytes),
        });
        for id in std::mem::take(&mut self.pending_session_ids) {
            let Some(session) = self.sessions.get_mut(&id) else {
                continue;
            };
            session.capture_count = session.capture_count.saturating_add(1);
            session.total_readback_micros = session
                .total_readback_micros
                .saturating_add(sample.readback_micros);
            session.total_encode_micros = session
                .total_encode_micros
                .saturating_add(sample.encode_micros);
            session.total_end_to_end_micros = session
                .total_end_to_end_micros
                .saturating_add(sample.end_to_end_micros);
            session.max_readback_micros = session.max_readback_micros.max(sample.readback_micros);
            session.latest = Some(Arc::clone(&sample));
            session.last_error = None;
        }
        self.pending_started_at = None;
        Ok(())
    }

    pub(crate) fn report_capture_failure(&mut self) {
        self.finish_failure();
    }

    fn finish_failure(&mut self) {
        for id in std::mem::take(&mut self.pending_session_ids) {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.last_error = Some("Game View capture is temporarily unavailable; retry is allowed.");
            }
        }
        self.pending_started_at = None;
    }

    fn authenticate(
        &self,
        media_session_id: &str,
        media_token: &str,
    ) -> Result<&LiveObservationSession, LiveObservationError> {
        let session = self
            .sessions
            .get(media_session_id)
            .ok_or(LiveObservationError::NotFound)?;
        if media_token.is_empty() || session.media_token != media_token {
            return Err(LiveObservationError::Unauthorized);
        }
        Ok(session)
    }
}

fn bounded_rgba8(capture: &FrameCapture) -> Result<(u32, u32, Vec<u8>), LiveObservationError> {
    if capture.width == 0 || capture.height == 0 {
        return Err(LiveObservationError::InvalidFrame(
            "dimensions must be non-zero".to_owned(),
        ));
    }
    let expected_len = u64::from(capture.width)
        .checked_mul(u64::from(capture.height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| LiveObservationError::InvalidFrame("pixel dimensions overflow".to_owned()))?;
    if capture.rgba8.len() != expected_len {
        return Err(LiveObservationError::InvalidFrame(
            "RGBA8 byte count does not match dimensions".to_owned(),
        ));
    }
    if capture.width <= MAX_OUTPUT_WIDTH && capture.height <= MAX_OUTPUT_HEIGHT {
        return Ok((capture.width, capture.height, capture.rgba8.clone()));
    }

    let (width, height) = if u64::from(capture.width) * u64::from(MAX_OUTPUT_HEIGHT)
        >= u64::from(capture.height) * u64::from(MAX_OUTPUT_WIDTH)
    {
        let height = (u64::from(capture.height) * u64::from(MAX_OUTPUT_WIDTH)
            / u64::from(capture.width))
        .max(1) as u32;
        (MAX_OUTPUT_WIDTH, height)
    } else {
        let width = (u64::from(capture.width) * u64::from(MAX_OUTPUT_HEIGHT)
            / u64::from(capture.height))
        .max(1) as u32;
        (width, MAX_OUTPUT_HEIGHT)
    };
    let output_len = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| LiveObservationError::InvalidFrame("bounded dimensions overflow".to_owned()))?;
    let mut rgba8 = vec![0_u8; output_len];
    for y in 0..height {
        let source_y = u64::from(y) * u64::from(capture.height) / u64::from(height);
        for x in 0..width {
            let source_x = u64::from(x) * u64::from(capture.width) / u64::from(width);
            let source_index = usize::try_from(
                (source_y * u64::from(capture.width) + source_x) * 4,
            )
            .map_err(|_| LiveObservationError::InvalidFrame("source index overflow".to_owned()))?;
            let output_index = usize::try_from((u64::from(y) * u64::from(width) + u64::from(x)) * 4)
                .map_err(|_| LiveObservationError::InvalidFrame("output index overflow".to_owned()))?;
            rgba8[output_index..output_index + 4]
                .copy_from_slice(&capture.rgba8[source_index..source_index + 4]);
        }
    }
    Ok((width, height, rgba8))
}

fn random_hex(prefix: &str, byte_count: usize) -> Result<String, LiveObservationError> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes)
        .map_err(|error| LiveObservationError::Random(error.to_string()))?;
    let mut value = String::with_capacity(prefix.len() + 1 + byte_count * 2);
    value.push_str(prefix);
    value.push('-');
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}")
            .expect("writing hexadecimal text into String cannot fail");
    }
    Ok(value)
}

fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> FrameCapture {
        FrameCapture {
            width,
            height,
            rgba8: vec![127; width as usize * height as usize * 4],
        }
    }

    #[test]
    fn session_requires_media_specific_authentication_and_restart_rotates_it() {
        let mut manager = LiveObservationManager::default();
        let first = manager.start("run-a", 4).expect("first session");
        assert!(manager
            .status_json(&first.media_session_id, "wrong")
            .is_err());
        let second = manager.start("run-a", 4).expect("replacement session");
        assert_ne!(first.media_session_id, second.media_session_id);
        assert_ne!(first.media_token, second.media_token);
        assert!(manager
            .status_json(&first.media_session_id, &first.media_token)
            .is_err());
    }

    #[test]
    fn live_capture_is_rate_bounded_and_keeps_only_latest_sequence() {
        let mut manager = LiveObservationManager::default();
        let session = manager.start("run-a", 4).expect("session");
        assert!(manager.begin_capture());
        manager
            .report_capture(&frame(2, 2), Duration::from_micros(50))
            .expect("capture");
        assert!(!manager.begin_capture());
        let status = manager
            .status_json(&session.media_session_id, &session.media_token)
            .expect("status");
        let sequence = status["latest_sequence"].as_u64().expect("sequence");
        let bytes = manager
            .frame_bytes(&session.media_session_id, &session.media_token, sequence)
            .expect("frame");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(status["latest_readback_micros"].as_u64().is_some());
        assert!(status["latest_encode_micros"].as_u64().is_some());
        assert!(status["latest_end_to_end_micros"].as_u64().is_some());
    }

    #[test]
    fn live_encoder_downscales_without_upscaling() {
        let small = PngLiveMediaEncoder.encode(&frame(4, 2)).expect("small");
        assert_eq!((small.width, small.height), (4, 2));
        let large = PngLiveMediaEncoder
            .encode(&frame(2560, 1440))
            .expect("large");
        assert_eq!((large.width, large.height), (1280, 720));
    }

    #[test]
    fn transient_samples_do_not_create_agent_host_evidence_or_cancel_run() {
        use crate::agent_host::{AgentEventKind, AgentHost, AgentRunState};
        use std::fs;

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let project = std::env::temp_dir().join(format!(
            "gameengine-live-observation-project-{}-{suffix}",
            std::process::id()
        ));
        let storage = std::env::temp_dir().join(format!(
            "gameengine-live-observation-storage-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&project).expect("project");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session_id = host.create_session("Live observation").expect("session");
        let version = host
            .session(&session_id)
            .expect("session")
            .proposal
            .version;
        let run_id = host
            .start_run_authorized(&session_id, version, "test")
            .expect("run");
        let captured_before = host
            .run(&run_id)
            .expect("run")
            .events
            .iter()
            .filter(|event| event.kind == AgentEventKind::CapturedFrame)
            .count();

        let mut manager = LiveObservationManager::default();
        let _session = manager.start(&run_id, 4).expect("media session");
        assert!(manager.begin_capture());
        manager
            .report_capture(&frame(2, 2), Duration::from_micros(10))
            .expect("live sample");
        drop(manager);

        let run = host.run(&run_id).expect("run after media disconnect");
        assert_eq!(
            run.events
                .iter()
                .filter(|event| event.kind == AgentEventKind::CapturedFrame)
                .count(),
            captured_before
        );
        assert!(!matches!(
            run.state,
            AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
        ));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn invalid_frame_rate_is_rejected() {
        let mut manager = LiveObservationManager::default();
        assert!(matches!(
            manager.start("run", 0),
            Err(LiveObservationError::InvalidFps)
        ));
        assert!(matches!(
            manager.start("run", MAX_LIVE_OBSERVATION_FPS + 1),
            Err(LiveObservationError::InvalidFps)
        ));
    }
}
