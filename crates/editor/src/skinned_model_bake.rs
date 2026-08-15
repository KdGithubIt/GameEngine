//! Editor-only conversion of a Skinned Model into author-owned static meshes.
//!
//! The runtime engine owns the geometry primitives used here, while this
//! module owns project I/O and authoring commands. Keeping that boundary means
//! the runtime ECS never learns about editor projects or persisted manifests
//! (ADR 0089).

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use engine::{
    AssetManifest, ImportSettings, ImportedSubAssetKind, ManifestEntry, Mesh, ModelImportError,
};
use engine_authoring::{
    replace_file_contents, AssetId, AuthoringCommand, AuthoringScene, ComponentTypeId, EntityId,
    ProjectRoot, Transaction, Value,
};

use crate::session::{EditorSession, EditorSessionError};

/// Stable diagnostic code used when a configured controller blocks baking.
pub const CONFIGURED_CONTROLLER_DIAGNOSTIC: &str = "editor.skinned_model_bake_configured";

/// Stable diagnostic code used when a bone attachment blocks baking.
pub const BONE_ATTACHMENT_DIAGNOSTIC: &str = "editor.skinned_model_bake_attachment";

/// Summary of one completed Skinned Model bake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinnedModelBakeResult {
    /// Number of author-owned OBJ assets created.
    pub baked_meshes: usize,
    /// Number of original Skinned Mesh Renderer entities converted.
    pub render_parts: usize,
    /// Project-relative paths of the created OBJ assets.
    pub output_paths: Vec<PathBuf>,
}

/// Failure reported while planning or committing a Skinned Model bake.
#[derive(Debug)]
pub enum SkinnedModelBakeError {
    /// The active document is not a scene or the target entity disappeared.
    MissingModel(EntityId),
    /// The selected entity does not carry a valid Skinned Model component.
    InvalidModel(EntityId),
    /// A configured Animation Controller still uses the rig.
    ConfiguredController(EntityId),
    /// A Bone Attachment still targets the rig.
    BoneAttachment {
        /// Entity carrying the blocking attachment.
        entity: EntityId,
    },
    /// A listed render part is missing or does not carry the expected renderer.
    InvalidRenderPart {
        /// Render part that could not be converted.
        entity: EntityId,
        /// Human-readable reason.
        reason: String,
    },
    /// A mesh reference could not be resolved back to an imported model source.
    UnresolvedMesh {
        /// Referenced mesh sub-asset.
        mesh: AssetId,
        /// Human-readable reason.
        reason: String,
    },
    /// Importing the source model failed.
    Import {
        /// Source path that failed to import.
        path: PathBuf,
        /// Importer error.
        source: ModelImportError,
    },
    /// The generated authoring command batch failed validation.
    InvalidCommands(String),
    /// A project path could not be resolved safely.
    Project(String),
    /// Serializing or writing the manifest failed.
    Manifest(String),
    /// Creating or writing a baked mesh failed.
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// I/O error.
        source: std::io::Error,
    },
    /// Applying the already-validated scene transaction failed.
    Scene(EditorSessionError),
    /// Applying the scene transaction failed and restoring project files also failed.
    Rollback {
        /// Original scene-transaction failure.
        scene: String,
        /// Rollback failure.
        rollback: String,
    },
}

impl fmt::Display for SkinnedModelBakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModel(entity) => {
                write!(formatter, "Skinned Model `{}` no longer exists", entity.as_str())
            }
            Self::InvalidModel(entity) => write!(
                formatter,
                "entity `{}` does not contain a valid Skinned Model",
                entity.as_str()
            ),
            Self::ConfiguredController(entity) => write!(
                formatter,
                "entity `{}` has an Animation Controller with an assigned graph; remove the graph before baking",
                entity.as_str()
            ),
            Self::BoneAttachment { entity } => write!(
                formatter,
                "Bone Attachment on entity `{}` still targets this rig; remove or retarget it before baking",
                entity.as_str()
            ),
            Self::InvalidRenderPart { entity, reason } => write!(
                formatter,
                "render part `{}` cannot be baked: {reason}",
                entity.as_str()
            ),
            Self::UnresolvedMesh { mesh, reason } => {
                write!(formatter, "mesh `{}` cannot be baked: {reason}", mesh.as_str())
            }
            Self::Import { path, source } => {
                write!(formatter, "could not import `{}` for baking: {source}", path.display())
            }
            Self::InvalidCommands(reason) => {
                write!(formatter, "bake command batch is invalid: {reason}")
            }
            Self::Project(reason) => write!(formatter, "bake project path failed: {reason}"),
            Self::Manifest(reason) => write!(formatter, "could not update asset manifest: {reason}"),
            Self::Io { path, source } => {
                write!(formatter, "could not write `{}`: {source}", path.display())
            }
            Self::Scene(source) => write!(formatter, "could not convert the scene: {source}"),
            Self::Rollback { scene, rollback } => write!(
                formatter,
                "scene conversion failed ({scene}) and project-file rollback also failed ({rollback})"
            ),
        }
    }
}

