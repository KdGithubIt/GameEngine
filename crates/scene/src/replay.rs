//! Versioned deterministic virtual-input recording shared by the editor and tests.

use engine_ecs::World;
use engine_platform::input::{
    GamepadAxis, GamepadButton, GamepadId, InputCommand, InputSource, MouseButton,
    VirtualInputQueue,
};
use engine_platform::input_names::{parse_key_code_name, parse_mouse_button_name};
use serde::{Deserialize, Serialize};
use std::fmt;

/// First persisted replay schema described by ADR 0064.
pub const REPLAY_FORMAT_VERSION: u32 = 1;

/// Complete replay artifact stored as `*.replay.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputReplay {
    /// Persisted format version.
    pub format_version: u32,
    /// Engine package version that produced the recording.
    pub engine_version: String,
    /// Fixed simulation step used during capture and playback.
    pub fixed_step_seconds: f32,
    /// Ordered input batches keyed by fixed tick.
    pub ticks: Vec<ReplayTick>,
    /// Optional named milestones for tests and debugging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<ReplayCheckpoint>,
    /// Optional authoring-visible state expected after playback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_final_state: Option<String>,
}

/// Input commands consumed before one fixed update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayTick {
    /// Monotonically increasing fixed tick number, starting at zero.
    pub tick: u64,
    /// Commands retained in their original enqueue order.
    pub commands: Vec<ReplayCommand>,
}

/// Named deterministic milestone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCheckpoint {
    /// Tick at which the milestone should be observed.
    pub tick: u64,
    /// Project-defined stable checkpoint name.
    pub name: String,
}

/// Serializable counterpart to the engine virtual input command vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayCommand {
    /// Keyboard transition using the stable `KeyCode` debug spelling.
    Key {
        /// Stable physical-key spelling.
        key: String,
        /// New held state.
        pressed: bool,
    },
    /// Mouse-button transition.
    MouseButton {
        /// Stable mouse-button spelling.
        button: String,
        /// New held state.
        pressed: bool,
    },
    /// Absolute pointer position in render-target pixels.
    Pointer {
        /// Horizontal and vertical render-target position.
        position: [f32; 2],
    },
    /// Relative pointer delta in render-target pixels.
    PointerDelta {
        /// Horizontal and vertical relative motion.
        delta: [f64; 2],
    },
    /// Vertical pointer-wheel amount.
    PointerScroll {
        /// Vertical line-unit amount.
        amount: f32,
    },
    /// Gamepad connection transition.
    GamepadConnection {
        /// Runtime device number.
        gamepad: u32,
        /// Whether the device became connected.
        connected: bool,
    },
    /// Gamepad button transition.
    GamepadButton {
        /// Runtime device number.
        gamepad: u32,
        /// Stable gamepad-button spelling.
        button: String,
        /// New held state.
        pressed: bool,
    },
    /// Gamepad analog-axis value.
    GamepadAxis {
        /// Runtime device number.
        gamepad: u32,
        /// Stable gamepad-axis spelling.
        axis: String,
        /// Normalized analog value.
        value: f32,
    },
    /// Releases all held input at a focus boundary.
    ReleaseAll,
}

/// Replay validation or command-decoding failure.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayError {
    /// File version is non-current and unsupported.
    UnsupportedFormat(u32),
    /// Recorded and running engine major versions differ.
    IncompatibleEngine {
        /// Version stored in the artifact.
        recorded: String,
        /// Version of the current engine crate.
        running: String,
    },
    /// Fixed step was non-finite or non-positive.
    InvalidFixedStep(f32),
    /// Tick numbers were duplicate or descending.
    UnorderedTicks,
    /// A serialized device input spelling is unknown.
    UnknownInput(String),
    /// JSON parsing or serialization failed.
    Json(String),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(version) => {
                write!(formatter, "unsupported replay format {version}")
            }
            Self::IncompatibleEngine { recorded, running } => write!(
                formatter,
                "replay engine {recorded} is incompatible with running engine {running}"
            ),
            Self::InvalidFixedStep(step) => write!(
                formatter,
                "replay fixed step must be finite and positive, found {step}"
            ),
            Self::UnorderedTicks => formatter.write_str("replay ticks must be strictly increasing"),
            Self::UnknownInput(value) => {
                write!(formatter, "replay contains unknown input value `{value}`")
            }
            Self::Json(error) => write!(formatter, "invalid replay JSON: {error}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl InputReplay {
    /// Creates an empty recording for the running engine.
    pub fn new(fixed_step_seconds: f32) -> Result<Self, ReplayError> {
        let replay = Self {
            format_version: REPLAY_FORMAT_VERSION,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            fixed_step_seconds,
            ticks: Vec::new(),
            checkpoints: Vec::new(),
            expected_final_state: None,
        };
        replay.validate()?;
        Ok(replay)
    }

    /// Parses and validates one persisted replay.
    pub fn from_json(json: &str) -> Result<Self, ReplayError> {
        let replay: Self =
            serde_json::from_str(json).map_err(|error| ReplayError::Json(error.to_string()))?;
        replay.validate()?;
        Ok(replay)
    }

    /// Produces stable pretty JSON for source control.
    pub fn to_json(&self) -> Result<String, ReplayError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| ReplayError::Json(error.to_string()))
    }

    /// Validates version compatibility and deterministic ordering.
    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.format_version != REPLAY_FORMAT_VERSION {
            return Err(ReplayError::UnsupportedFormat(self.format_version));
        }
        if !self.fixed_step_seconds.is_finite() || self.fixed_step_seconds <= 0.0 {
            return Err(ReplayError::InvalidFixedStep(self.fixed_step_seconds));
        }
        let running = env!("CARGO_PKG_VERSION");
        if major_version(&self.engine_version) != major_version(running) {
            return Err(ReplayError::IncompatibleEngine {
                recorded: self.engine_version.clone(),
                running: running.to_owned(),
            });
        }
        if self
            .ticks
            .windows(2)
            .any(|pair| pair[0].tick >= pair[1].tick)
        {
            return Err(ReplayError::UnorderedTicks);
        }
        Ok(())
    }
}

