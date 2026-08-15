//! glTF / GLB parser (Phase 36 / 48, ADR 0032 / 0043, split out by ADR 0078).
//!
//! Parses `.gltf` and `.glb` byte slices into a format-independent
//! `ModelDecument`: node hierarchy, meshes, skins, clips,
//! materials, and textures, already normalized to engine conventions (see
//! `crate::model_ir` for the exact contract). This module owns every
//! `gltf` crate dependency — accessor reads, buffer/URI resolution, image
//! decoding — and assigns no sub-asset IDs and no skeleton identity; that is
//! `crate::model_import`'s job, so a future FBX/USD parser can reuse it
//! without reimplementing anything in this file.
//!
//! Byte-slice parsing resolves embedded data, while path-based parsing also
//! resolves external buffer and image sidecars relative to the source
//! document. `import_gltf_bytes` and `import_gltf_path` are the public
//! entry points most callers want: they parse and build in one step and
//! return the same `GltfImportResult` shape this module has always
//! produced.

use crate::asset::SkeletonRecord;
use crate::model_import::GltfImportResult;
use crate::model_ir::{
    IrClip, IrClipChannel, IrMaterial, IrMesh, IrNode, IrSkin, IrTexture, ModelDocument, IrAnimProperty as AnimProperty, IrKeyframe as Keyframe, IrMeshData as Mesh, IrSkinningVertexData as SkinningVertexData, IrSubmesh, IrVertex as Vertex,
};
use crate::skinning::MAX_JOINTS;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use engine_authoring::{
    LinearRgba, MaterialAlphaMode, MaterialCullMode, MaterialOutline, MaterialShadingModel,
    MaterialSphereBlendMode, MaterialSphereCoordinateSource,
};
use glam::{Mat4, Quat, Vec3};
use std::fmt;
use std::path::{Path, PathBuf};

/// Tolerated deviation of a vertex weight sum from 1.0 before the importer
/// reports a renormalization warning (ADR 0043).
const WEIGHT_SUM_TOLERANCE: f32 = 1.0e-3;

/// Reports a fatal error that prevents parsing from completing.
#[derive(Debug)]
pub enum GltfImportError {
    /// glTF / GLB parse or buffer/image resolution failed.
    Parse(gltf::Error),
}

impl fmt::Display for GltfImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "glTF parse error: {e}"),
        }
    }
}

impl std::error::Error for GltfImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Import entry points
// ---------------------------------------------------------------------------

