//! Player controller component and system.

use crate::camera::{select_active_game_camera, Camera3D};
use crate::character_controller::KinematicCharacterController;
use crate::game_io::GameInputActionState;
use crate::input::{GamepadAxis, GamepadAxisState, GamepadButton, Input, KeyCode, MouseButton};
use crate::time::FixedTime;
use crate::transform::{GlobalTransform, Transform};
use engine_authoring::{InputAction, ProjectSettings};
use engine_ecs::World;
use glam::{Quat, Vec2, Vec3};
use std::collections::HashMap;

pub use engine_gameplay::player::{
    MovePlane, PlayerController, PlayerMarker, PlayerMovementIntent,
    PlayerMovementIntents,
};

/// Maps logical action names to [`KeyCode`] values (ADR 0031 / Phase 34).
///
/// Insert this resource into the ECS world to override the default
/// WASD key bindings used by [`player_controller_system`].  When absent,
/// the system falls back to hardcoded WASD so that existing examples
/// continue to work without a `project_settings.json` file.
///
/// Build manually via [`InputActionMap::insert`] or start with the empty map.
#[derive(Debug, Clone, Default)]
pub struct InputActionMap {
    actions: HashMap<String, RuntimeInputAction>,
}

#[derive(Debug, Clone, Default)]
struct RuntimeInputAction {
    keys: Vec<KeyCode>,
    mouse_buttons: Vec<MouseButton>,
    gamepad_buttons: Vec<GamepadButton>,
    gamepad_axes: Vec<RuntimeGamepadAxisBinding>,
    key_axes: Vec<RuntimeKeyAxisBinding>,
}

#[derive(Debug, Clone)]
struct RuntimeGamepadAxisBinding {
    axis: GamepadAxis,
    deadzone: f32,
    scale: f32,
    invert: bool,
}

#[derive(Debug, Clone)]
struct RuntimeKeyAxisBinding {
    vector_component: usize,
    negative_keys: Vec<KeyCode>,
    positive_keys: Vec<KeyCode>,
    scale: f32,
}

/// Non-blocking diagnostic produced while compiling project input bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputActionMapDiagnostic {
    /// Logical action containing the ignored binding.
    pub action: String,
    /// Human-readable reason the binding could not be used.
    pub message: String,
}

impl InputActionMap {
    /// Creates an empty action map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a mapping from `action_name` to `keys`.
    pub fn insert(&mut self, action_name: impl Into<String>, keys: Vec<KeyCode>) {
        self.actions.insert(
            action_name.into(),
            RuntimeInputAction {
                keys,
                ..RuntimeInputAction::default()
            },
        );
    }

    /// Returns whether the project declared `action_name`.
    pub fn contains_action(&self, action_name: &str) -> bool {
        self.actions.contains_key(action_name)
    }

    /// Compiles one runtime map from the canonical project settings document.
    ///
    /// Unknown physical binding names are ignored individually and returned as
    /// diagnostics; valid bindings in the same action remain usable.
    pub fn from_project_settings(
        settings: &ProjectSettings,
    ) -> (Self, Vec<InputActionMapDiagnostic>) {
        let mut map = Self::new();
        let mut diagnostics = Vec::new();
        for action in &settings.input_actions {
            if map.actions.contains_key(&action.name) {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: "duplicate action name was ignored after its first definition"
                        .to_owned(),
                });
                continue;
            }
            let compiled = compile_input_action(action, &mut diagnostics);
            map.actions.insert(action.name.clone(), compiled);
        }
        (map, diagnostics)
    }

    /// Returns `true` when any key in the action's binding is currently pressed.
    ///
    /// Returns `false` when the action is not registered or has no bindings.
    pub fn action_pressed(&self, action: &str, keyboard: &Input<KeyCode>) -> bool {
        self.actions
            .get(action)
            .map(|binding| resolve_runtime_input(binding, Some(keyboard), None, None, None).pressed)
            .unwrap_or(false)
    }

    /// Returns whether any configured keyboard, gamepad button, or analog axis
    /// source currently activates the action.
    pub fn action_pressed_with_devices(
        &self,
        action: &str,
        keyboard: &Input<KeyCode>,
        buttons: Option<&Input<GamepadButton>>,
        axes: Option<&GamepadAxisState>,
    ) -> bool {
        self.actions
            .get(action)
            .map(|binding| {
                resolve_runtime_input(binding, Some(keyboard), None, buttons, axes).pressed
            })
            .unwrap_or(false)
    }

    /// Resolves one action from the current merged keyboard and gamepad state.
    pub fn resolve_action(&self, action: &str, world: &World) -> GameInputActionState {
        self.actions
            .get(action)
            .map(|binding| {
                resolve_runtime_input(
                    binding,
                    world.get_resource::<Input<KeyCode>>(),
                    world.get_resource::<Input<MouseButton>>(),
                    world.get_resource::<Input<GamepadButton>>(),
                    world.get_resource::<GamepadAxisState>(),
                )
            })
            .unwrap_or_default()
    }

    /// Resolves every declared action in stable name order for debugging.
    pub fn resolved_actions(&self, world: &World) -> Vec<(String, GameInputActionState)> {
        let mut actions: Vec<_> = self
            .actions
            .iter()
            .map(|(name, binding)| {
                (
                    name.clone(),
                    resolve_runtime_input(
                        binding,
                        world.get_resource::<Input<KeyCode>>(),
                        world.get_resource::<Input<MouseButton>>(),
                        world.get_resource::<Input<GamepadButton>>(),
                        world.get_resource::<GamepadAxisState>(),
                    ),
                )
            })
            .collect();
        actions.sort_by(|left, right| left.0.cmp(&right.0));
        actions
    }
}