/// Mutable recorder that groups commands by their target fixed tick.
pub struct ReplayRecorder {
    replay: InputReplay,
}

impl ReplayRecorder {
    /// Starts an empty recording with the supplied deterministic step.
    pub fn new(fixed_step_seconds: f32) -> Result<Self, ReplayError> {
        Ok(Self {
            replay: InputReplay::new(fixed_step_seconds)?,
        })
    }

    /// Appends one virtual command to `tick`, retaining enqueue order.
    pub fn record(&mut self, tick: u64, command: InputCommand) {
        let command = ReplayCommand::from(command);
        if let Some(batch) = self
            .replay
            .ticks
            .last_mut()
            .filter(|batch| batch.tick == tick)
        {
            batch.commands.push(command);
        } else {
            self.replay.ticks.push(ReplayTick {
                tick,
                commands: vec![command],
            });
        }
    }

    /// Ends capture and returns the serializable artifact.
    pub fn finish(self) -> InputReplay {
        self.replay
    }
}

/// Cursor that injects recorded commands at their fixed tick boundary.
pub struct ReplayPlayer {
    replay: InputReplay,
    cursor: usize,
}

impl ReplayPlayer {
    /// Validates an artifact and creates a playback cursor.
    pub fn new(replay: InputReplay) -> Result<Self, ReplayError> {
        replay.validate()?;
        Ok(Self { replay, cursor: 0 })
    }

    /// Returns the fixed interval the host must use during playback.
    pub fn fixed_step_seconds(&self) -> f32 {
        self.replay.fixed_step_seconds
    }

    /// Returns whether every recorded tick batch was consumed.
    pub fn is_finished(&self) -> bool {
        self.cursor >= self.replay.ticks.len()
    }

    /// Queues commands for exactly `tick` through [`VirtualInputQueue`].
    pub fn inject_tick(&mut self, world: &mut World, tick: u64) -> Result<usize, ReplayError> {
        let Some(batch) = self
            .replay
            .ticks
            .get(self.cursor)
            .filter(|batch| batch.tick == tick)
        else {
            return Ok(0);
        };
        let commands = batch
            .commands
            .iter()
            .map(ReplayCommand::to_input)
            .collect::<Result<Vec<_>, _>>()?;
        let count = commands.len();
        let queue = world
            .get_resource_mut::<VirtualInputQueue>()
            .ok_or_else(|| {
                ReplayError::UnknownInput("VirtualInputQueue resource is missing".to_owned())
            })?;
        for command in commands {
            queue.push(InputSource::Replay, command);
        }
        self.cursor += 1;
        Ok(count)
    }
}

impl From<InputCommand> for ReplayCommand {
    fn from(command: InputCommand) -> Self {
        match command {
            InputCommand::GamepadConnected { gamepad } => Self::GamepadConnection {
                gamepad: gamepad.0,
                connected: true,
            },
            InputCommand::Key { key, pressed } => Self::Key {
                key: format!("{key:?}"),
                pressed,
            },
            InputCommand::MouseButton { button, pressed } => Self::MouseButton {
                button: format!("{button:?}"),
                pressed,
            },
            InputCommand::MouseMove { position } => Self::Pointer {
                position: [position.0, position.1],
            },
            InputCommand::MouseDelta { delta } => Self::PointerDelta {
                delta: [delta.0, delta.1],
            },
            InputCommand::MouseScroll { amount } => Self::PointerScroll { amount },
            InputCommand::GamepadButton {
                gamepad,
                button,
                pressed,
            } => Self::GamepadButton {
                gamepad: gamepad.0,
                button: format!("{button:?}"),
                pressed,
            },
            InputCommand::GamepadAxis {
                gamepad,
                axis,
                value,
            } => Self::GamepadAxis {
                gamepad: gamepad.0,
                axis: format!("{axis:?}"),
                value,
            },
            InputCommand::GamepadDisconnected { gamepad } => Self::GamepadConnection {
                gamepad: gamepad.0,
                connected: false,
            },
            InputCommand::ReleaseAll => Self::ReleaseAll,
        }
    }
}

