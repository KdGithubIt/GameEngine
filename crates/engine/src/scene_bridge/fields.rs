//! Typed field access over authoring component objects.

use super::*;

/// Typed field access over one authoring component object.
///
/// The reader captures the authoring entity, component type, and the
/// component's expected-shape message once, so each field read is a single
/// call and every failure produces a consistent
/// [`SceneBridgeError::InvalidComponentValue`].
pub(super) struct ComponentFields<'a> {
    entity: &'a AuthoringEntity,
    component_type: &'a ComponentTypeId,
    object: &'a BTreeMap<String, Value>,
    expected: &'static str,
}

impl<'a> ComponentFields<'a> {
    /// Wraps a component [`Value`] that must be an object.
    pub(super) fn new(
        entity: &'a AuthoringEntity,
        component_type: &'a ComponentTypeId,
        value: &'a Value,
        expected: &'static str,
    ) -> Result<Self, SceneBridgeError> {
        match value {
            Value::Object(object) => Ok(Self {
                entity,
                component_type,
                object,
                expected,
            }),
            _ => Err(invalid_component(entity, component_type, expected)),
        }
    }

    /// Builds the invalid-component error with a custom expectation, used by
    /// range and cross-field validation after the fields were read.
    pub(super) fn invalid(&self, expected: &'static str) -> SceneBridgeError {
        invalid_component(self.entity, self.component_type, expected)
    }

    pub(super) fn get(&self, field: &str) -> Option<&'a Value> {
        self.object.get(field)
    }

    pub(super) fn f32(&self, field: &str) -> Result<f32, SceneBridgeError> {
        let number = match self.object.get(field) {
            Some(Value::F64(number)) => *number as f32,
            Some(Value::I64(number)) => *number as f32,
            Some(Value::U64(number)) => *number as f32,
            None | Some(_) => return Err(self.invalid(self.expected)),
        };
        if !number.is_finite() {
            return Err(self.invalid(self.expected));
        }
        Ok(number)
    }

    pub(super) fn f32_or(&self, field: &str, default: f32) -> Result<f32, SceneBridgeError> {
        if self.object.contains_key(field) {
            self.f32(field)
        } else {
            Ok(default)
        }
    }

    pub(super) fn i64(&self, field: &str) -> Result<i64, SceneBridgeError> {
        match self.object.get(field) {
            Some(Value::I64(number)) => Ok(*number),
            Some(Value::U64(number)) => {
                i64::try_from(*number).map_err(|_| self.invalid(self.expected))
            }
            None | Some(_) => Err(self.invalid(self.expected)),
        }
    }

    pub(super) fn bool(&self, field: &str) -> Result<bool, SceneBridgeError> {
        match self.object.get(field) {
            Some(Value::Bool(value)) => Ok(*value),
            _ => Err(self.invalid(self.expected)),
        }
    }

    pub(super) fn bool_or(&self, field: &str, default: bool) -> Result<bool, SceneBridgeError> {
        if self.object.contains_key(field) {
            self.bool(field)
        } else {
            Ok(default)
        }
    }

    /// Reads a required string field, used by authoring components that
    /// encode an enum as a plain string (e.g. `engine.collider`'s `shape` and
    /// `engine.physics_body`'s `kind`).
    pub(super) fn string(&self, field: &str) -> Result<&'a str, SceneBridgeError> {
        match self.object.get(field) {
            Some(Value::String(value)) => Ok(value.as_str()),
            _ => Err(self.invalid(self.expected)),
        }
    }

    pub(super) fn asset_ref(&self, field: &str) -> Result<&'a AssetId, SceneBridgeError> {
        match self.object.get(field) {
            Some(Value::AssetRef(asset)) => Ok(asset),
            _ => Err(self.invalid(self.expected)),
        }
    }

    /// Reads a schema-required asset reference that may still be unassigned
    /// while the user finishes editing (ADR 0069).
    ///
    /// Returns `Ok(None)` when the field is absent so the caller can convert
    /// the component to its inactive state instead of failing. A field that
    /// is present but not an `AssetRef` is still an error: that is corrupted
    /// data, not an editing gap.
    pub(super) fn assignable_asset_ref(
        &self,
        field: &str,
    ) -> Result<Option<&'a AssetId>, SceneBridgeError> {
        match self.object.get(field) {
            None => Ok(None),
            Some(Value::AssetRef(asset)) => Ok(Some(asset)),
            Some(_) => Err(self.invalid(self.expected)),
        }
    }

    /// Reads the conventional `color_r`/`color_g`/`color_b` triple.
    pub(super) fn color(&self) -> Result<Vec3, SceneBridgeError> {
        let color = Vec3::new(
            self.f32("color_r")?,
            self.f32("color_g")?,
            self.f32("color_b")?,
        );
        if !color.is_finite() {
            return Err(self.invalid("finite numeric color fields"));
        }
        Ok(color)
    }
}

pub(super) fn validate_player_marker_value(
    entity: &AuthoringEntity,
    component_type: &ComponentTypeId,
    value: &Value,
) -> Result<(), SceneBridgeError> {
    match value {
        Value::Object(object) if object.is_empty() => Ok(()),
        _ => Err(invalid_component(entity, component_type, "an empty object")),
    }
}

pub(super) fn component_asset_ref<'a>(
    entity: &AuthoringEntity,
    component_type: &ComponentTypeId,
    value: &'a Value,
) -> Result<&'a AssetId, SceneBridgeError> {
    match value {
        Value::AssetRef(asset) => Ok(asset),
        _ => Err(invalid_component(entity, component_type, "an AssetRef")),
    }
}

pub(super) fn invalid_component(
    entity: &AuthoringEntity,
    component_type: &ComponentTypeId,
    expected: &'static str,
) -> SceneBridgeError {
    SceneBridgeError::InvalidComponentValue {
        entity: entity.id.clone(),
        component_type: component_type.clone(),
        expected,
    }
}
