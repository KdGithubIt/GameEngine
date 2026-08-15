//! Host-independent Rust gameplay ABI type contracts.

use std::collections::{BTreeMap, BTreeSet};

use engine_assets::data_asset::DataAssetRef;
use engine_ecs::SystemId;
use serde::{Deserialize, Serialize};

pub use engine_authoring::id::ComponentTypeId;
pub use engine_authoring::schema::{ComponentSchema, FieldSchema, FieldType};
pub use engine_authoring::value::Value;

/// Runtime schedules available to project-local Rust systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameSystemSchedule {
    /// Runs once after the built-in per-frame ECS schedule.
    Update,
    /// Runs once after each built-in fixed-timestep ECS schedule step.
    FixedUpdate,
}

/// Converts one supported public Rust field to and from authoring data.
pub trait GameField: Sized {
    /// Returns the authoring schema type for this Rust field.
    fn field_type() -> FieldType;
    /// Converts this field into deterministic authoring data.
    fn to_value(&self) -> Value;
    /// Decodes this field from authoring data.
    fn from_value(value: &Value) -> Result<Self, String>;
}

impl GameField for bool {
    fn field_type() -> FieldType {
        FieldType::Bool
    }
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(value) => Ok(*value),
            _ => Err("expected a boolean".to_owned()),
        }
    }
}

impl GameField for i64 {
    fn field_type() -> FieldType {
        FieldType::I64
    }
    fn to_value(&self) -> Value {
        Value::I64(*self)
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::I64(value) => Ok(*value),
            _ => Err("expected a signed integer".to_owned()),
        }
    }
}

impl GameField for u64 {
    fn field_type() -> FieldType {
        FieldType::U64
    }
    fn to_value(&self) -> Value {
        Value::U64(*self)
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::U64(value) => Ok(*value),
            Value::I64(value) if *value >= 0 => Ok(*value as u64),
            _ => Err("expected a non-negative integer".to_owned()),
        }
    }
}

impl GameField for f32 {
    fn field_type() -> FieldType {
        FieldType::F64
    }
    fn to_value(&self) -> Value {
        Value::F64(f64::from(*self))
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        decode_f64(value).map(|value| value as f32)
    }
}

impl GameField for f64 {
    fn field_type() -> FieldType {
        FieldType::F64
    }
    fn to_value(&self) -> Value {
        Value::F64(*self)
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        decode_f64(value)
    }
}

impl GameField for String {
    fn field_type() -> FieldType {
        FieldType::String
    }
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(value) => Ok(value.clone()),
            _ => Err("expected a string".to_owned()),
        }
    }
}

impl GameField for glam::Vec2 {
    fn field_type() -> FieldType {
        FieldType::Vec2
    }
    fn to_value(&self) -> Value {
        Value::Object(BTreeMap::from([
            ("x".to_owned(), Value::F64(f64::from(self.x))),
            ("y".to_owned(), Value::F64(f64::from(self.y))),
        ]))
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        let fields = object_fields(value, "two-dimensional vector")?;
        Ok(Self::new(
            decode_named_f32(fields, "x")?,
            decode_named_f32(fields, "y")?,
        ))
    }
}

impl GameField for glam::Vec3 {
    fn field_type() -> FieldType {
        FieldType::Vec3
    }
    fn to_value(&self) -> Value {
        Value::Object(BTreeMap::from([
            ("x".to_owned(), Value::F64(f64::from(self.x))),
            ("y".to_owned(), Value::F64(f64::from(self.y))),
            ("z".to_owned(), Value::F64(f64::from(self.z))),
        ]))
    }
    fn from_value(value: &Value) -> Result<Self, String> {
        let fields = object_fields(value, "three-dimensional vector")?;
        Ok(Self::new(
            decode_named_f32(fields, "x")?,
            decode_named_f32(fields, "y")?,
            decode_named_f32(fields, "z")?,
        ))
    }
}

fn decode_f64(value: &Value) -> Result<f64, String> {
    match value {
        Value::F64(value) => Ok(*value),
        Value::I64(value) => Ok(*value as f64),
        Value::U64(value) => Ok(*value as f64),
        _ => Err("expected a number".to_owned()),
    }
}

fn object_fields<'a>(
    value: &'a Value,
    expected: &str,
) -> Result<&'a BTreeMap<String, Value>, String> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(format!("expected a {expected} object")),
    }
}

fn decode_named_f32(fields: &BTreeMap<String, Value>, name: &str) -> Result<f32, String> {
    fields
        .get(name)
        .ok_or_else(|| format!("vector field `{name}` is missing"))
        .and_then(decode_f64)
        .map(|value| value as f32)
}

/// Validates one exported project-resource schema.
#[doc(hidden)]
pub fn validate_resource_schema(schema: &GameResourceSchema) -> Result<(), String> {
    SystemId::try_new(schema.id.clone())
        .map_err(|error| format!("invalid stable resource ID: {error}"))?;
    if schema.display_name.trim().is_empty() {
        return Err("display name cannot be empty".to_owned());
    }
    if schema.version == 0 {
        return Err("schema version must be at least 1".to_owned());
    }
    let mut names = BTreeSet::new();
    for field in &schema.fields {
        if field.name.is_empty() {
            return Err("field name cannot be empty".to_owned());
        }
        if !names.insert(field.name.as_str()) {
            return Err(format!("duplicate field `{}`", field.name));
        }
    }
    validate_resource_value(schema, &schema.default_value)
        .map_err(|error| format!("invalid default value: {error}"))
}

