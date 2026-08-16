//! PMX model parser via `mmd-anim-format` (ADR 0097, ADR 0112).
//!
//! Parses `.pmx` byte slices and files into a format-independent
//! `ModelDocument` (see `crate::model_ir` for the exact normalization
//! contract), mirroring `crate::fbx_import`'s structure. This module owns
//! every `mmd_anim_format::pmx` symbol and normalizes model data before the
//! format-independent builder assigns sub-asset IDs and skeleton identity;
//! that remains `crate::model_import`'s job, unchanged by which parser
//! produced its input (ADR 0078).
//!
//! # Scope
//!
//! This module imports PMX mesh, skeleton, material, morph, and rigid-body/joint
//! data into the format-independent model IR (ADR 0097 §1, §2, §4, §5, §6).
//! Rigid bodies and joints are best-effort source hints for an engine-native
//! Secondary Motion rig (ADR 0112): unsupported or lossy constructs emit
//! actionable diagnostics instead of making otherwise usable model content fail.
//! VMD motion baking (§3) remains in `crate::vmd_import`, which consumes the
//! same `.pmx` bytes to build its evaluator rig, so `ModelDocument::clips` is
//! always empty for a PMX source.
//!
//! # Normalization contract
//!
//! - **Axes**: PMX/MMD is left-handed, +Y up. The engine is right-handed,
//!   +Y up. Every position, normal, and bone rest position has its Z
//!   component negated (`convert_position`); each triangle's last two
//!   corners are swapped after conversion so winding stays front-facing
//!   under the new handedness (see `build_group`).
//! - **Units**: PMX has no declared physical unit; MMD models are
//!   conventionally authored at roughly 8 engine-agnostic units per 1.6 m of
//!   human height. `PMX_TO_METERS` is a fixed, documented convention
//!   (not derived from the source file) applied uniformly to every position
//!   and bone rest translation so an imported character ends up roughly
//!   life-sized in meters. A model authored at a different internal scale
//!   will import at a correspondingly different real-world size; there is no
//!   per-file signal this importer could use to detect that case.
//! - **UVs**: PMX stores UVs with the same top-left-origin convention as
//!   glTF (MMD, like PMX's other conventions, follows DirectX's coordinate
//!   layout), which already matches the engine's convention, so — unlike
//!   `crate::fbx_import`'s Maya-origin flip — no `v` flip is applied here.
//! - **Bones -> `IrNode`**: one node per PMX bone, in PMX's own bone order
//!   (`document.nodes[i]` corresponds to PMX bone index `i`, with no
//!   remapping). `crate::model_import::build_import_result`'s internal
//!   `build_skeleton_bones` helper already places a skin's bones in
//!   parent-before-child order via a bounded, order-agnostic topological
//!   pass (it only requires `IrNode::parent` to point at *some* valid
//!   earlier-or-later index, not a specific array order), so this parser
//!   does not need to defensively resort PMX's bone array itself — doing so
//!   would only add a remapping layer with no observable benefit, since the
//!   identity mapping (PMX bone index == node index) already keeps every
//!   bone-indexed reference (vertex skin indices, IK/append targets we
//!   intentionally never read) trivially consistent. A bone with an
//!   out-of-range or self-referencing `parent_index` is treated as a root
//!   and reported via `pmx.bone_invalid_parent`.
//!
//!   `IrNode::translation` is derived from `PmxParsedBone::position`,
//!   which PMX defines as the bone's *absolute* rest position, not a local
//!   offset: this parser converts every bone's absolute position once
//!   (`convert_position`) and subtracts the (also-converted) parent
//!   position to get a plain local translation. `IrNode::rotation` is
//!   always `Quat::IDENTITY` and `IrNode::scale` is always `Vec3::ONE`,
//!   since PMX bones carry no rest rotation or scale.
//!
//!   IK, appended-parent, fixed-axis, and local-axis data
//!   (`PmxParsedBone::ik` / `append_transform` / `fixed_axis` /
//!   `local_axis`) are per-frame runtime constraints, not rest-pose data —
//!   this parser never reads those fields, so they cannot leak into the IR.
//!   Resolving them into plain FK curves is `crate::vmd_import`'s job
//!   (ADR 0097 §3), which reads them from the source `.pmx` bytes directly
//!   rather than from this IR.
//! - **Meshes and skins (multi-skin split, ADR 0097 §4)**: PMX materials are
//!   the natural mesh partition, produced directly by
//!   `mmd_anim_format::pmx::split_pmx_model_by_material`. For each resulting
//!   per-material mesh, this parser computes the distinct set of bones with
//!   non-zero vertex weight; a set of at most `MAX_JOINTS` becomes one
//!   `IrMesh` / `IrSkin` pair directly. A larger set is further split by
//!   `group_triangles_by_bone_locality` into several groups, each at most
//!   `MAX_JOINTS` bones, so every material — including ones whose jiggle
//!   rig exceeds the render skin cap — still renders in full. See that
//!   function's doc for the triangle-ordering heuristic and its rationale.
//!
//!   **Sub-asset selector scheme**: one PMX material can now produce several
//!   `IrMesh`/`IrSkin` pairs, so a single running counter (starting at 0)
//!   assigns each pair's `source_index`: it visits
//!   `split_pmx_model_by_material`'s returned meshes in order (ascending
//!   original PMX material index — that function's own iteration order) and,
//!   within a material that needed a bone-count sub-split, visits that
//!   material's groups in `group_triangles_by_bone_locality`'s
//!   deterministic output order. A mesh/skin pair emitted at counter value
//!   `n` gets `IrMesh::source_index == IrSkin::source_index == n`; this does
//!   not collide between the two lists because `crate::asset::
//!   imported_sub_asset_id` hashes each `crate::asset::
//!   ImportedSubAssetKind`'s own prefix together with the index. This
//!   scheme is deterministic for identical source bytes (both
//!   `split_pmx_model_by_material` and the greedy grouping pass are pure
//!   functions of the parsed model), so re-importing the same file yields
//!   identical sub-asset IDs.
//!
//!   **Shared skeleton (ADR 0097 §4a)**: every split `IrSkin` produced above
//!   still describes the same single PMX rig, so `build_model_document` sets
//!   `ModelDocument::skeleton_scope` to
//!   `crate::model_ir::SkeletonScope::SharedAcrossDocument` with
//!   `skeleton_nodes` set to exactly `0..bone_count` — the PMX bone range,
//!   captured before the per-split-part mesh anchor `IrNode`s (the
//!   parentless nodes each split group attaches its `mesh`/`skin` indices
//!   to, pushed onto `ModelDocument::nodes` after the bones) are appended.
//!   Those anchor nodes are deliberately *excluded* from `skeleton_nodes`:
//!   were they included, the shared skeleton's bone list — and therefore its
//!   `crate::skeleton_asset::SkeletonIdentity` (ADR 0077 §4) — would depend
//!   on how many render parts the multi-skin split happened to produce
//!   rather than on the PMX rig itself, so tuning the split heuristic or
//!   editing a model enough to change a split part count would spuriously
//!   trip ADR 0077's rebind path even though the rig never changed.
//!   `crate::model_import::build_import_result` then builds exactly one
//!   `crate::skeleton_asset::SkeletonAsset` spanning every PMX bone and
//!   binds every split skin to it, rather than deriving a separate,
//!   differently-scoped skeleton per render part. This is what makes a split
//!   PMX character one Skinned Model over one shared rig instead of one
//!   Skinned Model per material, and what gives a future baked VMD clip
//!   (ADR 0097 §3) a single skeleton to target.
//!
//!   `skeleton_nodes` is the *full* PMX bone range, not only the bones some
//!   skin weights vertices to: PMX's IK bones (`左足ＩＫ`/`右足ＩＫ` and
//!   similar) carry no vertex weight at all, so they appear in no skin's
//!   `joint_nodes`, but VMD baking (`crate::vmd_import`, ADR 0097 §3)
//!   still drives them by name. Passing the whole bone range means every
//!   such bone is still spawned and addressable in the shared skeleton
//!   (ADR 0086 §1's existing "a bone no skin references is still spawned"
//!   guarantee), which is what makes that binding resolve.
//! - **Materials**: `PmxParsedMaterial` -> `IrMaterial`, one entry per PMX
//!   material (never split or dropped, so `IrMaterial::source_index` is
//!   always the original PMX material index). PMX surface properties map
//!   directly into `MaterialShadingModel::ToonLit`; no MMD-only shader is used.
//! - **Textures**: `PmxParsedMaterial::texture_path` is resolved relative to
//!   the PMX file's directory. PMX texture paths may use either separator
//!   and may contain non-ASCII (already decoded to a Rust `String` by the
//!   parser); this module normalizes both `/` and `\` before joining.
//!   Decoded with the `image` crate into RGBA8, same as
//!   `crate::gltf_import` / `crate::fbx_import`. A texture that fails to
//!   decode is dropped with a warning diagnostic, not a hard error.
//!   `pmx_source_dependencies` returns the resolved texture paths.
//!   Base, sphere-map, and non-shared toon textures use the same imported
//!   texture path.
//! - **Morphs**: PMX vertex, material, and group morphs are normalized into
//!   format-independent morph targets.
//! - **Secondary Motion hints**: PMX rigid bodies and joints are converted
//!   best-effort into generic `IrRigidBodyRig` data. Invalid body bindings,
//!   unsupported body modes/shapes, and dangling joint endpoints produce
//!   actionable warnings while preserving the rest of the import.
//! - **Soft bodies**: PMX soft bodies have no Secondary Motion representation
//!   and are omitted with `pmx.soft_body_unsupported` diagnostics.
//! - **Clips**: `ModelDocument::clips` is always empty; PMX carries no
//!   animation on its own — motion is a separate `.vmd` source imported
//!   through `crate::vmd_import` (ADR 0097 §3).

use crate::asset::SkeletonRecord;
use crate::model_import::GltfImportResult;
use crate::model_ir::{
    IrJoint, IrMaterial, IrMaterialMorphOffset, IrMaterialMorphOperation, IrMesh, IrMorphTarget, IrNode, IrRigidBody, IrRigidBodyMode, IrRigidBodyRig, IrRigidBodyShape, IrSkin, IrTexture, ModelDocument, SkeletonScope, IrMeshData as Mesh, IrSkinningVertexData as SkinningVertexData, IrSubmesh as Submesh, IrVertex as Vertex,
};
use crate::skinning::MAX_JOINTS;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use engine_authoring::{
    LinearRgba, MaterialAlphaMode, MaterialCullMode, MaterialOutline, MaterialShadingModel,
    MaterialSphereBlendMode, MaterialSphereCoordinateSource,
};
use glam::{Mat4, Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use mmd_anim_format::pmx::{
    PmxParsedBone, PmxParsedGeometry, PmxParsedMaterial, PmxParsedModel, PmxParsedRigidBody,
};
use std::fmt;
use std::path::{Path, PathBuf};

/// Uniform scale applied to every imported PMX position and bone rest
/// translation.
///
/// PMX carries no physical unit of its own; MMD models are conventionally
/// authored at roughly 8 internal units per 1.6 m of human height, so this
/// constant (`0.08` -> 8 units become 0.64 m... concretely: 8 units x
/// `PMX_TO_METERS` = 0.64 m per "MMD head unit" pairing) brings a typical
/// character to roughly life-sized in meters. This is a fixed,
/// convention-based heuristic documented here, not a value derived from any
/// per-file data — a model authored at a different internal scale imports at
/// a correspondingly different real-world size.
pub const PMX_TO_METERS: f32 = 0.08;

/// Tolerated deviation of a vertex weight sum from 1.0 before the importer
/// reports a renormalization warning (parity with [`crate::gltf_import`] and
/// [`crate::fbx_import`]).
const WEIGHT_SUM_TOLERANCE: f32 = 1.0e-3;

/// Squared length below which a morph's per-vertex delta is dropped as noise
/// (ADR 0097 §5).
///
/// `1e-6` m squared is a micrometre of movement: invisible at character
/// scale, but a vertex kept at that magnitude still costs a slot in the
/// sparse working set the blend walks every frame the morph is active.
const MORPH_DELTA_EPSILON_SQUARED: f32 = 1.0e-12;

/// Reports a fatal error that prevents parsing from completing.
#[derive(Debug)]
pub enum PmxImportError {
    /// PMX parsing failed; see the wrapped `mmd_anim_format` error for
    /// detail.
    Parse(mmd_anim_format::error::ImportError),
    /// Reading the source file from disk failed.
    Io(std::io::Error),
}

impl fmt::Display for PmxImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "PMX parse error: {error}"),
            Self::Io(error) => write!(f, "PMX source I/O error: {error}"),
        }
    }
}

impl std::error::Error for PmxImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

// ---------------------------------------------------------------------------
// Import entry points
// ---------------------------------------------------------------------------

