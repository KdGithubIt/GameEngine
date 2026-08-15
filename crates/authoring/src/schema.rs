//! Component schema definitions for the authoring model.
//!
//! A [`ComponentSchema`] describes the fields, types, constraints, and
//! defaults of a component type. Schemas allow tools to discover valid
//! components and generate editing UI without reading Rust source code.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §8.1.

use crate::id::ComponentTypeId;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The declared type of a single component field.
///
/// Field types guide validation and UI generation. They do not carry Rust
/// type names; instead they carry authoring-level semantic types that tools
/// can interpret without compiling game code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// A boolean value.
    Bool,
    /// A signed 64-bit integer.
    I64,
    /// An unsigned 64-bit integer.
    U64,
    /// A 64-bit floating-point number.
    F64,
    /// A two-dimensional floating-point vector serialized as an object.
    Vec2,
    /// A three-dimensional floating-point vector serialized as an object.
    Vec3,
    /// A UTF-8 string.
    String,
    /// A reference to an authoring entity.
    EntityRef,
    /// A reference to an authoring asset.
    AssetRef,
    /// An ordered list of the given element type.
    Array(Box<FieldType>),
    /// A free-form string-keyed map.
    Object,
}

/// A machine-readable description of a single component field.
///
/// Field schemas are used by tools to generate editing UI, validate values,
/// and provide defaults without reading Rust source code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSchema {
    /// The Rust-style field name, e.g. `current_health`.
    pub name: String,

    /// The human-readable label for UI display, e.g. `Current Health`.
    pub display_name: String,

    /// A description of the field's meaning and constraints.
    pub description: String,

    /// The declared value type of this field.
    pub field_type: FieldType,

    /// Whether this field must be present for the component to be valid.
    pub required: bool,

    /// The default value used when the field is omitted.
    ///
    /// `None` means no default is defined and the field must be explicitly
    /// provided when `required` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
}

/// A machine-readable description of an editable component type.
///
/// Each component that can be added to an [`AuthoringEntity`] MUST expose a
/// `ComponentSchema` before it can be used in authoring commands or validated
/// by the authoring pipeline.
///
/// The `type_id` and `version` together identify a schema revision. Changing
/// field types or removing required fields requires a new version and a
/// migration record.
///
/// [`AuthoringEntity`]: crate::entity::AuthoringEntity
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentSchema {
    /// The stable identifier for this component type.
    pub type_id: ComponentTypeId,

    /// The human-readable component name for display in tools.
    pub display_name: String,

    /// A description of the component's purpose for tools and AI agents.
    pub description: String,

    /// The category used for grouping in component pickers.
    pub category: String,

    /// The schema version. Increment when field definitions change.
    pub version: u32,

    /// The fields defined by this component type.
    pub fields: Vec<FieldSchema>,

    /// A complete default component value overriding field defaults.
    ///
    /// Most components are objects built from [`FieldSchema`] defaults, but
    /// some component values are not objects at all (for example asset
    /// reference components whose canonical value is a bare `asset_ref`).
    /// Those schemas set this override and leave `fields` empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_default: Option<Value>,
}

impl ComponentSchema {
    /// Builds a default component value for this schema.
    ///
    /// Returns `component_default` when one is defined. Otherwise field
    /// defaults are placed into a deterministic object map; schemas with no
    /// fields produce an empty object, which is the canonical value for
    /// marker components in the current authoring format.
    pub fn default_value(&self) -> Value {
        if let Some(value) = &self.component_default {
            return value.clone();
        }
        let mut fields = BTreeMap::new();
        for field in &self.fields {
            if let Some(default) = &field.default_value {
                fields.insert(field.name.clone(), default.clone());
            }
        }
        Value::Object(fields)
    }
}

/// Hand-written registry of component schemas exposed to authoring tools.
///
/// This registry is intentionally small. Reflection-backed schema discovery
/// is still an open decision, so it starts from the built-in components that
/// can be described without referencing other crates. Downstream tools that
/// know about additional component types (such as the editor's engine asset
/// components) extend it through [`register`][Self::register].
#[derive(Debug, Clone)]
pub struct ComponentSchemaRegistry {
    schemas: BTreeMap<ComponentTypeId, ComponentSchema>,
}

impl ComponentSchemaRegistry {
    /// Creates a registry populated with built-in addable component schemas.
    pub fn builtin() -> Self {
        let mut registry = Self {
            schemas: BTreeMap::new(),
        };
        registry.register(transform_schema());
        registry.register(player_marker_schema());
        registry
    }

    /// Returns a schema by component type.
    pub fn get(&self, component_type: &ComponentTypeId) -> Option<&ComponentSchema> {
        self.schemas.get(component_type)
    }

    /// Iterates all registered schemas in stable component type order.
    pub fn schemas(&self) -> impl Iterator<Item = &ComponentSchema> {
        self.schemas.values()
    }

    /// Registers a schema, replacing any schema with the same component type.
    pub fn register(&mut self, schema: ComponentSchema) {
        self.schemas.insert(schema.type_id.clone(), schema);
    }
}

