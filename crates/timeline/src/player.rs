//! Per-player transient Timeline state (ADR 0126 §3).
//!
//! A compiled Timeline is immutable and shared. Everything that differs between
//! two entities playing the same Timeline lives here, so two players never leak
//! state into each other.

use crate::CompiledTimeline;
use crate::evaluate::{TimelineEvaluation, evaluate_intervals};
use engine_authoring::{TIMELINE_TICKS_PER_SECOND, TimelineTick};

/// Whether a player is advancing, held, or reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelinePlayState {
    /// The playhead advances with the fixed clock.
    Playing,
    /// The playhead holds its position and keeps its state.
    Paused,
    /// The playhead is at its start and holds no adapter state.
    #[default]
    Stopped,
}

/// An authored loop region and repeat budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    /// Inclusive loop start.
    pub start: TimelineTick,
    /// Exclusive loop end.
    pub end: TimelineTick,
    /// Loop repetitions, or `None` for an unbounded loop.
    pub count: Option<u32>,
}

/// How a seek is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineSeek {
    /// Editor scrubbing: samples visual state and suppresses gameplay events.
    Scrub,
    /// Authoring preview that deliberately opts into event side effects.
    PreviewEvents,
    /// Playback seek performed by gameplay; events fire as they do in playback.
    Playback,
}

/// Transient playback state for one Timeline instance.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePlayer {
    tick: TimelineTick,
    previous_tick: TimelineTick,
    state: TimelinePlayState,
    rate: f32,
    loop_region: Option<LoopRegion>,
    loops_completed: u32,
    generation: u64,
    residual: f64,
}

impl Default for TimelinePlayer {
    fn default() -> Self {
        Self {
            tick: TimelineTick::ZERO,
            previous_tick: TimelineTick::ZERO,
            state: TimelinePlayState::Stopped,
            rate: 1.0,
            loop_region: None,
            loops_completed: 0,
            generation: 0,
            residual: 0.0,
        }
    }
}

impl TimelinePlayer {
    /// Creates a stopped player at tick zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current playhead position.
    pub fn tick(&self) -> TimelineTick {
        self.tick
    }

    /// Playhead position before the most recent advance or seek.
    pub fn previous_tick(&self) -> TimelineTick {
        self.previous_tick
    }

    /// Current play state.
    pub fn state(&self) -> TimelinePlayState {
        self.state
    }

    /// Playback rate multiplier.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Evaluation generation, advanced by every stop, play, or seek.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Completed loop repetitions since the last play or stop.
    pub fn loops_completed(&self) -> u32 {
        self.loops_completed
    }

    /// Active loop region, when one is configured.
    pub fn loop_region(&self) -> Option<LoopRegion> {
        self.loop_region
    }

    /// Sets the playback rate.
    ///
    /// A non-finite or negative rate is rejected rather than silently clamped,
    /// because reverse playback has its own event semantics that this player
    /// does not fabricate.
    pub fn set_rate(&mut self, rate: f32) -> bool {
        if !rate.is_finite() || rate < 0.0 {
            return false;
        }
        self.rate = rate;
        true
    }

    /// Sets or clears the loop region.
    pub fn set_loop_region(&mut self, region: Option<LoopRegion>) -> bool {
        if let Some(region) = region
            && region.end <= region.start
        {
            return false;
        }
        self.loop_region = region;
        self.loops_completed = 0;
        true
    }

