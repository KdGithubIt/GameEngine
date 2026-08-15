/// Default interval between fixed-timestep updates, in seconds (60 Hz).
pub const FIXED_DELTA_SECONDS: f32 = 1.0 / 60.0;

/// Maximum number of fixed-update steps dispatched in a single frame.
///
/// Prevents a "spiral of death" after a long frame stall: without this cap an
/// unusually slow frame would cause many fixed steps, each of which costs time,
/// making the next frame slower still.
pub const MAX_FIXED_STEPS_PER_FRAME: u32 = 5;

/// Maximum delta time fed into one frame advance, in seconds.
///
/// Frames ride a repaint loop, so a stalled, dragged, or minimized window
/// would otherwise resume with one huge delta and teleport everything that
/// integrates over time.
pub const MAX_DELTA_SECONDS: f32 = 0.1;

/// Stores frame timing information as an ECS resource.
#[derive(Debug, Clone)]
pub struct Time {
    /// The elapsed time since the previous frame, in seconds.
    pub delta_seconds: f32,
    /// The elapsed time since application start, in seconds.
    pub elapsed_seconds: f32,
    /// Number of frames that have advanced since application start.
    pub frame_count: u64,
}

impl Time {
    /// Advances the clock by one frame of `delta_seconds`.
    ///
    /// The delta is clamped to [`MAX_DELTA_SECONDS`] before it is applied, so
    /// callers can pass the raw wall-clock difference between frames.
    pub fn advance(&mut self, delta_seconds: f32) {
        let delta = delta_seconds.min(MAX_DELTA_SECONDS);
        self.delta_seconds = delta;
        self.elapsed_seconds += delta;
        self.frame_count = self.frame_count.saturating_add(1);
    }
}

impl Default for Time {
    fn default() -> Self {
        Self {
            delta_seconds: 0.0,
            elapsed_seconds: 0.0,
            frame_count: 0,
        }
    }
}

/// Tracks accumulated time for the fixed-timestep update loop.
///
/// Insert this resource and call [`FixedTime::step`] each frame to determine
/// how many fixed-update passes to run. Register systems with
/// [`engine_ecs::App::add_fixed_system`].
#[derive(Debug, Clone)]
pub struct FixedTime {
    /// The time interval between fixed-update steps, in seconds.
    pub fixed_delta: f32,
    /// Time accumulated since the last fixed-update step.
    accumulator: f32,
    /// Total fixed steps dispatched since runtime start.
    pub step_count: u64,
}

impl FixedTime {
    /// Creates a `FixedTime` with the given update interval.
    pub fn with_delta(fixed_delta: f32) -> Self {
        Self {
            fixed_delta,
            accumulator: 0.0,
            step_count: 0,
        }
    }

    /// Adds `delta` to the accumulator and returns the number of fixed steps to run.
    ///
    /// The accumulator is capped to avoid a spiral-of-death after long stalls.
    pub fn step(&mut self, delta: f32) -> u32 {
        let cap = self.fixed_delta * MAX_FIXED_STEPS_PER_FRAME as f32;
        self.accumulator = (self.accumulator + delta).min(cap);
        let steps = (self.accumulator / self.fixed_delta) as u32;
        self.accumulator -= steps as f32 * self.fixed_delta;
        steps
    }

    /// Returns how far presentation has advanced from the previous fixed
    /// sample toward the current one.
    ///
    /// Call this after [`Self::step`] for the current frame. The returned
    /// value is the unconsumed accumulator fraction in `0.0..=1.0`, suitable
    /// for render-time interpolation between two authoritative fixed states.
    /// It never advances simulation state.
    #[must_use]
    pub fn interpolation_alpha(&self) -> f32 {
        if !self.fixed_delta.is_finite() || self.fixed_delta <= f32::EPSILON {
            return 0.0;
        }

        (self.accumulator / self.fixed_delta).clamp(0.0, 1.0)
    }

    /// Marks the start of one fixed schedule pass returned by [`Self::step`].
    ///
    /// Hosts call this immediately before each pass so every project callback
    /// sees the precise current fixed-step index, including catch-up frames.
    pub fn begin_step(&mut self) {
        self.step_count = self.step_count.saturating_add(1);
    }
}

impl Default for FixedTime {
    fn default() -> Self {
        Self::with_delta(FIXED_DELTA_SECONDS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_accumulates_delta_and_frame_count() {
        let mut time = Time::default();

        time.advance(0.016);
        time.advance(0.016);

        assert_eq!(time.delta_seconds, 0.016);
        assert!((time.elapsed_seconds - 0.032).abs() < 1e-6);
        assert_eq!(time.frame_count, 2);
    }

    #[test]
    fn advance_clamps_runaway_delta_after_stall() {
        let mut time = Time::default();

        time.advance(5.0);

        assert_eq!(time.delta_seconds, MAX_DELTA_SECONDS);
        assert_eq!(time.elapsed_seconds, MAX_DELTA_SECONDS);
        assert_eq!(time.frame_count, 1);
    }

    #[test]
    fn fixed_time_step_returns_correct_step_count() {
        let mut ft = FixedTime::with_delta(1.0 / 60.0);
        let steps = ft.step(1.0 / 60.0);
        assert_eq!(steps, 1);
        ft.begin_step();
        assert_eq!(ft.step_count, 1);
    }

    #[test]
    fn fixed_time_step_accumulates_sub_step_remainder() {
        let mut ft = FixedTime::with_delta(1.0 / 60.0);
        // Two half-steps should produce 0 steps each and 1 step together.
        let s0 = ft.step(1.0 / 120.0);
        let s1 = ft.step(1.0 / 120.0);
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        ft.begin_step();
        assert_eq!(ft.step_count, 1);
    }

    #[test]
    fn fixed_time_interpolation_alpha_tracks_the_unconsumed_remainder() {
        let mut ft = FixedTime::with_delta(1.0);

        assert_eq!(ft.step(1.5), 1);
        assert!((ft.interpolation_alpha() - 0.5).abs() < 1.0e-6);
        assert_eq!(ft.step(0.5), 1);
        assert!(ft.interpolation_alpha().abs() < 1.0e-6);
    }

    #[test]
    fn fixed_time_step_caps_stall() {
        let mut ft = FixedTime::with_delta(1.0 / 60.0);
        let steps = ft.step(10.0);
        assert_eq!(steps, MAX_FIXED_STEPS_PER_FRAME);
    }
}