/// Imports static meshes, skins, and materials from a PMX byte slice.
///
/// Sub-asset IDs are deterministic: re-importing the same `bytes` with the
/// same `source_id` produces identical sub-asset IDs. Internally this parses
/// `bytes` into a `ModelDocument` ([`parse_pmx`]) and hands it to
/// [`crate::model_import::build_import_result`]; see that function for the
/// asset-building contract.
///
/// Only textures embedded... no PMX texture is embedded, so a byte-slice
/// import (no source path context) cannot resolve any external texture file;
/// see [`import_pmx_path`] to resolve textures from disk.
///
/// See [`crate::gltf_import::import_gltf_bytes`] for the `existing_skeletons`
/// contract; this function has the identical contract.
///
/// # Errors
///
/// Returns [`PmxImportError::Parse`] when `bytes` is not a valid PMX
/// document.
pub fn import_pmx_bytes(
    source_id: &AssetId,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, PmxImportError> {
    let document = parse_pmx(bytes)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Imports a PMX source from disk, resolving external texture sidecars
/// relative to the source file.
///
/// See [`import_pmx_bytes`] for the `existing_skeletons` contract.
///
/// # Errors
///
/// Returns [`PmxImportError::Io`] when the file cannot be read, or
/// [`PmxImportError::Parse`] when its contents are not a valid PMX document.
pub fn import_pmx_path(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, PmxImportError> {
    let document = parse_pmx_path(path)?;
    Ok(crate::model_import::build_import_result(
        &document,
        source_id,
        existing_skeletons,
    ))
}

/// Same as [`import_pmx_path`], but overrides the ground-contact candidate
/// -bone name heuristic (ADR 0080 §1) with `contact_bone_names` when it is
/// non-empty, matching `crate::asset::ImportSettings::contact_bones`'s
/// contract — exact parity with
/// [`crate::fbx_import::import_fbx_path_with_contact_bones`].
///
/// # Errors
///
/// See [`import_pmx_path`].
pub fn import_pmx_path_with_contact_bones(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, PmxImportError> {
    let document = parse_pmx_path(path)?;
    Ok(crate::model_import::build_import_result_with_contact_bones(
        &document,
        source_id,
        existing_skeletons,
        contact_bone_names,
    ))
}

/// Parses a PMX byte slice into a format-independent `ModelDocument`
/// (ADR 0078), assigning no sub-asset IDs and no skeleton identity.
///
/// # Errors
///
/// Returns [`PmxImportError::Parse`] when `bytes` is not a valid PMX
/// document.
pub fn parse_pmx(bytes: &[u8]) -> Result<ModelDocument, PmxImportError> {
    let model = mmd_anim_format::pmx::parse_pmx_model(bytes).map_err(PmxImportError::Parse)?;
    Ok(build_model_document(&model, None))
}

/// Parses a PMX source from disk into a format-independent `ModelDocument`
/// (ADR 0078), resolving external texture sidecars relative to the source
/// file.
///
/// # Errors
///
/// Returns [`PmxImportError::Io`] when the file cannot be read, or
/// [`PmxImportError::Parse`] when its contents are not a valid PMX document.
pub fn parse_pmx_path(path: &Path) -> Result<ModelDocument, PmxImportError> {
    let bytes = std::fs::read(path).map_err(PmxImportError::Io)?;
    let model = mmd_anim_format::pmx::parse_pmx_model(&bytes).map_err(PmxImportError::Parse)?;
    Ok(build_model_document(&model, path.parent()))
}

/// Returns external texture sidecars declared by a PMX document.
///
/// # Errors
///
/// Returns [`PmxImportError::Io`] when the file cannot be read, or
/// [`PmxImportError::Parse`] when its contents are not a valid PMX document.
pub fn pmx_source_dependencies(path: &Path) -> Result<Vec<PathBuf>, PmxImportError> {
    let bytes = std::fs::read(path).map_err(PmxImportError::Io)?;
    let model = mmd_anim_format::pmx::parse_pmx_model(&bytes).map_err(PmxImportError::Parse)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut dependencies = Vec::new();
    for material in &model.materials {
        for relative in [
            &material.texture_path,
            &material.sphere_texture_path,
            &material.toon_texture_path,
        ] {
            if !relative.is_empty() {
                dependencies.push(resolve_pmx_texture_path(parent, relative));
            }
        }
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

/// Computes a deterministic content fingerprint over a source and sidecars.
/// Absolute paths are excluded so moving a project does not force reimport
/// (parity with [`crate::fbx_import::fingerprint_fbx_source`]).
pub fn fingerprint_pmx_source(
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

/// Parses a loaded PMX model into a `ModelDocument` (ADR 0078).
///
/// `source_dir` is the source file's parent directory, used to resolve
/// texture files; pass `None` for a byte-slice parse (any texture reference
/// is then dropped with a diagnostic instead, since PMX never embeds media).
fn build_model_document(model: &PmxParsedModel, source_dir: Option<&Path>) -> ModelDocument {
    let mut diagnostics = Vec::new();

    let world_positions: Vec<Vec3> = model
        .skeleton
        .bones
        .iter()
        .map(|bone| convert_position(bone.position))
        .collect();
    let mut nodes = build_bone_nodes(&model.skeleton.bones, &world_positions, &mut diagnostics);
    // Captured before mesh anchor nodes are appended below, so this is
    // exactly the PMX bone count: `nodes[0..bone_count]` are the bones
    // (`build_bone_nodes` emits one `IrNode` per PMX bone, in PMX order, and
    // nothing else), regardless of how many split render parts follow.
    let bone_count = nodes.len();

    let (texture_paths, texture_index_of) = collect_texture_paths(&model.materials);
    let (mut textures, texture_valid) = parse_textures(&texture_paths, source_dir, &mut diagnostics);
    let shared_toon_selector = append_shared_toon_textures(&model.materials, &mut textures, texture_paths.len());
    let mut materials = parse_materials(
        &model.materials,
        &texture_index_of,
        &texture_valid,
        &shared_toon_selector,
        &mut diagnostics,
    );

    let (mesh_nodes, meshes, skins) =
        build_meshes_and_skins(model, &world_positions, &mut diagnostics);
    nodes.extend(mesh_nodes);
    refine_alpha_modes_from_sampled_texels(&mut materials, &meshes, &textures);

    let rigid_body_rig = parse_rigid_body_rig(model, &world_positions, &mut diagnostics);

    ModelDocument {
        nodes,
        meshes,
        skins,
        // Every split render part binds to one document-wide skeleton
        // spanning the full 376-bone-scale PMX rig (ADR 0097 §4a), not one
        // skeleton per split skin: PMX's single implicit rig is exactly the
        // "several skins over one armature" case that field exists for.
        // `skeleton_nodes` is exactly the PMX bone range (0..bone_count),
        // deliberately excluding the mesh anchor nodes appended above, so
        // the shared skeleton's identity (ADR 0077 §4) depends only on the
        // PMX rig and never on how many render parts the multi-skin split
        // (§4) happened to produce.
        skeleton_scope: SkeletonScope::SharedAcrossDocument {
            skeleton_nodes: (0..bone_count).collect(),
        },
        clips: Vec::new(),
        materials,
        textures,
        rigid_body_rig,
        diagnostics,
    }
}

/// Converts one PMX-space position to the engine's right-handed, +Y-up,
/// meter-scaled convention: negate Z, then apply [`PMX_TO_METERS`].
///
/// Visible to [`crate::vmd_import`] so a baked motion (ADR 0097 §3) lands in
/// exactly the space this parser put the model in; the conversion must never
/// exist in two places.
/// Converts one PMX-space rotation to the engine's right-handed convention.
///
/// [`convert_position`] maps points with `diag(1, 1, -1)` (then a uniform
/// scale, which does not affect rotations). Conjugating a rotation by that
/// improper transform keeps the angle and negates the axis' X and Y
/// components — for a quaternion `(x, y, z, w)`, that is `(-x, -y, z, w)`.
///
/// Visible to [`crate::vmd_import`] for the same reason
/// [`convert_position`] is: the axis convention must exist in exactly one
/// place.
pub(crate) fn convert_rotation(rotation: [f32; 4]) -> Quat {
    Quat::from_xyzw(-rotation[0], -rotation[1], rotation[2], rotation[3]).normalize()
}

/// Builds a rotation from PMX's per-axis Euler angles, then converts it to
/// engine space.
///
/// PMX hands these XYZ component angles to Bullet as
/// `btQuaternion::setEulerZYX(z, y, x)`. `mmd-anim-physics-bullet`, which
/// consumes the same parser output as this importer, uses the equivalent
/// glam [`EulerRot::ZYX`] construction. Keeping the order conversion here
/// makes rigid-body and joint frames share one handedness/order path.
fn convert_euler_rotation(euler: [f32; 3]) -> Quat {
    let mmd = glam::Quat::from_euler(glam::EulerRot::ZYX, euler[2], euler[1], euler[0]);
    convert_rotation(mmd.to_array())
}

pub(crate) fn convert_position(position: [f32; 3]) -> Vec3 {
    Vec3::new(position[0], position[1], -position[2]) * PMX_TO_METERS
}

/// Builds one [`IrNode`] per PMX bone, in PMX's own bone order (see the
/// module doc for why no defensive resort is needed).
fn build_bone_nodes(
    bones: &[PmxParsedBone],
    world_positions: &[Vec3],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrNode> {
    let bone_count = bones.len();
    bones
        .iter()
        .enumerate()
        .map(|(index, bone)| {
            let parent = resolve_parent(bone.parent_index, index, bone_count, diagnostics);
            let translation = match parent {
                Some(parent_index) => world_positions[index] - world_positions[parent_index],
                None => world_positions[index],
            };
            IrNode {
                name: bone_display_name(bone, index),
                parent,
                translation,
                rotation: Quat::IDENTITY,
                scale: Vec3::ONE,
                mesh: None,
                skin: None,
            }
        })
        .collect()
}

/// Resolves a PMX bone's `parent_index` to a validated node index, reporting
/// `pmx.bone_invalid_parent` and falling back to a root bone (`None`) when it
/// is negative, out of range, or self-referencing.
fn resolve_parent(
    parent_index: i32,
    bone_index: usize,
    bone_count: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<usize> {
    if parent_index < 0 {
        return None;
    }
    let candidate = parent_index as usize;
    if candidate < bone_count && candidate != bone_index {
        return Some(candidate);
    }
    diagnostics.push(Diagnostic::warning(
        "pmx.bone_invalid_parent",
        format!(
            "bone {bone_index} declares an out-of-range or self parent ({parent_index}); it was treated as a root bone"
        ),
    ));
    None
}

fn bone_display_name(bone: &PmxParsedBone, index: usize) -> String {
    if bone.name.is_empty() {
        format!("bone_{index}")
    } else {
        bone.name.clone()
    }
}

fn material_display_name(material: &PmxParsedMaterial, index: usize) -> String {
    if material.name.is_empty() {
        format!("material_{index}")
    } else {
        material.name.clone()
    }
}

/// Collects every distinct base, toon-ramp, and sphere-map path.
///
/// Returns the ordered path list (its position is the texture's
/// `source_index`) and a `path -> source_index` lookup for
/// [`parse_materials`].
fn collect_texture_paths(materials: &[PmxParsedMaterial]) -> (Vec<String>, HashMap<String, usize>) {
    let mut ordered = Vec::new();
    let mut index_of = HashMap::new();
    for material in materials {
        for path in [
            &material.texture_path,
            &material.toon_texture_path,
            &material.sphere_texture_path,
        ] {
            if !path.is_empty() && !index_of.contains_key(path) {
                index_of.insert(path.clone(), ordered.len());
                ordered.push(path.clone());
            }
        }
    }
    (ordered, index_of)
}

/// Normalizes a PMX-authored relative texture path (which may use either
/// separator) and joins it to `dir`.
fn resolve_pmx_texture_path(dir: &Path, relative: &str) -> PathBuf {
    let normalized = relative.replace(['\\', '/'], std::path::MAIN_SEPARATOR_STR);
    dir.join(normalized)
}

fn texture_display_name(relative: &str) -> String {
    Path::new(relative)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| relative.to_owned())
}

/// Resolves and decodes every distinct texture path collected by
/// [`collect_texture_paths`]; the returned `Vec<bool>` marks which *original*
/// selectors (positions in `paths`) survived, for [`parse_materials`] to null
/// out dangling references.
fn parse_textures(
    paths: &[String],
    source_dir: Option<&Path>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<IrTexture>, Vec<bool>) {
    let mut textures = Vec::new();
    let mut valid = vec![false; paths.len()];

    for (index, relative) in paths.iter().enumerate() {
        let bytes = match source_dir {
            Some(dir) => {
                let path = resolve_pmx_texture_path(dir, relative);
                match std::fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(_) => {
                        diagnostics.push(Diagnostic::warning(
                            "pmx.texture_file_missing",
                            format!(
                                "texture '{relative}' file '{}' could not be read; texture skipped",
                                path.display()
                            ),
                        ));
                        None
                    }
                }
            }
            None => {
                diagnostics.push(Diagnostic::warning(
                    "pmx.texture_not_embedded",
                    format!(
                        "texture '{relative}' is not embedded; a byte-slice import cannot resolve its external file"
                    ),
                ));
                None
            }
        };

        let Some(bytes) = bytes else { continue };
        let Some((width, height, rgba8)) = decode_rgba8(&bytes) else {
            diagnostics.push(Diagnostic::warning(
                "pmx.texture_format_unsupported",
                format!("texture '{relative}' could not be decoded; texture skipped"),
            ));
            continue;
        };
        if width > crate::render_limits::MAX_TEXTURE_DIMENSION
            || height > crate::render_limits::MAX_TEXTURE_DIMENSION
        {
            diagnostics.push(Diagnostic::error(
                "renderer.texture_dimension_limit",
                format!(
                    "PMX texture '{relative}' is {width}x{height}, exceeding the {}px renderer limit",
                    crate::render_limits::MAX_TEXTURE_DIMENSION
                ),
            ));
        }
        valid[index] = true;
        textures.push(IrTexture {
            source_index: index,
            name: texture_display_name(relative),
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

/// Synthesizes and appends one [`IrTexture`] per distinct
/// `shared_toon_index` referenced by `materials`, returning a lookup from
/// that index to the appended texture's selector.
///
/// A PMX material with `toon_flag == 1` names one of MikuMikuDance's ten
/// conventional "shared" toon ramps (`toon01.bmp`..`toon10.bmp`, indexed
/// 0-9) instead of embedding a custom texture — PMX never carries their
/// pixels, since every MMD-compatible viewer is expected to already have its
/// own copy. This synthesizes a same-shaped replacement (bright at the top,
/// tinted toward a per-index shade at the bottom) rather than an exact
/// reproduction of the freeware-bundled originals, so shared-toon materials
/// get real per-vertex shading instead of silently losing their ramp.
///
/// Appended selectors start at `next_selector` (the real per-file texture
/// count), so they can never collide with a `source_index` returned by
/// [`collect_texture_paths`].
fn append_shared_toon_textures(
    materials: &[PmxParsedMaterial],
    textures: &mut Vec<IrTexture>,
    next_selector: usize,
) -> HashMap<u8, usize> {
    let mut selector_of = HashMap::new();
    for material in materials {
        let Some(shared_index) = material.shared_toon_index else {
            continue;
        };
        selector_of.entry(shared_index).or_insert_with(|| {
            let selector = next_selector + usize::from(shared_index);
            textures.push(builtin_toon_texture(shared_index, selector));
            selector
        });
    }
    selector_of
}

fn linear_to_srgb_u8(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Procedurally builds one approximation of a MikuMikuDance shared toon ramp
/// (see [`append_shared_toon_textures`]). `shared_toon_index` is PMX's
/// zero-based selector (`0` names the conventional `toon01.bmp`).
///
/// The palette below is authored in the engine's scene-linear color space.
/// [`IrTexture::rgba8`] is later uploaded through the toon-ramp color slot,
/// which is sRGB-decoded by the GPU, so generated RGB bytes must carry the
/// sRGB source transfer function just like decoded PNG/BMP color textures.
fn builtin_toon_texture(shared_toon_index: u8, selector: usize) -> IrTexture {
    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 64;
    // Roughly follows the shared set's usual progression: mild, warm
    // shadows on the first couple of indices, cooler and more saturated
    // shadows through the middle, and a near-flat, bright ramp at the end
    // (conventionally used as a "barely toon-shaded" default).
    const SHADES: [[f32; 3]; 10] = [
        [0.82, 0.80, 0.78],
        [0.72, 0.70, 0.70],
        [0.55, 0.62, 0.78],
        [0.45, 0.52, 0.74],
        [0.58, 0.52, 0.68],
        [0.70, 0.48, 0.52],
        [0.74, 0.62, 0.42],
        [0.52, 0.68, 0.54],
        [0.62, 0.62, 0.64],
        [0.90, 0.90, 0.90],
    ];
    let shade = SHADES[usize::from(shared_toon_index.min(9))];
    let mut rgba8 = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        // 0.0 at the top (bright) to 1.0 at the bottom (shaded), matching
        // the vertical gradient every conventional toon ramp uses.
        let t = y as f32 / (HEIGHT - 1) as f32;
        let pixel = [
            (1.0 + (shade[0] - 1.0) * t).clamp(0.0, 1.0),
            (1.0 + (shade[1] - 1.0) * t).clamp(0.0, 1.0),
            (1.0 + (shade[2] - 1.0) * t).clamp(0.0, 1.0),
        ];
        for _ in 0..WIDTH {
            rgba8.push(linear_to_srgb_u8(pixel[0]));
            rgba8.push(linear_to_srgb_u8(pixel[1]));
            rgba8.push(linear_to_srgb_u8(pixel[2]));
            rgba8.push(255);
        }
    }
    IrTexture {
        source_index: selector,
        name: format!("builtin_toon{:02}", shared_toon_index + 1),
        width: WIDTH,
        height: HEIGHT,
        rgba8,
    }
}

/// Parses every PMX material to the engine's material contract as an
/// [`IrMaterial`] (ADR 0097 §2). Materials are never dropped or split, so
/// `IrMaterial::source_index` is always the original PMX material index.
fn parse_materials(
    materials: &[PmxParsedMaterial],
    texture_index_of: &HashMap<String, usize>,
    texture_valid: &[bool],
    shared_toon_selector: &HashMap<u8, usize>,
    _diagnostics: &mut Vec<Diagnostic>,
) -> Vec<IrMaterial> {
    materials
        .iter()
        .enumerate()
        .map(|(index, material)| {
            let base_color_texture = if material.texture_path.is_empty() {
                None
            } else {
                texture_index_of
                    .get(&material.texture_path)
                    .copied()
                    .filter(|&texture_index| {
                        texture_valid.get(texture_index).copied().unwrap_or(false)
                    })
            };
            let texture_ref = |path: &str| {
                (!path.is_empty())
                    .then(|| texture_index_of.get(path).copied())
                    .flatten()
                    .filter(|&texture_index| texture_valid.get(texture_index).copied().unwrap_or(false))
            };
            // `toon_texture_path` (custom) and `shared_toon_index` (one of
            // MikuMikuDance's ten conventional ramps) are mutually
            // exclusive per PMX's `toon_flag` (see `append_shared_toon_textures`).
            let toon_ramp_texture = match material.shared_toon_index {
                Some(shared_index) => shared_toon_selector.get(&shared_index).copied(),
                None => texture_ref(&material.toon_texture_path),
            };
            let sphere_texture = (material.sphere_mode != "none")
                .then(|| texture_ref(&material.sphere_texture_path))
                .flatten();
            let (sphere_blend, sphere_coordinates) = match material.sphere_mode.as_str() {
                "add" => (MaterialSphereBlendMode::Add, MaterialSphereCoordinateSource::ViewNormal),
                "subTexture" => (MaterialSphereBlendMode::Multiply, MaterialSphereCoordinateSource::AdditionalUv0),
                _ => (MaterialSphereBlendMode::Multiply, MaterialSphereCoordinateSource::ViewNormal),
            };

            IrMaterial {
                source_index: index,
                name: material_display_name(material, index),
                base_color: LinearRgba {
                    r: material.diffuse[0],
                    g: material.diffuse[1],
                    b: material.diffuse[2],
                    a: material.diffuse[3],
                },
                base_color_texture,
                normal_texture: None,
                metallic_roughness_texture: None,
                occlusion_texture: None,
                emissive_texture: None,
                emissive_color: LinearRgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                normal_scale: 1.0,
                occlusion_strength: 1.0,
                roughness: 0.5,
                metallic: 0.0,
                // Refined to `Blend` after mesh building by
                // `refine_alpha_modes_from_sampled_texels` if the base
                // texture turns out to carry real transparency within the
                // UVs this material's own vertices actually sample; PMX
                // shares one texture atlas across many materials, and most
                // of an atlas is usually outside the region any given
                // material uses, so scanning the whole image here would
                // over-trigger `Blend` and classify opaque regions as
                // translucent in both the main and outline-mask passes far
                // more often than it would catch a genuinely translucent
                // material (see ADR 0100).
                alpha_mode: if material.diffuse[3] < 1.0 {
                    MaterialAlphaMode::Blend
                } else {
                    MaterialAlphaMode::Opaque
                },
                alpha_cutoff: 0.5,
                cull_mode: if material.flags.double_sided {
                    MaterialCullMode::None
                } else {
                    MaterialCullMode::Back
                },
                shading_model: MaterialShadingModel::ToonLit,
                toon_ramp_texture,
                toon_shadow_color: LinearRgba { r: 0.55, g: 0.55, b: 0.62, a: 1.0 },
                toon_ambient_color: LinearRgba {
                    r: material.ambient[0],
                    g: material.ambient[1],
                    b: material.ambient[2],
                    a: 1.0,
                },
                toon_specular_color: LinearRgba {
                    r: material.specular[0],
                    g: material.specular[1],
                    b: material.specular[2],
                    a: 1.0,
                },
                toon_specular_power: material.specular_power.max(0.0),
                sphere_texture,
                sphere_blend,
                sphere_coordinates,
                outline: MaterialOutline {
                    enabled: material.flags.edge,
                    color: LinearRgba {
                        r: material.edge_color[0],
                        g: material.edge_color[1],
                        b: material.edge_color[2],
                        a: material.edge_color[3],
                    },
                    width: material.edge_size.max(0.0) * PMX_TO_METERS * 0.1,
                    // PMX has no semantic field for skin. Common MMD naming
                    // conventions still identify body surfaces reliably
                    // enough to retain a restrained skin/clothing boundary;
                    // all other materials preserve silhouette-only behavior.
                    internal_boundary_strength: pmx_skin_internal_boundary_strength(
                        &material.name,
                    ),
                },
                cast_shadow: material.flags.self_shadow_map,
                receive_shadow: material.flags.self_shadow,
            }
        })
        .collect()
}

/// Returns a restrained same-model boundary strength for conventional PMX skin names.
fn pmx_skin_internal_boundary_strength(name: &str) -> f32 {
    let lowercase = name.to_lowercase();
    if lowercase.contains("body") || lowercase.contains("skin") || name.contains('肌') {
        0.55
    } else {
        0.0
    }
}

/// Refines a material's `alpha_mode` from the base-texture alpha its own
/// submesh vertices actually sample. Runs after mesh building, since only
/// then do real per-vertex UVs exist to sample — `parse_materials` sees
/// materials in isolation, before any mesh does.
///
/// PMX shares one texture atlas across many materials, and most of an atlas
/// is usually outside the region any one material's UVs cover (unused
/// margins, other materials' regions). Scanning the *whole* image, as an
/// earlier version of this importer did, over-triggers on transparency the
/// material never shows. Sampling every vertex the submesh actually has,
/// rather than rasterizing every triangle's interior, is a close
/// approximation: PMX atlases are painted as smooth per-region alpha (opaque
/// garment vs. transparent margin), not fine-grained per-triangle detail.
///
/// Which non-opaque mode a material lands in matters beyond how it looks.
/// `Blend` disables depth writes, so a blended surface is invisible to every
/// later depth-tested draw — the editor's floor grid drew straight through a
/// character's blouse for exactly this reason. `Mask` keeps depth writes and
/// alpha-tests instead, which is what a cutout wants, and it also keeps the
/// surface out of the sorted transparent pass.
///
/// The two cases separate by how much of the material's *own* surface is
/// non-opaque. A garment whose only transparent texels are a hem frill or a
/// printed logo is a cutout; a lace overlay, an ambient-occlusion layer, or a
/// hair-shadow plane samples non-opaque texels across most of itself and is
/// genuinely translucent. Measured over this repository's MMD sources the two
/// populations sit far apart (cutouts at 0.2%–6%, translucent layers at
/// 50%–100%), so the threshold between them is not delicate.
fn model_ir_draw_ranges(mesh: &Mesh) -> Vec<Submesh> {
    if !mesh.submeshes.is_empty() {
        return mesh.submeshes.clone();
    }
    let count = match &mesh.indices {
        Some(indices) => indices.len(),
        None => mesh.vertices.len(),
    };
    vec![Submesh {
        start: 0,
        count: u32::try_from(count).unwrap_or(u32::MAX),
    }]
}

fn refine_alpha_modes_from_sampled_texels(
    materials: &mut [IrMaterial],
    meshes: &[IrMesh],
    textures: &[IrTexture],
) {
    // A pixel a shade under fully opaque is ordinary PNG/compression noise,
    // not authored transparency.
    const OPAQUE_ALPHA_THRESHOLD: u8 = 250;
    // Share of a material's own sampled surface that must be non-opaque
    // before it counts as a translucent layer rather than a cutout.
    const TRANSLUCENT_COVERAGE_RATIO: f32 = 0.25;

    // Accumulated per material rather than decided per submesh, so a material
    // several submeshes share lands in one mode regardless of mesh order.
    let mut coverage = vec![(0usize, 0usize); materials.len()];

    for mesh in meshes {
        for (range, material_index) in model_ir_draw_ranges(&mesh.mesh).iter()
            .zip(mesh.submesh_materials.iter())
        {
            let Some(material_index) = *material_index else {
                continue;
            };
            let Some(material) = materials.get(material_index) else {
                continue;
            };
            // A material the author already made translucent through diffuse
            // alpha keeps that decision; only opaque ones are refined here.
            if material.alpha_mode != MaterialAlphaMode::Opaque {
                continue;
            }
            let Some(texture_selector) = material.base_color_texture else {
                continue;
            };
            let Some(texture) = textures
                .iter()
                .find(|texture| texture.source_index == texture_selector)
            else {
                continue;
            };
            let Some(counters) = coverage.get_mut(material_index) else {
                continue;
            };
            for selector in submesh_vertex_selectors(&mesh.mesh, range) {
                let Some(vertex) = mesh.mesh.vertices.get(selector) else {
                    continue;
                };
                counters.0 += 1;
                if sample_alpha(texture, vertex.uv) < OPAQUE_ALPHA_THRESHOLD {
                    counters.1 += 1;
                }
            }
        }
    }

    for (material, (sampled, non_opaque)) in materials.iter_mut().zip(coverage) {
        if sampled == 0 || non_opaque == 0 {
            continue;
        }
        material.alpha_mode = if non_opaque as f32
            >= sampled as f32 * TRANSLUCENT_COVERAGE_RATIO
        {
            MaterialAlphaMode::Blend
        } else {
            MaterialAlphaMode::Mask
        };
    }
}

/// Returns the distinct vertex selectors one submesh draws, resolving indices
/// when the mesh has them and falling back to the direct vertex range when it
/// does not.
///
/// Distinct rather than per-index, so a densely triangulated region does not
/// weigh more than a sparse one of the same area in the coverage ratio above.
fn submesh_vertex_selectors(mesh: &Mesh, range: &Submesh) -> Vec<usize> {
    let mut selectors: Vec<usize> = match mesh.indices.as_ref() {
        Some(indices) => {
            let start = (range.start as usize).min(indices.len());
            let end = start
                .saturating_add(range.count as usize)
                .min(indices.len());
            indices[start..end]
                .iter()
                .map(|index| *index as usize)
                .collect()
        }
        None => {
            let start = (range.start as usize).min(mesh.vertices.len());
            let end = start
                .saturating_add(range.count as usize)
                .min(mesh.vertices.len());
            (start..end).collect()
        }
    };
    selectors.sort_unstable();
    selectors.dedup();
    selectors
}

/// Samples one texel's alpha channel, clamping `uv` to the texture edge —
/// matching the renderer's own sampler address mode (`ClampToEdge`).
fn sample_alpha(texture: &IrTexture, uv: [f32; 2]) -> u8 {
    if texture.width == 0 || texture.height == 0 {
        return 255;
    }
    let x = (uv[0].clamp(0.0, 1.0) * (texture.width - 1) as f32).round() as u32;
    let y = (uv[1].clamp(0.0, 1.0) * (texture.height - 1) as f32).round() as u32;
    let index = ((y * texture.width + x) * 4 + 3) as usize;
    texture.rgba8.get(index).copied().unwrap_or(255)
}

// ---------------------------------------------------------------------------
// Secondary Motion import hints (ADR 0097 §6, ADR 0112)
// ---------------------------------------------------------------------------

/// Converts PMX rigid bodies and joints into format-independent Secondary Motion hints.
///
/// The conversion is deliberately best effort (ADR 0112): stable source intent is
/// preserved where possible, while unsupported or lossy inputs emit diagnostics
/// instead of failing an otherwise usable model import. Nothing here simulates.
///
/// Returns `None` for a model that declares no bodies, so the common
/// non-MMD case never carries an empty rig.
fn parse_rigid_body_rig(
    model: &PmxParsedModel,
    world_positions: &[Vec3],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IrRigidBodyRig> {
    report_unsupported_soft_bodies(model.soft_bodies.len(), diagnostics);
    if model.rigid_bodies.is_empty() {
        if !model.joints.is_empty() {
            diagnostics.push(Diagnostic::warning(
                "pmx.joint_without_bodies",
                format!(
                    "{} PMX joints cannot form Secondary Motion constraints because the model declares no rigid bodies; those joints were omitted",
                    model.joints.len()
                ),
            ));
        }
        return None;
    }

    let bone_count = model.skeleton.bones.len();
    let mut invalid_bone_bindings = 0usize;
    let mut unsupported_shapes = 0usize;
    let mut unsupported_modes = 0usize;
    let bodies: Vec<IrRigidBody> = model
        .rigid_bodies
        .iter()
        .map(|body| {
            let bone_node = resolve_rigid_body_bone_index(
                body.bone_index,
                bone_count,
                &mut invalid_bone_bindings,
            );
            // PMX stores a body's rest transform in model space, while the
            // simulation needs it relative to the bone it rides; the bone's
            // own rest position is the only anchor either side agrees on.
            let bone_world = bone_node
                .and_then(|node| world_positions.get(node).copied())
                .unwrap_or(Vec3::ZERO);
            IrRigidBody {
                name: body.name.clone(),
                bone_node,
                shape: convert_rigid_body_shape(body, &mut unsupported_shapes),
                bone_offset_translation: convert_position(body.position) - bone_world,
                bone_offset_rotation: convert_euler_rotation(body.rotation),
                mass: body.mass,
                linear_damping: body.linear_damping,
                angular_damping: body.angular_damping,
                restitution: body.restitution,
                friction: body.friction,
                mode: convert_rigid_body_mode(&body.mode, &mut unsupported_modes),
                group: body.group,
                // mmd-anim-format exposes this field as the groups this body
                // is allowed to contact, matching the engine IR contract.
                collides_with: body.mask,
            }
        })
        .collect();

    if invalid_bone_bindings > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.rigid_body_bone_out_of_range",
            format!(
                "{invalid_bone_bindings} rigid bodies reference a PMX bone that does not exist; those bodies were imported unbound. Rebind them to a valid skeleton bone before enabling Secondary Motion"
            ),
        ));
    }

    if unsupported_shapes > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.rigid_body_shape_unsupported",
            format!(
                "{unsupported_shapes} rigid bodies declare a shape this importer does not represent; they were imported as spheres. Review those Secondary Motion colliders before enabling simulation"
            ),
        ));
    }

    if unsupported_modes > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.rigid_body_mode_unsupported",
            format!(
                "{unsupported_modes} rigid bodies use an unsupported PMX mode; they were imported as FollowBone. Review those Secondary Motion bodies before enabling simulation"
            ),
        ));
    }

    let body_count = bodies.len();
    let mut dangling_joints = 0usize;
    let mut unsupported_joint_kinds = 0usize;
    let joints: Vec<IrJoint> = model
        .joints
        .iter()
        .filter_map(|joint| {
            if joint.kind != "spring6dof" {
                unsupported_joint_kinds += 1;
                return None;
            }
            let resolve = |index: i32, dangling: &mut usize| {
                if index >= 0 && (index as usize) < body_count {
                    Some(index as usize)
                } else {
                    *dangling += 1;
                    None
                }
            };
            Some(IrJoint {
                name: joint.name.clone(),
                body_a: resolve(joint.rigid_body_index_a, &mut dangling_joints),
                body_b: resolve(joint.rigid_body_index_b, &mut dangling_joints),
                translation: convert_position(joint.position),
                rotation: convert_euler_rotation(joint.rotation),
                // Limits are per-axis extents in the joint's own frame, so
                // they take the same Z negation and metre scale as any other
                // length, and the lower/upper pair swaps on the flipped axis.
                translation_lower: convert_limit_lower(
                    joint.translation_lower_limit,
                    joint.translation_upper_limit,
                ),
                translation_upper: convert_limit_upper(
                    joint.translation_lower_limit,
                    joint.translation_upper_limit,
                ),
                // Angles carry no length, so only the handedness flip
                // applies: negating a rotation about X or Y swaps its limits.
                rotation_lower: Vec3::new(
                    -joint.rotation_upper_limit[0],
                    -joint.rotation_upper_limit[1],
                    joint.rotation_lower_limit[2],
                ),
                rotation_upper: Vec3::new(
                    -joint.rotation_lower_limit[0],
                    -joint.rotation_lower_limit[1],
                    joint.rotation_upper_limit[2],
                ),
                // Stiffnesses are magnitudes: no sign, no scale.
                spring_translation: Vec3::from_array(joint.spring_translation_factor),
                spring_rotation: Vec3::from_array(joint.spring_rotation_factor),
            })
        })
        .collect();

    if unsupported_joint_kinds > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.joint_kind_unsupported",
            format!(
                "{unsupported_joint_kinds} PMX joints use a constraint kind Secondary Motion does not represent and were omitted. Recreate equivalent engine-native constraints if needed"
            ),
        ));
    }

    if dangling_joints > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.joint_body_out_of_range",
            format!(
                "{dangling_joints} joint endpoints reference a rigid body that does not exist; those endpoints were left unbound. Reconnect or remove those Secondary Motion constraints before enabling simulation"
            ),
        ));
    }

    Some(IrRigidBodyRig { bodies, joints })
}

