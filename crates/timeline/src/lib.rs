//! Dependency-neutral Timeline scheduling and playback contracts (ADR 0126).
//!
//! This crate owns canonical integer time, immutable compiled schedule
//! traversal, per-player clocks, seek-policy reporting, and the typed adapter
//! boundary. It intentionally has no dependency on animation, rendering,
//! audio, VFX, physics, authoring, or editor implementations.
//!
//! [`CompiledTimeline`] is generic over its payload. The payload type is the
//! typed extension point for composition layers: a host defines a closed Rust
//! type containing only the domain payloads it actually supports, and an
//! implementation of [`TimelineEvaluationAdapter`] consumes those payloads.
//! The neutral scheduler therefore never needs stringly-typed payloads,
//! `serde_json::Value`, or runtime downcasts.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

use std::fmt;
use std::sync::Arc;

/// Canonical Timeline clock rate in integer ticks per second.
pub const TIMELINE_TICKS_PER_SECOND: i64 = 48_000;

/// One exact position on the canonical Timeline clock.
///
/// Persisted clip boundaries and markers use this integer type. Floating-point
/// seconds are presentation or host-clock inputs only; they are never the
/// canonical stored position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct TimelineTick(i64);

impl TimelineTick {
    /// Tick zero, the start of a Timeline.
    pub const ZERO: Self = Self(0);

    /// Creates an exact Timeline tick.
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the integer tick value.
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Converts this position to seconds for a target-domain API.
    pub fn to_seconds_f64(self) -> f64 {
        self.0 as f64 / TIMELINE_TICKS_PER_SECOND as f64
    }

    /// Adds an integer delta, saturating at the integer limits.
    pub const fn saturating_add(self, delta: i64) -> Self {
        Self(self.0.saturating_add(delta))
    }

    /// Returns this tick clamped to the inclusive range `[minimum, maximum]`.
    pub fn clamp(self, minimum: Self, maximum: Self) -> Self {
        Self(self.0.clamp(minimum.0, maximum.0))
    }
}

impl fmt::Display for TimelineTick {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// How a track payload can reconstruct state after discontinuous time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SeekCapability {
    /// Evaluation is a pure function of the requested target tick.
    Stateless,
    /// The target domain exposes a deterministic seek or sample operation.
    Seekable,
    /// State must be reconstructed by replaying from a known earlier point.
    ReplayRequired,
    /// The target domain cannot provide a meaningful discontinuous sample.
    NonSeekable,
}

/// The reason a compiled schedule is being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationMode {
    /// Normal forward playback from the previous tick to the current tick.
    Playback,
    /// A discontinuous seek/scrub to the current tick.
    Seek {
        /// Whether point-event side effects may be previewed while seeking.
        preview_events: bool,
    },
}

impl EvaluationMode {
    /// Returns whether event side effects may be previewed in this evaluation.
    pub const fn preview_events(self) -> bool {
        match self {
            Self::Playback => true,
            Self::Seek { preview_events } => preview_events,
        }
    }
}

/// One immutable interval or point in a compiled Timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledSpan {
    /// An active half-open clip interval `[start, end)`.
    Interval {
        /// Inclusive first tick of the clip.
        start: TimelineTick,
        /// Exclusive tick at which the clip is no longer active.
        end: TimelineTick,
    },
    /// An exact point marker.
    Point {
        /// Exact marker tick.
        tick: TimelineTick,
    },
}

impl CompiledSpan {
    /// Returns the first tick represented by this span.
    pub const fn start(self) -> TimelineTick {
        match self {
            Self::Interval { start, .. } => start,
            Self::Point { tick } => tick,
        }
    }

    /// Returns whether this span is active at `tick` as sampled state.
    pub const fn contains_sample(self, tick: TimelineTick) -> bool {
        match self {
            Self::Interval { start, end } => tick.0 >= start.0 && tick.0 < end.0,
            Self::Point { tick: point } => tick.0 == point.0,
        }
    }
}

/// One typed immutable entry in a compiled Timeline schedule.
///
/// `P` is intentionally generic. Domain composition layers choose a closed,
/// typed payload enum or struct; the neutral Timeline crate never interprets
/// it or converts it through strings/JSON.
#[derive(Debug, Clone)]
pub struct CompiledEntry<P> {
    track_order: u32,
    item_order: u32,
    span: CompiledSpan,
    seek_capability: SeekCapability,
    point_side_effect: bool,
    payload: P,
}