impl std::error::Error for SkinnedModelBakeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Import { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::Scene(source) => Some(source),
            Self::MissingModel(_)
            | Self::InvalidModel(_)
            | Self::ConfiguredController(_)
            | Self::BoneAttachment { .. }
            | Self::InvalidRenderPart { .. }
            | Self::UnresolvedMesh { .. }
            | Self::InvalidCommands(_)
            | Self::Project(_)
            | Self::Manifest(_)
            | Self::Rollback { .. } => None,
        }
    }
}

/// One OBJ file and manifest entry prepared before any project mutation.
struct PlannedMeshAsset {
    id: AssetId,
    relative_path: PathBuf,
    absolute_path: PathBuf,
    display_name: String,
    obj: String,
}

/// Fully validated conversion plan.
struct SkinnedModelBakePlan {
    commands: Vec<AuthoringCommand>,
    assets: Vec<PlannedMeshAsset>,
    render_parts: usize,
}

/// Converts `model` into static renderers and persists author-owned OBJ files.
///
/// Planning loads every referenced source mesh and validates the complete
/// authoring command batch before writing anything. OBJ files and the manifest
/// are then persisted, followed by one scene transaction. If persistence or
/// the scene transaction fails, newly-created files are removed and the old
/// manifest is restored.
///
/// # Errors
///
/// Returns [`SkinnedModelBakeError`] when a blocking rig dependency exists, a
/// source mesh cannot be resolved, project persistence fails, or the scene
/// command transaction cannot commit.
pub fn bake_skinned_model(
    project: &ProjectRoot,
    manifest: &mut AssetManifest,
    session: &mut EditorSession,
    model: &EntityId,
) -> Result<SkinnedModelBakeResult, SkinnedModelBakeError> {
    let scene = session
        .scene()
        .cloned()
        .ok_or_else(|| SkinnedModelBakeError::MissingModel(model.clone()))?;
    let plan = plan_skinned_model_bake(project, manifest, &scene, model)?;
    validate_commands(&scene, &plan.commands)?;

    let baked_directory = project.assets_root().join("baked_meshes");
    fs::create_dir_all(&baked_directory).map_err(|source| SkinnedModelBakeError::Io {
        path: baked_directory,
        source,
    })?;
    let manifest_path = project.path().join("asset_manifest.json");
    let old_manifest = manifest.clone();
    let old_manifest_json = old_manifest
        .to_canonical_json()
        .map_err(|error| SkinnedModelBakeError::Manifest(error.to_string()))?;
    let manifest_existed = manifest_path.exists();

    let mut next_manifest = old_manifest.clone();
    for asset in &plan.assets {
        next_manifest.insert(
            asset.id.clone(),
            ManifestEntry {
                path: path_to_manifest_string(&asset.relative_path)?,
                name: Some(asset.display_name.clone()),
                import_settings: ImportSettings::default(),
            },
        );
    }
    let next_manifest_json = next_manifest
        .to_canonical_json()
        .map_err(|error| SkinnedModelBakeError::Manifest(error.to_string()))?;

    let mut written = Vec::new();
    for asset in &plan.assets {
        if let Err(source) = replace_file_contents(&asset.absolute_path, &asset.obj) {
            cleanup_created_files(&written);
            return Err(SkinnedModelBakeError::Io {
                path: asset.absolute_path.clone(),
                source: std::io::Error::other(source.to_string()),
            });
        }
        written.push(asset.absolute_path.clone());
    }
    if let Err(source) = replace_file_contents(&manifest_path, &next_manifest_json) {
        cleanup_created_files(&written);
        return Err(SkinnedModelBakeError::Manifest(format!(
            "failed to write {}: {source}",
            manifest_path.display()
        )));
    }

    if let Err(scene_error) = session.apply_scene_commands(plan.commands) {
        let rollback = restore_project_files(
            &manifest_path,
            manifest_existed,
            &old_manifest_json,
            &written,
        );
        return match rollback {
            Ok(()) => Err(SkinnedModelBakeError::Scene(scene_error)),
            Err(rollback) => Err(SkinnedModelBakeError::Rollback {
                scene: scene_error.to_string(),
                rollback,
            }),
        };
    }

    *manifest = next_manifest;
    Ok(SkinnedModelBakeResult {
        baked_meshes: plan.assets.len(),
        render_parts: plan.render_parts,
        output_paths: plan
            .assets
            .into_iter()
            .map(|asset| asset.relative_path)
            .collect(),
    })
}