fn resolve_runtime_input(
    binding: &RuntimeInputAction,
    keyboard: Option<&Input<KeyCode>>,
    mouse: Option<&Input<MouseButton>>,
    buttons: Option<&Input<GamepadButton>>,
    axes: Option<&GamepadAxisState>,
) -> GameInputActionState {
    let digital_pressed = binding
        .keys
        .iter()
        .copied()
        .any(|key| input_pressed(keyboard, key, false))
        || binding
            .mouse_buttons
            .iter()
            .copied()
            .any(|button| input_pressed(mouse, button, false))
        || binding
            .gamepad_buttons
            .iter()
            .copied()
            .any(|button| input_pressed(buttons, button, false));
    let previous_digital_pressed = binding
        .keys
        .iter()
        .copied()
        .any(|key| input_pressed(keyboard, key, true))
        || binding
            .mouse_buttons
            .iter()
            .copied()
            .any(|button| input_pressed(mouse, button, true))
        || binding
            .gamepad_buttons
            .iter()
            .copied()
            .any(|button| input_pressed(buttons, button, true));

    let has_axis_bindings = !binding.gamepad_axes.is_empty() || !binding.key_axes.is_empty();
    let mut scalar: f32 = if !has_axis_bindings && digital_pressed {
        1.0
    } else {
        0.0
    };
    let mut vector = resolve_axis_vector(binding, keyboard, axes, false);
    if !has_axis_bindings {
        vector[0] = scalar;
    }
    let previous_vector = resolve_axis_vector(binding, keyboard, axes, true);
    for value in &mut vector {
        if value.abs() > scalar.abs() {
            scalar = *value;
        }
    }
    let axis_pressed = vector != [0.0, 0.0];
    let previous_axis_pressed = previous_vector != [0.0, 0.0];
    let pressed = digital_pressed || axis_pressed;
    let previous_pressed = previous_digital_pressed || previous_axis_pressed;

    GameInputActionState {
        pressed,
        just_pressed: pressed && !previous_pressed,
        just_released: !pressed && previous_pressed,
        scalar,
        vector,
    }
}

fn input_pressed<T>(input: Option<&Input<T>>, key: T, previous: bool) -> bool
where
    T: Copy + Eq + std::hash::Hash,
{
    let Some(input) = input else { return false };
    if !previous {
        return input.pressed(key);
    }
    if input.just_pressed(key) {
        false
    } else if input.just_released(key) {
        true
    } else {
        input.pressed(key)
    }
}

fn resolve_axis_vector(
    binding: &RuntimeInputAction,
    keyboard: Option<&Input<KeyCode>>,
    axes: Option<&GamepadAxisState>,
    previous: bool,
) -> [f32; 2] {
    let mut vector = [0.0, 0.0];
    for (index, axis) in binding.gamepad_axes.iter().enumerate() {
        let raw = axes.map_or(0.0, |state| {
            if previous {
                state.previous_merged_axis(axis.axis)
            } else {
                state.merged_axis(axis.axis)
            }
        });
        let directed = if axis.invert { -raw } else { raw };
        let value = if directed.abs() >= axis.deadzone {
            directed * axis.scale
        } else {
            0.0
        };
        let component = index.min(vector.len() - 1);
        vector[component] += value;
    }
    for axis in &binding.key_axes {
        let negative = axis
            .negative_keys
            .iter()
            .filter(|key| input_pressed(keyboard, **key, previous))
            .count() as f32;
        let positive = axis
            .positive_keys
            .iter()
            .filter(|key| input_pressed(keyboard, **key, previous))
            .count() as f32;
        vector[axis.vector_component] += (positive - negative) * axis.scale;
    }
    for value in &mut vector {
        *value = value.clamp(-1.0, 1.0);
    }
    vector
}

