use bytemuck::{Pod, Zeroable};
use hashbrown::HashMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use crate::asset::RuntimeAssetId;

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

impl Vertex {
    /// The GPU vertex buffer layout for [`Vertex`].
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x3,
            3 => Float32x2,
            13 => Float32x3,
        ],
    };
}

/// Per-vertex skinning attributes uploaded to vertex slot 2 (ADR 0043).
///
/// Kept out of [`Vertex`] so static meshes carry no skinning cost. The
/// number of entries must equal the mesh vertex count; [`Mesh::validate`]
/// rejects mismatches. Weights are expected to sum to 1.0 per vertex; the
/// glTF importer renormalizes on import.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct SkinningVertexData {
    /// Indices into the skin's joint array.
    pub joints: [u16; 4],
    /// Normalized blend weights matching `joints`.
    pub weights: [f32; 4],
}

impl SkinningVertexData {
    /// The GPU vertex buffer layout for skinning attributes (slot 2).
    ///
    /// Shader locations 9–10 follow the instance attributes (4–8).
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SkinningVertexData>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            9 => Uint16x4,
            10 => Float32x4,
        ],
    };
}

/// Per-vertex tangent-space direction uploaded separately from [`Vertex`].
///
/// `xyz` is the object-space tangent and `w` is its bitangent handedness.
/// A zero `w` is the renderer's explicit "source tangent unavailable" marker;
/// material shaders retain the derivative-based TBN fallback for that case.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct TangentVertexData {
    /// Object-space tangent direction and handedness.
    pub tangent: [f32; 4],
}

impl TangentVertexData {
    /// GPU vertex layout used by both static and skinned material pipelines.
    ///
    /// Location 15 is outside the existing mesh, instance, skinning, and
    /// outline attribute ranges without changing [`Vertex::LAYOUT`].
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![15 => Float32x4],
    };
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
    ///
    /// When present, the length must equal `vertices.len()`.
    pub skinning: Option<Vec<SkinningVertexData>>,
    /// Optional imported tangent vectors (`xyz`) and handedness (`w`).
    ///
    /// Tangents remain separate from [`Vertex`] so the stable built-in vertex
    /// layout and morph-position upload contract do not change. Material
    /// pipelines consume this stream when present; meshes without source
    /// tangents retain the derivative-based normal-map fallback.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Ranges drawn with independent material slots (ADR 0076).
    ///
    /// Empty means the mesh draws as one range covering all of its indices
    /// (or vertices when unindexed), which is how every mesh without
    /// per-part materials behaves.
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
    ///
    /// Callers iterate this instead of [`Mesh::submeshes`] so a mesh that
    /// declares no ranges still yields exactly one draw.
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
}

/// Extracts the vertices `range` draws into a fully self-contained mesh with
/// skinning and tangent data dropped (ADR 0089 §3).
///
/// Source vertex positions are kept unchanged: a skin's bind pose is defined
/// so `joint_world * inverse_bind` is the identity per joint at rest
/// (ADR 0043), so the imported mesh's raw positions already are the correct
/// baked geometry. This is a reindex, not a pose computation.
///
/// Indices (or, for an unindexed mesh, vertex positions) outside the source
/// vertex list are skipped rather than treated as an error, matching
/// [`Mesh::validate`]'s degrade-gracefully posture for malformed ranges.
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

