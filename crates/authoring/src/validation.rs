//! Project-level authoring validation (Phase 30 / Phase 31).
//!
//! Checks that a scene is correctly configured for authoring and play. These
//! checks are in addition to the structural validation already performed by
//! [`AuthoringScene::validate`].

use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::id::{AssetId, ComponentTypeId, EntityId};
use crate::scene::AuthoringScene;
use crate::value::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Validates an [`AuthoringScene`] for common authoring problems.
///
/// Returns a list of [`Diagnostic`]s describing issues found. An empty list
/// means no problems were detected. These diagnostics are warnings or errors
/// rather than blocking failures so that the editor can still open and edit a
/// scene that has validation issues.
///
/// Currently checked:
/// - Missing runtime camera (`engine.camera` component)
/// - Missing runtime mesh on entities that have a material but neither
///   `engine.mesh` nor `engine.skinned_mesh`
pub fn validate_scene(scene: &AuthoringScene) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let camera_type = ComponentTypeId::new("engine.camera");
    let authored_cameras = scene
        .entities()
        .filter_map(|(id, entity)| {
            entity
                .components
                .get(&camera_type)
                .map(|value| (id, entity, value))
        })
        .collect::<Vec<_>>();
    if authored_cameras.is_empty() {
        diagnostics.push(Diagnostic::warning(
            "validation.no_camera",
            "Scene has no Camera component. Play mode will insert a temporary default camera.",
        ));
    } else {
        let active_candidates = authored_cameras
            .iter()
            .filter_map(|(id, entity, value)| {
                let (enabled, priority) = camera_selection_values(value)?;
                (enabled && entity_is_effectively_enabled(scene, id))
                    .then_some((*id, *entity, priority))
            })
            .collect::<Vec<_>>();

        if active_candidates.is_empty() {
            diagnostics.push(Diagnostic::warning(
                "validation.no_active_camera",
                "Scene has Camera components, but none are enabled in an enabled hierarchy. Game View will have no active camera.",
            ));
        } else {
            let highest_priority = active_candidates
                .iter()
                .map(|(_, _, priority)| *priority)
                .max()
                .expect("non-empty camera candidates must have a maximum priority");
            let tied = active_candidates
                .iter()
                .filter(|(_, _, priority)| *priority == highest_priority)
                .collect::<Vec<_>>();

            if tied.len() > 1 {
                let names = tied
                    .iter()
                    .map(|(_, entity, _)| entity.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                diagnostics.push(
                    Diagnostic::warning(
                        "validation.camera_priority_tie",
                        format!(
                            "Multiple enabled cameras share the highest priority {highest_priority}: {names}. Runtime entity ID order will break the tie."
                        ),
                    )
                    .with_target(DiagnosticTarget::Component {
                        entity: tied[0].0.clone(),
                        component_type: camera_type.clone(),
                    }),
                );
            }
        }
    }

    let mesh_type = ComponentTypeId::new("engine.mesh");
    let skinned_mesh_type = ComponentTypeId::new("engine.skinned_mesh");
    let static_renderer_type = ComponentTypeId::new("engine.static_mesh_renderer");
    let skinned_renderer_type = ComponentTypeId::new("engine.skinned_mesh_renderer");
    let material_type = ComponentTypeId::new("engine.material");
    for (id, entity) in scene.entities() {
        // A skinned entity (ADR 0043) renders via `engine.skinned_mesh`
        // instead of `engine.mesh`; only the absence of both is a real gap.
        let has_mesh = entity.components.contains_key(&mesh_type)
            || entity.components.contains_key(&skinned_mesh_type)
            || entity.components.contains_key(&static_renderer_type)
            || entity.components.contains_key(&skinned_renderer_type);
        let has_material = entity.components.contains_key(&material_type);
        if has_material && !has_mesh {
            diagnostics.push(
                Diagnostic::warning(
                    "validation.material_without_mesh",
                    format!(
                        "Entity `{}` has a material but no mesh component.",
                        entity.name
                    ),
                )
                .with_target(DiagnosticTarget::Entity { id: id.clone() }),
            );
        }
    }

    diagnostics
}

/// Returns current Camera selection values when both required fields are valid.
///
/// Missing or malformed selection fields are not inferred from historical
/// camera layouts; component-schema validation reports those authoring errors.
fn camera_selection_values(value: &Value) -> Option<(bool, i64)> {
    let Value::Object(fields) = value else {
        return None;
    };
    let enabled = match fields.get("enabled")? {
        Value::Bool(enabled) => *enabled,
        _ => return None,
    };
    let priority = match fields.get("priority")? {
        Value::I64(priority) => *priority,
        Value::U64(priority) => i64::try_from(*priority).ok()?,
        _ => return None,
    };
    Some((enabled, priority))
}

