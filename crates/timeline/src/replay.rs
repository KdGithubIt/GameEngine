//! Forward-only reconstruction support for `ReplayRequired` Timeline tracks.
//!
//! The neutral Timeline crate owns only scheduling policy and transient cache
//! mechanics. A concrete domain adapter supplies the state snapshot and the
//! deterministic forward-step function. This keeps VFX, physics, and other
//! domain types out of the Timeline crate while giving every ReplayRequired
//! adapter the same checkpoint, debounce, and cancellation semantics.

use engine_authoring::TimelineTick;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

/// Default reconstruction quantum: one 60 Hz step at 48,000 Timeline ticks/s.
pub const DEFAULT_REPLAY_STEP_TICKS: i64 = 800;

/// Default distance between transient reconstruction checkpoints.
pub const DEFAULT_REPLAY_CHECKPOINT_INTERVAL_TICKS: i64 = 24_000;

/// Default maximum number of transient checkpoints retained per replay state.
pub const DEFAULT_REPLAY_CHECKPOINT_LIMIT: usize = 32;

/// Default quiet period before an Editor scrub request starts reconstruction.
pub const DEFAULT_REPLAY_DEBOUNCE: Duration = Duration::from_millis(50);

/// Invalid configuration for a [`ReplayCheckpointCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayCheckpointConfigError {
    /// Forward reconstruction requires a positive step size.
    NonPositiveStep,
    /// Checkpoint spacing must be positive.
    NonPositiveCheckpointInterval,
    /// A cache must retain at least one checkpoint.
    ZeroCheckpointLimit,
}

impl fmt::Display for ReplayCheckpointConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveStep => write!(formatter, "replay step ticks must be positive"),
            Self::NonPositiveCheckpointInterval => {
                write!(formatter, "replay checkpoint interval ticks must be positive")
            }
            Self::ZeroCheckpointLimit => {
                write!(formatter, "replay checkpoint limit must be at least one")
            }
        }
    }
}

impl std::error::Error for ReplayCheckpointConfigError {}

/// One generation-tagged reconstruction request.
///
/// A newer request automatically makes every older request stale. Domain
/// workers keep the corresponding [`ReplayCancellationToken`] and check it
/// between deterministic forward steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayRequest {
    generation: u64,
    target: TimelineTick,
}

impl ReplayRequest {
    /// Generation that uniquely identifies this request relative to its controller.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Target Timeline tick requested by the caller.
    pub const fn target(self) -> TimelineTick {
        self.target
    }
}

/// Cheap cooperative cancellation token for an in-flight reconstruction.
///
/// Cloning this token is inexpensive and allows a background worker to observe
/// superseding scrub requests without the Timeline core owning any worker
/// threads or domain state.
#[derive(Debug, Clone, Default)]
pub struct ReplayCancellationToken {
    generation: Arc<AtomicU64>,
}

impl ReplayCancellationToken {
    /// Returns whether `request` is still the newest non-cancelled generation.
    pub fn is_current(&self, request: ReplayRequest) -> bool {
        self.generation.load(Ordering::Acquire) == request.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingReplayRequest {
    request: ReplayRequest,
    requested_at: Duration,
}

/// Debounces Editor scrub requests and invalidates superseded reconstruction.
///
/// The controller does not create threads. The caller supplies a monotonic
/// `Duration` so tests and Editor clocks can drive debounce deterministically,
/// then runs the returned request on its own worker or scheduling mechanism.
#[derive(Debug)]
pub struct ReplayRequestController {
    debounce: Duration,
    cancellation: ReplayCancellationToken,
    pending: Option<PendingReplayRequest>,
}

impl Default for ReplayRequestController {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_DEBOUNCE)
    }
}

