//! Dependency-neutral Timeline scheduling and playback contracts (ADR 0126).
//!
//! Canonical time is a signed 48 kHz integer tick. Compiled schedules are
//! immutable/shareable and every player owns only transient playback state.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

use std::fmt;
use std::sync::Arc;

/// Canonical Timeline clock rate.
pub const TIMELINE_TICKS_PER_SECOND: i64 = 48_000;

/// Exact persisted Timeline position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct TimelineTick(i64);

impl TimelineTick {
    /// Tick zero.
    pub const ZERO: Self = Self(0);
    /// Creates an exact tick.
    pub const fn new(value: i64) -> Self { Self(value) }
    /// Returns the integer value.
    pub const fn get(self) -> i64 { self.0 }
    /// Converts to presentation seconds.
    pub fn to_seconds_f64(self) -> f64 { self.0 as f64 / TIMELINE_TICKS_PER_SECOND as f64 }
    /// Adds with saturation.
    pub const fn saturating_add(self, delta: i64) -> Self { Self(self.0.saturating_add(delta)) }
    /// Clamps this value.
    pub fn clamp(self, minimum: Self, maximum: Self) -> Self { Self(self.0.clamp(minimum.0, maximum.0)) }
}

impl fmt::Display for TimelineTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

/// Discontinuous-time reconstruction contract for a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SeekCapability {
    /// Pure sample of target tick.
    Stateless,
    /// Target runtime supports deterministic seek/sample.
    Seekable,
    /// Runtime must reconstruct by replay.
    ReplayRequired,
    /// No meaningful discontinuous sample exists.
    NonSeekable,
}

/// Why a schedule is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationMode {
    /// Forward/reverse production playback.
    Playback,
    /// Discontinuous seek/scrub.
    Seek {
        /// Explicit opt-in for gameplay/irreversible point events.
        preview_events: bool,
    },
}

/// One immutable interval or marker point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledSpan {
    /// Active half-open interval `[start,end)`.
    Interval {
        /// Inclusive start.
        start: TimelineTick,
        /// Exclusive end.
        end: TimelineTick,
    },
    /// Exact point.
    Point {
        /// Marker tick.
        tick: TimelineTick,
    },
}

impl CompiledSpan {
    /// First represented tick.
    pub const fn start(self) -> TimelineTick {
        match self { Self::Interval { start, .. } => start, Self::Point { tick } => tick }
    }
}

/// One typed immutable compiled entry.
#[derive(Debug, Clone)]
pub struct CompiledEntry<P> {
    track_order: u32,
    item_order: u32,
    span: CompiledSpan,
    seek: SeekCapability,
    point_side_effect: bool,
    payload: P,
}

impl<P> CompiledEntry<P> {
    /// Creates an interval entry.
    pub fn interval(track_order: u32, item_order: u32, start: TimelineTick, end: TimelineTick, seek: SeekCapability, payload: P) -> Self {
        Self { track_order, item_order, span: CompiledSpan::Interval { start, end }, seek, point_side_effect: false, payload }
    }
    /// Creates a point entry.
    pub fn point(track_order: u32, item_order: u32, tick: TimelineTick, seek: SeekCapability, side_effect: bool, payload: P) -> Self {
        Self { track_order, item_order, span: CompiledSpan::Point { tick }, seek, point_side_effect: side_effect, payload }
    }
    /// Deterministic track order.
    pub const fn track_order(&self) -> u32 { self.track_order }
    /// Deterministic item order.
    pub const fn item_order(&self) -> u32 { self.item_order }
    /// Compiled span.
    pub const fn span(&self) -> CompiledSpan { self.span }
    /// Seek contract.
    pub const fn seek_capability(&self) -> SeekCapability { self.seek }
    /// Typed payload.
    pub const fn payload(&self) -> &P { &self.payload }
    fn order_key(&self) -> (u32, TimelineTick, u32, u8) {
        let kind = if matches!(self.span, CompiledSpan::Interval { .. }) { 0 } else { 1 };
        (self.track_order, self.span.start(), self.item_order, kind)
    }
}