fn compile_input_action(
    action: &InputAction,
    diagnostics: &mut Vec<InputActionMapDiagnostic>,
) -> RuntimeInputAction {
    let keys = action
        .keys
        .iter()
        .filter_map(|binding| match parse_key_code(binding) {
            Some(key) => Some(key),
            None => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!("unknown keyboard binding `{binding}` was ignored"),
                });
                None
            }
        })
        .collect();
    let gamepad_buttons = action
        .gamepad_buttons
        .iter()
        .filter_map(|binding| match gamepad_button_from_index(*binding) {
            Some(button) => Some(button),
            None => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!("unknown gamepad button index `{binding}` was ignored"),
                });
                None
            }
        })
        .collect();
    let mouse_buttons = action
        .mouse_buttons
        .iter()
        .filter_map(|binding| match parse_mouse_button(binding) {
            Some(button) => Some(button),
            None => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!("unknown mouse button binding `{binding}` was ignored"),
                });
                None
            }
        })
        .collect();
    let gamepad_axes = action
        .gamepad_axes
        .iter()
        .filter_map(|binding| match gamepad_axis_from_index(binding.axis) {
            Some(axis) if binding.deadzone.is_finite() && binding.scale.is_finite() => {
                Some(RuntimeGamepadAxisBinding {
                    axis,
                    deadzone: binding.deadzone.clamp(0.0, 1.0),
                    scale: binding.scale,
                    invert: binding.invert,
                })
            }
            Some(_) => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!(
                        "gamepad axis index `{}` has a non-finite deadzone or scale and was ignored",
                        binding.axis
                    ),
                });
                None
            }
            None => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!("unknown gamepad axis index `{}` was ignored", binding.axis),
                });
                None
            }
        })
        .collect();
    let key_axes = action
        .key_axes
        .iter()
        .filter_map(|binding| {
            if binding.vector_component > 1 || !binding.scale.is_finite() {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!(
                        "key axis component `{}` or scale is invalid and was ignored",
                        binding.vector_component
                    ),
                });
                return None;
            }
            Some(RuntimeKeyAxisBinding {
                vector_component: usize::from(binding.vector_component),
                negative_keys: compile_key_list(
                    action,
                    &binding.negative_keys,
                    "negative key axis",
                    diagnostics,
                ),
                positive_keys: compile_key_list(
                    action,
                    &binding.positive_keys,
                    "positive key axis",
                    diagnostics,
                ),
                scale: binding.scale,
            })
        })
        .collect();
    RuntimeInputAction {
        keys,
        mouse_buttons,
        gamepad_buttons,
        gamepad_axes,
        key_axes,
    }
}

fn compile_key_list(
    action: &InputAction,
    bindings: &[String],
    label: &str,
    diagnostics: &mut Vec<InputActionMapDiagnostic>,
) -> Vec<KeyCode> {
    bindings
        .iter()
        .filter_map(|binding| match parse_key_code(binding) {
            Some(key) => Some(key),
            None => {
                diagnostics.push(InputActionMapDiagnostic {
                    action: action.name.clone(),
                    message: format!("unknown {label} `{binding}` was ignored"),
                });
                None
            }
        })
        .collect()
}

pub(crate) fn parse_mouse_button(value: &str) -> Option<MouseButton> {
    Some(match value {
        "Left" => MouseButton::Left,
        "Right" => MouseButton::Right,
        "Middle" => MouseButton::Middle,
        "Back" => MouseButton::Back,
        "Forward" => MouseButton::Forward,
        _ => return None,
    })
}