impl ReplayRequestController {
    /// Creates a controller with the requested debounce quiet period.
    ///
    /// Use a zero duration for gameplay seeks that must reconstruct immediately.
    pub fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            cancellation: ReplayCancellationToken::default(),
            pending: None,
        }
    }

    /// Queues a target, replacing any older pending request.
    ///
    /// Replacing the target also invalidates an older request that may already
    /// be reconstructing on another thread.
    pub fn request(&mut self, target: TimelineTick, now: Duration) -> ReplayRequest {
        let generation = self
            .cancellation
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let request = ReplayRequest { generation, target };
        self.pending = Some(PendingReplayRequest {
            request,
            requested_at: now,
        });
        request
    }

    /// Cancels both a pending request and any in-flight request from this controller.
    pub fn cancel(&mut self) {
        self.cancellation.generation.fetch_add(1, Ordering::AcqRel);
        self.pending = None;
    }

    /// Returns a token an in-flight worker can use for cooperative cancellation.
    pub fn cancellation_token(&self) -> ReplayCancellationToken {
        self.cancellation.clone()
    }

    /// Takes the newest request once the debounce quiet period has elapsed.
    pub fn take_ready(&mut self, now: Duration) -> Option<ReplayRequest> {
        let pending = self.pending?;
        if now.saturating_sub(pending.requested_at) < self.debounce {
            return None;
        }
        self.pending = None;
        self.cancellation.is_current(pending.request).then_some(pending.request)
    }

    /// Whether a request is currently waiting for its debounce period.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// Result of reconstructing one ReplayRequired domain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayReconstruction<S> {
    /// Reconstruction reached the requested target using forward-only steps.
    Completed {
        /// Reconstructed domain state at the target tick.
        state: S,
        /// Checkpoint tick reconstruction started from.
        source_tick: TimelineTick,
        /// Requested target tick after non-negative clamping.
        target_tick: TimelineTick,
        /// Number of Timeline ticks simulated after restoring the checkpoint.
        simulated_ticks: i64,
    },
    /// A newer request or explicit cancellation interrupted reconstruction.
    Cancelled {
        /// Last fully simulated tick before cancellation was observed.
        reached_tick: TimelineTick,
    },
}

impl<S> ReplayReconstruction<S> {
    /// Consumes a completed result and returns its reconstructed state.
    pub fn into_completed_state(self) -> Option<S> {
        match self {
            Self::Completed { state, .. } => Some(state),
            Self::Cancelled { .. } => None,
        }
    }
}

/// Bounded transient checkpoints for one ReplayRequired adapter state.
///
/// `S` is owned by the concrete domain adapter. The cache never serializes it
/// and never interprets its contents. Callers must clear the cache when the
/// compiled Timeline, binding, seed, or any other input that changes replay
/// semantics is invalidated.
#[derive(Debug, Clone)]
pub struct ReplayCheckpointCache<S> {
    checkpoints: BTreeMap<TimelineTick, S>,
    step_ticks: i64,
    checkpoint_interval_ticks: i64,
    checkpoint_limit: usize,
}

impl<S> Default for ReplayCheckpointCache<S> {
    fn default() -> Self {
        Self {
            checkpoints: BTreeMap::new(),
            step_ticks: DEFAULT_REPLAY_STEP_TICKS,
            checkpoint_interval_ticks: DEFAULT_REPLAY_CHECKPOINT_INTERVAL_TICKS,
            checkpoint_limit: DEFAULT_REPLAY_CHECKPOINT_LIMIT,
        }
    }
}

impl<S> ReplayCheckpointCache<S> {
    /// Creates a cache with explicit deterministic step and retention policy.
    pub fn new(
        step_ticks: i64,
        checkpoint_interval_ticks: i64,
        checkpoint_limit: usize,
    ) -> Result<Self, ReplayCheckpointConfigError> {
        if step_ticks <= 0 {
            return Err(ReplayCheckpointConfigError::NonPositiveStep);
        }
        if checkpoint_interval_ticks <= 0 {
            return Err(ReplayCheckpointConfigError::NonPositiveCheckpointInterval);
        }
        if checkpoint_limit == 0 {
            return Err(ReplayCheckpointConfigError::ZeroCheckpointLimit);
        }
        Ok(Self {
            checkpoints: BTreeMap::new(),
            step_ticks,
            checkpoint_interval_ticks,
            checkpoint_limit,
        })
    }

    /// Drops every transient checkpoint after a semantic invalidation.
    pub fn clear(&mut self) {
        self.checkpoints.clear();
    }

    /// Number of transient checkpoints currently retained.
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Whether no transient checkpoint is currently retained.
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// Fixed forward simulation quantum in Timeline ticks.
    pub const fn step_ticks(&self) -> i64 {
        self.step_ticks
    }

