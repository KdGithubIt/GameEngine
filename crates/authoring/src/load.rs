//! Scene loading from serialized JSON.
//!
//! This module provides a minimal read path from a JSON representation of an
//! [`AuthoringScene`] back into a live scene object. It is intentionally
//! limited to parsing and basic structural validation: full semantic schema
//! validation is not in scope for Phase 2.5.
//!
//! The JSON format is a flat array of [`AuthoringEntity`] objects with a
//! required current `schema_version` field:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "entities": [
//!     {
//!       "id": "entity_01JP0000000000000000000001",
//!       "name": "player",
//!       "display_name": "Player",
//!       "description": "The player-controlled entity.",
//!       "components": {
//!         "engine.player_marker": null,
//!         "engine.transform": { "x": -0.5, "y": 0.0, "z": 0.0 }
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! A missing or non-current `schema_version` is rejected. Entity
//! `display_name` and `description` are required even when empty. The `parent`
//! field remains optional because the current writer omits it for root entities.
//!
//! [`SCENE_SCHEMA_VERSION`]: crate::scene::SCENE_SCHEMA_VERSION

use crate::entity::AuthoringEntity;
use crate::id::EntityId;
use crate::scene::{AuthoringScene, SCENE_SCHEMA_VERSION};
use serde::Deserialize;
use std::fmt;

/// Describes why [`load_scene_from_json`] failed.
#[derive(Debug)]
pub enum SceneLoadError {
    /// The input is not valid JSON, or a required field has the wrong type.
    Json(serde_json::Error),
    /// The input contains two entities with the same stable ID.
    DuplicateEntityId(EntityId),
    /// The `schema_version` in the file is not the current version.
    UnsupportedVersion {
        /// The version number found in the file.
        found: u32,
        /// The current version this build can load.
        supported: u32,
    },
}

impl fmt::Display for SceneLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "scene JSON is invalid: {e}"),
            Self::DuplicateEntityId(id) => {
                write!(f, "scene contains duplicate entity ID: {id}")
            }
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "scene schema_version {found} is not supported by this build (expected: {supported})"
            ),
        }
    }
}

impl std::error::Error for SceneLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::DuplicateEntityId(_) | Self::UnsupportedVersion { .. } => None,
        }
    }
}

impl From<serde_json::Error> for SceneLoadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Intermediate deserialization target — not part of the public API.
#[derive(Deserialize)]
struct SceneFile {
    schema_version: u32,
    entities: Vec<AuthoringEntity>,
}