pub(crate) fn parse_key_code(value: &str) -> Option<KeyCode> {
    Some(match value {
        "KeyA" => KeyCode::KeyA,
        "KeyB" => KeyCode::KeyB,
        "KeyC" => KeyCode::KeyC,
        "KeyD" => KeyCode::KeyD,
        "KeyE" => KeyCode::KeyE,
        "KeyF" => KeyCode::KeyF,
        "KeyG" => KeyCode::KeyG,
        "KeyH" => KeyCode::KeyH,
        "KeyI" => KeyCode::KeyI,
        "KeyJ" => KeyCode::KeyJ,
        "KeyK" => KeyCode::KeyK,
        "KeyL" => KeyCode::KeyL,
        "KeyM" => KeyCode::KeyM,
        "KeyN" => KeyCode::KeyN,
        "KeyO" => KeyCode::KeyO,
        "KeyP" => KeyCode::KeyP,
        "KeyQ" => KeyCode::KeyQ,
        "KeyR" => KeyCode::KeyR,
        "KeyS" => KeyCode::KeyS,
        "KeyT" => KeyCode::KeyT,
        "KeyU" => KeyCode::KeyU,
        "KeyV" => KeyCode::KeyV,
        "KeyW" => KeyCode::KeyW,
        "KeyX" => KeyCode::KeyX,
        "KeyY" => KeyCode::KeyY,
        "KeyZ" => KeyCode::KeyZ,
        "Digit0" => KeyCode::Digit0,
        "Digit1" => KeyCode::Digit1,
        "Digit2" => KeyCode::Digit2,
        "Digit3" => KeyCode::Digit3,
        "Digit4" => KeyCode::Digit4,
        "Digit5" => KeyCode::Digit5,
        "Digit6" => KeyCode::Digit6,
        "Digit7" => KeyCode::Digit7,
        "Digit8" => KeyCode::Digit8,
        "Digit9" => KeyCode::Digit9,
        "ArrowUp" => KeyCode::ArrowUp,
        "ArrowDown" => KeyCode::ArrowDown,
        "ArrowLeft" => KeyCode::ArrowLeft,
        "ArrowRight" => KeyCode::ArrowRight,
        "Space" => KeyCode::Space,
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "Tab" => KeyCode::Tab,
        "ShiftLeft" => KeyCode::ShiftLeft,
        "ShiftRight" => KeyCode::ShiftRight,
        "ControlLeft" => KeyCode::ControlLeft,
        "ControlRight" => KeyCode::ControlRight,
        _ => return None,
    })
}

fn gamepad_button_from_index(index: u32) -> Option<GamepadButton> {
    Some(match index {
        0 => GamepadButton::South,
        1 => GamepadButton::East,
        2 => GamepadButton::West,
        3 => GamepadButton::North,
        4 => GamepadButton::LeftShoulder,
        5 => GamepadButton::RightShoulder,
        6 => GamepadButton::Select,
        7 => GamepadButton::Start,
        _ => return None,
    })
}

fn gamepad_axis_from_index(index: u32) -> Option<GamepadAxis> {
    Some(match index {
        0 => GamepadAxis::LeftStickX,
        1 => GamepadAxis::LeftStickY,
        2 => GamepadAxis::RightStickX,
        3 => GamepadAxis::RightStickY,
        4 => GamepadAxis::LeftTrigger,
        5 => GamepadAxis::RightTrigger,
        _ => return None,
    })
}