    /// Distance between automatically captured reconstruction checkpoints.
    pub const fn checkpoint_interval_ticks(&self) -> i64 {
        self.checkpoint_interval_ticks
    }

    /// Returns whether `tick` lies on an automatic checkpoint boundary.
    pub fn is_checkpoint_tick(&self, tick: TimelineTick) -> bool {
        tick >= TimelineTick::ZERO && tick.get() % self.checkpoint_interval_ticks == 0
    }

    /// Records a checkpoint captured by ordinary forward playback.
    ///
    /// This lets a later seek start from the real forward-playback state
    /// rather than replaying from zero. Negative ticks are rejected.
    pub fn record_checkpoint(&mut self, tick: TimelineTick, state: S) -> bool {
        if tick < TimelineTick::ZERO {
            return false;
        }
        self.checkpoints.insert(tick, state);
        self.prune();
        true
    }

    /// Removes checkpoints after `tick`, preserving earlier valid history.
    ///
    /// Adapters use this after resuming forward playback from an earlier seek,
    /// because checkpoints from the abandoned future no longer belong to the
    /// active state history.
    pub fn invalidate_after(&mut self, tick: TimelineTick) {
        self.checkpoints.retain(|checkpoint, _| *checkpoint <= tick);
    }

    /// Nearest retained checkpoint at or before `target`.
    pub fn checkpoint_tick_at_or_before(&self, target: TimelineTick) -> Option<TimelineTick> {
        self.checkpoints.range(..=target).next_back().map(|(tick, _)| *tick)
    }

    fn prune(&mut self) {
        while self.checkpoints.len() > self.checkpoint_limit {
            let removable = self
                .checkpoints
                .keys()
                .copied()
                .find(|tick| *tick != TimelineTick::ZERO)
                .or_else(|| self.checkpoints.keys().next().copied());
            let Some(removable) = removable else {
                break;
            };
            self.checkpoints.remove(&removable);
        }
    }
}

impl<S: Clone> ReplayCheckpointCache<S> {
    /// Reconstructs `target` from the nearest checkpoint using only forward steps.
    ///
    /// `start_state` is the exact adapter state at tick zero and is used when no
    /// checkpoint exists before the target. `advance` must deterministically
    /// advance `state` from `from` to `to`; the core guarantees `to > from`.
    /// `is_cancelled` is checked between steps. Checkpoints produced by a
    /// cancelled reconstruction are not committed to the cache.
    pub fn reconstruct<Advance, IsCancelled>(
        &mut self,
        target: TimelineTick,
        start_state: &S,
        mut advance: Advance,
        mut is_cancelled: IsCancelled,
    ) -> ReplayReconstruction<S>
    where
        Advance: FnMut(&mut S, TimelineTick, TimelineTick),
        IsCancelled: FnMut() -> bool,
    {
        let target = target.max(TimelineTick::ZERO);
        let (source_tick, mut state) = self
            .checkpoints
            .range(..=target)
            .next_back()
            .map(|(tick, state)| (*tick, state.clone()))
            .unwrap_or_else(|| (TimelineTick::ZERO, start_state.clone()));

        let mut current = source_tick;
        let mut staged = Vec::new();

        while current < target {
            if is_cancelled() {
                return ReplayReconstruction::Cancelled {
                    reached_tick: current,
                };
            }

            let step_end = current.saturating_add(TimelineTick(self.step_ticks));
            let checkpoint_end = next_checkpoint_after(current, self.checkpoint_interval_ticks);
            let mut next = step_end.min(checkpoint_end).min(target);
            if next <= current {
                next = target;
            }

            advance(&mut state, current, next);
            current = next;

            if self.is_checkpoint_tick(current) {
                staged.push((current, state.clone()));
            }
        }

        if is_cancelled() {
            return ReplayReconstruction::Cancelled {
                reached_tick: current,
            };
        }

        self.checkpoints.entry(TimelineTick::ZERO).or_insert_with(|| start_state.clone());
        for (tick, state) in staged {
            self.checkpoints.insert(tick, state);
        }
        self.prune();

        ReplayReconstruction::Completed {
            state,
            source_tick,
            target_tick: target,
            simulated_ticks: target.get().saturating_sub(source_tick.get()),
        }
    }
}