/// Builds every command and OBJ payload without mutating the project.
fn plan_skinned_model_bake(
    project: &ProjectRoot,
    manifest: &AssetManifest,
    scene: &AuthoringScene,
    model: &EntityId,
) -> Result<SkinnedModelBakePlan, SkinnedModelBakeError> {
    let model_type = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
    let controller_type =
        ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
    let attachment_type = ComponentTypeId::new(engine::scene_bridge::BONE_ATTACHMENT_COMPONENT);
    let skinned_renderer_type =
        ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
    let static_renderer_type =
        ComponentTypeId::new(engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT);
    let transform_type = ComponentTypeId::new("engine.transform");

    let model_entity = scene
        .entity(model)
        .ok_or_else(|| SkinnedModelBakeError::MissingModel(model.clone()))?;
    let Value::Object(_model_fields) = model_entity
        .components
        .get(&model_type)
        .ok_or_else(|| SkinnedModelBakeError::InvalidModel(model.clone()))?
    else {
        return Err(SkinnedModelBakeError::InvalidModel(model.clone()));
    };

    if model_entity
        .components
        .get(&controller_type)
        .is_some_and(controller_has_graph)
    {
        return Err(SkinnedModelBakeError::ConfiguredController(model.clone()));
    }
    if let Some((entity, _)) = scene.entities().find(|(_, candidate)| {
        matches!(
            candidate.components.get(&attachment_type),
            Some(Value::Object(fields))
                if fields.get("rig") == Some(&Value::EntityRef(model.clone()))
        )
    }) {
        return Err(SkinnedModelBakeError::BoneAttachment {
            entity: entity.clone(),
        });
    }

    let render_parts = scene
        .entities()
        .filter_map(|(entity_id, candidate)| {
            let Value::Object(renderer) = candidate.components.get(&skinned_renderer_type)? else {
                return None;
            };
            let referenced_model = renderer
                .get("model")
                .or_else(|| renderer.get("rig_source"))
                .or_else(|| renderer.get("skeleton"));
            matches!(
                referenced_model,
                Some(Value::EntityRef(referenced)) if referenced == model
            )
            .then(|| entity_id.clone())
        })
        .collect::<Vec<_>>();

    let existing_skeletons = manifest
        .iter()
        .flat_map(|(_, entry)| entry.import_settings.skeleton_records.iter().cloned())
        .collect::<Vec<_>>();
    let mut imports = BTreeMap::<AssetId, engine::GltfImportResult>::new();
    let mut commands = Vec::new();
    let mut assets = Vec::new();

    for part_id in &render_parts {
        let part =
            scene
                .entity(part_id)
                .ok_or_else(|| SkinnedModelBakeError::InvalidRenderPart {
                    entity: part_id.clone(),
                    reason: "the entity no longer exists".to_owned(),
                })?;
        if part.components.contains_key(&static_renderer_type) {
            return Err(SkinnedModelBakeError::InvalidRenderPart {
                entity: part_id.clone(),
                reason: "it already carries a Static Mesh Renderer".to_owned(),
            });
        }
        let Some(Value::Object(renderer)) = part.components.get(&skinned_renderer_type) else {
            return Err(SkinnedModelBakeError::InvalidRenderPart {
                entity: part_id.clone(),
                reason: "it does not carry a Skinned Mesh Renderer".to_owned(),
            });
        };
        let Some(Value::AssetRef(mesh_id)) = renderer.get("mesh") else {
            return Err(SkinnedModelBakeError::InvalidRenderPart {
                entity: part_id.clone(),
                reason: "its mesh reference is unassigned".to_owned(),
            });
        };
        let source_mesh = resolve_source_mesh(
            project,
            manifest,
            mesh_id,
            &existing_skeletons,
            &mut imports,
        )?;
        let ranges = source_mesh.draw_ranges();
        if ranges.is_empty() {
            return Err(SkinnedModelBakeError::InvalidRenderPart {
                entity: part_id.clone(),
                reason: "its imported mesh contains no drawable triangles".to_owned(),
            });
        }

        let base_name = if part.display_name.is_empty() {
            part.name.clone()
        } else {
            part.display_name.clone()
        };
        let mut baked_ids = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().copied().enumerate() {
            let baked = engine::extract_baked_submesh(&source_mesh, range);
            if baked.indices.as_ref().is_none_or(Vec::is_empty) {
                return Err(SkinnedModelBakeError::InvalidRenderPart {
                    entity: part_id.clone(),
                    reason: format!("submesh {} contains no valid triangle indices", index + 1),
                });
            }
            let id = AssetId::generate();
            let relative_path = PathBuf::from("baked_meshes").join(format!("{}.obj", id.as_str()));
            let absolute_path = project.assets_root().join(&relative_path);
            let display_name = if ranges.len() == 1 {
                format!("{base_name} Baked")
            } else {
                format!("{base_name} Baked {}", index + 1)
            };
            assets.push(PlannedMeshAsset {
                id: id.clone(),
                relative_path,
                absolute_path,
                display_name,
                obj: engine::mesh_to_obj(&baked),
            });
            baked_ids.push(id);
        }

        commands.push(AuthoringCommand::RemoveComponent {
            entity: part_id.clone(),
            component_type: skinned_renderer_type.clone(),
        });
        commands.push(AuthoringCommand::AddComponent {
            entity: part_id.clone(),
            component_type: static_renderer_type.clone(),
            value: static_renderer_value(baked_ids[0].clone(), material_for_submesh(renderer, 0)),
        });

        for (index, baked_id) in baked_ids.iter().enumerate().skip(1) {
            let sibling = EntityId::generate();
            commands.push(AuthoringCommand::CreateEntity {
                id: sibling.clone(),
                name: format!("{}_submesh_{}", part.name, index + 1),
                parent: part.parent.clone(),
            });
            if !part.display_name.is_empty() {
                commands.push(AuthoringCommand::SetEntityDisplayName {
                    entity: sibling.clone(),
                    display_name: format!("{} {}", part.display_name, index + 1),
                });
            }
            if let Some(transform) = part.components.get(&transform_type) {
                commands.push(AuthoringCommand::AddComponent {
                    entity: sibling.clone(),
                    component_type: transform_type.clone(),
                    value: transform.clone(),
                });
            }
            commands.push(AuthoringCommand::AddComponent {
                entity: sibling,
                component_type: static_renderer_type.clone(),
                value: static_renderer_value(
                    baked_id.clone(),
                    material_for_submesh(renderer, index),
                ),
            });
        }
    }

    if model_entity.components.contains_key(&controller_type) {
        commands.push(AuthoringCommand::RemoveComponent {
            entity: model.clone(),
            component_type: controller_type,
        });
    }
    commands.push(AuthoringCommand::RemoveComponent {
        entity: model.clone(),
        component_type: model_type,
    });

    Ok(SkinnedModelBakePlan {
        commands,
        assets,
        render_parts: render_parts.len(),
    })
}