/// Captures configured actions for every player before the fixed-step motor.
///
/// A project-defined vector action named `move` is preferred. Existing
/// `move_forward`, `move_back`, `move_left`, and `move_right` actions remain a
/// compatibility path. Hardcoded WASD is used only when no action map exists,
/// which keeps standalone examples working without making normal project Play
/// bypass Project Settings.
pub fn player_controller_system(
    keyboard: engine_ecs::Res<Input<KeyCode>>,
    mouse_buttons: Option<engine_ecs::Res<Input<MouseButton>>>,
    gamepad_buttons: Option<engine_ecs::Res<Input<GamepadButton>>>,
    gamepad_axes: Option<engine_ecs::Res<GamepadAxisState>>,
    action_map: Option<engine_ecs::Res<InputActionMap>>,
    mut intents: engine_ecs::ResMut<PlayerMovementIntents>,
    query: engine_ecs::Query<&PlayerController, engine_ecs::With<PlayerMarker>>,
) {
    let capture_count = intents.next_capture_generation();
    let mut next = HashMap::new();
    for (entity, _) in query.iter() {
        let (movement, sprint_requested, dodge_requested) = if let Some(ref map) = action_map {
            let movement = if map.contains_action("move") {
                let state = resolve_named_action(
                    map,
                    "move",
                    &keyboard,
                    mouse_buttons.as_deref(),
                    gamepad_buttons.as_deref(),
                    gamepad_axes.as_deref(),
                );
                Vec2::from_array(state.vector)
            } else {
                resolve_legacy_movement(
                    map,
                    &keyboard,
                    mouse_buttons.as_deref(),
                    gamepad_buttons.as_deref(),
                    gamepad_axes.as_deref(),
                )
            };
            let sprint = resolve_named_action(
                map,
                "sprint",
                &keyboard,
                mouse_buttons.as_deref(),
                gamepad_buttons.as_deref(),
                gamepad_axes.as_deref(),
            );
            let dodge = resolve_named_action(
                map,
                "dodge",
                &keyboard,
                mouse_buttons.as_deref(),
                gamepad_buttons.as_deref(),
                gamepad_axes.as_deref(),
            );
            (movement, sprint.pressed, dodge.just_pressed)
        } else {
            (
                Vec2::new(
                    digital_axis(
                        keyboard.pressed(KeyCode::KeyA),
                        keyboard.pressed(KeyCode::KeyD),
                    ),
                    digital_axis(
                        keyboard.pressed(KeyCode::KeyS),
                        keyboard.pressed(KeyCode::KeyW),
                    ),
                ),
                false,
                false,
            )
        };
        next.insert(
            entity,
            PlayerMovementIntent {
                movement: movement.clamp_length_max(1.0),
                sprint_requested,
                dodge_requested,
                capture_generation: capture_count,
            },
        );
    }
    intents.replace_capture(capture_count, next);
}

fn resolve_legacy_movement(
    map: &InputActionMap,
    keyboard: &Input<KeyCode>,
    mouse: Option<&Input<MouseButton>>,
    gamepad_buttons: Option<&Input<GamepadButton>>,
    gamepad_axes: Option<&GamepadAxisState>,
) -> Vec2 {
    let pressed = |name| {
        resolve_named_action(map, name, keyboard, mouse, gamepad_buttons, gamepad_axes).pressed
    };
    Vec2::new(
        digital_axis(pressed("move_left"), pressed("move_right")),
        digital_axis(pressed("move_back"), pressed("move_forward")),
    )
}

fn digital_axis(negative: bool, positive: bool) -> f32 {
    f32::from(u8::from(positive)) - f32::from(u8::from(negative))
}

fn resolve_named_action(
    map: &InputActionMap,
    name: &str,
    keyboard: &Input<KeyCode>,
    mouse: Option<&Input<MouseButton>>,
    gamepad_buttons: Option<&Input<GamepadButton>>,
    gamepad_axes: Option<&GamepadAxisState>,
) -> GameInputActionState {
    map.actions
        .get(name)
        .map(|binding| {
            resolve_runtime_input(
                binding,
                Some(keyboard),
                mouse,
                gamepad_buttons,
                gamepad_axes,
            )
        })
        .unwrap_or_default()
}

/// Converts captured logical movement into kinematic-controller velocity.
///
/// Register this on FixedUpdate before [`crate::character_controller_system`].
/// It never integrates [`Transform::translation`]; collision and movement
/// therefore have one owner regardless of rendered frame rate.
pub fn player_character_motor_system(
    fixed_time: engine_ecs::Res<FixedTime>,
    intents: engine_ecs::Res<PlayerMovementIntents>,
    cameras: engine_ecs::Query<(&Camera3D, &GlobalTransform)>,
    mut players: engine_ecs::Query<
        (
            &PlayerController,
            &mut KinematicCharacterController,
            &mut Transform,
        ),
        engine_ecs::With<PlayerMarker>,
    >,
) {
    let camera_basis = select_active_game_camera(cameras.iter()).map(|(_, (_, global))| {
        let (_, rotation, _) = global.matrix().to_scale_rotation_translation();
        planar_camera_basis(rotation)
    });

    for (entity, (config, motor, transform)) in &mut players {
        let intent = intents.get(entity).unwrap_or_default();
        let speed_multiplier = if intent.sprint_requested {
            config.sprint_multiplier.max(0.0)
        } else {
            1.0
        };
        let target = match config.move_plane {
            MovePlane::Xz => {
                let (right, forward) = if config.camera_relative {
                    camera_basis.unwrap_or((Vec3::X, Vec3::NEG_Z))
                } else {
                    (Vec3::X, Vec3::NEG_Z)
                };
                (right * intent.movement.x + forward * intent.movement.y)
                    * config.move_speed.max(0.0)
                    * speed_multiplier
            }
            MovePlane::Xy => {
                Vec3::new(intent.movement.x, intent.movement.y, 0.0)
                    * config.move_speed.max(0.0)
                    * speed_multiplier
            }
        };

        let current = match config.move_plane {
            MovePlane::Xz => Vec3::new(motor.velocity.x, 0.0, motor.velocity.z),
            MovePlane::Xy => Vec3::new(motor.velocity.x, motor.velocity.y, 0.0),
        };
        let rate = if target.length_squared() > 0.0 {
            config.acceleration
        } else {
            config.deceleration
        }
        .max(0.0);
        let velocity = move_towards(current, target, rate * fixed_time.fixed_delta);

        match config.move_plane {
            MovePlane::Xz => {
                motor.velocity.x = velocity.x;
                motor.velocity.z = velocity.z;
                if config.face_movement && target.length_squared() > 1.0e-6 {
                    let direction = target.normalize();
                    transform.rotation = Quat::from_rotation_y((-direction.x).atan2(-direction.z));
                }
            }
            MovePlane::Xy => {
                motor.velocity.x = velocity.x;
                motor.velocity.y = velocity.y;
            }
        }
    }
}

