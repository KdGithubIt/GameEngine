//! Authoring entity model.
//!
//! An [`AuthoringEntity`] is the editable source of truth for a game object.
//! It is converted to a runtime ECS `Entity` during the build or play
//! pipeline. Runtime entity IDs MUST NOT be stored in project files.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §7.2.

use crate::id::{ComponentTypeId, EntityId};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An editable game object in the authoring model.
///
/// The `id` is stable across renames and runtime rebuilds. The `name`,
/// `display_name`, and `description` fields are mutable human-readable
/// metadata that can change freely without affecting `id` or any references
/// to this entity.
///
/// Components are keyed by [`ComponentTypeId`] and stored in a sorted map for
/// deterministic serialization order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthoringEntity {
    /// The stable project-wide identifier for this entity.
    ///
    /// Never changes when the entity is renamed. References to this entity
    /// from other entities or assets use this `id`.
    pub id: EntityId,

    /// A short lowercase slug used by AI agents and CLI tools for search.
    ///
    /// For example: `player`, `enemy_guard`, `door_main`. Mutable; changing
    /// this field does not affect `id` or any references to this entity.
    pub name: String,

    /// The human-readable label shown in editor UI.
    ///
    /// May contain spaces and mixed case. The field is required in the current
    /// serialized format even when the label is empty. Mutable; changing this
    /// field does not affect `id`.
    pub display_name: String,

    /// Extended documentation and AI context for this entity.
    ///
    /// Describes the entity's role and behavior in plain text. Used by AI
    /// agents to understand the entity without inspecting components. The
    /// field is required in the current serialized format even when empty.
    pub description: String,

    /// The stable identifier of the parent entity, if any.
    ///
    /// Entity hierarchy is expressed through parent references rather than
    /// embedding child lists. A `None` parent indicates a root entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntityId>,

    /// Whether this entity participates in runtime conversion.
    ///
    /// A disabled entity and all of its descendants are skipped when the
    /// scene is converted for Play or packaging. The current canonical writer
    /// omits `enabled` while it is `true`, so a missing field means enabled.
    #[serde(default = "default_enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,

    /// The component values attached to this entity, keyed by component type.
    ///
    /// Keys are sorted for deterministic serialization. Multiple components
    /// of the same type are not supported.
    pub components: BTreeMap<ComponentTypeId, Value>,
}

impl AuthoringEntity {
    /// Creates a new entity with the given stable ID and name.
    ///
    /// `display_name` and `description` default to empty strings. `parent`
    /// defaults to `None`. No components are attached.
    pub fn new(id: EntityId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            display_name: String::new(),
            description: String::new(),
            parent: None,
            enabled: true,
            components: BTreeMap::new(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

// serde's skip_serializing_if passes the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AssetId, EntityId};
    use crate::value::Value;

    fn make_entity() -> AuthoringEntity {
        let entity_id = EntityId::generate();
        let parent_id = EntityId::generate();
        let target_id = EntityId::generate();
        let icon_id = AssetId::generate();

        let mut components = BTreeMap::new();

        let mut health = BTreeMap::new();
        health.insert("current".into(), Value::I64(80));
        health.insert("maximum".into(), Value::I64(100));
        components.insert(
            ComponentTypeId::new("gameplay.health"),
            Value::Object(health),
        );
        components.insert(
            ComponentTypeId::new("gameplay.target"),
            Value::EntityRef(target_id),
        );
        components.insert(
            ComponentTypeId::new("gameplay.icon"),
            Value::AssetRef(icon_id),
        );

        AuthoringEntity {
            id: entity_id,
            name: "player".into(),
            display_name: "Player Character".into(),
            description: "The entity controlled by the player.".into(),
            enabled: true,
            parent: Some(parent_id),
            components,
        }
    }

    #[test]
    fn authoring_entity_survives_json_roundtrip() {
        let entity = make_entity();
        let json = serde_json::to_string_pretty(&entity).unwrap();
        let loaded: AuthoringEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(entity, loaded);
    }

    #[test]
    fn display_name_and_description_appear_in_serialized_json() {
        let mut entity = AuthoringEntity::new(EntityId::generate(), "enemy_guard");
        entity.display_name = "Enemy Guard".into();
        entity.description = "Patrols the corridor and attacks on sight.".into();

        let json = serde_json::to_string(&entity).unwrap();

        assert!(
            json.contains("\"display_name\""),
            "display_name must appear in json: {json}"
        );
        assert!(
            json.contains("\"description\""),
            "description must appear in json: {json}"
        );
        assert!(
            json.contains("Enemy Guard"),
            "display_name value must appear in json: {json}"
        );
        assert!(
            json.contains("Patrols the corridor"),
            "description value must appear in json: {json}"
        );
    }

    #[test]
    fn display_name_and_description_survive_roundtrip() {
        let mut entity = AuthoringEntity::new(EntityId::generate(), "door_main");
        entity.display_name = "Main Door".into();
        entity.description = "The entrance door to the level.".into();

        let json = serde_json::to_string(&entity).unwrap();
        let loaded: AuthoringEntity = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.display_name, "Main Door");
        assert_eq!(loaded.description, "The entrance door to the level.");
    }

    #[test]
    fn empty_display_name_and_description_appear_in_json() {
        // display_name and description must always be present in serialized
        // output, even when empty, to satisfy canonical serialization.
        let entity = AuthoringEntity::new(EntityId::generate(), "unnamed");
        let json = serde_json::to_string(&entity).unwrap();

        assert!(
            json.contains("\"display_name\""),
            "display_name must appear even when empty: {json}"
        );
        assert!(
            json.contains("\"description\""),
            "description must appear even when empty: {json}"
        );
    }

    #[test]
    fn missing_display_name_or_description_is_rejected() {
        let missing_display_name = format!(
            r#"{{"id":"{}","name":"missing_display","description":"","components":{{}}}}"#,
            EntityId::generate().as_str()
        );
        assert!(serde_json::from_str::<AuthoringEntity>(&missing_display_name).is_err());

        let missing_description = format!(
            r#"{{"id":"{}","name":"missing_description","display_name":"","components":{{}}}}"#,
            EntityId::generate().as_str()
        );
        assert!(serde_json::from_str::<AuthoringEntity>(&missing_description).is_err());
    }

    #[test]
    fn entity_ref_component_survives_roundtrip_as_entity_ref() {
        let target_id = EntityId::generate();
        let mut components = BTreeMap::new();
        components.insert(
            ComponentTypeId::new("gameplay.target"),
            Value::EntityRef(target_id.clone()),
        );
        let entity = AuthoringEntity {
            id: EntityId::generate(),
            name: "enemy".into(),
            display_name: String::new(),
            description: String::new(),
            parent: None,
            enabled: true,
            components,
        };

        let json = serde_json::to_string(&entity).unwrap();
        let loaded: AuthoringEntity = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded
                .components
                .get(&ComponentTypeId::new("gameplay.target"))
                .unwrap(),
            &Value::EntityRef(target_id),
            "EntityRef must be preserved after roundtrip"
        );
    }

