//! Shared read-only asset discovery and inspection over the project manifest.
//!
//! The catalog keeps stable [`AssetId`] lookup and search semantics outside
//! Editor, CLI, and MCP adapters. File existence checks always pass through
//! [`ProjectRoot`] so manifest paths cannot bypass the project boundary.

use crate::asset::{AssetManifest, ImportSettings, ImportedSubAsset, ImportedSubAssetKind};
use engine_authoring::{
    AssetId, AuthoringPermission, AuthoringPermissionError, AuthoringPermissions, IdError,
    ProjectError, ProjectRoot, StableId,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;

/// One stable asset visible through catalog search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCatalogEntry {
    /// Stable asset identity used for all subsequent operations.
    pub id: AssetId,
    /// Searchable authoring name when one is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Asset-root-relative source path owning this asset.
    pub path: String,
    /// Source asset that owns an imported sub-asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_asset: Option<AssetId>,
    /// Imported category when this row represents a deterministic sub-asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_kind: Option<ImportedSubAssetKind>,
}

/// Deterministic search output from the project asset catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCatalogSearch {
    /// Matching assets sorted by stable ID.
    pub assets: Vec<AssetCatalogEntry>,
}

/// Detailed information about one stable asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInspection {
    /// Stable identity that was inspected.
    pub id: AssetId,
    /// Searchable authoring name when one is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Asset-root-relative source path.
    pub path: String,
    /// Whether the source file currently exists as a regular file inside the project.
    pub file_exists: bool,
    /// Import metadata owned by the registered source asset.
    pub import_settings: ImportSettings,
    /// Source asset that owns this imported sub-asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_asset: Option<AssetId>,
    /// Imported selector metadata when this asset is a deterministic sub-asset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported: Option<ImportedSubAsset>,
}

/// Failure from shared asset catalog operations.
#[derive(Debug)]
pub enum AssetCatalogError {
    /// The caller lacks read permission.
    Permission(AuthoringPermissionError),
    /// A manifest path could not be resolved safely through the project root.
    Project(ProjectError),
    /// Imported metadata contains a malformed stable asset ID.
    InvalidImportedAssetId {
        /// Rejected persisted ID.
        id: String,
        /// Typed stable-ID validation error.
        source: IdError,
    },
    /// No manifest entry or imported sub-asset has the requested stable ID.
    NotFound(AssetId),
}

impl AssetCatalogError {
    /// Returns the stable diagnostic-style code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(source) => source.code(),
            Self::Project(_) => "asset.invalid_path",
            Self::InvalidImportedAssetId { .. } => "asset.invalid_imported_id",
            Self::NotFound(_) => "asset.not_found",
        }
    }
}

impl fmt::Display for AssetCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(source) => source.fmt(formatter),
            Self::Project(source) => write!(formatter, "asset path is not readable: {source}"),
            Self::InvalidImportedAssetId { id, source } => {
                write!(formatter, "imported asset ID `{id}` is invalid: {source}")
            }
            Self::NotFound(id) => {
                write!(formatter, "asset `{id}` is not registered in the project")
            }
        }
    }
}

impl std::error::Error for AssetCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(source) => Some(source),
            Self::Project(source) => Some(source),
            Self::InvalidImportedAssetId { source, .. } => Some(source),
            Self::NotFound(_) => None,
        }
    }
}

impl From<AuthoringPermissionError> for AssetCatalogError {
    fn from(source: AuthoringPermissionError) -> Self {
        Self::Permission(source)
    }
}

/// Shared read-only asset discovery service.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssetCatalogService;

impl AssetCatalogService {
    /// Creates the shared project asset catalog service.
    pub fn new() -> Self {
        Self
    }

