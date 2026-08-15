//! FBX parser via the `ufbx` crate (ADR 0081).
//!
//! Parses `.fbx` byte slices and files into a format-independent
//! `ModelDocument` (see `crate::model_ir` for the exact normalization
//! contract), mirroring `crate::gltf_import`'s structure exactly. This
//! module owns every `ufbx` symbol — node/mesh/skin/material/animation
//! extraction — and assigns no sub-asset IDs and no skeleton identity; that
//! remains `crate::model_import`'s job, unchanged by which parser produced
//! its input (ADR 0078).
//!
//! Normalization (meters, right-handed +Y up, baked pivots/PreRotation,
//! linear-sampled clips) is achieved by configuring `ufbx`'s load options
//! (see `base_load_opts`), not by post-processing: `ufbx` owns the FBX
//! semantic layer (unit/axis conversion, geometry-transform helper nodes,
//! animation evaluation) so this module never has to reimplement it.
//!
//! `import_fbx_bytes` and `import_fbx_path` are the public entry points
//! most callers want: they parse and build in one step and return the same
//! `GltfImportResult` shape `crate::gltf_import::import_gltf_bytes`
//! produces (that type's name is a pre-existing misnomer, unchanged by ADR
//! 0078; no serialized format names it).

use crate::asset::SkeletonRecord;
use crate::model_import::GltfImportResult;
use crate::model_ir::{
    IrClip, IrClipChannel, IrMaterial, IrMesh, IrNode, IrSkin, IrTexture, ModelDocument, IrAnimProperty as AnimProperty, IrKeyframe as Keyframe, IrMeshData as Mesh, IrSkinningVertexData as SkinningVertexData, IrSubmesh as Submesh, IrVertex as Vertex,
};
use crate::skinning::MAX_JOINTS;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use engine_authoring::{
    LinearRgba, MaterialAlphaMode, MaterialCullMode, MaterialOutline, MaterialShadingModel,
    MaterialSphereBlendMode, MaterialSphereCoordinateSource,
};
use glam::{Mat4, Quat, Vec3};
use hashbrown::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Tolerated deviation of a vertex weight sum from 1.0 before the importer
/// reports a renormalization warning (parity with [`crate::gltf_import`]).
const WEIGHT_SUM_TOLERANCE: f32 = 1.0e-3;

/// Fallback resample rate, in Hz, used when a source document does not
/// declare a scene frame rate (ADR 0081 §3).
const DEFAULT_FRAME_RATE: f64 = 30.0;

/// Reports a fatal error that prevents parsing from completing.
#[derive(Debug)]
pub enum FbxImportError {
    /// FBX load or evaluation failed; see the wrapped `ufbx` error for detail.
    Parse(Box<ufbx::Error>),
    /// The source path is not valid UTF-8, which `ufbx`'s file-loading API
    /// requires.
    InvalidPath,
}

