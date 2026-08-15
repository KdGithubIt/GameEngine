pub use engine_core::input::{
    GamepadAxis, GamepadAxisState, GamepadButton, GamepadConnectionState, GamepadId, Input,
    InputSource, MouseInput,
};
#[cfg(not(feature = "window-input"))]
pub use engine_core::physical_input::{KeyCode, MouseButton};
#[cfg(feature = "window-input")]
pub use winit::event::MouseButton;
#[cfg(feature = "window-input")]
pub use winit::keyboard::KeyCode;

/// A device-independent input event that can be injected into the runtime.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputCommand {
    /// Records that a gamepad became available.
    GamepadConnected {
        /// Runtime identifier reported by the platform adapter.
        gamepad: GamepadId,
    },
    /// Sets a keyboard key pressed or released.
    Key {
        /// Physical keyboard key.
        key: KeyCode,
        /// Whether the key is pressed.
        pressed: bool,
    },
    /// Sets a mouse button pressed or released.
    MouseButton {
        /// Mouse button.
        button: MouseButton,
        /// Whether the button is pressed.
        pressed: bool,
    },
    /// Moves the cursor to a position in render-target physical pixels.
    MouseMove {
        /// Cursor position in the same coordinate space as captured frames.
        position: (f32, f32),
    },
    /// Accumulates relative mouse movement for the current frame.
    MouseDelta {
        /// Relative movement in physical pixels.
        delta: (f64, f64),
    },
    /// Accumulates vertical scroll input for the current frame.
    MouseScroll {
        /// Scroll amount in line units.
        amount: f32,
    },
    /// Sets a gamepad button pressed or released.
    GamepadButton {
        /// Runtime gamepad identifier.
        gamepad: GamepadId,
        /// Gamepad button.
        button: GamepadButton,
        /// Whether the button is pressed.
        pressed: bool,
    },
    /// Sets a gamepad analog axis value.
    GamepadAxis {
        /// Runtime gamepad identifier.
        gamepad: GamepadId,
        /// Gamepad axis.
        axis: GamepadAxis,
        /// Axis value.
        value: f32,
    },
    /// Releases state associated with a disconnected gamepad.
    GamepadDisconnected {
        /// Runtime identifier reported by the platform adapter.
        gamepad: GamepadId,
    },
    /// Releases every held input when its owning surface loses focus.
    ReleaseAll,
}

/// Queue of virtual input commands awaiting application to runtime input state.
#[derive(Default)]
pub struct VirtualInputQueue {
    commands: Vec<(InputSource, InputCommand)>,
}

impl VirtualInputQueue {
    /// Creates an empty virtual input queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one input command tagged with its source.
    pub fn push(&mut self, source: InputSource, command: InputCommand) {
        self.commands.push((source, command));
    }

    /// Returns the number of queued commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Returns `true` when no commands are queued.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn drain(&mut self) -> Vec<(InputSource, InputCommand)> {
        self.commands.drain(..).collect()
    }
}

/// Clears keyboard, mouse-button, and gamepad-button transition flags in a runtime world.
pub fn clear_input_transitions(world: &mut engine_ecs::World) {
    if let Some(keyboard) = world.get_resource_mut::<Input<KeyCode>>() {
        keyboard.clear_transitions();
    }
    if let Some(mouse_buttons) = world.get_resource_mut::<Input<MouseButton>>() {
        mouse_buttons.clear_transitions();
    }
    if let Some(gamepad_buttons) = world.get_resource_mut::<Input<GamepadButton>>() {
        gamepad_buttons.clear_transitions();
    }
    if let Some(gamepad_axes) = world.get_resource_mut::<GamepadAxisState>() {
        gamepad_axes.begin_frame();
    }
}

/// Publishes accumulated mouse delta and scroll values for the next schedule.
pub fn prepare_mouse_frame(world: &mut engine_ecs::World) {
    if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
        mouse.prepare_frame();
    }
}

/// Releases all held controls and discards transient pointer movement.
///
/// Window and embedded Game View adapters call this when they lose focus. The
/// release transitions remain visible for the next schedule so action-based
/// gameplay code can finish holds without leaving movement stuck.
pub fn release_all_input(world: &mut engine_ecs::World) {
    if let Some(keyboard) = world.get_resource_mut::<Input<KeyCode>>() {
        keyboard.release_all();
    }
    if let Some(mouse_buttons) = world.get_resource_mut::<Input<MouseButton>>() {
        mouse_buttons.release_all();
    }
    if let Some(gamepad_buttons) = world.get_resource_mut::<Input<GamepadButton>>() {
        gamepad_buttons.release_all();
    }
    if let Some(gamepad_axes) = world.get_resource_mut::<GamepadAxisState>() {
        gamepad_axes.release_all();
    }
    if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
        mouse.release_transient_state();
    }
}

