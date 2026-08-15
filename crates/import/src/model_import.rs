//! Format-independent asset builder: sub-asset IDs, skeleton identity, and
//! clip [`crate::skeleton_asset::BoneId`] resolution from a `ModelDocument`
//! (ADR 0078, moving the ADR 0077 logic out of the glTF-specific parser).
//!
//! [`build_import_result`](crate::model_import::build_import_result) is the one place sub-asset IDs are derived
//! (`imported_sub_asset_id`, unchanged) and the one place skeleton
//! construction, identity, dedupe, and reimport rebind happen. It reads only
//! [`crate::model_ir`] types, so a future format parser (FBX, USD) reuses
//! every rule here by producing a `ModelDocument` — nothing in this module
//! ever branches on which parser produced its input.
//!
//! `GltfImportResult` and its nested `Gltf*Data` types keep their name and
//! shape from before this split (their content was already format-agnostic);
//! only their owning module changed. [`crate::gltf_import::import_gltf_bytes`]
//! and [`crate::gltf_import::import_gltf_path`] are the public entry points
//! most callers want: they parse and call [`build_import_result`](crate::model_import::build_import_result) in one
//! step.

use crate::animation::{AnimChannel, AnimProperty, AnimationClip, Keyframe};
use crate::asset::{
    imported_sub_asset_id, ImportedSubAssetKind, SkeletonBoneRecord, SkeletonRecord,
};
use crate::mesh::{Mesh, SkinningVertexData, Submesh, Vertex};
use crate::model_ir::{IrAnimProperty, IrMeshData, ModelDocument, SkeletonScope};
use crate::morph::{MaterialMorphOffset, MaterialMorphOperation, MorphAsset};
use crate::rigid_body_rig::{
    JointDef, RigidBodyDef, RigidBodyMode, RigidBodyRigAsset, RigidBodyShape,
};
use crate::skeleton_asset::{compute_skeleton_identity, BoneDef, BoneId, SkeletonAsset};
use crate::skinning::SkeletonNodeDesc;
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use engine_authoring::MaterialAsset;
use glam::Mat4;
use hashbrown::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Diagnostic code reported by [`build_import_result`] when a reimport's
/// bone catalog changed (bones kept / added / retired) against the recorded
/// [`SkeletonRecord`] (ADR 0077 §6). The message carries only aggregate
/// counts and names; the editor's per-bone bind report (AP-5) is built by
/// comparing the previous and current [`SkeletonRecord`] directly rather than
/// by parsing this string.
pub const SKELETON_REBIND_DIAGNOSTIC: &str = "anim.skeleton_rebind";

/// A mesh extracted from a glTF source asset.
#[derive(Clone)]
pub struct GltfMeshData {
    /// Original zero-based glTF mesh selector.
    pub source_index: usize,
    /// Deterministic sub-asset ID derived from the source asset and mesh index.
    pub id: AssetId,
    /// Human-readable mesh name from the glTF document.
    pub name: String,
    /// CPU-side mesh geometry, including skinning attributes and the submesh
    /// ranges its primitives produced (ADR 0076).
    pub mesh: Mesh,
    /// Material sub-asset per submesh, in submesh order.
    ///
    /// An entry is `None` when that primitive declares no material. The
    /// length matches the primitives that survived import, which is one
    /// entry even when [`Mesh::submeshes`] is left empty for a
    /// single-primitive mesh.
    pub materials: Vec<Option<AssetId>>,
    /// Index into [`GltfImportResult::skins`] when a document node binds this
    /// mesh to a skin (first binding wins).
    pub skin_index: Option<usize>,
    /// Named deformations this mesh can blend (ADR 0097 §5), in source
    /// order. Empty for a source that declares none.
    pub morphs: Vec<MorphAsset>,
}

/// A skin extracted from a glTF source asset (ADR 0043, revised by ADR 0077).
#[derive(Clone)]
pub struct GltfSkinData {
    /// Original zero-based glTF skin selector.
    pub source_index: usize,
    /// Naturally-derived stable skeleton ID for this source and skin index
    /// (existing `imported_sub_asset_id` derivation, unchanged). Used for the
    /// `sub_assets` catalog, which requires this exact formula regardless of
    /// dedupe adoption; see [`Self::skeleton`] for the ID actually bound at
    /// runtime.
    pub skeleton_id: AssetId,
    /// Deterministic sub-asset ID derived from the source asset and skin index.
    pub id: AssetId,
    /// Human-readable skin name from the glTF document.
    pub name: String,
    /// Indices into [`GltfImportResult::nodes`] in skin joint order.
    pub joint_nodes: Vec<usize>,
    /// One inverse bind matrix per joint, same order.
    pub inverse_bind_matrices: Vec<Mat4>,
    /// The resolved skeleton asset this skin binds to (ADR 0077).
    ///
    /// Covers this skin's joints and their ancestors, parent-before-child.
    /// Its `id` may differ from [`Self::skeleton_id`] when the dedupe rule
    /// adopted an identical rig already recorded elsewhere in the project
    /// (see this module's private `resolve_skeleton_ids` function).
    pub skeleton: SkeletonAsset,
    /// [`BoneId`] per joint, same order as [`Self::joint_nodes`] (ADR 0077).
    /// Fills [`crate::skinning::Skeleton::bone_ids`] at spawn time.
    pub joint_bone_ids: Vec<BoneId>,
}

/// An animation clip extracted from a glTF source asset (ADR 0043).
#[derive(Clone)]
pub struct GltfAnimationData {
    /// Original zero-based glTF animation selector.
    pub source_index: usize,
    /// Deterministic sub-asset ID derived from the source asset and
    /// animation index.
    pub id: AssetId,
    /// Human-readable animation name from the glTF document.
    pub name: String,
    /// Index into [`GltfImportResult::skins`] whose skeleton the clip's
    /// [`AnimChannel::target_bone`] values refer to (ADR 0077).
    pub skin_index: usize,
    /// The runtime clip with joint-targeted channels.
    pub clip: AnimationClip,
}

/// One decoded texture sub-asset extracted from a glTF source.
#[derive(Clone)]
pub struct GltfTextureData {
    /// Original zero-based glTF texture selector.
    pub source_index: usize,
    /// Stable ID derived from source asset and glTF texture index.
    pub id: AssetId,
    /// Human-readable texture name.
    pub name: String,
    /// Decoded pixel width.
    pub width: u32,
    /// Decoded pixel height.
    pub height: u32,
    /// Tightly packed RGBA8 pixels.
    pub rgba8: Vec<u8>,
}

/// One material sub-asset converted to the persisted material v2 contract.
#[derive(Clone)]
pub struct GltfMaterialData {
    /// Original zero-based glTF material selector.
    pub source_index: usize,
    /// Stable ID derived from source asset and glTF material index.
    pub id: AssetId,
    /// Human-readable material name.
    pub name: String,
    /// Material values and derived texture references.
    pub material: MaterialAsset,
}

/// What one glTF node draws, resolved to importer-side selectors.
///
/// Nodes that only position other nodes (including every joint node) leave
/// both fields empty.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct GltfNodeBinding {
    /// Zero-based glTF mesh index this node instantiates, when it has one.
    ///
    /// Match it against [`GltfMeshData::source_index`] to find the mesh the
    /// node draws.
    pub gltf_mesh_index: Option<usize>,
    /// Index into [`GltfImportResult::skins`] when this node binds a skin
    /// that survived import.
    pub skin_index: Option<usize>,
}

/// All data extracted from a single glTF / GLB import operation.
#[derive(Clone)]
pub struct GltfImportResult {
    /// Static meshes extracted from the document.
    pub meshes: Vec<GltfMeshData>,
    /// One entry per document node, preserving glTF node indices.
    ///
    /// The subset a skin needs becomes that skin's [`SkeletonAsset`] bones,
    /// which is what [`crate::skinning::spawn_rig`] instantiates.
    pub nodes: Vec<SkeletonNodeDesc>,
    /// One entry per document node, in the same order as [`Self::nodes`],
    /// describing what each node draws. Used to instantiate a source as an
    /// entity hierarchy (ADR 0074).
    pub node_bindings: Vec<GltfNodeBinding>,
    /// Skins usable with `nodes`; oversized skins are dropped with an error
    /// diagnostic.
    pub skins: Vec<GltfSkinData>,
    /// Skeletal animation clips; channels targeting non-joint nodes are
    /// skipped with diagnostics.
    pub animations: Vec<GltfAnimationData>,
    /// Decoded image-backed texture sub-assets.
    pub textures: Vec<GltfTextureData>,
    /// PBR materials converted to the engine material v2 schema.
    pub materials: Vec<GltfMaterialData>,
    /// The source's secondary-motion rigid-body rig, if it declared one
    /// (ADR 0097 §6). `None` for every glTF and FBX source.
    pub rigid_body_rig: Option<RigidBodyRigAsset>,
    /// Non-fatal diagnostics (missing normals, unsupported features).
    pub diagnostics: Vec<Diagnostic>,
    /// Bone-catalog records for every skeleton this import bound to, one per
    /// [`Self::skins`] entry (ADR 0077). The caller persists these into
    /// `ImportSettings::skeleton_records` after a successful import so a
    /// later reimport (of this source or another one) can dedupe or
    /// reconcile against them.
    pub skeleton_records: Vec<SkeletonRecord>,
}