    #[test]
    fn asset_ref_component_survives_roundtrip_as_asset_ref() {
        let icon_id = AssetId::generate();
        let mut components = BTreeMap::new();
        components.insert(
            ComponentTypeId::new("gameplay.icon"),
            Value::AssetRef(icon_id.clone()),
        );
        let entity = AuthoringEntity {
            id: EntityId::generate(),
            name: "enemy".into(),
            display_name: String::new(),
            description: String::new(),
            parent: None,
            enabled: true,
            components,
        };

        let json = serde_json::to_string(&entity).unwrap();
        let loaded: AuthoringEntity = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded
                .components
                .get(&ComponentTypeId::new("gameplay.icon"))
                .unwrap(),
            &Value::AssetRef(icon_id),
            "AssetRef must be preserved after roundtrip"
        );
    }

    #[test]
    fn enabled_defaults_to_true_and_is_omitted_from_json_when_true() {
        let entity = AuthoringEntity::new(EntityId::generate(), "guard");
        let json = serde_json::to_string(&entity).unwrap();
        assert!(
            !json.contains("\"enabled\""),
            "enabled must be omitted while true in canonical output: {json}"
        );

        let current_without_enabled = format!(
            r#"{{"id":"{}","name":"guard","display_name":"","description":"","components":{{}}}}"#,
            EntityId::generate().as_str()
        );
        let loaded: AuthoringEntity = serde_json::from_str(&current_without_enabled).unwrap();
        assert!(loaded.enabled, "missing enabled key must load as true");
    }

    #[test]
    fn disabled_entity_serializes_the_flag_and_roundtrips() {
        let mut entity = AuthoringEntity::new(EntityId::generate(), "guard");
        entity.enabled = false;
        let json = serde_json::to_string(&entity).unwrap();
        assert!(
            json.contains("\"enabled\":false"),
            "disabled flag must be serialized: {json}"
        );
        let loaded: AuthoringEntity = serde_json::from_str(&json).unwrap();
        assert!(!loaded.enabled);
    }

    #[test]
    fn entity_without_parent_omits_parent_field_in_json() {
        let entity = AuthoringEntity::new(EntityId::generate(), "root_entity");
        let json = serde_json::to_string(&entity).unwrap();
        assert!(
            !json.contains("\"parent\""),
            "parent field should be absent when None: {json}"
        );
    }

    #[test]
    fn entity_with_parent_includes_parent_field_in_json() {
        let parent_id = EntityId::generate();
        let mut entity = AuthoringEntity::new(EntityId::generate(), "child_entity");
        entity.parent = Some(parent_id.clone());
        let json = serde_json::to_string(&entity).unwrap();
        assert!(
            json.contains("\"parent\""),
            "parent field must be present: {json}"
        );
        assert!(
            json.contains(parent_id.as_str()),
            "parent id must appear in json: {json}"
        );
    }

    #[test]
    fn component_keys_are_sorted_in_serialized_output() {
        let mut components = BTreeMap::new();
        components.insert(ComponentTypeId::new("z.transform"), Value::Null);
        components.insert(ComponentTypeId::new("a.health"), Value::Null);
        components.insert(ComponentTypeId::new("m.render"), Value::Null);
        let entity = AuthoringEntity {
            id: EntityId::generate(),
            name: "test".into(),
            display_name: String::new(),
            description: String::new(),
            parent: None,
            enabled: true,
            components,
        };
        let json = serde_json::to_string(&entity).unwrap();
        let a_pos = json.find("a.health").unwrap();
        let m_pos = json.find("m.render").unwrap();
        let z_pos = json.find("z.transform").unwrap();
        assert!(
            a_pos < m_pos && m_pos < z_pos,
            "component keys must be sorted: {json}"
        );
    }
}
