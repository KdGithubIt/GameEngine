//! Gamepad input via gilrs (desktop platforms only).
//!
//! On WASM, the Web Gamepad API binding is used instead (Phase 43-C).
//! All events are converted to [`InputCommand`] entries and pushed into the
//! [`VirtualInputQueue`], keeping the game layer platform-agnostic.

#[cfg(not(target_arch = "wasm32"))]
use crate::input::{
    GamepadAxis, GamepadButton, GamepadId, InputCommand, InputSource, VirtualInputQueue,
};

#[cfg(not(target_arch = "wasm32"))]
fn map_button(button: gilrs::Button) -> Option<GamepadButton> {
    Some(match button {
        gilrs::Button::South => GamepadButton::South,
        gilrs::Button::East => GamepadButton::East,
        gilrs::Button::West => GamepadButton::West,
        gilrs::Button::North => GamepadButton::North,
        gilrs::Button::LeftTrigger => GamepadButton::LeftShoulder,
        gilrs::Button::RightTrigger => GamepadButton::RightShoulder,
        gilrs::Button::Select => GamepadButton::Select,
        gilrs::Button::Start => GamepadButton::Start,
        _ => return None,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn map_axis(axis: gilrs::Axis) -> Option<GamepadAxis> {
    Some(match axis {
        gilrs::Axis::LeftStickX => GamepadAxis::LeftStickX,
        gilrs::Axis::LeftStickY => GamepadAxis::LeftStickY,
        gilrs::Axis::RightStickX => GamepadAxis::RightStickX,
        gilrs::Axis::RightStickY => GamepadAxis::RightStickY,
        gilrs::Axis::LeftZ => GamepadAxis::LeftTrigger,
        gilrs::Axis::RightZ => GamepadAxis::RightTrigger,
        _ => return None,
    })
}

/// Wraps a gilrs context and translates raw OS events into [`VirtualInputQueue`] entries.
///
/// Only available on desktop targets.
#[cfg(not(target_arch = "wasm32"))]
pub struct GilrsContext {
    gilrs: gilrs::Gilrs,
}

#[cfg(not(target_arch = "wasm32"))]
impl GilrsContext {
    /// Tries to initialise gilrs. Returns `None` if the platform does not support it.
    ///
    /// Having no gamepads connected is not a failure — this returns `None` only when
    /// the gilrs backend itself cannot start (unsupported OS configuration).
    pub fn try_new() -> Option<Self> {
        match gilrs::Gilrs::new() {
            Ok(gilrs) => Some(Self { gilrs }),
            Err(e) => {
                log::warn!("gamepad: gilrs init failed ({e}), controller input disabled");
                None
            }
        }
    }

    /// Drains pending OS gamepad events and pushes converted entries to `queue`.
    ///
    /// Raw normalized axes are forwarded unchanged; project action bindings
    /// own deadzone policy so virtual input and physical devices agree.
    pub fn poll(&mut self, queue: &mut VirtualInputQueue) {
        use gilrs::EventType;

        while let Some(event) = self.gilrs.next_event() {
            let raw_id: usize = event.id.into();
            let gamepad_id = GamepadId(raw_id as u32);

            match event.event {
                EventType::ButtonPressed(button, _) => {
                    if let Some(btn) = map_button(button) {
                        queue.push(
                            InputSource::Human,
                            InputCommand::GamepadButton {
                                gamepad: gamepad_id,
                                button: btn,
                                pressed: true,
                            },
                        );
                    }
                }
                EventType::ButtonReleased(button, _) => {
                    if let Some(btn) = map_button(button) {
                        queue.push(
                            InputSource::Human,
                            InputCommand::GamepadButton {
                                gamepad: gamepad_id,
                                button: btn,
                                pressed: false,
                            },
                        );
                    }
                }
                EventType::AxisChanged(axis, value, _) => {
                    if let Some(ax) = map_axis(axis) {
                        queue.push(
                            InputSource::Human,
                            InputCommand::GamepadAxis {
                                gamepad: gamepad_id,
                                axis: ax,
                                value,
                            },
                        );
                    }
                }
                EventType::Connected => {
                    let name = self.gilrs.gamepad(event.id).name().to_string();
                    log::info!("gamepad.connected — {name}");
                    queue.push(
                        InputSource::Human,
                        InputCommand::GamepadConnected {
                            gamepad: gamepad_id,
                        },
                    );
                }
                EventType::Disconnected => {
                    let name = self.gilrs.gamepad(event.id).name().to_string();
                    log::info!("gamepad.disconnected — {name}");
                    queue.push(
                        InputSource::Human,
                        InputCommand::GamepadDisconnected {
                            gamepad: gamepad_id,
                        },
                    );
                }
                _ => {}
            }
        }
    }
}