/// Serializes `mesh` as Wavefront OBJ text: `v`/`vt`/`vn`/`f` lines only
/// (ADR 0089 §4).
///
/// [`Vertex`] always carries a normal and a UV, so every emitted face
/// references all three per corner. Vertex color has no OBJ representation
/// and is dropped without loss: the high-level engine OBJ loader ignores
/// vertex color on import, setting every loaded vertex to white. `mesh` must
/// carry indices in triangle-list order; a non-triangle remainder at the end
/// of the list is silently dropped, matching the renderer's own
/// triangle-list-only assumption.
pub fn mesh_to_obj(mesh: &Mesh) -> String {
    let mut text = String::new();
    for vertex in &mesh.vertices {
        let [x, y, z] = vertex.position;
        let _ = writeln!(text, "v {x} {y} {z}");
    }
    for vertex in &mesh.vertices {
        // The high-level engine OBJ loader flips V on read (`1.0 - v`) to match the
        // glTF importer's UV convention; flipping again here round-trips the
        // original UV exactly.
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

impl Mesh {
    /// Validates that this mesh can be uploaded and drawn by the built-in pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error for empty data, counts that exceed the GPU draw API,
    /// or indices that refer outside the vertex list.
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
            && skinning.len() != self.vertices.len() {
                return Err(MeshValidationError::SkinningLengthMismatch {
                    skinning_count: skinning.len(),
                    vertex_count: self.vertices.len(),
                });
            }

        if let Some(tangents) = &self.tangents
            && tangents.len() != self.vertices.len() {
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
            (
                [1.0, 0.0, 0.0],
                [
                    [0.5, 0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, 0.5, -0.5],
                ],
            ),
            (
                [-1.0, 0.0, 0.0],
                [
                    [-0.5, 0.5, -0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, -0.5, 0.5],
                    [-0.5, 0.5, 0.5],
                ],
            ),
            (
                [0.0, 1.0, 0.0],
                [
                    [-0.5, 0.5, -0.5],
                    [-0.5, 0.5, 0.5],
                    [0.5, 0.5, 0.5],
                    [0.5, 0.5, -0.5],
                ],
            ),
            (
                [0.0, -1.0, 0.0],
                [
                    [-0.5, -0.5, 0.5],
                    [-0.5, -0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [0.5, -0.5, 0.5],
                ],
            ),
            (
                [0.0, 0.0, 1.0],
                [
                    [-0.5, 0.5, 0.5],
                    [-0.5, -0.5, 0.5],
                    [0.5, -0.5, 0.5],
                    [0.5, 0.5, 0.5],
                ],
            ),
            (
                [0.0, 0.0, -1.0],
                [
                    [0.5, 0.5, -0.5],
                    [0.5, -0.5, -0.5],
                    [-0.5, -0.5, -0.5],
                    [-0.5, 0.5, -0.5],
                ],
            ),
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
                Vertex {
                    position: [-half_width, 0.0, -half_depth],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [0.0, 0.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [-half_width, 0.0, half_depth],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [0.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [half_width, 0.0, half_depth],
                    normal,
                    color: [1.0, 1.0, 1.0],
                    uv: [1.0, 1.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                },
                Vertex {
                    position: [half_width, 0.0, -half_depth],
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
                    color: [1.0, 1.0, 1.0],
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
                // Counter-clockwise winding viewed from outside, matching the
                // pipeline's front_face: Ccw with back-face culling.
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

fn gpu_tangent_data(mesh: &Mesh) -> Vec<TangentVertexData> {
    match &mesh.tangents {
        Some(tangents) => tangents
            .iter()
            .copied()
            .map(|tangent| TangentVertexData { tangent })
            .collect(),
        None => vec![
            TangentVertexData {
                tangent: [0.0; 4],
            };
            mesh.vertices.len()
        ],
    }
}

/// Stores GPU buffers for one uploaded [`Mesh`].
///
/// Buffer handles are reference counted so render extraction can own a cheap
/// snapshot without retaining raw pointers into ECS storage.
#[derive(Clone)]
pub struct GpuMesh {
    /// The uploaded vertex buffer.
    pub vertex_buffer: Arc<wgpu::Buffer>,
    /// The uploaded index buffer, when the source mesh is indexed.
    pub index_buffer: Option<Arc<wgpu::Buffer>>,
    /// The uploaded skinning attribute buffer (slot 2), when the source mesh
    /// carries skinning data (ADR 0043).
    pub skinning_buffer: Option<Arc<wgpu::Buffer>>,
    /// Tangent-space data consumed only by material pipelines.
    ///
    /// This buffer is always present. Meshes without imported tangents carry
    /// the zero-handedness sentinel defined by [`TangentVertexData`].
    pub tangent_buffer: Arc<wgpu::Buffer>,
    /// The number of vertices in the source mesh.
    pub vertex_count: u32,
    /// The number of indices in the source mesh.
    pub index_count: u32,
    /// Ranges drawn with independent material slots (ADR 0076).
    ///
    /// Always non-empty: a source mesh that declares no submeshes yields one
    /// range covering the whole mesh, so draw code never special-cases it.
    pub submeshes: Vec<Submesh>,
}

// wgpu's WebGPU backend on wasm32-unknown-unknown wraps JS objects in Rc<RefCell<...>>
// making them !Send/!Sync. wasm32-unknown-unknown is strictly single-threaded so
// there is never an actual cross-thread transfer; these impls are sound.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for GpuMesh {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for GpuMesh {}

impl GpuMesh {
    /// Uploads a CPU-side mesh to GPU buffers.
    ///
    /// # Errors
    ///
    /// Returns an error when `mesh` cannot be represented safely by the
    /// built-in GPU draw path.
    pub fn upload(device: &wgpu::Device, mesh: &Mesh) -> Result<Self, MeshValidationError> {
        use wgpu::util::DeviceExt;

        mesh.validate()?;
        let vertex_count = u32::try_from(mesh.vertices.len()).map_err(|_| {
            MeshValidationError::TooManyVertices {
                count: mesh.vertices.len(),
            }
        })?;
        let maximum_buffer_size = device.limits().max_buffer_size;
        let vertex_buffer_size = u64::from(vertex_count) * std::mem::size_of::<Vertex>() as u64;
        if vertex_buffer_size > maximum_buffer_size {
            return Err(MeshValidationError::VertexBufferTooLarge {
                size: vertex_buffer_size,
                maximum: maximum_buffer_size,
            });
        }

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Vertex buffer"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            // `COPY_DST` so a morphing entity's blended positions can be
            // written into its own buffer each frame (ADR 0097 §5). Granting
            // it unconditionally keeps one upload path: the flag only permits
            // writes, it does not cost anything for the meshes that never
            // take one.
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        let (index_buffer, index_count) = if let Some(indices) = &mesh.indices {
            let index_count =
                u32::try_from(indices.len()).map_err(|_| MeshValidationError::TooManyIndices {
                    count: indices.len(),
                })?;
            let index_buffer_size = u64::from(index_count) * std::mem::size_of::<u32>() as u64;
            if index_buffer_size > maximum_buffer_size {
                return Err(MeshValidationError::IndexBufferTooLarge {
                    size: index_buffer_size,
                    maximum: maximum_buffer_size,
                });
            }
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Index buffer"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            (Some(Arc::new(buffer)), index_count)
        } else {
            (None, 0)
        };

        let skinning_buffer = if let Some(skinning) = &mesh.skinning {
            let skinning_buffer_size =
                u64::from(vertex_count) * std::mem::size_of::<SkinningVertexData>() as u64;
            if skinning_buffer_size > maximum_buffer_size {
                return Err(MeshValidationError::VertexBufferTooLarge {
                    size: skinning_buffer_size,
                    maximum: maximum_buffer_size,
                });
            }
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Skinning vertex buffer"),
                contents: bytemuck::cast_slice(skinning),
                usage: wgpu::BufferUsages::VERTEX,
            });
            Some(Arc::new(buffer))
        } else {
            None
        };

        let tangent_data = gpu_tangent_data(mesh);
        let tangent_buffer_size =
            u64::from(vertex_count) * std::mem::size_of::<TangentVertexData>() as u64;
        if tangent_buffer_size > maximum_buffer_size {
            return Err(MeshValidationError::VertexBufferTooLarge {
                size: tangent_buffer_size,
                maximum: maximum_buffer_size,
            });
        }
        let tangent_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Tangent vertex buffer"),
            contents: bytemuck::cast_slice(&tangent_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Ok(Self {
            vertex_buffer: Arc::new(vertex_buffer),
            index_buffer,
            skinning_buffer,
            tangent_buffer: Arc::new(tangent_buffer),
            vertex_count,
            index_count,
            submeshes: mesh.draw_ranges(),
        })
    }

    /// Returns the range to draw for `submesh`, clamped to this mesh.
    ///
    /// An out-of-range slot yields an empty range so a mismatch between a
    /// mesh and stale material slots draws nothing instead of reading past
    /// the buffer.
    fn range(&self, submesh: Option<usize>) -> std::ops::Range<u32> {
        let total = if self.index_buffer.is_some() {
            self.index_count
        } else {
            self.vertex_count
        };
        let Some(index) = submesh else {
            return 0..total;
        };
        let Some(range) = self.submeshes.get(index) else {
            return 0..0;
        };
        let start = range.start.min(total);
        let end = range.start.saturating_add(range.count).min(total);
        start..end
    }

    /// Records draw commands for this mesh (single instance).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw_submesh(pass, None);
    }

    /// Records draw commands for one submesh, or the whole mesh for `None`.
    pub fn draw_submesh<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, submesh: Option<usize>) {
        let range = self.range(submesh);
        if range.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        if let Some(index_buffer) = &self.index_buffer {
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range, 0, 0..1);
        } else {
            pass.draw(range, 0..1);
        }
    }

    /// Records a single skinned draw binding the skinning buffer at slot 2.
    ///
    /// `instance_buffer` supplies one [`InstanceData`] (model matrix and tint
    /// color) at slot 1, mirroring the static instanced path. Returns `false`
    /// without drawing when this mesh has no skinning buffer, so callers can
    /// fall back to the static path instead of panicking.
    pub fn draw_skinned<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
    ) -> bool {
        self.draw_skinned_submesh(pass, instance_buffer, None)
    }

    /// Records one skinned draw restricted to `submesh`.
    pub fn draw_skinned_submesh<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
        submesh: Option<usize>,
    ) -> bool {
        let Some(skinning_buffer) = &self.skinning_buffer else {
            return false;
        };
        let range = self.range(submesh);
        if range.is_empty() {
            return true;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, instance_buffer.slice(..));
        pass.set_vertex_buffer(2, skinning_buffer.slice(..));
        if let Some(index_buffer) = &self.index_buffer {
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range, 0, 0..1);
        } else {
            pass.draw(range, 0..1);
        }
        true
    }

    /// Records one static material draw with the tangent stream at slot 2.
    pub(crate) fn draw_material_instanced_submesh<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
        instance_count: u32,
        submesh: Option<usize>,
    ) {
        pass.set_vertex_buffer(2, self.tangent_buffer.slice(..));
        self.draw_instanced_submesh(pass, instance_buffer, instance_count, submesh);
    }

    /// Records one skinned material draw with the tangent stream at slot 3.
    pub(crate) fn draw_material_skinned_submesh<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
        submesh: Option<usize>,
    ) -> bool {
        pass.set_vertex_buffer(3, self.tangent_buffer.slice(..));
        self.draw_skinned_submesh(pass, instance_buffer, submesh)
    }

    /// Records instanced draw commands using `instance_buffer` as vertex slot 1.
    pub fn draw_instanced<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
        instance_count: u32,
    ) {
        self.draw_instanced_submesh(pass, instance_buffer, instance_count, None);
    }

    /// Records instanced draw commands restricted to `submesh`.
    pub fn draw_instanced_submesh<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instance_buffer: &'a wgpu::Buffer,
        instance_count: u32,
        submesh: Option<usize>,
    ) {
        let range = self.range(submesh);
        if range.is_empty() {
            return;
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, instance_buffer.slice(..));
        if let Some(index_buffer) = &self.index_buffer {
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range, 0, 0..instance_count);
        } else {
            pass.draw(range, 0..instance_count);
        }
    }
}