impl GltfImportResult {
    /// Builds the persistent stable-ID catalog for this imported document.
    ///
    /// The editor stores this output only after the entire import succeeds;
    /// keeping catalog construction beside the importer prevents background,
    /// test, and future CLI workflows from assigning different selectors.
    ///
    /// [`Self::skins`] each carry their own [`GltfSkinData::skeleton_id`], so
    /// the Skeleton entry is deduplicated by that ID rather than emitted once
    /// per skin: under [`SkeletonScope::PerSkin`] every skin's `skeleton_id`
    /// is naturally distinct (derived from that skin's own source selector),
    /// so this produces exactly the same one-entry-per-skin catalog as
    /// before; under [`SkeletonScope::SharedAcrossDocument`] (ADR 0097 §4a)
    /// every skin shares the same `skeleton_id`, so the shared skeleton
    /// contributes exactly one entry no matter how many skins bind to it.
    /// `GltfImportResult` does not carry the source `ModelDocument`'s scope
    /// at this point, so deduplicating by ID (rather than branching on scope)
    /// is the option that needs no new field and is correct either way.
    pub fn imported_sub_assets(&self) -> Vec<crate::asset::ImportedSubAsset> {
        use crate::asset::ImportedSubAsset;

        let make = |id: &AssetId, kind: ImportedSubAssetKind, name: String, source_index: usize| {
            ImportedSubAsset {
                id: id.as_str().to_owned(),
                kind,
                name,
                index: u32::try_from(source_index).unwrap_or(u32::MAX),
                target_model_source: None,
            }
        };
        let mut assets = Vec::new();
        assets.extend(self.meshes.iter().map(|asset| {
            make(
                &asset.id,
                ImportedSubAssetKind::Mesh,
                asset.name.clone(),
                asset.source_index,
            )
        }));
        assets.extend(self.materials.iter().map(|asset| {
            make(
                &asset.id,
                ImportedSubAssetKind::Material,
                asset.name.clone(),
                asset.source_index,
            )
        }));
        assets.extend(self.textures.iter().map(|asset| {
            make(
                &asset.id,
                ImportedSubAssetKind::Texture,
                asset.name.clone(),
                asset.source_index,
            )
        }));
        let mut seen_skeleton_ids: HashSet<AssetId> = HashSet::new();
        for skin in &self.skins {
            if seen_skeleton_ids.insert(skin.skeleton_id.clone()) {
                assets.push(make(
                    &skin.skeleton_id,
                    ImportedSubAssetKind::Skeleton,
                    format!("{} Skeleton", skin.name),
                    skin.source_index,
                ));
            }
            assets.push(make(
                &skin.id,
                ImportedSubAssetKind::Skin,
                skin.name.clone(),
                skin.source_index,
            ));
        }
        assets.extend(self.animations.iter().map(|asset| {
            make(
                &asset.id,
                ImportedSubAssetKind::Animation,
                asset.name.clone(),
                asset.source_index,
            )
        }));
        // Morphs are catalogued per owning mesh, so a morph shared by
        // several render parts (ADR 0097 §4) contributes one entry per part
        // — each addresses that part's own vertices and is separately
        // assignable.
        for mesh in &self.meshes {
            assets.extend(mesh.morphs.iter().map(|morph| {
                make(
                    &morph.id,
                    ImportedSubAssetKind::Morph,
                    morph.name.clone(),
                    morph.source_index,
                )
            }));
        }
        if let Some(rig) = &self.rigid_body_rig {
            assets.push(make(
                &rig.id,
                ImportedSubAssetKind::RigidBodyRig,
                rig.name.clone(),
                0,
            ));
        }
        assets
    }
}

// ---------------------------------------------------------------------------
// Format dispatch (ADR 0081 §4)
// ---------------------------------------------------------------------------

/// Reports why [`import_model_path`] (and its sibling dispatch functions)
/// could not produce a result.
///
/// Wraps each format parser's own error type unchanged, so a caller that
/// wants format-specific detail can still match through; most callers just
/// use [`std::fmt::Display`].
#[derive(Debug)]
pub enum ModelImportError {
    /// The glTF/GLB parser ([`crate::gltf_import`]) failed.
    Gltf(crate::gltf_import::GltfImportError),
    /// The FBX parser ([`crate::fbx_import`]) failed.
    #[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
    Fbx(crate::fbx_import::FbxImportError),
    /// The PMX parser ([`crate::pmx_import`]) failed.
    #[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
    Pmx(crate::pmx_import::PmxImportError),
    /// Reading a source or sidecar file failed while computing a
    /// fingerprint or dependency list.
    Io(std::io::Error),
    /// `path`'s extension does not name a format this build can import —
    /// either it is genuinely unrecognized, or it names a format whose
    /// support is compiled out (the message says which).
    UnsupportedExtension(String),
}

impl std::fmt::Display for ModelImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gltf(error) => write!(f, "{error}"),
            #[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
            Self::Fbx(error) => write!(f, "{error}"),
            #[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
            Self::Pmx(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::UnsupportedExtension(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ModelImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Gltf(error) => Some(error),
            #[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
            Self::Fbx(error) => Some(error),
            #[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
            Self::Pmx(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::UnsupportedExtension(_) => None,
        }
    }
}

/// A model source format this build can dispatch to a parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFormat {
    Gltf,
    Fbx,
    Pmx,
}

/// Resolves `path`'s lowercased extension to a [`ModelFormat`], or an
/// [`ModelImportError::UnsupportedExtension`] naming why not (unknown
/// extension, or a format compiled out on this build).
fn detect_model_format(path: &Path) -> Result<ModelFormat, ModelImportError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "gltf" | "glb" => Ok(ModelFormat::Gltf),
        "fbx" => Ok(ModelFormat::Fbx),
        "pmx" => Ok(ModelFormat::Pmx),
        other => Err(ModelImportError::UnsupportedExtension(format!(
            "'{other}' is not a supported model source extension (expected gltf, glb, fbx, or pmx)"
        ))),
    }
}

/// Imports a model source from disk, dispatching to the glTF or FBX parser
/// by `path`'s extension (ADR 0081 §4).
///
/// See [`crate::gltf_import::import_gltf_bytes`] for the `existing_skeletons`
/// contract; this function has the identical contract regardless of format.
///
/// # Errors
///
/// Returns [`ModelImportError::UnsupportedExtension`] when `path`'s
/// extension is not `gltf`, `glb`, or `fbx`, or when it is `fbx` on a build
/// with the `fbx-import` feature disabled or targeting wasm32 (ADR 0081 §2).
/// Otherwise forwards the resolved format's own parse error.
pub fn import_model_path(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    import_model_path_with_contact_bones(source_id, path, existing_skeletons, &[])
}

