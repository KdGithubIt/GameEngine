use hashbrown::{HashMap, HashSet};
use std::hash::Hash;

/// Stores held and frame-transition input state for keys or buttons.
pub struct Input<T: Eq + Hash> {
    pressed: HashSet<T>,
    just_pressed: HashSet<T>,
    just_released: HashSet<T>,
}

impl<T: Eq + Hash + Copy> Input<T> {
    /// Creates an empty input state.
    pub fn new() -> Self {
        Self {
            pressed: HashSet::new(),
            just_pressed: HashSet::new(),
            just_released: HashSet::new(),
        }
    }

    /// Returns `true` while `key` is held.
    pub fn pressed(&self, key: T) -> bool {
        self.pressed.contains(&key)
    }

    /// Returns `true` only during the frame in which `key` was pressed.
    pub fn just_pressed(&self, key: T) -> bool {
        self.just_pressed.contains(&key)
    }

    /// Returns `true` only during the frame in which `key` was released.
    pub fn just_released(&self, key: T) -> bool {
        self.just_released.contains(&key)
    }

    /// Iterates currently held values for runtime debugging.
    pub fn pressed_values(&self) -> impl Iterator<Item = T> + '_ {
        self.pressed.iter().copied()
    }

    /// Applies a pressed transition from a platform or virtual-input adapter.
    #[doc(hidden)]
    pub fn press(&mut self, key: T) {
        if self.pressed.insert(key) {
            self.just_pressed.insert(key);
        }
    }

    /// Applies a released transition from a platform or virtual-input adapter.
    #[doc(hidden)]
    pub fn release(&mut self, key: T) {
        if self.pressed.remove(&key) {
            self.just_released.insert(key);
        }
    }

    /// Clears frame-local transition flags while preserving held state.
    #[doc(hidden)]
    pub fn clear_transitions(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
    }

    /// Releases every held value and records the release transitions.
    #[doc(hidden)]
    pub fn release_all(&mut self) {
        self.just_released.extend(self.pressed.drain());
    }
}

impl<T: Eq + Hash + Copy> Default for Input<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Stores cursor position, frame movement, and scroll input.
#[derive(Default)]
pub struct MouseInput {
    /// The cursor position inside the render target, in physical pixels.
    pub position: (f32, f32),
    /// Mouse movement published for the current frame.
    pub delta: (f32, f32),
    /// Scroll amount published for the current frame.
    pub scroll: f32,
    delta_accum: (f64, f64),
    scroll_accum: f32,
}

impl MouseInput {
    /// Accumulates relative pointer motion before the next frame boundary.
    #[doc(hidden)]
    pub fn accumulate_delta(&mut self, dx: f64, dy: f64) {
        self.delta_accum.0 += dx;
        self.delta_accum.1 += dy;
    }

    /// Accumulates vertical scroll before the next frame boundary.
    #[doc(hidden)]
    pub fn accumulate_scroll(&mut self, y: f32) {
        self.scroll_accum += y;
    }

    /// Sets the absolute cursor position reported by the active platform adapter.
    #[doc(hidden)]
    pub fn set_position(&mut self, x: f32, y: f32) {
        self.position = (x, y);
    }

    /// Publishes accumulated pointer motion and starts a new accumulation frame.
    #[doc(hidden)]
    pub fn prepare_frame(&mut self) {
        self.delta = (self.delta_accum.0 as f32, self.delta_accum.1 as f32);
        self.scroll = self.scroll_accum;
        self.delta_accum = (0.0, 0.0);
        self.scroll_accum = 0.0;
    }

    /// Discards held transient motion when the owning surface loses focus.
    #[doc(hidden)]
    pub fn release_transient_state(&mut self) {
        self.delta = (0.0, 0.0);
        self.scroll = 0.0;
        self.delta_accum = (0.0, 0.0);
        self.scroll_accum = 0.0;
    }
}

/// Identifies the origin of a queued virtual input command.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputSource {
    /// Input produced by the platform event stream.
    Human,
    /// Input produced by an external AI or agent bridge.
    AiAgent,
    /// Input read from a replay stream.
    Replay,
    /// Input produced by a test harness.
    Test,
}

/// Stable runtime identifier reserved for a gamepad device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GamepadId(pub u32);

/// Gamepad buttons supported by the engine-owned input layer.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadButton {
    /// South face button.
    South,
    /// East face button.
    East,
    /// West face button.
    West,
    /// North face button.
    North,
    /// Left shoulder button.
    LeftShoulder,
    /// Right shoulder button.
    RightShoulder,
    /// Select or back button.
    Select,
    /// Start or menu button.
    Start,
}

/// Gamepad axes supported by the engine-owned input layer.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GamepadAxis {
    /// Horizontal axis of the left stick.
    LeftStickX,
    /// Vertical axis of the left stick.
    LeftStickY,
    /// Horizontal axis of the right stick.
    RightStickX,
    /// Vertical axis of the right stick.
    RightStickY,
    /// Left analog trigger.
    LeftTrigger,
    /// Right analog trigger.
    RightTrigger,
}

