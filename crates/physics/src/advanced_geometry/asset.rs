//! Persisted advanced-geometry documents shared by the designer and runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use glam::Vec3;
use serde::{Deserialize, Serialize};

use super::core::{LayeredNavMesh, NavMeshLayer, NavMeshLayerLink, StaticTriangleMesh};
use crate::navmesh::{bake_from_obstacles, NavMeshSettings};

/// Current persisted advanced-geometry schema version.
pub const ADVANCED_GEOMETRY_SCHEMA_VERSION: u32 = 1;

/// Conventional suffix used by advanced-geometry documents.
pub const ADVANCED_GEOMETRY_FILE_SUFFIX: &str = ".advanced-geometry.json";

/// One axis-aligned obstacle baked into a navigation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedGeometryObstacle {
    /// World-space center.
    pub center: [f32; 3],
    /// Non-negative half extents.
    pub half_extents: [f32; 3],
}

/// One named height layer and its grid-bake settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedGeometryNavLayer {
    /// Stable project-local layer ID.
    pub id: String,
    /// Inclusive minimum world height assigned to this layer.
    pub minimum_height: f32,
    /// Inclusive maximum world height assigned to this layer.
    pub maximum_height: f32,
    /// Grid cell size.
    pub cell_size: f32,
    /// Radius used to expand authored obstacles.
    pub agent_radius: f32,
    /// Minimum grid-bake world bound.
    pub world_min: [f32; 3],
    /// Maximum grid-bake world bound.
    pub world_max: [f32; 3],
    /// Y coordinate emitted for walkable points.
    pub walkable_height: f32,
    /// Agent height used by the grid bake.
    pub agent_height: f32,
    /// Axis-aligned obstacles excluded from the layer.
    pub obstacles: Vec<AdvancedGeometryObstacle>,
}

impl Default for AdvancedGeometryNavLayer {
    fn default() -> Self {
        Self {
            id: "layer".to_owned(),
            minimum_height: -0.5,
            maximum_height: 0.5,
            cell_size: 0.5,
            agent_radius: 0.4,
            world_min: [-10.0, 0.0, -10.0],
            world_max: [10.0, 0.0, 10.0],
            walkable_height: 0.0,
            agent_height: 1.8,
            obstacles: Vec::new(),
        }
    }
}

/// Explicit stairs, lift, or drop connection between two height layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedGeometryNavLink {
    /// Source layer ID.
    pub from_layer: String,
    /// Destination layer ID.
    pub to_layer: String,
    /// Source world position.
    pub from: [f32; 3],
    /// Destination world position.
    pub to: [f32; 3],
    /// Additional non-negative traversal cost.
    pub cost: f32,
    /// Whether the reverse direction is also available.
    pub bidirectional: bool,
}

/// One immutable indexed triangle mesh used by static spatial queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedGeometryStaticMesh {
    /// Stable project-local mesh ID.
    pub id: String,
    /// World-space vertices.
    pub vertices: Vec<[f32; 3]>,
    /// Indexed triangles.
    pub triangles: Vec<[u32; 3]>,
}

impl Default for AdvancedGeometryStaticMesh {
    fn default() -> Self {
        Self {
            id: "mesh".to_owned(),
            vertices: vec![[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [0.0, 0.0, 1.0]],
            triangles: vec![[0, 1, 2]],
        }
    }
}

/// Persisted source document produced by Advanced Geometry Designer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvancedGeometryDocument {
    /// Persisted format version.
    pub schema_version: u32,
    /// Authored navigation height layers.
    pub layers: Vec<AdvancedGeometryNavLayer>,
    /// Authored inter-layer links.
    pub links: Vec<AdvancedGeometryNavLink>,
    /// Authored static triangle meshes.
    pub meshes: Vec<AdvancedGeometryStaticMesh>,
}

impl Default for AdvancedGeometryDocument {
    fn default() -> Self {
        let ground = AdvancedGeometryNavLayer {
            id: "ground".to_owned(),
            ..AdvancedGeometryNavLayer::default()
        };
        let upper = AdvancedGeometryNavLayer {
            id: "upper".to_owned(),
            minimum_height: 2.5,
            maximum_height: 3.5,
            world_min: [-10.0, 3.0, -10.0],
            world_max: [10.0, 3.0, 10.0],
            walkable_height: 3.0,
            ..AdvancedGeometryNavLayer::default()
        };
        Self {
            schema_version: ADVANCED_GEOMETRY_SCHEMA_VERSION,
            layers: vec![ground, upper],
            links: vec![AdvancedGeometryNavLink {
                from_layer: "ground".to_owned(),
                to_layer: "upper".to_owned(),
                from: [0.0, 0.0, 0.0],
                to: [0.0, 3.0, 0.0],
                cost: 1.0,
                bidirectional: true,
            }],
            meshes: vec![AdvancedGeometryStaticMesh {
                id: "ground_probe".to_owned(),
                ..AdvancedGeometryStaticMesh::default()
            }],
        }
    }
}