/// Validates one authoring-shaped value against a project-resource schema.
#[doc(hidden)]
pub fn validate_resource_value(schema: &GameResourceSchema, value: &Value) -> Result<(), String> {
    let values = object_fields(value, "resource")?;
    for field in &schema.fields {
        match values.get(&field.name) {
            Some(value) => validate_field_value(&field.field_type, value)
                .map_err(|error| format!("field `{}`: {error}", field.name))?,
            None if field.required => {
                return Err(format!("required field `{}` is missing", field.name))
            }
            None => {}
        }
    }
    Ok(())
}

fn validate_field_value(field_type: &FieldType, value: &Value) -> Result<(), String> {
    let valid = match field_type {
        FieldType::Bool => matches!(value, Value::Bool(_)),
        FieldType::I64 => matches!(value, Value::I64(_)),
        FieldType::U64 => {
            matches!(value, Value::U64(_)) || matches!(value, Value::I64(number) if *number >= 0)
        }
        FieldType::F64 => matches!(value, Value::F64(_) | Value::I64(_) | Value::U64(_)),
        FieldType::String => matches!(value, Value::String(_)),
        FieldType::EntityRef => matches!(value, Value::EntityRef(_)),
        FieldType::AssetRef => matches!(value, Value::AssetRef(_)),
        FieldType::Object => matches!(value, Value::Object(_)),
        FieldType::Vec2 => validate_vector_value(value, &["x", "y"]).is_ok(),
        FieldType::Vec3 => validate_vector_value(value, &["x", "y", "z"]).is_ok(),
        FieldType::Array(element) => {
            let Value::Array(values) = value else {
                return Err("expected an array".to_owned());
            };
            for (index, value) in values.iter().enumerate() {
                validate_field_value(element, value)
                    .map_err(|error| format!("array element {index}: {error}"))?;
            }
            true
        }
    };
    valid.then_some(()).ok_or_else(|| {
        format!(
            "expected {}",
            match field_type {
                FieldType::Bool => "a boolean",
                FieldType::I64 => "a signed integer",
                FieldType::U64 => "a non-negative integer",
                FieldType::F64 => "a number",
                FieldType::Vec2 => "a two-dimensional vector",
                FieldType::Vec3 => "a three-dimensional vector",
                FieldType::String => "a string",
                FieldType::EntityRef => "an entity reference",
                FieldType::AssetRef => "an asset reference",
                FieldType::Array(_) => "an array",
                FieldType::Object => "an object",
            }
        )
    })
}

fn validate_vector_value(value: &Value, axes: &[&str]) -> Result<(), String> {
    let fields = object_fields(value, "vector")?;
    for axis in axes {
        fields
            .get(*axis)
            .ok_or_else(|| format!("vector field `{axis}` is missing"))
            .and_then(decode_f64)?;
    }
    Ok(())
}

/// Contract generated for each project-local Rust component.
pub trait GameComponent: engine_ecs::Component + Default {
    /// Stable persisted component type ID, independent from the Rust type name.
    const TYPE_ID: &'static str;
    /// Human-readable label shown by the editor.
    const DISPLAY_NAME: &'static str;
    /// Returns this component's complete authoring schema.
    fn schema() -> ComponentSchema;
    /// Builds the runtime component from its persisted authoring value.
    fn from_authoring_value(value: &Value) -> Result<Self, String>;
    /// Converts the runtime Rust value back into authoring data.
    fn to_authoring_value(&self) -> Value;
}

/// Contract generated for each project-wide runtime resource.
///
/// A resource is copied into scoped callbacks as authoring-shaped data, but it
/// is owned by the host runtime rather than inserted as a concrete Rust ECS
/// resource. This keeps project type layouts behind the native-module ABI.
pub trait GameResource: Default {
    /// Stable dotted ID, independent from the Rust type name.
    const RESOURCE_ID: &'static str;
    /// Human-readable label used by diagnostics and future editor tooling.
    const DISPLAY_NAME: &'static str;
    /// Returns the complete runtime value schema and initial value.
    fn schema() -> GameResourceSchema;
    /// Decodes one host-shaped value into the concrete project type.
    fn from_value(value: &Value) -> Result<Self, String>;
    /// Converts the concrete value back into deterministic host data.
    fn to_value(&self) -> Value;
}

/// Schema exported for one project-wide runtime resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameResourceSchema {
    /// Stable dotted resource identifier.
    pub id: String,
    /// Human-readable resource name.
    pub display_name: String,
    /// Editor-facing explanation of the resource's purpose.
    pub description: String,
    /// Schema revision used by diagnostics and future migrations.
    pub version: u32,
    /// Named fields accepted in the resource object.
    pub fields: Vec<FieldSchema>,
    /// Complete value installed at the beginning of a Play generation.
    pub default_value: Value,
}


impl GameField for DataAssetRef {
    fn field_type() -> FieldType {
        FieldType::Object
    }

    fn to_value(&self) -> Value {
        self.to_authoring_value()
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        Self::from_authoring_value(value)
    }
}