/// Converts a PMX shape name and its size triple to an [`IrRigidBodyShape`].
///
/// PMX encodes size as three floats whose meaning depends on the shape; an
/// unrecognized shape name counts into `unsupported` and falls back to a
/// sphere, which is the shape whose size field (`x` = radius) every PMX
/// writer fills in.
fn convert_rigid_body_shape(
    body: &PmxParsedRigidBody,
    unsupported: &mut usize,
) -> IrRigidBodyShape {
    match body.shape.as_str() {
        "sphere" => IrRigidBodyShape::Sphere {
            radius: body.size[0] * PMX_TO_METERS,
        },
        "box" => IrRigidBodyShape::Box {
            half_extents: Vec3::new(body.size[0], body.size[1], body.size[2]) * PMX_TO_METERS,
        },
        "capsule" => IrRigidBodyShape::Capsule {
            radius: body.size[0] * PMX_TO_METERS,
            half_height: body.size[1] * 0.5 * PMX_TO_METERS,
        },
        _ => {
            *unsupported += 1;
            IrRigidBodyShape::Sphere {
                radius: body.size[0] * PMX_TO_METERS,
            }
        }
    }
}

fn report_unsupported_soft_bodies(count: usize, diagnostics: &mut Vec<Diagnostic>) {
    if count > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.soft_body_unsupported",
            format!(
                "{count} PMX soft bodies cannot be represented by Secondary Motion and were omitted. Recreate equivalent motion with an engine-native Secondary Motion rig if needed"
            ),
        ));
    }
}

