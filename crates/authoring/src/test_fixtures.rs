//! Opt-in scene fixtures shared by cross-crate authoring-to-runtime tests.
//!
//! The current serialized scene format requires a document `schema_version`
//! and, for every entity, the `display_name` and `description` labels. Those
//! values never vary between conversion fixtures, so tests write only the
//! entities and components under test and complete the document here.

use serde_json::Value as JsonValue;

use crate::load::{load_scene_from_json, SceneLoadError};
use crate::scene::{AuthoringScene, SCENE_SCHEMA_VERSION};

/// Completes a partial scene document and returns its serialized text.
///
/// Only missing keys are inserted, so a fixture that asserts on
/// `schema_version`, `display_name`, or `description` keeps its own values.
/// Use this when a test needs the document text itself, for example to write a
/// scene file; [`load_scene_fixture`] covers the common in-memory case.
///
/// # Errors
///
/// Returns [`SceneLoadError::Json`] when `json` is not valid JSON.
///
/// # Examples
///
/// ```
/// use engine_authoring::test_fixtures::complete_scene_document;
///
/// let document = complete_scene_document(
///     r#"{"entities":[{"id":"entity_01JP0000000000000000000001","name":"a","components":{}}]}"#,
/// )
/// .expect("fixture must be valid JSON");
/// assert!(document.contains("display_name"));
/// ```
pub fn complete_scene_document(json: &str) -> Result<String, SceneLoadError> {
    let mut document: JsonValue = serde_json::from_str(json).map_err(SceneLoadError::Json)?;
    if let Some(root) = document.as_object_mut() {
        root.entry("schema_version")
            .or_insert_with(|| JsonValue::from(SCENE_SCHEMA_VERSION));
        if let Some(entities) = root.get_mut("entities").and_then(JsonValue::as_array_mut) {
            for entity in entities.iter_mut().filter_map(JsonValue::as_object_mut) {
                entity
                    .entry("display_name")
                    .or_insert_with(|| JsonValue::from(""));
                entity
                    .entry("description")
                    .or_insert_with(|| JsonValue::from(""));
            }
        }
    }
    Ok(document.to_string())
}

/// Loads a partial scene fixture through the current scene loader.
///
/// The fixture is completed by [`complete_scene_document`] and then parsed by
/// [`load_scene_from_json`], so a fixture that violates the current format in
/// any other way still fails exactly as production data would.
///
/// # Errors
///
/// Returns the [`SceneLoadError`] reported by [`load_scene_from_json`], or
/// [`SceneLoadError::Json`] when `json` is not valid JSON.
///
/// # Examples
///
/// ```
/// use engine_authoring::test_fixtures::load_scene_fixture;
///
/// let scene = load_scene_fixture(
///     r#"{"entities":[{"id":"entity_01JP0000000000000000000001","name":"a","components":{}}]}"#,
/// )
/// .expect("fixture must load");
/// assert_eq!(scene.entities().count(), 1);
/// ```
pub fn load_scene_fixture(json: &str) -> Result<AuthoringScene, SceneLoadError> {
    load_scene_from_json(&complete_scene_document(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTIAL: &str = r#"{
        "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "player",
            "components": {}
        }]
    }"#;

    #[test]
    fn partial_fixture_gains_the_current_envelope_and_entity_labels() {
        let scene = load_scene_fixture(PARTIAL).expect("fixture must load");

        let (_, entity) = scene.entities().next().expect("fixture entity");
        assert_eq!(entity.name, "player");
        assert!(entity.display_name.is_empty());
        assert!(entity.description.is_empty());
    }

    #[test]
    fn authored_labels_are_never_replaced() {
        let scene = load_scene_fixture(
            r#"{
                "entities": [{
                    "id": "entity_01JP0000000000000000000001",
                    "name": "player",
                    "display_name": "Player",
                    "description": "The player entity.",
                    "components": {}
                }]
            }"#,
        )
        .expect("fixture must load");

        let (_, entity) = scene.entities().next().expect("fixture entity");
        assert_eq!(entity.display_name, "Player");
        assert_eq!(entity.description, "The player entity.");
    }

    #[test]
    fn a_fixture_that_is_not_valid_json_reports_a_load_error() {
        assert!(matches!(
            load_scene_fixture("{"),
            Err(SceneLoadError::Json(_))
        ));
    }
}