fn transform_schema() -> ComponentSchema {
    ComponentSchema {
        type_id: ComponentTypeId::new("engine.transform"),
        display_name: "Transform".into(),
        description: "Local position, Euler rotation, and scale for an entity.".into(),
        category: "Engine".into(),
        version: 2,
        fields: vec![
            transform_field("x", "Position X", "X position.", 0.0, true),
            transform_field("y", "Position Y", "Y position.", 0.0, true),
            transform_field("z", "Position Z", "Z position.", 0.0, true),
            transform_field(
                "rotation_x_degrees",
                "Rotation X",
                "X-axis Euler rotation in degrees.",
                0.0,
                false,
            ),
            transform_field(
                "rotation_y_degrees",
                "Rotation Y",
                "Y-axis Euler rotation in degrees.",
                0.0,
                false,
            ),
            transform_field(
                "rotation_z_degrees",
                "Rotation Z",
                "Z-axis Euler rotation in degrees.",
                0.0,
                false,
            ),
            transform_field("scale_x", "Scale X", "X scale multiplier.", 1.0, false),
            transform_field("scale_y", "Scale Y", "Y scale multiplier.", 1.0, false),
            transform_field("scale_z", "Scale Z", "Z scale multiplier.", 1.0, false),
        ],
        component_default: None,
    }
}

fn transform_field(
    name: &str,
    display_name: &str,
    description: &str,
    default: f64,
    required: bool,
) -> FieldSchema {
    FieldSchema {
        name: name.into(),
        display_name: display_name.into(),
        description: description.into(),
        field_type: FieldType::F64,
        required,
        default_value: Some(Value::F64(default)),
    }
}

fn player_marker_schema() -> ComponentSchema {
    ComponentSchema {
        type_id: ComponentTypeId::new("engine.player_marker"),
        display_name: "Player Marker".into(),
        description: "Marks the entity as the player start entity.".into(),
        category: "Engine".into(),
        version: 1,
        fields: vec![],
        component_default: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ComponentTypeId;

    fn health_schema() -> ComponentSchema {
        ComponentSchema {
            type_id: ComponentTypeId::new("gameplay.health"),
            display_name: "Health".into(),
            description: "The entity's hit points.".into(),
            category: "Gameplay".into(),
            version: 1,
            fields: vec![
                FieldSchema {
                    name: "current".into(),
                    display_name: "Current HP".into(),
                    description: "Current hit points.".into(),
                    field_type: FieldType::I64,
                    required: true,
                    default_value: Some(Value::I64(100)),
                },
                FieldSchema {
                    name: "maximum".into(),
                    display_name: "Maximum HP".into(),
                    description: "Maximum hit points.".into(),
                    field_type: FieldType::I64,
                    required: true,
                    default_value: Some(Value::I64(100)),
                },
            ],
            component_default: None,
        }
    }

    #[test]
    fn component_schema_survives_json_roundtrip() {
        let schema = health_schema();
        let json = serde_json::to_string_pretty(&schema).unwrap();
        let loaded: ComponentSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, loaded);
    }

    #[test]
    fn field_type_array_survives_roundtrip() {
        let ft = FieldType::Array(Box::new(FieldType::EntityRef));
        let json = serde_json::to_string(&ft).unwrap();
        let loaded: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, loaded);
    }

    #[test]
    fn field_without_default_omits_default_in_json() {
        let field = FieldSchema {
            name: "target".into(),
            display_name: "Target".into(),
            description: "The target entity.".into(),
            field_type: FieldType::EntityRef,
            required: false,
            default_value: None,
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(
            !json.contains("default_value"),
            "absent default should not appear in json: {json}"
        );
    }

    #[test]
    fn builtin_registry_contains_addable_engine_components() {
        let registry = ComponentSchemaRegistry::builtin();

        assert!(registry
            .get(&ComponentTypeId::new("engine.transform"))
            .is_some());
        assert!(registry
            .get(&ComponentTypeId::new("engine.player_marker"))
            .is_some());
    }

    #[test]
    fn component_schema_default_value_uses_field_defaults() {
        let registry = ComponentSchemaRegistry::builtin();
        let transform = registry
            .get(&ComponentTypeId::new("engine.transform"))
            .expect("transform schema must exist");

        assert_eq!(
            transform.default_value(),
            Value::Object(BTreeMap::from([
                ("rotation_x_degrees".into(), Value::F64(0.0)),
                ("rotation_y_degrees".into(), Value::F64(0.0)),
                ("rotation_z_degrees".into(), Value::F64(0.0)),
                ("scale_x".into(), Value::F64(1.0)),
                ("scale_y".into(), Value::F64(1.0)),
                ("scale_z".into(), Value::F64(1.0)),
                ("x".into(), Value::F64(0.0)),
                ("y".into(), Value::F64(0.0)),
                ("z".into(), Value::F64(0.0)),
            ]))
        );
    }

    #[test]
    fn marker_component_default_value_is_empty_object() {
        let registry = ComponentSchemaRegistry::builtin();
        let marker = registry
            .get(&ComponentTypeId::new("engine.player_marker"))
            .expect("player marker schema must exist");

        assert_eq!(marker.default_value(), Value::Object(BTreeMap::new()));
    }

    #[test]
    fn component_default_overrides_field_defaults() {
        let mut schema = health_schema();
        schema.component_default = Some(Value::String("override".into()));

        assert_eq!(schema.default_value(), Value::String("override".into()));
    }

    #[test]
    fn absent_component_default_is_omitted_from_json() {
        let json = serde_json::to_string(&health_schema()).unwrap();
        assert!(
            !json.contains("component_default"),
            "absent override should not appear in json: {json}"
        );
    }

    #[test]
    fn registered_schema_is_returned_by_get_and_schemas() {
        let mut registry = ComponentSchemaRegistry::builtin();
        registry.register(health_schema());

        let health_type = ComponentTypeId::new("gameplay.health");
        assert!(registry.get(&health_type).is_some());
        assert!(registry
            .schemas()
            .any(|schema| schema.type_id == health_type));
    }
}
