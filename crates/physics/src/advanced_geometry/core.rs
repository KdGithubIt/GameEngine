//! Multi-layer grid navigation and static triangle-mesh query helpers.
//!
//! These types extend the current primitive-collider and single-plane NavMesh
//! line without changing the fixed-step collision pipeline. They are suitable
//! for authored stairs, floor links, selection raycasts, and future baked mesh
//! collider integration.

use std::collections::BTreeSet;
use std::fmt;

use glam::Vec3;

use crate::navmesh::{NavMesh, NavMeshQuery};

/// One named navigation floor with an accepted world-height interval.
#[derive(Debug, Clone)]
pub struct NavMeshLayer {
    /// Stable project-local layer ID.
    pub id: String,
    /// Inclusive minimum world height assigned to this floor.
    pub minimum_height: f32,
    /// Inclusive maximum world height assigned to this floor.
    pub maximum_height: f32,
    /// Existing grid-backed navigation data for this floor.
    pub nav_mesh: NavMesh,
}

/// Explicit connection between two navigation floors.
#[derive(Debug, Clone, PartialEq)]
pub struct NavMeshLayerLink {
    /// Source floor ID.
    pub from_layer: String,
    /// Destination floor ID.
    pub to_layer: String,
    /// Walkable source position such as the bottom of stairs.
    pub from: Vec3,
    /// Walkable destination position such as the top of stairs.
    pub to: Vec3,
    /// Additional authored traversal cost.
    pub cost: f32,
    /// Whether the reverse direction is also available.
    pub bidirectional: bool,
}

/// Validation failure for [`LayeredNavMesh`].
#[derive(Debug, Clone, PartialEq)]
pub enum LayeredNavMeshError {
    /// At least one floor must exist.
    Empty,
    /// A floor ID is blank.
    BlankLayerId,
    /// The same floor ID appears more than once.
    DuplicateLayerId(String),
    /// A floor height range is invalid.
    InvalidHeightRange {
        /// Floor ID.
        layer: String,
        /// Rejected minimum height.
        minimum: f32,
        /// Rejected maximum height.
        maximum: f32,
    },
    /// A link references an unknown floor.
    MissingLinkLayer(String),
    /// A link position or cost is non-finite or negative.
    InvalidLink {
        /// Source floor ID.
        from_layer: String,
        /// Destination floor ID.
        to_layer: String,
    },
}

impl fmt::Display for LayeredNavMeshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "layered NavMesh requires at least one layer"),
            Self::BlankLayerId => write!(formatter, "NavMesh layer ID must not be blank"),
            Self::DuplicateLayerId(id) => write!(formatter, "NavMesh layer `{id}` is duplicated"),
            Self::InvalidHeightRange {
                layer,
                minimum,
                maximum,
            } => write!(
                formatter,
                "NavMesh layer `{layer}` has invalid height range {minimum}..={maximum}"
            ),
            Self::MissingLinkLayer(id) => {
                write!(formatter, "NavMesh link references missing layer `{id}`")
            }
            Self::InvalidLink {
                from_layer,
                to_layer,
            } => write!(
                formatter,
                "NavMesh link `{from_layer}` -> `{to_layer}` has invalid position or cost"
            ),
        }
    }
}

impl std::error::Error for LayeredNavMeshError {}

/// Validated collection of grid floors connected by explicit authored links.
#[derive(Debug, Clone)]
pub struct LayeredNavMesh {
    layers: Vec<NavMeshLayer>,
    links: Vec<NavMeshLayerLink>,
}