/// Applies queued virtual input commands to the existing runtime input resources.
pub fn drain_virtual_input(world: &mut engine_ecs::World) {
    let Some(commands) = world
        .get_resource_mut::<VirtualInputQueue>()
        .map(VirtualInputQueue::drain)
    else {
        return;
    };

    for (_source, command) in commands {
        match command {
            InputCommand::Key { key, pressed } => {
                if let Some(input) = world.get_resource_mut::<Input<KeyCode>>() {
                    if pressed {
                        input.press(key);
                    } else {
                        input.release(key);
                    }
                }
            }
            InputCommand::MouseButton { button, pressed } => {
                if let Some(input) = world.get_resource_mut::<Input<MouseButton>>() {
                    if pressed {
                        input.press(button);
                    } else {
                        input.release(button);
                    }
                }
            }
            InputCommand::MouseMove { position } => {
                if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                    mouse.set_position(position.0, position.1);
                }
            }
            InputCommand::MouseDelta { delta } => {
                if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                    mouse.accumulate_delta(delta.0, delta.1);
                }
            }
            InputCommand::MouseScroll { amount } => {
                if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                    mouse.accumulate_scroll(amount);
                }
            }
            InputCommand::GamepadButton {
                gamepad,
                button,
                pressed,
            } => {
                mark_gamepad_connected(world, gamepad);
                if let Some(input) = world.get_resource_mut::<Input<GamepadButton>>() {
                    if pressed {
                        input.press(button);
                    } else {
                        input.release(button);
                    }
                }
            }
            InputCommand::GamepadAxis {
                gamepad,
                axis,
                value,
            } => {
                mark_gamepad_connected(world, gamepad);
                if let Some(axes) = world.get_resource_mut::<GamepadAxisState>() {
                    axes.set(gamepad, axis, value);
                }
            }
            InputCommand::GamepadDisconnected { gamepad } => {
                if let Some(buttons) = world.get_resource_mut::<Input<GamepadButton>>() {
                    // Button state is merged for Editor Ready v1, so releasing all
                    // pads is safer than retaining an uncertain held input.
                    buttons.release_all();
                }
                if let Some(axes) = world.get_resource_mut::<GamepadAxisState>() {
                    axes.disconnect(gamepad);
                }
                if let Some(connections) = world.get_resource_mut::<GamepadConnectionState>() {
                    connections.set_connected(gamepad, false);
                }
            }
            InputCommand::GamepadConnected { gamepad } => mark_gamepad_connected(world, gamepad),
            InputCommand::ReleaseAll => release_all_input(world),
        }
    }
}