    /// Searches registered source assets and imported sub-assets.
    ///
    /// Matching is case-insensitive across stable ID, authoring name, source
    /// path, and imported kind. An empty query returns the complete catalog.
    /// Results are sorted by stable ID for deterministic adapter output.
    ///
    /// # Errors
    ///
    /// Returns [`AssetCatalogError`] when read permission is denied or imported
    /// metadata contains an invalid stable ID.
    pub fn search(
        &self,
        _project: &ProjectRoot,
        manifest: &AssetManifest,
        permissions: &AuthoringPermissions,
        query: &str,
    ) -> Result<AssetCatalogSearch, AssetCatalogError> {
        permissions.require(AuthoringPermission::Read)?;
        let query = query.trim().to_ascii_lowercase();
        let mut assets = Vec::new();

        for (source_id, entry) in manifest.iter() {
            if matches_query(
                &query,
                [source_id.as_str(), entry.path.as_str()]
                    .into_iter()
                    .chain(entry.name.as_deref()),
            ) {
                assets.push(AssetCatalogEntry {
                    id: source_id.clone(),
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                    source_asset: None,
                    imported_kind: None,
                });
            }

            for imported in &entry.import_settings.sub_assets {
                let imported_id = AssetId::from_stable_id(StableId::new(imported.id.clone()))
                    .map_err(|source| AssetCatalogError::InvalidImportedAssetId {
                        id: imported.id.clone(),
                        source,
                    })?;
                let imported_kind = imported_kind_name(imported.kind);
                if matches_query(
                    &query,
                    [
                        imported.id.as_str(),
                        imported.name.as_str(),
                        entry.path.as_str(),
                        imported_kind,
                    ]
                    .into_iter(),
                ) {
                    assets.push(AssetCatalogEntry {
                        id: imported_id,
                        name: Some(imported.name.clone()),
                        path: entry.path.clone(),
                        source_asset: Some(source_id.clone()),
                        imported_kind: Some(imported.kind),
                    });
                }
            }
        }

        assets.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        Ok(AssetCatalogSearch { assets })
    }

    /// Inspects one registered source asset or deterministic imported sub-asset.
    ///
    /// Missing source files are reported with `file_exists = false` rather
    /// than failing the catalog operation, matching the manifest contract.
    /// Unsafe manifest paths remain hard errors through [`ProjectRoot`].
    ///
    /// # Errors
    ///
    /// Returns [`AssetCatalogError`] for permission denial, unsafe/unreadable
    /// project paths other than a missing file, or an unknown stable ID.
    pub fn inspect(
        &self,
        project: &ProjectRoot,
        manifest: &AssetManifest,
        permissions: &AuthoringPermissions,
        id: &AssetId,
    ) -> Result<AssetInspection, AssetCatalogError> {
        permissions.require(AuthoringPermission::Read)?;

        if let Some(entry) = manifest.get(id) {
            return Ok(AssetInspection {
                id: id.clone(),
                name: entry.name.clone(),
                path: entry.path.clone(),
                file_exists: source_file_exists(project, &entry.path)?,
                import_settings: entry.import_settings.clone(),
                source_asset: None,
                imported: None,
            });
        }

        if let Some((source_id, entry, imported)) = manifest.imported_sub_asset(id) {
            return Ok(AssetInspection {
                id: id.clone(),
                name: Some(imported.name.clone()),
                path: entry.path.clone(),
                file_exists: source_file_exists(project, &entry.path)?,
                import_settings: entry.import_settings.clone(),
                source_asset: Some(source_id.clone()),
                imported: Some(imported.clone()),
            });
        }

        Err(AssetCatalogError::NotFound(id.clone()))
    }
}

fn source_file_exists(project: &ProjectRoot, relative: &str) -> Result<bool, AssetCatalogError> {
    match project.resolve_asset(relative) {
        Ok(path) => Ok(path.is_file()),
        Err(ProjectError::Io(source)) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(AssetCatalogError::Project(source)),
    }
}

fn matches_query<'a>(query: &str, mut values: impl Iterator<Item = &'a str>) -> bool {
    query.is_empty()
        || values.any(|value| value.to_ascii_lowercase().contains(query))
}