impl LayeredNavMesh {
    /// Validates floors and links.
    ///
    /// # Errors
    ///
    /// Returns [`LayeredNavMeshError`] for invalid IDs, height ranges, missing
    /// link endpoints, or non-finite link data.
    pub fn new(
        mut layers: Vec<NavMeshLayer>,
        links: Vec<NavMeshLayerLink>,
    ) -> Result<Self, LayeredNavMeshError> {
        if layers.is_empty() {
            return Err(LayeredNavMeshError::Empty);
        }
        let mut ids = BTreeSet::new();
        for layer in &layers {
            if layer.id.trim().is_empty() {
                return Err(LayeredNavMeshError::BlankLayerId);
            }
            if !ids.insert(layer.id.clone()) {
                return Err(LayeredNavMeshError::DuplicateLayerId(layer.id.clone()));
            }
            if !layer.minimum_height.is_finite()
                || !layer.maximum_height.is_finite()
                || layer.minimum_height > layer.maximum_height
            {
                return Err(LayeredNavMeshError::InvalidHeightRange {
                    layer: layer.id.clone(),
                    minimum: layer.minimum_height,
                    maximum: layer.maximum_height,
                });
            }
        }
        for link in &links {
            if !ids.contains(&link.from_layer) {
                return Err(LayeredNavMeshError::MissingLinkLayer(
                    link.from_layer.clone(),
                ));
            }
            if !ids.contains(&link.to_layer) {
                return Err(LayeredNavMeshError::MissingLinkLayer(link.to_layer.clone()));
            }
            if !link.from.is_finite()
                || !link.to.is_finite()
                || !link.cost.is_finite()
                || link.cost < 0.0
            {
                return Err(LayeredNavMeshError::InvalidLink {
                    from_layer: link.from_layer.clone(),
                    to_layer: link.to_layer.clone(),
                });
            }
        }
        layers.sort_by(|left, right| {
            left.minimum_height
                .total_cmp(&right.minimum_height)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(Self { layers, links })
    }

    /// Returns validated floors ordered by height and ID.
    pub fn layers(&self) -> &[NavMeshLayer] {
        &self.layers
    }

    /// Returns authored inter-floor links.
    pub fn links(&self) -> &[NavMeshLayerLink] {
        &self.links
    }

    /// Selects the floor containing `position.y`.
    ///
    /// Overlapping height intervals choose the floor whose interval midpoint is
    /// nearest to the position, then the lexicographically smaller ID.
    pub fn layer_at(&self, position: Vec3) -> Option<&NavMeshLayer> {
        self.layers
            .iter()
            .filter(|layer| (layer.minimum_height..=layer.maximum_height).contains(&position.y))
            .min_by(|left, right| {
                let left_mid = (left.minimum_height + left.maximum_height) * 0.5;
                let right_mid = (right.minimum_height + right.maximum_height) * 0.5;
                (position.y - left_mid)
                    .abs()
                    .total_cmp(&(position.y - right_mid).abs())
                    .then_with(|| left.id.cmp(&right.id))
            })
    }

    /// Finds a route on one floor or through one explicit inter-floor link.
    ///
    /// Direct links are the minimum useful extension for stairs, lifts, and
    /// drop-downs. Multi-link graph search remains a separate follow-up once a
    /// production scene demonstrates that need.
    pub fn find_path(&self, start: Vec3, end: Vec3) -> Option<Vec<Vec3>> {
        let start_layer = self.layer_at(start)?;
        let end_layer = self.layer_at(end)?;
        if start_layer.id == end_layer.id {
            return NavMeshQuery::new(start_layer.nav_mesh.clone()).find_path(start, end);
        }

        let mut best: Option<(f32, Vec<Vec3>)> = None;
        for authored in &self.links {
            for (from_layer, to_layer, from, to) in link_directions(authored) {
                if from_layer != start_layer.id || to_layer != end_layer.id {
                    continue;
                }
                let Some(mut first) =
                    NavMeshQuery::new(start_layer.nav_mesh.clone()).find_path(start, from)
                else {
                    continue;
                };
                let Some(mut second) =
                    NavMeshQuery::new(end_layer.nav_mesh.clone()).find_path(to, end)
                else {
                    continue;
                };
                push_distinct(&mut first, from);
                push_distinct(&mut first, to);
                if !second.is_empty() {
                    if first.last() == second.first() {
                        second.remove(0);
                    }
                    first.extend(second);
                }
                let score = path_length(&first) + authored.cost;
                if best
                    .as_ref()
                    .is_none_or(|(best_score, _)| score < *best_score)
                {
                    best = Some((score, first));
                }
            }
        }
        best.map(|(_, path)| path)
    }
}

fn link_directions(link: &NavMeshLayerLink) -> impl Iterator<Item = (&str, &str, Vec3, Vec3)> {
    let forward = Some((
        link.from_layer.as_str(),
        link.to_layer.as_str(),
        link.from,
        link.to,
    ));
    let reverse = link.bidirectional.then_some((
        link.to_layer.as_str(),
        link.from_layer.as_str(),
        link.to,
        link.from,
    ));
    forward.into_iter().chain(reverse)
}

fn push_distinct(path: &mut Vec<Vec3>, point: Vec3) {
    if path.last().is_none_or(|last| *last != point) {
        path.push(point);
    }
}

fn path_length(path: &[Vec3]) -> f32 {
    path.windows(2)
        .map(|segment| segment[0].distance(segment[1]))
        .sum()
}

/// Validated immutable triangle geometry for static-scene queries.
#[derive(Debug, Clone)]
pub struct StaticTriangleMesh {
    vertices: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    minimum: Vec3,
    maximum: Vec3,
}

/// Validation failure for [`StaticTriangleMesh`].
#[derive(Debug, Clone, PartialEq)]
pub enum StaticTriangleMeshError {
    /// At least one triangle is required.
    Empty,
    /// A vertex contains NaN or infinity.
    NonFiniteVertex(usize),
    /// A triangle references a missing vertex.
    InvalidIndex {
        /// Triangle index.
        triangle: usize,
        /// Rejected vertex index.
        vertex: u32,
    },
    /// A triangle has zero or near-zero area.
    DegenerateTriangle(usize),
}

impl fmt::Display for StaticTriangleMeshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(
                formatter,
                "static triangle mesh requires at least one triangle"
            ),
            Self::NonFiniteVertex(index) => {
                write!(
                    formatter,
                    "static triangle mesh vertex {index} is non-finite"
                )
            }
            Self::InvalidIndex { triangle, vertex } => write!(
                formatter,
                "static triangle {triangle} references missing vertex {vertex}"
            ),
            Self::DegenerateTriangle(index) => {
                write!(formatter, "static triangle {index} is degenerate")
            }
        }
    }
}

