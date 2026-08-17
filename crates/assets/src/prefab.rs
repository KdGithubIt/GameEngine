//! Shared prefab asset creation and loading inside an `engine_authoring::ProjectRoot`.
//!
//! This service owns the asset-side half of prefab authoring: safe project path
//! resolution, subtree capture, deterministic serialization, fresh manifest
//! identity, and manifest persistence. Scene mutation remains in
//! `engine-authoring::PrefabAuthoringService`.

use crate::asset::{AssetManifest, ImportSettings, ManifestEntry};
use engine_authoring::{
    replace_file_contents, AssetId, AuthoringPermission, AuthoringPermissionError,
    AuthoringPermissions, AuthoringScene, EntityId, PersistError, PrefabAsset, PrefabError,
    PrefabSourceError, PrefabSourcePath, ProjectError, ProjectRoot,
};
use serde::Serialize;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const ASSET_MANIFEST_FILE: &str = "asset_manifest.json";

/// Successful prefab asset creation result shared by Editor, CLI, and MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrefabCreation {
    /// Fresh stable identity registered in `asset_manifest.json`.
    pub asset_id: AssetId,
    /// Asset-root-relative path written to the manifest.
    pub path: String,
    /// Unique searchable manifest name chosen for the prefab.
    pub name: String,
    /// Prefab-local root ID stored in the new prefab document.
    pub root: EntityId,
    /// Number of entities captured from the selected root subtree.
    pub entity_count: usize,
}

/// Prefab loaded from an asset-root-relative project path.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPrefab {
    /// Parsed prefab document.
    pub prefab: PrefabAsset,
    /// Portable project-relative source for the persisted instance marker.
    ///
    /// This is what belongs in canonical Scene data; [`Self::path`] is the
    /// machine-specific location and must not be serialized (ADR 0134).
    pub source: PrefabSourcePath,
    /// Canonical resolved file path that was opened.
    pub path: PathBuf,
}

/// Failure from shared prefab asset operations.
#[derive(Debug)]
pub enum PrefabAssetError {
    /// The caller lacks the required shared authoring permission.
    Permission(AuthoringPermissionError),
    /// A source or destination path violates the project boundary.
    Project(ProjectError),
    /// The selected root is absent from the source Scene.
    MissingEntity(EntityId),
    /// The requested destination already exists.
    DestinationExists(String),
    /// Prefab assets cannot be created below the user script tree.
    ScriptFolder(String),
    /// Authored prefab assets must use the `.prefab.json` suffix.
    InvalidSuffix(String),
    /// Captured prefab structure is invalid.
    Prefab(PrefabError),
    /// The resolved prefab file cannot be named as portable project data.
    Source(PrefabSourceError),
    /// Prefab JSON serialization failed.
    PrefabJson(serde_json::Error),
    /// Asset manifest JSON serialization failed.
    ManifestJson(serde_json::Error),
    /// Reading an existing prefab source failed.
    Io {
        /// Source path that failed.
        path: PathBuf,
        /// Underlying IO error.
        source: io::Error,
    },
    /// Atomic prefab persistence failed.
    PrefabPersist(PersistError),
    /// Atomic asset-manifest persistence failed.
    ManifestPersist(PersistError),
}

impl PrefabAssetError {
    /// Returns the stable diagnostic-style code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(source) => source.code(),
            Self::Project(_) => "asset.invalid_path",
            Self::MissingEntity(_) => "prefab.entity_not_found",
            Self::DestinationExists(_) => "prefab.destination_exists",
            Self::ScriptFolder(_) => "prefab.script_folder",
            Self::InvalidSuffix(_) => "prefab.invalid_path",
            Self::Prefab(_) => "prefab.invalid_selection",
            Self::Source(source) => source.code(),
            Self::PrefabJson(_) => "prefab.serialization_failed",
            Self::ManifestJson(_) => "asset.manifest_serialization_failed",
            Self::Io { .. } => "prefab.read_failed",
            Self::PrefabPersist(_) => "prefab.write_failed",
            Self::ManifestPersist(_) => "asset.manifest_write_failed",
        }
    }
}