impl ReplayCommand {
    fn to_input(&self) -> Result<InputCommand, ReplayError> {
        Ok(match self {
            Self::Key { key, pressed } => InputCommand::Key {
                key: parse_key_code_name(key)
                    .ok_or_else(|| ReplayError::UnknownInput(key.clone()))?,
                pressed: *pressed,
            },
            Self::MouseButton { button, pressed } => InputCommand::MouseButton {
                button: parse_mouse_button(button)?,
                pressed: *pressed,
            },
            Self::Pointer { position } => InputCommand::MouseMove {
                position: (position[0], position[1]),
            },
            Self::PointerDelta { delta } => InputCommand::MouseDelta {
                delta: (delta[0], delta[1]),
            },
            Self::PointerScroll { amount } => InputCommand::MouseScroll { amount: *amount },
            Self::GamepadConnection {
                gamepad,
                connected: true,
            } => InputCommand::GamepadConnected {
                gamepad: GamepadId(*gamepad),
            },
            Self::GamepadConnection {
                gamepad,
                connected: false,
            } => InputCommand::GamepadDisconnected {
                gamepad: GamepadId(*gamepad),
            },
            Self::GamepadButton {
                gamepad,
                button,
                pressed,
            } => InputCommand::GamepadButton {
                gamepad: GamepadId(*gamepad),
                button: parse_gamepad_button(button)?,
                pressed: *pressed,
            },
            Self::GamepadAxis {
                gamepad,
                axis,
                value,
            } => InputCommand::GamepadAxis {
                gamepad: GamepadId(*gamepad),
                axis: parse_gamepad_axis(axis)?,
                value: *value,
            },
            Self::ReleaseAll => InputCommand::ReleaseAll,
        })
    }
}

fn major_version(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

fn parse_mouse_button(value: &str) -> Result<MouseButton, ReplayError> {
    parse_mouse_button_name(value).ok_or_else(|| ReplayError::UnknownInput(value.to_owned()))
}

fn parse_gamepad_button(value: &str) -> Result<GamepadButton, ReplayError> {
    Ok(match value {
        "South" => GamepadButton::South,
        "East" => GamepadButton::East,
        "West" => GamepadButton::West,
        "North" => GamepadButton::North,
        "LeftShoulder" => GamepadButton::LeftShoulder,
        "RightShoulder" => GamepadButton::RightShoulder,
        "Select" => GamepadButton::Select,
        "Start" => GamepadButton::Start,
        _ => return Err(ReplayError::UnknownInput(value.to_owned())),
    })
}

fn parse_gamepad_axis(value: &str) -> Result<GamepadAxis, ReplayError> {
    Ok(match value {
        "LeftStickX" => GamepadAxis::LeftStickX,
        "LeftStickY" => GamepadAxis::LeftStickY,
        "RightStickX" => GamepadAxis::RightStickX,
        "RightStickY" => GamepadAxis::RightStickY,
        "LeftTrigger" => GamepadAxis::LeftTrigger,
        "RightTrigger" => GamepadAxis::RightTrigger,
        _ => return Err(ReplayError::UnknownInput(value.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_platform::input::{drain_virtual_input, Input, KeyCode};

    #[test]
    fn replay_round_trip_preserves_ordered_commands() {
        let mut recorder = ReplayRecorder::new(1.0 / 60.0).unwrap();
        recorder.record(
            3,
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            },
        );
        recorder.record(
            3,
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: false,
            },
        );
        let replay = recorder.finish();
        assert_eq!(
            InputReplay::from_json(&replay.to_json().unwrap()).unwrap(),
            replay
        );
    }

    #[test]
    fn missing_current_ticks_field_is_rejected() {
        let json = format!(
            r#"{{"format_version":{REPLAY_FORMAT_VERSION},"engine_version":"{}","fixed_step_seconds":0.016666668}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert!(matches!(InputReplay::from_json(&json), Err(ReplayError::Json(_))));
    }

    #[test]
    fn unordered_ticks_are_rejected() {
        let mut replay = InputReplay::new(1.0 / 60.0).unwrap();
        replay.ticks = vec![
            ReplayTick {
                tick: 2,
                commands: vec![],
            },
            ReplayTick {
                tick: 1,
                commands: vec![],
            },
        ];
        assert_eq!(replay.validate(), Err(ReplayError::UnorderedTicks));
    }

    #[test]
    fn player_injects_through_the_shared_virtual_queue() {
        let mut recorder = ReplayRecorder::new(1.0 / 60.0).unwrap();
        recorder.record(
            0,
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            },
        );
        let mut player = ReplayPlayer::new(recorder.finish()).unwrap();
        let mut world = World::new();
        world.insert_resource(VirtualInputQueue::default());
        world.insert_resource(Input::<KeyCode>::default());

        assert_eq!(player.inject_tick(&mut world, 0).unwrap(), 1);
        drain_virtual_input(&mut world);
        assert!(world
            .get_resource::<Input<KeyCode>>()
            .unwrap()
            .pressed(KeyCode::KeyW));
    }
}