impl std::error::Error for StaticTriangleMeshError {}

/// Closest ray intersection with a [`StaticTriangleMesh`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriangleMeshRayHit {
    /// Triangle index in the authored index list.
    pub triangle: usize,
    /// Distance along the normalized ray direction.
    pub distance: f32,
    /// World-space hit position.
    pub position: Vec3,
    /// Unit face normal.
    pub normal: Vec3,
}

impl StaticTriangleMesh {
    /// Validates vertices and indexed triangles and computes mesh bounds.
    ///
    /// # Errors
    ///
    /// Returns [`StaticTriangleMeshError`] for missing, invalid, or degenerate
    /// geometry.
    pub fn new(
        vertices: Vec<Vec3>,
        triangles: Vec<[u32; 3]>,
    ) -> Result<Self, StaticTriangleMeshError> {
        if triangles.is_empty() {
            return Err(StaticTriangleMeshError::Empty);
        }
        for (index, vertex) in vertices.iter().enumerate() {
            if !vertex.is_finite() {
                return Err(StaticTriangleMeshError::NonFiniteVertex(index));
            }
        }
        for (triangle_index, triangle) in triangles.iter().enumerate() {
            let mut points = [Vec3::ZERO; 3];
            for (point_index, vertex_index) in triangle.iter().copied().enumerate() {
                points[point_index] = *vertices.get(vertex_index as usize).ok_or(
                    StaticTriangleMeshError::InvalidIndex {
                        triangle: triangle_index,
                        vertex: vertex_index,
                    },
                )?;
            }
            if (points[1] - points[0])
                .cross(points[2] - points[0])
                .length_squared()
                <= f32::EPSILON
            {
                return Err(StaticTriangleMeshError::DegenerateTriangle(triangle_index));
            }
        }
        let minimum = vertices
            .iter()
            .copied()
            .reduce(Vec3::min)
            .unwrap_or(Vec3::ZERO);
        let maximum = vertices
            .iter()
            .copied()
            .reduce(Vec3::max)
            .unwrap_or(Vec3::ZERO);
        Ok(Self {
            vertices,
            triangles,
            minimum,
            maximum,
        })
    }

    /// Returns the minimum world-space mesh bound.
    pub fn minimum(&self) -> Vec3 {
        self.minimum
    }

    /// Returns the maximum world-space mesh bound.
    pub fn maximum(&self) -> Vec3 {
        self.maximum
    }

