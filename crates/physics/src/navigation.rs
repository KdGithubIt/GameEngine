//! Production tiled polygon navigation, baking, queries, and path following.
//!
//! The runtime asset is engine-owned and backend-neutral. Authoring/build code
//! supplies [`NavigationBuildInput`]; [`NavigationBaker`] produces a versioned
//! tiled polygon asset that runtime queries can load without any bake backend.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use engine_ecs::{Query, Res};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::time::FixedTime;
use crate::transform::Transform;

/// Current runtime NavMesh asset schema.
pub const NAV_MESH_SCHEMA_VERSION: u32 = 2;
/// Default profile ID used by compatibility APIs and newly created agents.
pub const DEFAULT_NAVIGATION_PROFILE: &str = "default";
const DEFAULT_NEAREST_DISTANCE: f32 = 2.0;
const EDGE_QUANTIZATION: f32 = 10_000.0;

/// Stable project-local agent profile identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NavigationProfileId(pub String);

impl NavigationProfileId {
    /// Creates a profile ID after trimming surrounding whitespace.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().trim().to_owned())
    }

    /// Returns the persisted profile ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for NavigationProfileId {
    fn default() -> Self {
        Self(DEFAULT_NAVIGATION_PROFILE.to_owned())
    }
}

/// Per-profile bake constraints and runtime area costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationAgentProfile {
    /// Stable ID referenced by agents and links.
    pub id: NavigationProfileId,
    /// Human-readable authoring label.
    pub name: String,
    /// Horizontal agent radius.
    pub radius: f32,
    /// Required vertical clearance.
    pub height: f32,
    /// Steepest accepted triangle slope in degrees.
    pub max_slope_degrees: f32,
    /// Maximum authored/rasterized climb height.
    pub max_climb: f32,
    /// Area-cost overrides; absent areas cost `1.0`.
    pub area_costs: BTreeMap<u16, f32>,
}

impl Default for NavigationAgentProfile {
    fn default() -> Self {
        Self {
            id: NavigationProfileId::default(),
            name: "Default".to_owned(),
            radius: 0.4,
            height: 1.8,
            max_slope_degrees: 50.0,
            max_climb: 0.4,
            area_costs: BTreeMap::new(),
        }
    }
}

impl NavigationAgentProfile {
    fn area_cost(&self, area: u16) -> f32 {
        self.area_costs.get(&area).copied().unwrap_or(1.0)
    }

    fn validate(&self) -> Result<(), NavigationBakeError> {
        if self.id.as_str().is_empty() || self.name.trim().is_empty() {
            return Err(NavigationBakeError::InvalidProfile(
                self.id.as_str().to_owned(),
            ));
        }
        if !self.radius.is_finite()
            || self.radius < 0.0
            || !self.height.is_finite()
            || self.height <= 0.0
            || !self.max_slope_degrees.is_finite()
            || !(0.0..90.0).contains(&self.max_slope_degrees)
            || !self.max_climb.is_finite()
            || self.max_climb < 0.0
            || self
                .area_costs
                .values()
                .any(|cost| !cost.is_finite() || *cost <= 0.0)
        {
            return Err(NavigationBakeError::InvalidProfile(
                self.id.as_str().to_owned(),
            ));
        }
        Ok(())
    }
}

/// Engine-owned triangle supplied to the production baker.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NavigationTriangle {
    /// First world-space vertex.
    pub a: [f32; 3],
    /// Second world-space vertex.
    pub b: [f32; 3],
    /// Third world-space vertex.
    pub c: [f32; 3],
    /// Default traversal area assigned before modifiers.
    pub area: u16,
}

impl NavigationTriangle {
    fn points(self) -> [Vec3; 3] {
        [
            Vec3::from_array(self.a),
            Vec3::from_array(self.b),
            Vec3::from_array(self.c),
        ]
    }

    fn centroid(self) -> Vec3 {
        let [a, b, c] = self.points();
        (a + b + c) / 3.0
    }
}

/// Navigation modifier operation applied by world-space bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NavigationModifierMode {
    /// Removes matching polygons from the profile bake.
    Exclude,
    /// Reassigns the polygon area and multiplies its traversal cost.
    Area {
        /// Runtime area identifier.
        area: u16,
        /// Additional deterministic cost multiplier.
        cost_multiplier: f32,
    },
}

/// Backend-neutral navigation area modifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationModifier {
    /// Stable authoring identity used by diagnostics.
    pub id: String,
    /// Inclusive minimum world-space bound.
    pub minimum: [f32; 3],
    /// Inclusive maximum world-space bound.
    pub maximum: [f32; 3],
    /// Optional profile filter; empty applies to every profile.
    pub profiles: Vec<NavigationProfileId>,
    /// Modifier operation.
    pub mode: NavigationModifierMode,
}

impl NavigationModifier {
    fn applies_to(&self, profile: &NavigationProfileId, point: Vec3) -> bool {
        let minimum = Vec3::from_array(self.minimum);
        let maximum = Vec3::from_array(self.maximum);
        (self.profiles.is_empty() || self.profiles.contains(profile))
            && point.cmpge(minimum).all()
            && point.cmple(maximum).all()
    }
}

/// Backend-neutral off-mesh link supplied by scene authoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationBuildLink {
    /// Stable authoring/gameplay identity.
    pub id: String,
    /// World-space source endpoint.
    pub start: [f32; 3],
    /// World-space destination endpoint.
    pub end: [f32; 3],
    /// Whether the reverse traversal is also available.
    pub bidirectional: bool,
    /// Optional profile filter; empty applies to every profile.
    pub profiles: Vec<NavigationProfileId>,
    /// Area applied to the special traversal.
    pub area: u16,
    /// Additional traversal cost.
    pub cost: f32,
    /// Stable project-facing traversal tag such as `jump` or `door`.
    pub traversal_tag: String,
}

/// Deterministic backend-neutral input to a navigation baker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationBuildInput {
    /// Normalized source fingerprint recorded into the derived asset.
    pub source_fingerprint: String,
    /// World-space geometry considered for walkability.
    pub triangles: Vec<NavigationTriangle>,
    /// Area/exclusion modifiers.
    pub modifiers: Vec<NavigationModifier>,
    /// Explicit off-mesh traversals.
    pub links: Vec<NavigationBuildLink>,
    /// Stable agent profiles baked into independent tile sets.
    pub profiles: Vec<NavigationAgentProfile>,
    /// World-space XZ tile edge length.
    pub tile_size: f32,
}

impl NavigationBuildInput {
    /// Validates backend-neutral input before any bake backend is invoked.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationBakeError`] for invalid geometry, profiles, links,
    /// modifiers, or duplicate stable IDs.
    pub fn validate(&self) -> Result<(), NavigationBakeError> {
        if self.source_fingerprint.trim().is_empty()
            || !self.tile_size.is_finite()
            || self.tile_size <= 0.0
            || self.triangles.is_empty()
            || self.profiles.is_empty()
        {
            return Err(NavigationBakeError::InvalidInput);
        }
        let mut profiles = BTreeSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !profiles.insert(profile.id.clone()) {
                return Err(NavigationBakeError::DuplicateProfile(
                    profile.id.as_str().to_owned(),
                ));
            }
        }
        for (index, triangle) in self.triangles.iter().copied().enumerate() {
            let points = triangle.points();
            if points.iter().any(|point| !point.is_finite())
                || (points[1] - points[0])
                    .cross(points[2] - points[0])
                    .length_squared()
                    <= f32::EPSILON
            {
                return Err(NavigationBakeError::InvalidTriangle(index));
            }
        }
        let mut ids = BTreeSet::new();
        for link in &self.links {
            if link.id.trim().is_empty()
                || link.traversal_tag.trim().is_empty()
                || !Vec3::from_array(link.start).is_finite()
                || !Vec3::from_array(link.end).is_finite()
                || !link.cost.is_finite()
                || link.cost < 0.0
                || !ids.insert(format!("link:{}", link.id))
                || link.profiles.iter().any(|id| !profiles.contains(id))
            {
                return Err(NavigationBakeError::InvalidLink(link.id.clone()));
            }
        }
        for modifier in &self.modifiers {
            let minimum = Vec3::from_array(modifier.minimum);
            let maximum = Vec3::from_array(modifier.maximum);
            let valid_mode = match modifier.mode {
                NavigationModifierMode::Exclude => true,
                NavigationModifierMode::Area {
                    cost_multiplier, ..
                } => cost_multiplier.is_finite() && cost_multiplier > 0.0,
            };
            if modifier.id.trim().is_empty()
                || !minimum.is_finite()
                || !maximum.is_finite()
                || minimum.cmpgt(maximum).any()
                || !valid_mode
                || !ids.insert(format!("modifier:{}", modifier.id))
                || modifier.profiles.iter().any(|id| !profiles.contains(id))
            {
                return Err(NavigationBakeError::InvalidModifier(
                    modifier.id.clone(),
                ));
            }
        }
        Ok(())
    }
}