/// Same as [`import_model_path`], but overrides the ground-contact
/// candidate-bone name heuristic (ADR 0080 §1) with `contact_bone_names`
/// when it is non-empty, matching
/// `crate::asset::ImportSettings::contact_bones`'s contract.
///
/// # Errors
///
/// See [`import_model_path`].
pub fn import_model_path_with_contact_bones(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, ModelImportError> {
    match detect_model_format(path)? {
        ModelFormat::Gltf => crate::gltf_import::import_gltf_path_with_contact_bones(
            source_id,
            path,
            existing_skeletons,
            contact_bone_names,
        )
        .map_err(ModelImportError::Gltf),
        ModelFormat::Fbx => import_fbx_path_with_contact_bones_dispatch(
            source_id,
            path,
            existing_skeletons,
            contact_bone_names,
        ),
        ModelFormat::Pmx => import_pmx_path_with_contact_bones_dispatch(
            source_id,
            path,
            existing_skeletons,
            contact_bone_names,
        ),
    }
}

/// Imports a model source from an in-memory byte slice, dispatching on
/// `extension` (lowercase, without the leading dot — e.g. `"gltf"`, `"glb"`,
/// `"fbx"`) since raw bytes carry no format hint of their own.
///
/// See [`crate::gltf_import::import_gltf_bytes`] for the `existing_skeletons`
/// contract.
///
/// # Errors
///
/// See [`import_model_path`].
pub fn import_model_bytes(
    source_id: &AssetId,
    extension: &str,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    match extension.to_ascii_lowercase().as_str() {
        "gltf" | "glb" => {
            crate::gltf_import::import_gltf_bytes(source_id, bytes, existing_skeletons)
                .map_err(ModelImportError::Gltf)
        }
        "fbx" => import_fbx_bytes_dispatch(source_id, bytes, existing_skeletons),
        "pmx" => import_pmx_bytes_dispatch(source_id, bytes, existing_skeletons),
        other => Err(ModelImportError::UnsupportedExtension(format!(
            "'{other}' is not a supported model source extension (expected gltf, glb, fbx, or pmx)"
        ))),
    }
}

/// Returns external sidecar dependencies (buffers, images, or texture files)
/// declared by a model source, dispatching on `path`'s extension.
///
/// # Errors
///
/// See [`import_model_path`].
pub fn model_source_dependencies(path: &Path) -> Result<Vec<PathBuf>, ModelImportError> {
    match detect_model_format(path)? {
        ModelFormat::Gltf => {
            crate::gltf_import::gltf_source_dependencies(path).map_err(ModelImportError::Gltf)
        }
        ModelFormat::Fbx => fbx_source_dependencies_dispatch(path),
        ModelFormat::Pmx => pmx_source_dependencies_dispatch(path),
    }
}

/// Computes a deterministic content fingerprint over a model source and its
/// `dependencies`, dispatching on `path`'s extension only to validate the
/// extension is supported — the underlying hash (`fingerprint_gltf_source` /
/// `fingerprint_fbx_source`) is pure byte hashing and does not otherwise
/// depend on format.
///
/// # Errors
///
/// See [`import_model_path`]; also returns [`ModelImportError::Io`] when a
/// source or dependency file cannot be read.
pub fn fingerprint_model_source(
    path: &Path,
    dependencies: &[PathBuf],
) -> Result<String, ModelImportError> {
    match detect_model_format(path)? {
        ModelFormat::Gltf => crate::gltf_import::fingerprint_gltf_source(path, dependencies)
            .map_err(ModelImportError::Io),
        ModelFormat::Fbx => fingerprint_fbx_source_dispatch(path, dependencies),
        ModelFormat::Pmx => fingerprint_pmx_source_dispatch(path, dependencies),
    }
}

#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
fn import_fbx_path_with_contact_bones_dispatch(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, ModelImportError> {
    crate::fbx_import::import_fbx_path_with_contact_bones(
        source_id,
        path,
        existing_skeletons,
        contact_bone_names,
    )
    .map_err(ModelImportError::Fbx)
}

#[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
fn import_fbx_path_with_contact_bones_dispatch(
    _source_id: &AssetId,
    _path: &Path,
    _existing_skeletons: &[SkeletonRecord],
    _contact_bone_names: &[String],
) -> Result<GltfImportResult, ModelImportError> {
    Err(fbx_support_unavailable())
}

#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
fn import_fbx_bytes_dispatch(
    source_id: &AssetId,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    crate::fbx_import::import_fbx_bytes(source_id, bytes, existing_skeletons)
        .map_err(ModelImportError::Fbx)
}

#[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
fn import_fbx_bytes_dispatch(
    _source_id: &AssetId,
    _bytes: &[u8],
    _existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    Err(fbx_support_unavailable())
}

#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
fn fbx_source_dependencies_dispatch(path: &Path) -> Result<Vec<PathBuf>, ModelImportError> {
    crate::fbx_import::fbx_source_dependencies(path).map_err(ModelImportError::Fbx)
}

#[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
fn fbx_source_dependencies_dispatch(_path: &Path) -> Result<Vec<PathBuf>, ModelImportError> {
    Err(fbx_support_unavailable())
}

#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
fn fingerprint_fbx_source_dispatch(
    path: &Path,
    dependencies: &[PathBuf],
) -> Result<String, ModelImportError> {
    crate::fbx_import::fingerprint_fbx_source(path, dependencies).map_err(ModelImportError::Io)
}

#[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
fn fingerprint_fbx_source_dispatch(
    _path: &Path,
    _dependencies: &[PathBuf],
) -> Result<String, ModelImportError> {
    Err(fbx_support_unavailable())
}

#[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
fn fbx_support_unavailable() -> ModelImportError {
    ModelImportError::UnsupportedExtension(
        "FBX import requires the \"fbx-import\" feature on a non-wasm32 target".to_owned(),
    )
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn import_pmx_path_with_contact_bones_dispatch(
    source_id: &AssetId,
    path: &Path,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> Result<GltfImportResult, ModelImportError> {
    crate::pmx_import::import_pmx_path_with_contact_bones(
        source_id,
        path,
        existing_skeletons,
        contact_bone_names,
    )
    .map_err(ModelImportError::Pmx)
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn import_pmx_path_with_contact_bones_dispatch(
    _source_id: &AssetId,
    _path: &Path,
    _existing_skeletons: &[SkeletonRecord],
    _contact_bone_names: &[String],
) -> Result<GltfImportResult, ModelImportError> {
    Err(pmx_support_unavailable())
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn import_pmx_bytes_dispatch(
    source_id: &AssetId,
    bytes: &[u8],
    existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    crate::pmx_import::import_pmx_bytes(source_id, bytes, existing_skeletons)
        .map_err(ModelImportError::Pmx)
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn import_pmx_bytes_dispatch(
    _source_id: &AssetId,
    _bytes: &[u8],
    _existing_skeletons: &[SkeletonRecord],
) -> Result<GltfImportResult, ModelImportError> {
    Err(pmx_support_unavailable())
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn pmx_source_dependencies_dispatch(path: &Path) -> Result<Vec<PathBuf>, ModelImportError> {
    crate::pmx_import::pmx_source_dependencies(path).map_err(ModelImportError::Pmx)
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn pmx_source_dependencies_dispatch(_path: &Path) -> Result<Vec<PathBuf>, ModelImportError> {
    Err(pmx_support_unavailable())
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn fingerprint_pmx_source_dispatch(
    path: &Path,
    dependencies: &[PathBuf],
) -> Result<String, ModelImportError> {
    crate::pmx_import::fingerprint_pmx_source(path, dependencies).map_err(ModelImportError::Io)
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn fingerprint_pmx_source_dispatch(
    _path: &Path,
    _dependencies: &[PathBuf],
) -> Result<String, ModelImportError> {
    Err(pmx_support_unavailable())
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn pmx_support_unavailable() -> ModelImportError {
    ModelImportError::UnsupportedExtension(
        "PMX import requires the \"mmd-import\" feature on a non-wasm32 target".to_owned(),
    )
}

// ---------------------------------------------------------------------------
// Builder entry point
// ---------------------------------------------------------------------------

/// Builds a `GltfImportResult` from a format-independent `ModelDocument`
/// (ADR 0078).
///
/// This is where every sub-asset ID is derived (`imported_sub_asset_id`,
/// unchanged formula) and where skeleton construction, canonical identity,
/// cross-source dedupe, and reimport rebind (ADR 0077) happen. Every
/// cross-reference in `document` (an [`crate::model_ir::IrNode`]'s `mesh` /
/// `skin`, a submesh's material, a clip's bound skin) is the *original*
/// source selector; this function is what resolves those selectors against
/// each list's own `source_index` and assigns the IDs and BoneIds that
/// depend on them.
///
/// See [`crate::gltf_import::import_gltf_bytes`] for the `existing_skeletons`
/// contract; this function has the identical contract, just taking an
/// already-parsed document instead of raw bytes.
///
/// Ground-contact detection (ADR 0080 §1) runs with the default name
/// heuristic (no bone-name override); use
/// [`build_import_result_with_contact_bones`] to apply
/// `ImportSettings::contact_bones` instead.
pub fn build_import_result(
    document: &ModelDocument,
    source_id: &AssetId,
    existing_skeletons: &[SkeletonRecord],
) -> GltfImportResult {
    build_import_result_with_contact_bones(document, source_id, existing_skeletons, &[])
}

/// Same as [`build_import_result`], but overrides the ground-contact
/// candidate-bone name heuristic (ADR 0080 §1) with `contact_bone_names`
/// when it is non-empty, matching
/// `crate::asset::ImportSettings::contact_bones`'s contract.
pub fn build_import_result_with_contact_bones(
    document: &ModelDocument,
    source_id: &AssetId,
    existing_skeletons: &[SkeletonRecord],
    contact_bone_names: &[String],
) -> GltfImportResult {
    let mut diagnostics = document.diagnostics.clone();

    let nodes: Vec<SkeletonNodeDesc> = document
        .nodes
        .iter()
        .map(|node| SkeletonNodeDesc {
            name: node.name.clone(),
            parent: node.parent,
            translation: node.translation,
            rotation: node.rotation,
            scale: node.scale,
        })
        .collect();

    let skins = build_skins(
        source_id,
        document,
        &nodes,
        existing_skeletons,
        &mut diagnostics,
    );
    let skin_position_by_source: HashMap<usize, usize> = skins
        .iter()
        .enumerate()
        .map(|(position, skin)| (skin.source_index, position))
        .collect();

    // First node that binds a mesh to a skin wins (unchanged rule).
    let mut mesh_skin_bindings: HashMap<usize, usize> = HashMap::new();
    for node in &document.nodes {
        if let (Some(mesh_source), Some(skin_source)) = (node.mesh, node.skin)
            && let Some(&skin_position) = skin_position_by_source.get(&skin_source) {
                mesh_skin_bindings
                    .entry(mesh_source)
                    .or_insert(skin_position);
            }
    }

    let node_bindings: Vec<GltfNodeBinding> = document
        .nodes
        .iter()
        .map(|node| GltfNodeBinding {
            gltf_mesh_index: node.mesh,
            skin_index: node
                .skin
                .and_then(|raw| skin_position_by_source.get(&raw).copied()),
        })
        .collect();

    let meshes = build_meshes(source_id, document, &mesh_skin_bindings);
    let animations = build_animations(
        source_id,
        document,
        &skins,
        &skin_position_by_source,
        contact_bone_names,
    );
    let textures = build_textures(source_id, document);
    let materials = build_materials(source_id, document);
    let rigid_body_rig = build_rigid_body_rig(source_id, document, &skins, &mut diagnostics);

    // Deduplicated by the *resolved* skeleton ID (`skin.skeleton.id`, which
    // reflects dedupe adoption, unlike `skeleton_id`) so a document-wide
    // shared skeleton (ADR 0097 §4a) contributes exactly one record even
    // though every skin clones the same `SkeletonAsset`. Under
    // `SkeletonScope::PerSkin` every skin resolves to its own distinct
    // skeleton in the ordinary case, so this reproduces the previous
    // one-record-per-skin output unchanged.
    let mut seen_resolved_skeleton_ids: HashSet<AssetId> = HashSet::new();
    let skeleton_records = skins
        .iter()
        .filter(|skin| seen_resolved_skeleton_ids.insert(skin.skeleton.id.clone()))
        .map(|skin| SkeletonRecord {
            id: skin.skeleton.id.as_str().to_owned(),
            identity: skin.skeleton.identity.0,
            next_bone_id: skin.skeleton.next_bone_id,
            bones: skin
                .skeleton
                .bones
                .iter()
                .map(|bone| SkeletonBoneRecord {
                    bone_id: bone.id.0,
                    name: bone.name.clone(),
                })
                .collect(),
        })
        .collect();

    GltfImportResult {
        meshes,
        nodes,
        node_bindings,
        skins,
        animations,
        textures,
        materials,
        rigid_body_rig,
        diagnostics,
        skeleton_records,
    }
}

/// Builds every [`GltfSkinData`] entry for `document`, honoring
/// [`ModelDocument::skeleton_scope`] (ADR 0097 §4a).
///
/// Dispatches to [`build_skins_per_skin`] (the unchanged, default behavior)
/// or [`build_skins_shared_across_document`]; see each for its contract.
fn build_skins(
    source_id: &AssetId,
    document: &ModelDocument,
    nodes: &[SkeletonNodeDesc],
    existing_skeletons: &[SkeletonRecord],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<GltfSkinData> {
    match &document.skeleton_scope {
        SkeletonScope::PerSkin => {
            build_skins_per_skin(source_id, document, nodes, existing_skeletons, diagnostics)
        }
        SkeletonScope::SharedAcrossDocument { skeleton_nodes } => {
            build_skins_shared_across_document(
                source_id,
                document,
                nodes,
                skeleton_nodes,
                existing_skeletons,
                diagnostics,
            )
        }
    }
}

/// Builds one [`SkeletonAsset`] per skin, from that skin's joints and their
/// ancestors (ADR 0077, unchanged by ADR 0097 §4a). This is the glTF/FBX
/// behavior and [`SkeletonScope`]'s default.
fn build_skins_per_skin(
    source_id: &AssetId,
    document: &ModelDocument,
    nodes: &[SkeletonNodeDesc],
    existing_skeletons: &[SkeletonRecord],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<GltfSkinData> {
    document
        .skins
        .iter()
        .map(|ir_skin| {
            let skeleton_own_id = imported_sub_asset_id(
                source_id,
                ImportedSubAssetKind::Skeleton,
                ir_skin.source_index,
            );
            let (bones, node_to_bone) = build_skeleton_bones(nodes, &ir_skin.joint_nodes);
            let skeleton = resolve_skeleton_ids(
                &skeleton_own_id,
                format!("{} Skeleton", ir_skin.name),
                bones,
                existing_skeletons,
                diagnostics,
            );
            let joint_bone_ids: Vec<BoneId> = ir_skin
                .joint_nodes
                .iter()
                .map(|joint_node| {
                    let bone_position = node_to_bone
                        .get(joint_node)
                        .copied()
                        .expect("every skin joint is included by build_skeleton_bones");
                    skeleton.bones[bone_position].id
                })
                .collect();

            GltfSkinData {
                source_index: ir_skin.source_index,
                id: imported_sub_asset_id(
                    source_id,
                    ImportedSubAssetKind::Skin,
                    ir_skin.source_index,
                ),
                skeleton_id: skeleton_own_id,
                name: ir_skin.name.clone(),
                joint_nodes: ir_skin.joint_nodes.clone(),
                inverse_bind_matrices: ir_skin.inverse_bind_matrices.clone(),
                skeleton,
                joint_bone_ids,
            }
        })
        .collect()
}

/// Diagnostic code reported by `build_skins_shared_across_document` when a
/// skin's joint node is absent from the shared document skeleton. This is
/// normally unreachable — a well-formed [`crate::model_ir::SkeletonScope::
/// SharedAcrossDocument`] declaration lists every node any skin joints
/// against — but it is still possible for a malformed
/// [`crate::model_ir::IrSkin::joint_nodes`] entry that points outside
/// [`ModelDocument::nodes`] entirely, or for a `skeleton_nodes` declaration
/// that omits a node some skin actually joints against. The affected joint
/// resolves to a [`BoneId`] absent from the skeleton, which
/// [`crate::skinning::joint_palette_system`] already renders as an identity
/// transform (ADR 0086 §3) rather than panicking.
pub const SHARED_SKELETON_JOINT_MISSING_DIAGNOSTIC: &str =
    "model_import.shared_skeleton_joint_missing";

/// Builds one [`SkeletonAsset`] shared by every skin in `document`, spanning
/// exactly `skeleton_nodes` (ADR 0097 §4a, ADR 0086 §4).
///
/// The shared skeleton is built and identity-resolved exactly once, reusing
/// [`build_skeleton_bones`] (passing `skeleton_nodes` as its `joint_nodes`
/// argument, so its ancestor-closure and parent-before-child ordering logic
/// is not duplicated — callers only need to list the rig's own nodes, not
/// manually include ancestors) and the unchanged [`resolve_skeleton_ids`]
/// (ADR 0077). Every returned [`GltfSkinData`] clones that same
/// [`SkeletonAsset`] and shares its `skeleton_id`, derived with selector `0`
/// since there is one skeleton sub-asset for the whole document rather than
/// one per skin.
///
/// `skeleton_nodes` is deliberately *not* inferred from `document.nodes`
/// (e.g. "every node without a mesh"): a format's `ModelDocument` may
/// contain nodes that are not part of the rig at all (PMX's per-split-part
/// mesh anchor nodes, see [`crate::pmx_import`]), and including those would
/// make [`crate::skeleton_asset::SkeletonIdentity`] depend on incidental
/// document structure instead of the rig itself.
fn build_skins_shared_across_document(
    source_id: &AssetId,
    document: &ModelDocument,
    nodes: &[SkeletonNodeDesc],
    skeleton_nodes: &[usize],
    existing_skeletons: &[SkeletonRecord],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<GltfSkinData> {
    let (bones, node_to_bone) = build_skeleton_bones(nodes, skeleton_nodes);
    let skeleton_own_id = imported_sub_asset_id(source_id, ImportedSubAssetKind::Skeleton, 0);
    let skeleton = resolve_skeleton_ids(
        &skeleton_own_id,
        "Shared Skeleton".to_owned(),
        bones,
        existing_skeletons,
        diagnostics,
    );

    document
        .skins
        .iter()
        .map(|ir_skin| {
            let joint_bone_ids: Vec<BoneId> = ir_skin
                .joint_nodes
                .iter()
                .map(|joint_node| match node_to_bone.get(joint_node).copied() {
                    Some(bone_position) => skeleton.bones[bone_position].id,
                    None => {
                        // A well-formed `skeleton_nodes` declaration covers
                        // every node any skin joints against, so this only
                        // triggers for malformed input (an out-of-range
                        // `joint_node`) or a `skeleton_nodes` list that
                        // omitted a node a skin actually uses; fall back to a
                        // `BoneId` guaranteed absent from `skeleton.bones`
                        // rather than panicking. IDs are assigned
                        // sequentially from 0 (or from a dedupe-adopted
                        // record's much smaller `next_bone_id`), so
                        // `u32::MAX` can never collide with a real bone.
                        diagnostics.push(Diagnostic::warning(
                            SHARED_SKELETON_JOINT_MISSING_DIAGNOSTIC,
                            format!(
                                "skin '{}' joint node {joint_node} is outside the shared skeleton's declared node set; it will resolve to an identity transform",
                                ir_skin.name
                            ),
                        ));
                        BoneId(u32::MAX)
                    }
                })
                .collect();

            GltfSkinData {
                source_index: ir_skin.source_index,
                id: imported_sub_asset_id(
                    source_id,
                    ImportedSubAssetKind::Skin,
                    ir_skin.source_index,
                ),
                skeleton_id: skeleton_own_id.clone(),
                name: ir_skin.name.clone(),
                joint_nodes: ir_skin.joint_nodes.clone(),
                inverse_bind_matrices: ir_skin.inverse_bind_matrices.clone(),
                skeleton: skeleton.clone(),
                joint_bone_ids,
            }
        })
        .collect()
}

struct MikktspaceGeometry<'a> {
    vertices: &'a [crate::model_ir::IrVertex],
    indices: Option<&'a [u32]>,
    tangents: Vec<[f32; 4]>,
}

impl MikktspaceGeometry<'_> {
    fn vertex_index(&self, face: usize, vertex: usize) -> usize {
        let corner = face * 3 + vertex;
        self.indices
            .map(|indices| indices[corner] as usize)
            .unwrap_or(corner)
    }
}

impl bevy_mikktspace::Geometry for MikktspaceGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.indices.map_or(self.vertices.len(), <[u32]>::len) / 3
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vertex: usize) -> [f32; 3] {
        self.vertices[self.vertex_index(face, vertex)].position
    }

    fn normal(&self, face: usize, vertex: usize) -> [f32; 3] {
        self.vertices[self.vertex_index(face, vertex)].normal
    }

    fn tex_coord(&self, face: usize, vertex: usize) -> [f32; 2] {
        self.vertices[self.vertex_index(face, vertex)].uv
    }

    fn set_tangent(
        &mut self,
        tangent_space: Option<bevy_mikktspace::TangentSpace>,
        face: usize,
        vertex: usize,
    ) {
        let index = self.vertex_index(face, vertex);
        self.tangents[index] = tangent_space.unwrap_or_default().tangent_encoded();
    }
}

fn generate_missing_tangents(ir: &IrMeshData) -> Option<Vec<[f32; 4]>> {
    let corner_count = ir
        .indices
        .as_deref()
        .map_or(ir.vertices.len(), <[u32]>::len);
    if corner_count == 0 || !corner_count.is_multiple_of(3) {
        return None;
    }
    if let Some(indices) = ir.indices.as_deref()
        && indices
            .iter()
            .any(|&index| index as usize >= ir.vertices.len())
    {
        return None;
    }

    let mut geometry = MikktspaceGeometry {
        vertices: &ir.vertices,
        indices: ir.indices.as_deref(),
        tangents: vec![[0.0; 4]; ir.vertices.len()],
    };
    bevy_mikktspace::generate_tangents(&mut geometry).ok()?;

    // MikkTSpace's encoded sign is opposite to the `cross(N, T) * w`
    // convention used by the renderer and authored glTF tangents.
    for tangent in &mut geometry.tangents {
        tangent[3] = -tangent[3];
    }
    Some(geometry.tangents)
}

fn build_runtime_mesh(ir: &IrMeshData) -> Mesh {
    let tangents = ir
        .tangents
        .clone()
        .or_else(|| generate_missing_tangents(ir));
    Mesh {
        vertices: ir
            .vertices
            .iter()
            .map(|vertex| Vertex {
                position: vertex.position,
                normal: vertex.normal,
                color: vertex.color,
                uv: vertex.uv,
                outline_scale: vertex.outline_scale,
                additional_uv: vertex.additional_uv,
            })
            .collect(),
        indices: ir.indices.clone(),
        skinning: ir.skinning.as_ref().map(|entries| {
            entries
                .iter()
                .map(|entry| SkinningVertexData {
                    joints: entry.joints,
                    weights: entry.weights,
                })
                .collect()
        }),
        tangents,
        submeshes: ir
            .submeshes
            .iter()
            .map(|range| Submesh {
                start: range.start,
                count: range.count,
            })
            .collect(),
    }
}

fn build_meshes(
    source_id: &AssetId,
    document: &ModelDocument,
    mesh_skin_bindings: &HashMap<usize, usize>,
) -> Vec<GltfMeshData> {
    document
        .meshes
        .iter()
        .map(|ir_mesh| GltfMeshData {
            source_index: ir_mesh.source_index,
            id: imported_sub_asset_id(source_id, ImportedSubAssetKind::Mesh, ir_mesh.source_index),
            name: ir_mesh.name.clone(),
            mesh: build_runtime_mesh(&ir_mesh.mesh),
            materials: ir_mesh
                .submesh_materials
                .iter()
                .map(|material| {
                    material.map(|index| {
                        imported_sub_asset_id(source_id, ImportedSubAssetKind::Material, index)
                    })
                })
                .collect(),
            skin_index: mesh_skin_bindings.get(&ir_mesh.source_index).copied(),
            morphs: build_morphs(source_id, ir_mesh),
        })
        .collect()
}

/// Builds one [`MorphAsset`] per surviving morph target of `ir_mesh`
/// (ADR 0097 §5).
///
/// A morph's sub-asset selector combines its owning mesh's selector with its
/// own, because a source's morph index is only unique *within* a mesh: a PMX
/// character's `まばたき` morph appears on every render part it touches
/// (ADR 0097 §4), and giving those the same ID would make them collide in
/// the catalog. Pairing the two selectors keeps each mesh's copy distinct
/// while staying a pure function of the source, so re-importing the same
/// file reproduces every ID.
fn build_morphs(source_id: &AssetId, ir_mesh: &crate::model_ir::IrMesh) -> Vec<MorphAsset> {
    ir_mesh
        .morph_targets
        .iter()
        .map(|target| MorphAsset {
            source_index: morph_selector(ir_mesh.source_index, target.source_index),
            id: imported_sub_asset_id(
                source_id,
                ImportedSubAssetKind::Morph,
                morph_selector(ir_mesh.source_index, target.source_index),
            ),
            name: target.name.clone(),
            vertex_deltas: target.vertex_deltas.clone(),
            material_offsets: target
                .material_offsets
                .iter()
                .map(|offset| MaterialMorphOffset {
                    // Every mesh this importer emits draws with one material
                    // slot, so a morph's material override addresses that
                    // primary material rather than a slot index.
                    slot: None,
                    operation: match offset.operation {
                        crate::model_ir::IrMaterialMorphOperation::Add => {
                            MaterialMorphOperation::Add
                        }
                        crate::model_ir::IrMaterialMorphOperation::Multiply => {
                            MaterialMorphOperation::Multiply
                        }
                    },
                    base_color: offset.base_color,
                })
                .collect(),
        })
        .collect()
}

/// Packs a `(mesh selector, morph selector)` pair into the single index
/// [`imported_sub_asset_id`] takes.
///
/// The shift is wide enough that no realistic morph count can carry into the
/// mesh field: a source with more than 65,535 morphs on one mesh would be
/// two orders of magnitude past anything an authoring tool produces, and the
/// saturating `min` keeps even that case collision-free within its own mesh
/// rather than silently aliasing another mesh's morph.
fn morph_selector(mesh_index: usize, morph_index: usize) -> usize {
    (mesh_index << 16) | morph_index.min(0xFFFF)
}

/// Builds the source's rigid-body rig sub-asset, if it declared one
/// (ADR 0097 §6).
///
/// Bone references are resolved through the document-wide shared skeleton:
/// a rig describes secondary motion for one character, so it binds the one
/// rig every render part shares (ADR 0097 §4a). A document whose skins each
/// own a skeleton ([`crate::model_ir::SkeletonScope::PerSkin`]) has no single
/// skeleton for a rig to reference, so bodies bind no bone there; no format
/// produces that combination today.
fn build_rigid_body_rig(
    source_id: &AssetId,
    document: &ModelDocument,
    skins: &[GltfSkinData],
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<RigidBodyRigAsset> {
    let ir_rig = document.rigid_body_rig.as_ref()?;
    let shared_skeleton = matches!(
        document.skeleton_scope,
        crate::model_ir::SkeletonScope::SharedAcrossDocument { .. }
    )
    .then(|| skins.first().map(|skin| &skin.skeleton))
    .flatten();

    let mut unbound_bodies = 0usize;
    let bodies = ir_rig
        .bodies
        .iter()
        .map(|body| {
            let bone_name = body
                .bone_node
                .and_then(|node| document.nodes.get(node))
                .map(|node| node.name.clone())
                .unwrap_or_default();
            let bone = shared_skeleton.and_then(|skeleton| {
                skeleton
                    .bones
                    .iter()
                    .find(|candidate| candidate.name == bone_name)
                    .map(|candidate| candidate.id)
            });
            if body.bone_node.is_some() && bone.is_none() {
                unbound_bodies += 1;
            }
            RigidBodyDef {
                name: body.name.clone(),
                bone,
                bone_name,
                shape: match body.shape {
                    crate::model_ir::IrRigidBodyShape::Sphere { radius } => {
                        RigidBodyShape::Sphere { radius }
                    }
                    crate::model_ir::IrRigidBodyShape::Box { half_extents } => {
                        RigidBodyShape::Box {
                            half_extents: half_extents.to_array(),
                        }
                    }
                    crate::model_ir::IrRigidBodyShape::Capsule {
                        radius,
                        half_height,
                    } => RigidBodyShape::Capsule {
                        radius,
                        half_height,
                    },
                },
                bone_offset_translation: body.bone_offset_translation.to_array(),
                bone_offset_rotation: body.bone_offset_rotation.to_array(),
                mass: body.mass,
                linear_damping: body.linear_damping,
                angular_damping: body.angular_damping,
                restitution: body.restitution,
                friction: body.friction,
                mode: match body.mode {
                    crate::model_ir::IrRigidBodyMode::Dynamic => RigidBodyMode::Dynamic,
                    crate::model_ir::IrRigidBodyMode::DynamicWithBonePosition => {
                        RigidBodyMode::DynamicWithBonePosition
                    }
                    crate::model_ir::IrRigidBodyMode::FollowBone => RigidBodyMode::FollowBone,
                },
                group: body.group,
                collides_with: body.collides_with,
            }
        })
        .collect();

    if unbound_bodies > 0 {
        diagnostics.push(Diagnostic::warning(
            "anim.rigid_body_bone_unresolved",
            format!(
                "{unbound_bodies} rigid bodies name a bone absent from the imported skeleton and will drive nothing"
            ),
        ));
    }

    let joints = ir_rig
        .joints
        .iter()
        .map(|joint| JointDef {
            name: joint.name.clone(),
            body_a: joint.body_a,
            body_b: joint.body_b,
            translation: joint.translation.to_array(),
            rotation: joint.rotation.to_array(),
            translation_lower: joint.translation_lower.to_array(),
            translation_upper: joint.translation_upper.to_array(),
            rotation_lower: joint.rotation_lower.to_array(),
            rotation_upper: joint.rotation_upper.to_array(),
            spring_translation: joint.spring_translation.to_array(),
            spring_rotation: joint.spring_rotation.to_array(),
        })
        .collect();

    Some(RigidBodyRigAsset {
        schema_version: crate::rigid_body_rig::RIGID_BODY_RIG_SCHEMA_VERSION,
        // A source declares at most one rig, so selector 0 is the whole
        // space and the ID stays stable no matter how the rig changes.
        id: imported_sub_asset_id(source_id, ImportedSubAssetKind::RigidBodyRig, 0),
        name: "Rigid Body Rig".to_owned(),
        skeleton: shared_skeleton.map(|skeleton| skeleton.id.clone()),
        skeleton_identity: shared_skeleton.map(|skeleton| skeleton.identity),
        bodies,
        joints,
    })
}

fn build_textures(source_id: &AssetId, document: &ModelDocument) -> Vec<GltfTextureData> {
    document
        .textures
        .iter()
        .map(|texture| GltfTextureData {
            source_index: texture.source_index,
            id: imported_sub_asset_id(
                source_id,
                ImportedSubAssetKind::Texture,
                texture.source_index,
            ),
            name: texture.name.clone(),
            width: texture.width,
            height: texture.height,
            rgba8: texture.rgba8.clone(),
        })
        .collect()
}

fn build_materials(source_id: &AssetId, document: &ModelDocument) -> Vec<GltfMaterialData> {
    document
        .materials
        .iter()
        .map(|material| {
            let texture_id = |index: usize| {
                imported_sub_asset_id(source_id, ImportedSubAssetKind::Texture, index)
            };
            let asset = MaterialAsset {
                base_color: material.base_color,
                base_color_texture: material.base_color_texture.map(texture_id),
                normal_texture: material.normal_texture.map(texture_id),
                metallic_roughness_texture: material.metallic_roughness_texture.map(texture_id),
                occlusion_texture: material.occlusion_texture.map(texture_id),
                emissive_texture: material.emissive_texture.map(texture_id),
                emissive_color: material.emissive_color,
                normal_scale: material.normal_scale,
                occlusion_strength: material.occlusion_strength,
                roughness: material.roughness,
                metallic: material.metallic,
                alpha_mode: material.alpha_mode,
                alpha_cutoff: material.alpha_cutoff,
                cull_mode: material.cull_mode,
                shading_model: material.shading_model,
                toon: engine_authoring::ToonLitProperties {
                    ramp_texture: material.toon_ramp_texture.map(texture_id),
                    shadow_color: material.toon_shadow_color,
                    ambient_color: material.toon_ambient_color,
                    specular_color: material.toon_specular_color,
                    specular_power: material.toon_specular_power,
                    sphere_texture: material.sphere_texture.map(texture_id),
                    sphere_blend: material.sphere_blend,
                    sphere_coordinates: material.sphere_coordinates,
                    ..engine_authoring::ToonLitProperties::default()
                },
                outline: material.outline.clone(),
                cast_shadow: material.cast_shadow,
                receive_shadow: material.receive_shadow,
                ..MaterialAsset::default()
            };
            GltfMaterialData {
                source_index: material.source_index,
                id: imported_sub_asset_id(
                    source_id,
                    ImportedSubAssetKind::Material,
                    material.source_index,
                ),
                name: material.name.clone(),
                material: asset,
            }
        })
        .collect()
}

fn build_animations(
    source_id: &AssetId,
    document: &ModelDocument,
    skins: &[GltfSkinData],
    skin_position_by_source: &HashMap<usize, usize>,
    contact_bone_names: &[String],
) -> Vec<GltfAnimationData> {
    document
        .clips
        .iter()
        .filter_map(|clip| {
            let skin_index = *skin_position_by_source.get(&clip.skin_index)?;
            let skin = &skins[skin_index];
            let channels: Vec<AnimChannel> = clip
                .channels
                .iter()
                .map(|channel| AnimChannel {
                    property: match channel.property {
                        IrAnimProperty::Translation => AnimProperty::Translation,
                        IrAnimProperty::Rotation => AnimProperty::Rotation,
                        IrAnimProperty::Scale => AnimProperty::Scale,
                    },
                    target_bone: skin.joint_bone_ids.get(channel.joint_index).copied(),
                    keyframes: channel
                        .keyframes
                        .iter()
                        .map(|keyframe| Keyframe {
                            time: keyframe.time,
                            value: keyframe.value,
                        })
                        .collect(),
                })
                .collect();
            let root_bone = detect_root_bone(&skin.skeleton, &channels);
            let mut built_clip = AnimationClip {
                duration: clip.duration,
                channels,
                morph_channels: Vec::new(),
                // glTF has no native event concept; events are added by
                // code after import (Phase 59).
                events: Vec::new(),
                skeleton: Some(skin.skeleton.id.clone()),
                skeleton_identity: Some(skin.skeleton.identity),
                root_bone,
                contacts: Vec::new(),
            };
            // Ground-contact detection (ADR 0080 §1) runs here, after BoneId
            // resolution, against this clip's own bound skeleton.
            built_clip.contacts = crate::contact_detect::detect_contact_intervals(
                &built_clip,
                &skin.skeleton,
                contact_bone_names,
            );
            Some(GltfAnimationData {
                source_index: clip.source_index,
                id: imported_sub_asset_id(
                    source_id,
                    ImportedSubAssetKind::Animation,
                    clip.source_index,
                ),
                name: clip.name.clone(),
                skin_index,
                clip: built_clip,
            })
        })
        .collect()
}

/// Auto-detects [`AnimationClip::root_bone`] as the topmost bone (closest to
/// a skeleton root) that has any translation channel (ADR 0077).
///
/// Ties (equal depth) break by the bone's position in
/// [`SkeletonAsset::bones`], which is parent-before-child, so the choice is
/// deterministic across imports of the same document.
///
/// Visible to [`crate::vmd_import`], which builds clips outside this module's
/// [`crate::model_ir::ModelDocument`] path (ADR 0097 §3) but must detect the
/// root bone by the same rule.
pub(crate) fn detect_root_bone(
    skeleton: &SkeletonAsset,
    channels: &[AnimChannel],
) -> Option<BoneId> {
    channels
        .iter()
        .filter(|channel| channel.property == AnimProperty::Translation)
        .filter_map(|channel| channel.target_bone)
        .filter_map(|bone_id| skeleton.bone_index(bone_id).map(|index| (index, bone_id)))
        .min_by_key(|&(index, _)| (bone_depth(skeleton, index), index))
        .map(|(_, bone_id)| bone_id)
}

/// Counts the ancestor hops from `bone_index` up to a skeleton root.
///
/// Bounded by `skeleton.bones.len()` so a corrupt parent cycle cannot loop
/// forever.
fn bone_depth(skeleton: &SkeletonAsset, bone_index: usize) -> usize {
    let mut depth = 0_usize;
    let mut current = skeleton.bones.get(bone_index).and_then(|bone| bone.parent);
    while let Some(parent) = current {
        depth += 1;
        if depth > skeleton.bones.len() {
            break;
        }
        current = skeleton.bones.get(parent).and_then(|bone| bone.parent);
    }
    depth
}

/// Builds the parent-before-child bone list covering `joint_nodes` and their
/// ancestors (ADR 0077), plus a `node index -> bone position` map used to
/// resolve joint [`BoneId`]s afterward.
///
/// Every [`BoneDef::id`] in the returned list is a placeholder (`BoneId(0)`);
/// [`resolve_skeleton_ids`] assigns the real values, since
/// [`compute_skeleton_identity`] must run before IDs exist.
fn build_skeleton_bones(
    nodes: &[SkeletonNodeDesc],
    joint_nodes: &[usize],
) -> (Vec<BoneDef>, HashMap<usize, usize>) {
    // Include each joint and all of its ancestors, mirroring
    // `crate::skinning::spawn_skin`'s inclusion walk.
    let mut included = vec![false; nodes.len()];
    for &joint in joint_nodes {
        let mut current = Some(joint);
        let mut steps = 0;
        while let Some(index) = current {
            if index >= nodes.len() || included[index] || steps > nodes.len() {
                break;
            }
            included[index] = true;
            steps += 1;
            current = nodes[index].parent;
        }
    }

    // Repeated passes place a node once its parent (if any) is already
    // placed; bounded by `nodes.len()` passes so a corrupt cycle terminates.
    let mut order: Vec<usize> = Vec::new();
    let mut placed = vec![false; nodes.len()];
    let mut progress = true;
    while progress {
        progress = false;
        for index in 0..nodes.len() {
            if !included[index] || placed[index] {
                continue;
            }
            let parent_ready = match nodes[index].parent {
                Some(parent) => placed.get(parent).copied().unwrap_or(true),
                None => true,
            };
            if parent_ready {
                order.push(index);
                placed[index] = true;
                progress = true;
            }
        }
    }

    let node_to_bone: HashMap<usize, usize> = order
        .iter()
        .enumerate()
        .map(|(bone_index, &node_index)| (node_index, bone_index))
        .collect();

    let bones = order
        .iter()
        .map(|&node_index| {
            let node = &nodes[node_index];
            BoneDef {
                id: BoneId(0),
                name: node.name.clone(),
                parent: node
                    .parent
                    .and_then(|parent| node_to_bone.get(&parent).copied()),
                rest_translation: node.translation,
                rest_rotation: node.rotation,
                rest_scale: node.scale,
            }
        })
        .collect();

    (bones, node_to_bone)
}

/// Resolves a candidate skeleton's `id`, per-bone [`BoneId`]s, and
/// `next_bone_id` against `existing_skeletons`, then returns the finished
/// [`SkeletonAsset`] (ADR 0077 §4, §6).
///
/// Three outcomes, checked in order:
/// 1. **Dedupe** — some record (from this source or another, whichever the
///    caller included in `existing_skeletons`) already has the same
///    [`crate::skeleton_asset::SkeletonIdentity`]: adopt its `id`,
///    `next_bone_id`, and bone IDs by position (identical identity implies
///    identical order, names, and topology, so no diagnostic is needed).
/// 2. **Reimport, rig edited** — a record's `id` equals `own_id` (this skin's
///    own naturally-derived skeleton ID) but its identity differs: match new
///    bones to the record's bones by exact name, keep matched IDs, allocate
///    fresh IDs from the record's `next_bone_id` for unmatched new bones, and
///    retire (drop) recorded bones absent from the new import. Reports an
///    `anim.skeleton_rebind` diagnostic listing what changed.
/// 3. **Brand new** — neither matched: assign sequential IDs from zero.
fn resolve_skeleton_ids(
    own_id: &AssetId,
    name: String,
    mut bones: Vec<BoneDef>,
    existing_skeletons: &[SkeletonRecord],
    diagnostics: &mut Vec<Diagnostic>,
) -> SkeletonAsset {
    let identity = compute_skeleton_identity(&bones);

    if let Some(record) = existing_skeletons
        .iter()
        .find(|record| record.identity == identity.0)
    {
        let id = AssetId::from_stable_id(engine_authoring::StableId::new(record.id.clone()))
            .unwrap_or_else(|_| own_id.clone());
        for (bone, recorded) in bones.iter_mut().zip(&record.bones) {
            bone.id = BoneId(recorded.bone_id);
        }
        return SkeletonAsset {
            id,
            name,
            bones,
            identity,
            next_bone_id: record.next_bone_id,
        };
    }

    if let Some(record) = existing_skeletons
        .iter()
        .find(|record| record.id == own_id.as_str())
    {
        let mut next_bone_id = record.next_bone_id;
        let mut kept = 0_usize;
        let mut added: Vec<String> = Vec::new();
        for bone in bones.iter_mut() {
            if let Some(recorded) = record.bones.iter().find(|b| b.name == bone.name) {
                bone.id = BoneId(recorded.bone_id);
                kept += 1;
            } else {
                bone.id = BoneId(next_bone_id);
                next_bone_id += 1;
                added.push(bone.name.clone());
            }
        }
        let retired: Vec<String> = record
            .bones
            .iter()
            .filter(|recorded| !bones.iter().any(|bone| bone.name == recorded.name))
            .map(|recorded| recorded.name.clone())
            .collect();
        if !added.is_empty() || !retired.is_empty() {
            diagnostics.push(Diagnostic::warning(
                SKELETON_REBIND_DIAGNOSTIC,
                format!(
                    "skeleton '{name}' bone catalog changed on reimport: {kept} kept, {} added ({}), {} retired ({})",
                    added.len(),
                    if added.is_empty() { "none".to_owned() } else { added.join(", ") },
                    retired.len(),
                    if retired.is_empty() { "none".to_owned() } else { retired.join(", ") },
                ),
            ));
        }
        return SkeletonAsset {
            id: own_id.clone(),
            name,
            bones,
            identity,
            next_bone_id,
        };
    }

    for (index, bone) in bones.iter_mut().enumerate() {
        bone.id = BoneId(index as u32);
    }
    let next_bone_id = bones.len() as u32;
    SkeletonAsset {
        id: own_id.clone(),
        name,
        bones,
        identity,
        next_bone_id,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_ir::{
        IrAnimProperty, IrClip, IrClipChannel, IrKeyframe, IrMaterial, IrMesh, IrMeshData,
        IrNode, IrSkin, IrTexture, IrVertex,
    };
    use engine_authoring::{LinearRgba, MaterialAlphaMode, MaterialCullMode, MaterialShadingModel};
    use glam::{Quat, Vec3};

    fn sample_mesh_geometry() -> IrMeshData {
        IrMeshData {
            vertices: vec![IrVertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0],
                uv: [0.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            }],
            indices: None,
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    fn tangent_vertex(position: [f32; 3], uv: [f32; 2]) -> IrVertex {
        IrVertex {
            position,
            normal: [0.0, 0.0, 1.0],
            color: [1.0, 1.0, 1.0],
            uv,
            outline_scale: 1.0,
            additional_uv: [0.0; 2],
        }
    }

    fn indexed_quad_geometry() -> IrMeshData {
        IrMeshData {
            vertices: vec![
                tangent_vertex([0.0, 0.0, 0.0], [0.0, 0.0]),
                tangent_vertex([1.0, 0.0, 0.0], [1.0, 0.0]),
                tangent_vertex([1.0, 1.0, 0.0], [1.0, 1.0]),
                tangent_vertex([0.0, 1.0, 0.0], [0.0, 1.0]),
            ],
            indices: Some(vec![0, 1, 2, 0, 2, 3]),
            skinning: None,
            tangents: None,
            submeshes: Vec::new(),
        }
    }

    /// A two-bone rig ("root" -> "tip") described directly as IR, with no
    /// glTF/GLB file involved, exercising the builder in isolation (ADR
    /// 0078's stated purpose for builder tests).
    fn two_bone_rig_document() -> ModelDocument {
        let mut document = ModelDocument {
            nodes: vec![
                IrNode {
                    name: "root".to_owned(),
                    parent: None,
                    translation: Vec3::ZERO,
                    rotation: Quat::IDENTITY,
                    scale: Vec3::ONE,
                    mesh: None,
                    skin: None,
                },
                IrNode {
                    name: "tip".to_owned(),
                    parent: Some(0),
                    translation: Vec3::new(0.0, 1.0, 0.0),
                    rotation: Quat::IDENTITY,
                    scale: Vec3::ONE,
                    mesh: None,
                    skin: None,
                },
            ],
            ..Default::default()
        };
        document.skins.push(IrSkin {
            source_index: 0,
            name: "skeleton".to_owned(),
            joint_nodes: vec![0, 1],
            inverse_bind_matrices: vec![Mat4::IDENTITY; 2],
        });
        document
    }

    fn ir_node(name: &str, parent: Option<usize>, translation: Vec3) -> IrNode {
        IrNode {
            name: name.to_owned(),
            parent,
            translation,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            mesh: None,
            skin: None,
        }
    }

    /// Two disjoint two-bone rigs ("root_a" -> "tip_a", "root_b" -> "tip_b")
    /// described as one document with two skins, one per rig, and no shared
    /// ancestor between them. `scope` drives [`ModelDocument::skeleton_scope`]
    /// so this one fixture exercises both [`SkeletonScope::PerSkin`] (must
    /// stay exactly today's behavior) and
    /// [`SkeletonScope::SharedAcrossDocument`] (ADR 0097 §4a).
    fn two_disjoint_skins_document(scope: SkeletonScope) -> ModelDocument {
        let mut document = ModelDocument {
            nodes: vec![
                ir_node("root_a", None, Vec3::ZERO),
                ir_node("tip_a", Some(0), Vec3::new(0.0, 1.0, 0.0)),
                ir_node("root_b", None, Vec3::new(2.0, 0.0, 0.0)),
                ir_node("tip_b", Some(2), Vec3::new(0.0, 1.0, 0.0)),
            ],
            skeleton_scope: scope,
            ..Default::default()
        };
        document.skins.push(IrSkin {
            source_index: 0,
            name: "skin_a".to_owned(),
            joint_nodes: vec![0, 1],
            inverse_bind_matrices: vec![Mat4::IDENTITY; 2],
        });
        document.skins.push(IrSkin {
            source_index: 1,
            name: "skin_b".to_owned(),
            joint_nodes: vec![2, 3],
            inverse_bind_matrices: vec![Mat4::IDENTITY; 2],
        });
        document
    }

    #[test]
    fn runtime_mesh_preserves_authored_tangents() {
        let mut geometry = indexed_quad_geometry();
        let authored = vec![[0.0, 1.0, 0.0, -1.0]; geometry.vertices.len()];
        geometry.tangents = Some(authored.clone());

        assert_eq!(build_runtime_mesh(&geometry).tangents, Some(authored));
    }

    #[test]
    fn runtime_mesh_generates_mikktspace_tangents_for_indexed_quad() {
        let tangents = build_runtime_mesh(&indexed_quad_geometry())
            .tangents
            .expect("valid indexed quad should receive generated tangents");

        assert_eq!(tangents.len(), 4);
        for tangent in tangents {
            assert!((tangent[0] - 1.0).abs() < 1.0e-5);
            assert!(tangent[1].abs() < 1.0e-5);
            assert!(tangent[2].abs() < 1.0e-5);
            assert!((tangent[3] - 1.0).abs() < 1.0e-5);
        }
    }

    #[test]
    fn runtime_mesh_leaves_tangents_absent_for_invalid_triangle_indices() {
        let mut geometry = indexed_quad_geometry();
        geometry.indices = Some(vec![0, 1, 99]);

        assert!(build_runtime_mesh(&geometry).tangents.is_none());
    }

    #[test]
    fn mesh_and_material_ids_use_the_original_source_selector() {
        let source = AssetId::generate();
        let mut document = ModelDocument::default();
        // Selector 2 simulates two earlier meshes in the source document that
        // were dropped for having no decodable primitives; the surviving
        // mesh must still be identified by its original selector, not its
        // position in `document.meshes`.
        document.meshes.push(IrMesh {
            source_index: 2,
            name: "survivor".to_owned(),
            mesh: sample_mesh_geometry(),
            submesh_materials: vec![Some(5)],
            morph_targets: Vec::new(),
        });
        document.materials.push(IrMaterial {
            source_index: 5,
            name: "surface".to_owned(),
            base_color: LinearRgba::WHITE,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            emissive_color: LinearRgba::WHITE,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            roughness: 0.5,
            metallic: 0.0,
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
            sphere_blend: engine_authoring::MaterialSphereBlendMode::Multiply,
            sphere_coordinates: engine_authoring::MaterialSphereCoordinateSource::ViewNormal,
            outline: engine_authoring::MaterialOutline::default(),
            cast_shadow: true,
            receive_shadow: true,
        });

        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.meshes.len(), 1);
        assert_eq!(
            result.meshes[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Mesh, 2)
        );
        assert_eq!(
            result.meshes[0].materials,
            vec![Some(imported_sub_asset_id(
                &source,
                ImportedSubAssetKind::Material,
                5
            ))]
        );
        assert_eq!(result.materials.len(), 1);
        assert_eq!(
            result.materials[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Material, 5)
        );
    }

    #[test]
    fn dedupe_adopts_the_skeleton_id_of_an_identical_hand_written_rig() {
        let source_a = AssetId::generate();
        let result_a = build_import_result(&two_bone_rig_document(), &source_a, &[]);

        // A second, unrelated source describing the exact same rig: dedupe
        // must adopt the first source's skeleton ID and bone IDs as long as
        // the caller passes its ledger.
        let source_b = AssetId::generate();
        let result_b = build_import_result(
            &two_bone_rig_document(),
            &source_b,
            &result_a.skeleton_records,
        );

        assert_eq!(result_b.skins[0].skeleton.id, result_a.skins[0].skeleton.id);
        assert_eq!(
            result_b.skins[0].joint_bone_ids,
            result_a.skins[0].joint_bone_ids
        );
        assert_ne!(
            result_b.skins[0].skeleton_id, result_b.skins[0].skeleton.id,
            "the second source's own naturally-derived skeleton ID must differ from the adopted one"
        );
        assert!(
            !result_b
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SKELETON_REBIND_DIAGNOSTIC),
            "a same-identity dedupe carries bones over silently"
        );
    }

    // -----------------------------------------------------------------------
    // Skeleton scope (ADR 0097 §4a)
    // -----------------------------------------------------------------------

    #[test]
    fn per_skin_scope_keeps_two_disjoint_skins_on_distinct_skeletons() {
        let source = AssetId::generate();
        let document = two_disjoint_skins_document(SkeletonScope::PerSkin);

        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.skins.len(), 2);
        assert_ne!(
            result.skins[0].skeleton.id, result.skins[1].skeleton.id,
            "PerSkin scope must derive one distinct skeleton per skin, unchanged"
        );
        assert_eq!(
            result.skins[0].skeleton.bones.len(),
            2,
            "skin 0's skeleton must cover only its own joints, not the whole document"
        );
        assert_eq!(
            result.skins[1].skeleton.bones.len(),
            2,
            "skin 1's skeleton must cover only its own joints, not the whole document"
        );
        assert_eq!(
            result
                .imported_sub_assets()
                .iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::Skeleton)
                .count(),
            2,
            "two distinct skeletons must still produce two catalog entries"
        );
        assert_eq!(
            result.skeleton_records.len(),
            2,
            "two distinct skeletons must still produce two skeleton records"
        );
    }

    #[test]
    fn shared_scope_binds_every_skin_to_one_document_wide_skeleton() {
        let source = AssetId::generate();
        let document = two_disjoint_skins_document(SkeletonScope::SharedAcrossDocument {
            skeleton_nodes: vec![0, 1, 2, 3],
        });

        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.skins.len(), 2);
        assert_eq!(
            result.skins[0].skeleton.id, result.skins[1].skeleton.id,
            "every skin must resolve to the same shared skeleton identity"
        );
        assert_eq!(
            result.skins[0].skeleton.identity, result.skins[1].skeleton.identity,
            "every skin must resolve to the same shared skeleton identity"
        );
        assert_eq!(
            result.skins[0].skeleton.bones.len(),
            4,
            "the shared skeleton must contain every node's bone, not just the union of skin joints"
        );

        // Each skin's own joints must resolve to the right bones of the
        // shared skeleton, by node name (position in `skeleton.bones` is an
        // implementation detail of `build_skeleton_bones`'s ordering pass).
        let shared = &result.skins[0].skeleton;
        let bone_name = |bone_id: BoneId| {
            shared
                .bones
                .iter()
                .find(|bone| bone.id == bone_id)
                .map(|bone| bone.name.as_str())
                .expect("resolved BoneId must exist in the shared skeleton")
        };
        assert_eq!(
            result.skins[0]
                .joint_bone_ids
                .iter()
                .map(|&id| bone_name(id))
                .collect::<Vec<_>>(),
            vec!["root_a", "tip_a"]
        );
        assert_eq!(
            result.skins[1]
                .joint_bone_ids
                .iter()
                .map(|&id| bone_name(id))
                .collect::<Vec<_>>(),
            vec!["root_b", "tip_b"]
        );

        let sub_assets = result.imported_sub_assets();
        assert_eq!(
            sub_assets
                .iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::Skeleton)
                .count(),
            1,
            "a shared skeleton must contribute exactly one catalog entry"
        );
        assert_eq!(
            sub_assets
                .iter()
                .filter(|asset| asset.kind == ImportedSubAssetKind::Skin)
                .count(),
            2,
            "every skin must still contribute its own catalog entry"
        );
        assert_eq!(
            result.skeleton_records.len(),
            1,
            "a shared skeleton must contribute exactly one skeleton record"
        );
    }

    #[test]
    fn shared_scope_accepts_a_skeleton_larger_than_max_joints_when_every_skin_stays_under_it() {
        use crate::skinning::MAX_JOINTS;

        // Two skins, each with MAX_JOINTS root joints of their own (well
        // under the per-skin cap), but together spanning more than
        // MAX_JOINTS document nodes: exactly the ADR 0086 §4 promise this
        // feature exists to make reachable.
        let per_skin_joint_count = MAX_JOINTS;
        let total_nodes = per_skin_joint_count * 2;
        assert!(total_nodes > MAX_JOINTS);

        let nodes: Vec<IrNode> = (0..total_nodes)
            .map(|index| ir_node(&format!("bone_{index}"), None, Vec3::ZERO))
            .collect();
        let mut document = ModelDocument {
            nodes,
            skeleton_scope: SkeletonScope::SharedAcrossDocument {
                skeleton_nodes: (0..total_nodes).collect(),
            },
            ..Default::default()
        };
        document.skins.push(IrSkin {
            source_index: 0,
            name: "skin_a".to_owned(),
            joint_nodes: (0..per_skin_joint_count).collect(),
            inverse_bind_matrices: vec![Mat4::IDENTITY; per_skin_joint_count],
        });
        document.skins.push(IrSkin {
            source_index: 1,
            name: "skin_b".to_owned(),
            joint_nodes: (per_skin_joint_count..total_nodes).collect(),
            inverse_bind_matrices: vec![Mat4::IDENTITY; per_skin_joint_count],
        });

        let source = AssetId::generate();
        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.skins.len(), 2);
        assert!(
            result.skins[0].skeleton.bones.len() > MAX_JOINTS,
            "the shared skeleton is permitted to exceed MAX_JOINTS (ADR 0086 §4)"
        );
        assert_eq!(result.skins[0].skeleton.bones.len(), total_nodes);
        for skin in &result.skins {
            assert!(
                skin.joint_bone_ids.len() <= MAX_JOINTS,
                "no single skin's own joint list may exceed MAX_JOINTS"
            );
            assert_eq!(skin.joint_bone_ids.len(), per_skin_joint_count);
        }
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SHARED_SKELETON_JOINT_MISSING_DIAGNOSTIC),
            "every joint node exists in the shared skeleton, so this diagnostic must not fire"
        );
    }

    #[test]
    fn shared_scope_identity_is_independent_of_mesh_anchor_node_count() {
        // Regression guard: `skeleton_nodes` must list only the rig, never
        // nodes that exist purely to carry a split render part's
        // `mesh`/`skin` indices (PMX's per-split mesh anchor nodes). If
        // those leaked into the shared skeleton's bone list,
        // `SkeletonIdentity` (ADR 0077 §4) would depend on how many render
        // parts the multi-skin split happened to produce rather than on the
        // rig itself — so tuning the split heuristic, or editing a model
        // just enough to change a split part count (e.g. hair splitting
        // into three groups instead of two), would spuriously trip ADR
        // 0077's rebind path even though the rig never changed. Two
        // documents with an identical rig (root/tip) but a different number
        // of mesh-anchor/skin entries must resolve to the same skeleton
        // identity and the same skeleton AssetId, with no rebind
        // diagnostic.
        fn document_with_split_part_count(part_count: usize) -> ModelDocument {
            let mut nodes = vec![
                ir_node("root", None, Vec3::ZERO),
                ir_node("tip", Some(0), Vec3::new(0.0, 1.0, 0.0)),
            ];
            let mut skins = Vec::new();
            for index in 0..part_count {
                // A mesh anchor node, mirroring pmx_import.rs's per-split
                // -part `IrNode`: parentless, carries only `mesh`/`skin`,
                // and is not part of the rig.
                nodes.push(ir_node(&format!("part_{index}"), None, Vec3::ZERO));
                skins.push(IrSkin {
                    source_index: index,
                    name: format!("part_{index}"),
                    joint_nodes: vec![0, 1],
                    inverse_bind_matrices: vec![Mat4::IDENTITY; 2],
                });
            }
            ModelDocument {
                nodes,
                skins,
                skeleton_scope: SkeletonScope::SharedAcrossDocument {
                    skeleton_nodes: vec![0, 1],
                },
                ..Default::default()
            }
        }

        let source = AssetId::generate();
        let two_parts = build_import_result(&document_with_split_part_count(2), &source, &[]);

        // Reimport the same source after the split heuristic changed the
        // part count from 2 to 5, passing the ledger the first import
        // produced, exactly like a real reimport would.
        let five_parts = build_import_result(
            &document_with_split_part_count(5),
            &source,
            &two_parts.skeleton_records,
        );

        assert_eq!(
            two_parts.skins[0].skeleton.identity, five_parts.skins[0].skeleton.identity,
            "the rig (root/tip) is identical in both documents; only the split part count \
             differs, so skeleton identity must not change"
        );
        assert_eq!(
            two_parts.skins[0].skeleton.id, five_parts.skins[0].skeleton.id,
            "an unchanged rig must resolve to the same skeleton AssetId across the split-count \
             change"
        );
        assert_eq!(
            two_parts.skins[0].skeleton.bones.len(),
            2,
            "the shared skeleton must contain only the two rig bones, never the mesh anchor nodes"
        );
        assert_eq!(five_parts.skins[0].skeleton.bones.len(), 2);
        assert!(
            !five_parts
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SKELETON_REBIND_DIAGNOSTIC),
            "a split-part-count change alone must not be treated as a rig edit"
        );
    }

    #[test]
    fn clip_channel_resolves_to_the_skeletons_bone_id_by_joint_position() {
        let source = AssetId::generate();
        let mut document = two_bone_rig_document();
        document.clips.push(IrClip {
            source_index: 0,
            name: "spin".to_owned(),
            skin_index: 0,
            channels: vec![IrClipChannel {
                joint_index: 1,
                property: IrAnimProperty::Rotation,
                keyframes: vec![
                    IrKeyframe {
                        time: 0.0,
                        value: [0.0, 0.0, 0.0, 1.0],
                    },
                    IrKeyframe {
                        time: 1.0,
                        value: [0.0, 0.0, 0.0, 1.0],
                    },
                ],
            }],
            duration: 1.0,
        });

        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.animations.len(), 1);
        let clip = &result.animations[0].clip;
        assert_eq!(clip.channels.len(), 1);
        assert_eq!(
            clip.channels[0].target_bone,
            Some(result.skins[0].joint_bone_ids[1]),
            "the channel's joint position (1 = tip) must resolve to that bone's BoneId"
        );
        assert_eq!(clip.skeleton, Some(result.skins[0].skeleton.id.clone()));
        assert_eq!(
            clip.skeleton_identity,
            Some(result.skins[0].skeleton.identity)
        );
    }

    #[test]
    fn a_clip_bound_to_a_dropped_skin_selector_is_silently_omitted() {
        // Defensive case: a clip whose `skin_index` does not match any
        // surviving skin (e.g. a corrupt or since-removed reference) must be
        // dropped rather than panic, matching this crate's no-panic-on
        // recoverable-input-error rule.
        let source = AssetId::generate();
        let mut document = two_bone_rig_document();
        document.clips.push(IrClip {
            source_index: 0,
            name: "orphan".to_owned(),
            skin_index: 99,
            channels: vec![IrClipChannel {
                joint_index: 0,
                property: IrAnimProperty::Rotation,
                keyframes: vec![IrKeyframe {
                    time: 0.0,
                    value: [0.0, 0.0, 0.0, 1.0],
                }],
            }],
            duration: 0.0,
        });

        let result = build_import_result(&document, &source, &[]);

        assert!(result.animations.is_empty());
    }

    #[test]
    fn texture_ids_use_the_original_source_selector() {
        let source = AssetId::generate();
        let mut document = ModelDocument::default();
        document.textures.push(IrTexture {
            source_index: 3,
            name: "albedo".to_owned(),
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
        });

        let result = build_import_result(&document, &source, &[]);

        assert_eq!(result.textures.len(), 1);
        assert_eq!(
            result.textures[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Texture, 3)
        );
    }

    #[test]
    fn direct_parse_and_build_matches_the_public_entry_point_catalog() {
        let source = AssetId::generate();
        let bytes = crate::gltf_import::tests::three_clip_character_glb();

        let via_entry_point = crate::gltf_import::import_gltf_bytes(&source, &bytes, &[])
            .expect("public entry point import must succeed");

        let document = crate::gltf_import::parse_gltf(&bytes).expect("direct parse must succeed");
        let direct = build_import_result(&document, &source, &[]);

        assert_eq!(
            direct.imported_sub_assets(),
            via_entry_point.imported_sub_assets(),
            "parse_gltf + build_import_result must match the import_gltf_bytes catalog exactly"
        );
    }

    // -----------------------------------------------------------------------
    // Format dispatch (ADR 0081 §4)
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_routes_glb_extension_to_the_gltf_parser_unchanged() {
        let source = AssetId::generate();
        let bytes = crate::gltf_import::tests::three_clip_character_glb();
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("character.glb");
        std::fs::write(&path, &bytes).expect("glb fixture");

        let dispatched = import_model_path(&source, &path, &[]).expect("glb must dispatch");
        let direct =
            crate::gltf_import::import_gltf_bytes(&source, &bytes, &[]).expect("direct import");
        assert_eq!(
            dispatched.imported_sub_assets(),
            direct.imported_sub_assets(),
            "dispatching a .glb path must match the direct glTF import exactly"
        );
    }

    #[test]
    fn dispatch_rejects_an_unknown_extension() {
        let source = AssetId::generate();
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("character.obj");
        std::fs::write(&path, b"o cube").expect("unrelated fixture");

        match import_model_path(&source, &path, &[]) {
            Err(ModelImportError::UnsupportedExtension(_)) => {}
            Ok(_) => panic!("an .obj path must not dispatch to any model parser"),
            Err(_) => panic!("expected UnsupportedExtension for an .obj path"),
        }
    }

    #[test]
    fn dispatch_routes_fbx_extension_to_the_fbx_parser() {
        let source = AssetId::generate();
        #[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
        {
            let bytes = crate::fbx_import::tests::SKINNED_FBX.as_bytes();
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("character.fbx");
            std::fs::write(&path, bytes).expect("fbx fixture");

            let dispatched = import_model_path(&source, &path, &[]).expect("fbx must dispatch");
            let direct =
                crate::fbx_import::import_fbx_bytes(&source, bytes, &[]).expect("direct import");
            assert_eq!(
                dispatched.imported_sub_assets(),
                direct.imported_sub_assets(),
                "dispatching a .fbx path must match the direct FBX import exactly"
            );
        }
        #[cfg(not(all(feature = "fbx-import", not(target_arch = "wasm32"))))]
        {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("character.fbx");
            std::fs::write(&path, b"; not a real fbx file").expect("placeholder fixture");
            match import_model_path(&source, &path, &[]) {
                Err(ModelImportError::UnsupportedExtension(_)) => {}
                Ok(_) => panic!("fbx-import disabled or wasm32 must reject .fbx"),
                Err(_) => panic!("expected UnsupportedExtension when fbx-import is unavailable"),
            }
        }
    }

    #[test]
    fn dispatch_routes_pmx_extension_to_the_pmx_parser() {
        let source = AssetId::generate();
        #[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
        {
            let bytes = crate::pmx_import::tests::skinned_pmx_fixture();
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("character.pmx");
            std::fs::write(&path, &bytes).expect("pmx fixture");

            let dispatched = import_model_path(&source, &path, &[]).expect("pmx must dispatch");
            let direct =
                crate::pmx_import::import_pmx_bytes(&source, &bytes, &[]).expect("direct import");
            assert_eq!(
                dispatched.imported_sub_assets(),
                direct.imported_sub_assets(),
                "dispatching a .pmx path must match the direct PMX import exactly"
            );
        }
        #[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
        {
            let directory = tempfile::tempdir().expect("temporary directory");
            let path = directory.path().join("character.pmx");
            std::fs::write(&path, b"not a real pmx file").expect("placeholder fixture");
            match import_model_path(&source, &path, &[]) {
                Err(ModelImportError::UnsupportedExtension(_)) => {}
                Ok(_) => panic!("mmd-import disabled or wasm32 must reject .pmx"),
                Err(_) => panic!("expected UnsupportedExtension when mmd-import is unavailable"),
            }
        }
    }

    #[test]
    fn import_model_bytes_dispatches_on_extension_hint() {
        let source = AssetId::generate();
        let bytes = crate::gltf_import::tests::three_clip_character_glb();
        let dispatched =
            import_model_bytes(&source, "glb", &bytes, &[]).expect("glb bytes must dispatch");
        let direct =
            crate::gltf_import::import_gltf_bytes(&source, &bytes, &[]).expect("direct import");
        assert_eq!(
            dispatched.imported_sub_assets(),
            direct.imported_sub_assets()
        );

        match import_model_bytes(&source, "obj", &bytes, &[]) {
            Err(ModelImportError::UnsupportedExtension(_)) => {}
            Ok(_) => panic!("an unsupported extension hint must be rejected"),
            Err(_) => panic!("expected UnsupportedExtension for an unrecognized extension hint"),
        }
    }
}