    /// Finds the closest triangle intersected by a finite ray segment.
    pub fn raycast(
        &self,
        origin: Vec3,
        direction: Vec3,
        maximum_distance: f32,
    ) -> Option<TriangleMeshRayHit> {
        if !origin.is_finite()
            || !direction.is_finite()
            || !maximum_distance.is_finite()
            || maximum_distance < 0.0
        {
            return None;
        }
        let direction = direction.try_normalize()?;
        let mut best = None;
        for (triangle_index, indices) in self.triangles.iter().enumerate() {
            let a = self.vertices[indices[0] as usize];
            let b = self.vertices[indices[1] as usize];
            let c = self.vertices[indices[2] as usize];
            let edge1 = b - a;
            let edge2 = c - a;
            let p = direction.cross(edge2);
            let determinant = edge1.dot(p);
            if determinant.abs() <= f32::EPSILON {
                continue;
            }
            let inverse = determinant.recip();
            let t = origin - a;
            let u = t.dot(p) * inverse;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = t.cross(edge1);
            let v = direction.dot(q) * inverse;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let distance = edge2.dot(q) * inverse;
            if distance < 0.0 || distance > maximum_distance {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|hit: &TriangleMeshRayHit| distance < hit.distance)
            {
                best = Some(TriangleMeshRayHit {
                    triangle: triangle_index,
                    distance,
                    position: origin + direction * distance,
                    normal: edge1.cross(edge2).normalize(),
                });
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navmesh::{bake_from_obstacles, NavMeshSettings};

    fn floor(height: f32) -> NavMesh {
        let settings = NavMeshSettings {
            world_min: Vec3::new(-2.0, height, -2.0),
            world_max: Vec3::new(2.0, height, 2.0),
            walkable_height: height,
            ..NavMeshSettings::default()
        };
        bake_from_obstacles(&[], &settings)
    }

    #[test]
    fn direct_layer_link_builds_cross_floor_path() {
        let nav = LayeredNavMesh::new(
            vec![
                NavMeshLayer {
                    id: "ground".to_owned(),
                    minimum_height: -0.5,
                    maximum_height: 0.5,
                    nav_mesh: floor(0.0),
                },
                NavMeshLayer {
                    id: "upper".to_owned(),
                    minimum_height: 2.5,
                    maximum_height: 3.5,
                    nav_mesh: floor(3.0),
                },
            ],
            vec![NavMeshLayerLink {
                from_layer: "ground".to_owned(),
                to_layer: "upper".to_owned(),
                from: Vec3::new(0.0, 0.0, 0.0),
                to: Vec3::new(0.0, 3.0, 0.0),
                cost: 1.0,
                bidirectional: true,
            }],
        )
        .expect("layered NavMesh must validate");

        let path = nav
            .find_path(Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 3.0, 0.0))
            .expect("direct floor link must produce a route");

        assert!(path.iter().any(|point| point.y == 3.0));
    }

    #[test]
    fn invalid_link_does_not_hide_later_valid_link() {
        let nav = LayeredNavMesh::new(
            vec![
                NavMeshLayer {
                    id: "ground".to_owned(),
                    minimum_height: -0.5,
                    maximum_height: 0.5,
                    nav_mesh: floor(0.0),
                },
                NavMeshLayer {
                    id: "upper".to_owned(),
                    minimum_height: 2.5,
                    maximum_height: 3.5,
                    nav_mesh: floor(3.0),
                },
            ],
            vec![
                NavMeshLayerLink {
                    from_layer: "ground".to_owned(),
                    to_layer: "upper".to_owned(),
                    from: Vec3::new(20.0, 0.0, 20.0),
                    to: Vec3::new(20.0, 3.0, 20.0),
                    cost: 0.0,
                    bidirectional: false,
                },
                NavMeshLayerLink {
                    from_layer: "ground".to_owned(),
                    to_layer: "upper".to_owned(),
                    from: Vec3::ZERO,
                    to: Vec3::new(0.0, 3.0, 0.0),
                    cost: 1.0,
                    bidirectional: false,
                },
            ],
        )
        .expect("layered NavMesh must validate");

        assert!(nav
            .find_path(Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 3.0, 0.0))
            .is_some());
    }

    #[test]
    fn triangle_mesh_raycast_returns_closest_hit() {
        let mesh = StaticTriangleMesh::new(
            vec![
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
            ],
            vec![[0, 1, 2]],
        )
        .expect("triangle mesh must validate");

        let hit = mesh
            .raycast(Vec3::Y, -Vec3::Y, 2.0)
            .expect("downward ray must hit triangle");

        assert!((hit.distance - 1.0).abs() < f32::EPSILON);
        assert_eq!(hit.triangle, 0);
    }

    #[test]
    fn degenerate_triangle_is_rejected() {
        let error =
            StaticTriangleMesh::new(vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0], vec![[0, 1, 2]])
                .expect_err("collinear triangle must be rejected");

        assert_eq!(error, StaticTriangleMeshError::DegenerateTriangle(0));
    }
}