fn resolve_rigid_body_bone_index(
    index: i32,
    bone_count: usize,
    invalid: &mut usize,
) -> Option<usize> {
    if index == -1 {
        None
    } else if index >= 0 && (index as usize) < bone_count {
        Some(index as usize)
    } else {
        *invalid += 1;
        None
    }
}

/// Converts PMX's rigid-body mode name to [`IrRigidBodyMode`].
///
/// Unknown modes are counted for a structured import diagnostic and fall back
/// to [`IrRigidBodyMode::FollowBone`], the safest non-simulating behavior.
fn convert_rigid_body_mode(mode: &str, unsupported: &mut usize) -> IrRigidBodyMode {
    match mode {
        "static" => IrRigidBodyMode::FollowBone,
        "dynamic" => IrRigidBodyMode::Dynamic,
        "dynamicBone" | "dynamicBonePosition" | "dynamic_bone_position" => {
            IrRigidBodyMode::DynamicWithBonePosition
        }
        _ => {
            *unsupported += 1;
            IrRigidBodyMode::FollowBone
        }
    }
}

/// Converts a PMX per-axis lower limit, accounting for the Z negation
/// swapping which end of the Z range is the lower one.
fn convert_limit_lower(lower: [f32; 3], upper: [f32; 3]) -> Vec3 {
    Vec3::new(lower[0], lower[1], -upper[2]) * PMX_TO_METERS
}

/// Converts a PMX per-axis upper limit; see [`convert_limit_lower`].
fn convert_limit_upper(lower: [f32; 3], upper: [f32; 3]) -> Vec3 {
    Vec3::new(upper[0], upper[1], -lower[2]) * PMX_TO_METERS
}

// ---------------------------------------------------------------------------
// Mesh / multi-skin split (ADR 0097 §4)
// ---------------------------------------------------------------------------

/// Per-vertex bone influences with non-zero weight, at most 4 per vertex
/// (PMX's own vertex weight encodings never exceed 4 slots).
type VertexInfluences = Vec<Vec<(usize, f32)>>;

fn collect_influences(geometry: &PmxParsedGeometry, vertex_count: usize) -> VertexInfluences {
    (0..vertex_count)
        .map(|vertex| {
            (0..4)
                .filter_map(|slot| {
                    let flat = vertex * 4 + slot;
                    let bone = *geometry.skin_indices.get(flat)?;
                    let weight = *geometry.skin_weights.get(flat)?;
                    (weight > 0.0).then_some((bone as usize, weight))
                })
                .collect()
        })
        .collect()
}

/// Builds every mesh/skin pair for `model` (ADR 0097 §4), plus one synthetic
/// [`IrNode`] per pair that instantiates it (`mesh` and `skin` both set).
/// These nodes carry an identity transform: like every skinned mesh node in
/// this engine (see [`crate::gltf_prefab`]'s "skinned draw" handling), a
/// skinned draw's world placement comes entirely from the joint palette, so
/// the instantiating node's own transform is never read for anything.
fn build_meshes_and_skins(
    model: &PmxParsedModel,
    world_positions: &[Vec3],
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<IrNode>, Vec<IrMesh>, Vec<IrSkin>) {
    let split = mmd_anim_format::pmx::split_pmx_model_by_material(model);

    let mut nodes = Vec::new();
    let mut meshes = Vec::new();
    let mut skins = Vec::new();
    let mut next_group_id = 0usize;
    let mut renormalized_total = 0usize;

    for split_mesh in &split.meshes {
        let geometry = &split_mesh.geometry;
        let vertex_count = geometry.positions.len() / 3;
        let influences = collect_influences(geometry, vertex_count);
        let triangle_count = geometry.indices.len() / 3;

        let mut distinct_bones: HashSet<usize> = HashSet::new();
        for influence in &influences {
            distinct_bones.extend(influence.iter().map(|&(bone, _)| bone));
        }

        let triangle_groups: Vec<Vec<usize>> = if distinct_bones.len() <= MAX_JOINTS {
            vec![(0..triangle_count).collect()]
        } else {
            group_triangles_by_bone_locality(geometry, &influences, triangle_count)
        };

        let group_count = triangle_groups.len();
        if group_count > 1 {
            diagnostics.push(Diagnostic::warning(
                "pmx.skin_split_by_bone_count",
                format!(
                    "material '{}' needed {group_count} render parts to stay within the {MAX_JOINTS}-joint skin limit ({} distinct bones)",
                    split_mesh.material.name,
                    distinct_bones.len()
                ),
            ));
        }

        // Flattened once per material rather than once per render part: the
        // fold is over the material's whole morph list, which every part of
        // that material shares.
        let flattened_morphs =
            flatten_split_morphs(&split_mesh.morphs, split_mesh.original_material_index);
        let base_name =
            material_display_name(&split_mesh.material, split_mesh.original_material_index);
        for (group_index, triangles) in triangle_groups.into_iter().enumerate() {
            let name = if group_count > 1 {
                format!("{base_name} ({}/{group_count})", group_index + 1)
            } else {
                base_name.clone()
            };
            let group_id = next_group_id;
            next_group_id += 1;

            let (mesh, skin, renormalized) = build_group(GroupBuildInput {
                model,
                split_mesh,
                flattened_morphs: &flattened_morphs,
                influences: &influences,
                triangles: &triangles,
                name: &name,
                group_id,
                world_positions,
            });
            renormalized_total += renormalized;

            nodes.push(IrNode {
                name,
                parent: None,
                translation: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                scale: Vec3::ONE,
                mesh: Some(group_id),
                skin: Some(group_id),
            });
            meshes.push(mesh);
            skins.push(skin);
        }
    }

    if renormalized_total > 0 {
        diagnostics.push(Diagnostic::warning(
            "pmx.skin_weights_renormalized",
            format!(
                "{renormalized_total} vertices had skin weights not summing to 1.0; weights were renormalized"
            ),
        ));
    }

    (nodes, meshes, skins)
}

// ---------------------------------------------------------------------------
// Morph targets (ADR 0097 §5)
// ---------------------------------------------------------------------------

/// One morph resolved to the vertex and material changes it actually
/// applies, with every group reference already folded in.
#[derive(Default)]
struct FlattenedMorph {
    /// Position deltas keyed by *split-mesh-local* vertex index, in PMX
    /// units and axes (converted only once, when the target is emitted).
    vertex_deltas: HashMap<u32, [f32; 3]>,
    /// Accumulated base-color change, `None` until some contribution sets it.
    material: Option<(IrMaterialMorphOperation, LinearRgba)>,
}

/// Resolves one split mesh's morph list into flat per-morph deltas
/// (ADR 0097 §5).
///
/// PMX group morphs (kind 0) are weighted combinations of other morphs. They
/// are folded in here rather than carried into the IR, mirroring how
/// [`crate::fbx_import`] resolves FBX's multi-take structure into independent
/// clips instead of exporting the structure itself: the runtime then needs no
/// "group" concept at all, and a motion driving a group morph by name gets
/// the same result as MMD without a second evaluation pass.
///
/// Recursion is bounded by `visiting`, so a group morph that (directly or
/// through a cycle) references itself contributes nothing instead of looping
/// forever — PMX does not forbid such a cycle.
fn flatten_split_morphs(
    morphs: &[mmd_anim_format::pmx::PmxParsedMorph],
    material_index: usize,
) -> Vec<FlattenedMorph> {
    (0..morphs.len())
        .map(|index| {
            let mut flattened = FlattenedMorph::default();
            let mut visiting = Vec::new();
            accumulate_morph(
                morphs,
                index,
                1.0,
                material_index,
                &mut visiting,
                &mut flattened,
            );
            flattened
        })
        .collect()
}

/// Adds morph `index`'s contribution at `weight` into `out`, recursing
/// through group and flip references.
fn accumulate_morph(
    morphs: &[mmd_anim_format::pmx::PmxParsedMorph],
    index: usize,
    weight: f32,
    material_index: usize,
    visiting: &mut Vec<usize>,
    out: &mut FlattenedMorph,
) {
    let Some(morph) = morphs.get(index) else {
        return;
    };
    if visiting.contains(&index) {
        return;
    }
    visiting.push(index);

    for offset in &morph.vertex_offsets {
        let delta = out.vertex_deltas.entry(offset.vertex_index).or_insert([0.0; 3]);
        for (axis, value) in offset.position.iter().enumerate() {
            delta[axis] += value * weight;
        }
    }
    for offset in &morph.material_offsets {
        // The split already narrowed material offsets to this mesh's own
        // material (or left the source-wide `-1` sentinel), so both cases
        // address exactly one material here.
        let operation = match offset.operation.as_str() {
            "add" => IrMaterialMorphOperation::Add,
            _ => IrMaterialMorphOperation::Multiply,
        };
        let contribution = LinearRgba {
            r: offset.diffuse[0] * weight,
            g: offset.diffuse[1] * weight,
            b: offset.diffuse[2] * weight,
            a: offset.diffuse[3] * weight,
        };
        match &mut out.material {
            Some((existing_operation, color)) if *existing_operation == operation => {
                color.r += contribution.r;
                color.g += contribution.g;
                color.b += contribution.b;
                color.a += contribution.a;
            }
            // Mixing operations inside one group morph has no meaningful
            // combined form; the first contribution wins, which matches
            // MMD's own last-writer-per-operation behavior closely enough
            // for the single-operation morphs real models use.
            Some(_) => {}
            slot @ None => *slot = Some((operation, contribution)),
        }
    }
    let _ = material_index;

    for reference in morph.group_offsets.iter().chain(&morph.flip_offsets) {
        if reference.morph_index < 0 {
            continue;
        }
        accumulate_morph(
            morphs,
            reference.morph_index as usize,
            weight * reference.weight,
            material_index,
            visiting,
            out,
        );
    }

    visiting.pop();
}