/// Resolves an imported mesh sub-asset, importing each owning source once.
fn resolve_source_mesh(
    project: &ProjectRoot,
    manifest: &AssetManifest,
    mesh: &AssetId,
    existing_skeletons: &[engine::SkeletonRecord],
    imports: &mut BTreeMap<AssetId, engine::GltfImportResult>,
) -> Result<Mesh, SkinnedModelBakeError> {
    let (source_id, entry, sub_asset) =
        manifest
            .imported_sub_asset(mesh)
            .ok_or_else(|| SkinnedModelBakeError::UnresolvedMesh {
                mesh: mesh.clone(),
                reason: "it is not a registered imported sub-asset".to_owned(),
            })?;
    if sub_asset.kind != ImportedSubAssetKind::Mesh {
        return Err(SkinnedModelBakeError::UnresolvedMesh {
            mesh: mesh.clone(),
            reason: "the reference does not identify a Mesh sub-asset".to_owned(),
        });
    }
    if !imports.contains_key(source_id) {
        let path = project
            .resolve_asset(&entry.path)
            .map_err(|error| SkinnedModelBakeError::Project(error.to_string()))?;
        let imported =
            engine::import_model_path(source_id, &path, existing_skeletons).map_err(|source| {
                SkinnedModelBakeError::Import {
                    path: path.clone(),
                    source,
                }
            })?;
        imports.insert(source_id.clone(), imported);
    }
    imports
        .get(source_id)
        .and_then(|imported| {
            imported
                .meshes
                .iter()
                .find(|candidate| &candidate.id == mesh)
        })
        .map(|candidate| candidate.mesh.clone())
        .ok_or_else(|| SkinnedModelBakeError::UnresolvedMesh {
            mesh: mesh.clone(),
            reason: "reimporting its source did not produce this mesh".to_owned(),
        })
}