    /// Starts or resumes playback.
    pub fn play(&mut self) {
        self.state = TimelinePlayState::Playing;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Holds the playhead without discarding player state.
    pub fn pause(&mut self) {
        self.state = TimelinePlayState::Paused;
    }

    /// Resets the playhead and clears loop progress.
    pub fn stop(&mut self) {
        self.state = TimelinePlayState::Stopped;
        self.previous_tick = self.tick;
        self.tick = TimelineTick::ZERO;
        self.residual = 0.0;
        self.loops_completed = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Moves the playhead to an exact tick and evaluates the target.
    ///
    /// A scrub samples visual state and suppresses gameplay events by default;
    /// `TimelineSeek::PreviewEvents` opts into event preview explicitly.
    pub fn seek(
        &mut self,
        timeline: &CompiledTimeline,
        target: TimelineTick,
        seek: TimelineSeek,
    ) -> TimelineEvaluation {
        let clamped = clamp_tick(target, timeline.duration);
        self.previous_tick = self.tick;
        self.tick = clamped;
        self.residual = 0.0;
        self.generation = self.generation.wrapping_add(1);
        let intervals = match seek {
            // A discontinuous jump crosses no interval: events belong to the
            // traversal a seek deliberately skipped.
            TimelineSeek::Scrub => Vec::new(),
            TimelineSeek::PreviewEvents | TimelineSeek::Playback => {
                vec![(clamped, clamped.saturating_add(TimelineTick(1)))]
            }
        };
        evaluate_intervals(timeline, clamped, &intervals)
    }

    /// Advances the playhead by one fixed-step delta and evaluates the result.
    ///
    /// The fractional remainder of a delta is accumulated rather than rounded
    /// away, so a sequence advanced by many small deltas lands on the same tick
    /// as one advanced by their sum.
    pub fn advance(
        &mut self,
        timeline: &CompiledTimeline,
        delta_seconds: f32,
    ) -> TimelineEvaluation {
        // A stopped player contributes nothing: it holds no adapter state, so a
        // camera cut, a property write, or a cue that a clip would produce at
        // tick zero must not be reported while playback is stopped.
        if self.state == TimelinePlayState::Stopped {
            return TimelineEvaluation {
                tick: self.tick,
                ..TimelineEvaluation::default()
            };
        }
        if self.state != TimelinePlayState::Playing || !delta_seconds.is_finite() {
            return evaluate_intervals(timeline, self.tick, &[]);
        }
        let scaled = f64::from(delta_seconds.max(0.0)) * f64::from(self.rate);
        let exact = scaled * TIMELINE_TICKS_PER_SECOND as f64 + self.residual;
        let whole = exact.trunc();
        self.residual = exact - whole;
        let mut remaining = whole as i64;
        let mut intervals = Vec::new();
        self.previous_tick = self.tick;
        while remaining > 0 {
            let boundary = self.forward_boundary(timeline);
            let available = boundary.get() - self.tick.get();
            if available <= 0 {
                break;
            }
            let step = remaining.min(available);
            let from = self.tick;
            let to = TimelineTick(self.tick.get() + step);
            intervals.push((from, to));
            self.tick = to;
            remaining -= step;
            if self.tick >= boundary && !self.wrap_at_boundary(boundary, timeline) {
                break;
            }
        }
        evaluate_intervals(timeline, self.tick, &intervals)
    }

    /// Next tick where traversal must stop: the loop end or the duration.
    fn forward_boundary(&self, timeline: &CompiledTimeline) -> TimelineTick {
        match self.loop_region {
            Some(region) if self.tick >= region.start && self.loop_budget_remains(region) => {
                region.end.min(timeline.duration)
            }
            _ => timeline.duration,
        }
    }

    fn loop_budget_remains(&self, region: LoopRegion) -> bool {
        region
            .count
            .is_none_or(|count| self.loops_completed < count)
    }

    /// Wraps at a loop end, or holds at the duration when playback finished.
    ///
    /// Returns whether traversal may continue.
    fn wrap_at_boundary(&mut self, boundary: TimelineTick, timeline: &CompiledTimeline) -> bool {
        match self.loop_region {
            Some(region)
                if boundary == region.end.min(timeline.duration)
                    && self.loop_budget_remains(region) =>
            {
                self.loops_completed = self.loops_completed.saturating_add(1);
                if self.loop_budget_remains(region) {
                    self.tick = region.start;
                    return true;
                }
                // The last authored repetition just finished. Playback carries
                // on from the loop end rather than replaying the region once
                // more, so a bounded loop crosses each marker exactly `count`
                // times.
                self.tick < timeline.duration
            }
            _ => {
                self.state = TimelinePlayState::Paused;
                false
            }
        }
    }
}

fn clamp_tick(tick: TimelineTick, duration: TimelineTick) -> TimelineTick {
    if tick.get() < 0 {
        TimelineTick::ZERO
    } else if tick > duration {
        duration
    } else {
        tick
    }
}
