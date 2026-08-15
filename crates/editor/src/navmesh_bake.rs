//! Editor-owned NavMesh bake document, stale detection, and project output.

use engine::glam::Vec3;
use engine::{AssetManifest, AssetServer, ManifestEntry, NavMesh, NavMeshSettings};
use engine_authoring::{AssetId, AuthoringCommand, AuthoringScene, ComponentTypeId, ProjectRoot};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Current serialized bake-document schema.
pub const NAVMESH_BAKE_SCHEMA_VERSION: u32 = 1;

/// Serializable vector used by the editor bake document.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BakeVector3(pub [f32; 3]);

impl From<Vec3> for BakeVector3 {
    fn from(value: Vec3) -> Self {
        Self(value.to_array())
    }
}

impl From<BakeVector3> for Vec3 {
    fn from(value: BakeVector3) -> Self {
        Self::from_array(value.0)
    }
}

/// Persisted NavMesh bake settings owned by one scene workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavMeshBakeSettings {
    /// Square grid cell size in world units.
    pub cell_size: f32,
    /// Horizontal obstacle expansion for agent clearance.
    pub agent_radius: f32,
    /// Minimum world-space navigation bounds.
    pub world_min: BakeVector3,
    /// Maximum world-space navigation bounds.
    pub world_max: BakeVector3,
    /// Height of the walkable arena plane.
    pub walkable_height: f32,
    /// Required vertical agent clearance.
    pub agent_height: f32,
}

impl Default for NavMeshBakeSettings {
    fn default() -> Self {
        Self::from(&NavMeshSettings::default())
    }
}

impl From<&NavMeshSettings> for NavMeshBakeSettings {
    fn from(settings: &NavMeshSettings) -> Self {
        Self {
            cell_size: settings.cell_size,
            agent_radius: settings.agent_radius,
            world_min: settings.world_min.into(),
            world_max: settings.world_max.into(),
            walkable_height: settings.walkable_height,
            agent_height: settings.agent_height,
        }
    }
}

impl From<&NavMeshBakeSettings> for NavMeshSettings {
    fn from(settings: &NavMeshBakeSettings) -> Self {
        Self {
            cell_size: settings.cell_size,
            agent_radius: settings.agent_radius,
            world_min: settings.world_min.into(),
            world_max: settings.world_max.into(),
            walkable_height: settings.walkable_height,
            agent_height: settings.agent_height,
        }
    }
}

/// Scene-owned bake document stored beside navigation assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavMeshBakeDocument {
    /// Document schema version.
    pub schema_version: u32,
    /// Relative `.navmesh.json` output path under the project's assets folder.
    pub output_asset: String,
    /// Fingerprint of the scene and settings used by the last successful bake.
    pub source_fingerprint: Option<String>,
    /// User-editable bake settings.
    pub settings: NavMeshBakeSettings,
}

impl Default for NavMeshBakeDocument {
    fn default() -> Self {
        Self {
            schema_version: NAVMESH_BAKE_SCHEMA_VERSION,
            output_asset: "navigation/arena.navmesh.json".to_owned(),
            source_fingerprint: None,
            settings: NavMeshBakeSettings::default(),
        }
    }
}

impl NavMeshBakeDocument {
    /// Parses and validates a bake document.
    pub fn from_json(json: &str) -> Result<Self, NavMeshBakeError> {
        let document: Self = serde_json::from_str(json).map_err(NavMeshBakeError::Json)?;
        if document.schema_version > NAVMESH_BAKE_SCHEMA_VERSION {
            return Err(NavMeshBakeError::UnsupportedVersion(
                document.schema_version,
            ));
        }
        document.validate()?;
        Ok(document)
    }

    /// Writes canonical pretty JSON.
    pub fn to_canonical_json(&self) -> Result<String, NavMeshBakeError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(NavMeshBakeError::Json)
    }

    /// Returns whether the scene or settings changed after the last bake.
    pub fn is_stale(&self, scene: &AuthoringScene) -> Result<bool, NavMeshBakeError> {
        Ok(self.source_fingerprint.as_deref()
            != Some(scene_fingerprint(scene, &self.settings)?.as_str()))
    }

    fn validate(&self) -> Result<(), NavMeshBakeError> {
        let settings: NavMeshSettings = (&self.settings).into();
        if !settings.cell_size.is_finite()
            || settings.cell_size <= 0.0
            || !settings.agent_radius.is_finite()
            || settings.agent_radius < 0.0
            || !settings.agent_height.is_finite()
            || settings.agent_height <= 0.0
            || settings.world_min.x >= settings.world_max.x
            || settings.world_min.z >= settings.world_max.z
            || !self.output_asset.ends_with(".navmesh.json")
            || Path::new(&self.output_asset).is_absolute()
            || Path::new(&self.output_asset)
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(NavMeshBakeError::InvalidSettings);
        }
        Ok(())
    }
}

/// Successful bake output and registered stable asset ID.
pub struct NavMeshBakeResult {
    /// Newly baked runtime grid.
    pub nav_mesh: NavMesh,
    /// Stable manifest ID associated with the output path.
    pub asset_id: AssetId,
    /// Absolute output path.
    pub output_path: PathBuf,
}