/// Builds this render group's [`IrMorphTarget`] list.
///
/// `vertex_remap` maps split-mesh-local vertex indices to this group's own,
/// so a morph that only touches vertices outside this group (the common case
/// once a material is sub-split, §4 step 3) contributes nothing here and is
/// dropped rather than emitted empty.
fn build_group_morph_targets(
    split_mesh: &mmd_anim_format::pmx::PmxMaterialSplitMesh,
    model: &PmxParsedModel,
    flattened: &[FlattenedMorph],
    vertex_remap: &HashMap<u32, u32>,
) -> Vec<IrMorphTarget> {
    let mut targets = Vec::new();
    for entry in &split_mesh.morph_index_map {
        let Some(flat) = flattened.get(entry.local_index) else {
            continue;
        };
        let mut vertex_deltas: Vec<(u32, Vec3)> = flat
            .vertex_deltas
            .iter()
            .filter_map(|(split_vertex, delta)| {
                vertex_remap
                    .get(split_vertex)
                    .map(|&local| (local, convert_delta(*delta)))
            })
            // A delta that rounds to nothing after conversion is noise the
            // blend would pay for on every frame it is active.
            .filter(|(_, delta)| delta.length_squared() > MORPH_DELTA_EPSILON_SQUARED)
            .collect();
        vertex_deltas.sort_by_key(|(vertex, _)| *vertex);

        let material_offsets = flat
            .material
            .map(|(operation, base_color)| {
                vec![IrMaterialMorphOffset {
                    material_index: split_mesh.original_material_index,
                    operation,
                    base_color,
                }]
            })
            .unwrap_or_default();

        if vertex_deltas.is_empty() && material_offsets.is_empty() {
            continue;
        }
        targets.push(IrMorphTarget {
            // The *original* PMX morph index, never the split-local one, so
            // a morph's sub-asset ID depends on the model's own morph order
            // and not on how the multi-skin split happened to partition it.
            source_index: entry.original_index,
            name: morph_display_name(model, entry.original_index),
            vertex_deltas,
            material_offsets,
        });
    }
    targets
}

/// Converts one PMX-space position *delta* to engine units and axes.
///
/// A delta is a difference of positions, so it takes exactly the same linear
/// map [`convert_position`] applies — but never a translation, which is why
/// it goes through the same formula rather than through two position
/// conversions.
fn convert_delta(delta: [f32; 3]) -> Vec3 {
    convert_position(delta)
}

fn morph_display_name(model: &PmxParsedModel, index: usize) -> String {
    match model.morphs.get(index) {
        Some(morph) if !morph.name.is_empty() => morph.name.clone(),
        _ => format!("morph_{index}"),
    }
}