/// Imports static meshes from a glTF or GLB byte slice.
///
/// Sub-asset IDs are deterministic: re-importing the same `bytes` with the
/// same `source_id` produces identical sub-asset IDs. Internally this parses
/// `bytes` into a `ModelDocument` ([`parse_gltf`]) and hands it to
/// [`crate::model_import::build_import_result`]; see that function for the
/// asset-building contract.
///
/// Only embedded buffers (data URIs in `.gltf` or GLB binary chunks) are
/// resolved.  Missing normals or UV coordinates produce non-fatal
/// [`Diagnostic`] warnings and fall back to sensible defaults.
///
/// `existing_skeletons` is the bone-catalog ledger the caller already has on
/// file — typically a source's own previous `ImportSettings::skeleton_records`
/// plus any other project sources the caller wants considered for
/// cross-source dedupe (ADR 0077 §4). Pass an empty slice for a first import
/// or when no ledger is available.
///
/// # Errors
///
/// Returns [`GltfImportError::Parse`] when `bytes` is not a valid glTF / GLB
/// document or when embedded buffer resolution fails.
pub fn import_gltf_bytes(
    source_id: &AssetId,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, GltfImportError> {
    let document = parse_gltf(bytes)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Imports a glTF/GLB source from disk, resolving external buffers and images
/// relative to the source file as required by the glTF specification.
///
/// See [`import_gltf_bytes`] for the `existing_skeletons` contract.
///
/// # Errors
///
/// Returns [`GltfImportError::Parse`] when the document, one of its sidecar
/// dependencies, or an embedded payload cannot be loaded.
pub fn import_gltf_path(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, GltfImportError> {
    let document = parse_gltf_path(path)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Same as [`import_gltf_path`], but overrides the ground-contact candidate
/// -bone name heuristic (ADR 0080 §1) with `contact_bone_names` when it is
/// non-empty, matching `crate::asset::ImportSettings::contact_bones`'s
/// contract (AP-5, editor observability).
///
/// # Errors
///
/// Returns [`GltfImportError::Parse`] when the document, one of its sidecar
/// dependencies, or an embedded payload cannot be loaded.
pub fn import_gltf_path_with_contact_bones(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, GltfImportError> {
    let document = parse_gltf_path(path)?;
    Ok(crate::model_import::build_import_result_with_contact_bones(
        &document,
        source_id,
        existing_skeletons,
        contact_bone_names,
    ))
}

/// Parses a glTF / GLB byte slice into a format-independent `ModelDocument`
/// (ADR 0078), assigning no sub-asset IDs and no skeleton identity.
///
/// # Errors
///
/// Returns [`GltfImportError::Parse`] when `bytes` is not a valid glTF / GLB
/// document or when embedded buffer resolution fails.
pub fn parse_gltf(bytes: &[u8]) -> Result<ModelDocument, GltfImportError> {
    let (document, buffers, images) = gltf::import_slice(bytes).map_err(GltfImportError::Parse)?;
    Ok(build_model_document(&document, &buffers, &images))
}

/// Parses a glTF / GLB source from disk into a format-independent
/// `ModelDocument` (ADR 0078), resolving external buffers and images
/// relative to the source file.
///
/// # Errors
///
/// Returns [`GltfImportError::Parse`] when the document, one of its sidecar
/// dependencies, or an embedded payload cannot be loaded.
pub fn parse_gltf_path(path: &Path) -> Result<ModelDocument, GltfImportError> {
    let (document, buffers, images) = gltf::import(path).map_err(GltfImportError::Parse)?;
    Ok(build_model_document(&document, &buffers, &images))
}

/// Returns external buffer/image sidecars declared by a glTF document.
/// Embedded data URIs and GLB buffer views are intentionally omitted.
pub fn gltf_source_dependencies(path: &Path) -> Result<Vec<PathBuf>, GltfImportError> {
    let gltf = gltf::Gltf::open(path).map_err(GltfImportError::Parse)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut dependencies = Vec::new();
    for buffer in gltf.buffers() {
        if let gltf::buffer::Source::Uri(uri) = buffer.source() {
            push_external_dependency(parent, uri, &mut dependencies);
        }
    }
    for image in gltf.images() {
        if let gltf::image::Source::Uri { uri, .. } = image.source() {
            push_external_dependency(parent, uri, &mut dependencies);
        }
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

fn push_external_dependency(parent: &Path, uri: &str, dependencies: &mut Vec<PathBuf>) {
    if !uri.starts_with("data:") && !uri.contains("://") {
        let decoded = percent_decode_uri(uri).unwrap_or_else(|| uri.to_owned());
        dependencies.push(parent.join(decoded.replace('/', std::path::MAIN_SEPARATOR_STR)));
    }
}

fn percent_decode_uri(uri: &str) -> Option<String> {
    let bytes = uri.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Computes a deterministic content fingerprint over a source and sidecars.
/// Absolute paths are excluded so moving a project does not force reimport.
pub fn fingerprint_gltf_source(
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
// Parser core
// ---------------------------------------------------------------------------

/// Parses a loaded glTF document into a `ModelDocument` (ADR 0078).
///
/// Nodes, skins (structurally valid ones), and clips (ones with at least one
/// channel bound to a skin joint) are parsed here in full, including every
/// diagnostic the importer has always reported; only sub-asset ID
/// derivation and skeleton identity/dedupe/rebind are deferred to
/// [`crate::model_import`].
fn build_model_document(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    images: &[gltf::image::Data],
) -> ModelDocument {
    let mut diagnostics = Vec::new();
    diagnose_document_features(document, &mut diagnostics);

    let nodes = parse_nodes(document);
    let skins = parse_skins(document, buffers, nodes.len(), &mut diagnostics);
    let meshes = parse_meshes(document, buffers, &mut diagnostics);
    let clips = parse_clips(document, buffers, &skins, &mut diagnostics);
    let (textures, texture_valid) = parse_textures(document, images, &mut diagnostics);
    let materials = parse_materials(document, &texture_valid, &mut diagnostics);

    ModelDocument {
        nodes,
        meshes,
        skins,
        // A glTF skin's joint list is its skeleton; each skin keeps its own
        // (ADR 0097 §4a's default, unchanged glTF behavior).
        skeleton_scope: crate::model_ir::SkeletonScope::PerSkin,
        clips,
        materials,
        textures,
        // glTF has no secondary-motion physics concept (ADR 0097 §6).
        rigid_body_rig: None,
        diagnostics,
    }
}

fn diagnose_document_features(document: &gltf::Document, diagnostics: &mut Vec<Diagnostic>) {
    for extension in document.extensions_used() {
        let (code, detail) = match extension {
            "KHR_draco_mesh_compression" | "EXT_meshopt_compression" => (
                "gltf.compression_unsupported",
                "compressed primitives are not decoded by this importer",
            ),
            "KHR_mesh_quantization" => (
                "gltf.quantization_unsupported",
                "quantized attributes are not guaranteed to import correctly",
            ),
            _ => (
                "gltf.extension_unsupported",
                "the extension is not interpreted and its optional semantics are ignored",
            ),
        };
        diagnostics.push(Diagnostic::warning(
            code,
            format!("glTF extension `{extension}` is unsupported: {detail}"),
        ));
    }
}

/// Parses every document node as an [`IrNode`], preserving glTF node indices.
///
/// [`IrNode::mesh`] and [`IrNode::skin`] always carry the *original* glTF
/// selector (see [`crate::model_ir`]'s cross-reference contract), even
/// though the referenced mesh or skin may be absent from
/// [`ModelDocument::meshes`] / [`ModelDocument::skins`] when it was dropped.
fn parse_nodes(document: &gltf::Document) -> Vec<IrNode> {
    let mut nodes: Vec<IrNode> = document
        .nodes()
        .map(|node| {
            let (translation, rotation, scale) = node.transform().decomposed();
            IrNode {
                name: node.name().unwrap_or("node").to_owned(),
                parent: None,
                translation: Vec3::from(translation),
                rotation: Quat::from_array(rotation),
                scale: Vec3::from(scale),
                mesh: node.mesh().map(|mesh| mesh.index()),
                skin: node.skin().map(|skin| skin.index()),
            }
        })
        .collect();
    for node in document.nodes() {
        for child in node.children() {
            nodes[child.index()].parent = Some(node.index());
        }
    }
    nodes
}

/// Parses every structurally valid skin as an [`IrSkin`], dropping skins that
/// exceed [`MAX_JOINTS`] or reference a node outside the document, with
/// diagnostics (unchanged behavior from the pre-ADR-0078 importer).
///
/// Skeleton construction (bone list, identity, dedupe/rebind) is a
/// [`crate::model_import`] concern; this function only extracts the raw
/// joint node list and inverse bind matrices.
fn parse_skins(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    node_count: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrSkin> {
    let mut skins = Vec::new();

    for (skin_idx, skin) in document.skins().enumerate() {
        let name = skin.name().unwrap_or("unnamed").to_owned();
        let joint_nodes: Vec<usize> = skin.joints().map(|joint| joint.index()).collect();

        if joint_nodes.len() > MAX_JOINTS {
            diagnostics.push(Diagnostic::error(
                "gltf.skin_too_many_joints",
                format!(
                    "skin '{name}' has {} joints, exceeding the supported maximum of {MAX_JOINTS}; skin dropped",
                    joint_nodes.len()
                ),
            ));
            continue;
        }
        if joint_nodes.iter().any(|&joint| joint >= node_count) {
            diagnostics.push(Diagnostic::error(
                "gltf.skin_invalid_joint",
                format!("skin '{name}' references a node outside the document; skin dropped"),
            ));
            continue;
        }

        let reader = skin.reader(|buf| buffers.get(buf.index()).map(|d| d.as_ref()));
        let mut inverse_bind_matrices: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
            Some(iter) => iter.map(|m| Mat4::from_cols_array_2d(&m)).collect(),
            None => {
                diagnostics.push(Diagnostic::warning(
                    "gltf.skin_missing_inverse_bind",
                    format!(
                        "skin '{name}' has no inverseBindMatrices accessor; identity matrices are used"
                    ),
                ));
                vec![Mat4::IDENTITY; joint_nodes.len()]
            }
        };
        if inverse_bind_matrices.len() != joint_nodes.len() {
            diagnostics.push(Diagnostic::warning(
                "gltf.skin_inverse_bind_count",
                format!(
                    "skin '{name}' has {} inverse bind matrices for {} joints; the list was resized with identity padding",
                    inverse_bind_matrices.len(),
                    joint_nodes.len()
                ),
            ));
            inverse_bind_matrices.resize(joint_nodes.len(), Mat4::IDENTITY);
        }

        skins.push(IrSkin {
            source_index: skin_idx,
            name,
            joint_nodes,
            inverse_bind_matrices,
        });
    }

    skins
}

/// Parses every mesh with at least one decodable primitive as an [`IrMesh`]
/// (ADR 0076 submesh ranges included), dropping meshes whose primitives all
/// failed to decode. `submesh_materials` entries are the *original* material
/// selector, since materials are never dropped by this parser.
fn parse_meshes(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrMesh> {
    let mut meshes = Vec::new();

    for (mesh_idx, gltf_mesh) in document.meshes().enumerate() {
        let name = gltf_mesh.name().unwrap_or("unnamed").to_owned();

        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut skinning: Vec<SkinningVertexData> = Vec::new();
        let mut has_skinning = false;
        let mut tangents: Vec<[f32; 4]> = Vec::new();
        let mut has_tangents = false;
        let mut renormalized_weights = 0usize;
        let mut submeshes: Vec<IrSubmesh> = Vec::new();
        let mut submesh_materials: Vec<Option<usize>> = Vec::new();

        for prim in gltf_mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                diagnostics.push(Diagnostic::warning(
                    "gltf.primitive_mode_unsupported",
                    format!(
                        "mesh '{name}' primitive {} uses {:?}; only triangle lists are imported",
                        prim.index(),
                        prim.mode()
                    ),
                ));
                continue;
            }
            if prim.morph_targets().len() > 0 {
                diagnostics.push(Diagnostic::warning(
                    "gltf.morph_targets_unsupported",
                    format!(
                        "mesh '{name}' primitive {} contains morph targets; they were ignored",
                        prim.index()
                    ),
                ));
            }
            let reader = prim.reader(|buf| buffers.get(buf.index()).map(|d| d.as_ref()));

            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(iter) => iter.collect(),
                None => {
                    diagnostics.push(Diagnostic::warning(
                        "gltf.missing_positions",
                        format!(
                            "mesh '{name}' primitive {} has no POSITION data; primitive skipped",
                            prim.index()
                        ),
                    ));
                    continue;
                }
            };

            if positions.is_empty() {
                continue;
            }

            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(values) => values.collect(),
                None => {
                    diagnostics.push(Diagnostic::warning(
                        "gltf.missing_normals",
                        format!(
                            "mesh '{name}' primitive {} has no NORMAL data; upward normals were substituted",
                            prim.index()
                        ),
                    ));
                    vec![[0.0, 1.0, 0.0]; positions.len()]
                }
            };

            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(values) => values.into_f32().collect(),
                None => {
                    diagnostics.push(Diagnostic::warning(
                        "gltf.missing_uv0",
                        format!(
                            "mesh '{name}' primitive {} has no TEXCOORD_0 data; zero UVs were substituted",
                            prim.index()
                        ),
                    ));
                    vec![[0.0, 0.0]; positions.len()]
                }
            };

            match reader.read_tangents() {
                Some(values) => {
                    has_tangents = true;
                    let values: Vec<[f32; 4]> = values.collect();
                    tangents.extend(
                        (0..positions.len()).map(|index| {
                            values.get(index).copied().unwrap_or([1.0, 0.0, 0.0, 1.0])
                        }),
                    );
                }
                None => tangents.extend(std::iter::repeat_n([1.0, 0.0, 0.0, 1.0], positions.len())),
            }

            let joints: Option<Vec<[u16; 4]>> =
                reader.read_joints(0).map(|it| it.into_u16().collect());
            let weights: Option<Vec<[f32; 4]>> =
                reader.read_weights(0).map(|it| it.into_f32().collect());

            let base = vertices.len() as u32;
            for (i, &position) in positions.iter().enumerate() {
                vertices.push(Vertex {
                    position,
                    normal: normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                    color: [1.0, 1.0, 1.0],
                    uv: uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                });
            }

            // Keep the skinning list parallel to `vertices` even when only
            // some primitives carry JOINTS_0/WEIGHTS_0; zero weights keep
            // the bind pose in the shader.
            match (&joints, &weights) {
                (Some(joints), Some(weights)) => {
                    has_skinning = true;
                    for i in 0..positions.len() {
                        let raw_weights = weights.get(i).copied().unwrap_or([0.0; 4]);
                        let entry = SkinningVertexData {
                            joints: joints.get(i).copied().unwrap_or([0; 4]),
                            weights: normalize_weights(raw_weights, &mut renormalized_weights),
                        };
                        skinning.push(entry);
                    }
                }
                _ => {
                    skinning.extend(std::iter::repeat_n(
                        SkinningVertexData {
                            joints: [0; 4],
                            weights: [0.0; 4],
                        },
                        positions.len(),
                    ));
                }
            }

            let submesh_start = indices.len() as u32;
            match reader.read_indices() {
                Some(idx_iter) => indices.extend(idx_iter.into_u32().map(|index| base + index)),
                None => indices.extend(base..base + positions.len() as u32),
            }
            submeshes.push(IrSubmesh {
                start: submesh_start,
                count: indices.len() as u32 - submesh_start,
            });
            submesh_materials.push(prim.material().index());
        }

        if vertices.is_empty() {
            continue;
        }

        if renormalized_weights > 0 {
            diagnostics.push(Diagnostic::warning(
                "gltf.weights_renormalized",
                format!(
                    "mesh '{name}' has {renormalized_weights} vertices whose skin weights did not sum to 1.0; weights were renormalized"
                ),
            ));
        }

        meshes.push(IrMesh {
            // This format's morph targets are not wired through yet
            // (ADR 0097 §5's IR support is reusable, the parser side is not
            // part of that change).
            morph_targets: Vec::new(),
            source_index: mesh_idx,
            name,
            mesh: Mesh {
                vertices,
                indices: Some(indices),
                skinning: has_skinning.then_some(skinning),
                tangents: has_tangents.then_some(tangents),
                // A single-primitive mesh needs no ranges; leaving them empty
                // keeps its draw path identical to every other simple mesh.
                submeshes: if submeshes.len() > 1 {
                    submeshes
                } else {
                    Vec::new()
                },
            },
            submesh_materials,
        });
    }

    meshes
}

/// Normalizes one vertex's weights to sum to 1.0.
///
/// Increments `renormalized` when the source sum deviates beyond
/// [`WEIGHT_SUM_TOLERANCE`]. All-zero weights are returned unchanged; the
/// skinned shader keeps such vertices in bind pose.
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

/// Parses every animation with at least one channel bound to a skin joint as
/// an [`IrClip`] (ADR 0043).
///
/// Each clip binds to the first kept skin that contains one of its target
/// nodes as a joint, exactly like the pre-ADR-0078 importer; channels
/// targeting nodes outside that skin and morph weight channels are skipped
/// with diagnostics, and `STEP` / `CUBICSPLINE` samplers are downgraded to
/// linear interpolation. Resolving a channel's joint index to a stable
/// [`crate::skeleton_asset::BoneId`] requires skeleton identity, so that
/// step is left to [`crate::model_import`].
fn parse_clips(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    skins: &[IrSkin],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrClip> {
    use gltf::animation::util::ReadOutputs;
    use gltf::animation::Interpolation;

    let mut clips = Vec::new();

    for (anim_idx, animation) in document.animations().enumerate() {
        let name = animation.name().unwrap_or("unnamed").to_owned();
        let mut channels: Vec<IrClipChannel> = Vec::new();
        let mut skin_position: Option<usize> = None;
        let mut duration = 0.0_f32;
        let mut skipped_targets = 0_usize;
        let mut morph_channels = 0_usize;
        let mut downgraded_samplers = 0_usize;

        for channel in animation.channels() {
            let node_index = channel.target().node().index();
            if skin_position.is_none() {
                skin_position = skins
                    .iter()
                    .position(|skin| skin.joint_nodes.contains(&node_index));
            }
            let Some(skin_pos) = skin_position else {
                skipped_targets += 1;
                continue;
            };
            let Some(joint_index) = skins[skin_pos]
                .joint_nodes
                .iter()
                .position(|&joint_node| joint_node == node_index)
            else {
                skipped_targets += 1;
                continue;
            };

            let interpolation = channel.sampler().interpolation();
            let cubic = matches!(interpolation, Interpolation::CubicSpline);
            if !matches!(interpolation, Interpolation::Linear) {
                downgraded_samplers += 1;
            }

            let reader = channel.reader(|buf| buffers.get(buf.index()).map(|d| d.as_ref()));
            let Some(inputs) = reader.read_inputs() else {
                continue;
            };
            let times: Vec<f32> = inputs.collect();
            let Some(outputs) = reader.read_outputs() else {
                continue;
            };
            let (property, values): (AnimProperty, Vec<[f32; 4]>) = match outputs {
                ReadOutputs::Translations(iter) => (
                    AnimProperty::Translation,
                    iter.map(|v| [v[0], v[1], v[2], 1.0]).collect(),
                ),
                ReadOutputs::Rotations(rotations) => {
                    (AnimProperty::Rotation, rotations.into_f32().collect())
                }
                ReadOutputs::Scales(iter) => (
                    AnimProperty::Scale,
                    iter.map(|v| [v[0], v[1], v[2], 1.0]).collect(),
                ),
                ReadOutputs::MorphTargetWeights(_) => {
                    morph_channels += 1;
                    continue;
                }
            };

            // CUBICSPLINE outputs are (in-tangent, value, out-tangent)
            // triples; keep only the value element.
            let values: Vec<[f32; 4]> = if cubic {
                values
                    .chunks(3)
                    .filter_map(|triple| triple.get(1).copied())
                    .collect()
            } else {
                values
            };

            let keyframes: Vec<Keyframe> = times
                .iter()
                .zip(values)
                .map(|(&time, value)| Keyframe { time, value })
                .collect();
            if let Some(last) = keyframes.last() {
                duration = duration.max(last.time);
            }
            channels.push(IrClipChannel {
                joint_index,
                property,
                keyframes,
            });
        }

        if skipped_targets > 0 {
            diagnostics.push(Diagnostic::warning(
                "gltf.animation_target_not_joint",
                format!(
                    "animation '{name}' has {skipped_targets} channels targeting nodes outside the bound skin; channels skipped"
                ),
            ));
        }
        if morph_channels > 0 {
            diagnostics.push(Diagnostic::warning(
                "gltf.morph_weights_unsupported",
                format!(
                    "animation '{name}' has {morph_channels} morph weight channels; morph targets are not supported"
                ),
            ));
        }
        if downgraded_samplers > 0 {
            diagnostics.push(Diagnostic::warning(
                "gltf.animation_interpolation_downgraded",
                format!(
                    "animation '{name}' has {downgraded_samplers} STEP/CUBICSPLINE samplers played back with linear interpolation"
                ),
            ));
        }

        let (Some(skin_pos), false) = (skin_position, channels.is_empty()) else {
            continue;
        };
        clips.push(IrClip {
            source_index: anim_idx,
            name,
            skin_index: skins[skin_pos].source_index,
            channels,
            duration,
        });
    }

    clips
}

/// Parses every texture that decodes to a supported pixel format as an
/// [`IrTexture`]; the returned `Vec<bool>` marks which *original* texture
/// selectors survived, for [`parse_materials`] to null out dangling
/// references.
fn parse_textures(
    document: &gltf::Document,
    images: &[gltf::image::Data],
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<IrTexture>, Vec<bool>) {
    let mut textures = Vec::new();
    let mut valid = vec![false; document.textures().len()];
    for texture in document.textures() {
        let index = texture.index();
        let image_index = texture.source().index();
        let Some(image) = images.get(image_index) else {
            diagnostics.push(Diagnostic::error(
                "gltf.texture_image_missing",
                format!("texture {index} references unavailable image {image_index}"),
            ));
            continue;
        };
        let Some(rgba8) = image_rgba8(image) else {
            diagnostics.push(Diagnostic::error(
                "gltf.texture_format_unsupported",
                format!(
                    "texture {index} uses unsupported decoded image format {:?}",
                    image.format
                ),
            ));
            continue;
        };
        if image.width > crate::render_limits::MAX_TEXTURE_DIMENSION
            || image.height > crate::render_limits::MAX_TEXTURE_DIMENSION
        {
            diagnostics.push(Diagnostic::error(
                "renderer.texture_dimension_limit",
                format!(
                    "glTF texture {index} is {}x{}, exceeding the {}px renderer limit",
                    image.width,
                    image.height,
                    crate::render_limits::MAX_TEXTURE_DIMENSION
                ),
            ));
        }
        valid[index] = true;
        textures.push(IrTexture {
            source_index: index,
            name: texture.name().unwrap_or("unnamed").to_owned(),
            width: image.width,
            height: image.height,
            rgba8,
        });
    }
    (textures, valid)
}

fn image_rgba8(image: &gltf::image::Data) -> Option<Vec<u8>> {
    use gltf::image::Format;
    let pixel_count = usize::try_from(image.width)
        .ok()?
        .checked_mul(usize::try_from(image.height).ok()?)?;
    let mut rgba = Vec::with_capacity(pixel_count.checked_mul(4)?);
    match image.format {
        Format::R8 => {
            for &r in &image.pixels {
                rgba.extend_from_slice(&[r, r, r, 255]);
            }
        }
        Format::R8G8 => {
            for pixel in image.pixels.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        Format::R8G8B8 => {
            for pixel in image.pixels.chunks_exact(3) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        Format::R8G8B8A8 => rgba.extend_from_slice(&image.pixels),
        _ => return None,
    }
    (rgba.len() == pixel_count * 4).then_some(rgba)
}

/// Parses every material to the engine's material contract as an
/// [`IrMaterial`]; texture references use `texture_valid` to null out any
/// reference to a texture [`parse_textures`] dropped.
fn parse_materials(
    document: &gltf::Document,
    texture_valid: &[bool],
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrMaterial> {
    document
        .materials()
        .enumerate()
        .map(|(index, source)| {
            let pbr = source.pbr_metallic_roughness();
            let base = pbr.base_color_factor();
            let emissive = source.emissive_factor();
            let texture_ref = |index: usize| {
                texture_valid
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    .then_some(index)
            };
            IrMaterial {
                source_index: index,
                name: source.name().unwrap_or("unnamed").to_owned(),
                base_color: LinearRgba {
                    r: base[0],
                    g: base[1],
                    b: base[2],
                    a: base[3],
                },
                base_color_texture: pbr
                    .base_color_texture()
                    .and_then(|texture| texture_ref(texture.texture().index())),
                normal_texture: source
                    .normal_texture()
                    .and_then(|texture| texture_ref(texture.texture().index())),
                metallic_roughness_texture: pbr
                    .metallic_roughness_texture()
                    .and_then(|texture| texture_ref(texture.texture().index())),
                occlusion_texture: source
                    .occlusion_texture()
                    .and_then(|texture| texture_ref(texture.texture().index())),
                emissive_texture: source
                    .emissive_texture()
                    .and_then(|texture| texture_ref(texture.texture().index())),
                emissive_color: LinearRgba {
                    r: emissive[0],
                    g: emissive[1],
                    b: emissive[2],
                    a: 1.0,
                },
                normal_scale: source
                    .normal_texture()
                    .map(|texture| texture.scale())
                    .unwrap_or(1.0),
                occlusion_strength: source
                    .occlusion_texture()
                    .map(|texture| texture.strength())
                    .unwrap_or(1.0),
                roughness: pbr.roughness_factor(),
                metallic: pbr.metallic_factor(),
                alpha_mode: match source.alpha_mode() {
                    gltf::material::AlphaMode::Opaque => MaterialAlphaMode::Opaque,
                    gltf::material::AlphaMode::Mask => MaterialAlphaMode::Mask,
                    gltf::material::AlphaMode::Blend => MaterialAlphaMode::Blend,
                },
                alpha_cutoff: source.alpha_cutoff().unwrap_or(0.5),
                cull_mode: if source.double_sided() {
                    MaterialCullMode::None
                } else {
                    MaterialCullMode::Back
                },
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
    use crate::skeleton_asset::BoneId;
    use engine_authoring::id::AssetId;

    const EMPTY_GLTF: &[u8] = br#"{"asset":{"version":"2.0"}}"#;

    #[test]
    fn import_empty_gltf_returns_no_meshes() {
        let id = AssetId::generate();
        let result = import_gltf_bytes(&id, EMPTY_GLTF, &[]).expect("empty glTF must parse");
        assert!(result.meshes.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn import_invalid_bytes_returns_error() {
        let id = AssetId::generate();
        assert!(
            import_gltf_bytes(&id, b"not a valid gltf document", &[]).is_err(),
            "garbage bytes must produce a parse error"
        );
    }

    #[test]
    fn external_dependency_uri_decodes_percent_escaped_file_names() {
        assert_eq!(
            percent_decode_uri("textures/Hero%20Base%20Color.png").as_deref(),
            Some("textures/Hero Base Color.png")
        );
        assert!(percent_decode_uri("broken%2Gname.bin").is_none());
    }

    #[test]
    fn path_import_resolves_external_buffer_image_and_material() {
        let directory = tempfile::tempdir().expect("temporary glTF directory");
        let mut positions = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            positions.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(directory.path().join("mesh.bin"), positions).expect("buffer fixture");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([20, 40, 60, 255]))
            .save(directory.path().join("pixel.png"))
            .expect("image fixture");
        let document = r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"mesh.bin","byteLength":36}],
            "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "images":[{"uri":"pixel.png"}],
            "textures":[{"name":"albedo","source":0}],
            "materials":[{"name":"surface","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"roughnessFactor":0.25,"metallicFactor":0.75}}],
            "meshes":[{"name":"triangle","primitives":[{"attributes":{"POSITION":0},"material":0}]}]
        }"#;
        let path = directory.path().join("external.gltf");
        std::fs::write(&path, document).expect("document fixture");
        let source = AssetId::generate();

        let imported = import_gltf_path(&source, &path, &[]).expect("path import");

        assert_eq!(imported.meshes.len(), 1);
        assert_eq!(imported.textures.len(), 1);
        assert_eq!(imported.textures[0].rgba8, vec![20, 40, 60, 255]);
        assert_eq!(imported.materials.len(), 1);
        assert_eq!(imported.materials[0].material.roughness, 0.25);
        assert_eq!(imported.materials[0].material.metallic, 0.75);
        assert_eq!(
            imported.materials[0].material.base_color_texture,
            Some(AssetId::derive(&source, "texture:0"))
        );
    }

    #[test]
    fn path_import_preserves_vertex_tangents() {
        let directory = tempfile::tempdir().expect("temporary glTF directory");
        let mut buffer = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for tangent in [
            [1.0_f32, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, -1.0],
            [0.0, 0.0, 1.0, 1.0],
        ] {
            for value in tangent {
                buffer.extend_from_slice(&value.to_le_bytes());
            }
        }
        std::fs::write(directory.path().join("mesh.bin"), buffer).expect("buffer fixture");
        let document = r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"mesh.bin","byteLength":84}],
            "bufferViews":[
                {"buffer":0,"byteOffset":0,"byteLength":36},
                {"buffer":0,"byteOffset":36,"byteLength":48}
            ],
            "accessors":[
                {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                {"bufferView":1,"componentType":5126,"count":3,"type":"VEC4"}
            ],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"TANGENT":1}}]}]
        }"#;
        let path = directory.path().join("tangents.gltf");
        std::fs::write(&path, document).expect("document fixture");

        let imported = import_gltf_path(&AssetId::generate(), &path, &[]).expect("path import");
        assert_eq!(
            imported.meshes[0].mesh.tangents.as_deref(),
            Some(
                &[
                    [1.0, 0.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, -1.0],
                    [0.0, 0.0, 1.0, 1.0]
                ][..]
            )
        );
        assert!(imported.meshes[0].mesh.validate().is_ok());
    }

    #[test]
    fn a_skipped_primitive_leaves_the_mesh_with_its_surviving_surfaces() {
        let directory = tempfile::tempdir().expect("temporary glTF directory");
        let mut positions = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            positions.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(directory.path().join("mesh.bin"), positions).expect("buffer fixture");
        let path = directory.path().join("selectors.gltf");
        std::fs::write(
            &path,
            r#"{
                "asset":{"version":"2.0"},
                "buffers":[{"uri":"mesh.bin","byteLength":36}],
                "bufferViews":[{"buffer":0,"byteLength":36}],
                "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
                "meshes":[
                    {"name":"body","primitives":[
                        {"attributes":{"POSITION":0},"mode":0},
                        {"attributes":{"POSITION":0}}
                    ]}
                ]
            }"#,
        )
        .expect("document fixture");
        let source = AssetId::generate();

        let imported = import_gltf_path(&source, &path, &[]).expect("path import");

        // The unsupported point primitive is dropped, leaving the mesh with
        // the one surviving surface. Its selector is the glTF mesh index.
        assert_eq!(imported.meshes.len(), 1);
        assert_eq!(imported.meshes[0].source_index, 0);
        assert_eq!(imported.meshes[0].name, "body");
        assert_eq!(imported.meshes[0].materials.len(), 1);
        assert!(
            imported.meshes[0].mesh.submeshes.is_empty(),
            "a mesh left with one surface needs no explicit ranges"
        );
        assert_eq!(
            imported.meshes[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Mesh, 0)
        );
    }

    #[test]
    fn glb_character_imports_skin_three_clips_material_and_texture() {
        let directory = tempfile::tempdir().expect("temporary GLB directory");
        let path = directory.path().join("character.glb");
        std::fs::write(&path, three_clip_character_glb()).expect("GLB fixture");
        let source = AssetId::generate();

        let imported = import_gltf_path(&source, &path, &[]).expect("character GLB import");

        assert_eq!(imported.meshes.len(), 1);
        assert_eq!(imported.skins.len(), 1);
        assert_eq!(imported.animations.len(), 3);
        assert_eq!(imported.materials.len(), 1);
        assert_eq!(imported.textures.len(), 1);
        assert_eq!(
            imported
                .animations
                .iter()
                .map(|animation| animation.name.as_str())
                .collect::<Vec<_>>(),
            ["idle", "attack", "damage"]
        );
        for (index, animation) in imported.animations.iter().enumerate() {
            assert_eq!(
                animation.id,
                imported_sub_asset_id(&source, ImportedSubAssetKind::Animation, index)
            );
        }
    }

    /// Produces one self-contained character GLB for cross-module acceptance
    /// tests. It extends the canonical skinned fixture with three named clips
    /// and an image-backed PBR material.
    pub(crate) fn three_clip_character_glb() -> Vec<u8> {
        let mut document: serde_json::Value =
            serde_json::from_str(SKINNED_GLTF).expect("skinned fixture JSON");
        let uri = document["buffers"][0]["uri"]
            .as_str()
            .expect("embedded fixture buffer");
        let mut binary = decode_base64(uri.split_once(',').expect("data URI separator").1);
        document["buffers"][0]
            .as_object_mut()
            .expect("buffer object")
            .remove("uri");

        let animation = document["animations"][0].clone();
        document["animations"] = serde_json::Value::Array(
            ["idle", "attack", "damage"]
                .into_iter()
                .map(|name| {
                    let mut animation = animation.clone();
                    animation["name"] = serde_json::Value::String(name.into());
                    animation
                })
                .collect(),
        );

        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }
        let image_offset = binary.len();
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([80, 120, 200, 255]),
        ));
        let mut png = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("PNG fixture encoding");
        let png = png.into_inner();
        let png_length = png.len();
        binary.extend_from_slice(&png);
        while !binary.len().is_multiple_of(4) {
            binary.push(0);
        }

        document["bufferViews"]
            .as_array_mut()
            .expect("buffer views")
            .push(serde_json::json!({
                "buffer": 0,
                "byteOffset": image_offset,
                "byteLength": png_length
            }));
        document["buffers"][0]["byteLength"] = serde_json::json!(binary.len());
        document["images"] = serde_json::json!([{
            "bufferView": 6,
            "mimeType": "image/png",
            "name": "armor_albedo"
        }]);
        document["textures"] = serde_json::json!([{
            "source": 0,
            "name": "armor_albedo"
        }]);
        document["materials"] = serde_json::json!([{
            "name": "armor",
            "pbrMetallicRoughness": {
                "baseColorTexture": {"index": 0},
                "roughnessFactor": 0.45,
                "metallicFactor": 0.6
            }
        }]);
        document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(0);

        let mut json = serde_json::to_vec(&document).expect("GLB JSON serialization");
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total_length = 12 + 8 + json.len() + 8 + binary.len();
        let mut glb = Vec::with_capacity(total_length);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&(total_length as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534A_u32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942_u32.to_le_bytes());
        glb.extend_from_slice(&binary);
        glb
    }

    fn decode_base64(encoded: &str) -> Vec<u8> {
        let mut output = Vec::new();
        let mut bits = 0_u32;
        let mut bit_count = 0_u8;
        for byte in encoded.bytes().filter(|byte| *byte != b'=') {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            bits = (bits << 6) | u32::from(value);
            bit_count += 6;
            if bit_count >= 8 {
                bit_count -= 8;
                output.push((bits >> bit_count) as u8);
                bits &= (1_u32 << bit_count) - 1;
            }
        }
        output
    }

    /// One triangle skinned to a two-joint skeleton with a rotation clip on
    /// the tip joint. Vertex 2's weights sum to 2.0 to exercise
    /// renormalization.
    pub(crate) const SKINNED_GLTF: &str = r#"{
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 2]}],
        "nodes": [
            {"name": "root_joint", "children": [1]},
            {"name": "tip_joint", "translation": [0.0, 1.0, 0.0]},
            {"name": "character", "mesh": 0, "skin": 0}
        ],
        "skins": [{
            "name": "skeleton",
            "joints": [0, 1],
            "inverseBindMatrices": 3
        }],
        "meshes": [{
            "name": "triangle",
            "primitives": [{
                "attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}
            }]
        }],
        "animations": [{
            "name": "spin",
            "channels": [
                {"sampler": 0, "target": {"node": 1, "path": "rotation"}},
                {"sampler": 1, "target": {"node": 2, "path": "translation"}}
            ],
            "samplers": [
                {"input": 4, "output": 5, "interpolation": "LINEAR"},
                {"input": 4, "output": 6, "interpolation": "LINEAR"}
            ]
        }],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
            {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
            {"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR",
             "min": [0.0], "max": [1.0]},
            {"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC4"},
            {"bufferView": 0, "componentType": 5126, "count": 2, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 0.0, 0.0]}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 24},
            {"buffer": 0, "byteOffset": 60, "byteLength": 48},
            {"buffer": 0, "byteOffset": 108, "byteLength": 128},
            {"buffer": 0, "byteOffset": 236, "byteLength": 8},
            {"buffer": 0, "byteOffset": 244, "byteLength": 32}
        ],
        "buffers": [{
            "byteLength": 276,
            "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAABAAAAAAAAAAEAAAAAAAAAAQAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAD8AAAA/AAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAPMENT/zBDU/"
        }]
    }"#;

    #[test]
    fn import_skinned_gltf_extracts_skin_nodes_and_weights() {
        let id = AssetId::generate();
        let result =
            import_gltf_bytes(&id, SKINNED_GLTF.as_bytes(), &[]).expect("skinned glTF must parse");

        assert_eq!(result.meshes.len(), 1);
        assert_eq!(result.skins.len(), 1);
        assert_eq!(result.nodes.len(), 3);

        let mesh_data = &result.meshes[0];
        assert_eq!(mesh_data.skin_index, Some(0));
        let skinning = mesh_data
            .mesh
            .skinning
            .as_ref()
            .expect("skinned mesh must carry skinning attributes");
        assert_eq!(skinning.len(), mesh_data.mesh.vertices.len());
        assert_eq!(skinning[0].joints, [0, 1, 0, 0]);
        assert_eq!(skinning[0].weights, [1.0, 0.0, 0.0, 0.0]);
        // Vertex 2 weights summed to 2.0 and must be renormalized.
        assert!((skinning[2].weights.iter().sum::<f32>() - 1.0).abs() < 1.0e-6);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "gltf.weights_renormalized"));

        let skin = &result.skins[0];
        assert_eq!(skin.joint_nodes, vec![0, 1]);
        assert_eq!(skin.inverse_bind_matrices.len(), 2);
        // A brand-new skeleton assigns sequential bone IDs from zero.
        assert_eq!(skin.joint_bone_ids, vec![BoneId(0), BoneId(1)]);
        assert_eq!(skin.skeleton.bones.len(), 2);
        assert_eq!(skin.skeleton.next_bone_id, 2);

        assert_eq!(result.nodes[1].parent, Some(0));
        assert_eq!(result.nodes[1].translation, glam::Vec3::new(0.0, 1.0, 0.0));

        // The full pipeline: spawn the skeleton and validate the mesh.
        assert!(mesh_data.mesh.validate().is_ok());
        let mut world = engine_ecs::World::new();
        let rig = crate::skinning::spawn_rig(&mut world, &skin.skeleton)
            .expect("imported rig must spawn");
        assert_eq!(rig.skeleton.joints.len(), skin.skeleton.bones.len());
        assert_eq!(
            rig.skeleton.bone_ids,
            skin.skeleton
                .bones
                .iter()
                .map(|bone| bone.id)
                .collect::<Vec<_>>()
        );
        assert_eq!(rig.skeleton.asset, Some(skin.skeleton.id.clone()));
    }

    #[test]
    fn import_skinned_gltf_extracts_bone_targeted_animation() {
        let id = AssetId::generate();
        let result =
            import_gltf_bytes(&id, SKINNED_GLTF.as_bytes(), &[]).expect("skinned glTF must parse");

        assert_eq!(result.animations.len(), 1);
        let animation = &result.animations[0];
        assert_eq!(animation.name, "spin");
        assert_eq!(animation.skin_index, 0);
        assert!((animation.clip.duration - 1.0).abs() < 1.0e-6);
        let skin = &result.skins[0];
        assert_eq!(animation.clip.skeleton, Some(skin.skeleton.id.clone()));
        assert_eq!(
            animation.clip.skeleton_identity,
            Some(skin.skeleton.identity)
        );
        // Only the rotation channel survives (the translation channel targets
        // a non-joint mesh node and is skipped), so no translation channel
        // exists to auto-detect a root bone from.
        assert_eq!(animation.clip.root_bone, None);

        // The rotation channel targets tip_joint (skin joint index 1); the
        // translation channel targets the non-joint mesh node and is skipped.
        assert_eq!(animation.clip.channels.len(), 1);
        let channel = &animation.clip.channels[0];
        assert_eq!(channel.target_bone, Some(skin.joint_bone_ids[1]));
        assert_eq!(channel.keyframes.len(), 2);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "gltf.animation_target_not_joint"));
    }

    #[test]
    fn dedupe_adopts_the_skeleton_id_of_an_identical_rig_from_another_source() {
        let character_source = AssetId::generate();
        let character_result = import_gltf_bytes(&character_source, SKINNED_GLTF.as_bytes(), &[])
            .expect("character glTF must parse");
        let character_skin = &character_result.skins[0];

        // A different source (e.g. a motion-only file) describing the exact
        // same rig: dedupe must adopt the character's skeleton ID and bone
        // assignments even though this source has never been imported
        // before, as long as the caller includes the character's ledger.
        let motion_source = AssetId::generate();
        let motion_result = import_gltf_bytes(
            &motion_source,
            SKINNED_GLTF.as_bytes(),
            &character_result.skeleton_records,
        )
        .expect("motion glTF must parse");
        let motion_skin = &motion_result.skins[0];

        assert_eq!(motion_skin.skeleton.id, character_skin.skeleton.id);
        assert_eq!(motion_skin.joint_bone_ids, character_skin.joint_bone_ids);
        assert_ne!(
            motion_skin.skeleton_id, motion_skin.skeleton.id,
            "the motion source's own naturally-derived skeleton ID must differ from the adopted one"
        );
        assert!(
            !motion_result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "anim.skeleton_rebind"),
            "a same-identity dedupe carries bones over silently, with no rebind diagnostic"
        );
    }

    #[test]
    fn reimport_with_a_renamed_bone_retires_the_old_name_and_adds_the_new_one() {
        let source = AssetId::generate();
        let first = import_gltf_bytes(&source, SKINNED_GLTF.as_bytes(), &[])
            .expect("first import must parse");
        let first_skin = &first.skins[0];
        assert_eq!(first_skin.skeleton.next_bone_id, 2);

        // Renaming "tip_joint" changes the skeleton's identity, so reimport
        // falls back to name matching against the source's own prior record
        // instead of the identity dedupe path.
        let renamed_gltf = SKINNED_GLTF.replace("tip_joint", "renamed_tip");
        let second = import_gltf_bytes(&source, renamed_gltf.as_bytes(), &first.skeleton_records)
            .expect("renamed import must parse");
        let second_skin = &second.skins[0];

        assert_eq!(
            second_skin.skeleton.id, first_skin.skeleton.id,
            "the source's own skeleton ID must not change on reimport"
        );
        let root_bone = second_skin
            .skeleton
            .bones
            .iter()
            .find(|bone| bone.name == "root_joint")
            .expect("root_joint must survive the rename");
        assert_eq!(
            root_bone.id,
            BoneId(0),
            "an unchanged bone must keep its ID"
        );
        let renamed_bone = second_skin
            .skeleton
            .bones
            .iter()
            .find(|bone| bone.name == "renamed_tip")
            .expect("renamed_tip must be present");
        assert_eq!(
            renamed_bone.id,
            BoneId(2),
            "a renamed bone must get a fresh ID rather than reuse tip_joint's retired one"
        );
        assert_eq!(
            second_skin.skeleton.next_bone_id, 3,
            "next_bone_id must advance monotonically"
        );
        assert!(second
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.skeleton_rebind"));

        // Reimporting the already-renamed rig again must now dedupe by
        // identity: no further rebind, and next_bone_id never decreases or
        // reuses the retired ID.
        let third = import_gltf_bytes(&source, renamed_gltf.as_bytes(), &second.skeleton_records)
            .expect("second renamed import must parse");
        let third_skin = &third.skins[0];
        assert_eq!(third_skin.skeleton.next_bone_id, 3);
        assert!(!third
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.skeleton_rebind"));
    }

    #[test]
    fn sub_asset_ids_are_deterministic() {
        let parent = AssetId::generate();
        let id_a = AssetId::derive(&parent, "mesh:0");
        let id_b = AssetId::derive(&parent, "mesh:0");
        assert_eq!(
            id_a, id_b,
            "same parent and discriminant must produce the same sub-asset ID"
        );

        let id_c = AssetId::derive(&parent, "mesh:1");
        assert_ne!(
            id_a, id_c,
            "different discriminants must produce different sub-asset IDs"
        );

        let other_parent = AssetId::generate();
        let id_d = AssetId::derive(&other_parent, "mesh:0");
        assert_ne!(
            id_a, id_d,
            "different parent sources must produce different sub-asset IDs"
        );
    }
}