impl<P> CompiledEntry<P> {
    /// Creates an interval entry.
    pub fn interval(
        track_order: u32,
        item_order: u32,
        start: TimelineTick,
        end: TimelineTick,
        seek_capability: SeekCapability,
        payload: P,
    ) -> Self {
        Self {
            track_order,
            item_order,
            span: CompiledSpan::Interval { start, end },
            seek_capability,
            point_side_effect: false,
            payload,
        }
    }

    /// Creates an exact point entry.
    ///
    /// `side_effect` identifies point entries such as gameplay Event markers
    /// that are suppressed by default during manual seek/scrub.
    pub fn point(
        track_order: u32,
        item_order: u32,
        tick: TimelineTick,
        seek_capability: SeekCapability,
        side_effect: bool,
        payload: P,
    ) -> Self {
        Self {
            track_order,
            item_order,
            span: CompiledSpan::Point { tick },
            seek_capability,
            point_side_effect: side_effect,
            payload,
        }
    }

    /// Returns the deterministic compiled track order.
    pub const fn track_order(&self) -> u32 {
        self.track_order
    }

    /// Returns the deterministic order within the compiled track.
    pub const fn item_order(&self) -> u32 {
        self.item_order
    }

    /// Returns the clip interval or point marker span.
    pub const fn span(&self) -> CompiledSpan {
        self.span
    }

    /// Returns the payload's discontinuous-time capability.
    pub const fn seek_capability(&self) -> SeekCapability {
        self.seek_capability
    }

    /// Returns the strongly typed payload owned by the composition layer.
    pub const fn payload(&self) -> &P {
        &self.payload
    }

    fn order_key(&self) -> (u32, TimelineTick, u32, u8) {
        let kind = match self.span {
            CompiledSpan::Interval { .. } => 0,
            CompiledSpan::Point { .. } => 1,
        };
        (self.track_order, self.span.start(), self.item_order, kind)
    }
}

/// Reports invalid input while constructing an immutable compiled Timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledTimelineError {
    /// The declared Timeline duration was negative.
    NegativeDuration {
        /// Invalid duration.
        duration: TimelineTick,
    },
    /// A compiled interval was empty, reversed, negative, or beyond duration.
    InvalidInterval {
        /// Invalid interval start.
        start: TimelineTick,
        /// Invalid interval end.
        end: TimelineTick,
        /// Timeline duration used for validation.
        duration: TimelineTick,
    },
    /// A point marker was negative or beyond the Timeline duration.
    InvalidPoint {
        /// Invalid point tick.
        tick: TimelineTick,
        /// Timeline duration used for validation.
        duration: TimelineTick,
    },
}

impl fmt::Display for CompiledTimelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeDuration { duration } => {
                write!(formatter, "timeline duration {duration} is negative")
            }
            Self::InvalidInterval {
                start,
                end,
                duration,
            } => write!(
                formatter,
                "compiled interval [{start}, {end}) is outside timeline duration {duration}"
            ),
            Self::InvalidPoint { tick, duration } => write!(
                formatter,
                "compiled point {tick} is outside timeline duration {duration}"
            ),
        }
    }
}

impl std::error::Error for CompiledTimelineError {}

/// Immutable, deterministically ordered Timeline evaluation representation.
///
/// Construction validates all spans and sorts entries by compiled track order,
/// span start, item order, and span kind. The resulting `Arc` slice may be
/// shared by any number of independent [`TimelinePlayer`] instances.
#[derive(Debug, Clone)]
pub struct CompiledTimeline<P> {
    duration: TimelineTick,
    entries: Arc<[CompiledEntry<P>]>,
}

impl<P> CompiledTimeline<P> {
    /// Validates and freezes a compiled schedule.
    pub fn new(
        duration: TimelineTick,
        mut entries: Vec<CompiledEntry<P>>,
    ) -> Result<Self, CompiledTimelineError> {
        if duration < TimelineTick::ZERO {
            return Err(CompiledTimelineError::NegativeDuration { duration });
        }
        for entry in &entries {
            match entry.span {
                CompiledSpan::Interval { start, end }
                    if start < TimelineTick::ZERO || start >= end || end > duration =>
                {
                    return Err(CompiledTimelineError::InvalidInterval {
                        start,
                        end,
                        duration,
                    });
                }
                CompiledSpan::Point { tick }
                    if tick < TimelineTick::ZERO || tick > duration =>
                {
                    return Err(CompiledTimelineError::InvalidPoint { tick, duration });
                }
                _ => {}
            }
        }
        entries.sort_by_key(CompiledEntry::order_key);
        Ok(Self {
            duration,
            entries: entries.into(),
        })
    }