impl fmt::Display for PrefabAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(source) => source.fmt(formatter),
            Self::Project(source) => write!(formatter, "prefab path is invalid: {source}"),
            Self::MissingEntity(entity) => {
                write!(formatter, "entity `{entity}` is absent from the source Scene")
            }
            Self::DestinationExists(path) => {
                write!(formatter, "prefab destination already exists: `{path}`")
            }
            Self::ScriptFolder(path) => {
                write!(formatter, "prefab destination `{path}` is inside the script tree")
            }
            Self::InvalidSuffix(path) => {
                write!(formatter, "prefab path `{path}` must end with .prefab.json")
            }
            Self::Prefab(source) => write!(formatter, "prefab selection is invalid: {source}"),
            Self::Source(source) => source.fmt(formatter),
            Self::PrefabJson(source) => write!(formatter, "failed to serialize prefab: {source}"),
            Self::ManifestJson(source) => {
                write!(formatter, "failed to serialize asset manifest: {source}")
            }
            Self::Io { path, source } => {
                write!(formatter, "failed to read prefab `{}`: {source}", path.display())
            }
            Self::PrefabPersist(source) => write!(formatter, "failed to save prefab: {source}"),
            Self::ManifestPersist(source) => {
                write!(formatter, "failed to save asset manifest: {source}")
            }
        }
    }
}

impl std::error::Error for PrefabAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(source) => Some(source),
            Self::Project(source) => Some(source),
            Self::Prefab(source) => Some(source),
            Self::Source(source) => Some(source),
            Self::PrefabJson(source) | Self::ManifestJson(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::PrefabPersist(source) | Self::ManifestPersist(source) => Some(source),
            Self::MissingEntity(_)
            | Self::DestinationExists(_)
            | Self::ScriptFolder(_)
            | Self::InvalidSuffix(_) => None,
        }
    }
}

impl From<AuthoringPermissionError> for PrefabAssetError {
    fn from(source: AuthoringPermissionError) -> Self {
        Self::Permission(source)
    }
}

impl From<ProjectError> for PrefabAssetError {
    fn from(source: ProjectError) -> Self {
        Self::Project(source)
    }
}

impl From<PrefabError> for PrefabAssetError {
    fn from(source: PrefabError) -> Self {
        Self::Prefab(source)
    }
}

impl From<PrefabSourceError> for PrefabAssetError {
    fn from(source: PrefabSourceError) -> Self {
        Self::Source(source)
    }
}

/// GUI-free project prefab asset service.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrefabAssetService;

impl PrefabAssetService {
    /// Creates the stateless prefab asset service.
    pub fn new() -> Self {
        Self
    }

    /// Creates one prefab from `root` and all descendants, then registers it
    /// with a fresh [`AssetId`] in the project manifest.
    ///
    /// The destination is asset-root-relative. Existing files are rejected. If
    /// manifest persistence fails after the new prefab file is written, the new
    /// file is removed and the caller's in-memory manifest remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAssetError`] for permission denial, unsafe paths,
    /// missing Scene entities, invalid prefab structure, or persistence errors.
    pub fn create_from_root(
        &self,
        project: &ProjectRoot,
        manifest: &mut AssetManifest,
        permissions: &AuthoringPermissions,
        scene: &AuthoringScene,
        root: &EntityId,
        destination_relative: &str,
    ) -> Result<PrefabCreation, PrefabAssetError> {
        permissions.require(AuthoringPermission::Read)?;
        permissions.require(AuthoringPermission::AssetWrite)?;

        let normalized = normalize_relative_path(destination_relative)?;
        require_prefab_suffix(&normalized)?;
        let destination = project.resolve_asset_for_write(&normalized)?;
        if Path::new(&normalized).starts_with("scripts") {
            return Err(PrefabAssetError::ScriptFolder(normalized));
        }
        if destination.exists() {
            return Err(PrefabAssetError::DestinationExists(normalized));
        }

        let ids = subtree_ids(scene, root)?;
        let prefab = PrefabAsset::from_selection(ids.iter().map(|id| {
            let entity = scene
                .entity(id)
                .expect("subtree IDs are collected from the same Scene");
            (id, entity)
        }))?;
        let prefab_json = prefab.to_json().map_err(PrefabAssetError::PrefabJson)?;

        let asset_id = AssetId::generate();
        let name = unique_asset_name(&normalized, manifest);
        let mut next_manifest = manifest.clone();
        next_manifest.insert(
            asset_id.clone(),
            ManifestEntry {
                path: normalized.clone(),
                name: Some(name.clone()),
                import_settings: ImportSettings::default(),
            },
        );
        let manifest_json = next_manifest
            .to_canonical_json()
            .map_err(PrefabAssetError::ManifestJson)?;

        replace_file_contents(&destination, &prefab_json)
            .map_err(PrefabAssetError::PrefabPersist)?;
        let manifest_path = project.path().join(ASSET_MANIFEST_FILE);
        if let Err(source) = replace_file_contents(&manifest_path, &manifest_json) {
            let _ = fs::remove_file(&destination);
            return Err(PrefabAssetError::ManifestPersist(source));
        }

        *manifest = next_manifest;
        Ok(PrefabCreation {
            asset_id,
            path: normalized,
            name,
            root: prefab.root,
            entity_count: prefab.entities.len(),
        })
    }