/// Compile-time schedule validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledTimelineError {
    /// Negative duration.
    NegativeDuration(TimelineTick),
    /// Invalid interval.
    InvalidInterval {
        /// Start.
        start: TimelineTick,
        /// End.
        end: TimelineTick,
        /// Timeline duration.
        duration: TimelineTick,
    },
    /// Invalid marker.
    InvalidPoint {
        /// Point tick.
        tick: TimelineTick,
        /// Timeline duration.
        duration: TimelineTick,
    },
}

impl fmt::Display for CompiledTimelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeDuration(v) => write!(f, "negative timeline duration {v}"),
            Self::InvalidInterval { start, end, duration } => write!(f, "invalid interval [{start},{end}) for duration {duration}"),
            Self::InvalidPoint { tick, duration } => write!(f, "invalid point {tick} for duration {duration}"),
        }
    }
}
impl std::error::Error for CompiledTimelineError {}

/// Immutable, deterministic schedule shared by players.
#[derive(Debug, Clone)]
pub struct CompiledTimeline<P> {
    duration: TimelineTick,
    entries: Arc<[CompiledEntry<P>]>,
}

impl<P> CompiledTimeline<P> {
    /// Validates, sorts and freezes entries.
    pub fn new(duration: TimelineTick, mut entries: Vec<CompiledEntry<P>>) -> Result<Self, CompiledTimelineError> {
        if duration < TimelineTick::ZERO { return Err(CompiledTimelineError::NegativeDuration(duration)); }
        for entry in &entries {
            match entry.span {
                CompiledSpan::Interval { start, end } if start < TimelineTick::ZERO || start >= end || end > duration => {
                    return Err(CompiledTimelineError::InvalidInterval { start, end, duration });
                }
                CompiledSpan::Point { tick } if tick < TimelineTick::ZERO || tick > duration => {
                    return Err(CompiledTimelineError::InvalidPoint { tick, duration });
                }
                _ => {}
            }
        }
        entries.sort_by_key(CompiledEntry::order_key);
        Ok(Self { duration, entries: entries.into() })
    }
    /// Inclusive final tick.
    pub const fn duration(&self) -> TimelineTick { self.duration }
    /// Frozen entries.
    pub fn entries(&self) -> &[CompiledEntry<P>] { &self.entries }
    /// Evaluates one deterministic transition.
    pub fn evaluate(&self, request: &EvaluationRequest) -> Vec<EvaluationItem<'_, P>> {
        let mut out = Vec::new();
        for entry in self.entries.iter() {
            match entry.span {
                CompiledSpan::Interval { start, end } => {
                    if request.current_tick >= start && request.current_tick < end {
                        out.push(EvaluationItem { entry, local_tick: TimelineTick::new(request.current_tick.0 - start.0), decision: decision(entry.seek, request.mode, start) });
                    }
                }
                CompiledSpan::Point { tick } => {
                    let selected = match request.mode {
                        EvaluationMode::Playback => request.current_tick >= request.previous_tick && tick > request.previous_tick && tick <= request.current_tick,
                        EvaluationMode::Seek { preview_events } => tick == request.current_tick && (!entry.point_side_effect || preview_events),
                    };
                    if selected { out.push(EvaluationItem { entry, local_tick: TimelineTick::ZERO, decision: decision(entry.seek, request.mode, tick) }); }
                }
            }
        }
        out
    }
}

fn decision(seek: SeekCapability, mode: EvaluationMode, origin: TimelineTick) -> EvaluationDecision {
    if !matches!(mode, EvaluationMode::Seek { .. }) { return EvaluationDecision::Apply; }
    match seek {
        SeekCapability::Stateless | SeekCapability::Seekable => EvaluationDecision::Apply,
        SeekCapability::ReplayRequired => EvaluationDecision::ReplayRequired { from_tick: origin },
        SeekCapability::NonSeekable => EvaluationDecision::NonSeekable,
    }
}