    /// Returns the inclusive final Timeline tick.
    pub const fn duration(&self) -> TimelineTick {
        self.duration
    }

    /// Returns the immutable deterministic schedule.
    pub fn entries(&self) -> &[CompiledEntry<P>] {
        &self.entries
    }

    /// Evaluates the schedule for one player transition.
    ///
    /// Interval state is sampled only at `request.current_tick`. Point entries
    /// fire during forward playback when crossed by `(previous, current]`.
    /// Reverse playback never invents symmetric point-event semantics. During
    /// seek, point side effects are suppressed unless `preview_events` is
    /// explicitly enabled.
    pub fn evaluate(&self, request: &EvaluationRequest) -> Vec<EvaluationItem<'_, P>> {
        let mut output = Vec::new();
        for entry in self.entries.iter() {
            match entry.span {
                CompiledSpan::Interval { start, end } => {
                    if request.current_tick < start || request.current_tick >= end {
                        continue;
                    }
                    output.push(EvaluationItem {
                        entry,
                        local_tick: TimelineTick::new(request.current_tick.0 - start.0),
                        decision: decision_for(entry.seek_capability, request.mode, start),
                    });
                }
                CompiledSpan::Point { tick } => {
                    let selected = match request.mode {
                        EvaluationMode::Playback => {
                            request.current_tick >= request.previous_tick
                                && tick > request.previous_tick
                                && tick <= request.current_tick
                        }
                        EvaluationMode::Seek { preview_events } => {
                            tick == request.current_tick
                                && (!entry.point_side_effect || preview_events)
                        }
                    };
                    if selected {
                        output.push(EvaluationItem {
                            entry,
                            local_tick: TimelineTick::ZERO,
                            decision: decision_for(entry.seek_capability, request.mode, tick),
                        });
                    }
                }
            }
        }
        output
    }
}

fn decision_for(
    capability: SeekCapability,
    mode: EvaluationMode,
    replay_origin: TimelineTick,
) -> EvaluationDecision {
    if !matches!(mode, EvaluationMode::Seek { .. }) {
        return EvaluationDecision::Apply;
    }
    match capability {
        SeekCapability::Stateless | SeekCapability::Seekable => EvaluationDecision::Apply,
        SeekCapability::ReplayRequired => EvaluationDecision::ReplayRequired {
            from_tick: replay_origin,
        },
        SeekCapability::NonSeekable => EvaluationDecision::NonSeekable,
    }
}

/// What a domain adapter must do for one selected compiled entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationDecision {
    /// Apply/sample the payload at the supplied local tick.
    Apply,
    /// Reconstruct state by replaying from an earlier deterministic point.
    ReplayRequired {
        /// Earliest point the neutral scheduler can prove for this entry.
        from_tick: TimelineTick,
    },
    /// Do not fabricate a discontinuous result for this payload.
    NonSeekable,
}

/// One selected entry returned by [`CompiledTimeline::evaluate`].
#[derive(Debug, Clone, Copy)]
pub struct EvaluationItem<'a, P> {
    entry: &'a CompiledEntry<P>,
    local_tick: TimelineTick,
    decision: EvaluationDecision,
}

impl<'a, P> EvaluationItem<'a, P> {
    /// Returns the immutable compiled entry.
    pub const fn entry(&self) -> &'a CompiledEntry<P> {
        self.entry
    }

    /// Returns time relative to the selected interval's start.
    pub const fn local_tick(&self) -> TimelineTick {
        self.local_tick
    }

    /// Returns the discontinuous-time action required by the payload.
    pub const fn decision(&self) -> EvaluationDecision {
        self.decision
    }
}

/// Typed composition-layer hook for consuming neutral evaluation results.
///
/// This is intentionally generic over `P`: adding a future domain such as a
/// production Audio or VFX payload requires an explicit Rust payload type and
/// adapter implementation at the composition layer, not a string/JSON escape
/// hatch in the scheduler.
pub trait TimelineEvaluationAdapter<P> {
    /// Adapter-specific failure returned to the owning runtime host.
    type Error;