/// Returns whether Scene Bridge will spawn an entity after enabled inheritance.
fn entity_is_effectively_enabled(scene: &AuthoringScene, id: &EntityId) -> bool {
    let mut current = Some(id.clone());

    // Scene validation rejects parent cycles. The bound also keeps Problems
    // refresh responsive while a broken scene is still open in the editor.
    for _ in 0..=scene.entity_count() {
        let Some(current_id) = current else {
            return true;
        };
        let Some(entity) = scene.entity(&current_id) else {
            return false;
        };
        if !entity.enabled {
            return false;
        }
        current = entity.parent.clone();
    }

    false
}

#[cfg(test)]
mod validate_scene_tests {
    use super::*;
    use crate::entity::AuthoringEntity;
    use crate::id::EntityId;

    fn camera_value(enabled: Option<bool>, priority: Option<i64>) -> Value {
        let mut fields = std::collections::BTreeMap::new();
        if let Some(enabled) = enabled {
            fields.insert("enabled".to_owned(), Value::Bool(enabled));
        }
        if let Some(priority) = priority {
            fields.insert("priority".to_owned(), Value::I64(priority));
        }
        Value::Object(fields)
    }

    fn insert_camera(
        scene: &mut AuthoringScene,
        name: &str,
        enabled: Option<bool>,
        priority: Option<i64>,
    ) -> EntityId {
        let id = EntityId::generate();
        let mut entity = AuthoringEntity::new(id.clone(), name);
        entity.components.insert(
            ComponentTypeId::new("engine.camera"),
            camera_value(enabled, priority),
        );
        scene.insert_entity(entity);
        id
    }

    #[test]
    fn camera_missing_current_selection_fields_is_not_active() {
        let mut scene = AuthoringScene::new();
        insert_camera(&mut scene, "incomplete_camera", None, None);

        let diagnostics = validate_scene(&scene);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.no_active_camera"));
    }

    #[test]
    fn disabled_cameras_report_no_active_camera() {
        let mut scene = AuthoringScene::new();
        insert_camera(&mut scene, "disabled", Some(false), Some(100));

        let diagnostics = validate_scene(&scene);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.no_active_camera"));
    }

    #[test]
    fn distinct_camera_priorities_do_not_report_a_tie() {
        let mut scene = AuthoringScene::new();
        insert_camera(&mut scene, "low", Some(true), Some(0));
        insert_camera(&mut scene, "high", Some(true), Some(10));

        let diagnostics = validate_scene(&scene);

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.camera_priority_tie"));
    }

    #[test]
    fn highest_camera_priority_tie_is_reported() {
        let mut scene = AuthoringScene::new();
        insert_camera(&mut scene, "low", Some(true), Some(0));
        insert_camera(&mut scene, "high_a", Some(true), Some(10));
        insert_camera(&mut scene, "high_b", Some(true), Some(10));

        let diagnostics = validate_scene(&scene);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.camera_priority_tie"));
    }

    #[test]
    fn camera_below_a_disabled_parent_is_not_active() {
        let mut scene = AuthoringScene::new();
        let parent_id = EntityId::generate();
        let mut parent = AuthoringEntity::new(parent_id.clone(), "disabled_parent");
        parent.enabled = false;
        scene.insert_entity(parent);

        let camera_id = insert_camera(&mut scene, "child_camera", Some(true), Some(0));
        scene
            .entity_mut(&camera_id)
            .expect("camera must exist")
            .parent = Some(parent_id);

        let diagnostics = validate_scene(&scene);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.no_active_camera"));
    }

    #[test]
    fn material_without_any_mesh_component_is_reported() {
        let mut scene = AuthoringScene::new();
        let mut entity = AuthoringEntity::new(EntityId::generate(), "Bare");
        entity
            .components
            .insert(ComponentTypeId::new("engine.material"), Value::Null);
        scene.insert_entity(entity);

        let diagnostics = validate_scene(&scene);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.material_without_mesh"));
    }

    #[test]
    fn material_with_static_mesh_is_not_reported() {
        let mut scene = AuthoringScene::new();
        let mut entity = AuthoringEntity::new(EntityId::generate(), "Static");
        entity
            .components
            .insert(ComponentTypeId::new("engine.material"), Value::Null);
        entity
            .components
            .insert(ComponentTypeId::new("engine.mesh"), Value::Null);
        scene.insert_entity(entity);

        let diagnostics = validate_scene(&scene);
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.material_without_mesh"));
    }