impl AdvancedGeometryDocument {
    /// Parses and validates one JSON document.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, unsupported schemas, or geometry
    /// rejected by the same runtime constructors used during gameplay.
    pub fn from_json_str(json: &str) -> Result<Self, AdvancedGeometryAssetError> {
        let document: Self = serde_json::from_str(json).map_err(AdvancedGeometryAssetError::Json)?;
        document.build()?;
        Ok(document)
    }

    /// Serializes a validated document as readable JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when the document is invalid or cannot be serialized.
    pub fn to_json_string(&self) -> Result<String, AdvancedGeometryAssetError> {
        self.build()?;
        serde_json::to_string_pretty(self).map_err(AdvancedGeometryAssetError::Json)
    }

    /// Loads and validates an advanced-geometry document from disk.
    ///
    /// # Errors
    ///
    /// Returns an I/O, JSON, schema, or runtime-construction error.
    pub fn load(path: &Path) -> Result<Self, AdvancedGeometryAssetError> {
        let json = fs::read_to_string(path).map_err(|source| AdvancedGeometryAssetError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_json_str(&json)
    }

    /// Saves a validated advanced-geometry document to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, serialization, or writing fails.
    pub fn save(&self, path: &Path) -> Result<(), AdvancedGeometryAssetError> {
        let json = self.to_json_string()?;
        fs::write(path, json).map_err(|source| AdvancedGeometryAssetError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Builds the runtime navigation and static-query objects.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema or any invalid authored value.
    pub fn build(&self) -> Result<AdvancedGeometryRuntime, AdvancedGeometryAssetError> {
        if self.schema_version != ADVANCED_GEOMETRY_SCHEMA_VERSION {
            return Err(AdvancedGeometryAssetError::Invalid(format!(
                "unsupported advanced geometry schema {}; expected {ADVANCED_GEOMETRY_SCHEMA_VERSION}",
                self.schema_version
            )));
        }

        let layers = self
            .layers
            .iter()
            .map(build_layer)
            .collect::<Result<Vec<_>, _>>()?;
        let links = self
            .links
            .iter()
            .map(|link| NavMeshLayerLink {
                from_layer: link.from_layer.clone(),
                to_layer: link.to_layer.clone(),
                from: Vec3::from_array(link.from),
                to: Vec3::from_array(link.to),
                cost: link.cost,
                bidirectional: link.bidirectional,
            })
            .collect();
        let nav_mesh = LayeredNavMesh::new(layers, links)
            .map_err(|error| AdvancedGeometryAssetError::Invalid(error.to_string()))?;

        let mut mesh_ids = BTreeSet::new();
        let mut static_meshes = BTreeMap::new();
        for mesh in &self.meshes {
            let id = mesh.id.trim();
            if id.is_empty() {
                return Err(AdvancedGeometryAssetError::Invalid(
                    "static mesh ID must not be blank".to_owned(),
                ));
            }
            if !mesh_ids.insert(id.to_owned()) {
                return Err(AdvancedGeometryAssetError::Invalid(format!(
                    "static mesh ID `{id}` is duplicated"
                )));
            }
            let runtime = StaticTriangleMesh::new(
                mesh.vertices
                    .iter()
                    .copied()
                    .map(Vec3::from_array)
                    .collect(),
                mesh.triangles.clone(),
            )
            .map_err(|error| AdvancedGeometryAssetError::Invalid(error.to_string()))?;
            static_meshes.insert(id.to_owned(), runtime);
        }

        Ok(AdvancedGeometryRuntime {
            nav_mesh,
            static_meshes,
        })
    }
}

fn build_layer(
    layer: &AdvancedGeometryNavLayer,
) -> Result<NavMeshLayer, AdvancedGeometryAssetError> {
    if !layer.cell_size.is_finite() || layer.cell_size <= 0.0 {
        return Err(invalid_layer(layer, "cell size must be positive"));
    }
    if !layer.agent_radius.is_finite() || layer.agent_radius < 0.0 {
        return Err(invalid_layer(layer, "agent radius must be non-negative"));
    }
    if !layer.agent_height.is_finite() || layer.agent_height <= 0.0 {
        return Err(invalid_layer(layer, "agent height must be positive"));
    }
    if !layer.walkable_height.is_finite()
        || !array_is_finite(layer.world_min)
        || !array_is_finite(layer.world_max)
        || layer.world_max[0] <= layer.world_min[0]
        || layer.world_max[2] <= layer.world_min[2]
    {
        return Err(invalid_layer(layer, "world bounds are invalid"));
    }
    for obstacle in &layer.obstacles {
        if !array_is_finite(obstacle.center)
            || !array_is_finite(obstacle.half_extents)
            || obstacle.half_extents.iter().any(|value| *value < 0.0)
        {
            return Err(invalid_layer(layer, "contains an invalid obstacle"));
        }
    }

    let settings = NavMeshSettings {
        cell_size: layer.cell_size,
        agent_radius: layer.agent_radius,
        world_min: Vec3::from_array(layer.world_min),
        world_max: Vec3::from_array(layer.world_max),
        walkable_height: layer.walkable_height,
        agent_height: layer.agent_height,
    };
    let obstacles = layer
        .obstacles
        .iter()
        .map(|obstacle| {
            (
                Vec3::from_array(obstacle.center),
                Vec3::from_array(obstacle.half_extents),
            )
        })
        .collect::<Vec<_>>();
    Ok(NavMeshLayer {
        id: layer.id.clone(),
        minimum_height: layer.minimum_height,
        maximum_height: layer.maximum_height,
        nav_mesh: bake_from_obstacles(&obstacles, &settings),
    })
}

fn invalid_layer(
    layer: &AdvancedGeometryNavLayer,
    message: &str,
) -> AdvancedGeometryAssetError {
    AdvancedGeometryAssetError::Invalid(format!("layer `{}` {message}", layer.id))
}

fn array_is_finite(value: [f32; 3]) -> bool {
    value.into_iter().all(f32::is_finite)
}

/// Runtime objects constructed from one validated advanced-geometry document.
#[derive(Debug, Clone)]
pub struct AdvancedGeometryRuntime {
    nav_mesh: LayeredNavMesh,
    static_meshes: BTreeMap<String, StaticTriangleMesh>,
}

impl AdvancedGeometryRuntime {
    /// Returns the layered navigation query object.
    pub fn nav_mesh(&self) -> &LayeredNavMesh {
        &self.nav_mesh
    }

    /// Returns one named static mesh for raycasts or other spatial queries.
    pub fn static_mesh(&self, id: &str) -> Option<&StaticTriangleMesh> {
        self.static_meshes.get(id)
    }

    /// Iterates static mesh IDs in deterministic order.
    pub fn static_mesh_ids(&self) -> impl Iterator<Item = &str> {
        self.static_meshes.keys().map(String::as_str)
    }

    /// Separates the runtime into its navigation and mesh collections.
    pub fn into_parts(self) -> (LayeredNavMesh, BTreeMap<String, StaticTriangleMesh>) {
        (self.nav_mesh, self.static_meshes)
    }
}

/// Failure to load, validate, build, or save an advanced-geometry document.
#[derive(Debug)]
pub enum AdvancedGeometryAssetError {
    /// Reading or writing a document failed.
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// Authored data was rejected by schema or runtime validation.
    Invalid(String),
}

impl fmt::Display for AdvancedGeometryAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json(source) => source.fmt(formatter),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AdvancedGeometryAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            Self::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_document_roundtrips_and_builds_runtime_queries() {
        let document = AdvancedGeometryDocument::default();
        let json = document.to_json_string().expect("default document is valid");
        let decoded = AdvancedGeometryDocument::from_json_str(&json)
            .expect("serialized document should decode");
        assert_eq!(decoded, document);
        let runtime = decoded.build().expect("decoded document should build");
        assert_eq!(runtime.nav_mesh().layers().len(), 2);
        assert!(runtime.static_mesh("ground_probe").is_some());
    }

    #[test]
    fn duplicate_static_mesh_ids_are_rejected() {
        let mut document = AdvancedGeometryDocument::default();
        document.meshes.push(document.meshes[0].clone());
        let error = document.build().expect_err("duplicate IDs must fail");
        assert!(error.to_string().contains("duplicated"));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let document = AdvancedGeometryDocument {
            schema_version: ADVANCED_GEOMETRY_SCHEMA_VERSION + 1,
            ..AdvancedGeometryDocument::default()
        };
        let error = document.build().expect_err("future schema must fail");
        assert!(error.to_string().contains("unsupported advanced geometry schema"));
    }
}