fn imported_kind_name(kind: ImportedSubAssetKind) -> &'static str {
    match kind {
        ImportedSubAssetKind::Mesh => "mesh",
        ImportedSubAssetKind::Material => "material",
        ImportedSubAssetKind::Texture => "texture",
        ImportedSubAssetKind::Skeleton => "skeleton",
        ImportedSubAssetKind::Skin => "skin",
        ImportedSubAssetKind::Animation => "animation",
        ImportedSubAssetKind::HumanoidMotion => "humanoid_motion",
        ImportedSubAssetKind::Morph => "morph",
        ImportedSubAssetKind::RigidBodyRig => "rigid_body_rig",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{imported_sub_asset_id, ManifestEntry};
    use engine_authoring::{ProjectConfig, PROJECT_SCHEMA_VERSION};
    use std::fs;

    fn project() -> (tempfile::TempDir, ProjectRoot) {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let root = ProjectRoot::create(
            directory.path(),
            ProjectConfig {
                name: "AssetCatalogTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture must be created");
        (directory, root)
    }

    fn manifest_with_imported_asset(root: &ProjectRoot) -> (AssetManifest, AssetId, AssetId) {
        let source_id = AssetId::generate();
        let imported_id = imported_sub_asset_id(&source_id, ImportedSubAssetKind::Mesh, 0);
        fs::create_dir_all(root.assets_root().join("models"))
            .expect("models directory must be created");
        fs::write(root.assets_root().join("models/hero.glb"), b"glTF")
            .expect("source fixture must be written");

        let mut manifest = AssetManifest::default();
        manifest.insert(
            source_id.clone(),
            ManifestEntry {
                path: "models/hero.glb".into(),
                name: Some("hero_model".into()),
                import_settings: ImportSettings {
                    sub_assets: vec![ImportedSubAsset {
                        id: imported_id.as_str().to_owned(),
                        kind: ImportedSubAssetKind::Mesh,
                        name: "hero_body".into(),
                        index: 0,
                        target_model_source: None,
                    }],
                    ..ImportSettings::default()
                },
            },
        );
        (manifest, source_id, imported_id)
    }

    #[test]
    fn search_finds_source_and_imported_assets_with_stable_ids() {
        let (_directory, root) = project();
        let (manifest, source_id, imported_id) = manifest_with_imported_asset(&root);
        let service = AssetCatalogService::new();
        let permissions = AuthoringPermissions::read_only();

        let source = service
            .search(&root, &manifest, &permissions, "hero_model")
            .expect("source search must succeed");
        assert_eq!(source.assets.len(), 1);
        assert_eq!(source.assets[0].id, source_id);

        let by_kind = service
            .search(&root, &manifest, &permissions, "mesh")
            .expect("kind search must succeed");
        assert_eq!(by_kind.assets.len(), 1);
        assert_eq!(by_kind.assets[0].id, imported_id);
    }

    #[test]
    fn inspect_imported_asset_reports_source_metadata_and_file_state() {
        let (_directory, root) = project();
        let (manifest, source_id, imported_id) = manifest_with_imported_asset(&root);

        let inspection = AssetCatalogService::new()
            .inspect(
                &root,
                &manifest,
                &AuthoringPermissions::read_only(),
                &imported_id,
            )
            .expect("imported inspection must succeed");

        assert_eq!(inspection.source_asset, Some(source_id));
        assert!(inspection.file_exists);
        assert_eq!(
            inspection.imported.as_ref().map(|asset| asset.kind),
            Some(ImportedSubAssetKind::Mesh)
        );
    }

    #[test]
    fn missing_source_file_is_nonfatal_but_unsafe_path_is_rejected() {
        let (_directory, root) = project();
        let missing = AssetId::generate();
        let unsafe_id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            missing.clone(),
            ManifestEntry {
                path: "models/missing.glb".into(),
                name: Some("missing".into()),
                import_settings: ImportSettings::default(),
            },
        );
        manifest.insert(
            unsafe_id.clone(),
            ManifestEntry {
                path: "../outside.glb".into(),
                name: Some("unsafe".into()),
                import_settings: ImportSettings::default(),
            },
        );

        let service = AssetCatalogService::new();
        let permissions = AuthoringPermissions::read_only();
        let missing = service
            .inspect(&root, &manifest, &permissions, &missing)
            .expect("missing file must remain inspectable");
        assert!(!missing.file_exists);

        let error = service
            .inspect(&root, &manifest, &permissions, &unsafe_id)
            .expect_err("path traversal must reject inspection");
        assert_eq!(error.code(), "asset.invalid_path");
    }

    #[test]
    fn catalog_requires_shared_read_permission() {
        let (_directory, root) = project();
        let manifest = AssetManifest::default();
        let error = AssetCatalogService::new()
            .search(&root, &manifest, &AuthoringPermissions::none(), "")
            .expect_err("catalog access must require read permission");
        assert_eq!(error.code(), "authoring.permission_denied");
    }
}
