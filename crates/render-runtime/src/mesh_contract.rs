//! CPU-side mesh contracts for builds that do not need a GPU backend.
//!
//! The default `gpu` feature uses `mesh.rs`, which adds GPU upload and draw
//! contracts. This module intentionally mirrors the CPU-facing API used by
//! importers so `engine-import` can compile without pulling `wgpu` into its
//! dependency graph.

use bytemuck::{Pod, Zeroable};
use hashbrown::HashMap;
use std::fmt;
use std::fmt::Write as _;

/// A vertex consumed by the built-in mesh pipeline.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    /// The object-space position.
    pub position: [f32; 3],
    /// The object-space normal.
    pub normal: [f32; 3],
    /// The vertex RGB color.
    pub color: [f32; 3],
    /// The texture coordinate.
    pub uv: [f32; 2],
    /// Per-vertex multiplier applied to material outline width.
    pub outline_scale: f32,
    /// First generic additional UV channel, used by toon SubTexture maps.
    pub additional_uv: [f32; 2],
}

/// Per-vertex skinning attributes uploaded to vertex slot 2 (ADR 0043).
///
/// Kept out of [`Vertex`] so static meshes carry no skinning cost. The number
/// of entries must equal the mesh vertex count; [`Mesh::validate`] rejects
/// mismatches. Weights are expected to sum to 1.0 per vertex; importers
/// normalize them before producing this contract.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct SkinningVertexData {
    /// Indices into the skin's joint array.
    pub joints: [u16; 4],
    /// Normalized blend weights matching `joints`.
    pub weights: [f32; 4],
}

/// Reports invalid mesh data that cannot be uploaded safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshValidationError {
    /// A mesh has no vertices.
    EmptyVertices,
    /// An indexed mesh has an empty index list.
    EmptyIndices,
    /// The vertex count cannot be represented by the GPU draw API.
    TooManyVertices {
        /// The requested number of vertices.
        count: usize,
    },
    /// The index count cannot be represented by the GPU draw API.
    TooManyIndices {
        /// The requested number of indices.
        count: usize,
    },
    /// The vertex buffer exceeds the selected device limit.
    VertexBufferTooLarge {
        /// The requested buffer size in bytes.
        size: u64,
        /// The maximum supported buffer size in bytes.
        maximum: u64,
    },
    /// The index buffer exceeds the selected device limit.
    IndexBufferTooLarge {
        /// The requested buffer size in bytes.
        size: u64,
        /// The maximum supported buffer size in bytes.
        maximum: u64,
    },
    /// An index refers to a vertex outside the mesh.
    IndexOutOfBounds {
        /// The position of the invalid index in the index list.
        position: usize,
        /// The invalid vertex index.
        index: u32,
        /// The number of available vertices.
        vertex_count: usize,
    },
    /// The skinning attribute count does not match the vertex count.
    SkinningLengthMismatch {
        /// The number of skinning attribute entries.
        skinning_count: usize,
        /// The number of vertices in the mesh.
        vertex_count: usize,
    },
    /// The imported tangent count does not match the vertex count.
    TangentLengthMismatch {
        /// The number of tangent entries.
        tangent_count: usize,
        /// The number of vertices in the mesh.
        vertex_count: usize,
    },
}

impl fmt::Display for MeshValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyVertices => formatter.write_str("mesh must contain at least one vertex"),
            Self::EmptyIndices => {
                formatter.write_str("indexed mesh must contain at least one index")
            }
            Self::TooManyVertices { count } => {
                write!(formatter, "mesh vertex count {count} exceeds u32::MAX")
            }
            Self::TooManyIndices { count } => {
                write!(formatter, "mesh index count {count} exceeds u32::MAX")
            }
            Self::VertexBufferTooLarge { size, maximum } => write!(
                formatter,
                "mesh vertex buffer size {size} exceeds device limit {maximum}"
            ),
            Self::IndexBufferTooLarge { size, maximum } => write!(
                formatter,
                "mesh index buffer size {size} exceeds device limit {maximum}"
            ),
            Self::IndexOutOfBounds {
                position,
                index,
                vertex_count,
            } => write!(
                formatter,
                "mesh index {index} at position {position} is outside {vertex_count} vertices"
            ),
            Self::SkinningLengthMismatch {
                skinning_count,
                vertex_count,
            } => write!(
                formatter,
                "mesh has {skinning_count} skinning entries for {vertex_count} vertices"
            ),
            Self::TangentLengthMismatch {
                tangent_count,
                vertex_count,
            } => write!(
                formatter,
                "mesh has {tangent_count} tangent entries for {vertex_count} vertices"
            ),
        }
    }
}

impl std::error::Error for MeshValidationError {}