/// Required action for a selected entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationDecision {
    /// Apply/sample now.
    Apply,
    /// Reconstruct deterministically from this origin.
    ReplayRequired {
        /// Earliest deterministic origin.
        from_tick: TimelineTick,
    },
    /// Suppress rather than fabricate a seek result.
    NonSeekable,
}

/// One selected entry.
#[derive(Debug, Clone, Copy)]
pub struct EvaluationItem<'a, P> { entry: &'a CompiledEntry<P>, local_tick: TimelineTick, decision: EvaluationDecision }
impl<'a, P> EvaluationItem<'a, P> {
    /// Entry.
    pub const fn entry(&self) -> &'a CompiledEntry<P> { self.entry }
    /// Tick relative to clip start.
    pub const fn local_tick(&self) -> TimelineTick { self.local_tick }
    /// Seek action.
    pub const fn decision(&self) -> EvaluationDecision { self.decision }
}

/// One exact transition produced by one player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationRequest {
    /// Previous playhead.
    pub previous_tick: TimelineTick,
    /// Current playhead.
    pub current_tick: TimelineTick,
    /// Evaluation reason.
    pub mode: EvaluationMode,
    /// Monotonic player generation.
    pub generation: u64,
}

/// Adapter boundary consumed by production domain owners.
pub trait TimelineEvaluationAdapter<P> {
    /// Error returned by the owner.
    type Error;
    /// Applies one selected item.
    fn apply(&mut self, request: &EvaluationRequest, item: EvaluationItem<'_, P>) -> Result<(), Self::Error>;
}

/// Evaluates then applies in deterministic schedule order.
pub fn evaluate_with_adapter<P, A: TimelineEvaluationAdapter<P>>(timeline: &CompiledTimeline<P>, request: &EvaluationRequest, adapter: &mut A) -> Result<(), A::Error> {
    for item in timeline.evaluate(request) { adapter.apply(request, item)?; }
    Ok(())
}

/// Runtime playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelinePlaybackState {
    /// Stopped.
    Stopped,
    /// Advancing.
    Playing,
    /// Held.
    Paused,
}

/// Deterministic rational playback rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackRate { numerator: i32, denominator: u32 }
impl PlaybackRate {
    /// Normal 1x.
    pub const ONE: Self = Self { numerator: 1, denominator: 1 };
    /// Creates a bounded rational rate.
    pub fn new(numerator: i32, denominator: u32) -> Result<Self, PlaybackRateError> {
        if denominator == 0 || (numerator as i64).unsigned_abs() > denominator as u64 * 16 { return Err(PlaybackRateError); }
        Ok(Self { numerator, denominator })
    }
    /// Numerator.
    pub const fn numerator(self) -> i32 { self.numerator }
    /// Denominator.
    pub const fn denominator(self) -> u32 { self.denominator }
}

/// Invalid playback-rate request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackRateError;
impl fmt::Display for PlaybackRateError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("playback rate must have nonzero denominator and magnitude <= 16x") } }
impl std::error::Error for PlaybackRateError {}

/// Optional half-open loop range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineLoop {
    /// Inclusive loop start.
    pub start: TimelineTick,
    /// Exclusive loop end.
    pub end: TimelineTick,
}

/// Error for invalid player bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineLoopError;
impl fmt::Display for TimelineLoopError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("loop must be nonempty and within timeline duration") } }
impl std::error::Error for TimelineLoopError {}

/// Per-instance transient playback state.
#[derive(Debug, Clone)]
pub struct TimelinePlayer {
    duration: TimelineTick,
    tick: TimelineTick,
    state: TimelinePlaybackState,
    rate: PlaybackRate,
    rate_remainder: i128,
    loop_range: Option<TimelineLoop>,
    generation: u64,
}