/// Returns whether a controller has active graph playback configured.
fn controller_has_graph(controller: &Value) -> bool {
    matches!(
        controller,
        Value::Object(fields) if matches!(fields.get("graph"), Some(Value::AssetRef(_)))
    )
}

/// Resolves one submesh's material, falling back to the renderer's base slot.
fn material_for_submesh(renderer: &BTreeMap<String, Value>, index: usize) -> AssetId {
    renderer
        .get("material_slots")
        .and_then(|slots| match slots {
            Value::Array(slots) => slots.get(index),
            _ => None,
        })
        .and_then(|material| match material {
            Value::AssetRef(material) => Some(material.clone()),
            _ => None,
        })
        .or_else(|| match renderer.get("material") {
            Some(Value::AssetRef(material)) => Some(material.clone()),
            _ => None,
        })
        .unwrap_or_else(builtin_white_material)
}

/// Builds the existing Static Mesh Renderer schema value for one OBJ asset.
fn static_renderer_value(mesh: AssetId, material: AssetId) -> Value {
    Value::Object(BTreeMap::from([
        ("mesh".to_owned(), Value::AssetRef(mesh)),
        ("material".to_owned(), Value::AssetRef(material)),
        ("material_slots".to_owned(), Value::Array(Vec::new())),
    ]))
}

/// Returns the built-in white material used by renderer schema defaults.
fn builtin_white_material() -> AssetId {
    AssetId::from_stable_id(engine_authoring::StableId::new(
        engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID,
    ))
    .expect("built-in white material ID must be valid")
}

/// Validates the full command batch against a cloned scene before project I/O.
fn validate_commands(
    scene: &AuthoringScene,
    commands: &[AuthoringCommand],
) -> Result<(), SkinnedModelBakeError> {
    let mut candidate = scene.clone();
    let mut transaction = Transaction::begin(&candidate);
    for command in commands {
        transaction.apply(command.clone());
    }
    transaction
        .commit(&mut candidate)
        .map_err(|error| SkinnedModelBakeError::InvalidCommands(error.to_string()))?;
    Ok(())
}

/// Converts a platform path to the manifest's forward-slash convention.
fn path_to_manifest_string(path: &Path) -> Result<String, SkinnedModelBakeError> {
    path.to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| {
            SkinnedModelBakeError::Project(format!(
                "path `{}` is not representable as UTF-8",
                path.display()
            ))
        })
}

/// Removes files created by the current bake attempt.
fn cleanup_created_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