/// Stores CPU-side vertex and optional index data.
#[derive(Clone)]
pub struct Mesh {
    /// Vertices in draw order.
    pub vertices: Vec<Vertex>,
    /// Optional triangle-list indices.
    pub indices: Option<Vec<u32>>,
    /// Optional per-vertex skinning attributes (ADR 0043).
    pub skinning: Option<Vec<SkinningVertexData>>,
    /// Optional glTF tangent vectors (`xyz`) and handedness (`w`).
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Ranges drawn with independent material slots (ADR 0076).
    pub submeshes: Vec<Submesh>,
}

/// One contiguous run of a mesh drawn with a single material slot (ADR 0076).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submesh {
    /// First index into [`Mesh::indices`], or first vertex when unindexed.
    pub start: u32,
    /// Number of indices, or vertices when unindexed.
    pub count: u32,
}

impl Mesh {
    /// Returns the ranges to draw, treating an empty list as one full range.
    pub fn draw_ranges(&self) -> Vec<Submesh> {
        if !self.submeshes.is_empty() {
            return self.submeshes.clone();
        }
        let count = match &self.indices {
            Some(indices) => indices.len(),
            None => self.vertices.len(),
        };
        vec![Submesh {
            start: 0,
            count: u32::try_from(count).unwrap_or(u32::MAX),
        }]
    }

    /// Validates CPU mesh invariants shared by import and GPU upload paths.
    ///
    /// # Errors
    ///
    /// Returns an error for empty data, counts that exceed the draw API, or
    /// indices and per-vertex streams that do not match the vertex list.
    pub fn validate(&self) -> Result<(), MeshValidationError> {
        if self.vertices.is_empty() {
            return Err(MeshValidationError::EmptyVertices);
        }

        let vertex_count = u32::try_from(self.vertices.len()).map_err(|_| {
            MeshValidationError::TooManyVertices {
                count: self.vertices.len(),
            }
        })?;

        if let Some(indices) = &self.indices {
            if indices.is_empty() {
                return Err(MeshValidationError::EmptyIndices);
            }
            u32::try_from(indices.len()).map_err(|_| MeshValidationError::TooManyIndices {
                count: indices.len(),
            })?;

            if let Some((position, &index)) = indices
                .iter()
                .enumerate()
                .find(|(_, index)| **index >= vertex_count)
            {
                return Err(MeshValidationError::IndexOutOfBounds {
                    position,
                    index,
                    vertex_count: self.vertices.len(),
                });
            }
        }

        if let Some(skinning) = &self.skinning
            && skinning.len() != self.vertices.len()
        {
            return Err(MeshValidationError::SkinningLengthMismatch {
                skinning_count: skinning.len(),
                vertex_count: self.vertices.len(),
            });
        }

        if let Some(tangents) = &self.tangents
            && tangents.len() != self.vertices.len()
        {
            return Err(MeshValidationError::TangentLengthMismatch {
                tangent_count: tangents.len(),
                vertex_count: self.vertices.len(),
            });
        }

        Ok(())
    }