    /// Loads one author-owned prefab through the project asset boundary.
    ///
    /// The returned [`LoadedPrefab::source`] is the portable project-relative
    /// name used by the instance marker. The resolved filesystem path stays in
    /// [`LoadedPrefab::path`] so it cannot reach canonical project data.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAssetError`] for read permission denial, an unsafe or
    /// missing source path, a resolved file outside the project root, IO
    /// failure, or invalid prefab JSON/structure.
    pub fn load(
        &self,
        project: &ProjectRoot,
        permissions: &AuthoringPermissions,
        source_relative: &str,
    ) -> Result<LoadedPrefab, PrefabAssetError> {
        permissions.require(AuthoringPermission::Read)?;
        let normalized = normalize_relative_path(source_relative)?;
        require_prefab_suffix(&normalized)?;
        let path = project.resolve_asset(&normalized)?;
        let source = PrefabSourcePath::from_project_path(project.path(), &path)?;
        let json = fs::read_to_string(&path).map_err(|error| PrefabAssetError::Io {
            path: path.clone(),
            source: error,
        })?;
        let prefab = PrefabAsset::from_json(&json)?;
        Ok(LoadedPrefab {
            prefab,
            source,
            path,
        })
    }
}

fn normalize_relative_path(relative: &str) -> Result<String, PrefabAssetError> {
    let portable = relative.replace('\\', "/");
    let mut parts = Vec::new();
    for component in Path::new(&portable).components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(PrefabAssetError::Project(ProjectError::PathTraversal {
                        requested: relative.to_owned(),
                    }));
                };
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PrefabAssetError::Project(ProjectError::PathTraversal {
                    requested: relative.to_owned(),
                }));
            }
        }
    }
    if parts.is_empty() {
        return Err(PrefabAssetError::Project(ProjectError::PathTraversal {
            requested: relative.to_owned(),
        }));
    }
    Ok(parts.join("/"))
}

fn require_prefab_suffix(relative: &str) -> Result<(), PrefabAssetError> {
    if relative.ends_with(".prefab.json") {
        Ok(())
    } else {
        Err(PrefabAssetError::InvalidSuffix(relative.to_owned()))
    }
}

fn subtree_ids(scene: &AuthoringScene, root: &EntityId) -> Result<Vec<EntityId>, PrefabAssetError> {
    if scene.entity(root).is_none() {
        return Err(PrefabAssetError::MissingEntity(root.clone()));
    }
    let mut result = Vec::new();
    let mut frontier = vec![root.clone()];
    while let Some(parent) = frontier.pop() {
        result.push(parent.clone());
        let mut children = scene
            .entities()
            .filter(|(_, entity)| entity.parent.as_ref() == Some(&parent))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        children.reverse();
        frontier.extend(children);
    }
    Ok(result)
}