fn planar_camera_basis(rotation: Quat) -> (Vec3, Vec3) {
    let right = (rotation * Vec3::X)
        .reject_from(Vec3::Y)
        .try_normalize()
        .unwrap_or(Vec3::X);
    let forward = (rotation * Vec3::NEG_Z)
        .reject_from(Vec3::Y)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    (right, forward)
}

fn move_towards(current: Vec3, target: Vec3, max_delta: f32) -> Vec3 {
    let delta = target - current;
    if delta.length_squared() <= max_delta * max_delta || max_delta <= 0.0 {
        if max_delta <= 0.0 {
            current
        } else {
            target
        }
    } else {
        current + delta.normalize() * max_delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::GamepadId;
    use engine_authoring::project_settings::AxisBinding;
    use engine_ecs::{IntoSystem, System, World};
    use glam::Vec3;

    fn make_world_with_player(move_plane: MovePlane) -> (World, engine_ecs::Entity) {
        let mut world = World::new();
        world.insert_resource(Input::<KeyCode>::new());
        world.insert_resource(Input::<MouseButton>::new());
        world.insert_resource(Input::<GamepadButton>::new());
        world.insert_resource(GamepadAxisState::new());
        world.insert_resource(PlayerMovementIntents::default());
        world.insert_resource(FixedTime::with_delta(1.0));

        let entity = world.spawn().expect("spawn");
        world
            .add_component(entity, Transform::default())
            .expect("transform");
        world
            .add_component(
                entity,
                PlayerController {
                    move_speed: 1.0,
                    move_plane,
                    ..PlayerController::default()
                },
            )
            .expect("controller");
        world.add_component(entity, PlayerMarker).expect("marker");
        world
            .add_component(entity, KinematicCharacterController::default())
            .expect("motor");
        (world, entity)
    }

    fn run_player_motor(world: &mut World) {
        let mut input = player_controller_system
            .into_system()
            .expect("input system");
        input.run(world).expect("capture input");
        let mut motor = player_character_motor_system
            .into_system()
            .expect("motor system");
        motor.run(world).expect("apply motor");
    }

    #[test]
    fn xz_move_plane_w_decreases_z() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);
        world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("input")
            .press(KeyCode::KeyW);

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!(motor.velocity.z < 0.0, "W in XZ mode must decrease Z");
        assert_eq!(
            world
                .get_component::<Transform>(entity)
                .expect("transform")
                .translation,
            Vec3::ZERO,
            "the motor must leave translation integration to the character controller"
        );
    }

    #[test]
    fn xy_move_plane_w_increases_y() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xy);
        world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("input")
            .press(KeyCode::KeyW);

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!(motor.velocity.y > 0.0, "W in XY mode must increase Y");
    }

    #[test]
    fn no_input_leaves_position_unchanged() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);

        run_player_motor(&mut world);

        let transform = world.get_component::<Transform>(entity).expect("transform");
        assert_eq!(transform.translation, Vec3::ZERO);
        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert_eq!(motor.velocity, Vec3::ZERO);
    }

    #[test]
    fn player_controller_default_uses_xz_plane() {
        let controller = PlayerController::default();
        assert_eq!(controller.move_plane, MovePlane::Xz);
    }

    #[test]
    fn vector_action_preserves_analog_sprint_dodge_facing_and_camera_basis() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);
        let settings = ProjectSettings {
            input_actions: vec![
                InputAction {
                    name: "move".to_owned(),
                    keys: Vec::new(),
                    mouse_buttons: Vec::new(),
                    gamepad_buttons: Vec::new(),
                    gamepad_axes: vec![
                        AxisBinding {
                            axis: 0,
                            deadzone: 0.1,
                            scale: 1.0,
                            invert: false,
                        },
                        AxisBinding {
                            axis: 1,
                            deadzone: 0.1,
                            scale: 1.0,
                            invert: false,
                        },
                    ],
                    key_axes: Vec::new(),
                },
                InputAction {
                    name: "sprint".to_owned(),
                    keys: vec!["ShiftLeft".to_owned()],
                    mouse_buttons: Vec::new(),
                    gamepad_buttons: Vec::new(),
                    gamepad_axes: Vec::new(),
                    key_axes: Vec::new(),
                },
                InputAction {
                    name: "dodge".to_owned(),
                    keys: vec!["Space".to_owned()],
                    mouse_buttons: Vec::new(),
                    gamepad_buttons: Vec::new(),
                    gamepad_axes: Vec::new(),
                    key_axes: Vec::new(),
                },
            ],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        world.insert_resource(map);
        world
            .get_resource_mut::<GamepadAxisState>()
            .expect("axes")
            .set(GamepadId(0), GamepadAxis::LeftStickY, 0.5);
        let keyboard = world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("keyboard");
        keyboard.press(KeyCode::ShiftLeft);
        keyboard.press(KeyCode::Space);

        let standby_camera = world.spawn().expect("standby camera");
        world
            .add_component(standby_camera, Camera3D::default())
            .expect("standby camera component");
        world
            .add_component(standby_camera, GlobalTransform::default())
            .expect("standby camera transform");

        let camera = world.spawn().expect("camera");
        let active_camera = Camera3D {
            priority: 10,
            ..Camera3D::default()
        };
        world
            .add_component(camera, active_camera)
            .expect("camera component");
        world
            .add_component(
                camera,
                GlobalTransform(glam::Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2)),
            )
            .expect("camera transform");

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!((motor.velocity.x.abs() - 0.75).abs() < 1.0e-5);
        assert!(motor.velocity.z.abs() < 1.0e-5);
        let intent = world
            .get_resource::<PlayerMovementIntents>()
            .expect("intents")
            .get(entity)
            .expect("player intent");
        assert!(intent.sprint_requested);
        assert!(intent.dodge_requested);
        let forward = world
            .get_component::<Transform>(entity)
            .expect("transform")
            .rotation
            * Vec3::NEG_Z;
        assert!(forward.dot(motor.velocity.normalize()) > 0.999);
    }

    #[test]
    fn action_map_overrides_keyboard_binding() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);

        let mut map = InputActionMap::new();
        map.insert("move_forward", vec![KeyCode::ArrowUp]);
        map.insert("move_back", vec![KeyCode::ArrowDown]);
        map.insert("move_left", vec![KeyCode::ArrowLeft]);
        map.insert("move_right", vec![KeyCode::ArrowRight]);
        world.insert_resource(map);

        world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("input")
            .press(KeyCode::ArrowUp);

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!(
            motor.velocity.z < 0.0,
            "ArrowUp via action map must move forward (decrease Z)"
        );
    }

    #[test]
    fn action_map_absent_falls_back_to_wasd() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);
        world
            .get_resource_mut::<Input<KeyCode>>()
            .expect("input")
            .press(KeyCode::KeyW);

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!(
            motor.velocity.z < 0.0,
            "without action map, W must still move forward"
        );
    }

    #[test]
    fn legacy_direction_actions_resolve_mouse_bindings_through_common_path() {
        let (mut world, entity) = make_world_with_player(MovePlane::Xz);
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "move_forward".to_owned(),
                keys: Vec::new(),
                mouse_buttons: vec!["Left".to_owned()],
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        world.insert_resource(map);
        world
            .get_resource_mut::<Input<MouseButton>>()
            .expect("mouse buttons")
            .press(MouseButton::Left);

        run_player_motor(&mut world);

        let motor = world
            .get_component::<KinematicCharacterController>(entity)
            .expect("motor");
        assert!(motor.velocity.z < 0.0);
    }

    #[test]
    fn project_settings_compile_to_keyboard_transition_state() {
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "attack".to_owned(),
                keys: vec!["Space".to_owned()],
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        let mut world = World::new();
        let mut keyboard = Input::<KeyCode>::new();
        keyboard.press(KeyCode::Space);
        world.insert_resource(keyboard);

        let state = map.resolve_action("attack", &world);
        assert!(state.pressed);
        assert!(state.just_pressed);
        assert_eq!(state.scalar, 1.0);
        assert_eq!(state.vector, [1.0, 0.0]);
    }

    #[test]
    fn project_settings_axes_apply_deadzone_and_report_unknown_bindings() {
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "move".to_owned(),
                keys: vec!["NotAKey".to_owned()],
                mouse_buttons: Vec::new(),
                gamepad_buttons: vec![99],
                gamepad_axes: vec![
                    AxisBinding {
                        axis: 0,
                        deadzone: 0.2,
                        scale: 1.0,
                        invert: false,
                    },
                    AxisBinding {
                        axis: 1,
                        deadzone: 0.2,
                        scale: 1.0,
                        invert: false,
                    },
                ],
                key_axes: Vec::new(),
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert_eq!(diagnostics.len(), 2);
        let mut world = World::new();
        let mut axes = GamepadAxisState::new();
        axes.set(GamepadId(0), GamepadAxis::LeftStickX, 0.1);
        axes.set(GamepadId(0), GamepadAxis::LeftStickY, -0.75);
        world.insert_resource(axes);

        let state = map.resolve_action("move", &world);
        assert!(state.pressed);
        assert_eq!(state.scalar, -0.75);
        assert_eq!(state.vector, [0.0, -0.75]);
    }

    #[test]
    fn mouse_scaled_axis_and_key_pair_resolve_through_one_action_map() {
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "move_or_attack".to_owned(),
                keys: Vec::new(),
                mouse_buttons: vec!["Left".to_owned()],
                gamepad_buttons: Vec::new(),
                gamepad_axes: vec![AxisBinding {
                    axis: 0,
                    deadzone: 0.1,
                    scale: 2.0,
                    invert: true,
                }],
                key_axes: vec![engine_authoring::KeyAxisBinding {
                    vector_component: 1,
                    negative_keys: vec!["KeyS".to_owned()],
                    positive_keys: vec!["KeyW".to_owned()],
                    scale: 0.5,
                }],
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        let mut world = World::new();
        let mut mouse = Input::<MouseButton>::new();
        mouse.press(MouseButton::Left);
        world.insert_resource(mouse);
        let mut keyboard = Input::<KeyCode>::new();
        keyboard.press(KeyCode::KeyW);
        world.insert_resource(keyboard);
        let mut axes = GamepadAxisState::new();
        axes.set(GamepadId(0), GamepadAxis::LeftStickX, 0.25);
        world.insert_resource(axes);

        let state = map.resolve_action("move_or_attack", &world);
        assert!(state.pressed);
        assert!(state.just_pressed);
        assert_eq!(state.scalar, -0.5);
        assert_eq!(state.vector, [-0.5, 0.5]);
    }

    #[test]
    fn analog_actions_publish_transitions_at_frame_boundaries() {
        let settings = ProjectSettings {
            input_actions: vec![InputAction {
                name: "move".to_owned(),
                keys: Vec::new(),
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: vec![AxisBinding {
                    axis: 0,
                    deadzone: 0.2,
                    scale: 1.0,
                    invert: false,
                }],
                key_axes: Vec::new(),
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        let mut world = World::new();
        world.insert_resource(GamepadAxisState::new());

        world
            .get_resource_mut::<GamepadAxisState>()
            .expect("axes")
            .set(GamepadId(0), GamepadAxis::LeftStickX, 0.8);
        let pressed = map.resolve_action("move", &world);
        assert!(pressed.pressed);
        assert!(pressed.just_pressed);
        assert!(!pressed.just_released);

        world
            .get_resource_mut::<GamepadAxisState>()
            .expect("axes")
            .begin_frame();
        let held = map.resolve_action("move", &world);
        assert!(held.pressed);
        assert!(!held.just_pressed);

        let axes = world.get_resource_mut::<GamepadAxisState>().expect("axes");
        axes.begin_frame();
        axes.set(GamepadId(0), GamepadAxis::LeftStickX, 0.0);
        let released = map.resolve_action("move", &world);
        assert!(!released.pressed);
        assert!(released.just_released);
    }
}