/// Per-instance data uploaded to vertex slot 1.
///
/// Must match the `InstanceInput` struct in both mesh shaders — 112 bytes
/// total. Locations 9–10 remain reserved for skinned vertex attributes, so
/// material data intentionally resumes at locations 11–12.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct InstanceData {
    /// Column-major model matrix.
    pub model: [[f32; 4]; 4],
    /// RGBA tint color (multiplied with vertex and texture color in the shader).
    pub color: [f32; 4],
    /// Emissive RGB and an unlit flag in `w`.
    pub emissive_and_model: [f32; 4],
    /// Roughness, metallic, alpha cutoff, and alpha-mode numeric code.
    pub surface: [f32; 4],
}

impl InstanceData {
    /// Vertex buffer layout for per-instance data (step mode = Instance).
    ///
    /// Shader locations 4–8 and 11–12: matrix, color, and material properties.
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<InstanceData>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            4 => Float32x4,  // model col 0
            5 => Float32x4,  // model col 1
            6 => Float32x4,  // model col 2
            7 => Float32x4,  // model col 3
            8 => Float32x4,  // color
            11 => Float32x4, // emissive RGB + unlit flag
            12 => Float32x4, // roughness + metallic + alpha cutoff/mode
        ],
    };

    /// Constructs an `InstanceData` from a world-space matrix and RGBA color.
    pub fn from_transform_color(matrix: glam::Mat4, color: [f32; 4]) -> Self {
        Self {
            model: matrix.to_cols_array_2d(),
            color,
            emissive_and_model: [0.0, 0.0, 0.0, 0.0],
            surface: [0.5, 0.0, 0.5, 0.0],
        }
    }

    /// Constructs an instance with the complete built-in material payload.
    pub fn from_transform_material(
        matrix: glam::Mat4,
        color: [f32; 4],
        emissive_and_model: [f32; 4],
        surface: [f32; 4],
    ) -> Self {
        Self {
            model: matrix.to_cols_array_2d(),
            color,
            emissive_and_model,
            surface,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MeshContentKey {
    first: u64,
    second: u64,
    vertices: usize,
    indices: usize,
}

/// GPU uploads shared by preview worlds that use the same wgpu device.
#[derive(Clone, Default)]
pub struct SharedGpuMeshCache {
    entries: Arc<Mutex<HashMap<MeshContentKey, GpuMesh>>>,
}

impl SharedGpuMeshCache {
    /// Returns an upload already shared for identical mesh contents.
    #[doc(hidden)]
    pub fn get(&self, mesh: &Mesh) -> Option<GpuMesh> {
        let key = mesh_content_key(mesh);
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
    }

    /// Shares a GPU upload under the mesh's stable content key.
    #[doc(hidden)]
    pub fn insert(&self, mesh: &Mesh, gpu_mesh: GpuMesh) {
        let key = mesh_content_key(mesh);
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, gpu_mesh);
    }

    /// Removes every resident upload, releasing buffers after current users drop them.
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

}

/// Per-world aliases into an optional shared GPU mesh upload cache.
///
/// Allows multiple entities that reference the same mesh handle to share
/// one `Arc<wgpu::Buffer>`, making them eligible for instance-draw batching.
///
/// Insert this resource into the world before any rendering begins.
pub struct GpuMeshCache {
    meshes: HashMap<RuntimeAssetId, GpuMesh>,
    shared: SharedGpuMeshCache,
}

impl GpuMeshCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self::with_shared(SharedGpuMeshCache::default())
    }

    /// Creates a world-local alias table backed by shared resident uploads.
    pub fn with_shared(shared: SharedGpuMeshCache) -> Self {
        Self {
            meshes: HashMap::new(),
            shared,
        }
    }

    /// Returns the cached [`GpuMesh`] for `id`, or `None` if not yet uploaded.
    pub fn get(&self, id: RuntimeAssetId) -> Option<&GpuMesh> {
        self.meshes.get(&id)
    }

    /// Inserts a [`GpuMesh`] into the cache.
    pub fn insert(&mut self, id: RuntimeAssetId, gpu_mesh: GpuMesh) {
        self.meshes.insert(id, gpu_mesh);
    }

    /// Iterates the runtime asset IDs aliased by this world-local cache.
    #[doc(hidden)]
    pub fn runtime_ids(&self) -> impl Iterator<Item = RuntimeAssetId> + '_ {
        self.meshes.keys().copied()
    }

    /// Returns the shared resident upload cache backing this world.
    #[doc(hidden)]
    pub fn shared(&self) -> SharedGpuMeshCache {
        self.shared.clone()
    }
}