/// Stable independently replaceable tile coordinate.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct NavigationTileId {
    /// Tile coordinate along world X.
    pub x: i32,
    /// Tile coordinate along world Z.
    pub z: i32,
}

/// Runtime polygon identity within one stable tile.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct NavigationPolygonRef {
    /// Stable tile coordinate.
    pub tile: NavigationTileId,
    /// Tile-local polygon index.
    pub polygon: u32,
}

/// Portal to an adjacent polygon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationPortal {
    /// Adjacent polygon.
    pub to: NavigationPolygonRef,
    /// Left portal endpoint for corridor smoothing.
    pub left: [f32; 3],
    /// Right portal endpoint for corridor smoothing.
    pub right: [f32; 3],
}

/// One neutral convex runtime polygon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationPolygon {
    /// Tile-local vertex indices.
    pub vertices: Vec<u32>,
    /// Adjacency portals.
    pub portals: Vec<NavigationPortal>,
    /// Traversal area identifier.
    pub area: u16,
    /// Additional cost multiplier baked from modifiers.
    pub cost_multiplier: f32,
    /// Traversal flags reserved for future policy filtering.
    pub traversal_flags: u32,
}

/// One independently addressable runtime navigation tile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationTile {
    /// Stable tile coordinate.
    pub id: NavigationTileId,
    /// Inclusive world-space minimum bound.
    pub minimum: [f32; 3],
    /// Inclusive world-space maximum bound.
    pub maximum: [f32; 3],
    /// Tile-local vertices.
    pub vertices: Vec<[f32; 3]>,
    /// Tile-local polygons.
    pub polygons: Vec<NavigationPolygon>,
    /// Source/build hash for future local invalidation.
    pub build_fingerprint: String,
}

/// Runtime off-mesh traversal resolved to polygons for one profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationLink {
    /// Stable project-facing link identity.
    pub id: String,
    /// Source polygon.
    pub from: NavigationPolygonRef,
    /// Destination polygon.
    pub to: NavigationPolygonRef,
    /// World-space source endpoint.
    pub start: [f32; 3],
    /// World-space destination endpoint.
    pub end: [f32; 3],
    /// Whether reverse traversal is valid.
    pub bidirectional: bool,
    /// Traversal area.
    pub area: u16,
    /// Additional traversal cost.
    pub cost: f32,
    /// Stable project-defined traversal tag.
    pub traversal_tag: String,
}

/// Profile-specific tile set stored in one production asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationProfileMesh {
    /// Profile settings used for this tile set.
    pub profile: NavigationAgentProfile,
    /// Independently addressable tiles.
    pub tiles: Vec<NavigationTile>,
    /// Resolved off-mesh traversals.
    pub links: Vec<NavigationLink>,
}

/// Versioned tiled polygon navigation asset used by Editor Play and Player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavMesh {
    /// Serialized runtime schema version.
    pub schema_version: u32,
    /// Normalized authoring source fingerprint.
    pub source_fingerprint: String,
    /// Profile-specific tile sets.
    pub profiles: Vec<NavigationProfileMesh>,
}

impl NavMesh {
    /// Returns one profile mesh by stable profile ID.
    pub fn profile(&self, id: &str) -> Option<&NavigationProfileMesh> {
        self.profiles
            .iter()
            .find(|profile| profile.profile.id.as_str() == id)
    }

    /// Iterates every polygon as `(reference, tile, polygon)` tuples.
    pub fn polygons(
        &self,
        profile_id: &str,
    ) -> impl Iterator<Item = (NavigationPolygonRef, &NavigationTile, &NavigationPolygon)> {
        self.profile(profile_id)
            .into_iter()
            .flat_map(|profile| profile.tiles.iter())
            .flat_map(|tile| {
                tile.polygons
                    .iter()
                    .enumerate()
                    .map(move |(index, polygon)| {
                        (
                            NavigationPolygonRef {
                                tile: tile.id,
                                polygon: index as u32,
                            },
                            tile,
                            polygon,
                        )
                    })
            })
    }