    /// Applies one deterministically selected Timeline entry.
    fn apply(
        &mut self,
        request: &EvaluationRequest,
        item: EvaluationItem<'_, P>,
    ) -> Result<(), Self::Error>;
}

/// Evaluates `timeline` and passes each selected item to a typed adapter.
pub fn evaluate_with_adapter<P, A>(
    timeline: &CompiledTimeline<P>,
    request: &EvaluationRequest,
    adapter: &mut A,
) -> Result<(), A::Error>
where
    A: TimelineEvaluationAdapter<P>,
{
    for item in timeline.evaluate(request) {
        adapter.apply(request, item)?;
    }
    Ok(())
}

/// Immutable description of one Timeline evaluation transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationRequest {
    /// Previously evaluated playhead tick.
    pub previous_tick: TimelineTick,
    /// Tick to sample for this evaluation.
    pub current_tick: TimelineTick,
    /// Normal playback or discontinuous seek semantics.
    pub mode: EvaluationMode,
    /// Monotonic generation owned by one [`TimelinePlayer`].
    pub generation: u64,
}

/// Runtime playback state owned by one Timeline player instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelinePlaybackState {
    /// Playback is stopped.
    Stopped,
    /// The playhead advances when the host supplies elapsed time.
    Playing,
    /// The playhead is held at its current position.
    Paused,
}

/// Error returned when an invalid playback rate is requested.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRateError {
    attempted: f64,
}

impl PlaybackRateError {
    /// Returns the rejected rate.
    pub const fn attempted(self) -> f64 {
        self.attempted
    }
}

impl fmt::Display for PlaybackRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "timeline playback rate {} must be finite and non-negative",
            self.attempted
        )
    }
}

impl std::error::Error for PlaybackRateError {}

/// Per-player transient clock and playback state.
///
/// The player stores only neutral time state. Adapter-owned domain state is
/// keyed to [`Self::evaluation_generation`] by the composition layer, so a
/// seek/stop can invalidate stale adapter tokens without making the neutral
/// core depend on their concrete types.
#[derive(Debug, Clone)]
pub struct TimelinePlayer {
    current_tick: TimelineTick,
    previous_tick: TimelineTick,
    state: TimelinePlaybackState,
    playback_rate: f64,
    looping: bool,
    residual_ticks: f64,
    evaluation_generation: u64,
}

impl Default for TimelinePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelinePlayer {
    /// Creates a stopped player at tick zero with rate `1.0`.
    pub const fn new() -> Self {
        Self {
            current_tick: TimelineTick::ZERO,
            previous_tick: TimelineTick::ZERO,
            state: TimelinePlaybackState::Stopped,
            playback_rate: 1.0,
            looping: false,
            residual_ticks: 0.0,
            evaluation_generation: 0,
        }
    }

    /// Returns the current playhead tick.
    pub const fn current_tick(&self) -> TimelineTick {
        self.current_tick
    }

    /// Returns the previously evaluated tick.
    pub const fn previous_tick(&self) -> TimelineTick {
        self.previous_tick
    }

    /// Returns current playback state.
    pub const fn state(&self) -> TimelinePlaybackState {
        self.state
    }

    /// Returns the non-negative playback multiplier.
    pub const fn playback_rate(&self) -> f64 {
        self.playback_rate
    }

    /// Returns whether segmented advancement wraps at the Timeline duration.
    pub const fn looping(&self) -> bool {
        self.looping
    }

    /// Enables or disables exact loop-boundary playback.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Returns the monotonic evaluation generation for this player.
    pub const fn evaluation_generation(&self) -> u64 {
        self.evaluation_generation
    }

    /// Starts or resumes forward playback from the current tick.
    pub fn play(&mut self) {
        self.state = TimelinePlaybackState::Playing;
    }

    /// Pauses playback without changing the current tick.
    pub fn pause(&mut self) {
        self.state = TimelinePlaybackState::Paused;
    }

    /// Stops playback, resets to tick zero, and returns the resulting seek
    /// request so adapters can clear/reconstruct transient state.
    pub fn stop(&mut self) -> EvaluationRequest {
        let previous = self.current_tick;
        self.previous_tick = previous;
        self.current_tick = TimelineTick::ZERO;
        self.state = TimelinePlaybackState::Stopped;
        self.residual_ticks = 0.0;
        self.next_request(previous, TimelineTick::ZERO, EvaluationMode::Seek {
            preview_events: false,
        })
    }