impl TimelinePlayer {
    /// Creates stopped state for a compiled duration.
    pub fn new(duration: TimelineTick) -> Self {
        Self { duration: duration.clamp(TimelineTick::ZERO, TimelineTick::new(i64::MAX)), tick: TimelineTick::ZERO, state: TimelinePlaybackState::Stopped, rate: PlaybackRate::ONE, rate_remainder: 0, loop_range: None, generation: 0 }
    }
    /// State.
    pub const fn state(&self) -> TimelinePlaybackState { self.state }
    /// Current tick.
    pub const fn tick(&self) -> TimelineTick { self.tick }
    /// Generation.
    pub const fn generation(&self) -> u64 { self.generation }
    /// Playback rate.
    pub const fn rate(&self) -> PlaybackRate { self.rate }
    /// Loop range.
    pub const fn loop_range(&self) -> Option<TimelineLoop> { self.loop_range }
    /// Starts or resumes.
    pub fn play(&mut self) { self.state = TimelinePlaybackState::Playing; }
    /// Pauses without changing playhead.
    pub fn pause(&mut self) { if self.state == TimelinePlaybackState::Playing { self.state = TimelinePlaybackState::Paused; } }
    /// Stops, resets playhead and invalidates transient owner state.
    pub fn stop(&mut self) { self.state = TimelinePlaybackState::Stopped; self.tick = TimelineTick::ZERO; self.rate_remainder = 0; self.generation = self.generation.wrapping_add(1); }
    /// Restarts at zero and plays.
    pub fn restart(&mut self) { self.stop(); self.play(); }
    /// Sets deterministic playback rate.
    pub fn set_rate(&mut self, rate: PlaybackRate) { self.rate = rate; self.rate_remainder = 0; }
    /// Configures a loop range.
    pub fn set_loop(&mut self, range: Option<TimelineLoop>) -> Result<(), TimelineLoopError> {
        if let Some(r) = range {
            if r.start < TimelineTick::ZERO || r.start >= r.end || r.end > self.duration {
                return Err(TimelineLoopError);
            }
        }
        self.loop_range = range;
        Ok(())
    }
    /// Seeks and returns the exact discontinuous evaluation request.
    pub fn seek(&mut self, tick: TimelineTick, preview_events: bool) -> EvaluationRequest {
        let previous = self.tick; self.tick = tick.clamp(TimelineTick::ZERO, self.duration); self.rate_remainder = 0; self.generation = self.generation.wrapping_add(1);
        EvaluationRequest { previous_tick: previous, current_tick: self.tick, mode: EvaluationMode::Seek { preview_events }, generation: self.generation }
    }
    /// Advances by canonical host ticks. Loop crossings are split so point events
    /// preserve `(previous,current]` ordering on both sides of the wrap.
    pub fn advance_ticks(&mut self, host_ticks: i64) -> Vec<EvaluationRequest> {
        if self.state != TimelinePlaybackState::Playing || host_ticks == 0 || self.rate.numerator == 0 { return Vec::new(); }
        let scaled = host_ticks as i128 * self.rate.numerator as i128 + self.rate_remainder;
        let denominator = self.rate.denominator as i128;
        let delta128 = scaled / denominator; self.rate_remainder = scaled % denominator;
        let delta = delta128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        if delta == 0 { return Vec::new(); }
        if delta < 0 {
            let previous = self.tick; self.tick = self.tick.saturating_add(delta).clamp(TimelineTick::ZERO, self.duration);
            return vec![EvaluationRequest { previous_tick: previous, current_tick: self.tick, mode: EvaluationMode::Playback, generation: self.generation }];
        }
        if let Some(range) = self.loop_range { return self.advance_forward_looped(delta, range); }
        let previous = self.tick; self.tick = self.tick.saturating_add(delta).clamp(TimelineTick::ZERO, self.duration);
        if self.tick == self.duration { self.state = TimelinePlaybackState::Stopped; }
        vec![EvaluationRequest { previous_tick: previous, current_tick: self.tick, mode: EvaluationMode::Playback, generation: self.generation }]
    }
    fn advance_forward_looped(&mut self, mut delta: i64, range: TimelineLoop) -> Vec<EvaluationRequest> {
        let mut requests = Vec::new();
        if self.tick < range.start || self.tick >= range.end { self.tick = range.start; self.generation = self.generation.wrapping_add(1); }
        while delta > 0 {
            let remaining = range.end.get() - self.tick.get();
            if delta < remaining {
                let previous = self.tick; self.tick = self.tick.saturating_add(delta); delta = 0;
                requests.push(EvaluationRequest { previous_tick: previous, current_tick: self.tick, mode: EvaluationMode::Playback, generation: self.generation });
            } else {
                let previous = self.tick; self.tick = range.end; delta -= remaining;
                requests.push(EvaluationRequest { previous_tick: previous, current_tick: range.end, mode: EvaluationMode::Playback, generation: self.generation });
                self.tick = range.start; self.generation = self.generation.wrapping_add(1);
                requests.push(EvaluationRequest { previous_tick: range.start, current_tick: range.start, mode: EvaluationMode::Seek { preview_events: false }, generation: self.generation });
            }
        }
        requests
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_rate_is_48khz() { assert_eq!(TIMELINE_TICKS_PER_SECOND, 48_000); }
    #[test]
    fn compile_order_is_deterministic() {
        let t = CompiledTimeline::new(TimelineTick::new(100), vec![
            CompiledEntry::point(1, 0, TimelineTick::new(5), SeekCapability::Stateless, false, "b"),
            CompiledEntry::point(0, 1, TimelineTick::new(7), SeekCapability::Stateless, false, "a2"),
            CompiledEntry::point(0, 0, TimelineTick::new(7), SeekCapability::Stateless, false, "a1"),
        ]).unwrap();
        assert_eq!(t.entries().iter().map(|e| *e.payload()).collect::<Vec<_>>(), vec!["a1", "a2", "b"]);
    }
    #[test]
    fn forward_event_crossing_is_open_closed() {
        let t = CompiledTimeline::new(TimelineTick::new(20), vec![CompiledEntry::point(0,0,TimelineTick::new(10),SeekCapability::Stateless,true,())]).unwrap();
        assert_eq!(t.evaluate(&EvaluationRequest { previous_tick: TimelineTick::new(10), current_tick: TimelineTick::new(11), mode: EvaluationMode::Playback, generation: 0 }).len(), 0);
        assert_eq!(t.evaluate(&EvaluationRequest { previous_tick: TimelineTick::new(9), current_tick: TimelineTick::new(10), mode: EvaluationMode::Playback, generation: 0 }).len(), 1);
    }
    #[test]
    fn scrub_suppresses_side_effect_points_by_default() {
        let t = CompiledTimeline::new(TimelineTick::new(20), vec![CompiledEntry::point(0,0,TimelineTick::new(10),SeekCapability::Stateless,true,())]).unwrap();
        assert!(t.evaluate(&EvaluationRequest { previous_tick: TimelineTick::ZERO, current_tick: TimelineTick::new(10), mode: EvaluationMode::Seek { preview_events: false }, generation: 1 }).is_empty());
    }
    #[test]
    fn players_are_independent() {
        let mut a = TimelinePlayer::new(TimelineTick::new(100)); let mut b = a.clone(); a.play(); a.advance_ticks(7);
        assert_eq!(a.tick(), TimelineTick::new(7)); assert_eq!(b.tick(), TimelineTick::ZERO); b.play(); b.advance_ticks(2); assert_eq!(b.tick(), TimelineTick::new(2));
    }
    #[test]
    fn loop_splits_crossing_and_bumps_generation() {
        let mut p = TimelinePlayer::new(TimelineTick::new(100)); p.set_loop(Some(TimelineLoop { start: TimelineTick::new(10), end: TimelineTick::new(20) })).unwrap(); p.seek(TimelineTick::new(18), false); p.play();
        let g = p.generation(); let requests = p.advance_ticks(5); assert_eq!(p.tick(), TimelineTick::new(13)); assert!(requests.len() >= 3); assert!(p.generation() > g);
    }
}