    fn validate(&self) -> Result<(), NavMeshError> {
        if self.schema_version != NAV_MESH_SCHEMA_VERSION
            || self.source_fingerprint.trim().is_empty()
            || self.profiles.is_empty()
        {
            return Err(NavMeshError::InvalidAsset(
                "unsupported or incomplete production NavMesh".to_owned(),
            ));
        }
        let mut profile_ids = BTreeSet::new();
        for profile in &self.profiles {
            profile
                .profile
                .validate()
                .map_err(|error| NavMeshError::InvalidAsset(error.to_string()))?;
            if !profile_ids.insert(profile.profile.id.clone()) {
                return Err(NavMeshError::InvalidAsset(format!(
                    "duplicate navigation profile `{}`",
                    profile.profile.id.as_str()
                )));
            }
            let mut tile_ids = BTreeSet::new();
            for tile in &profile.tiles {
                if !tile_ids.insert(tile.id) {
                    return Err(NavMeshError::InvalidAsset(format!(
                        "duplicate navigation tile ({}, {})",
                        tile.id.x, tile.id.z
                    )));
                }
                for polygon in &tile.polygons {
                    if polygon.vertices.len() < 3
                        || polygon
                            .vertices
                            .iter()
                            .any(|index| *index as usize >= tile.vertices.len())
                        || !polygon.cost_multiplier.is_finite()
                        || polygon.cost_multiplier <= 0.0
                    {
                        return Err(NavMeshError::InvalidAsset(
                            "navigation polygon contains invalid indices or cost".to_owned(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Failure produced while validating or baking [`NavigationBuildInput`].
#[derive(Debug, Clone, PartialEq)]
pub enum NavigationBakeError {
    /// Top-level input is missing required deterministic data.
    InvalidInput,
    /// A triangle is non-finite or degenerate.
    InvalidTriangle(usize),
    /// A profile contains invalid settings.
    InvalidProfile(String),
    /// A stable profile ID occurs more than once.
    DuplicateProfile(String),
    /// A link contains invalid endpoints, cost, ID, tag, or profile filters.
    InvalidLink(String),
    /// A modifier contains invalid bounds, cost, ID, or profile filters.
    InvalidModifier(String),
    /// Cancellation was requested before producing an asset.
    Cancelled,
}

impl fmt::Display for NavigationBakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("invalid navigation build input"),
            Self::InvalidTriangle(index) => {
                write!(formatter, "invalid navigation triangle {index}")
            }
            Self::InvalidProfile(id) => write!(formatter, "invalid navigation profile `{id}`"),
            Self::DuplicateProfile(id) => {
                write!(formatter, "duplicate navigation profile `{id}`")
            }
            Self::InvalidLink(id) => write!(formatter, "invalid navigation link `{id}`"),
            Self::InvalidModifier(id) => {
                write!(formatter, "invalid navigation modifier `{id}`")
            }
            Self::Cancelled => formatter.write_str("navigation bake cancelled"),
        }
    }
}

impl std::error::Error for NavigationBakeError {}

/// Backend boundary that converts neutral build input into the runtime asset.
pub trait NavigationBaker {
    /// Bakes one deterministic production asset.
    ///
    /// # Errors
    ///
    /// Returns [`NavigationBakeError`] when input is invalid or cancellation is
    /// observed.
    fn bake(
        &self,
        input: &NavigationBuildInput,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<NavMesh, NavigationBakeError>;
}

/// Engine-owned deterministic polygon baker used by the first production release.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProductionNavigationBaker;

impl NavigationBaker for ProductionNavigationBaker {
    fn bake(
        &self,
        input: &NavigationBuildInput,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<NavMesh, NavigationBakeError> {
        input.validate()?;
        if cancelled() {
            return Err(NavigationBakeError::Cancelled);
        }
        let mut profiles = Vec::with_capacity(input.profiles.len());
        for profile in &input.profiles {
            if cancelled() {
                return Err(NavigationBakeError::Cancelled);
            }
            profiles.push(bake_profile(input, profile));
        }
        Ok(NavMesh {
            schema_version: NAV_MESH_SCHEMA_VERSION,
            source_fingerprint: input.source_fingerprint.clone(),
            profiles,
        })
    }
}

#[derive(Clone)]
struct BakedTriangle {
    points: [Vec3; 3],
    area: u16,
    cost_multiplier: f32,
}

fn bake_profile(
    input: &NavigationBuildInput,
    profile: &NavigationAgentProfile,
) -> NavigationProfileMesh {
    let mut accepted = Vec::new();
    for triangle in input.triangles.iter().copied() {
        let points = triangle.points();
        if !triangle_is_walkable(points, profile) || !has_clearance(triangle, input, profile) {
            continue;
        }
        let centroid = triangle.centroid();
        let mut area = triangle.area;
        let mut cost_multiplier = 1.0;
        let mut excluded = false;
        for modifier in &input.modifiers {
            if !modifier.applies_to(&profile.id, centroid) {
                continue;
            }
            match modifier.mode {
                NavigationModifierMode::Exclude => excluded = true,
                NavigationModifierMode::Area {
                    area: replacement,
                    cost_multiplier: multiplier,
                } => {
                    area = replacement;
                    cost_multiplier *= multiplier;
                }
            }
        }
        if !excluded {
            accepted.push(BakedTriangle {
                points,
                area,
                cost_multiplier,
            });
        }
    }

    let mut grouped: BTreeMap<NavigationTileId, Vec<BakedTriangle>> = BTreeMap::new();
    for triangle in accepted {
        let centroid = (triangle.points[0] + triangle.points[1] + triangle.points[2]) / 3.0;
        let id = NavigationTileId {
            x: (centroid.x / input.tile_size).floor() as i32,
            z: (centroid.z / input.tile_size).floor() as i32,
        };
        grouped.entry(id).or_default().push(triangle);
    }

    let mut tiles = Vec::with_capacity(grouped.len());
    for (id, triangles) in grouped {
        let mut vertices = Vec::with_capacity(triangles.len() * 3);
        let mut polygons = Vec::with_capacity(triangles.len());
        let mut minimum = Vec3::splat(f32::INFINITY);
        let mut maximum = Vec3::splat(f32::NEG_INFINITY);
        for triangle in triangles {
            let base = vertices.len() as u32;
            for point in triangle.points {
                minimum = minimum.min(point);
                maximum = maximum.max(point);
                vertices.push(point.to_array());
            }
            polygons.push(NavigationPolygon {
                vertices: vec![base, base + 1, base + 2],
                portals: Vec::new(),
                area: triangle.area,
                cost_multiplier: triangle.cost_multiplier,
                traversal_flags: u32::MAX,
            });
        }
        tiles.push(NavigationTile {
            id,
            minimum: minimum.to_array(),
            maximum: maximum.to_array(),
            vertices,
            polygons,
            build_fingerprint: format!("{}:{}:{}", input.source_fingerprint, id.x, id.z),
        });
    }
    build_adjacency(&mut tiles, profile);

    let mut profile_mesh = NavigationProfileMesh {
        profile: profile.clone(),
        tiles,
        links: Vec::new(),
    };
    for link in &input.links {
        if !link.profiles.is_empty() && !link.profiles.contains(&profile.id) {
            continue;
        }
        let start = Vec3::from_array(link.start);
        let end = Vec3::from_array(link.end);
        let Some((from, _)) = nearest_polygon_in_profile(&profile_mesh, start) else {
            continue;
        };
        let Some((to, _)) = nearest_polygon_in_profile(&profile_mesh, end) else {
            continue;
        };
        profile_mesh.links.push(NavigationLink {
            id: link.id.clone(),
            from,
            to,
            start: link.start,
            end: link.end,
            bidirectional: link.bidirectional,
            area: link.area,
            cost: link.cost,
            traversal_tag: link.traversal_tag.clone(),
        });
    }
    profile_mesh.links.sort_by(|left, right| left.id.cmp(&right.id));
    profile_mesh
}

fn triangle_is_walkable(points: [Vec3; 3], profile: &NavigationAgentProfile) -> bool {
    let edge_a = points[1] - points[0];
    let edge_b = points[2] - points[0];
    let normal = edge_a.cross(edge_b).try_normalize().unwrap_or(Vec3::ZERO);
    let slope_cosine = profile.max_slope_degrees.to_radians().cos();
    normal.y.abs() >= slope_cosine
}

fn has_clearance(
    candidate: NavigationTriangle,
    input: &NavigationBuildInput,
    profile: &NavigationAgentProfile,
) -> bool {
    let point = candidate.centroid();
    for ceiling in input.triangles.iter().copied() {
        if ceiling == candidate {
            continue;
        }
        let ceiling_points = ceiling.points();
        let minimum_y = ceiling_points
            .iter()
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);
        if minimum_y <= point.y + 1e-4 || minimum_y - point.y >= profile.height {
            continue;
        }
        if point_in_triangle_xz(point, ceiling_points) {
            return false;
        }
    }
    true
}

fn point_in_triangle_xz(point: Vec3, triangle: [Vec3; 3]) -> bool {
    let p = Vec2::new(point.x, point.z);
    let a = Vec2::new(triangle[0].x, triangle[0].z);
    let b = Vec2::new(triangle[1].x, triangle[1].z);
    let c = Vec2::new(triangle[2].x, triangle[2].z);
    let ab = cross_2d(b - a, p - a);
    let bc = cross_2d(c - b, p - b);
    let ca = cross_2d(a - c, p - c);
    (ab >= -1e-5 && bc >= -1e-5 && ca >= -1e-5)
        || (ab <= 1e-5 && bc <= 1e-5 && ca <= 1e-5)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct QuantizedPointXZ(i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey(QuantizedPointXZ, QuantizedPointXZ);

fn quantize_xz(point: Vec3) -> QuantizedPointXZ {
    QuantizedPointXZ(
        (point.x * EDGE_QUANTIZATION).round() as i32,
        (point.z * EDGE_QUANTIZATION).round() as i32,
    )
}

fn canonical_edge(key: EdgeKey, a: Vec3, b: Vec3) -> (Vec3, Vec3) {
    if quantize_xz(a) == key.0 {
        (a, b)
    } else {
        (b, a)
    }
}

fn build_adjacency(tiles: &mut [NavigationTile], profile: &NavigationAgentProfile) {
    let mut edges: BTreeMap<EdgeKey, Vec<(NavigationPolygonRef, Vec3, Vec3)>> = BTreeMap::new();
    let mut centers = HashMap::new();
    for tile in tiles.iter() {
        for (polygon_index, polygon) in tile.polygons.iter().enumerate() {
            let reference = NavigationPolygonRef {
                tile: tile.id,
                polygon: polygon_index as u32,
            };
            let points = polygon
                .vertices
                .iter()
                .map(|index| Vec3::from_array(tile.vertices[*index as usize]))
                .collect::<Vec<_>>();
            let center = points.iter().copied().sum::<Vec3>() / points.len() as f32;
            centers.insert(reference, center);
            for edge_index in 0..points.len() {
                let a = points[edge_index];
                let b = points[(edge_index + 1) % points.len()];
                let qa = quantize_xz(a);
                let qb = quantize_xz(b);
                let key = if qa <= qb {
                    EdgeKey(qa, qb)
                } else {
                    EdgeKey(qb, qa)
                };
                edges.entry(key).or_default().push((reference, a, b));
            }
        }
    }

    let index = polygon_index(tiles);
    for (key, shared) in edges.iter().filter(|(_, entries)| entries.len() > 1) {
        for first_index in 0..shared.len() {
            for second_index in (first_index + 1)..shared.len() {
                let (first, first_a, first_b) = shared[first_index];
                let (second, second_a, second_b) = shared[second_index];
                if first == second {
                    continue;
                }
                let (first_low, first_high) = canonical_edge(*key, first_a, first_b);
                let (second_low, second_high) = canonical_edge(*key, second_a, second_b);
                let climb = (first_low.y - second_low.y)
                    .abs()
                    .max((first_high.y - second_high.y).abs());
                let portal_width = Vec2::new(
                    first_high.x - first_low.x,
                    first_high.z - first_low.z,
                )
                .length();
                if climb > profile.max_climb + 1e-4
                    || portal_width + 1e-4 < profile.radius * 2.0
                {
                    continue;
                }
                let Some(first_center) = centers.get(&first).copied() else {
                    continue;
                };
                let Some(second_center) = centers.get(&second).copied() else {
                    continue;
                };
                let (first_left, first_right) =
                    orient_portal(first_a, first_b, first_center, second_center);
                let (second_left, second_right) =
                    orient_portal(second_a, second_b, second_center, first_center);
                if let Some((tile_index, polygon_index)) = index.get(&first).copied() {
                    tiles[tile_index].polygons[polygon_index]
                        .portals
                        .push(NavigationPortal {
                            to: second,
                            left: first_left.to_array(),
                            right: first_right.to_array(),
                        });
                }
                if let Some((tile_index, polygon_index)) = index.get(&second).copied() {
                    tiles[tile_index].polygons[polygon_index]
                        .portals
                        .push(NavigationPortal {
                            to: first,
                            left: second_left.to_array(),
                            right: second_right.to_array(),
                        });
                }
            }
        }
    }
    for tile in tiles {
        for polygon in &mut tile.polygons {
            polygon.portals.sort_by_key(|portal| portal.to);
            polygon.portals.dedup_by_key(|portal| portal.to);
        }
    }
}

fn orient_portal(a: Vec3, b: Vec3, from: Vec3, to: Vec3) -> (Vec3, Vec3) {
    let direction = Vec2::new(to.x - from.x, to.z - from.z);
    let edge = Vec2::new(b.x - a.x, b.z - a.z);
    if cross_2d(direction, edge) >= 0.0 {
        (a, b)
    } else {
        (b, a)
    }
}

fn polygon_index(tiles: &[NavigationTile]) -> HashMap<NavigationPolygonRef, (usize, usize)> {
    let mut index = HashMap::new();
    for (tile_index, tile) in tiles.iter().enumerate() {
        for polygon_index in 0..tile.polygons.len() {
            index.insert(
                NavigationPolygonRef {
                    tile: tile.id,
                    polygon: polygon_index as u32,
                },
                (tile_index, polygon_index),
            );
        }
    }
    index
}

/// Typed failure from a production path query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationQueryFailure {
    /// Requested profile does not exist in the loaded asset.
    MissingProfile(String),
    /// No production navigation data is available for the profile.
    MissingNavigationData,
    /// Start position cannot be projected to nearby navigation.
    StartOutside,
    /// Destination cannot be projected to any navigation polygon.
    EndOutside,
    /// Both endpoints resolve but no topological route exists.
    NoPath,
}

/// Successful or partial production navigation path.
#[derive(Debug, Clone, PartialEq)]
pub struct NavigationPath {
    /// Smoothed world-space steering points.
    pub waypoints: Vec<Vec3>,
    /// Polygon corridor retained for diagnostics and Editor path testing.
    pub corridor: Vec<NavigationPolygonRef>,
    /// Stable off-mesh link IDs used by this path, in traversal order.
    pub links: Vec<String>,
    /// Deterministic traversal cost including areas and off-mesh links.
    pub total_cost: f32,
}

/// Typed path-query result.
#[derive(Debug, Clone, PartialEq)]
pub enum NavigationPathResult {
    /// Destination was reached through the selected profile.
    Complete(NavigationPath),
    /// A best reachable point was found but the requested destination was not.
    Partial(NavigationPath),
    /// Query could not produce a usable route.
    Failure(NavigationQueryFailure),
}

/// Engine-owned query-time traversal overrides for dynamic links such as doors.
///
/// Stable link IDs and neutral traversal tags keep runtime state independent of
/// any third-party polygon/reference representation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationQueryFilter {
    disabled_link_ids: BTreeSet<String>,
    disabled_traversal_tags: BTreeSet<String>,
}

impl NavigationQueryFilter {
    /// Disables one authored off-mesh link by stable ID.
    pub fn disable_link(&mut self, id: impl Into<String>) {
        self.disabled_link_ids.insert(id.into());
    }

    /// Re-enables one previously disabled off-mesh link.
    pub fn enable_link(&mut self, id: &str) {
        self.disabled_link_ids.remove(id);
    }

    /// Disables all links carrying one neutral traversal tag, for example `door`.
    pub fn disable_traversal_tag(&mut self, tag: impl Into<String>) {
        self.disabled_traversal_tags.insert(tag.into());
    }

    /// Re-enables one previously disabled traversal tag.
    pub fn enable_traversal_tag(&mut self, tag: &str) {
        self.disabled_traversal_tags.remove(tag);
    }

    /// Returns whether a baked link is currently traversable.
    pub fn allows_link(&self, link: &NavigationLink) -> bool {
        !self.disabled_link_ids.contains(&link.id)
            && !self.disabled_traversal_tags.contains(&link.traversal_tag)
    }
}

/// Runtime resource wrapping one loaded production navigation asset.
pub struct NavMeshQuery {
    /// Underlying tiled production asset.
    pub nav_mesh: NavMesh,
}

impl NavMeshQuery {
    /// Creates a query resource from a validated production asset.
    pub fn new(nav_mesh: NavMesh) -> Self {
        Self { nav_mesh }
    }

    /// Finds a complete path using the default profile.
    ///
    /// This compatibility method returns `None` for typed failures and partial
    /// paths. Use [`Self::query_path`] when failure/partial detail matters.
    pub fn find_path(&self, start: Vec3, end: Vec3) -> Option<Vec<Vec3>> {
        match self.query_path(DEFAULT_NAVIGATION_PROFILE, start, end) {
            NavigationPathResult::Complete(path) => Some(path.waypoints),
            NavigationPathResult::Partial(_) | NavigationPathResult::Failure(_) => None,
        }
    }

    /// Projects `point` to the nearest polygon for one profile.
    pub fn nearest_point(
        &self,
        profile_id: &str,
        point: Vec3,
        maximum_distance: f32,
    ) -> Result<(Vec3, NavigationPolygonRef), NavigationQueryFailure> {
        let profile = self
            .nav_mesh
            .profile(profile_id)
            .ok_or_else(|| NavigationQueryFailure::MissingProfile(profile_id.to_owned()))?;
        if profile.tiles.is_empty() {
            return Err(NavigationQueryFailure::MissingNavigationData);
        }
        let (reference, nearest) = nearest_polygon_in_profile(profile, point)
            .ok_or(NavigationQueryFailure::MissingNavigationData)?;
        if point.distance(nearest) > maximum_distance.max(0.0) {
            return Err(NavigationQueryFailure::StartOutside);
        }
        Ok((nearest, reference))
    }

    /// Runs a typed corridor/path query for one stable agent profile.
    pub fn query_path(&self, profile_id: &str, start: Vec3, end: Vec3) -> NavigationPathResult {
        self.query_path_with_filter(profile_id, start, end, &NavigationQueryFilter::default())
    }

    /// Runs a typed path query while applying engine-owned dynamic traversal overrides.
    pub fn query_path_with_filter(
        &self,
        profile_id: &str,
        start: Vec3,
        end: Vec3,
        filter: &NavigationQueryFilter,
    ) -> NavigationPathResult {
        let Some(profile) = self.nav_mesh.profile(profile_id) else {
            return NavigationPathResult::Failure(NavigationQueryFailure::MissingProfile(
                profile_id.to_owned(),
            ));
        };
        if profile.tiles.is_empty() {
            return NavigationPathResult::Failure(NavigationQueryFailure::MissingNavigationData);
        }
        let Some((start_ref, start_point)) = nearest_polygon_in_profile(profile, start) else {
            return NavigationPathResult::Failure(NavigationQueryFailure::MissingNavigationData);
        };
        if start.distance(start_point) > DEFAULT_NEAREST_DISTANCE {
            return NavigationPathResult::Failure(NavigationQueryFailure::StartOutside);
        }
        let Some((end_ref, end_point)) = nearest_polygon_in_profile(profile, end) else {
            return NavigationPathResult::Failure(NavigationQueryFailure::EndOutside);
        };
        if end.distance(end_point) > DEFAULT_NEAREST_DISTANCE {
            return NavigationPathResult::Failure(NavigationQueryFailure::EndOutside);
        }
        match astar_polygons(profile, start_ref, end_ref, filter) {
            Some(SearchOutcome::Complete(search)) => NavigationPathResult::Complete(
                materialize_path(profile, start_point, end_point, search),
            ),
            Some(SearchOutcome::Partial(search)) => {
                let Some(partial_ref) = search.corridor.last().copied() else {
                    return NavigationPathResult::Failure(NavigationQueryFailure::NoPath);
                };
                let Some(partial_end) = closest_point_on_polygon(profile, partial_ref, end) else {
                    return NavigationPathResult::Failure(NavigationQueryFailure::NoPath);
                };
                NavigationPathResult::Partial(materialize_path(
                    profile,
                    start_point,
                    partial_end,
                    search,
                ))
            }
            None => NavigationPathResult::Failure(NavigationQueryFailure::NoPath),
        }
    }
}

fn nearest_polygon_in_profile(
    profile: &NavigationProfileMesh,
    point: Vec3,
) -> Option<(NavigationPolygonRef, Vec3)> {
    let mut best: Option<(NavigationPolygonRef, Vec3, f32)> = None;
    for tile in &profile.tiles {
        for (polygon_index, polygon) in tile.polygons.iter().enumerate() {
            let points = polygon
                .vertices
                .iter()
                .map(|index| Vec3::from_array(tile.vertices[*index as usize]))
                .collect::<Vec<_>>();
            if points.len() != 3 {
                continue;
            }
            let nearest = closest_point_on_triangle(point, points[0], points[1], points[2]);
            let distance = point.distance_squared(nearest);
            if best
                .as_ref()
                .is_none_or(|(_, _, best_distance)| distance < *best_distance)
            {
                best = Some((
                    NavigationPolygonRef {
                        tile: tile.id,
                        polygon: polygon_index as u32,
                    },
                    nearest,
                    distance,
                ));
            }
        }
    }
    best.map(|(reference, nearest, _)| (reference, nearest))
}

fn closest_point_on_polygon(
    profile: &NavigationProfileMesh,
    reference: NavigationPolygonRef,
    point: Vec3,
) -> Option<Vec3> {
    let tile = profile.tiles.iter().find(|tile| tile.id == reference.tile)?;
    let polygon = tile.polygons.get(reference.polygon as usize)?;
    if polygon.vertices.len() != 3 {
        return None;
    }
    let a = Vec3::from_array(tile.vertices[*polygon.vertices.first()? as usize]);
    let b = Vec3::from_array(tile.vertices[*polygon.vertices.get(1)? as usize]);
    let c = Vec3::from_array(tile.vertices[*polygon.vertices.get(2)? as usize]);
    Some(closest_point_on_triangle(point, a, b, c))
}

fn closest_point_on_triangle(point: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = point - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = point - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = point - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let bc = c - b;
        return b + bc * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denominator = (va + vb + vc).recip();
    let v = vb * denominator;
    let w = vc * denominator;
    a + ab * v + ac * w
}

#[derive(Debug, Clone)]
struct SearchResult {
    corridor: Vec<NavigationPolygonRef>,
    edges: Vec<SearchEdge>,
    total_cost: f32,
}

#[derive(Debug, Clone)]
enum SearchEdge {
    Portal(NavigationPortal),
    Link(NavigationLink, bool),
}

#[derive(Debug, Clone, Copy)]
struct OpenEntry {
    estimated_total: f32,
    cost: f32,
    node: NavigationPolygonRef,
}

impl PartialEq for OpenEntry {
    fn eq(&self, other: &Self) -> bool {
        self.estimated_total.to_bits() == other.estimated_total.to_bits()
            && self.cost.to_bits() == other.cost.to_bits()
            && self.node == other.node
    }
}

impl Eq for OpenEntry {}

impl Ord for OpenEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .estimated_total
            .total_cmp(&self.estimated_total)
            .then_with(|| other.cost.total_cmp(&self.cost))
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for OpenEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone)]
enum SearchOutcome {
    Complete(SearchResult),
    Partial(SearchResult),
}

struct SearchFrontier {
    open: BinaryHeap<OpenEntry>,
    costs: HashMap<NavigationPolygonRef, f32>,
    previous: HashMap<NavigationPolygonRef, (NavigationPolygonRef, SearchEdge)>,
}

impl SearchFrontier {
    fn new(start: NavigationPolygonRef, estimated_total: f32) -> Self {
        let mut costs = HashMap::new();
        costs.insert(start, 0.0);
        let mut open = BinaryHeap::new();
        open.push(OpenEntry {
            estimated_total,
            cost: 0.0,
            node: start,
        });
        Self {
            open,
            costs,
            previous: HashMap::new(),
        }
    }

    fn relax(
        &mut self,
        next: NavigationPolygonRef,
        current: NavigationPolygonRef,
        edge: SearchEdge,
        new_cost: f32,
        end_center: Vec3,
        centers: &HashMap<NavigationPolygonRef, Vec3>,
    ) {
        if new_cost + 1e-5 >= self.costs.get(&next).copied().unwrap_or(f32::INFINITY) {
            return;
        }
        self.costs.insert(next, new_cost);
        self.previous.insert(next, (current, edge));
        let heuristic = centers
            .get(&next)
            .map_or(0.0, |center| center.distance(end_center));
        self.open.push(OpenEntry {
            estimated_total: new_cost + heuristic,
            cost: new_cost,
            node: next,
        });
    }
}

fn astar_polygons(
    profile: &NavigationProfileMesh,
    start: NavigationPolygonRef,
    end: NavigationPolygonRef,
    filter: &NavigationQueryFilter,
) -> Option<SearchOutcome> {
    let index = polygon_index(&profile.tiles);
    let centers = polygon_centers(profile, &index);
    let end_center = *centers.get(&end)?;
    let mut best_partial = start;
    let mut best_distance = centers.get(&start)?.distance_squared(end_center);
    let mut frontier = SearchFrontier::new(start, best_distance.sqrt());

    while let Some(entry) = frontier.open.pop() {
        if frontier
            .costs
            .get(&entry.node)
            .is_some_and(|best| entry.cost > *best + 1e-5)
        {
            continue;
        }
        let distance = centers.get(&entry.node)?.distance_squared(end_center);
        if distance + 1e-5 < best_distance
            || ((distance - best_distance).abs() <= 1e-5 && entry.node < best_partial)
        {
            best_distance = distance;
            best_partial = entry.node;
        }
        if entry.node == end {
            return Some(SearchOutcome::Complete(reconstruct_search(
                end,
                entry.cost,
                &frontier.previous,
            )));
        }
        let (tile_index, polygon_index) = *index.get(&entry.node)?;
        let polygon = &profile.tiles[tile_index].polygons[polygon_index];
        let from_center = *centers.get(&entry.node)?;
        for portal in &polygon.portals {
            let to_center = *centers.get(&portal.to)?;
            let (destination_tile_index, destination_polygon_index) = *index.get(&portal.to)?;
            let destination_polygon =
                &profile.tiles[destination_tile_index].polygons[destination_polygon_index];
            let step = from_center.distance(to_center)
                * profile.profile.area_cost(destination_polygon.area)
                * destination_polygon.cost_multiplier;
            frontier.relax(
                portal.to,
                entry.node,
                SearchEdge::Portal(portal.clone()),
                entry.cost + step,
                end_center,
                &centers,
            );
        }
        for link in &profile.links {
            if !filter.allows_link(link) {
                continue;
            }
            if link.from == entry.node {
                frontier.relax(
                    link.to,
                    entry.node,
                    SearchEdge::Link(link.clone(), false),
                    entry.cost
                        + link.cost
                        + profile.profile.area_cost(link.area)
                            * Vec3::from_array(link.start).distance(Vec3::from_array(link.end)),
                    end_center,
                    &centers,
                );
            }
            if link.bidirectional && link.to == entry.node {
                frontier.relax(
                    link.from,
                    entry.node,
                    SearchEdge::Link(link.clone(), true),
                    entry.cost
                        + link.cost
                        + profile.profile.area_cost(link.area)
                            * Vec3::from_array(link.start).distance(Vec3::from_array(link.end)),
                    end_center,
                    &centers,
                );
            }
        }
    }
    if best_partial == start {
        return None;
    }
    let cost = frontier.costs.get(&best_partial).copied()?;
    Some(SearchOutcome::Partial(reconstruct_search(
        best_partial,
        cost,
        &frontier.previous,
    )))
}

fn reconstruct_search(
    destination: NavigationPolygonRef,
    total_cost: f32,
    previous: &HashMap<NavigationPolygonRef, (NavigationPolygonRef, SearchEdge)>,
) -> SearchResult {
    let mut corridor = vec![destination];
    let mut edges = Vec::new();
    let mut current = destination;
    while let Some((parent, edge)) = previous.get(&current).cloned() {
        corridor.push(parent);
        edges.push(edge);
        current = parent;
    }
    corridor.reverse();
    edges.reverse();
    SearchResult {
        corridor,
        edges,
        total_cost,
    }
}

fn polygon_centers(
    profile: &NavigationProfileMesh,
    index: &HashMap<NavigationPolygonRef, (usize, usize)>,
) -> HashMap<NavigationPolygonRef, Vec3> {
    index
        .iter()
        .map(|(reference, (tile_index, polygon_index))| {
            let tile = &profile.tiles[*tile_index];
            let polygon = &tile.polygons[*polygon_index];
            let center = polygon
                .vertices
                .iter()
                .map(|vertex| Vec3::from_array(tile.vertices[*vertex as usize]))
                .sum::<Vec3>()
                / polygon.vertices.len() as f32;
            (*reference, center)
        })
        .collect()
}

fn materialize_path(
    _profile: &NavigationProfileMesh,
    start: Vec3,
    end: Vec3,
    search: SearchResult,
) -> NavigationPath {
    let mut waypoints = vec![start];
    let mut links = Vec::new();
    let mut portal_run = Vec::new();
    for edge in &search.edges {
        match edge {
            SearchEdge::Portal(portal) => portal_run.push(portal.clone()),
            SearchEdge::Link(link, reversed) => {
                append_funnel_run(&mut waypoints, &portal_run, None);
                portal_run.clear();
                let (from, to) = if *reversed {
                    (Vec3::from_array(link.end), Vec3::from_array(link.start))
                } else {
                    (Vec3::from_array(link.start), Vec3::from_array(link.end))
                };
                push_distinct(&mut waypoints, from);
                push_distinct(&mut waypoints, to);
                links.push(link.id.clone());
            }
        }
    }
    append_funnel_run(&mut waypoints, &portal_run, Some(end));
    push_distinct(&mut waypoints, end);
    NavigationPath {
        waypoints,
        corridor: search.corridor,
        links,
        total_cost: search.total_cost,
    }
}

fn append_funnel_run(
    path: &mut Vec<Vec3>,
    portals: &[NavigationPortal],
    destination: Option<Vec3>,
) {
    let Some(mut apex) = path.last().copied() else {
        return;
    };
    if portals.is_empty() {
        if let Some(destination) = destination {
            push_distinct(path, destination);
        }
        return;
    }
    let mut left = Vec3::from_array(portals[0].left);
    let mut right = Vec3::from_array(portals[0].right);
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    let mut index = 1usize;
    while index <= portals.len() {
        let (next_left, next_right) = if let Some(portal) = portals.get(index) {
            (
                Vec3::from_array(portal.left),
                Vec3::from_array(portal.right),
            )
        } else {
            let target = destination.unwrap_or_else(|| {
                let last = &portals[portals.len() - 1];
                (Vec3::from_array(last.left) + Vec3::from_array(last.right)) * 0.5
            });
            (target, target)
        };

        if tri_area_xz(apex, right, next_right) <= 0.0 {
            if apex == right || tri_area_xz(apex, left, next_right) > 0.0 {
                right = next_right;
                right_index = index;
            } else {
                push_distinct(path, left);
                apex = left;
                index = left_index + 1;
                left = apex;
                right = apex;
                left_index = index;
                right_index = index;
                continue;
            }
        }
        if tri_area_xz(apex, left, next_left) >= 0.0 {
            if apex == left || tri_area_xz(apex, right, next_left) < 0.0 {
                left = next_left;
                left_index = index;
            } else {
                push_distinct(path, right);
                apex = right;
                index = right_index + 1;
                left = apex;
                right = apex;
                left_index = index;
                right_index = index;
                continue;
            }
        }
        index += 1;
    }
}

fn tri_area_xz(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    cross_2d(
        Vec2::new(b.x - a.x, b.z - a.z),
        Vec2::new(c.x - a.x, c.z - a.z),
    )
}

fn cross_2d(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn push_distinct(path: &mut Vec<Vec3>, point: Vec3) {
    if path.last().is_none_or(|last| last.distance_squared(point) > 1e-8) {
        path.push(point);
    }
}

/// Scene-owned reference to the baked navigation artifact used at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavMeshSurface {
    /// Resolved project asset path retained for Play inspection and diagnostics.
    pub source: PathBuf,
}

/// Production navigation state for one [`NavMeshAgent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavMeshAgentStatus {
    /// The agent has no target and no path.
    Idle,
    /// A target exists but the runtime has no baked navigation resource.
    MissingNavMesh,
    /// The selected stable profile is missing from the loaded asset.
    MissingProfile,
    /// The start position cannot be projected onto navigation for this profile.
    StartOutside,
    /// The destination cannot be projected onto navigation for this profile.
    EndOutside,
    /// No usable route could be produced for the current target.
    NoPath,
    /// The agent is following a complete route.
    Moving,
    /// The agent is following the best available partial corridor.
    PartialPath,
    /// The agent reached the target's stopping distance.
    Arrived,
}

/// Moves an entity along paths from the production [`NavMeshQuery`].
pub struct NavMeshAgent {
    /// World-space destination.
    pub target: Option<Vec3>,
    /// Stable profile ID selecting one baked tile set.
    pub profile_id: NavigationProfileId,
    /// Movement speed in world units per second.
    pub speed: f32,
    /// Distance to the final waypoint at which the agent stops.
    pub stopping_distance: f32,
    /// Seconds between path refreshes while a target remains active.
    pub repath_interval: f32,
    /// Radius used for lightweight local agent separation.
    pub avoidance_radius: f32,
    /// Latest runtime navigation state.
    pub status: NavMeshAgentStatus,
    current_path: Vec<Vec3>,
    path_index: usize,
    computed_target: Option<Vec3>,
    repath_remaining: f32,
    current_path_partial: bool,
}

impl NavMeshAgent {
    /// Creates an agent using [`DEFAULT_NAVIGATION_PROFILE`].
    pub fn new(speed: f32) -> Self {
        Self {
            target: None,
            profile_id: NavigationProfileId::default(),
            speed,
            stopping_distance: 0.1,
            repath_interval: 0.5,
            avoidance_radius: 0.4,
            status: NavMeshAgentStatus::Idle,
            current_path: Vec::new(),
            path_index: 0,
            computed_target: None,
            repath_remaining: 0.0,
            current_path_partial: false,
        }
    }

    /// Returns the current steering waypoints.
    pub fn path(&self) -> &[Vec3] {
        &self.current_path
    }

    /// Returns `true` when no steering path remains.
    pub fn is_idle(&self) -> bool {
        self.current_path.is_empty()
    }

    /// Replaces the destination and forces a path refresh on the next fixed step.
    pub fn set_target(&mut self, target: Option<Vec3>) {
        self.target = target;
        self.repath_remaining = 0.0;
        if target.is_none() {
            self.current_path.clear();
            self.path_index = 0;
            self.current_path_partial = false;
            self.status = NavMeshAgentStatus::Idle;
        }
    }
}

impl Default for NavMeshAgent {
    fn default() -> Self {
        Self::new(3.5)
    }
}

/// Advances every production [`NavMeshAgent`] in the fixed schedule.
pub fn nav_mesh_agent_system(
    nav_mesh: Option<Res<NavMeshQuery>>,
    fixed_time: Res<FixedTime>,
    mut agents: Query<(&mut NavMeshAgent, &mut Transform)>,
) {
    let dt = fixed_time.fixed_delta;
    let Some(nav_mesh) = nav_mesh else {
        for (_, (agent, _)) in agents.iter_mut() {
            agent.status = if agent.target.is_some() {
                NavMeshAgentStatus::MissingNavMesh
            } else {
                NavMeshAgentStatus::Idle
            };
        }
        return;
    };

    for (_, (agent, transform)) in agents.iter_mut() {
        agent.repath_remaining = (agent.repath_remaining - dt).max(0.0);
        let target_changed = agent.target != agent.computed_target;
        if target_changed || (agent.target.is_some() && agent.repath_remaining <= 0.0) {
            agent.computed_target = agent.target;
            agent.repath_remaining = agent.repath_interval.max(0.0);
            agent.current_path.clear();
            agent.path_index = 0;
            agent.current_path_partial = false;
            if let Some(target) = agent.target {
                match nav_mesh.query_path(agent.profile_id.as_str(), transform.translation, target) {
                    NavigationPathResult::Complete(path) => {
                        agent.current_path = path.waypoints;
                        agent.status = NavMeshAgentStatus::Moving;
                    }
                    NavigationPathResult::Partial(path) => {
                        agent.current_path = path.waypoints;
                        agent.current_path_partial = true;
                        agent.status = NavMeshAgentStatus::PartialPath;
                    }
                    NavigationPathResult::Failure(NavigationQueryFailure::MissingProfile(_)) => {
                        agent.status = NavMeshAgentStatus::MissingProfile;
                    }
                    NavigationPathResult::Failure(NavigationQueryFailure::StartOutside) => {
                        agent.status = NavMeshAgentStatus::StartOutside;
                    }
                    NavigationPathResult::Failure(NavigationQueryFailure::EndOutside) => {
                        agent.status = NavMeshAgentStatus::EndOutside;
                    }
                    NavigationPathResult::Failure(NavigationQueryFailure::NoPath) => {
                        agent.status = NavMeshAgentStatus::NoPath;
                    }
                    NavigationPathResult::Failure(NavigationQueryFailure::MissingNavigationData) => {
                        agent.status = NavMeshAgentStatus::MissingNavMesh;
                    }
                }
            } else {
                agent.status = NavMeshAgentStatus::Idle;
            }
        }

        if agent.current_path.is_empty() {
            continue;
        }
        let final_waypoint = *agent
            .current_path
            .last()
            .expect("non-empty navigation path must have a final waypoint");
        if transform.translation.distance(final_waypoint) <= agent.stopping_distance {
            agent.current_path.clear();
            agent.path_index = 0;
            agent.status = if agent.current_path_partial {
                NavMeshAgentStatus::PartialPath
            } else {
                NavMeshAgentStatus::Arrived
            };
            continue;
        }
        while agent.path_index < agent.current_path.len()
            && transform
                .translation
                .distance(agent.current_path[agent.path_index])
                < agent.speed * dt * 1.5
        {
            agent.path_index += 1;
        }
        let Some(waypoint) = agent.current_path.get(agent.path_index).copied() else {
            continue;
        };
        let delta = waypoint - transform.translation;
        let distance = delta.length();
        if distance > 1e-4 {
            transform.translation += delta / distance * (agent.speed * dt).min(distance);
        }
    }

    let snapshots = agents
        .iter_mut()
        .map(|(entity, (agent, transform))| {
            (
                entity,
                transform.translation,
                agent.avoidance_radius.max(0.0),
            )
        })
        .collect::<Vec<_>>();
    let mut corrections = BTreeMap::new();
    for first_index in 0..snapshots.len() {
        for second_index in (first_index + 1)..snapshots.len() {
            let (first, first_position, first_radius) = snapshots[first_index];
            let (second, second_position, second_radius) = snapshots[second_index];
            let delta = Vec3::new(
                first_position.x - second_position.x,
                0.0,
                first_position.z - second_position.z,
            );
            let minimum = first_radius + second_radius;
            let distance = delta.length();
            if distance >= minimum || minimum <= 0.0 {
                continue;
            }
            let direction = if distance > 1e-5 {
                delta / distance
            } else if first <= second {
                Vec3::X
            } else {
                -Vec3::X
            };
            let correction = direction * ((minimum - distance) * 0.5);
            *corrections.entry(first).or_insert(Vec3::ZERO) += correction;
            *corrections.entry(second).or_insert(Vec3::ZERO) -= correction;
        }
    }
    for (entity, (_, transform)) in agents.iter_mut() {
        if let Some(correction) = corrections.get(&entity) {
            transform.translation += *correction;
        }
    }
}

/// Compatibility settings for the existing single-profile Editor bake entry point.
#[derive(Debug, Clone)]
pub struct NavMeshSettings {
    /// Compatibility grid cell size used to tessellate the planar source.
    pub cell_size: f32,
    /// Default production profile radius.
    pub agent_radius: f32,
    /// Compatibility world minimum bound.
    pub world_min: Vec3,
    /// Compatibility world maximum bound.
    pub world_max: Vec3,
    /// Compatibility planar floor height.
    pub walkable_height: f32,
    /// Default production profile clearance height.
    pub agent_height: f32,
}

impl Default for NavMeshSettings {
    fn default() -> Self {
        Self {
            cell_size: 0.5,
            agent_radius: 0.4,
            world_min: Vec3::new(-50.0, 0.0, -50.0),
            world_max: Vec3::new(50.0, 0.0, 50.0),
            walkable_height: 0.0,
            agent_height: 1.8,
        }
    }
}

/// Builds a production tiled polygon asset from compatibility obstacle AABBs.
pub fn bake_from_obstacles(obstacles: &[(Vec3, Vec3)], settings: &NavMeshSettings) -> NavMesh {
    let cell_size = settings.cell_size.max(0.01);
    let columns = ((settings.world_max.x - settings.world_min.x) / cell_size)
        .ceil()
        .max(1.0) as usize;
    let rows = ((settings.world_max.z - settings.world_min.z) / cell_size)
        .ceil()
        .max(1.0) as usize;
    let mut triangles = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let minimum_x = settings.world_min.x + column as f32 * cell_size;
            let minimum_z = settings.world_min.z + row as f32 * cell_size;
            let maximum_x = minimum_x + cell_size;
            let maximum_z = minimum_z + cell_size;
            let center = Vec3::new(
                (minimum_x + maximum_x) * 0.5,
                settings.walkable_height,
                (minimum_z + maximum_z) * 0.5,
            );
            let blocked = obstacles.iter().any(|(obstacle_center, half)| {
                let radius = settings.agent_radius;
                center.x >= obstacle_center.x - half.x - radius
                    && center.x <= obstacle_center.x + half.x + radius
                    && center.z >= obstacle_center.z - half.z - radius
                    && center.z <= obstacle_center.z + half.z + radius
            });
            if blocked {
                continue;
            }
            let y = settings.walkable_height;
            let a = [minimum_x, y, minimum_z];
            let b = [maximum_x, y, minimum_z];
            let c = [maximum_x, y, maximum_z];
            let d = [minimum_x, y, maximum_z];
            triangles.push(NavigationTriangle { a, b, c, area: 0 });
            triangles.push(NavigationTriangle { a, b: c, c: d, area: 0 });
        }
    }
    let input = NavigationBuildInput {
        source_fingerprint: "compat-planar-v2".to_owned(),
        triangles,
        modifiers: Vec::new(),
        links: Vec::new(),
        profiles: vec![NavigationAgentProfile {
            radius: settings.agent_radius,
            height: settings.agent_height,
            max_climb: cell_size,
            ..NavigationAgentProfile::default()
        }],
        tile_size: (cell_size * 16.0).max(1.0),
    };
    ProductionNavigationBaker
        .bake(&input, &|| false)
        .unwrap_or_else(|_| NavMesh {
            schema_version: NAV_MESH_SCHEMA_VERSION,
            source_fingerprint: input.source_fingerprint,
            profiles: vec![NavigationProfileMesh {
                profile: NavigationAgentProfile::default(),
                tiles: Vec::new(),
                links: Vec::new(),
            }],
        })
}

/// Queries static non-trigger colliders and writes a production compatibility bake.
#[cfg(not(target_arch = "wasm32"))]
pub fn bake_navmesh(
    world: &mut engine_ecs::World,
    settings: &NavMeshSettings,
    output_path: &Path,
) -> Result<NavMesh, NavMeshError> {
    use crate::collision::{Collider, PhysicsBody, TriggerVolume};
    use crate::transform::GlobalTransform;

    let obstacles = if let Ok(query) = world.query::<(
        &GlobalTransform,
        &Collider,
        Option<&PhysicsBody>,
        Option<&TriggerVolume>,
    )>() {
        query
            .iter()
            .filter_map(|(_, (transform, collider, body, trigger))| {
                if body != Some(&PhysicsBody::Static) || trigger.is_some() {
                    return None;
                }
                let aabb = collider.world_aabb(transform);
                let minimum_y = aabb.center.y - aabb.half_extents.y;
                let maximum_y = aabb.center.y + aabb.half_extents.y;
                let clearance_top = settings.walkable_height + settings.agent_height;
                if maximum_y <= settings.walkable_height + 0.05 || minimum_y >= clearance_top {
                    return None;
                }
                Some((aabb.center, aabb.half_extents))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let nav_mesh = bake_from_obstacles(&obstacles, settings);
    save_navmesh(&nav_mesh, output_path)?;
    Ok(nav_mesh)
}

/// Reports that filesystem-backed navigation baking is unavailable on wasm32.
#[cfg(target_arch = "wasm32")]
pub fn bake_navmesh(
    _world: &mut engine_ecs::World,
    _settings: &NavMeshSettings,
    _output_path: &Path,
) -> Result<NavMesh, NavMeshError> {
    Err(unsupported_navmesh_io())
}

/// Saves a production navigation asset as canonical pretty JSON.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_navmesh(nav_mesh: &NavMesh, path: &Path) -> Result<(), NavMeshError> {
    nav_mesh.validate()?;
    let json = serde_json::to_string_pretty(nav_mesh).map_err(NavMeshError::Json)?;
    std::fs::write(path, json).map_err(NavMeshError::Io)
}

/// Reports that filesystem-backed navigation persistence is unavailable on wasm32.
#[cfg(target_arch = "wasm32")]
pub fn save_navmesh(_nav_mesh: &NavMesh, _path: &Path) -> Result<(), NavMeshError> {
    Err(unsupported_navmesh_io())
}

/// Loads and validates a production navigation asset.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_navmesh(path: &Path) -> Result<NavMesh, NavMeshError> {
    let bytes = std::fs::read(path).map_err(NavMeshError::Io)?;
    let nav_mesh: NavMesh = serde_json::from_slice(&bytes).map_err(NavMeshError::Json)?;
    nav_mesh.validate()?;
    Ok(nav_mesh)
}

/// Reports that filesystem-backed navigation loading is unavailable on wasm32.
#[cfg(target_arch = "wasm32")]
pub fn load_navmesh(_path: &Path) -> Result<NavMesh, NavMeshError> {
    Err(unsupported_navmesh_io())
}

#[cfg(target_arch = "wasm32")]
fn unsupported_navmesh_io() -> NavMeshError {
    NavMeshError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "navigation asset file IO is not available on wasm32",
    ))
}

/// Error returned by production NavMesh persistence and validation.
#[derive(Debug)]
pub enum NavMeshError {
    /// Filesystem operation failed.
    Io(io::Error),
    /// JSON serialization or parsing failed.
    Json(serde_json::Error),
    /// Loaded runtime asset violated schema invariants.
    InvalidAsset(String),
}

impl fmt::Display for NavMeshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "navigation I/O error: {error}"),
            Self::Json(error) => write!(formatter, "navigation JSON error: {error}"),
            Self::InvalidAsset(message) => write!(formatter, "invalid navigation asset: {message}"),
        }
    }
}