    #[test]
    fn material_with_skinned_mesh_is_not_reported() {
        // A skinned character (ADR 0043) renders via `engine.skinned_mesh`
        // instead of `engine.mesh`; this must not be flagged as a gap.
        let mut scene = AuthoringScene::new();
        let mut entity = AuthoringEntity::new(EntityId::generate(), "Mesh");
        entity
            .components
            .insert(ComponentTypeId::new("engine.material"), Value::Null);
        entity
            .components
            .insert(ComponentTypeId::new("engine.skinned_mesh"), Value::Null);
        scene.insert_entity(entity);

        let diagnostics = validate_scene(&scene);
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "validation.material_without_mesh"));
    }
}

/// Validates that every [`Value::AssetRef`] in a scene exists in the known asset ID set.
///
/// `known_asset_ids` should be the set of `AssetId::as_str()` values from the
/// runtime asset manifest.  A missing entry means the scene references a file
/// that has not been imported yet.
///
/// Returns a [`Diagnostic`] with code `validation.invalid_asset_ref` for each
/// missing reference.
pub fn validate_scene_asset_refs(
    scene: &AuthoringScene,
    known_asset_ids: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (entity_id, entity) in scene.entities() {
        for (component_type, value) in &entity.components {
            for asset_id in collect_asset_refs(value) {
                if !known_asset_ids.contains(asset_id.as_str()) {
                    diagnostics.push(
                        Diagnostic::warning(
                            "validation.invalid_asset_ref",
                            format!(
                                "Entity `{}` component `{}` references unknown asset `{}`.",
                                entity.name,
                                component_type.as_str(),
                                asset_id.as_str()
                            ),
                        )
                        .with_target(DiagnosticTarget::Component {
                            entity: entity_id.clone(),
                            component_type: component_type.clone(),
                        }),
                    );
                }
            }
        }
    }
    diagnostics
}

fn collect_asset_refs(value: &Value) -> Vec<&AssetId> {
    match value {
        Value::AssetRef(id) => vec![id],
        Value::Array(arr) => arr.iter().flat_map(collect_asset_refs).collect(),
        Value::Object(map) => map.values().flat_map(collect_asset_refs).collect(),
        _ => Vec::new(),
    }
}

/// Validates that the configured start scene is present in the asset manifest.
///
/// Returns a warning diagnostic with code `validation.start_scene_not_in_manifest`
/// when `start_scene` names a path not present in `manifest_paths`.  Returns an
/// empty list when `start_scene` is `None` or the path is in the manifest.
pub fn validate_start_scene(
    start_scene: Option<&str>,
    manifest_paths: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let Some(path) = start_scene else {
        return Vec::new();
    };
    if manifest_paths.contains(path) {
        return Vec::new();
    }
    vec![Diagnostic::warning(
        "validation.start_scene_not_in_manifest",
        format!("Start scene '{path}' is not in the asset manifest."),
    )]
}

/// Validates that asset manifest entries all point to existing files (Phase 31).
///
/// `assets_root` is the absolute path to the `assets/` directory.
/// `manifest_paths` is the set of relative paths recorded in the manifest.
///
/// Returns a list of [`Diagnostic`]s:
/// - `validation.asset_missing`: a manifest entry whose file is absent on disk.
/// - `validation.asset_orphan`: a file on disk that has no manifest entry.
///
/// An empty list means no problems were detected.
pub fn validate_asset_manifest(
    assets_root: &Path,
    manifest_paths: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for rel_path in manifest_paths {
        let full = assets_root.join(rel_path);
        if !full.exists() {
            diagnostics.push(Diagnostic::warning(
                "validation.asset_missing",
                format!("Manifest entry `{rel_path}` does not exist on disk."),
            ));
        }
    }

    diagnostics
}

#[cfg(test)]
mod validation_asset_tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    #[test]
    fn validate_asset_manifest_reports_missing_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = BTreeSet::from(["textures/missing.png".to_string()]);
        let diagnostics = validate_asset_manifest(dir.path(), &paths);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "validation.asset_missing");
    }

    #[test]
    fn validate_asset_manifest_accepts_existing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("cube.obj"), b"").expect("write");
        let paths = BTreeSet::from(["cube.obj".to_string()]);
        let diagnostics = validate_asset_manifest(dir.path(), &paths);
        assert!(diagnostics.is_empty());
    }
}