fn mark_gamepad_connected(world: &mut engine_ecs::World, gamepad: GamepadId) {
    if let Some(connections) = world.get_resource_mut::<GamepadConnectionState>() {
        connections.set_connected(gamepad, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::World;

    #[test]
    fn virtual_input_drains_to_keyboard_transitions() {
        let mut world = input_world();
        world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist")
            .push(
                InputSource::Test,
                InputCommand::Key {
                    key: KeyCode::KeyW,
                    pressed: true,
                },
            );

        drain_virtual_input(&mut world);

        let keyboard = world
            .get_resource::<Input<KeyCode>>()
            .expect("keyboard input must exist");
        assert!(keyboard.pressed(KeyCode::KeyW));
        assert!(keyboard.just_pressed(KeyCode::KeyW));

        clear_input_transitions(&mut world);
        let keyboard = world
            .get_resource::<Input<KeyCode>>()
            .expect("keyboard input must exist");
        assert!(keyboard.pressed(KeyCode::KeyW));
        assert!(!keyboard.just_pressed(KeyCode::KeyW));
    }

    #[test]
    fn virtual_input_drains_to_mouse_state() {
        let mut world = input_world();
        let queue = world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist");
        queue.push(
            InputSource::Test,
            InputCommand::MouseMove {
                position: (12.0, 24.0),
            },
        );
        queue.push(
            InputSource::Test,
            InputCommand::MouseDelta { delta: (3.0, -2.0) },
        );
        queue.push(InputSource::Test, InputCommand::MouseScroll { amount: 1.0 });

        drain_virtual_input(&mut world);
        prepare_mouse_frame(&mut world);

        let mouse = world
            .get_resource::<MouseInput>()
            .expect("mouse input must exist");
        assert_eq!(mouse.position, (12.0, 24.0));
        assert_eq!(mouse.delta, (3.0, -2.0));
        assert_eq!(mouse.scroll, 1.0);
    }

    #[test]
    fn virtual_input_drains_release_transitions() {
        let mut world = input_world();
        world
            .get_resource_mut::<Input<MouseButton>>()
            .expect("mouse buttons must exist")
            .press(MouseButton::Left);
        clear_input_transitions(&mut world);
        world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist")
            .push(
                InputSource::Test,
                InputCommand::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
            );

        drain_virtual_input(&mut world);

        let mouse_buttons = world
            .get_resource::<Input<MouseButton>>()
            .expect("mouse buttons must exist");
        assert!(!mouse_buttons.pressed(MouseButton::Left));
        assert!(mouse_buttons.just_released(MouseButton::Left));
    }

    #[test]
    fn virtual_input_drains_to_gamepad_state() {
        let mut world = input_world();
        world.insert_resource(Input::<GamepadButton>::default());
        world.insert_resource(GamepadAxisState::default());
        let queue = world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist");
        queue.push(
            InputSource::Test,
            InputCommand::GamepadButton {
                gamepad: GamepadId(0),
                button: GamepadButton::South,
                pressed: true,
            },
        );
        queue.push(
            InputSource::Test,
            InputCommand::GamepadAxis {
                gamepad: GamepadId(0),
                axis: GamepadAxis::LeftStickX,
                value: 0.5,
            },
        );

        drain_virtual_input(&mut world);

        let buttons = world
            .get_resource::<Input<GamepadButton>>()
            .expect("gamepad buttons must exist");
        assert!(buttons.pressed(GamepadButton::South));
        assert!(buttons.just_pressed(GamepadButton::South));
        let axes = world
            .get_resource::<GamepadAxisState>()
            .expect("gamepad axes must exist");
        assert_eq!(axes.get(GamepadId(0), GamepadAxis::LeftStickX), 0.5);
        let connections = world
            .get_resource::<GamepadConnectionState>()
            .expect("connection state must exist");
        assert_eq!(connections.connected().collect::<Vec<_>>(), [GamepadId(0)]);
    }

    #[test]
    fn disconnect_releases_merged_buttons_and_only_the_device_axes() {
        let mut world = input_world();
        world.insert_resource(Input::<GamepadButton>::default());
        world.insert_resource(GamepadAxisState::default());
        let queue = world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist");
        queue.push(
            InputSource::Test,
            InputCommand::GamepadButton {
                gamepad: GamepadId(7),
                button: GamepadButton::South,
                pressed: true,
            },
        );
        queue.push(
            InputSource::Test,
            InputCommand::GamepadAxis {
                gamepad: GamepadId(7),
                axis: GamepadAxis::LeftStickX,
                value: 0.75,
            },
        );
        queue.push(
            InputSource::Test,
            InputCommand::GamepadAxis {
                gamepad: GamepadId(8),
                axis: GamepadAxis::LeftStickY,
                value: -0.5,
            },
        );
        drain_virtual_input(&mut world);
        clear_input_transitions(&mut world);

        world
            .get_resource_mut::<VirtualInputQueue>()
            .expect("queue must exist")
            .push(
                InputSource::Test,
                InputCommand::GamepadDisconnected {
                    gamepad: GamepadId(7),
                },
            );
        drain_virtual_input(&mut world);

        let buttons = world
            .get_resource::<Input<GamepadButton>>()
            .expect("gamepad buttons must exist");
        assert!(!buttons.pressed(GamepadButton::South));
        assert!(buttons.just_released(GamepadButton::South));
        let axes = world
            .get_resource::<GamepadAxisState>()
            .expect("gamepad axes must exist");
        assert_eq!(axes.merged_axis(GamepadAxis::LeftStickX), 0.0);
        assert_eq!(axes.previous_merged_axis(GamepadAxis::LeftStickX), 0.75);
        assert_eq!(axes.merged_axis(GamepadAxis::LeftStickY), -0.5);
    }

    #[test]
    fn release_all_clears_held_controls_and_stale_pointer_motion() {
        let mut world = input_world();
        world.insert_resource(Input::<GamepadButton>::default());
        world.insert_resource(GamepadAxisState::default());
        world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("keyboard")
            .press(KeyCode::KeyW);
        world
            .get_resource_mut::<Input<MouseButton>>()
            .expect("mouse buttons")
            .press(MouseButton::Left);
        let mouse = world.get_resource_mut::<MouseInput>().expect("mouse");
        mouse.accumulate_delta(12.0, -8.0);
        mouse.accumulate_scroll(2.0);

        release_all_input(&mut world);
        prepare_mouse_frame(&mut world);

        let keyboard = world.get_resource::<Input<KeyCode>>().expect("keyboard");
        assert!(!keyboard.pressed(KeyCode::KeyW));
        assert!(keyboard.just_released(KeyCode::KeyW));
        let mouse_buttons = world
            .get_resource::<Input<MouseButton>>()
            .expect("mouse buttons");
        assert!(!mouse_buttons.pressed(MouseButton::Left));
        assert!(mouse_buttons.just_released(MouseButton::Left));
        let mouse = world.get_resource::<MouseInput>().expect("mouse");
        assert_eq!(mouse.delta, (0.0, 0.0));
        assert_eq!(mouse.scroll, 0.0);
    }

    fn input_world() -> World {
        let mut world = World::new();
        world.insert_resource(Input::<KeyCode>::default());
        world.insert_resource(Input::<MouseButton>::default());
        world.insert_resource(MouseInput::default());
        world.insert_resource(VirtualInputQueue::default());
        world.insert_resource(GamepadConnectionState::default());
        world
    }
}