/// Restores the manifest and removes all newly-created OBJ files.
fn restore_project_files(
    manifest_path: &Path,
    manifest_existed: bool,
    old_manifest_json: &str,
    written: &[PathBuf],
) -> Result<(), String> {
    cleanup_created_files(written);
    if manifest_existed {
        replace_file_contents(manifest_path, old_manifest_json)
            .map_err(|error| format!("failed to restore {}: {error}", manifest_path.display()))
    } else {
        match fs::remove_file(manifest_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "failed to remove newly-created {}: {error}",
                manifest_path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::test_fixtures::load_scene_fixture;
    use engine_authoring::{ProjectConfig, PROJECT_SCHEMA_VERSION};

    /// Creates a complete project fixture with one placed Skinned Model.
    fn skinned_project(
        multi_submesh: bool,
    ) -> (
        tempfile::TempDir,
        ProjectRoot,
        AssetManifest,
        EditorSession,
        EntityId,
        EntityId,
    ) {
        let directory = tempfile::tempdir().expect("temporary project");
        let project = ProjectRoot::create(
            directory.path(),
            ProjectConfig {
                name: "SkinnedBakeTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        let source_path = project.assets_root().join("character.gltf");
        let fixture_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skinned_motion.gltf");
        let mut source_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(fixture_path).expect("skinned fixture"))
                .expect("fixture JSON");
        if multi_submesh {
            let primitive = source_json["meshes"][0]["primitives"][0].clone();
            source_json["meshes"][0]["primitives"]
                .as_array_mut()
                .expect("primitive array")
                .push(primitive);
        }
        fs::write(
            &source_path,
            serde_json::to_string_pretty(&source_json).expect("fixture serialization"),
        )
        .expect("source fixture");

        let source_id = AssetId::generate();
        let imported =
            engine::import_model_path(&source_id, &source_path, &[]).expect("fixture import");
        let mut manifest = AssetManifest::default();
        manifest.insert(
            source_id,
            ManifestEntry {
                path: "character.gltf".into(),
                name: Some("character".into()),
                import_settings: ImportSettings {
                    sub_assets: imported.imported_sub_assets(),
                    skeleton_records: imported.skeleton_records.clone(),
                    ..ImportSettings::default()
                },
            },
        );
        fs::write(
            project.path().join("asset_manifest.json"),
            manifest.to_canonical_json().expect("manifest JSON"),
        )
        .expect("manifest fixture");

        let model = EntityId::generate();
        let part = EntityId::generate();
        let renderer = engine::skinned_render_part_value(
            &imported.meshes[0].id,
            &imported.meshes[0].materials,
            &model,
        );
        let scene_json = serde_json::json!({
            "schema_version": 1,
            "entities": [
                {
                    "id": model.as_str(),
                    "name": "character",
                    "components": {
                        (engine::scene_bridge::SKINNED_MODEL_COMPONENT): {
                            "skeleton": {
                                "$type": "asset_ref",
                                "id": imported.skins[0].skeleton_id.as_str()
                            }
                        },
                        (engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT): {
                            "enabled": true
                        }
                    }
                },
                {
                    "id": part.as_str(),
                    "name": "body",
                    "parent": model.as_str(),
                    "components": {
                        "engine.transform": {
                            "x": 0.0,
                            "y": 0.0,
                            "z": 0.0
                        },
                        (engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT): renderer
                    }
                }
            ]
        });
        let scene = load_scene_fixture(&scene_json.to_string()).expect("scene fixture");
        let scene_path = project.scenes_dir().join("main.scene.json");
        fs::write(&scene_path, scene.to_canonical_json().expect("scene JSON"))
            .expect("scene fixture");
        let mut session = EditorSession::empty_behavior_tree();
        session
            .open_scene_discarding_changes(scene_path)
            .expect("open fixture scene");
        (directory, project, manifest, session, model, part)
    }

    #[test]
    fn bake_writes_obj_registers_assets_and_replaces_the_rig_components() {
        let (_directory, project, mut manifest, mut session, model, part) = skinned_project(false);

        let result = bake_skinned_model(&project, &mut manifest, &mut session, &model)
            .expect("bake succeeds");

        assert_eq!(result.baked_meshes, 1);
        assert_eq!(result.render_parts, 1);
        assert!(project
            .assets_root()
            .join(&result.output_paths[0])
            .is_file());
        let baked_id = manifest
            .iter()
            .find(|(_, entry)| {
                entry.path == result.output_paths[0].to_string_lossy().replace('\\', "/")
            })
            .map(|(id, _)| id.clone())
            .expect("baked asset is registered");
        let part = session.scene_entity(&part).expect("part remains");
        assert!(!part.components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT
        )));
        let Value::Object(renderer) = part
            .components
            .get(&ComponentTypeId::new(
                engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
            ))
            .expect("static renderer added")
        else {
            panic!("static renderer is an object");
        };
        assert_eq!(renderer.get("mesh"), Some(&Value::AssetRef(baked_id)));
        let model_entity = session.scene_entity(&model).expect("group entity remains");
        assert!(!model_entity.components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::SKINNED_MODEL_COMPONENT
        )));
        assert!(!model_entity.components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT
        )));
    }

    #[test]
    fn multi_submesh_bake_creates_static_siblings_with_the_same_parent() {
        let (_directory, project, mut manifest, mut session, model, part) = skinned_project(true);
        let original_parent = session
            .scene_entity(&part)
            .and_then(|entity| entity.parent.clone());

        let result = bake_skinned_model(&project, &mut manifest, &mut session, &model)
            .expect("multi-submesh bake succeeds");

        assert_eq!(result.baked_meshes, 2);
        let static_type =
            ComponentTypeId::new(engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT);
        let static_parts = session
            .scene()
            .expect("scene")
            .entities()
            .filter(|(_, entity)| entity.components.contains_key(&static_type))
            .collect::<Vec<_>>();
        assert_eq!(static_parts.len(), 2);
        assert!(static_parts
            .iter()
            .all(|(_, entity)| entity.parent == original_parent));
    }

    #[test]
    fn configured_controller_blocks_before_any_file_or_manifest_change() {
        let (_directory, project, mut manifest, mut session, model, _part) = skinned_project(false);
        let controller_type =
            ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
        let mut controller = match session
            .scene_entity(&model)
            .and_then(|entity| entity.components.get(&controller_type))
            .cloned()
            .expect("controller")
        {
            Value::Object(fields) => fields,
            _ => panic!("controller object"),
        };
        controller.insert("graph".into(), Value::AssetRef(AssetId::generate()));
        session
            .set_scene_component_value(model.clone(), controller_type, Value::Object(controller))
            .expect("configure controller");
        let manifest_before = manifest.to_canonical_json().expect("manifest");

        let error = bake_skinned_model(&project, &mut manifest, &mut session, &model)
            .expect_err("configured controller blocks");

        assert!(matches!(
            error,
            SkinnedModelBakeError::ConfiguredController(ref blocked) if blocked == &model
        ));
        assert_eq!(
            manifest.to_canonical_json().expect("manifest"),
            manifest_before
        );
        assert!(!project.assets_root().join("baked_meshes").exists());
    }

    #[test]
    fn bone_attachment_blocks_before_any_file_or_manifest_change() {
        let (_directory, project, mut manifest, mut session, model, _part) = skinned_project(false);
        let attachment = session
            .create_scene_entity("weapon")
            .expect("attachment entity");
        session
            .add_scene_component(
                attachment.clone(),
                ComponentTypeId::new(engine::scene_bridge::BONE_ATTACHMENT_COMPONENT),
                Value::Object(BTreeMap::from([
                    ("rig".into(), Value::EntityRef(model.clone())),
                    ("bone".into(), Value::I64(0)),
                    ("bone_name".into(), Value::String("root".into())),
                ])),
            )
            .expect("attachment component");

        let error = bake_skinned_model(&project, &mut manifest, &mut session, &model)
            .expect_err("attachment blocks");

        assert!(matches!(
            error,
            SkinnedModelBakeError::BoneAttachment { entity } if entity == attachment
        ));
        assert!(!project.assets_root().join("baked_meshes").exists());
    }

    #[test]
    fn undo_restores_the_skinned_authoring_components() {
        let (_directory, project, mut manifest, mut session, model, part) = skinned_project(false);
        bake_skinned_model(&project, &mut manifest, &mut session, &model).expect("bake succeeds");

        assert!(session.undo());

        assert!(session
            .scene_entity(&model)
            .expect("model")
            .components
            .contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT
            )));
        assert!(session
            .scene_entity(&part)
            .expect("part")
            .components
            .contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT
            )));
    }
}