    /// Seeks to `target`, clamped to `[0, duration]`.
    ///
    /// Gameplay Event point side effects are suppressed unless
    /// `preview_events` is explicitly enabled.
    pub fn seek(
        &mut self,
        target: TimelineTick,
        duration: TimelineTick,
        preview_events: bool,
    ) -> EvaluationRequest {
        let duration = non_negative_duration(duration);
        let target = target.clamp(TimelineTick::ZERO, duration);
        let previous = self.current_tick;
        self.previous_tick = previous;
        self.current_tick = target;
        self.residual_ticks = 0.0;
        self.next_request(
            previous,
            target,
            EvaluationMode::Seek { preview_events },
        )
    }

    /// Sets a finite, non-negative playback rate.
    ///
    /// Reverse playback is intentionally not inferred from a negative rate;
    /// reverse Event semantics require an explicit future policy.
    pub fn set_playback_rate(&mut self, rate: f64) -> Result<(), PlaybackRateError> {
        if !rate.is_finite() || rate < 0.0 {
            return Err(PlaybackRateError { attempted: rate });
        }
        self.playback_rate = rate;
        Ok(())
    }

    /// Advances by host-supplied whole Timeline ticks.
    ///
    /// Fractional ticks introduced by playback rate are retained in a residual
    /// accumulator rather than being written back into clip/marker positions.
    pub fn advance_ticks(
        &mut self,
        elapsed_ticks: i64,
        duration: TimelineTick,
    ) -> Option<EvaluationRequest> {
        if self.state != TimelinePlaybackState::Playing || elapsed_ticks <= 0 {
            return None;
        }
        self.advance_exact_ticks(elapsed_ticks as f64 * self.playback_rate, duration)
    }

    /// Advances from a host delta in seconds while preserving canonical
    /// integer Timeline positions and fractional residual ticks.
    pub fn advance_seconds(
        &mut self,
        delta_seconds: f64,
        duration: TimelineTick,
    ) -> Option<EvaluationRequest> {
        if self.state != TimelinePlaybackState::Playing
            || !delta_seconds.is_finite()
            || delta_seconds <= 0.0
        {
            return None;
        }
        self.advance_exact_ticks(
            delta_seconds * TIMELINE_TICKS_PER_SECOND as f64 * self.playback_rate,
            duration,
        )
    }

    /// Advances by whole host ticks and returns every deterministic playback
    /// segment needed to represent loop boundaries exactly.
    ///
    /// Non-looping players return zero or one segment. Looping players split a
    /// wrap into `(previous, duration]` followed by `[-1, 0]`; the virtual
    /// previous tick makes a marker authored at tick zero cross exactly once at
    /// each loop restart without changing the persisted integer time domain.
    pub fn advance_ticks_segmented(
        &mut self,
        elapsed_ticks: i64,
        duration: TimelineTick,
    ) -> Vec<EvaluationRequest> {
        if self.state != TimelinePlaybackState::Playing || elapsed_ticks <= 0 {
            return Vec::new();
        }
        if !self.looping {
            return self
                .advance_exact_ticks(elapsed_ticks as f64 * self.playback_rate, duration)
                .into_iter()
                .collect();
        }
        self.advance_exact_ticks_looping(elapsed_ticks as f64 * self.playback_rate, duration)
    }

    /// Advances from host seconds and returns exact loop-boundary segments.
    pub fn advance_seconds_segmented(
        &mut self,
        delta_seconds: f64,
        duration: TimelineTick,
    ) -> Vec<EvaluationRequest> {
        if self.state != TimelinePlaybackState::Playing
            || !delta_seconds.is_finite()
            || delta_seconds <= 0.0
        {
            return Vec::new();
        }
        let scaled = delta_seconds * TIMELINE_TICKS_PER_SECOND as f64 * self.playback_rate;
        if !self.looping {
            return self
                .advance_exact_ticks(scaled, duration)
                .into_iter()
                .collect();
        }
        self.advance_exact_ticks_looping(scaled, duration)
    }