/// Sub-splits `triangle_count` triangles of `geometry` into groups whose
/// referenced bone set never exceeds [`MAX_JOINTS`], for a material whose
/// full distinct bone count exceeded the cap (ADR 0097 §4 step 3).
///
/// **Triangle order heuristic**: triangles are first sorted by the lowest
/// original PMX bone index any of their three corners reference (ties keep
/// the original triangle order, so the sort is deterministic). PMX authors
/// number bones roughly in traversal order down each limb or part's chain
/// (a hair rig's bones are numbered consecutively, distinct from the body
/// rig's range), so nearby bone indices usually belong to the same physical
/// region. Sorting by lowest-referenced-bone-index keeps a single greedy
/// accumulation pass spatially coherent without the cost of computing a
/// mesh-wide bounding volume and a Morton/Z-order curve over triangle
/// centroids, which would need geometry the caller already has but this
/// function does not need to touch beyond index lookups.
///
/// **Grouping**: triangles are then visited in that order and accumulated
/// into the current group; a triangle that would push the group's distinct
/// bone count over [`MAX_JOINTS`] starts a new group instead. Every triangle
/// is visited exactly once, so no triangle is ever dropped, and no single
/// triangle can overflow a fresh group on its own (a triangle references at
/// most 12 distinct bones: 4 influences x 3 corners, well under the cap).
fn group_triangles_by_bone_locality(
    geometry: &PmxParsedGeometry,
    influences: &VertexInfluences,
    triangle_count: usize,
) -> Vec<Vec<usize>> {
    let mut triangle_bones: Vec<HashSet<usize>> = Vec::with_capacity(triangle_count);
    let mut sort_keys: Vec<(usize, usize)> = Vec::with_capacity(triangle_count);
    for triangle in 0..triangle_count {
        let mut bones = HashSet::new();
        for corner in 0..3 {
            if let Some(&vertex) = geometry.indices.get(triangle * 3 + corner)
                && let Some(influence) = influences.get(vertex as usize)
            {
                bones.extend(influence.iter().map(|&(bone, _)| bone));
            }
        }
        let sort_key = bones.iter().copied().min().unwrap_or(usize::MAX);
        sort_keys.push((sort_key, triangle));
        triangle_bones.push(bones);
    }

    let mut order: Vec<usize> = (0..triangle_count).collect();
    order.sort_by_key(|&triangle| sort_keys[triangle]);

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_bones: HashSet<usize> = HashSet::new();
    for triangle in order {
        let bones = &triangle_bones[triangle];
        let additional = bones.iter().filter(|bone| !current_bones.contains(*bone)).count();
        if !current.is_empty() && current_bones.len() + additional > MAX_JOINTS {
            groups.push(std::mem::take(&mut current));
            current_bones.clear();
        }
        current_bones.extend(bones.iter().copied());
        current.push(triangle);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

/// Extracts one render-part [`IrMesh`]/[`IrSkin`] pair from `triangles` of
/// `geometry` (a subset the caller chose so the pair's distinct bone count
/// never exceeds [`MAX_JOINTS`]), applying axis conversion (negate Z,
/// reverse triangle winding to match) and the [`PMX_TO_METERS`] scale.
///
/// Vertices are re-compacted from scratch (a PMX-local vertex may be shared
/// by triangles in different groups when a bone-count sub-split occurs; each
/// group gets its own copy of any vertex it touches, matching how any
/// triangle-partitioned mesh must duplicate seam data once submeshes become
/// independent draws). Per-vertex joint indices are remapped from the
/// PMX-global bone index into the position within this group's own
/// [`IrSkin::joint_nodes`] — this is the "skin index remapping" ADR 0097 §4
/// requires.
///
/// Returns the built mesh, the built skin, and the number of vertices whose
/// skin weights needed renormalizing (for the caller's aggregate diagnostic).
/// Everything one render part needs to become an [`IrMesh`]/[`IrSkin`] pair.
///
/// A struct rather than a parameter list because a group is described by
/// three parallel index spaces (PMX-global, split-mesh-local, and
/// group-local) and passing eight positional arguments would make it easy to
/// hand a function the wrong one.
struct GroupBuildInput<'a> {
    /// The whole parsed model, read only for original morph names.
    model: &'a PmxParsedModel,
    /// The per-material split this group belongs to.
    split_mesh: &'a mmd_anim_format::pmx::PmxMaterialSplitMesh,
    /// That material's morphs, already folded (see [`flatten_split_morphs`]).
    flattened_morphs: &'a [FlattenedMorph],
    /// Per-split-vertex bone influences.
    influences: &'a VertexInfluences,
    /// Split-local triangle indices belonging to this group.
    triangles: &'a [usize],
    /// Display name for the emitted mesh and skin.
    name: &'a str,
    /// The running sub-asset selector this group takes.
    group_id: usize,
    /// Bone rest positions in engine space, indexed by PMX bone index.
    world_positions: &'a [Vec3],
}

fn build_group(input: GroupBuildInput<'_>) -> (IrMesh, IrSkin, usize) {
    let GroupBuildInput {
        model,
        split_mesh,
        flattened_morphs,
        influences,
        triangles,
        name,
        group_id,
        world_positions,
    } = input;
    let geometry = &split_mesh.geometry;
    let material_index = split_mesh.original_material_index;
    let mut joint_position: HashMap<usize, u16> = HashMap::new();
    let mut joint_nodes: Vec<usize> = Vec::new();
    {
        let mut bone_set: HashSet<usize> = HashSet::new();
        for &triangle in triangles {
            for corner in 0..3 {
                if let Some(&vertex) = geometry.indices.get(triangle * 3 + corner)
                    && let Some(influence) = influences.get(vertex as usize)
                {
                    bone_set.extend(influence.iter().map(|&(bone, _)| bone));
                }
            }
        }
        let mut sorted_bones: Vec<usize> = bone_set.into_iter().collect();
        sorted_bones.sort_unstable();
        for bone in sorted_bones {
            joint_position.insert(bone, joint_nodes.len() as u16);
            joint_nodes.push(bone);
        }
    }

    let mut vertex_remap: HashMap<u32, u32> = HashMap::new();
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut skinning: Vec<SkinningVertexData> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut renormalized = 0usize;

    for &triangle in triangles {
        let Some(corners) = read_triangle_corners(geometry, triangle) else {
            continue;
        };

        let mut local = [0u32; 3];
        for (slot, &original) in corners.iter().enumerate() {
            local[slot] = *vertex_remap.entry(original).or_insert_with(|| {
                let index = original as usize;
                let position = read_vec3(&geometry.positions, index, [0.0, 0.0, 0.0]);
                let normal = read_vec3(&geometry.normals, index, [0.0, 1.0, 0.0]);
                let uv = read_vec2(&geometry.uvs, index, [0.0, 0.0]);

                let new_index = vertices.len() as u32;
                vertices.push(Vertex {
                    position: [
                        position[0] * PMX_TO_METERS,
                        position[1] * PMX_TO_METERS,
                        -position[2] * PMX_TO_METERS,
                    ],
                    normal: [normal[0], normal[1], -normal[2]],
                    color: [1.0, 1.0, 1.0],
                    uv,
                    outline_scale: geometry.edge_scale.get(index).copied().unwrap_or(1.0),
                    additional_uv: geometry
                        .additional_uvs
                        .first()
                        .and_then(|values| values.get(index * 4..index * 4 + 2))
                        .map(|values| [values[0], values[1]])
                        .unwrap_or([0.0; 2]),
                });

                let mut joints = [0u16; 4];
                let mut weights = [0.0f32; 4];
                let vertex_influences = influences.get(index).map(Vec::as_slice).unwrap_or(&[]);
                for (slot, &(bone, weight)) in vertex_influences.iter().take(4).enumerate() {
                    joints[slot] = joint_position.get(&bone).copied().unwrap_or(0);
                    weights[slot] = weight;
                }
                let sum: f32 = weights.iter().sum();
                if sum > f32::EPSILON && (sum - 1.0).abs() > WEIGHT_SUM_TOLERANCE {
                    for weight in &mut weights {
                        *weight /= sum;
                    }
                    renormalized += 1;
                }
                skinning.push(SkinningVertexData { joints, weights });
                new_index
            });
        }

        // Negating Z (module normalization contract) flips handedness;
        // swapping the last two corners keeps the triangle front-facing
        // under the engine's right-handed winding rule.
        indices.push(local[0]);
        indices.push(local[2]);
        indices.push(local[1]);
    }

    let inverse_bind_matrices: Vec<Mat4> = joint_nodes
        .iter()
        .map(|&bone| {
            let world = world_positions.get(bone).copied().unwrap_or(Vec3::ZERO);
            Mat4::from_translation(-world)
        })
        .collect();

    let mesh = IrMesh {
        source_index: group_id,
        name: name.to_owned(),
        mesh: Mesh {
            vertices,
            indices: Some(indices),
            skinning: Some(skinning),
            tangents: None,
            submeshes: Vec::new(),
        },
        submesh_materials: vec![Some(material_index)],
        morph_targets: build_group_morph_targets(
            split_mesh,
            model,
            flattened_morphs,
            &vertex_remap,
        ),
    };
    let skin = IrSkin {
        source_index: group_id,
        name: name.to_owned(),
        joint_nodes,
        inverse_bind_matrices,
    };

    (mesh, skin, renormalized)
}

fn read_triangle_corners(geometry: &PmxParsedGeometry, triangle: usize) -> Option<[u32; 3]> {
    let base = triangle * 3;
    Some([
        *geometry.indices.get(base)?,
        *geometry.indices.get(base + 1)?,
        *geometry.indices.get(base + 2)?,
    ])
}

fn read_vec3(values: &[f32], index: usize, default: [f32; 3]) -> [f32; 3] {
    let start = index * 3;
    match values.get(start..start + 3) {
        Some(slice) => [slice[0], slice[1], slice[2]],
        None => default,
    }
}

fn read_vec2(values: &[f32], index: usize, default: [f32; 2]) -> [f32; 2] {
    let start = index * 2;
    match values.get(start..start + 2) {
        Some(slice) => [slice[0], slice[1]],
        None => default,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::asset::{imported_sub_asset_id, ImportedSubAssetKind};
    use mmd_anim_format::pmx::{
        PmxParsedAppendTransform, PmxParsedBoneFlags, PmxParsedCounts, PmxParsedGroupMorphOffset,
        PmxParsedIndexSizes, PmxParsedJoint, PmxParsedMaterialFlags, PmxParsedMaterialMorphOffset,
        PmxParsedMetadata, PmxParsedMorph, PmxParsedQdef, PmxParsedSdef, PmxParsedSkeleton,
        PmxParsedVertexMorphOffset,
    };

    #[test]
    fn conventional_pmx_skin_names_receive_restrained_internal_outlines() {
        assert_eq!(pmx_skin_internal_boundary_strength("body01"), 0.55);
        assert_eq!(pmx_skin_internal_boundary_strength("SKIN"), 0.55);
        assert_eq!(pmx_skin_internal_boundary_strength("肌"), 0.55);
        assert_eq!(pmx_skin_internal_boundary_strength("hair01"), 0.0);
        assert_eq!(pmx_skin_internal_boundary_strength("clothes"), 0.0);
    }

    #[test]
    fn import_invalid_bytes_returns_error() {
        let id = AssetId::generate();
        assert!(
            import_pmx_bytes(&id, b"not a valid pmx document", &[]).is_err(),
            "garbage bytes must produce a parse error"
        );
    }

    #[test]
    fn pmx_physics_euler_rotation_matches_bullet_zyx_order() {
        let euler = [0.37, -0.29, 0.61];
        let expected = convert_rotation(
            Quat::from_euler(glam::EulerRot::ZYX, euler[2], euler[1], euler[0]).to_array(),
        );
        let actual = convert_euler_rotation(euler);

        assert!(
            actual.dot(expected).abs() > 1.0 - 1.0e-6,
            "PMX physics Euler angles must use Bullet's ZYX composition: got {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn sub_asset_ids_are_deterministic_across_repeated_imports() {
        let id = AssetId::generate();
        let bytes = skinned_pmx_fixture();
        let first = import_pmx_bytes(&id, &bytes, &[])
            .expect("fixture must parse")
            .imported_sub_assets();
        let second = import_pmx_bytes(&id, &bytes, &[])
            .expect("fixture must parse")
            .imported_sub_assets();
        assert_eq!(
            first, second,
            "re-importing identical bytes must produce identical sub-asset IDs"
        );
    }

    #[test]
    fn parse_pmx_extracts_a_normalized_bone_translation() {
        let bytes = skinned_pmx_fixture();
        let document = parse_pmx(&bytes).expect("fixture must parse");

        let root = document
            .nodes
            .iter()
            .find(|node| node.name == "root")
            .expect("root bone must be present");
        assert_eq!(root.parent, None);
        assert!((root.translation - Vec3::ZERO).length() < 1.0e-5);

        let (child_index, child) = document
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.name == "child")
            .expect("child bone must be present");
        let root_index = document
            .nodes
            .iter()
            .position(|node| node.name == "root")
            .expect("root index");
        assert_eq!(child.parent, Some(root_index));
        // The fixture's child bone sits at PMX-space (0, 1, 2): axis
        // conversion negates Z, then PMX_TO_METERS scales the whole vector.
        let expected = Vec3::new(0.0, 1.0, -2.0) * PMX_TO_METERS;
        assert!(
            (child.translation - expected).length() < 1.0e-5,
            "got {:?}, expected {expected:?}",
            child.translation
        );
        assert_eq!(child.rotation, Quat::IDENTITY);
        assert_eq!(child.scale, Vec3::ONE);
        let _ = child_index;
    }

    #[test]
    fn parse_pmx_converts_axes_and_reverses_winding() {
        let bytes = skinned_pmx_fixture();
        let document = parse_pmx(&bytes).expect("fixture must parse");
        let mesh = &document.meshes[0].mesh;

        // The fixture authors v0=(0,0,0), v1=(1,0,1), v2=(0,1,1) for its
        // first triangle; Z must be negated and PMX_TO_METERS applied.
        let expected_v0 = [0.0, 0.0, 0.0];
        let expected_v1 = [PMX_TO_METERS, 0.0, -PMX_TO_METERS];
        assert!(vec3_close(mesh.vertices[0].position, expected_v0));
        assert!(vec3_close(mesh.vertices[1].position, expected_v1));
        // Normals authored as (0, 0, 1) must also have Z negated.
        assert!(vec3_close(mesh.vertices[0].normal, [0.0, 0.0, -1.0]));
        // UVs keep the engine's top-left origin unchanged (no flip, unlike
        // the FBX importer's Maya-origin flip).
        assert_eq!(mesh.vertices[0].uv, [0.0, 0.0]);
        assert_eq!(mesh.vertices[1].uv, [1.0, 0.0]);

        // Winding: the source triangle is (0, 1, 2); after the Z flip the
        // last two corners swap so the triangle stays front-facing.
        assert_eq!(mesh.indices.as_deref(), Some([0u32, 2, 1].as_slice()));
    }

    #[test]
    fn parse_pmx_remaps_skin_indices_to_the_skins_own_joint_order() {
        let bytes = skinned_pmx_fixture();
        let document = parse_pmx(&bytes).expect("fixture must parse");

        // Every vertex in the fixture is fully weighted to the "child" bone
        // alone, so that mesh's skin carries exactly one joint.
        let skin = &document.skins[0];
        assert_eq!(skin.joint_nodes.len(), 1, "only the referenced bone is kept");
        let child_node = document
            .nodes
            .iter()
            .position(|node| node.name == "child")
            .expect("child node index");
        assert_eq!(skin.joint_nodes[0], child_node);

        let skinning = document.meshes[0]
            .mesh
            .skinning
            .as_ref()
            .expect("mesh must carry skinning data");
        for vertex in skinning {
            assert_eq!(
                vertex.joints[0], 0,
                "the single joint must remap to position 0 within its own skin"
            );
            assert!((vertex.weights[0] - 1.0).abs() < 1.0e-5);
        }
    }

    #[test]
    fn parse_pmx_preserves_toon_and_sphere_material_semantics() {
        let bytes = skinned_pmx_fixture();
        let document = parse_pmx(&bytes).expect("fixture must parse");

        assert!(!document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "pmx.toon_shading_unsupported"));
        assert_eq!(document.materials.len(), 2);
        assert_eq!(document.materials[0].shading_model, MaterialShadingModel::ToonLit);
        assert_eq!(document.materials[1].shading_model, MaterialShadingModel::ToonLit);
    }

    fn shared_toon_material(name: &str, shared_toon_index: u8) -> PmxParsedMaterial {
        PmxParsedMaterial {
            name: name.to_owned(),
            english_name: name.to_owned(),
            texture_path: String::new(),
            sphere_texture_path: String::new(),
            sphere_mode: "none".to_owned(),
            toon_texture_path: String::new(),
            shared_toon_index: Some(shared_toon_index),
            diffuse: [1.0; 4],
            specular: [0.0; 3],
            specular_power: 1.0,
            ambient: [0.0; 3],
            edge_color: [0.0, 0.0, 0.0, 1.0],
            edge_size: 1.0,
            flags: no_material_flags(false),
            face_count: 0,
        }
    }

    #[test]
    fn append_shared_toon_textures_deduplicates_by_index() {
        let materials = vec![
            shared_toon_material("skin", 2),
            shared_toon_material("clothes", 2),
            shared_toon_material("hair", 5),
        ];
        let mut textures = Vec::new();
        let selector_of = append_shared_toon_textures(&materials, &mut textures, 10);

        // Two distinct indices were referenced, so exactly two synthetic
        // textures are appended even though three materials asked for one.
        assert_eq!(textures.len(), 2);
        assert_eq!(selector_of[&2], 10 + 2);
        assert_eq!(selector_of[&5], 10 + 5);
        assert!(textures.iter().any(|texture| texture.source_index == 12));
        assert!(textures.iter().any(|texture| texture.source_index == 15));
    }

    #[test]
    fn synthesized_shared_toon_ramp_stores_srgb_encoded_color_bytes() {
        assert_eq!(linear_to_srgb_u8(0.5), 188);

        let texture = builtin_toon_texture(2, 0);
        let bottom = &texture.rgba8[texture.rgba8.len() - 4..];
        assert_eq!(bottom, &[196, 206, 229, 255]);
    }

    #[test]
    fn parse_materials_resolves_shared_toon_index_to_a_synthesized_ramp() {
        let materials = vec![shared_toon_material("skin", 3)];
        let mut textures = Vec::new();
        let selector_of = append_shared_toon_textures(&materials, &mut textures, 0);
        let mut diagnostics = Vec::new();

        let ir_materials = parse_materials(
            &materials,
            &HashMap::new(),
            &[],
            &selector_of,
            &mut diagnostics,
        );

        assert_eq!(ir_materials[0].toon_ramp_texture, Some(selector_of[&3]));
    }

    fn opaque_toon_material(source_index: usize, base_color_texture: Option<usize>) -> IrMaterial {
        IrMaterial {
            source_index,
            name: format!("material_{source_index}"),
            base_color: LinearRgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
            base_color_texture,
            normal_texture: None,
            metallic_roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            emissive_color: LinearRgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            roughness: 0.5,
            metallic: 0.0,
            alpha_mode: MaterialAlphaMode::Opaque,
            alpha_cutoff: 0.5,
            cull_mode: MaterialCullMode::Back,
            shading_model: MaterialShadingModel::ToonLit,
            toon_ramp_texture: None,
            toon_shadow_color: LinearRgba { r: 0.55, g: 0.55, b: 0.62, a: 1.0 },
            toon_ambient_color: LinearRgba { r: 0.5, g: 0.5, b: 0.5, a: 1.0 },
            toon_specular_color: LinearRgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            toon_specular_power: 1.0,
            sphere_texture: None,
            sphere_blend: MaterialSphereBlendMode::Multiply,
            sphere_coordinates: MaterialSphereCoordinateSource::ViewNormal,
            outline: MaterialOutline {
                enabled: false,
                color: LinearRgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
                width: 0.0,
                internal_boundary_strength: 0.0,
            },
            cast_shadow: true,
            receive_shadow: true,
        }
    }

    fn single_material_mesh(source_index: usize, material_index: usize, uvs: &[[f32; 2]]) -> IrMesh {
        let vertices = uvs
            .iter()
            .map(|&uv| Vertex {
                position: [0.0; 3],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                uv,
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            })
            .collect();
        IrMesh {
            source_index,
            name: format!("mesh_{source_index}"),
            mesh: Mesh {
                vertices,
                indices: None,
                skinning: None,
                tangents: None,
                submeshes: Vec::new(),
            },
            submesh_materials: vec![Some(material_index)],
            morph_targets: Vec::new(),
        }
    }

    /// A 2x1 atlas: x=0 is opaque, x=1 is fully transparent. This models a
    /// shared PMX texture atlas where only part of the image is actually used
    /// by any given material.
    fn cutout_atlas() -> IrTexture {
        IrTexture {
            source_index: 0,
            name: "atlas".to_owned(),
            width: 2,
            height: 1,
            rgba8: vec![255, 255, 255, 255, /* opaque */ 0, 0, 0, 0 /* transparent */],
        }
    }

    #[test]
    fn refine_alpha_modes_only_promotes_materials_whose_own_uvs_hit_transparency() {
        let texture = cutout_atlas();

        let mut materials = vec![
            opaque_toon_material(0, Some(0)),
            opaque_toon_material(1, Some(0)),
        ];
        let meshes = vec![
            // Material 0's own vertices only ever sample the opaque texel.
            single_material_mesh(0, 0, &[[0.0, 0.0], [0.0, 0.0]]),
            // Material 1's own vertices sample the transparent texel.
            single_material_mesh(1, 1, &[[1.0, 0.0]]),
        ];

        refine_alpha_modes_from_sampled_texels(&mut materials, &meshes, &[texture]);

        assert_eq!(materials[0].alpha_mode, MaterialAlphaMode::Opaque);
        assert_eq!(materials[1].alpha_mode, MaterialAlphaMode::Blend);
    }

    #[test]
    fn refine_alpha_modes_masks_a_material_whose_transparency_is_a_cutout() {
        // A garment whose only transparent texels are a hem frill must
        // alpha-test rather than blend. `Blend` drops depth writes, so the
        // surface stops occluding anything drawn later in the frame — which
        // is how the editor's floor grid showed through a character's blouse.
        let texture = cutout_atlas();

        let mut materials = vec![opaque_toon_material(0, Some(0))];
        let mut uvs = vec![[0.0, 0.0]; 9];
        uvs.push([1.0, 0.0]);
        let meshes = vec![single_material_mesh(0, 0, &uvs)];

        refine_alpha_modes_from_sampled_texels(&mut materials, &meshes, &[texture]);

        assert_eq!(materials[0].alpha_mode, MaterialAlphaMode::Mask);
    }

    #[test]
    fn refine_alpha_modes_blends_a_material_that_is_mostly_transparent() {
        // A lace overlay, an ambient-occlusion layer, or a hair-shadow plane
        // samples non-opaque texels across most of its own surface, and is
        // translucent rather than a cutout.
        let texture = cutout_atlas();

        let mut materials = vec![opaque_toon_material(0, Some(0))];
        let mut uvs = vec![[1.0, 0.0]; 3];
        uvs.push([0.0, 0.0]);
        let meshes = vec![single_material_mesh(0, 0, &uvs)];

        refine_alpha_modes_from_sampled_texels(&mut materials, &meshes, &[texture]);

        assert_eq!(materials[0].alpha_mode, MaterialAlphaMode::Blend);
    }

    #[test]
    fn refine_alpha_modes_counts_every_submesh_sharing_one_material() {
        // The same material reached through two submeshes must land in one
        // mode from their combined coverage, not from whichever mesh the
        // importer happened to visit last.
        let texture = cutout_atlas();

        let mut materials = vec![opaque_toon_material(0, Some(0))];
        let meshes = vec![
            single_material_mesh(0, 0, &[[1.0, 0.0]]),
            single_material_mesh(1, 0, &[[0.0, 0.0]; 9]),
        ];

        refine_alpha_modes_from_sampled_texels(&mut materials, &meshes, &[texture]);

        assert_eq!(materials[0].alpha_mode, MaterialAlphaMode::Mask);
    }

    // -----------------------------------------------------------------
    // Morph targets (ADR 0097 §5)
    // -----------------------------------------------------------------

    #[test]
    fn parse_pmx_converts_morph_deltas_to_engine_space() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let target = morph_target(&document, "smile").expect("the vertex morph must survive");

        assert_eq!(target.vertex_deltas.len(), 1);
        // The fixture's delta is PMX (0, 1, 2); a delta is a difference of
        // positions, so it takes the same Z negation and metre scale as any
        // position does.
        let expected = Vec3::new(0.0, 1.0, -2.0) * PMX_TO_METERS;
        assert!(
            (target.vertex_deltas[0].1 - expected).length() < 1.0e-6,
            "got {:?}, expected {expected:?}",
            target.vertex_deltas[0].1
        );
        assert!(target.material_offsets.is_empty());
    }

    #[test]
    fn parse_pmx_flattens_group_morphs_into_their_referents_deltas() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let smile = morph_target(&document, "smile").expect("the vertex morph must survive");
        let half = morph_target(&document, "half_smile").expect("the group morph must survive");

        // The group references `smile` at half weight, so it must come out as
        // its own plain delta list — no runtime "group" concept exists.
        assert_eq!(half.vertex_deltas.len(), 1);
        assert_eq!(half.vertex_deltas[0].0, smile.vertex_deltas[0].0);
        let expected = smile.vertex_deltas[0].1 * 0.5;
        assert!(
            (half.vertex_deltas[0].1 - expected).length() < 1.0e-6,
            "got {:?}, expected {expected:?}",
            half.vertex_deltas[0].1
        );
    }

    #[test]
    fn parse_pmx_captures_material_morphs_against_their_own_material() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let blush = morph_target(&document, "blush").expect("the material morph must survive");

        assert!(blush.vertex_deltas.is_empty());
        assert_eq!(blush.material_offsets.len(), 1);
        let offset = &blush.material_offsets[0];
        assert_eq!(offset.material_index, 0);
        assert_eq!(offset.operation, IrMaterialMorphOperation::Multiply);
        assert!((offset.base_color.g - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn morph_sub_asset_ids_are_stable_and_distinct_per_render_part() {
        let source = AssetId::generate();
        let bytes = skinned_pmx_fixture();
        let first = import_pmx_bytes(&source, &bytes, &[]).expect("fixture must import");
        let second = import_pmx_bytes(&source, &bytes, &[]).expect("fixture must import");

        let morph_ids = |result: &crate::model_import::GltfImportResult| {
            result
                .imported_sub_assets()
                .into_iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::Morph)
                .map(|asset| (asset.id, asset.index))
                .collect::<Vec<_>>()
        };
        let ids = morph_ids(&first);
        assert!(!ids.is_empty(), "the fixture declares morphs");
        assert_eq!(ids, morph_ids(&second), "morph IDs must be reproducible");

        // The catalog's index must be the selector the ID derives from;
        // a mismatch would orphan an author's binding on reimport.
        for mesh in &first.meshes {
            for morph in &mesh.morphs {
                assert!(
                    ids.iter()
                        .any(|(id, index)| *id == morph.id.as_str() && *index as usize == morph.source_index),
                    "morph `{}` is missing from the catalog under its own selector",
                    morph.name
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Secondary Motion import hints (ADR 0097 §6, ADR 0112)
    // -----------------------------------------------------------------

    #[test]
    fn parser_dynamic_bone_mode_maps_to_dynamic_with_bone_position() {
        let mut unsupported = 0;
        assert_eq!(
            convert_rigid_body_mode("dynamicBone", &mut unsupported),
            IrRigidBodyMode::DynamicWithBonePosition
        );
        assert_eq!(unsupported, 0);
    }

    #[test]
    fn invalid_secondary_motion_hints_are_counted_without_failing_import() {
        let mut invalid_bones = 0;
        assert_eq!(
            resolve_rigid_body_bone_index(-1, 2, &mut invalid_bones),
            None,
            "PMX -1 is the valid unbound-body sentinel"
        );
        assert_eq!(resolve_rigid_body_bone_index(1, 2, &mut invalid_bones), Some(1));
        assert_eq!(resolve_rigid_body_bone_index(5, 2, &mut invalid_bones), None);
        assert_eq!(invalid_bones, 1);

        let mut unsupported_modes = 0;
        assert_eq!(
            convert_rigid_body_mode("future_mode", &mut unsupported_modes),
            IrRigidBodyMode::FollowBone
        );
        assert_eq!(unsupported_modes, 1);

        let mut diagnostics = Vec::new();
        report_unsupported_soft_bodies(2, &mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "pmx.soft_body_unsupported");
    }

    #[test]
    fn unsupported_joint_kind_is_omitted_with_diagnostic() {
        let mut model = empty_pmx_model();
        model.rigid_bodies = fixture_rigid_bodies();
        for body in &mut model.rigid_bodies {
            body.bone_index = -1;
        }
        model.joints = fixture_joints();
        model.joints[0].kind = "hinge".to_owned();
        let mut diagnostics = Vec::new();

        let rig = parse_rigid_body_rig(&model, &[], &mut diagnostics)
            .expect("best-effort conversion must retain supported rigid bodies");

        assert_eq!(rig.bodies.len(), 2);
        assert!(rig.joints.is_empty());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "pmx.joint_kind_unsupported"));
    }

    #[test]
    fn orphaned_pmx_joints_report_diagnostic_instead_of_failing_import() {
        let mut model = empty_pmx_model();
        model.joints = fixture_joints();
        let mut diagnostics = Vec::new();

        assert!(parse_rigid_body_rig(&model, &[], &mut diagnostics).is_none());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "pmx.joint_without_bodies"));
    }

    #[test]
    fn parse_pmx_captures_rigid_bodies_relative_to_their_bone() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let rig = document.rigid_body_rig.expect("the fixture declares a rig");

        assert_eq!(rig.bodies.len(), 2);
        let anchor = &rig.bodies[0];
        assert_eq!(anchor.mode, IrRigidBodyMode::FollowBone);
        assert_eq!(anchor.bone_node, Some(1));
        // The fixture's bone rests at PMX (0, 1, 2) and its anchor body sits
        // at (0, 1, 4): two PMX units further along +Z, which becomes -Z in
        // engine space.
        let expected = Vec3::new(0.0, 0.0, -2.0) * PMX_TO_METERS;
        assert!(
            (anchor.bone_offset_translation - expected).length() < 1.0e-6,
            "got {:?}, expected {expected:?}",
            anchor.bone_offset_translation
        );
        // A body sitting exactly on its bone has no offset at all.
        let hair = &rig.bodies[1];
        assert_eq!(hair.mode, IrRigidBodyMode::Dynamic);
        assert!(hair.bone_offset_translation.length() < 1.0e-6);
    }

    #[test]
    fn parse_pmx_preserves_collide_with_groups() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let rig = document.rigid_body_rig.expect("the fixture declares a rig");

        // The parser dependency already exposes PMX masks as collide-with
        // groups, so importing must not invert the bits a second time.
        assert_eq!(rig.bodies[0].collides_with, 0xFFFF);
        assert_eq!(rig.bodies[1].collides_with, 0xFFFE);
    }

    #[test]
    fn parse_pmx_converts_rigid_body_shapes_to_meters() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let rig = document.rigid_body_rig.expect("the fixture declares a rig");

        assert_eq!(
            rig.bodies[0].shape,
            IrRigidBodyShape::Sphere {
                radius: 0.5 * PMX_TO_METERS
            }
        );
        // PMX stores a capsule's full length; the IR stores the half-height
        // of its cylindrical section.
        assert_eq!(
            rig.bodies[1].shape,
            IrRigidBodyShape::Capsule {
                radius: 0.25 * PMX_TO_METERS,
                half_height: 1.0 * PMX_TO_METERS,
            }
        );
    }

    #[test]
    fn parse_pmx_swaps_joint_limits_on_the_flipped_axis() {
        let document = parse_pmx(&skinned_pmx_fixture()).expect("fixture must parse");
        let rig = document.rigid_body_rig.expect("the fixture declares a rig");
        let joint = &rig.joints[0];

        assert_eq!(joint.body_a, Some(0));
        assert_eq!(joint.body_b, Some(1));
        // Z is negated, so the source's upper Z limit becomes the engine's
        // lower one; a limit range that came out inverted would let a body
        // travel where its author forbade it.
        assert!((joint.translation_lower.z - (-3.0 * PMX_TO_METERS)).abs() < 1.0e-6);
        assert!((joint.translation_upper.z - (1.0 * PMX_TO_METERS)).abs() < 1.0e-6);
        // Rotations about X and Y reverse sense, so their limits swap too.
        assert!((joint.rotation_lower.x - (-0.5)).abs() < 1.0e-6);
        assert!((joint.rotation_upper.x - 0.5).abs() < 1.0e-6);
        // Stiffnesses are magnitudes: neither scaled nor signed.
        assert_eq!(joint.spring_rotation, Vec3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn the_rig_sub_asset_binds_the_documents_shared_skeleton() {
        let source = AssetId::generate();
        let imported =
            import_pmx_bytes(&source, &skinned_pmx_fixture(), &[]).expect("fixture must import");
        let rig = imported
            .rigid_body_rig
            .as_ref()
            .expect("the fixture declares a rig");
        let skeleton = &imported.skins[0].skeleton;

        assert_eq!(rig.skeleton.as_ref(), Some(&skeleton.id));
        assert_eq!(rig.skeleton_identity, Some(skeleton.identity));
        assert_eq!(rig.dynamic_body_count(), 1);
        // Bodies must resolve to real BoneIds, not just carry a name.
        let child = skeleton
            .bones
            .iter()
            .find(|bone| bone.name == "child")
            .expect("fixture skeleton must contain the weighted bone");
        assert_eq!(rig.bodies[0].bone, Some(child.id));
        assert_eq!(rig.bodies[0].bone_name, "child");
        // And the rig must appear in the catalog exactly once.
        assert_eq!(
            imported
                .imported_sub_assets()
                .iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::SecondaryMotionRig)
                .count(),
            1
        );
    }

    fn morph_target<'a>(
        document: &'a ModelDocument,
        name: &str,
    ) -> Option<&'a crate::model_ir::IrMorphTarget> {
        document
            .meshes
            .iter()
            .flat_map(|mesh| mesh.morph_targets.iter())
            .find(|target| target.name == name)
    }

    #[test]
    fn import_pmx_bytes_produces_the_same_catalog_as_parse_then_build() {
        let source = AssetId::generate();
        let bytes = skinned_pmx_fixture();
        let document = parse_pmx(&bytes).expect("fixture must parse");
        assert!(
            matches!(
                document.skeleton_scope,
                SkeletonScope::SharedAcrossDocument { .. }
            ),
            "PMX must opt into one document-wide skeleton (ADR 0097 §4a)"
        );
        if let SkeletonScope::SharedAcrossDocument { skeleton_nodes } = &document.skeleton_scope {
            assert_eq!(
                skeleton_nodes,
                &vec![0usize, 1, 2],
                "skeleton_nodes must be exactly the three PMX bones, excluding the two \
                 per-split-part mesh anchor nodes appended after them"
            );
        }
        let direct = crate::model_import::build_import_result(&document, &source, &[]);
        let via_entry_point =
            import_pmx_bytes(&source, &bytes, &[]).expect("entry point import");
        assert_eq!(
            direct.imported_sub_assets(),
            via_entry_point.imported_sub_assets()
        );
        assert_eq!(direct.meshes.len(), 2, "the fixture has two materials");
        assert_eq!(
            direct.meshes[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Mesh, 0)
        );
        assert_eq!(
            direct.meshes[1].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Mesh, 1)
        );

        // Both materials' skins bind to the same shared skeleton, spanning
        // every PMX bone (including the "unused" bone that no vertex
        // weights), not a separate skeleton per material (ADR 0097 §4a).
        assert_eq!(direct.skins.len(), 2, "the fixture has two materials/skins");
        assert_eq!(
            direct.skins[0].skeleton.id, direct.skins[1].skeleton.id,
            "both split skins must bind to the same shared skeleton"
        );
        assert_eq!(
            direct.skins[0].skeleton.bones.len(),
            3,
            "the shared skeleton must cover exactly the three PMX bones (including 'unused', \
             which no skin weights vertices to), and must NOT include the two split-part mesh \
             anchor nodes: including those would make skeleton identity (ADR 0077 §4) depend on \
             the multi-skin split outcome instead of the rig itself"
        );
        assert_eq!(
            direct
                .imported_sub_assets()
                .iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::Skeleton)
                .count(),
            1,
            "a shared skeleton must contribute exactly one catalog entry even with two split skins"
        );
        assert_eq!(
            direct.skeleton_records.len(),
            1,
            "a shared skeleton must contribute exactly one skeleton record"
        );
    }

    // -----------------------------------------------------------------------
    // Multi-skin split (ADR 0097 §4)
    // -----------------------------------------------------------------------

    #[test]
    fn a_material_exceeding_max_joints_splits_into_multiple_capped_skins() {
        let triangle_count = 60;
        let geometry = many_bone_geometry(triangle_count);

        let vertex_count = geometry.positions.len() / 3;
        let influences = collect_influences(&geometry, vertex_count);
        let mut distinct_bones = HashSet::new();
        for influence in &influences {
            distinct_bones.extend(influence.iter().map(|&(bone, _)| bone));
        }
        assert!(
            distinct_bones.len() > MAX_JOINTS,
            "fixture must exceed the render skin cap to exercise the sub-split"
        );

        let groups = group_triangles_by_bone_locality(&geometry, &influences, triangle_count);
        assert!(
            groups.len() > 1,
            "an oversized material must split into more than one skin"
        );

        let world_positions = vec![Vec3::ZERO; vertex_count];
        let mut seen_triangles: HashSet<usize> = HashSet::new();
        for (group_index, triangles) in groups.iter().enumerate() {
            let model = empty_pmx_model();
            let split_mesh = single_material_split_mesh(geometry.clone());
            let (mesh, skin, renormalized) = build_group(GroupBuildInput {
                model: &model,
                split_mesh: &split_mesh,
                flattened_morphs: &[],
                influences: &influences,
                triangles,
                name: "part",
                group_id: group_index,
                world_positions: &world_positions,
            });
            assert!(
                skin.joint_nodes.len() <= MAX_JOINTS,
                "group {group_index} has {} joints, exceeding {MAX_JOINTS}",
                skin.joint_nodes.len()
            );
            assert_eq!(renormalized, 0, "the fixture's weights already sum to 1.0");

            let skinning = mesh.mesh.skinning.expect("group must carry skinning data");
            for vertex in &skinning {
                let joint = vertex.joints[0] as usize;
                assert!(
                    joint < skin.joint_nodes.len(),
                    "vertex joint {joint} is out of range for a skin with {} joints",
                    skin.joint_nodes.len()
                );
                assert!((vertex.weights[0] - 1.0).abs() < 1.0e-5);
            }

            for &triangle in triangles {
                assert!(
                    seen_triangles.insert(triangle),
                    "triangle {triangle} must not be assigned to more than one group"
                );
            }
        }
        assert_eq!(
            seen_triangles.len(),
            triangle_count,
            "every triangle must be assigned to exactly one group"
        );
    }

    /// Builds a synthetic single-material geometry whose `triangle_count`
    /// Wraps bare geometry as a one-material split mesh with no morphs, for
    /// tests that exercise the group builder without going through
    /// `split_pmx_model_by_material`.
    fn single_material_split_mesh(
        geometry: PmxParsedGeometry,
    ) -> mmd_anim_format::pmx::PmxMaterialSplitMesh {
        let vertex_count = (geometry.positions.len() / 3) as u32;
        mmd_anim_format::pmx::PmxMaterialSplitMesh {
            original_material_index: 0,
            original_vertex_indices: (0..vertex_count).collect(),
            geometry,
            material: PmxParsedMaterial {
                name: "part".to_owned(),
                english_name: "part".to_owned(),
                texture_path: String::new(),
                sphere_texture_path: String::new(),
                sphere_mode: "none".to_owned(),
                toon_texture_path: String::new(),
                shared_toon_index: None,
                diffuse: [1.0; 4],
                specular: [0.0; 3],
                specular_power: 1.0,
                ambient: [0.0; 3],
                edge_color: [0.0, 0.0, 0.0, 1.0],
                edge_size: 1.0,
                flags: no_material_flags(false),
                face_count: 0,
            },
            morphs: Vec::new(),
            morph_index_map: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// A model carrying nothing but the fields the group builder reads
    /// (morph names), for tests that supply no morphs.
    fn empty_pmx_model() -> PmxParsedModel {
        PmxParsedModel {
            metadata: PmxParsedMetadata {
                format: "pmx".to_owned(),
                version: 2.0,
                encoding: "utf-8".to_owned(),
                name: "empty".to_owned(),
                english_name: "empty".to_owned(),
                comment: String::new(),
                english_comment: String::new(),
                counts: PmxParsedCounts {
                    vertices: 0,
                    faces: 0,
                    materials: 0,
                    bones: 0,
                    morphs: 0,
                    display_frames: 0,
                    rigid_bodies: 2,
                    joints: 1,
                    soft_bodies: 0,
                },
                index_sizes: PmxParsedIndexSizes {
                    vertex: 4,
                    texture: 4,
                    material: 4,
                    bone: 4,
                    morph: 4,
                    rigid_body: 4,
                },
                additional_uv_count: 0,
            },
            geometry: empty_geometry(),
            materials: Vec::new(),
            skeleton: PmxParsedSkeleton { bones: Vec::new() },
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: Vec::new(),
            joints: Vec::new(),
            soft_bodies: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn empty_geometry() -> PmxParsedGeometry {
        PmxParsedGeometry {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            additional_uvs: Vec::new(),
            indices: Vec::new(),
            skin_indices: Vec::new(),
            skin_weights: Vec::new(),
            edge_scale: Vec::new(),
            material_groups: Vec::new(),
            sdef: PmxParsedSdef::default(),
            qdef: PmxParsedQdef::default(),
        }
    }

    /// triangles each reference three brand-new, never-reused bones, so the
    /// mesh's total distinct bone count (`triangle_count * 3`) comfortably
    /// exceeds [`MAX_JOINTS`] once `triangle_count` is large enough.
    fn many_bone_geometry(triangle_count: usize) -> PmxParsedGeometry {
        let vertex_count = triangle_count * 3;
        let mut positions = Vec::with_capacity(vertex_count * 3);
        let mut normals = Vec::with_capacity(vertex_count * 3);
        let mut uvs = Vec::with_capacity(vertex_count * 2);
        let mut skin_indices = Vec::with_capacity(vertex_count * 4);
        let mut skin_weights = Vec::with_capacity(vertex_count * 4);
        let mut indices = Vec::with_capacity(vertex_count);

        for vertex in 0..vertex_count {
            positions.extend_from_slice(&[0.0, 0.0, 0.0]);
            normals.extend_from_slice(&[0.0, 1.0, 0.0]);
            uvs.extend_from_slice(&[0.0, 0.0]);
            skin_indices.extend_from_slice(&[vertex as u32, 0, 0, 0]);
            skin_weights.extend_from_slice(&[1.0, 0.0, 0.0, 0.0]);
            indices.push(vertex as u32);
        }

        PmxParsedGeometry {
            positions,
            normals,
            uvs,
            additional_uvs: Vec::new(),
            indices,
            skin_indices,
            skin_weights,
            edge_scale: Vec::new(),
            material_groups: Vec::new(),
            sdef: PmxParsedSdef::default(),
            qdef: PmxParsedQdef::default(),
        }
    }

    fn vec3_close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1.0e-5)
    }

    fn no_bone_flags() -> PmxParsedBoneFlags {
        PmxParsedBoneFlags {
            indexed_tail: false,
            rotatable: true,
            translatable: true,
            visible: true,
            enabled: true,
            ik: false,
            append_local: false,
            append_rotate: false,
            append_translate: false,
            fixed_axis: false,
            local_axis: false,
            transform_after_physics: false,
            external_parent_transform: false,
        }
    }

    fn no_material_flags(double_sided: bool) -> PmxParsedMaterialFlags {
        PmxParsedMaterialFlags {
            double_sided,
            ground_shadow: true,
            self_shadow_map: false,
            self_shadow: false,
            edge: false,
            vertex_color: false,
            point_draw: false,
            line_draw: false,
        }
    }

    /// Builds a small, hand-written PMX document (two bones, two one
    /// -triangle materials, the second declaring a toon texture) and encodes
    /// it via `mmd_anim_format::pmx::export_pmx_model`, exactly like
    /// `fbx_import::tests::SKINNED_FBX` is a hand-written source document —
    /// except PMX is a binary format, so the crate's own exporter is used
    /// as the encoder instead of writing bytes by hand.
    ///
    /// Covers: header/metadata round-trip, a two-bone parent-child
    /// hierarchy, axis/winding conversion, and single-joint skin index
    /// remapping. The second material's `toon_texture_path` exercises the
    /// generic toon-ramp import path.
    pub(crate) fn skinned_pmx_fixture() -> Vec<u8> {
        let index_sizes = PmxParsedIndexSizes {
            vertex: 4,
            texture: 4,
            material: 4,
            bone: 4,
            morph: 4,
            rigid_body: 4,
        };

        let bones = vec![
            PmxParsedBone {
                name: "root".to_owned(),
                english_name: "root".to_owned(),
                parent_index: -1,
                layer: 0,
                position: [0.0, 0.0, 0.0],
                tail_index: -1,
                tail_position: None,
                flags: no_bone_flags(),
                append_transform: None,
                fixed_axis: None,
                local_axis: None,
                external_parent_key: None,
                ik: None,
            },
            PmxParsedBone {
                name: "child".to_owned(),
                english_name: "child".to_owned(),
                parent_index: 0,
                layer: 0,
                position: [0.0, 1.0, 2.0],
                tail_index: -1,
                tail_position: None,
                flags: no_bone_flags(),
                append_transform: None,
                fixed_axis: None,
                local_axis: None,
                external_parent_key: None,
                ik: None,
            },
            PmxParsedBone {
                name: "unused".to_owned(),
                english_name: "unused".to_owned(),
                parent_index: -1,
                layer: 0,
                position: [5.0, 5.0, 5.0],
                tail_index: -1,
                tail_position: None,
                flags: no_bone_flags(),
                append_transform: None,
                fixed_axis: None,
                local_axis: None,
                external_parent_key: None,
                ik: None,
            },
        ];
        // Bone-index remapping is unused per the module doc, but constructed
        // so `append_transform: None` never surprises a future reader.
        let _ = PmxParsedAppendTransform {
            parent_index: 0,
            weight: 0.0,
        };

        let positions = vec![
            0.0, 0.0, 0.0, // v0
            1.0, 0.0, 1.0, // v1
            0.0, 1.0, 1.0, // v2
            2.0, 0.0, 0.0, // v3
            3.0, 0.0, 1.0, // v4
            2.0, 1.0, 1.0, // v5
        ];
        let normals = vec![
            0.0, 0.0, 1.0, // v0
            0.0, 0.0, 1.0, // v1
            0.0, 0.0, 1.0, // v2
            0.0, 0.0, 1.0, // v3
            0.0, 0.0, 1.0, // v4
            0.0, 0.0, 1.0, // v5
        ];
        let uvs = vec![
            0.0, 0.0, // v0
            1.0, 0.0, // v1
            0.0, 1.0, // v2
            0.0, 0.0, // v3
            1.0, 0.0, // v4
            0.0, 1.0, // v5
        ];
        // Every vertex weighted entirely to bone index 1 ("child").
        let skin_indices = [1u32, 0, 0, 0].repeat(6);
        let skin_weights = [1.0f32, 0.0, 0.0, 0.0].repeat(6);
        let indices = vec![0u32, 1, 2, 3, 4, 5];

        let geometry = PmxParsedGeometry {
            positions,
            normals,
            uvs,
            additional_uvs: Vec::new(),
            indices,
            skin_indices,
            skin_weights,
            edge_scale: Vec::new(),
            material_groups: Vec::new(),
            sdef: PmxParsedSdef::default(),
            qdef: PmxParsedQdef::default(),
        };

        let materials = vec![
            PmxParsedMaterial {
                name: "body".to_owned(),
                english_name: "body".to_owned(),
                texture_path: String::new(),
                sphere_texture_path: String::new(),
                sphere_mode: "none".to_owned(),
                toon_texture_path: String::new(),
                shared_toon_index: None,
                diffuse: [1.0, 1.0, 1.0, 1.0],
                specular: [0.0, 0.0, 0.0],
                specular_power: 1.0,
                ambient: [0.0, 0.0, 0.0],
                edge_color: [0.0, 0.0, 0.0, 1.0],
                edge_size: 1.0,
                flags: no_material_flags(false),
                face_count: 1,
            },
            PmxParsedMaterial {
                name: "face".to_owned(),
                english_name: "face".to_owned(),
                texture_path: String::new(),
                sphere_texture_path: String::new(),
                sphere_mode: "none".to_owned(),
                toon_texture_path: "toon01.bmp".to_owned(),
                shared_toon_index: None,
                diffuse: [1.0, 1.0, 1.0, 1.0],
                specular: [0.0, 0.0, 0.0],
                specular_power: 1.0,
                ambient: [0.0, 0.0, 0.0],
                edge_color: [0.0, 0.0, 0.0, 1.0],
                edge_size: 1.0,
                flags: no_material_flags(true),
                face_count: 1,
            },
        ];

        let model = PmxParsedModel {
            metadata: PmxParsedMetadata {
                format: "pmx".to_owned(),
                version: 2.0,
                encoding: "utf-8".to_owned(),
                name: "fixture".to_owned(),
                english_name: "fixture".to_owned(),
                comment: String::new(),
                english_comment: String::new(),
                counts: PmxParsedCounts {
                    vertices: 6,
                    faces: 2,
                    materials: 2,
                    bones: 3,
                    morphs: 3,
                    display_frames: 0,
                    rigid_bodies: 0,
                    joints: 0,
                    soft_bodies: 0,
                },
                index_sizes,
                additional_uv_count: 0,
            },
            geometry,
            materials,
            skeleton: PmxParsedSkeleton { bones },
            morphs: fixture_morphs(),
            display_frames: Vec::new(),
            rigid_bodies: fixture_rigid_bodies(),
            joints: fixture_joints(),
            soft_bodies: Vec::new(),
            diagnostics: Vec::new(),
        };

        mmd_anim_format::pmx::export_pmx_model(&model)
    }

    /// Three morphs on the fixture's first material: a plain vertex morph, a
    /// material morph, and a group morph that combines the first at half
    /// weight — enough to exercise flattening, material capture, and the
    /// group fold (ADR 0097 §5).
    fn fixture_morphs() -> Vec<PmxParsedMorph> {
        vec![
            PmxParsedMorph {
                name: "smile".to_owned(),
                english_name: "smile".to_owned(),
                panel: "eyebrow".to_owned(),
                kind: "vertex".to_owned(),
                vertex_offsets: vec![PmxParsedVertexMorphOffset {
                    vertex_index: 1,
                    position: [0.0, 1.0, 2.0],
                }],
                group_offsets: Vec::new(),
                bone_offsets: Vec::new(),
                uv_offsets: Vec::new(),
                additional_uv_offsets: Vec::new(),
                material_offsets: Vec::new(),
                flip_offsets: Vec::new(),
                impulse_offsets: Vec::new(),
            },
            PmxParsedMorph {
                name: "blush".to_owned(),
                english_name: "blush".to_owned(),
                panel: "other".to_owned(),
                kind: "material".to_owned(),
                vertex_offsets: Vec::new(),
                group_offsets: Vec::new(),
                bone_offsets: Vec::new(),
                uv_offsets: Vec::new(),
                additional_uv_offsets: Vec::new(),
                material_offsets: vec![PmxParsedMaterialMorphOffset {
                    material_index: 0,
                    operation: "multiply".to_owned(),
                    diffuse: [1.0, 0.5, 0.5, 1.0],
                    specular: [0.0; 3],
                    specular_power: 1.0,
                    ambient: [0.0; 3],
                    edge_color: [0.0; 4],
                    edge_size: 0.0,
                    texture_factor: [1.0; 4],
                    sphere_texture_factor: [1.0; 4],
                    toon_texture_factor: [1.0; 4],
                }],
                flip_offsets: Vec::new(),
                impulse_offsets: Vec::new(),
            },
            PmxParsedMorph {
                name: "half_smile".to_owned(),
                english_name: "half_smile".to_owned(),
                panel: "eyebrow".to_owned(),
                kind: "group".to_owned(),
                vertex_offsets: Vec::new(),
                group_offsets: vec![PmxParsedGroupMorphOffset {
                    morph_index: 0,
                    weight: 0.5,
                }],
                bone_offsets: Vec::new(),
                uv_offsets: Vec::new(),
                additional_uv_offsets: Vec::new(),
                material_offsets: Vec::new(),
                flip_offsets: Vec::new(),
                impulse_offsets: Vec::new(),
            },
        ]
    }

    /// One bone-following anchor and one dynamic body, so the rig has both
    /// modes and one joint between them (ADR 0097 §6).
    fn fixture_rigid_bodies() -> Vec<PmxParsedRigidBody> {
        vec![
            PmxParsedRigidBody {
                name: "anchor".to_owned(),
                english_name: "anchor".to_owned(),
                bone_index: 1,
                group: 0,
                mask: 0xFFFF,
                shape: "sphere".to_owned(),
                size: [0.5, 0.0, 0.0],
                // The fixture's bone 1 ("child") rests at PMX (0, 1, 2), so a
                // body at (0, 1, 4) is two PMX units further along +Z.
                position: [0.0, 1.0, 4.0],
                rotation: [0.0, 0.0, 0.0],
                mass: 0.0,
                linear_damping: 0.5,
                angular_damping: 0.5,
                restitution: 0.0,
                friction: 0.5,
                mode: "static".to_owned(),
            },
            PmxParsedRigidBody {
                name: "hair".to_owned(),
                english_name: "hair".to_owned(),
                bone_index: 1,
                group: 1,
                mask: 0xFFFE,
                shape: "capsule".to_owned(),
                size: [0.25, 2.0, 0.0],
                position: [0.0, 1.0, 2.0],
                rotation: [0.0, 0.0, 0.0],
                mass: 1.5,
                linear_damping: 0.9,
                angular_damping: 0.9,
                restitution: 0.0,
                friction: 0.5,
                mode: "dynamic".to_owned(),
            },
        ]
    }

    fn fixture_joints() -> Vec<PmxParsedJoint> {
        vec![PmxParsedJoint {
            name: "hair_joint".to_owned(),
            english_name: "hair_joint".to_owned(),
            kind: "spring6dof".to_owned(),
            rigid_body_index_a: 0,
            rigid_body_index_b: 1,
            position: [0.0, 1.0, 2.0],
            rotation: [0.0, 0.0, 0.0],
            translation_lower_limit: [0.0, 0.0, -1.0],
            translation_upper_limit: [0.0, 0.0, 3.0],
            rotation_lower_limit: [-0.5, -0.25, -0.75],
            rotation_upper_limit: [0.5, 0.25, 0.75],
            spring_translation_factor: [1.0, 2.0, 3.0],
            spring_rotation_factor: [4.0, 5.0, 6.0],
        }]
    }
}