impl fmt::Display for FbxImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "FBX parse error: {error:?}"),
            Self::InvalidPath => write!(f, "FBX source path is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FbxImportError {}

// ---------------------------------------------------------------------------
// Import entry points
// ---------------------------------------------------------------------------

/// Imports static meshes, skins, and clips from an FBX byte slice.
///
/// Sub-asset IDs are deterministic: re-importing the same `bytes` with the
/// same `source_id` produces identical sub-asset IDs. Internally this parses
/// `bytes` into a `ModelDocument` ([`parse_fbx`]) and hands it to
/// [`crate::model_import::build_import_result`]; see that function for the
/// asset-building contract.
///
/// Only embedded media is resolved; external texture files referenced by
/// filename cannot be read without a source path (see [`import_fbx_path`]).
///
/// See [`crate::gltf_import::import_gltf_bytes`] for the `existing_skeletons`
/// contract; this function has the identical contract.
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when `bytes` is not a valid FBX
/// document.
pub fn import_fbx_bytes(
    source_id: &AssetId,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, FbxImportError> {
    let document = parse_fbx(bytes)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Imports an FBX source from disk, resolving external texture sidecars
/// relative to the source file.
///
/// See [`import_fbx_bytes`] for the `existing_skeletons` contract.
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when the document cannot be loaded, or
/// [`FbxImportError::InvalidPath`] when `path` is not valid UTF-8.
pub fn import_fbx_path(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, FbxImportError> {
    let document = parse_fbx_path(path)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Same as [`import_fbx_path`], but overrides the ground-contact candidate
/// -bone name heuristic (ADR 0080 §1) with `contact_bone_names` when it is
/// non-empty, matching `crate::asset::ImportSettings::contact_bones`'s
/// contract — exact parity with
/// [`crate::gltf_import::import_gltf_path_with_contact_bones`].
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when the document cannot be loaded, or
/// [`FbxImportError::InvalidPath`] when `path` is not valid UTF-8.
pub fn import_fbx_path_with_contact_bones(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, FbxImportError> {
    let document = parse_fbx_path(path)?;
    Ok(crate::model_import::build_import_result_with_contact_bones(
        &document,
        source_id,
        existing_skeletons,
        contact_bone_names,
    ))
}

/// Parses an FBX byte slice into a format-independent `ModelDocument`
/// (ADR 0078), assigning no sub-asset IDs and no skeleton identity.
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when `bytes` is not a valid FBX
/// document.
pub fn parse_fbx(bytes: &[u8]) -> Result<ModelDocument, FbxImportError> {
    let opts = base_load_opts(None);
    let scene =
        ufbx::load_memory(bytes, opts).map_err(|error| FbxImportError::Parse(Box::new(error)))?;
    Ok(build_model_document(&scene, None))
}

/// Parses an FBX source from disk into a format-independent `ModelDocument`
/// (ADR 0078), resolving external texture sidecars relative to the source
/// file.
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when the document cannot be loaded, or
/// [`FbxImportError::InvalidPath`] when `path` is not valid UTF-8.
pub fn parse_fbx_path(path: &Path) -> Result<ModelDocument, FbxImportError> {
    let path_str = path.to_str().ok_or(FbxImportError::InvalidPath)?;
    let opts = base_load_opts(Some(path_str));
    let scene =
        ufbx::load_file(path_str, opts).map_err(|error| FbxImportError::Parse(Box::new(error)))?;
    Ok(build_model_document(&scene, path.parent()))
}

/// Returns external texture sidecars declared by an FBX document. Embedded
/// media is intentionally omitted (parity with
/// [`crate::gltf_import::gltf_source_dependencies`]).
///
/// # Errors
///
/// Returns [`FbxImportError::Parse`] when the document cannot be loaded, or
/// [`FbxImportError::InvalidPath`] when `path` is not valid UTF-8.
pub fn fbx_source_dependencies(path: &Path) -> Result<Vec<PathBuf>, FbxImportError> {
    let path_str = path.to_str().ok_or(FbxImportError::InvalidPath)?;
    let opts = base_load_opts(Some(path_str));
    let scene =
        ufbx::load_file(path_str, opts).map_err(|error| FbxImportError::Parse(Box::new(error)))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut dependencies = Vec::new();
    for texture in &scene.textures {
        if !texture.has_file || !texture.content.is_empty() {
            // Embedded media needs no external dependency tracking, and a
            // texture with no file reference has nothing to track.
            continue;
        }
        let relative = texture.relative_filename.as_ref();
        if relative.is_empty() {
            continue;
        }
        dependencies.push(parent.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)));
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

/// Computes a deterministic content fingerprint over a source and sidecars.
/// Absolute paths are excluded so moving a project does not force reimport
/// (parity with [`crate::gltf_import::fingerprint_gltf_source`]).
pub fn fingerprint_fbx_source(
    source: &Path,
    dependencies: &[PathBuf],
) -> Result<String, std::io::Error> {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in std::iter::once(source).chain(dependencies.iter().map(PathBuf::as_path)) {
        let label = path.strip_prefix(parent).unwrap_or(path).to_string_lossy();
        hash_fnv1a(&mut hash, label.as_bytes());
        hash_fnv1a(&mut hash, &std::fs::read(path)?);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

fn hash_fnv1a(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
}

// ---------------------------------------------------------------------------
// Load options (ADR 0081 §3 normalization contract)
// ---------------------------------------------------------------------------

/// Builds the `ufbx` load options that satisfy [`crate::model_ir`]'s
/// normalization contract: meters, right-handed +Y up, and pivots /
/// `PreRotation` / `PostRotation` baked into a plain local TRS per node.
///
/// `filename` should be the source path (as a UTF-8 string) when loading from
/// disk, so `ufbx` can resolve relative sidecars; pass `None` for a byte-slice
/// parse with no path context.
fn base_load_opts(filename: Option<&str>) -> ufbx::LoadOpts<'_> {
    ufbx::LoadOpts {
        target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
        target_unit_meters: 1.0,
        // `AdjustTransforms` bakes the unit/axis conversion into every
        // node's own `local_transform`, unlike the default `TransformRoot`
        // which only adjusts the scene root; the IR contract requires every
        // node's local TRS to already be normalized in isolation.
        space_conversion: ufbx::SpaceConversion::AdjustTransforms,
        // Route geometry transforms (pivots baked as a non-inherited offset
        // between a node and its mesh) through synthetic helper nodes rather
        // than leaving them as residue on the mesh-owning node's transform
        // (ADR 0081 §3).
        geometry_transform_handling: ufbx::GeometryTransformHandling::HelperNodes,
        // Maya's "Segment Scale Compensate" rigs (the common case for
        // Mixamo skeletons) mark most bones `UFBX_INHERIT_MODE_IGNORE_PARENT_SCALE`
        // so a per-bone scale does not compound down the joint chain. The IR
        // contract has no inherit-mode concept, and `transform_propagation_system`
        // always fully inherits parent scale, so without this option every
        // level of the skeleton would multiply in the bone's compensating
        // scale again, collapsing deep joints (fingers, toes) toward the
        // root. `HelperNodes` bakes the compensation into synthetic scale
        // helper nodes instead, matching `geometry_transform_handling`
        // above so plain SRT composition already yields the correct result.
        inherit_mode_handling: ufbx::InheritModeHandling::HelperNodes,
        // Recovers roughness/metallic from Blender's legacy Phong export so
        // `material.pbr` is populated consistently regardless of the
        // authoring tool (ADR 0081 §3 "lambert/phong... map through ufbx's
        // PBR view").
        use_blender_pbr_material: true,
        filename: filename.map(ufbx::StringOpt::from).unwrap_or_default(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Parser core
// ---------------------------------------------------------------------------

/// Parses a loaded FBX scene into a `ModelDocument` (ADR 0078).
///
/// `source_dir` is the source file's parent directory, used to resolve
/// non-embedded texture files; pass `None` for a byte-slice parse (any such
/// texture is dropped with a diagnostic instead).
fn build_model_document(scene: &ufbx::Scene, source_dir: Option<&Path>) -> ModelDocument {
    let mut diagnostics = Vec::new();

    let nodes = parse_nodes(scene);
    let (skins, cluster_to_joint) = parse_skins(scene, nodes.len(), &mut diagnostics);
    let meshes = parse_meshes(scene, &cluster_to_joint, &mut diagnostics);
    let clips = parse_clips(scene, &skins, &mut diagnostics);
    let (textures, texture_valid) = parse_textures(scene, source_dir, &mut diagnostics);
    let materials = parse_materials(scene, &texture_valid);

    ModelDocument {
        nodes,
        meshes,
        skins,
        // An FBX skin's joint list is its skeleton; each skin keeps its own
        // (ADR 0097 §4a's default, unchanged FBX behavior).
        skeleton_scope: crate::model_ir::SkeletonScope::PerSkin,
        clips,
        materials,
        textures,
        // FBX has no secondary-motion physics concept (ADR 0097 §6).
        rigid_body_rig: None,
        diagnostics,
    }
}

/// Returns `element`'s name, or `fallback` when the source left it blank.
fn element_name(element: &ufbx::Element, fallback: &str) -> String {
    let name = element.name.as_ref();
    if name.is_empty() {
        fallback.to_owned()
    } else {
        name.to_owned()
    }
}

fn to_vec3(v: ufbx::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

fn to_quat(q: ufbx::Quat) -> Quat {
    Quat::from_xyzw(q.x as f32, q.y as f32, q.z as f32, q.w as f32)
}

/// Converts `ufbx`'s column-major 3x4 affine matrix to a `glam::Mat4`.
fn to_mat4(m: &ufbx::Matrix) -> Mat4 {
    Mat4::from_cols_array(&[
        m.m00 as f32,
        m.m10 as f32,
        m.m20 as f32,
        0.0,
        m.m01 as f32,
        m.m11 as f32,
        m.m21 as f32,
        0.0,
        m.m02 as f32,
        m.m12 as f32,
        m.m22 as f32,
        0.0,
        m.m03 as f32,
        m.m13 as f32,
        m.m23 as f32,
        1.0,
    ])
}

/// Parses every scene node as an [`IrNode`], preserving `ufbx`'s stable
/// `typed_id` selectors (ADR 0081 §5).
///
/// [`IrNode::translation`] / `.rotation` / `.scale` read `local_transform`
/// directly: with the load options in `base_load_opts`, `ufbx` already
/// bakes unit/axis conversion and any pivot / `PreRotation` / `PostRotation`
/// into this single decomposed TRS, satisfying the module normalization
/// contract without post-processing here.
fn parse_nodes(scene: &ufbx::Scene) -> Vec<IrNode> {
    scene
        .nodes
        .iter()
        .map(|node| IrNode {
            name: element_name(&node.element, "node"),
            parent: node
                .parent
                .as_ref()
                .map(|parent| parent.element.typed_id as usize),
            translation: to_vec3(node.local_transform.translation),
            rotation: to_quat(node.local_transform.rotation),
            scale: to_vec3(node.local_transform.scale),
            mesh: node
                .mesh
                .as_ref()
                .map(|mesh| mesh.element.typed_id as usize),
            skin: node.mesh.as_ref().and_then(|mesh| {
                mesh.skin_deformers
                    .first()
                    .map(|deformer| deformer.element.typed_id as usize)
            }),
        })
        .collect()
}

/// Parses every structurally valid skin deformer as an [`IrSkin`], dropping
/// ones that exceed [`MAX_JOINTS`] or reference a node outside the document
/// (parity with [`crate::gltf_import`]'s skin handling).
///
/// Also returns, per skin (keyed by its `source_index`), a `cluster index ->
/// joint position` map: a cluster missing its bone node is excluded from
/// [`IrSkin::joint_nodes`], so later per-vertex weight resolution (which
/// FBX keys by cluster index) needs this to find the right joint position.
fn parse_skins(
    scene: &ufbx::Scene,
    node_count: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<IrSkin>, HashMap<usize, Vec<Option<usize>>>) {
    let mut skins = Vec::new();
    let mut cluster_maps = HashMap::new();

    for deformer in &scene.skin_deformers {
        let name = element_name(&deformer.element, "unnamed");
        let mut joint_nodes = Vec::with_capacity(deformer.clusters.len());
        let mut inverse_bind_matrices = Vec::with_capacity(deformer.clusters.len());
        let mut cluster_to_joint: Vec<Option<usize>> = Vec::with_capacity(deformer.clusters.len());

        for cluster in &deformer.clusters {
            let Some(bone_node) = cluster.bone_node.as_ref() else {
                cluster_to_joint.push(None);
                continue;
            };
            cluster_to_joint.push(Some(joint_nodes.len()));
            joint_nodes.push(bone_node.element.typed_id as usize);
            inverse_bind_matrices.push(to_mat4(&cluster.geometry_to_bone));
        }

        let source_index = deformer.element.typed_id as usize;
        cluster_maps.insert(source_index, cluster_to_joint);

        if joint_nodes.len() > MAX_JOINTS {
            diagnostics.push(Diagnostic::error(
                "fbx.skin_too_many_joints",
                format!(
                    "skin '{name}' has {} joints, exceeding the supported maximum of {MAX_JOINTS}; skin dropped",
                    joint_nodes.len()
                ),
            ));
            continue;
        }
        if joint_nodes.iter().any(|&joint| joint >= node_count) {
            diagnostics.push(Diagnostic::error(
                "fbx.skin_invalid_joint",
                format!("skin '{name}' references a node outside the document; skin dropped"),
            ));
            continue;
        }

        skins.push(IrSkin {
            source_index,
            name,
            joint_nodes,
            inverse_bind_matrices,
        });
    }

    (skins, cluster_maps)
}

/// Parses every mesh element with at least one indexable vertex as an
/// [`IrMesh`] (ADR 0076 submesh ranges included).
///
/// FBX mesh geometry is exposed per-corner (one entry per polygon-vertex
/// slot, `mesh.num_indices` wide) rather than per shared vertex, so unlike
/// [`crate::gltf_import`]'s accessor-driven approach, no base-vertex offset
/// bookkeeping is needed: [`IrMesh::mesh`]'s vertex list is built directly at
/// that corner width and triangulated faces already index into it.
fn parse_meshes(
    scene: &ufbx::Scene,
    cluster_to_joint: &HashMap<usize, Vec<Option<usize>>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrMesh> {
    let mut meshes = Vec::new();

    for mesh in &scene.meshes {
        let name = element_name(&mesh.element, "unnamed");
        let num_indices = mesh.num_indices;

        if !mesh.vertex_position.exists || num_indices == 0 {
            diagnostics.push(Diagnostic::warning(
                "fbx.missing_positions",
                format!("mesh '{name}' has no position data; mesh skipped"),
            ));
            continue;
        }

        let has_normals = mesh.vertex_normal.exists;
        if !has_normals {
            diagnostics.push(Diagnostic::warning(
                "fbx.missing_normals",
                format!("mesh '{name}' has no normal data; upward normals were substituted"),
            ));
        }
        let has_uv = mesh.vertex_uv.exists;
        if !has_uv {
            diagnostics.push(Diagnostic::warning(
                "fbx.missing_uv0",
                format!("mesh '{name}' has no UV data; zero UVs were substituted"),
            ));
        }

        let mut vertices = Vec::with_capacity(num_indices);
        for i in 0..num_indices {
            let position = mesh.vertex_position[i];
            let normal = if has_normals {
                mesh.vertex_normal[i]
            } else {
                ufbx::Vec3 {
                    x: 0.0,
                    y: 1.0,
                    z: 0.0,
                }
            };
            // FBX places the UV origin at the bottom left, while the engine
            // follows glTF's top-left origin ([`crate::gltf_import`] passes
            // its UVs through unchanged). Without flipping `v`, every UV
            // island samples the vertically mirrored part of the atlas — a
            // character's face lands on its back, upside down.
            let uv = if has_uv {
                let uv = mesh.vertex_uv[i];
                [uv.x as f32, 1.0 - uv.y as f32]
            } else {
                [0.0, 0.0]
            };
            vertices.push(Vertex {
                position: [position.x as f32, position.y as f32, position.z as f32],
                normal: [normal.x as f32, normal.y as f32, normal.z as f32],
                color: [1.0, 1.0, 1.0],
                uv,
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            });
        }

        let has_tangents = mesh.vertex_tangent.exists;
        let tangents = has_tangents.then(|| {
            (0..num_indices)
                .map(|i| {
                    let t = mesh.vertex_tangent[i];
                    [t.x as f32, t.y as f32, t.z as f32, 1.0]
                })
                .collect()
        });

        let (skinning, renormalized_weights, truncated_weights) =
            parse_mesh_skinning(mesh, cluster_to_joint);
        if renormalized_weights > 0 {
            diagnostics.push(Diagnostic::warning(
                "fbx.weights_renormalized",
                format!(
                    "mesh '{name}' has {renormalized_weights} vertices whose skin weights did not sum to 1.0; weights were renormalized"
                ),
            ));
        }
        if truncated_weights > 0 {
            diagnostics.push(Diagnostic::warning(
                "fbx.skin_weights_truncated",
                format!(
                    "mesh '{name}' has {truncated_weights} vertices bound to more than 4 joints; only the 4 highest weights were kept"
                ),
            ));
        }

        let (indices, submeshes, submesh_materials) = build_submeshes(mesh);

        meshes.push(IrMesh {
            // This format's morph targets are not wired through yet
            // (ADR 0097 §5's IR support is reusable, the parser side is not
            // part of that change).
            morph_targets: Vec::new(),
            source_index: mesh.element.typed_id as usize,
            name,
            mesh: Mesh {
                vertices,
                indices: Some(indices),
                skinning,
                tangents,
                submeshes,
            },
            submesh_materials,
        });
    }

    meshes
}

/// Builds per-corner [`SkinningVertexData`] for `mesh`, returning `None` when
/// the mesh has no skin deformer. FBX allows an unbounded number of joint
/// influences per control point; the engine's fixed 4-wide vertex format
/// (matching glTF's `WEIGHTS_0`) does not, so influences beyond the 4
/// heaviest are dropped (counted in the returned `truncated` total) and the
/// survivors renormalized (counted in `renormalized`).
fn parse_mesh_skinning(
    mesh: &ufbx::Mesh,
    cluster_to_joint: &HashMap<usize, Vec<Option<usize>>>,
) -> (Option<Vec<SkinningVertexData>>, usize, usize) {
    let Some(deformer) = mesh.skin_deformers.first() else {
        return (None, 0, 0);
    };
    let Some(cluster_map) = cluster_to_joint.get(&(deformer.element.typed_id as usize)) else {
        return (None, 0, 0);
    };

    let mut renormalized = 0usize;
    let mut truncated = 0usize;
    let mut skinning = Vec::with_capacity(mesh.num_indices);

    for i in 0..mesh.num_indices {
        let control_point = mesh.vertex_indices.get(i).copied().unwrap_or(0) as usize;
        let mut influences: Vec<(u16, f32)> = Vec::new();
        if let Some(vertex) = deformer.vertices.get(control_point) {
            for weight_offset in 0..vertex.num_weights {
                let weight_index = vertex.weight_begin as usize + weight_offset as usize;
                let Some(weight) = deformer.weights.get(weight_index) else {
                    continue;
                };
                let Some(Some(joint_index)) = cluster_map.get(weight.cluster_index as usize) else {
                    continue;
                };
                influences.push((*joint_index as u16, weight.weight as f32));
            }
        }
        if influences.len() > 4 {
            truncated += 1;
            influences.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            influences.truncate(4);
        }
        let mut joints = [0u16; 4];
        let mut weights = [0.0f32; 4];
        for (slot, (joint, weight)) in influences.iter().enumerate() {
            joints[slot] = *joint;
            weights[slot] = *weight;
        }
        weights = normalize_weights(weights, &mut renormalized);
        skinning.push(SkinningVertexData { joints, weights });
    }

    (Some(skinning), renormalized, truncated)
}

/// Normalizes one vertex's weights to sum to 1.0 (parity with
/// [`crate::gltf_import`]'s identically named helper).
fn normalize_weights(weights: [f32; 4], renormalized: &mut usize) -> [f32; 4] {
    let sum: f32 = weights.iter().sum();
    if sum <= f32::EPSILON {
        return weights;
    }
    if (sum - 1.0).abs() <= WEIGHT_SUM_TOLERANCE {
        return weights;
    }
    *renormalized += 1;
    weights.map(|weight| weight / sum)
}

/// Triangulates `mesh`'s faces into an index buffer, grouped into per-
/// material [`Submesh`] ranges (ADR 0076) using `ufbx`'s own material-part
/// grouping (`material_part_usage_order`, first-face-appearance order, so the
/// result is deterministic across imports of the same file).
///
/// A mesh with at most one material needs no explicit ranges, matching
/// [`crate::gltf_import`]'s "single-primitive mesh" convention.
fn build_submeshes(mesh: &ufbx::Mesh) -> (Vec<u32>, Vec<Submesh>, Vec<Option<usize>>) {
    let mut buffer = vec![0u32; mesh.max_face_triangles * 3];
    let mut indices = Vec::new();

    if mesh.material_parts.len() <= 1 {
        for face in &mesh.faces {
            let count = mesh.triangulate_face(&mut buffer, *face);
            indices.extend_from_slice(&buffer[..count as usize * 3]);
        }
        let material = mesh
            .materials
            .first()
            .map(|material| material.element.typed_id as usize);
        return (indices, Vec::new(), vec![material]);
    }

    let mut submeshes = Vec::with_capacity(mesh.material_parts.len());
    let mut submesh_materials = Vec::with_capacity(mesh.material_parts.len());
    for &part_index in &mesh.material_part_usage_order {
        let Some(part) = mesh.material_parts.get(part_index as usize) else {
            continue;
        };
        let start = indices.len() as u32;
        for &face_index in &part.face_indices {
            let Some(face) = mesh.faces.get(face_index as usize) else {
                continue;
            };
            let count = mesh.triangulate_face(&mut buffer, *face);
            indices.extend_from_slice(&buffer[..count as usize * 3]);
        }
        submeshes.push(Submesh {
            start,
            count: indices.len() as u32 - start,
        });
        submesh_materials.push(
            mesh.materials
                .get(part.index as usize)
                .map(|material| material.element.typed_id as usize),
        );
    }
    (indices, submeshes, submesh_materials)
}

/// Recognizes the FBX transform property names `ufbx` reports on
/// [`ufbx::AnimProp::prop_name`], mapping them to [`AnimProperty`].
fn property_from_name(name: &str) -> Option<AnimProperty> {
    match name {
        "Lcl Translation" => Some(AnimProperty::Translation),
        "Lcl Rotation" => Some(AnimProperty::Rotation),
        "Lcl Scaling" => Some(AnimProperty::Scale),
        _ => None,
    }
}

/// Parses every animation stack with at least one channel bound to a skin
/// joint as an [`IrClip`] (ADR 0081 §3).
///
/// Each clip binds to the first kept skin that contains one of its animated
/// nodes as a joint, mirroring [`crate::gltf_import`]'s "first-resolved skin"
/// rule; channels targeting nodes outside that skin are dropped with a
/// diagnostic. Every surviving channel is resampled to linear keys via
/// `ufbx`'s transform evaluation at the scene's declared frame rate (30 Hz
/// fallback), so FBX's native curve interpolation modes never reach the IR;
/// this is reported once per clip via the non-fatal
/// `anim.fbx_curve_resampled` diagnostic (ADR 0081 §3).
fn parse_clips(
    scene: &ufbx::Scene,
    skins: &[IrSkin],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrClip> {
    let frame_rate =
        if scene.settings.frames_per_second.is_finite() && scene.settings.frames_per_second > 0.0 {
            scene.settings.frames_per_second
        } else {
            DEFAULT_FRAME_RATE
        };

    let mut clips = Vec::new();

    for stack in &scene.anim_stacks {
        let name = element_name(&stack.element, "unnamed");
        let anim = &*stack.anim;

        let mut animated: Vec<(usize, AnimProperty)> = Vec::new();
        for layer in &stack.layers {
            for anim_prop in &layer.anim_props {
                let element = anim_prop.element.as_ref();
                let Some(node) = ufbx::as_node(element) else {
                    continue;
                };
                let Some(property) = property_from_name(anim_prop.prop_name.as_ref()) else {
                    continue;
                };
                let node_index = node.element.typed_id as usize;
                if !animated.contains(&(node_index, property)) {
                    animated.push((node_index, property));
                }
            }
        }

        let Some(skin_position) = animated.iter().find_map(|&(node_index, _)| {
            skins
                .iter()
                .position(|skin| skin.joint_nodes.contains(&node_index))
        }) else {
            continue;
        };
        let skin = &skins[skin_position];

        let time_begin = stack.time_begin;
        let time_end = stack.time_end.max(time_begin);

        let mut channels = Vec::new();
        let mut skipped_targets = 0usize;
        let mut resampled_curves = 0usize;
        for &(node_index, property) in &animated {
            let Some(joint_index) = skin
                .joint_nodes
                .iter()
                .position(|&joint| joint == node_index)
            else {
                skipped_targets += 1;
                continue;
            };
            let Some(node) = scene.nodes.get(node_index) else {
                continue;
            };
            let keyframes =
                resample_property(anim, node, property, time_begin, time_end, frame_rate);
            if keyframes.is_empty() {
                continue;
            }
            resampled_curves += 1;
            channels.push(IrClipChannel {
                joint_index,
                property,
                keyframes,
            });
        }

        if channels.is_empty() {
            continue;
        }

        if skipped_targets > 0 {
            diagnostics.push(Diagnostic::warning(
                "fbx.animation_target_not_joint",
                format!(
                    "animation '{name}' has {skipped_targets} channels targeting nodes outside the bound skin; channels skipped"
                ),
            ));
        }
        diagnostics.push(Diagnostic::warning(
            "anim.fbx_curve_resampled",
            format!(
                "animation '{name}' resampled {resampled_curves} channel curves to linear keys at {frame_rate:.3} Hz"
            ),
        ));

        let duration = channels
            .iter()
            .filter_map(|channel| channel.keyframes.last())
            .fold(0.0_f32, |max, keyframe| max.max(keyframe.time));

        clips.push(IrClip {
            source_index: stack.element.typed_id as usize,
            name,
            skin_index: skin.source_index,
            channels,
            duration,
        });
    }

    clips
}

/// Resamples `node`'s evaluated local `property` to linear keyframes at
/// `frame_rate` Hz over `[time_begin, time_end]`, with keyframe times
/// relative to `time_begin` (module normalization contract).
fn resample_property(
    anim: &ufbx::Anim,
    node: &ufbx::Node,
    property: AnimProperty,
    time_begin: f64,
    time_end: f64,
    frame_rate: f64,
) -> Vec<Keyframe> {
    let duration = (time_end - time_begin).max(0.0);
    let frame_count = if duration <= 0.0 {
        0
    } else {
        (duration * frame_rate).ceil() as u32
    };

    let mut keyframes = Vec::with_capacity(frame_count as usize + 1);
    for step in 0..=frame_count {
        let time = (time_begin + f64::from(step) / frame_rate).min(time_end);
        let transform = ufbx::evaluate_transform(anim, node, time);
        let value = match property {
            AnimProperty::Translation => {
                let t = transform.translation;
                [t.x as f32, t.y as f32, t.z as f32, 1.0]
            }
            AnimProperty::Rotation => {
                let r = transform.rotation;
                [r.x as f32, r.y as f32, r.z as f32, r.w as f32]
            }
            AnimProperty::Scale => {
                let s = transform.scale;
                [s.x as f32, s.y as f32, s.z as f32, 1.0]
            }
        };
        keyframes.push(Keyframe {
            time: (time - time_begin) as f32,
            value,
        });
    }
    keyframes
}

/// Parses every texture element as an [`IrTexture`]; the returned `Vec<bool>`
/// marks which *original* texture selectors (`typed_id`) survived, for
/// [`parse_materials`] to null out dangling references.
///
/// Embedded media decodes directly. A file-referenced texture is read from
/// disk relative to `source_dir` when one is available (path-based parse);
/// with no directory context (byte-slice parse) it is dropped with a
/// diagnostic, matching [`crate::gltf_import::parse_textures`]'s "only
/// embedded data is resolved from bytes" rule.
fn parse_textures(
    scene: &ufbx::Scene,
    source_dir: Option<&Path>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<IrTexture>, Vec<bool>) {
    let mut textures = Vec::new();
    let mut valid = vec![false; scene.textures.len()];

    for texture in &scene.textures {
        let index = texture.element.typed_id as usize;
        let name = element_name(&texture.element, "unnamed");

        let bytes: Option<Vec<u8>> = if !texture.content.is_empty() {
            Some(texture.content.to_vec())
        } else if texture.has_file {
            match source_dir {
                Some(dir) => {
                    let relative = texture.relative_filename.as_ref();
                    let path = if relative.is_empty() {
                        PathBuf::from(texture.absolute_filename.as_ref())
                    } else {
                        dir.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
                    };
                    match std::fs::read(&path) {
                        Ok(bytes) => Some(bytes),
                        Err(_) => {
                            diagnostics.push(Diagnostic::warning(
                                "fbx.texture_file_missing",
                                format!(
                                    "texture '{name}' file '{}' could not be read; texture skipped",
                                    path.display()
                                ),
                            ));
                            None
                        }
                    }
                }
                None => {
                    diagnostics.push(Diagnostic::warning(
                        "fbx.texture_not_embedded",
                        format!(
                            "texture '{name}' is not embedded; a byte-slice import cannot resolve its external file"
                        ),
                    ));
                    None
                }
            }
        } else {
            None
        };

        let Some(bytes) = bytes else { continue };
        let Some((width, height, rgba8)) = decode_rgba8(&bytes) else {
            diagnostics.push(Diagnostic::error(
                "fbx.texture_format_unsupported",
                format!("texture '{name}' could not be decoded"),
            ));
            continue;
        };
        if width > crate::render_limits::MAX_TEXTURE_DIMENSION
            || height > crate::render_limits::MAX_TEXTURE_DIMENSION
        {
            diagnostics.push(Diagnostic::error(
                "renderer.texture_dimension_limit",
                format!(
                    "FBX texture '{name}' is {width}x{height}, exceeding the {}px renderer limit",
                    crate::render_limits::MAX_TEXTURE_DIMENSION
                ),
            ));
        }
        valid[index] = true;
        textures.push(IrTexture {
            source_index: index,
            name,
            width,
            height,
            rgba8,
        });
    }

    (textures, valid)
}

fn decode_rgba8(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Parses every material to the engine's material contract as an
/// [`IrMaterial`] via `ufbx`'s PBR material view (ADR 0081 §3);
/// `texture_valid` nulls out any reference to a texture [`parse_textures`]
/// dropped.
///
/// FBX has no native alpha-mode / cutoff or double-sided concept exposed
/// through `ufbx`'s PBR view, so materials always import as opaque,
/// back-face-culled, and lit — the same defaults
/// [`crate::gltf_import::parse_materials`] falls back to when a glTF material
/// leaves those fields unspecified.
fn parse_materials(scene: &ufbx::Scene, texture_valid: &[bool]) -> Vec<IrMaterial> {
    scene
        .materials
        .iter()
        .map(|material| {
            let texture_ref = |texture: Option<&ufbx::Ref<ufbx::Texture>>| {
                texture.and_then(|texture| {
                    let index = texture.element.typed_id as usize;
                    texture_valid
                        .get(index)
                        .copied()
                        .unwrap_or(false)
                        .then_some(index)
                })
            };
            let scalar = |map: &ufbx::MaterialMap, default: f32| {
                if map.has_value {
                    map.value_vec4.x as f32
                } else {
                    default
                }
            };
            let base_color = if material.pbr.base_color.has_value {
                let v = material.pbr.base_color.value_vec4;
                LinearRgba {
                    r: v.x as f32,
                    g: v.y as f32,
                    b: v.z as f32,
                    a: 1.0,
                }
            } else {
                LinearRgba::WHITE
            };
            let emissive_color = if material.pbr.emission_color.has_value {
                let v = material.pbr.emission_color.value_vec4;
                LinearRgba {
                    r: v.x as f32,
                    g: v.y as f32,
                    b: v.z as f32,
                    a: 1.0,
                }
            } else {
                LinearRgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }
            };
            IrMaterial {
                source_index: material.element.typed_id as usize,
                name: element_name(&material.element, "unnamed"),
                base_color,
                base_color_texture: texture_ref(material.pbr.base_color.texture.as_ref()),
                normal_texture: texture_ref(material.pbr.normal_map.texture.as_ref()),
                emissive_texture: texture_ref(material.pbr.emission_color.texture.as_ref()),
                emissive_color,
                roughness: scalar(&material.pbr.roughness, 0.5),
                metallic: scalar(&material.pbr.metalness, 0.0),
                alpha_mode: MaterialAlphaMode::Opaque,
                alpha_cutoff: 0.5,
                cull_mode: MaterialCullMode::Back,
                shading_model: MaterialShadingModel::StandardLit,
                toon_ramp_texture: None,
                toon_shadow_color: LinearRgba { r: 0.55, g: 0.55, b: 0.62, a: 1.0 },
                toon_ambient_color: LinearRgba { r: 0.2, g: 0.2, b: 0.2, a: 1.0 },
                toon_specular_color: LinearRgba::WHITE,
                toon_specular_power: 16.0,
                sphere_texture: None,
                sphere_blend: MaterialSphereBlendMode::Multiply,
                sphere_coordinates: MaterialSphereCoordinateSource::ViewNormal,
                outline: MaterialOutline::default(),
                cast_shadow: true,
                receive_shadow: true,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::asset::{imported_sub_asset_id, ImportedSubAssetKind};

    #[test]
    fn import_invalid_bytes_returns_error() {
        let id = AssetId::generate();
        assert!(
            import_fbx_bytes(&id, b"not a valid fbx document", &[]).is_err(),
            "garbage bytes must produce a parse error"
        );
    }

    #[test]
    fn sub_asset_ids_are_deterministic_across_repeated_imports() {
        let id = AssetId::generate();
        let first = import_fbx_bytes(&id, SKINNED_FBX.as_bytes(), &[])
            .expect("fixture must parse")
            .imported_sub_assets();
        let second = import_fbx_bytes(&id, SKINNED_FBX.as_bytes(), &[])
            .expect("fixture must parse")
            .imported_sub_assets();
        assert_eq!(
            first, second,
            "re-importing identical bytes must produce identical sub-asset IDs"
        );
    }

    #[test]
    fn parse_fbx_extracts_normalized_node_transforms() {
        let document = parse_fbx(SKINNED_FBX.as_bytes()).expect("fixture must parse");
        // ufbx always synthesizes an implicit scene root and parents every
        // top-level FBX model under it (the fixture's `root_joint` model
        // declares no explicit parent connection), so `root_joint`'s parent
        // is that synthetic root rather than `None`.
        let (root_index, root) = document
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.name == "root_joint")
            .expect("root_joint must be present");
        let scene_root = root
            .parent
            .expect("root_joint must hang off the implicit scene root");
        assert_eq!(
            document.nodes[scene_root].parent, None,
            "the implicit scene root must have no parent of its own"
        );
        let tip = document
            .nodes
            .iter()
            .find(|node| node.name == "tip_joint")
            .expect("tip_joint must be present");
        assert_eq!(
            tip.parent,
            Some(root_index),
            "tip_joint must be parented directly to root_joint"
        );
        assert!(
            (tip.translation - Vec3::new(0.0, 1.0, 0.0)).length() < 1.0e-3,
            "tip_joint must sit one meter above its parent with no pivot residue, got {:?}",
            tip.translation
        );
        assert!(
            (tip.rotation.to_array()[3] - 1.0).abs() < 1.0e-3,
            "tip_joint must rest at identity rotation"
        );
    }

    #[test]
    fn parse_fbx_extracts_skin_joints_and_inverse_binds() {
        let document = parse_fbx(SKINNED_FBX.as_bytes()).expect("fixture must parse");
        assert_eq!(
            document.skins.len(),
            1,
            "fixture declares one skin deformer"
        );
        let skin = &document.skins[0];
        assert_eq!(skin.joint_nodes.len(), 2);
        assert_eq!(skin.inverse_bind_matrices.len(), 2);
    }

    #[test]
    fn parse_fbx_flips_uvs_to_the_engine_top_left_origin() {
        // FBX authors UVs from the bottom left; the engine follows glTF's
        // top-left origin. Without the flip a character's face samples the
        // vertically mirrored part of its atlas and lands on its back.
        // The fixture authors (0,0), (1,0), (0,1) per polygon vertex.
        let document = parse_fbx(SKINNED_FBX.as_bytes()).expect("fixture must parse");
        let mesh = &document.meshes[0].mesh;
        let uvs: Vec<[f32; 2]> = mesh.vertices.iter().map(|vertex| vertex.uv).collect();
        assert_eq!(
            uvs,
            vec![[0.0, 1.0], [1.0, 1.0], [0.0, 0.0]],
            "each authored v must be mirrored to 1.0 - v"
        );
    }

    #[test]
    fn parse_fbx_extracts_a_joint_bound_linear_sampled_clip() {
        let document = parse_fbx(SKINNED_FBX.as_bytes()).expect("fixture must parse");
        assert_eq!(document.clips.len(), 1, "fixture declares one anim stack");
        let clip = &document.clips[0];
        assert!(!clip.channels.is_empty());
        assert!(clip.duration > 0.0);
        for channel in &clip.channels {
            assert!(
                channel.keyframes.len() >= 2,
                "a resampled channel must carry at least a start and end key"
            );
            let times: Vec<f32> = channel.keyframes.iter().map(|key| key.time).collect();
            assert!(
                times.windows(2).all(|pair| pair[0] <= pair[1]),
                "resampled keyframes must be sorted by time ascending"
            );
        }
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.fbx_curve_resampled"));
    }

    #[test]
    fn import_fbx_bytes_produces_the_same_catalog_as_parse_then_build() {
        let source = AssetId::generate();
        let document = parse_fbx(SKINNED_FBX.as_bytes()).expect("fixture must parse");
        let direct = crate::model_import::build_import_result(&document, &source, &[]);
        let via_entry_point =
            import_fbx_bytes(&source, SKINNED_FBX.as_bytes(), &[]).expect("entry point import");
        assert_eq!(
            direct.imported_sub_assets(),
            via_entry_point.imported_sub_assets()
        );
        assert_eq!(direct.meshes.len(), 1);
        assert_eq!(
            direct.meshes[0].id,
            // The fixture declares exactly one `Geometry` element, so its
            // `typed_id` (the source selector, ADR 0081 §5) is 0.
            imported_sub_asset_id(&source, ImportedSubAssetKind::Mesh, 0)
        );
    }

    /// A minimal ASCII FBX 7.4 document: a two-bone skeleton (`root_joint` ->
    /// `tip_joint`), one triangle mesh skinned entirely to `tip_joint`, and
    /// one animation stack rotating `tip_joint` over one second. Mirrors
    /// `gltf_import::tests::SKINNED_GLTF`'s role as the shared fixture for
    /// parser acceptance tests, written by hand against the FBX 7.4 ASCII
    /// grammar (ufbx parses ASCII FBX directly, so no binary encoding step is
    /// needed).
    ///
    /// Regenerate by hand-editing this constant directly; there is no
    /// external tool dependency to keep in sync.
    pub(crate) const SKINNED_FBX: &str = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
	FBXHeaderVersion: 1003
	FBXVersion: 7400
	Creator: "GameEngine fbx_import test fixture"
}
GlobalSettings:  {
	Version: 1000
	Properties70:  {
		P: "UpAxis", "int", "Integer", "",1
		P: "UpAxisSign", "int", "Integer", "",1
		P: "FrontAxis", "int", "Integer", "",2
		P: "FrontAxisSign", "int", "Integer", "",1
		P: "CoordAxis", "int", "Integer", "",0
		P: "CoordAxisSign", "int", "Integer", "",1
		P: "UnitScaleFactor", "double", "Number", "",1
	}
}
Objects:  {
	Model: 1000, "Model::root_joint", "LimbNode" {
		Version: 232
		Properties70:  {
			P: "Lcl Translation", "Lcl Translation", "", "A",0,0,0
			P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,0
			P: "Lcl Scaling", "Lcl Scaling", "", "A",1,1,1
		}
		Shading: T
		Culling: "CullingOff"
	}
	Model: 1001, "Model::tip_joint", "LimbNode" {
		Version: 232
		Properties70:  {
			P: "Lcl Translation", "Lcl Translation", "", "A",0,1,0
			P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,0
			P: "Lcl Scaling", "Lcl Scaling", "", "A",1,1,1
		}
		Shading: T
		Culling: "CullingOff"
	}
	Model: 1002, "Model::character", "Mesh" {
		Version: 232
		Properties70:  {
			P: "Lcl Translation", "Lcl Translation", "", "A",0,0,0
			P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,0
			P: "Lcl Scaling", "Lcl Scaling", "", "A",1,1,1
		}
		Shading: T
		Culling: "CullingOff"
	}
	Geometry: 2000, "Geometry::triangle", "Mesh" {
		Vertices: *9 {
			a: 0,0,0,1,0,0,0,1,0
		}
		PolygonVertexIndex: *3 {
			a: 0,1,-3
		}
		GeometryVersion: 124
		LayerElementNormal: 0 {
			Version: 101
			Name: ""
			MappingInformationType: "ByPolygonVertex"
			ReferenceInformationType: "Direct"
			Normals: *9 {
				a: 0,1,0,0,1,0,0,1,0
			}
		}
		LayerElementUV: 0 {
			Version: 101
			Name: ""
			MappingInformationType: "ByPolygonVertex"
			ReferenceInformationType: "IndexToDirect"
			UV: *6 {
				a: 0,0,1,0,0,1
			}
			UVIndex: *3 {
				a: 0,1,2
			}
		}
		Layer: 0 {
			Version: 100
			LayerElement:  {
				Type: "LayerElementNormal"
				TypedIndex: 0
			}
			LayerElement:  {
				Type: "LayerElementUV"
				TypedIndex: 0
			}
		}
	}
	Deformer: 3000, "Deformer::skin", "Skin" {
		Version: 101
		Link_DeformAcuracy: 50
	}
	Deformer: 3001, "SubDeformer::root", "Cluster" {
		Version: 100
		Indexes: *0 {
			a:
		}
		Weights: *0 {
			a:
		}
		Transform: *16 {
			a: 1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1
		}
		TransformLink: *16 {
			a: 1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1
		}
	}
	Deformer: 3002, "SubDeformer::tip", "Cluster" {
		Version: 100
		Indexes: *3 {
			a: 0,1,2
		}
		Weights: *3 {
			a: 1,1,1
		}
		Transform: *16 {
			a: 1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1
		}
		TransformLink: *16 {
			a: 1,0,0,0,0,1,0,0,0,0,1,0,-1,0,0,1
		}
	}
	AnimationStack: 4000, "AnimStack::spin", "" {
		Properties70:  {
			P: "LocalStart", "KTime", "Time", "",0
			P: "LocalStop", "KTime", "Time", "",46186158000
		}
	}
	AnimationLayer: 4001, "AnimLayer::BaseLayer", "" {
	}
	AnimationCurveNode: 4002, "AnimCurveNode::R", "" {
		Properties70:  {
			P: "d|X", "Number", "", "A",0
			P: "d|Y", "Number", "", "A",90
			P: "d|Z", "Number", "", "A",0
		}
	}
	AnimationCurve: 4003, "AnimCurve::", "" {
		Default: 0
		KeyVer: 4008
		KeyTime: *2 {
			a: 0,46186158000
		}
		KeyValueFloat: *2 {
			a: 0,90
		}
		KeyAttrFlags: *1 {
			a: 24856
		}
		KeyAttrDataFloat: *4 {
			a: 0,0,0,0
		}
		KeyAttrRefCount: *1 {
			a: 2
		}
	}
}
Connections:  {
	C: "OO",1001,1000
	C: "OO",2000,1002
	C: "OO",3000,1002
	C: "OO",3001,3000
	C: "OO",3002,3000
	C: "OO",1000,3001
	C: "OO",1001,3002
	C: "OO",4001,4000
	C: "OO",4002,4001
	C: "OP",4003,4002,"d|Y"
	C: "OP",4002,1001,"Lcl Rotation"
}
"#;
}