    fn advance_exact_ticks(
        &mut self,
        scaled_ticks: f64,
        duration: TimelineTick,
    ) -> Option<EvaluationRequest> {
        let duration = non_negative_duration(duration);
        let exact = scaled_ticks + self.residual_ticks;
        let whole = exact.floor();
        self.residual_ticks = exact - whole;
        if whole < 1.0 {
            return None;
        }
        let whole = whole.min(i64::MAX as f64) as i64;
        let previous = self.current_tick;
        let current = previous
            .saturating_add(whole)
            .clamp(TimelineTick::ZERO, duration);
        if current == previous {
            if current >= duration {
                self.state = TimelinePlaybackState::Stopped;
            }
            return None;
        }
        self.previous_tick = previous;
        self.current_tick = current;
        if current >= duration {
            self.state = TimelinePlaybackState::Stopped;
            self.residual_ticks = 0.0;
        }
        Some(self.next_request(previous, current, EvaluationMode::Playback))
    }

    fn advance_exact_ticks_looping(
        &mut self,
        scaled_ticks: f64,
        duration: TimelineTick,
    ) -> Vec<EvaluationRequest> {
        let duration = non_negative_duration(duration);
        if duration == TimelineTick::ZERO {
            self.current_tick = TimelineTick::ZERO;
            self.previous_tick = TimelineTick::ZERO;
            self.state = TimelinePlaybackState::Stopped;
            self.residual_ticks = 0.0;
            return Vec::new();
        }

        let exact = scaled_ticks + self.residual_ticks;
        let whole = exact.floor();
        self.residual_ticks = exact - whole;
        if whole < 1.0 {
            return Vec::new();
        }

        let mut remaining = whole.min(i64::MAX as f64) as i64;
        let mut requests = Vec::new();
        while remaining > 0 {
            let previous = self.current_tick;
            let until_end = duration.get().saturating_sub(previous.get());
            if remaining < until_end {
                let current = previous.saturating_add(remaining);
                self.previous_tick = previous;
                self.current_tick = current;
                requests.push(self.next_request(previous, current, EvaluationMode::Playback));
                break;
            }

            if until_end > 0 {
                self.previous_tick = previous;
                self.current_tick = duration;
                requests.push(self.next_request(previous, duration, EvaluationMode::Playback));
                remaining = remaining.saturating_sub(until_end);
            }

            self.current_tick = TimelineTick::ZERO;
            let virtual_previous = TimelineTick::new(-1);
            requests.push(self.next_request(
                virtual_previous,
                TimelineTick::ZERO,
                EvaluationMode::Playback,
            ));
            self.previous_tick = TimelineTick::ZERO;
        }
        requests
    }

    fn next_request(
        &mut self,
        previous_tick: TimelineTick,
        current_tick: TimelineTick,
        mode: EvaluationMode,
    ) -> EvaluationRequest {
        self.evaluation_generation = self.evaluation_generation.wrapping_add(1);
        EvaluationRequest {
            previous_tick,
            current_tick,
            mode,
            generation: self.evaluation_generation,
        }
    }
}

fn non_negative_duration(duration: TimelineTick) -> TimelineTick {
    if duration < TimelineTick::ZERO {
        TimelineTick::ZERO
    } else {
        duration
    }
}

/// Incremental forward-only reconstruction cursor for `ReplayRequired` tracks.
///
/// The owning runtime drives this cursor in bounded chunks (for example once
/// per preview frame). A generation mismatch or explicit cancellation ends the
/// work without applying stale state. Every emitted step has a non-negative
/// delta, so callers never reconstruct state by feeding a negative timestep to
/// simulation domains such as VFX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReconstruction {
    generation: u64,
    cursor_tick: TimelineTick,
    target_tick: TimelineTick,
    cancelled: bool,
}

impl ReplayReconstruction {
    /// Creates a reconstruction from `from_tick` through `target_tick`.
    ///
    /// Returns `None` when the requested target precedes the checkpoint/start.
    pub fn new(
        generation: u64,
        from_tick: TimelineTick,
        target_tick: TimelineTick,
    ) -> Option<Self> {
        (target_tick >= from_tick).then_some(Self {
            generation,
            cursor_tick: from_tick,
            target_tick,
            cancelled: false,
        })
    }

    /// Cancels the remaining reconstruction work.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Returns whether the target has been reconstructed.
    pub const fn is_complete(&self) -> bool {
        self.cursor_tick >= self.target_tick
    }

