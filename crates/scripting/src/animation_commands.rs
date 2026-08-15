//! Animation-specific deferred command constructors owned with [`crate::game_io::GameCommand`].

use std::collections::BTreeMap;

use engine_authoring::Value;

use crate::game_api::Commands;
use crate::game_io::{GameCommand, GameCommandFamily, GameEntityHandle};

impl GameCommand {
    /// Creates a raw persistent boolean Animation Graph parameter command.
    pub fn set_animation_bool(
        target: GameEntityHandle,
        name: impl Into<String>,
        value: bool,
    ) -> Self {
        animation_parameter_command(target, "set_bool", name, Some(Value::Bool(value)))
    }

    /// Creates a raw persistent float Animation Graph parameter command.
    pub fn set_animation_float(
        target: GameEntityHandle,
        name: impl Into<String>,
        value: f32,
    ) -> Self {
        animation_parameter_command(
            target,
            "set_float",
            name,
            Some(Value::F64(f64::from(value))),
        )
    }

    /// Creates a raw one-shot Animation Graph trigger command.
    pub fn trigger_animation(target: GameEntityHandle, name: impl Into<String>) -> Self {
        animation_parameter_command(target, "trigger", name, None)
    }
}

fn animation_parameter_command(
    target: GameEntityHandle,
    operation: &str,
    name: impl Into<String>,
    value: Option<Value>,
) -> GameCommand {
    let mut fields = BTreeMap::from([
        (
            "operation".to_owned(),
            Value::String(operation.to_owned()),
        ),
        ("name".to_owned(), Value::String(name.into())),
    ]);
    if let Some(value) = value {
        fields.insert("value".to_owned(), value);
    }
    GameCommand {
        family: GameCommandFamily::Animation,
        request_id: None,
        target: Some(target),
        payload: Value::Object(fields),
    }
}

impl Commands {
    /// Sets or creates a persistent boolean Animation Graph parameter.
    pub fn set_animation_bool(
        &mut self,
        target: GameEntityHandle,
        name: impl Into<String>,
        value: bool,
    ) {
        self.push(GameCommand::set_animation_bool(target, name, value));
    }

    /// Sets or creates a persistent finite float Animation Graph parameter.
    ///
    /// Non-finite values are rejected by host command preflight.
    pub fn set_animation_float(
        &mut self,
        target: GameEntityHandle,
        name: impl Into<String>,
        value: f32,
    ) {
        self.push(GameCommand::set_animation_float(target, name, value));
    }

    /// Sets a one-shot trigger consumed by the first matching transition.
    pub fn trigger_animation(&mut self, target: GameEntityHandle, name: impl Into<String>) {
        self.push(GameCommand::trigger_animation(target, name));
    }
}