impl Default for GpuMeshCache {
    fn default() -> Self {
        Self::new()
    }
}

fn mesh_content_key(mesh: &Mesh) -> MeshContentKey {
    let mut first = 0xcbf2_9ce4_8422_2325_u64;
    let mut second = 0x9e37_79b9_7f4a_7c15_u64;
    let mut hash = |bytes: &[u8]| {
        for &byte in bytes {
            first ^= u64::from(byte);
            first = first.wrapping_mul(0x0000_0100_0000_01b3);
            second ^= u64::from(byte).wrapping_add(0x9d);
            second = second.rotate_left(7).wrapping_mul(0x9e37_79b1_85eb_ca87);
        }
    };
    hash(bytemuck::cast_slice(&mesh.vertices));
    if let Some(indices) = &mesh.indices {
        hash(bytemuck::cast_slice(indices));
    }
    if let Some(skinning) = &mesh.skinning {
        hash(bytemuck::cast_slice(skinning));
    }
    if let Some(tangents) = &mesh.tangents {
        hash(bytemuck::cast_slice(tangents));
    }
    for submesh in &mesh.submeshes {
        hash(&submesh.start.to_le_bytes());
        hash(&submesh.count.to_le_bytes());
    }
    MeshContentKey {
        first,
        second,
        vertices: mesh.vertices.len(),
        indices: mesh.indices.as_ref().map_or(0, Vec::len),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_content_key_is_stable_and_changes_with_geometry() {
        let original = Mesh::triangle();
        let identical = original.clone();
        let mut changed = original.clone();
        changed.vertices[0].position[0] += 0.25;

        assert_eq!(mesh_content_key(&original), mesh_content_key(&identical));
        assert_ne!(mesh_content_key(&original), mesh_content_key(&changed));
    }

    #[test]
    fn a_mesh_without_submeshes_draws_as_one_full_range() {
        let mesh = Mesh::triangle();

        let ranges = mesh.draw_ranges();

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        let expected = mesh
            .indices
            .as_ref()
            .map(Vec::len)
            .unwrap_or(mesh.vertices.len());
        assert_eq!(ranges[0].count as usize, expected);
    }

    #[test]
    fn declared_submeshes_are_returned_unchanged() {
        let mut mesh = Mesh::quad();
        let declared = vec![
            Submesh { start: 0, count: 3 },
            Submesh { start: 3, count: 3 },
        ];
        mesh.submeshes = declared.clone();

        assert_eq!(mesh.draw_ranges(), declared);
    }

    #[test]
    fn mesh_validation_rejects_out_of_bounds_indices() {
        let mut mesh = Mesh::triangle();
        mesh.indices = Some(vec![0, 1, 3]);

        assert_eq!(
            mesh.validate(),
            Err(MeshValidationError::IndexOutOfBounds {
                position: 2,
                index: 3,
                vertex_count: 3,
            })
        );
    }

    #[test]
    fn built_in_meshes_are_valid() {
        assert!(Mesh::triangle().validate().is_ok());
        assert!(Mesh::quad().validate().is_ok());
        assert!(Mesh::cube().validate().is_ok());
        assert!(Mesh::plane(1.0, 1.0).validate().is_ok());
        assert!(Mesh::sphere(16, 32).validate().is_ok());
    }

    #[test]
    fn vertex_layout_includes_normals() {
        assert_eq!(std::mem::size_of::<Vertex>(), 56);
        assert_eq!(Vertex::LAYOUT.attributes.len(), 5);
        assert_eq!(Vertex::LAYOUT.attributes[1].shader_location, 1);
        assert_eq!(
            Vertex::LAYOUT.attributes[1].format,
            wgpu::VertexFormat::Float32x3
        );
        assert_eq!(Vertex::LAYOUT.attributes[4].shader_location, 13);
    }

    #[test]
    fn tangent_layout_uses_the_reserved_material_location() {
        assert_eq!(std::mem::size_of::<TangentVertexData>(), 16);
        assert_eq!(TangentVertexData::LAYOUT.attributes.len(), 1);
        assert_eq!(TangentVertexData::LAYOUT.attributes[0].shader_location, 15);
        assert_eq!(
            TangentVertexData::LAYOUT.attributes[0].format,
            wgpu::VertexFormat::Float32x4
        );
    }

    #[test]
    fn gpu_tangent_data_preserves_source_and_marks_missing_tangents() {
        let missing = Mesh::triangle();
        let missing_gpu = gpu_tangent_data(&missing);
        assert_eq!(missing_gpu.len(), missing.vertices.len());
        assert!(missing_gpu.iter().all(|entry| entry.tangent == [0.0; 4]));

        let mut sourced = Mesh::triangle();
        sourced.tangents = Some(vec![[1.0, 0.0, 0.0, -1.0]; sourced.vertices.len()]);
        let sourced_gpu = gpu_tangent_data(&sourced);
        assert!(sourced_gpu
            .iter()
            .all(|entry| entry.tangent == [1.0, 0.0, 0.0, -1.0]));
    }

    #[test]
    fn cube_has_distinct_vertices_per_face() {
        let mesh = Mesh::cube();

        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.as_ref().map(Vec::len), Some(36));
        for face in mesh.vertices.chunks_exact(4) {
            let normal = face[0].normal;
            assert!(face.iter().all(|vertex| vertex.normal == normal));
        }
    }

    #[test]
    fn plane_uses_xz_coordinates_and_upward_normals() {
        let mesh = Mesh::plane(2.0, 4.0);

        assert!(mesh.vertices.iter().all(|vertex| vertex.position[1] == 0.0));
        assert!(mesh
            .vertices
            .iter()
            .all(|vertex| vertex.normal == [0.0, 1.0, 0.0]));
    }

    #[test]
    fn sphere_triangles_wind_counter_clockwise_viewed_from_outside() {
        let mesh = Mesh::sphere(16, 32);
        let indices = mesh.indices.as_ref().expect("sphere must be indexed");

        for triangle in indices.chunks_exact(3) {
            let [v0, v1, v2] = [
                &mesh.vertices[triangle[0] as usize],
                &mesh.vertices[triangle[1] as usize],
                &mesh.vertices[triangle[2] as usize],
            ];
            let p0 = glam::Vec3::from_array(v0.position);
            let p1 = glam::Vec3::from_array(v1.position);
            let p2 = glam::Vec3::from_array(v2.position);
            let geometric_normal = (p1 - p0).cross(p2 - p0);

            // Pole quads contribute one zero-area triangle; winding is
            // meaningless there.
            if geometric_normal.length_squared() < 1e-12 {
                continue;
            }

            let vertex_normal = (glam::Vec3::from_array(v0.normal)
                + glam::Vec3::from_array(v1.normal)
                + glam::Vec3::from_array(v2.normal))
                / 3.0;
            assert!(
                geometric_normal.dot(vertex_normal) > 0.0,
                "triangle {triangle:?} winds clockwise viewed from outside"
            );
        }
    }

    #[test]
    fn sphere_counts_and_normals_are_stable() {
        let mesh = Mesh::sphere(16, 32);

        assert_eq!(mesh.vertices.len(), 17 * 33);
        assert_eq!(mesh.indices.as_ref().map(Vec::len), Some(16 * 32 * 6));
        for vertex in &mesh.vertices {
            let normal = glam::Vec3::from_array(vertex.normal);
            assert!(
                (normal.length() - 1.0).abs() < 0.0001,
                "sphere normal must be unit length"
            );
        }
    }

    /// A two-triangle quad with distinct skinning per vertex, standing in
    /// for one submesh of a skinned render part (ADR 0089).
    fn skinned_quad() -> Mesh {
        let mut mesh = Mesh::quad();
        mesh.skinning = Some(vec![
            SkinningVertexData {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            mesh.vertices.len()
        ]);
        mesh.tangents = Some(vec![[1.0, 0.0, 0.0, 1.0]; mesh.vertices.len()]);
        mesh
    }

    #[test]
    fn extract_baked_submesh_drops_skinning_and_tangents_but_keeps_positions() {
        let mesh = skinned_quad();
        let range = mesh.draw_ranges()[0];

        let baked = extract_baked_submesh(&mesh, range);

        assert!(baked.skinning.is_none());
        assert!(baked.tangents.is_none());
        assert!(baked.submeshes.is_empty());
        let positions: Vec<[f32; 3]> = baked.vertices.iter().map(|v| v.position).collect();
        let expected: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| v.position).collect();
        assert_eq!(
            positions, expected,
            "bind pose positions are copied verbatim (ADR 0089 §3)"
        );
    }

    #[test]
    fn extract_baked_submesh_only_includes_vertices_the_range_uses() {
        let mesh = Mesh::cube();
        // One face's worth of a multi-face mesh: the extracted mesh must be
        // self-contained and reindexed from zero, not a slice of the whole.
        let range = mesh.draw_ranges()[0];
        let one_face_range = crate::mesh::Submesh {
            start: range.start,
            count: 6,
        };

        let baked = extract_baked_submesh(&mesh, one_face_range);

        assert!(
            baked.vertices.len() <= 4,
            "one quad face has at most 4 distinct vertices"
        );
        assert_eq!(baked.indices.as_ref().map(Vec::len), Some(6));
        assert!(baked
            .indices
            .as_ref()
            .unwrap()
            .iter()
            .all(|&index| (index as usize) < baked.vertices.len()));
    }

    #[test]
    fn extract_baked_submesh_skips_out_of_range_indices_instead_of_panicking() {
        let mesh = Mesh::triangle();
        let out_of_range = crate::mesh::Submesh {
            start: 0,
            count: 1_000,
        };

        let baked = extract_baked_submesh(&mesh, out_of_range);

        assert_eq!(baked.vertices.len(), 3);
    }

    #[test]
    fn mesh_to_obj_drops_a_trailing_partial_triangle_instead_of_panicking() {
        let mut mesh = Mesh::triangle();
        mesh.indices = Some(vec![0, 1, 2, 0]);

        let text = mesh_to_obj(&mesh);

        assert_eq!(
            text.matches("\nf ").count() + usize::from(text.starts_with("f ")),
            1
        );
    }
}