    /// Produces at most `max_ticks` of forward reconstruction work.
    ///
    /// A zero budget simply yields [`ReplayProgress::Pending`]. A generation
    /// mismatch behaves like cancellation so a later seek can invalidate an
    /// older preview job without blocking the UI thread.
    pub fn next_step(&mut self, max_ticks: i64, current_generation: u64) -> ReplayProgress {
        if self.cancelled || current_generation != self.generation {
            self.cancelled = true;
            return ReplayProgress::Cancelled;
        }
        if self.is_complete() {
            return ReplayProgress::Complete;
        }
        if max_ticks <= 0 {
            return ReplayProgress::Pending;
        }
        let from_tick = self.cursor_tick;
        let to_tick = from_tick
            .saturating_add(max_ticks)
            .clamp(from_tick, self.target_tick);
        self.cursor_tick = to_tick;
        ReplayProgress::Step {
            from_tick,
            to_tick,
            complete: self.is_complete(),
        }
    }
}

/// Result of one bounded [`ReplayReconstruction`] pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayProgress {
    /// No work was performed because the supplied budget was zero.
    Pending,
    /// One forward-only interval should be simulated.
    Step {
        /// Inclusive reconstruction cursor before this step.
        from_tick: TimelineTick,
        /// New cursor after this step.
        to_tick: TimelineTick,
        /// Whether this step reached the target.
        complete: bool,
    },
    /// The target was already fully reconstructed.
    Complete,
    /// Work was explicitly cancelled or invalidated by a newer generation.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestPayload {
        State(u8),
        Event(u8),
    }

    #[test]
    fn canonical_clock_is_48khz_and_residual_is_retained() {
        assert_eq!(TIMELINE_TICKS_PER_SECOND, 48_000);
        let mut player = TimelinePlayer::new();
        player.set_playback_rate(0.5).expect("valid rate");
        player.play();
        let duration = TimelineTick::new(100);

        assert!(player.advance_ticks(1, duration).is_none());
        let request = player.advance_ticks(1, duration).expect("second half tick");
        assert_eq!(request.current_tick, TimelineTick::new(1));
    }

    #[test]
    fn compiled_order_is_deterministic() {
        let timeline = CompiledTimeline::new(
            TimelineTick::new(100),
            vec![
                CompiledEntry::interval(
                    1,
                    0,
                    TimelineTick::new(0),
                    TimelineTick::new(100),
                    SeekCapability::Stateless,
                    TestPayload::State(1),
                ),
                CompiledEntry::interval(
                    0,
                    1,
                    TimelineTick::new(10),
                    TimelineTick::new(20),
                    SeekCapability::Stateless,
                    TestPayload::State(2),
                ),
                CompiledEntry::interval(
                    0,
                    0,
                    TimelineTick::new(10),
                    TimelineTick::new(20),
                    SeekCapability::Stateless,
                    TestPayload::State(3),
                ),
            ],
        )
        .expect("valid timeline");

        let payloads: Vec<_> = timeline.entries().iter().map(CompiledEntry::payload).collect();
        assert_eq!(
            payloads,
            vec![
                &TestPayload::State(3),
                &TestPayload::State(2),
                &TestPayload::State(1)
            ]
        );
    }

    #[test]
    fn forward_event_crossing_fires_once_and_scrub_suppresses_by_default() {
        let timeline = CompiledTimeline::new(
            TimelineTick::new(100),
            vec![CompiledEntry::point(
                0,
                0,
                TimelineTick::new(50),
                SeekCapability::Stateless,
                true,
                TestPayload::Event(7),
            )],
        )
        .expect("valid timeline");

        let playback = EvaluationRequest {
            previous_tick: TimelineTick::new(40),
            current_tick: TimelineTick::new(60),
            mode: EvaluationMode::Playback,
            generation: 1,
        };
        assert_eq!(timeline.evaluate(&playback).len(), 1);

        let after = EvaluationRequest {
            previous_tick: TimelineTick::new(60),
            current_tick: TimelineTick::new(70),
            mode: EvaluationMode::Playback,
            generation: 2,
        };
        assert!(timeline.evaluate(&after).is_empty());

        let scrub = EvaluationRequest {
            previous_tick: TimelineTick::new(0),
            current_tick: TimelineTick::new(50),
            mode: EvaluationMode::Seek {
                preview_events: false,
            },
            generation: 3,
        };
        assert!(timeline.evaluate(&scrub).is_empty());

        let preview = EvaluationRequest {
            mode: EvaluationMode::Seek {
                preview_events: true,
            },
            ..scrub
        };
        assert_eq!(timeline.evaluate(&preview).len(), 1);
    }

    #[test]
    fn seek_capabilities_are_explicit() {
        let timeline = CompiledTimeline::new(
            TimelineTick::new(100),
            vec![
                CompiledEntry::interval(
                    0,
                    0,
                    TimelineTick::ZERO,
                    TimelineTick::new(100),
                    SeekCapability::ReplayRequired,
                    TestPayload::State(1),
                ),
                CompiledEntry::interval(
                    1,
                    0,
                    TimelineTick::ZERO,
                    TimelineTick::new(100),
                    SeekCapability::NonSeekable,
                    TestPayload::State(2),
                ),
            ],
        )
        .expect("valid timeline");
        let request = EvaluationRequest {
            previous_tick: TimelineTick::ZERO,
            current_tick: TimelineTick::new(10),
            mode: EvaluationMode::Seek {
                preview_events: false,
            },
            generation: 1,
        };
        let items = timeline.evaluate(&request);
        assert_eq!(
            items[0].decision(),
            EvaluationDecision::ReplayRequired {
                from_tick: TimelineTick::ZERO
            }
        );
        assert_eq!(items[1].decision(), EvaluationDecision::NonSeekable);
    }

    #[test]
    fn players_sharing_one_compiled_timeline_do_not_share_state() {
        let compiled = CompiledTimeline::<TestPayload>::new(TimelineTick::new(100), Vec::new())
            .expect("valid timeline");
        let mut first = TimelinePlayer::new();
        let second = TimelinePlayer::new();
        first.play();
        first.advance_ticks(10, compiled.duration());

        assert_eq!(first.current_tick(), TimelineTick::new(10));
        assert_eq!(second.current_tick(), TimelineTick::ZERO);
        assert_eq!(second.state(), TimelinePlaybackState::Stopped);
    }

    #[test]
    fn stop_and_seek_advance_generation_and_clamp() {
        let mut player = TimelinePlayer::new();
        player.play();
        player.advance_ticks(20, TimelineTick::new(100));
        let seek = player.seek(TimelineTick::new(500), TimelineTick::new(100), false);
        assert_eq!(seek.current_tick, TimelineTick::new(100));
        let generation = seek.generation;
        let stop = player.stop();
        assert_eq!(stop.current_tick, TimelineTick::ZERO);
        assert!(stop.generation > generation);
    }

    #[test]
    fn looping_segments_fire_boundary_markers_once_per_wrap() {
        let timeline = CompiledTimeline::new(
            TimelineTick::new(10),
            vec![
                CompiledEntry::point(
                    0,
                    0,
                    TimelineTick::ZERO,
                    SeekCapability::Stateless,
                    true,
                    TestPayload::Event(0),
                ),
                CompiledEntry::point(
                    0,
                    1,
                    TimelineTick::new(10),
                    SeekCapability::Stateless,
                    true,
                    TestPayload::Event(10),
                ),
            ],
        )
        .expect("valid timeline");
        let mut player = TimelinePlayer::new();
        player.seek(TimelineTick::new(8), timeline.duration(), false);
        player.set_looping(true);
        player.play();

        let requests = player.advance_ticks_segmented(5, timeline.duration());
        assert_eq!(player.current_tick(), TimelineTick::new(3));
        let fired = requests
            .iter()
            .flat_map(|request| timeline.evaluate(request))
            .map(|item| *item.entry().payload())
            .collect::<Vec<_>>();
        assert_eq!(fired, vec![TestPayload::Event(10), TestPayload::Event(0)]);
    }

    #[test]
    fn replay_reconstruction_is_forward_bounded_and_cancellable() {
        let mut replay = ReplayReconstruction::new(7, TimelineTick::new(10), TimelineTick::new(25))
            .expect("forward replay");
        assert_eq!(
            replay.next_step(6, 7),
            ReplayProgress::Step {
                from_tick: TimelineTick::new(10),
                to_tick: TimelineTick::new(16),
                complete: false,
            }
        );
        assert_eq!(
            replay.next_step(100, 7),
            ReplayProgress::Step {
                from_tick: TimelineTick::new(16),
                to_tick: TimelineTick::new(25),
                complete: true,
            }
        );
        assert_eq!(replay.next_step(1, 7), ReplayProgress::Complete);

        let mut stale = ReplayReconstruction::new(9, TimelineTick::ZERO, TimelineTick::new(5))
            .expect("forward replay");
        assert_eq!(stale.next_step(5, 10), ReplayProgress::Cancelled);
        assert!(ReplayReconstruction::new(1, TimelineTick::new(5), TimelineTick::new(4)).is_none());
    }
}