/// Stores analog gamepad axis values by device and axis.
#[derive(Default)]
pub struct GamepadAxisState {
    values: HashMap<(GamepadId, GamepadAxis), f32>,
    previous_values: HashMap<(GamepadId, GamepadAxis), f32>,
}

impl GamepadAxisState {
    /// Creates an empty gamepad axis state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets one axis value for a gamepad.
    pub fn set(&mut self, gamepad: GamepadId, axis: GamepadAxis, value: f32) {
        let value = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.values.insert((gamepad, axis), value);
    }

    /// Returns the last known value for one axis, or zero when absent.
    pub fn get(&self, gamepad: GamepadId, axis: GamepadAxis) -> f32 {
        self.values.get(&(gamepad, axis)).copied().unwrap_or(0.0)
    }

    /// Returns the largest absolute value for `axis` across all gamepads.
    pub fn merged_axis(&self, axis: GamepadAxis) -> f32 {
        merged_axis_from(&self.values, axis)
    }

    /// Returns the merged value captured at the previous frame boundary.
    pub fn previous_merged_axis(&self, axis: GamepadAxis) -> f32 {
        merged_axis_from(&self.previous_values, axis)
    }

    /// Returns non-zero per-device axes for runtime debugging.
    pub fn active_axes(&self) -> impl Iterator<Item = (GamepadId, GamepadAxis, f32)> + '_ {
        self.values.iter().filter_map(|((gamepad, axis), value)| {
            (*value != 0.0).then_some((*gamepad, *axis, *value))
        })
    }

    /// Captures current values as the previous frame's axis state.
    #[doc(hidden)]
    pub fn begin_frame(&mut self) {
        self.previous_values.clone_from(&self.values);
    }

    /// Drops axis state belonging to one disconnected controller.
    #[doc(hidden)]
    pub fn disconnect(&mut self, gamepad: GamepadId) {
        self.values.retain(|(id, _), _| *id != gamepad);
    }

    /// Clears every currently held analog axis.
    #[doc(hidden)]
    pub fn release_all(&mut self) {
        self.values.clear();
    }
}

/// Latest known controller connection state and change generation.
#[derive(Debug, Clone, Default)]
pub struct GamepadConnectionState {
    connected: HashSet<GamepadId>,
    generation: u64,
    last_change: Option<(GamepadId, bool)>,
}

impl GamepadConnectionState {
    /// Iterates connected runtime controller IDs.
    pub fn connected(&self) -> impl Iterator<Item = GamepadId> + '_ {
        self.connected.iter().copied()
    }

    /// Returns the monotonic generation incremented by each connection change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the latest `(device, connected)` transition.
    pub fn last_change(&self) -> Option<(GamepadId, bool)> {
        self.last_change
    }

    /// Applies one platform-observed controller connection state.
    #[doc(hidden)]
    pub fn set_connected(&mut self, gamepad: GamepadId, connected: bool) {
        let changed = if connected {
            self.connected.insert(gamepad)
        } else {
            self.connected.remove(&gamepad)
        };
        if changed {
            self.generation = self.generation.saturating_add(1);
            self.last_change = Some((gamepad, connected));
        }
    }
}

fn merged_axis_from(values: &HashMap<(GamepadId, GamepadAxis), f32>, axis: GamepadAxis) -> f32 {
    values
        .iter()
        .filter_map(|((_, candidate_axis), value)| (*candidate_axis == axis).then_some(*value))
        .max_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_remain_visible_until_cleared() {
        let mut input = Input::<u8>::new();
        input.press(1);
        assert!(input.pressed(1));
        assert!(input.just_pressed(1));
        input.clear_transitions();
        assert!(input.pressed(1));
        assert!(!input.just_pressed(1));
        input.release(1);
        assert!(!input.pressed(1));
        assert!(input.just_released(1));
    }

    #[test]
    fn mouse_accumulation_is_published_once_per_frame() {
        let mut input = MouseInput::default();
        input.accumulate_delta(2.0, -3.0);
        input.accumulate_scroll(1.5);
        input.prepare_frame();
        assert_eq!(input.delta, (2.0, -3.0));
        assert_eq!(input.scroll, 1.5);
        input.prepare_frame();
        assert_eq!(input.delta, (0.0, 0.0));
        assert_eq!(input.scroll, 0.0);
    }

    #[test]
    fn non_finite_axis_input_is_sanitized() {
        let mut axes = GamepadAxisState::new();
        axes.set(GamepadId(0), GamepadAxis::LeftStickX, f32::NAN);
        axes.set(GamepadId(0), GamepadAxis::LeftStickY, f32::INFINITY);
        assert_eq!(axes.merged_axis(GamepadAxis::LeftStickX), 0.0);
        assert_eq!(axes.merged_axis(GamepadAxis::LeftStickY), 0.0);
    }
}