/// Parses `json` and returns an [`AuthoringScene`] containing the described entities.
///
/// `schema_version` is required and must equal [`SCENE_SCHEMA_VERSION`].
///
/// # Errors
///
/// - [`SceneLoadError::Json`] when `json` is not valid JSON or a required
///   field is missing or has the wrong type.
/// - [`SceneLoadError::DuplicateEntityId`] when two entities share the same
///   stable ID.
/// - [`SceneLoadError::UnsupportedVersion`] when `schema_version` differs from
///   [`SCENE_SCHEMA_VERSION`].
///
/// # Notes
///
/// This function never panics on unexpected input. All structural errors are
/// returned as [`SceneLoadError`] values. Full semantic schema validation is
/// not performed.
///
/// [`SCENE_SCHEMA_VERSION`]: crate::scene::SCENE_SCHEMA_VERSION
pub fn load_scene_from_json(json: &str) -> Result<AuthoringScene, SceneLoadError> {
    let file: SceneFile = serde_json::from_str(json)?;
    if file.schema_version != SCENE_SCHEMA_VERSION {
        return Err(SceneLoadError::UnsupportedVersion {
            found: file.schema_version,
            supported: SCENE_SCHEMA_VERSION,
        });
    }
    let mut scene = AuthoringScene::new();
    for entity in file.entities {
        if scene.contains_entity(&entity.id) {
            return Err(SceneLoadError::DuplicateEntityId(entity.id));
        }
        scene.insert_entity(entity);
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid JSON used across multiple tests.
    const VALID_JSON: &str = r#"{
        "schema_version": 1,
        "entities": [
            {
                "id": "entity_01JP0000000000000000000001",
                "name": "player",
                "display_name": "Player",
                "description": "The player entity.",
                "components": {
                    "engine.player_marker": null,
                    "engine.transform": { "x": -0.5, "y": 0.0, "z": 0.0 }
                }
            },
            {
                "id": "entity_01JP0000000000000000000002",
                "name": "obstacle",
                "display_name": "Obstacle",
                "description": "A static obstacle.",
                "components": {
                    "engine.transform": { "x": 0.5, "y": 0.0, "z": 0.0 }
                }
            }
        ]
    }"#;

    #[test]
    fn valid_scene_json_produces_correct_entity_count() {
        let scene = load_scene_from_json(VALID_JSON).expect("valid JSON must parse");
        assert_eq!(scene.entity_count(), 2);
    }

    #[test]
    fn valid_scene_json_preserves_entity_names() {
        let scene = load_scene_from_json(VALID_JSON).expect("valid JSON must parse");
        let names: Vec<&str> = scene.entities().map(|(_, e)| e.name.as_str()).collect();
        assert!(names.contains(&"player"));
        assert!(names.contains(&"obstacle"));
    }

    #[test]
    fn missing_required_entity_metadata_is_rejected() {
        let missing_display_name = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "minimal",
                    "description": "",
                    "components": {}
                }
            ]
        }"#;
        assert!(matches!(
            load_scene_from_json(missing_display_name),
            Err(SceneLoadError::Json(_))
        ));

        let missing_description = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "minimal",
                    "display_name": "",
                    "components": {}
                }
            ]
        }"#;
        assert!(matches!(
            load_scene_from_json(missing_description),
            Err(SceneLoadError::Json(_))
        ));
    }

    #[test]
    fn empty_entities_array_produces_empty_scene() {
        let json = r#"{ "schema_version": 1, "entities": [] }"#;
        let scene = load_scene_from_json(json).expect("empty array must parse");
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn malformed_json_returns_json_error() {
        let result = load_scene_from_json("{not valid json");
        assert!(
            matches!(result, Err(SceneLoadError::Json(_))),
            "malformed JSON must return SceneLoadError::Json"
        );
    }

    #[test]
    fn wrong_top_level_type_returns_json_error() {
        // A JSON array instead of an object is a schema error.
        let result = load_scene_from_json(r#"[]"#);
        assert!(
            matches!(result, Err(SceneLoadError::Json(_))),
            "non-object root must return SceneLoadError::Json"
        );
    }

    #[test]
    fn missing_required_id_field_returns_json_error() {
        let json = r#"{
            "schema_version": 1,
            "entities": [{
                "name": "no_id",
                "display_name": "",
                "description": "",
                "components": {}
            }]
        }"#;
        let result = load_scene_from_json(json);
        assert!(
            matches!(result, Err(SceneLoadError::Json(_))),
            "missing id must return SceneLoadError::Json"
        );
    }

    #[test]
    fn duplicate_entity_id_returns_error() {
        let json = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "first",
                    "display_name": "",
                    "description": "",
                    "components": {}
                },
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "duplicate",
                    "display_name": "",
                    "description": "",
                    "components": {}
                }
            ]
        }"#;
        let result = load_scene_from_json(json);
        assert!(
            matches!(result, Err(SceneLoadError::DuplicateEntityId(_))),
            "duplicate IDs must return SceneLoadError::DuplicateEntityId"
        );
    }

    #[test]
    fn component_values_survive_json_roundtrip() {
        let scene = load_scene_from_json(VALID_JSON).expect("valid JSON must parse");
        let player = scene
            .entities()
            .find(|(_, e)| e.name == "player")
            .map(|(_, e)| e)
            .expect("player must exist");

        use crate::id::ComponentTypeId;
        use crate::value::Value;

        assert!(
            matches!(
                player
                    .components
                    .get(&ComponentTypeId::new("engine.player_marker")),
                Some(Value::Null)
            ),
            "engine.player_marker must deserialize as Value::Null"
        );

        if let Some(Value::Object(obj)) = player
            .components
            .get(&ComponentTypeId::new("engine.transform"))
        {
            if let Some(Value::F64(x)) = obj.get("x") {
                assert!(
                    (*x - (-0.5)).abs() < f64::EPSILON,
                    "transform x must be -0.5, got {x}"
                );
            } else {
                panic!("x must be Value::F64");
            }
        } else {
            panic!("engine.transform must be Value::Object");
        }
    }

    #[test]
    fn load_validate_save_load_roundtrip_preserves_scene_semantics() {
        let scene = load_scene_from_json(VALID_JSON).expect("valid JSON must parse");
        assert!(scene.validate().is_empty());

        let saved = scene
            .to_canonical_json()
            .expect("valid scene must serialize");
        let loaded = load_scene_from_json(&saved).expect("saved JSON must load");

        assert!(loaded.validate().is_empty());
        assert_eq!(scene.entity_count(), loaded.entity_count());
        for (id, entity) in scene.entities() {
            assert_eq!(loaded.entity(id), Some(entity));
        }
    }

    #[test]
    fn unknown_component_type_is_preserved_across_roundtrip() {
        let json = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "future",
                    "display_name": "",
                    "description": "",
                    "components": {
                        "future.camera": { "enabled": true }
                    }
                }
            ]
        }"#;
        let scene = load_scene_from_json(json).expect("scene must parse");
        let saved = scene
            .to_canonical_json()
            .expect("scene with unknown component must serialize");
        let loaded = load_scene_from_json(&saved).expect("saved scene must parse");
        let entity = loaded
            .entities()
            .find(|(_, entity)| entity.name == "future")
            .map(|(_, entity)| entity)
            .expect("future entity must exist");

        assert!(entity
            .components
            .contains_key(&crate::id::ComponentTypeId::new("future.camera")));
    }

    #[test]
    fn load_without_schema_version_is_rejected() {
        let json = r#"{ "entities": [] }"#;
        assert!(
            matches!(load_scene_from_json(json), Err(SceneLoadError::Json(_))),
            "missing schema_version must be rejected"
        );
    }

    #[test]
    fn load_with_explicit_current_schema_version_succeeds() {
        let json = r#"{ "schema_version": 1, "entities": [] }"#;
        assert!(
            load_scene_from_json(json).is_ok(),
            "current schema_version must succeed"
        );
    }

    #[test]
    fn load_non_current_schema_version_returns_unsupported_version_error() {
        for found in [0, 99] {
            let json = format!(r#"{{ "schema_version": {found}, "entities": [] }}"#);
            assert!(matches!(
                load_scene_from_json(&json),
                Err(SceneLoadError::UnsupportedVersion {
                    found: actual,
                    supported: _
                }) if actual == found
            ));
        }
    }

    #[test]
    fn unsupported_version_error_displays_found_and_supported() {
        let json = r#"{ "schema_version": 42, "entities": [] }"#;
        let err = load_scene_from_json(json).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("42"),
            "error message must include found version: {msg}"
        );
    }

    #[test]
    fn roundtrip_with_schema_version_field_preserves_entities() {
        let scene = load_scene_from_json(VALID_JSON).expect("valid JSON must parse");
        let saved = scene
            .to_canonical_json()
            .expect("valid scene must serialize");
        assert!(
            saved.contains("\"schema_version\""),
            "saved JSON must contain schema_version: {saved}"
        );
        let reloaded = load_scene_from_json(&saved).expect("saved JSON must reload");
        assert_eq!(scene.entity_count(), reloaded.entity_count());
    }

    #[test]
    fn typed_entity_id_validation_rejects_invalid_scene_id() {
        let json = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "asset_01JP0000000000000000000001",
                    "name": "bad",
                    "display_name": "",
                    "description": "",
                    "components": {}
                }
            ]
        }"#;
        assert!(matches!(
            load_scene_from_json(json),
            Err(SceneLoadError::Json(_))
        ));
    }

    #[test]
    fn component_type_validation_rejects_invalid_scene_key() {
        let json = r#"{
            "schema_version": 1,
            "entities": [
                {
                    "id": "entity_01JP0000000000000000000001",
                    "name": "bad",
                    "display_name": "",
                    "description": "",
                    "components": { "Not.Valid": null }
                }
            ]
        }"#;
        assert!(matches!(
            load_scene_from_json(json),
            Err(SceneLoadError::Json(_))
        ));
    }
}