fn next_checkpoint_after(current: TimelineTick, interval: i64) -> TimelineTick {
    let quotient = current.get().div_euclid(interval);
    let next = quotient.saturating_add(1).saturating_mul(interval);
    TimelineTick(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruction_matches_uninterrupted_forward_simulation() {
        let mut cache = ReplayCheckpointCache::<i64>::default();
        let start = 7_i64;
        let target = TimelineTick(73_123);

        let result = cache.reconstruct(
            target,
            &start,
            |state, from, to| *state += to.get() - from.get(),
            || false,
        );

        assert_eq!(
            result,
            ReplayReconstruction::Completed {
                state: start + target.get(),
                source_tick: TimelineTick::ZERO,
                target_tick: target,
                simulated_ticks: target.get(),
            }
        );
        assert!(cache.len() > 1);
    }

    #[test]
    fn reconstruction_uses_the_nearest_forward_playback_checkpoint() {
        let mut cache = ReplayCheckpointCache::<i64>::default();
        assert!(cache.record_checkpoint(TimelineTick::ZERO, 0));
        assert!(cache.record_checkpoint(TimelineTick(48_000), 48_000));

        let result = cache.reconstruct(
            TimelineTick(60_000),
            &0,
            |state, from, to| *state += to.get() - from.get(),
            || false,
        );

        assert_eq!(
            result,
            ReplayReconstruction::Completed {
                state: 60_000,
                source_tick: TimelineTick(48_000),
                target_tick: TimelineTick(60_000),
                simulated_ticks: 12_000,
            }
        );
    }

    #[test]
    fn cancelled_reconstruction_does_not_publish_partial_checkpoints() {
        let mut cache = ReplayCheckpointCache::<i64>::default();
        let mut checks = 0_usize;

        let result = cache.reconstruct(
            TimelineTick(96_000),
            &0,
            |state, from, to| *state += to.get() - from.get(),
            || {
                checks += 1;
                checks > 35
            },
        );

        assert!(matches!(
            result,
            ReplayReconstruction::Cancelled { reached_tick } if reached_tick > TimelineTick::ZERO
        ));
        assert!(cache.is_empty());
    }

    #[test]
    fn scrub_debounce_returns_only_the_latest_request_and_cancels_the_old_generation() {
        let mut controller = ReplayRequestController::new(Duration::from_millis(50));
        let old = controller.request(TimelineTick(1_000), Duration::from_millis(0));
        let token = controller.cancellation_token();

        assert!(controller.take_ready(Duration::from_millis(40)).is_none());

        let newest = controller.request(TimelineTick(2_000), Duration::from_millis(40));
        assert!(!token.is_current(old));
        assert!(token.is_current(newest));
        assert!(controller.take_ready(Duration::from_millis(89)).is_none());
        assert_eq!(controller.take_ready(Duration::from_millis(90)), Some(newest));
        assert!(!controller.has_pending());
    }

    #[test]
    fn explicit_cancel_invalidates_an_in_flight_request() {
        let mut controller = ReplayRequestController::new(Duration::from_millis(0));
        let request = controller.request(TimelineTick(1_000), Duration::from_millis(0));
        let token = controller.cancellation_token();
        assert_eq!(controller.take_ready(Duration::from_millis(0)), Some(request));
        assert!(token.is_current(request));

        controller.cancel();

        assert!(!token.is_current(request));
        assert!(!controller.has_pending());
    }

    #[test]
    fn invalid_configuration_is_rejected_instead_of_silently_clamped() {
        assert_eq!(
            ReplayCheckpointCache::<()>::new(0, 1, 1).expect_err("zero step"),
            ReplayCheckpointConfigError::NonPositiveStep
        );
        assert_eq!(
            ReplayCheckpointCache::<()>::new(1, 0, 1).expect_err("zero interval"),
            ReplayCheckpointConfigError::NonPositiveCheckpointInterval
        );
        assert_eq!(
            ReplayCheckpointCache::<()>::new(1, 1, 0).expect_err("zero limit"),
            ReplayCheckpointConfigError::ZeroCheckpointLimit
        );
    }
}