fn unique_asset_name(relative: &str, manifest: &AssetManifest) -> String {
    let display = Path::new(relative)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("prefab");
    let base = asset_name_slug(display);
    if !manifest
        .iter()
        .any(|(_, entry)| entry.name.as_deref() == Some(base.as_str()))
    {
        return base;
    }
    for suffix in 2.. {
        let candidate = format!("{base}_{suffix}");
        if !manifest
            .iter()
            .any(|(_, entry)| entry.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("usize suffix space must not be exhausted")
}

fn asset_name_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator && !slug.is_empty() {
            slug.push('_');
            previous_was_separator = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "asset".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        AuthoringCommand, AuthoringPermission, ProjectConfig, Transaction, PROJECT_SCHEMA_VERSION,
    };

    fn project() -> (tempfile::TempDir, ProjectRoot) {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let root = ProjectRoot::create(
            directory.path(),
            ProjectConfig {
                name: "PrefabAssetTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        fs::create_dir_all(root.assets_root().join("prefabs")).expect("prefab directory");
        (directory, root)
    }

    fn scene_with_nested_selection() -> (AuthoringScene, EntityId, EntityId) {
        let mut scene = AuthoringScene::new();
        let outer = EntityId::generate();
        let root = EntityId::generate();
        let child = EntityId::generate();
        let mut transaction = Transaction::begin(&scene);
        transaction.apply(AuthoringCommand::CreateEntity {
            id: outer.clone(),
            name: "outer".into(),
            parent: None,
        });
        transaction.apply(AuthoringCommand::CreateEntity {
            id: root.clone(),
            name: "enemy".into(),
            parent: Some(outer),
        });
        transaction.apply(AuthoringCommand::CreateEntity {
            id: child.clone(),
            name: "weapon".into(),
            parent: Some(root.clone()),
        });
        transaction.commit(&mut scene).expect("scene fixture");
        (scene, root, child)
    }

    fn write_permissions() -> AuthoringPermissions {
        AuthoringPermissions::read_only().with(AuthoringPermission::AssetWrite)
    }

    #[test]
    fn creation_captures_descendants_detaches_root_and_registers_manifest() {
        let (_directory, project) = project();
        let (scene, root, child) = scene_with_nested_selection();
        let mut manifest = AssetManifest::default();

        let result = PrefabAssetService::new()
            .create_from_root(
                &project,
                &mut manifest,
                &write_permissions(),
                &scene,
                &root,
                "prefabs/enemy.prefab.json",
            )
            .expect("prefab creation");

        assert_eq!(result.entity_count, 2);
        let entry = manifest.get(&result.asset_id).expect("manifest entry");
        assert_eq!(entry.path, "prefabs/enemy.prefab.json");
        let saved = fs::read_to_string(project.assets_root().join(&entry.path))
            .expect("prefab file");
        let prefab = PrefabAsset::from_json(&saved).expect("saved prefab must be reloadable");
        assert_eq!(prefab.entities.len(), 2);
        assert_eq!(prefab.entities[&prefab.root].parent, None);
        assert!(prefab.entities.contains_key(&child));

        let persisted_manifest = fs::read_to_string(project.path().join(ASSET_MANIFEST_FILE))
            .expect("manifest file");
        let persisted = AssetManifest::from_json(&persisted_manifest).expect("manifest parse");
        assert!(persisted.get(&result.asset_id).is_some());
    }

    #[test]
    fn creation_requires_asset_write_and_rejects_path_escape() {
        let (_directory, project) = project();
        let (scene, root, _) = scene_with_nested_selection();
        let mut manifest = AssetManifest::default();

        let denied = PrefabAssetService::new()
            .create_from_root(
                &project,
                &mut manifest,
                &AuthoringPermissions::read_only(),
                &scene,
                &root,
                "prefabs/enemy.prefab.json",
            )
            .expect_err("asset-write permission must be required");
        assert_eq!(denied.code(), "authoring.permission_denied");

        let escaped = PrefabAssetService::new()
            .create_from_root(
                &project,
                &mut manifest,
                &write_permissions(),
                &scene,
                &root,
                "../enemy.prefab.json",
            )
            .expect_err("path traversal must be rejected");
        assert_eq!(escaped.code(), "asset.invalid_path");
    }

    #[test]
    fn load_uses_project_boundary_and_shared_read_permission() {
        let (_directory, project) = project();
        let (scene, root, _) = scene_with_nested_selection();
        let mut manifest = AssetManifest::default();
        PrefabAssetService::new()
            .create_from_root(
                &project,
                &mut manifest,
                &write_permissions(),
                &scene,
                &root,
                "prefabs/enemy.prefab.json",
            )
            .expect("prefab creation");

        let loaded = PrefabAssetService::new()
            .load(
                &project,
                &AuthoringPermissions::read_only(),
                "prefabs/enemy.prefab.json",
            )
            .expect("prefab load");
        assert_eq!(loaded.prefab.entities.len(), 2);
        assert_eq!(
            loaded.source.as_str(),
            "assets/prefabs/enemy.prefab.json",
            "the instance marker source must be portable project data"
        );
        assert_eq!(loaded.source.resolve(project.path()), loaded.path);

        let denied = PrefabAssetService::new()
            .load(
                &project,
                &AuthoringPermissions::none(),
                "prefabs/enemy.prefab.json",
            )
            .expect_err("read permission must be required");
        assert_eq!(denied.code(), "authoring.permission_denied");
    }
}