    /// Creates a centered triangle with per-vertex colors.
    pub fn triangle() -> Self {
        let normal = [0.0, 0.0, 1.0];
        Self {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.5, 0.0],
                    normal,
                    color: [1.0, 0.0, 0.0],
                    uv: [0.5, 0.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [-0.5, -0.5, 0.0],
                    normal,
                    color: [0.0, 1.0, 0.0],
                    uv: [0.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [0.5, -0.5, 0.0],
                    normal,
                    color: [0.0, 0.0, 1.0],
                    uv: [1.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
            ],
            indices: None,
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    /// Creates a centered textured quad made from two triangles.
    pub fn quad() -> Self {
        let normal = [0.0, 0.0, 1.0];
        Self {
            vertices: vec![
                Vertex {
                    position: [-0.5, 0.5, 0.0],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [0.0, 0.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [-0.5, -0.5, 0.0],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [0.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [0.5, -0.5, 0.0],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [1.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [0.5, 0.5, 0.0],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [1.0, 0.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
            ],
            indices: Some(vec![0, 1, 2, 0, 2, 3]),
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    /// Creates a centered 1x1x1 cube with separate vertices per face.
    pub fn cube() -> Self {
        let mut vertices = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);
        let faces = [
            ([1.0, 0.0, 0.0], [[0.5, 0.5, 0.5], [0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5]]),
            ([-1.0, 0.0, 0.0], [[-0.5, 0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5, -0.5, 0.5], [-0.5, 0.5, 0.5]]),
            ([0.0, 1.0, 0.0], [[-0.5, 0.5, -0.5], [-0.5, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.5, -0.5]]),
            ([0.0, -1.0, 0.0], [[-0.5, -0.5, 0.5], [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, -0.5, 0.5]]),
            ([0.0, 0.0, 1.0], [[-0.5, 0.5, 0.5], [-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [0.5, 0.5, 0.5]]),
            ([0.0, 0.0, -1.0], [[0.5, 0.5, -0.5], [0.5, -0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5, 0.5, -0.5]]),
        ];

        for (normal, positions) in faces {
            let base = vertices.len() as u32;
            let uvs = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
            for (position, uv) in positions.into_iter().zip(uvs) {
                vertices.push(Vertex {
                    position,
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv,
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                });
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        Self {
            vertices,
            indices: Some(indices),
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    /// Creates a centered XZ plane at y=0 with an upward normal.
    pub fn plane(width: f32, depth: f32) -> Self {
        let half_width = width * 0.5;
        let half_depth = depth * 0.5;
        let normal = [0.0, 1.0, 0.0];
        Self {
            vertices: vec![
                Vertex { position: [-half_width, 0.0, -half_depth], normal, color: [1.0; 3], uv: [0.0, 0.0], outline_scale: 1.0, additional_uv: [0.0; 2] },
                Vertex { position: [-half_width, 0.0, half_depth], normal, color: [1.0; 3], uv: [0.0, 1.0], outline_scale: 1.0, additional_uv: [0.0; 2] },
                Vertex { position: [half_width, 0.0, half_depth], normal, color: [1.0; 3], uv: [1.0, 1.0], outline_scale: 1.0, additional_uv: [0.0; 2] },
                Vertex { position: [half_width, 0.0, -half_depth], normal, color: [1.0; 3], uv: [1.0, 0.0], outline_scale: 1.0, additional_uv: [0.0; 2] },
            ],
            indices: Some(vec![0, 1, 2, 0, 2, 3]),
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    /// Creates a UV sphere centered at the origin.
    pub fn sphere(rings: u32, sectors: u32) -> Self {
        let rings = rings.max(2);
        let sectors = sectors.max(3);
        let mut vertices = Vec::with_capacity(((rings + 1) * (sectors + 1)) as usize);
        let mut indices = Vec::with_capacity((rings * sectors * 6) as usize);

        for ring in 0..=rings {
            let theta = std::f32::consts::PI * ring as f32 / rings as f32;
            let sin_theta = theta.sin();
            let cos_theta = theta.cos();
            for sector in 0..=sectors {
                let phi = std::f32::consts::TAU * sector as f32 / sectors as f32;
                let normal = [sin_theta * phi.cos(), cos_theta, sin_theta * phi.sin()];
                vertices.push(Vertex {
                    position: [normal[0] * 0.5, normal[1] * 0.5, normal[2] * 0.5],
                    normal,
                    color: [1.0; 3],
                    uv: [sector as f32 / sectors as f32, ring as f32 / rings as f32],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                });
            }
        }

        let stride = sectors + 1;
        for ring in 0..rings {
            for sector in 0..sectors {
                let top_left = ring * stride + sector;
                let bottom_left = (ring + 1) * stride + sector;
                let top_right = top_left + 1;
                let bottom_right = bottom_left + 1;
                indices.extend_from_slice(&[
                    top_left,
                    bottom_right,
                    bottom_left,
                    top_left,
                    top_right,
                    bottom_right,
                ]);
            }
        }

        Self {
            vertices,
            indices: Some(indices),
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }
}

/// Extracts the vertices `range` draws into a fully self-contained mesh with
/// skinning and tangent data dropped (ADR 0089 §3).
pub fn extract_baked_submesh(mesh: &Mesh, range: Submesh) -> Mesh {
    let start = range.start as usize;
    let end = start.saturating_add(range.count as usize);
    let original_indices: Vec<u32> = match &mesh.indices {
        Some(indices) => indices[start.min(indices.len())..end.min(indices.len())].to_vec(),
        None => {
            let end = end.min(mesh.vertices.len());
            (start.min(mesh.vertices.len()) as u32..end as u32).collect()
        }
    };

    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut vertices = Vec::new();
    let mut indices = Vec::with_capacity(original_indices.len());
    for original in original_indices {
        let Some(&vertex) = mesh.vertices.get(original as usize) else {
            continue;
        };
        let new_index = *remap.entry(original).or_insert_with(|| {
            vertices.push(vertex);
            (vertices.len() - 1) as u32
        });
        indices.push(new_index);
    }

    Mesh {
        vertices,
        indices: Some(indices),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    }
}

/// Serializes `mesh` as Wavefront OBJ text: `v`/`vt`/`vn`/`f` lines only.
pub fn mesh_to_obj(mesh: &Mesh) -> String {
    let mut text = String::new();
    for vertex in &mesh.vertices {
        let [x, y, z] = vertex.position;
        let _ = writeln!(text, "v {x} {y} {z}");
    }
    for vertex in &mesh.vertices {
        let [u, v] = vertex.uv;
        let _ = writeln!(text, "vt {u} {}", 1.0 - v);
    }
    for vertex in &mesh.vertices {
        let [x, y, z] = vertex.normal;
        let _ = writeln!(text, "vn {x} {y} {z}");
    }
    if let Some(indices) = &mesh.indices {
        for triangle in indices.chunks_exact(3) {
            let corners: Vec<String> = triangle
                .iter()
                .map(|&index| {
                    let one_based = index + 1;
                    format!("{one_based}/{one_based}/{one_based}")
                })
                .collect();
            let _ = writeln!(text, "f {}", corners.join(" "));
        }
    }
    text
}