impl std::error::Error for NavMeshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidAsset(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle(a: Vec3, b: Vec3, c: Vec3) -> NavigationTriangle {
        NavigationTriangle {
            a: a.to_array(),
            b: b.to_array(),
            c: c.to_array(),
            area: 0,
        }
    }

    fn input(triangles: Vec<NavigationTriangle>) -> NavigationBuildInput {
        NavigationBuildInput {
            source_fingerprint: "fixture".to_owned(),
            triangles,
            modifiers: Vec::new(),
            links: Vec::new(),
            profiles: vec![NavigationAgentProfile {
                radius: 0.0,
                ..NavigationAgentProfile::default()
            }],
            tile_size: 4.0,
        }
    }

    #[test]
    fn stacked_floors_do_not_cross_link() {
        let mut triangles = Vec::new();
        for height in [0.0, 3.0] {
            triangles.push(triangle(
                Vec3::new(0.0, height, 0.0),
                Vec3::new(2.0, height, 0.0),
                Vec3::new(2.0, height, 2.0),
            ));
            triangles.push(triangle(
                Vec3::new(0.0, height, 0.0),
                Vec3::new(2.0, height, 2.0),
                Vec3::new(0.0, height, 2.0),
            ));
        }
        let asset = ProductionNavigationBaker
            .bake(&input(triangles), &|| false)
            .expect("stacked fixture must bake");
        let query = NavMeshQuery::new(asset);
        assert!(matches!(
            query.query_path(
                DEFAULT_NAVIGATION_PROFILE,
                Vec3::new(0.5, 0.0, 0.5),
                Vec3::new(0.5, 3.0, 0.5)
            ),
            NavigationPathResult::Failure(NavigationQueryFailure::NoPath)
        ));
    }


    #[test]
    fn max_climb_connects_matching_xz_edges_without_linking_stacked_floors() {
        let mut build = input(vec![
            triangle(
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 2.0),
            ),
            triangle(
                Vec3::new(2.0, 0.3, 0.0),
                Vec3::new(4.0, 0.3, 2.0),
                Vec3::new(2.0, 0.3, 2.0),
            ),
        ]);
        build.profiles[0].max_climb = 0.4;
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("step fixture must bake"),
        );
        assert!(matches!(
            query.query_path(
                DEFAULT_NAVIGATION_PROFILE,
                Vec3::new(1.5, 0.0, 0.5),
                Vec3::new(2.5, 0.3, 1.5),
            ),
            NavigationPathResult::Complete(_)
        ));

        build.profiles[0].max_climb = 0.2;
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("blocked step fixture must bake"),
        );
        assert!(matches!(
            query.query_path(
                DEFAULT_NAVIGATION_PROFILE,
                Vec3::new(1.5, 0.0, 0.5),
                Vec3::new(2.5, 0.3, 1.5),
            ),
            NavigationPathResult::Failure(NavigationQueryFailure::NoPath)
        ));
    }

    #[test]
    fn radius_rejects_portals_narrower_than_agent_diameter() {
        let triangles = vec![
            triangle(
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 1.0),
            ),
            triangle(
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, 1.0),
            ),
        ];
        let mut build = input(triangles);
        build.profiles = vec![
            NavigationAgentProfile {
                id: NavigationProfileId::new("small"),
                name: "Small".to_owned(),
                radius: 0.2,
                ..NavigationAgentProfile::default()
            },
            NavigationAgentProfile {
                id: NavigationProfileId::new("large"),
                name: "Large".to_owned(),
                radius: 0.6,
                ..NavigationAgentProfile::default()
            },
        ];
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("portal fixture must bake"),
        );
        assert!(matches!(
            query.query_path("small", Vec3::new(0.7, 0.0, 0.3), Vec3::new(1.3, 0.0, 0.7)),
            NavigationPathResult::Complete(_)
        ));
        assert!(matches!(
            query.query_path("large", Vec3::new(0.7, 0.0, 0.3), Vec3::new(1.3, 0.0, 0.7)),
            NavigationPathResult::Failure(NavigationQueryFailure::NoPath)
        ));
    }

    #[test]
    fn disconnected_destination_returns_best_reachable_partial_path() {
        let build = input(vec![
            triangle(
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 2.0),
            ),
            triangle(
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(4.0, 0.0, 2.0),
                Vec3::new(2.0, 0.0, 2.0),
            ),
            triangle(
                Vec3::new(7.0, 0.0, 0.0),
                Vec3::new(9.0, 0.0, 0.0),
                Vec3::new(9.0, 0.0, 2.0),
            ),
        ]);
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("partial fixture must bake"),
        );
        let result = query.query_path(
            DEFAULT_NAVIGATION_PROFILE,
            Vec3::new(1.0, 0.0, 0.5),
            Vec3::new(8.0, 0.0, 0.5),
        );
        let NavigationPathResult::Partial(path) = result else {
            panic!("disconnected target should produce a partial route");
        };
        assert!(path.corridor.len() >= 2);
        assert!(path.waypoints.last().unwrap().x < 5.0);
    }

    #[test]
    fn slope_limit_changes_reachable_geometry_by_profile() {
        let sloped = vec![triangle(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::new(0.0, 4.0, 2.0),
        )];
        let mut build = input(sloped);
        build.profiles = vec![
            NavigationAgentProfile {
                id: NavigationProfileId::new("walker"),
                name: "Walker".to_owned(),
                radius: 0.0,
                max_slope_degrees: 40.0,
                ..NavigationAgentProfile::default()
            },
            NavigationAgentProfile {
                id: NavigationProfileId::new("climber"),
                name: "Climber".to_owned(),
                radius: 0.0,
                max_slope_degrees: 70.0,
                ..NavigationAgentProfile::default()
            },
        ];
        let asset = ProductionNavigationBaker
            .bake(&build, &|| false)
            .expect("profile fixture must bake");
        assert_eq!(asset.profile("walker").unwrap().tiles.len(), 0);
        assert_eq!(asset.profile("climber").unwrap().tiles.len(), 1);
    }

    #[test]
    fn one_way_link_connects_stacked_surfaces_only_forward() {
        let mut build = input(vec![
            triangle(
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 2.0),
            ),
            triangle(
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(2.0, 3.0, 0.0),
                Vec3::new(0.0, 3.0, 2.0),
            ),
        ]);
        build.links.push(NavigationBuildLink {
            id: "jump_up".to_owned(),
            start: [0.5, 0.0, 0.5],
            end: [0.5, 3.0, 0.5],
            bidirectional: false,
            profiles: Vec::new(),
            area: 0,
            cost: 1.0,
            traversal_tag: "jump".to_owned(),
        });
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("link fixture must bake"),
        );
        let forward = query.query_path(
            DEFAULT_NAVIGATION_PROFILE,
            Vec3::new(0.5, 0.0, 0.5),
            Vec3::new(0.5, 3.0, 0.5),
        );
        assert!(matches!(forward, NavigationPathResult::Complete(_)));
        let mut closed_door = NavigationQueryFilter::default();
        closed_door.disable_traversal_tag("jump");
        let filtered = query.query_path_with_filter(
            DEFAULT_NAVIGATION_PROFILE,
            Vec3::new(0.5, 0.0, 0.5),
            Vec3::new(0.5, 3.0, 0.5),
            &closed_door,
        );
        assert!(matches!(
            filtered,
            NavigationPathResult::Failure(NavigationQueryFailure::NoPath)
        ));
        let reverse = query.query_path(
            DEFAULT_NAVIGATION_PROFILE,
            Vec3::new(0.5, 3.0, 0.5),
            Vec3::new(0.5, 0.0, 0.5),
        );
        assert!(matches!(
            reverse,
            NavigationPathResult::Failure(NavigationQueryFailure::NoPath)
        ));
    }

    #[test]
    fn area_cost_prefers_lower_cost_corridor() {
        let mut build = input(vec![
            triangle(Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 2.0)),
            triangle(Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 2.0), Vec3::new(0.0, 0.0, 2.0)),
            triangle(Vec3::new(2.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 2.0)),
            triangle(Vec3::new(4.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 2.0), Vec3::new(2.0, 0.0, 2.0)),
        ]);
        build.modifiers.push(NavigationModifier {
            id: "expensive".to_owned(),
            minimum: [1.9, -1.0, -0.1],
            maximum: [4.1, 1.0, 1.0],
            profiles: Vec::new(),
            mode: NavigationModifierMode::Area {
                area: 7,
                cost_multiplier: 20.0,
            },
        });
        let asset = ProductionNavigationBaker
            .bake(&build, &|| false)
            .expect("cost fixture must bake");
        assert!(asset
            .profile(DEFAULT_NAVIGATION_PROFILE)
            .unwrap()
            .tiles
            .iter()
            .flat_map(|tile| &tile.polygons)
            .any(|polygon| polygon.area == 7 && polygon.cost_multiplier > 1.0));
    }

    #[test]
    fn nearest_point_reports_far_start_as_outside() {
        let build = input(vec![triangle(
            Vec3::ZERO,
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 2.0),
        )]);
        let query = NavMeshQuery::new(
            ProductionNavigationBaker
                .bake(&build, &|| false)
                .expect("nearest fixture must bake"),
        );
        assert_eq!(
            query.nearest_point(DEFAULT_NAVIGATION_PROFILE, Vec3::new(10.0, 0.0, 10.0), 1.0),
            Err(NavigationQueryFailure::StartOutside)
        );
    }

    #[test]
    fn cancelled_bake_produces_no_asset() {
        let build = input(vec![triangle(
            Vec3::ZERO,
            Vec3::X * 2.0,
            Vec3::Z * 2.0,
        )]);
        assert_eq!(
            ProductionNavigationBaker.bake(&build, &|| true),
            Err(NavigationBakeError::Cancelled)
        );
    }

    #[test]
    fn compatibility_bake_writes_production_schema() {
        let settings = NavMeshSettings {
            cell_size: 1.0,
            agent_radius: 0.0,
            world_min: Vec3::ZERO,
            world_max: Vec3::new(2.0, 0.0, 2.0),
            ..NavMeshSettings::default()
        };
        let asset = bake_from_obstacles(&[], &settings);
        assert_eq!(asset.schema_version, NAV_MESH_SCHEMA_VERSION);
        assert!(asset.profile(DEFAULT_NAVIGATION_PROFILE).is_some());
    }
}