/// Bakes, saves, registers, and fingerprints one scene NavMesh.
pub fn bake_scene_navmesh(
    scene: &AuthoringScene,
    project_root: &ProjectRoot,
    manifest: &mut AssetManifest,
    document: &mut NavMeshBakeDocument,
    cancelled: &AtomicBool,
) -> Result<NavMeshBakeResult, NavMeshBakeError> {
    document.validate()?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(NavMeshBakeError::Cancelled);
    }

    // The active surface references the previous artifact and must not make a
    // first bake circular. Remove only that runtime-loading component from a
    // transaction-owned clone used for conversion.
    let bake_scene = scene_without_surface(scene)?;

    let mut world = engine::ecs::World::new();
    world.insert_resource(AssetServer::with_assets_root(project_root.assets_root()));
    world.insert_resource(manifest.clone());
    engine::scene_bridge::spawn_from_authoring_scene(&mut world, &bake_scene)
        .map_err(|error| NavMeshBakeError::Scene(error.to_string()))?;
    engine::transform_propagation_system(engine::Query::new(&mut world));
    if cancelled.load(Ordering::Relaxed) {
        return Err(NavMeshBakeError::Cancelled);
    }

    let output_path = project_root
        .resolve_asset_for_write(&document.output_asset)
        .map_err(|error| NavMeshBakeError::Project(error.to_string()))?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(NavMeshBakeError::Io)?;
    }
    let settings: NavMeshSettings = (&document.settings).into();
    let nav_mesh = engine::bake_navmesh(&mut world, &settings, &output_path)
        .map_err(|error| NavMeshBakeError::Bake(error.to_string()))?;

    let asset_id = manifest
        .iter()
        .find(|(_, entry)| entry.path == document.output_asset)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(AssetId::generate);
    manifest.insert(
        asset_id.clone(),
        ManifestEntry {
            path: document.output_asset.clone(),
            name: Some("Arena NavMesh".to_owned()),
            import_settings: Default::default(),
        },
    );
    let manifest_json = manifest
        .to_canonical_json()
        .map_err(NavMeshBakeError::Json)?;
    fs::write(
        project_root.path().join("asset_manifest.json"),
        manifest_json,
    )
    .map_err(NavMeshBakeError::Io)?;
    document.source_fingerprint = Some(scene_fingerprint(scene, &document.settings)?);

    Ok(NavMeshBakeResult {
        nav_mesh,
        asset_id,
        output_path,
    })
}

fn scene_fingerprint(
    scene: &AuthoringScene,
    settings: &NavMeshBakeSettings,
) -> Result<String, NavMeshBakeError> {
    let normalized_scene = scene_without_surface(scene)?;
    let scene_json = normalized_scene
        .to_canonical_json()
        .map_err(|error| NavMeshBakeError::Scene(error.to_string()))?;
    let settings_json = serde_json::to_vec(settings).map_err(NavMeshBakeError::Json)?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in scene_json.bytes().chain(settings_json) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

fn scene_without_surface(scene: &AuthoringScene) -> Result<AuthoringScene, NavMeshBakeError> {
    let mut normalized = scene.clone();
    let surface = ComponentTypeId::new(engine::scene_bridge::NAV_MESH_SURFACE_COMPONENT);
    let removals = normalized
        .entities()
        .filter(|(_, entity)| entity.components.contains_key(&surface))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if removals.is_empty() {
        return Ok(normalized);
    }
    let mut transaction = engine_authoring::Transaction::begin(&normalized);
    for entity in removals {
        transaction.apply(AuthoringCommand::RemoveComponent {
            entity,
            component_type: surface.clone(),
        });
    }
    transaction
        .commit(&mut normalized)
        .map_err(|error| NavMeshBakeError::Scene(error.to_string()))?;
    Ok(normalized)
}

/// Error from bake-document validation or execution.
#[derive(Debug)]
pub enum NavMeshBakeError {
    /// JSON parse or serialization failure.
    Json(serde_json::Error),
    /// Document schema is newer than the editor.
    UnsupportedVersion(u32),
    /// Bounds, numeric values, or output path are invalid.
    InvalidSettings,
    /// Authoring scene conversion failed.
    Scene(String),
    /// Project path resolution failed.
    Project(String),
    /// Runtime grid bake failed.
    Bake(String),
    /// File creation or persistence failed.
    Io(std::io::Error),
    /// The caller cancelled before output mutation.
    Cancelled,
}

impl fmt::Display for NavMeshBakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "NavMesh bake JSON error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported NavMesh bake schema version {version}"
                )
            }
            Self::InvalidSettings => formatter.write_str("invalid NavMesh bake settings"),
            Self::Scene(error) => write!(formatter, "NavMesh scene conversion failed: {error}"),
            Self::Project(error) => write!(formatter, "NavMesh project path failed: {error}"),
            Self::Bake(error) => write!(formatter, "NavMesh bake failed: {error}"),
            Self::Io(error) => write!(formatter, "NavMesh bake I/O failed: {error}"),
            Self::Cancelled => formatter.write_str("NavMesh bake cancelled"),
        }
    }
}

impl std::error::Error for NavMeshBakeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_change_marks_bake_document_stale() {
        let scene = AuthoringScene::new();
        let mut document = NavMeshBakeDocument::default();
        document.source_fingerprint = Some(scene_fingerprint(&scene, &document.settings).unwrap());
        assert!(!document.is_stale(&scene).unwrap());
        document.settings.agent_radius += 0.1;
        assert!(document.is_stale(&scene).unwrap());
    }

    #[test]
    fn bake_document_round_trips_canonically() {
        let document = NavMeshBakeDocument::default();
        let json = document.to_canonical_json().unwrap();
        assert_eq!(NavMeshBakeDocument::from_json(&json).unwrap(), document);
    }
}
