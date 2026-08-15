//! VMD (MMD motion) import: bakes MMD's per-frame bone pipeline into a plain
//! `AnimationClip` (ADR 0097 §3).
//!
//! A `.vmd` file is not a model — it carries no mesh, material, or texture,
//! only bone curves, morph curves, and (deliberately ignored, ADR 0097
//! Context) camera/light/self-shadow curves. So it does not go through
//! `crate::model_import`'s `crate::model_ir::ModelDocument` path at all;
//! this module is its own entry point, and its output is an ordinary
//! `AnimationClip` sub-asset indistinguishable from a glTF- or
//! FBX-imported one.
//!
//! # Why the motion is baked rather than evaluated at runtime
//!
//! MMD resolves a frame in three stages — FK from the VMD's own curves, then
//! IK solving, then appended-parent (付与親) propagation. None of the last
//! two is expressible as a keyframe curve; they are per-frame constraints.
//! Playing them back would need an MMD-only pose evaluator running every
//! frame, which would cut MMD characters off from Animation Sets, the
//! Animation Graph/Controller, cross-fade, Animation Events, Root Motion,
//! and retargeting (ADR 0079) — every tool built for every other imported
//! format.
//!
//! Instead this module runs `mmd-anim-runtime`'s evaluator across the whole
//! motion at import time and records the *result*: one flat local
//! translation/rotation per bone per sample. IK and appended-parent are
//! dissolved into ordinary FK curves, exactly the way
//! `crate::fbx_import` dissolves FBX `PreRotation` before anything reaches
//! the IR. The trade-off is recorded in ADR 0097's Consequences: a bug in
//! `mmd-anim-runtime`'s IK evaluation is baked permanently into the clip and
//! is fixed by reimporting, not by patching the runtime.
//!
//! # What a bake reads
//!
//! Baking needs the PMX rig, not merely the engine `SkeletonAsset` the
//! clip binds to: a `SkeletonAsset` carries bone names, parents, and rest
//! TRS, but no IK chains, no appended-parent links, no fixed/local axes, and
//! no bone morphs — precisely the data the evaluator exists to apply. So a
//! caller first builds a `VmdBakeRig` from the same `.pmx` bytes
//! `crate::pmx_import` imported, paired with the `SkeletonAsset` that
//! import produced, and then bakes any number of `.vmd` files against that
//! one rig without reparsing the model each time.
//!
//! # Bone binding
//!
//! MMD's bone naming is a de facto ecosystem standard (foot IK bones are
//! always named `左足ＩＫ`/`右足ＩＫ`, and so on), which is why a VMD
//! authored for an unrelated MMD character usually plays back correctly on
//! another one with no retarget map (ADR 0079) involved. Binding therefore
//! happens by name at two independent boundaries:
//!
//! 1. **VMD curve -> PMX bone**, done inside `mmd-anim-format` against the
//!    name table `VmdBakeRig` holds. A VMD bone name absent from the rig
//!    drops that curve with a `vmd.bone_not_found` diagnostic rather than
//!    failing the import.
//! 2. **PMX bone -> engine `crate::skeleton_asset::BoneId`**, done here by
//!    matching `crate::skeleton_asset::BoneDef::name` against the PMX bone
//!    name. `crate::pmx_import` emits one node per PMX bone using that
//!    same name, so the match is exact for every well-formed model. PMX does
//!    not guarantee unique bone names, so equal names are consumed in
//!    order (the *n*-th PMX bone named `X` binds to the *n*-th skeleton bone
//!    named `X`), which is deterministic and degrades to the obvious
//!    one-to-one mapping whenever names are unique. A PMX bone with no
//!    counterpart in the skeleton is reported once via
//!    `vmd.bone_not_in_skeleton` and contributes no channel.
//!
//! Because a mismatched pairing (a VMD baked against skeleton A while the
//! rig describes model B) would silently produce garbage rather than fail,
//! `VmdBakeRig` additionally compares every bound bone's rest translation
//! against the PMX rig's own and reports `vmd.rest_pose_mismatch` when they
//! disagree.
//!
//! # Coordinate conversion
//!
//! The evaluator works entirely in PMX space (left-handed, +Y up, MMD
//! units). Every baked value is converted to engine space with exactly the
//! transform `crate::pmx_import` applied to the model itself — negate Z,
//! then scale by `crate::pmx_import::PMX_TO_METERS` — so a baked pose
//! lands on the imported mesh. For a rotation that conjugation
//! (`diag(1, 1, -1)`, an improper transform) negates the axis' X and Y
//! components and preserves the angle, which for a quaternion `(x, y, z, w)`
//! is `(-x, -y, z, w)`; see `convert_rotation`.
//!
//! # Sampling and channel pruning
//!
//! MMD's native frame rate is 30 Hz and VMD keys are integer frame numbers,
//! so the default sample rate (`DEFAULT_VMD_SAMPLE_RATE`) samples exactly
//! one pose per source frame. Every bake is reported once via
//! `vmd.curve_resampled`, mirroring `crate::fbx_import`'s
//! `anim.fbx_curve_resampled` (ADR 0081 §3).
//!
//! Sampling produces a value for every bone at every sample, but a 376-bone
//! rig driven by a motion that animates thirty bones must not emit 376
//! channels of identical values. Each bone's baked track is therefore
//! reduced (see `push_channels`): a track that never leaves its rest value
//! emits no channel at all, a track that is constant but not at rest
//! collapses to a single keyframe (which `crate::animation::lerp_channel`
//! holds for the whole clip), and only a track that actually varies keeps
//! its full key list. Bones moved *only* by IK or appended-parent are
//! covered by this automatically: they vary, so they get channels, even
//! though the VMD names no curve for them.
//!
//! # Morph and scene channels
//!
//! Morph curves are applied to the evaluator so PMX bone morphs affect the
//! baked pose, and renderable curves are also emitted as logical name-based
//! `MorphChannel` tracks. Camera, light, and self-shadow sections are
//! inspected for content routing but remain scene-level data: scene-only VMD
//! files are never paired to a PMX, while mixed files import their model
//! channels and report that their scene channels were ignored (ADR 0098).

use crate::animation::{AnimChannel, AnimProperty, AnimationClip, Keyframe, MorphChannel};
use crate::asset::{
    imported_motion_sub_asset_id, imported_sub_asset_id, ImportedSubAssetKind,
};
use crate::derived_cache::{CacheKey, DerivedCache};
use crate::model_import::GltfImportResult;
use crate::pmx_import::{convert_position, convert_rotation};
use crate::skeleton_asset::{BoneId, SkeletonAsset};
use engine_authoring::diagnostic::Diagnostic;
use engine_authoring::id::AssetId;
use glam::{Quat, Vec3};
use mmd_anim_format::pmx::{
    import_pmx_runtime, parse_pmx_model, PmxParsedBone, PmxParsedModel, PmxRuntimeImport,
};
use mmd_anim_format::vmd::{
    build_mmd_registered_pair_clip, parse_vmd_shared_context, VmdImportResult as MmdMotion,
    VmdParsedAnimation, VmdParsedMorphFrame, VmdSharedContextSummary,
};
use mmd_anim_runtime::{BoneIndex, ModelArena, MorphIndex, RuntimeInstance};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// MMD's native frame rate: VMD keyframe numbers are frame indices at this
/// rate, and every frame/second conversion in this module goes through it.
pub const MMD_FRAME_RATE: f32 = 30.0;

/// Default bake sample rate in Hz, equal to [`MMD_FRAME_RATE`] so the
/// default bake takes exactly one sample per source VMD frame.
pub const DEFAULT_VMD_SAMPLE_RATE: f32 = MMD_FRAME_RATE;

/// Longest motion this module bakes, in MMD frames (two hours at
/// [`MMD_FRAME_RATE`]).
///
/// A VMD frame number is a `u32`, so a corrupt or hand-edited file can claim
/// a frame count that would exhaust memory during sampling. A motion past
/// this bound is truncated with a `vmd.motion_truncated` diagnostic rather
/// than failing the import, matching this module's "drop with a diagnostic"
/// convention everywhere else.
pub const MAX_BAKED_FRAME: f32 = 216_000.0;

/// Bumped whenever a change to this module would produce a different clip
/// from unchanged inputs, so cached bakes from an older build are not
/// served (see [`cache_key_for_baked_vmd`]).
pub const VMD_BAKE_ALGORITHM_VERSION: u32 = 3;

/// Semantic channel domain detected from one VMD document.
///
/// Classification uses section contents rather than file names because the
/// MMD ecosystem does not prescribe names such as `body`, `face`, or
/// `camera`, and one VMD can physically contain both model and scene tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmdContentKind {
    /// Bone, morph, or model-property tracks only.
    Model,
    /// Camera, light, or self-shadow tracks only.
    Scene,
    /// Both model and scene track domains are populated.
    Mixed,
    /// No supported track domain contains any keyframes.
    Empty,
}

/// Classifies a parsed VMD summary by the channels it actually contains.
pub fn classify_vmd_summary(summary: &VmdSharedContextSummary) -> VmdContentKind {
    let has_model = summary.bones.key_count != 0
        || summary.morphs.key_count != 0
        || summary.properties.key_count != 0;
    let has_scene = summary.cameras.key_count != 0
        || summary.lights.key_count != 0
        || summary.self_shadows.key_count != 0;
    match (has_model, has_scene) {
        (true, false) => VmdContentKind::Model,
        (false, true) => VmdContentKind::Scene,
        (true, true) => VmdContentKind::Mixed,
        (false, false) => VmdContentKind::Empty,
    }
}

/// Parses and classifies one VMD without constructing a model-bound clip.
pub fn classify_vmd_bytes(bytes: &[u8]) -> Result<VmdContentKind, VmdImportError> {
    let context =
        parse_vmd_shared_context(bytes).map_err(|error| VmdImportError::Parse(error.to_string()))?;
    Ok(classify_vmd_summary(context.summary()))
}

/// Reads and classifies one VMD file without requiring a PMX pairing.
pub fn classify_vmd_path(path: &Path) -> Result<VmdContentKind, VmdImportError> {
    let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
    classify_vmd_bytes(&bytes)
}

/// Reads the model name recorded in a VMD header without requiring a PMX.
///
/// The value is presentation-only metadata: import never guesses a PMX from
/// it because model names are neither stable nor necessarily unique. The
/// editor displays it beside the optional original-model picker so authors
/// can make the pairing decision with the information the file provides.
pub fn vmd_recorded_model_name_path(path: &Path) -> Result<String, VmdImportError> {
    let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
    let context =
        parse_vmd_shared_context(&bytes).map_err(|error| VmdImportError::Parse(error.to_string()))?;
    Ok(context.parsed_animation().metadata.model_name.clone())
}

/// Absolute tolerance used when deciding whether a VMD track ever leaves its
/// neutral value. VMD stores single-precision floats, so this deliberately
/// absorbs decoder noise while remaining far below a visible motion delta.
pub const VMD_COMPATIBILITY_NEUTRAL_EPSILON: f32 = 1.0e-6;

/// Exact-name match totals for one VMD track domain (bones or morphs).
///
/// `used_tracks` counts only tracks that leave their fixed neutral value.
/// Every used track belongs to exactly one of the remaining three counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmdPmxCompatibilitySummary {
    /// Number of VMD tracks that contain at least one non-neutral key.
    pub used_tracks: usize,
    /// Number of used tracks whose name occurs exactly once in the PMX.
    pub unique_tracks: usize,
    /// Number of used tracks whose name does not occur in the PMX.
    pub missing_tracks: usize,
    /// Number of used tracks whose name occurs more than once in the PMX.
    pub ambiguous_tracks: usize,
}

impl VmdPmxCompatibilitySummary {
    /// Returns the exact-name compatibility percentage, or `None` when the
    /// VMD has no meaningful tracks in this domain.
    pub fn compatibility_percent(self) -> Option<f32> {
        (self.used_tracks != 0)
            .then(|| self.unique_tracks as f32 * 100.0 / self.used_tracks as f32)
    }

    /// Returns whether every meaningful track has exactly one PMX match.
    pub fn is_unique_full_match(self) -> bool {
        self.used_tracks != 0 && self.unique_tracks == self.used_tracks
    }
}

/// A compatibility problem discovered while comparing one VMD with one PMX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VmdPmxCompatibilityIssueKind {
    /// A meaningful VMD bone track has no exact PMX name match.
    MissingBone,
    /// A meaningful VMD bone track has multiple exact PMX name matches.
    AmbiguousBone,
    /// A uniquely matched PMX bone cannot accept used rotation keys.
    RotationUnsupported,
    /// A uniquely matched PMX bone cannot accept used translation keys.
    TranslationUnsupported,
    /// A meaningful VMD morph track has no exact PMX name match.
    MissingMorph,
    /// A meaningful VMD morph track has multiple exact PMX name matches.
    AmbiguousMorph,
}

/// One named issue in a [`VmdPmxCompatibilityReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmdPmxCompatibilityIssue {
    /// Classification used for stable sorting and presentation.
    pub kind: VmdPmxCompatibilityIssueKind,
    /// Exact VMD track name involved in the issue.
    pub name: String,
    /// Total number of source keyframes on the affected VMD track.
    pub keyframe_count: usize,
}

/// Read-only exact-name compatibility result for one VMD/PMX pair.
///
/// This report intentionally does not inspect hierarchy, rest pose, IK, or a
/// Retarget Map. Those concerns belong to baking and retarget validation; the
/// report answers only whether meaningful VMD track names bind uniquely and
/// whether uniquely bound bones permit the operations the VMD actually uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmdPmxCompatibilityReport {
    /// Presentation-only target-model name recorded in the VMD header.
    pub recorded_model_name: String,
    /// Presentation-only model name recorded in the PMX metadata.
    pub pmx_model_name: String,
    /// Exact-name match totals for meaningful bone tracks.
    pub bones: VmdPmxCompatibilitySummary,
    /// Exact-name match totals for meaningful morph tracks.
    pub morphs: VmdPmxCompatibilitySummary,
    /// Missing, ambiguous, and unsupported-operation details.
    pub issues: Vec<VmdPmxCompatibilityIssue>,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsedBoneTrack {
    keyframe_count: usize,
    uses_translation: bool,
    uses_rotation: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct UsedMorphTrack {
    keyframe_count: usize,
    is_used: bool,
}

/// Reads and compares one VMD/PMX pair without importing or modifying either
/// asset. Intended for the editor's on-demand Import Settings check.
pub fn check_vmd_pmx_compatibility_path(
    vmd_path: &Path,
    pmx_path: &Path,
) -> Result<VmdPmxCompatibilityReport, VmdImportError> {
    let vmd_bytes = std::fs::read(vmd_path).map_err(VmdImportError::Io)?;
    let pmx_bytes = std::fs::read(pmx_path).map_err(VmdImportError::Io)?;
    check_vmd_pmx_compatibility_bytes(&vmd_bytes, &pmx_bytes)
}

/// Compares already-loaded VMD and PMX bytes. This is the deterministic core
/// used by both the path API and unit tests.
pub fn check_vmd_pmx_compatibility_bytes(
    vmd_bytes: &[u8],
    pmx_bytes: &[u8],
) -> Result<VmdPmxCompatibilityReport, VmdImportError> {
    let vmd = parse_vmd_shared_context(vmd_bytes)
        .map_err(|error| VmdImportError::Parse(error.to_string()))?;
    let pmx = parse_pmx_model(pmx_bytes)
        .map_err(|error| VmdImportError::Rig(error.to_string()))?;
    Ok(analyze_vmd_pmx_compatibility(
        vmd.parsed_animation(),
        &pmx,
    ))
}

fn analyze_vmd_pmx_compatibility(
    vmd: &VmdParsedAnimation,
    pmx: &PmxParsedModel,
) -> VmdPmxCompatibilityReport {
    let mut pmx_bones = BTreeMap::<&str, Vec<&PmxParsedBone>>::new();
    for bone in &pmx.skeleton.bones {
        pmx_bones.entry(bone.name.as_str()).or_default().push(bone);
    }
    let mut pmx_morph_counts = BTreeMap::<&str, usize>::new();
    for morph in &pmx.morphs {
        *pmx_morph_counts.entry(morph.name.as_str()).or_default() += 1;
    }

    let mut used_bones = BTreeMap::<&str, UsedBoneTrack>::new();
    for frame in &vmd.bone_frames {
        let track = used_bones.entry(frame.bone_name.as_str()).or_default();
        track.keyframe_count += 1;
        track.uses_translation |= frame
            .translation
            .iter()
            .any(|component| component.abs() > VMD_COMPATIBILITY_NEUTRAL_EPSILON);
        track.uses_rotation |= !is_neutral_vmd_rotation(frame.rotation);
    }
    used_bones.retain(|_, track| track.uses_translation || track.uses_rotation);

    let mut used_morphs = BTreeMap::<&str, UsedMorphTrack>::new();
    for frame in &vmd.morph_frames {
        let track = used_morphs.entry(frame.morph_name.as_str()).or_default();
        track.keyframe_count += 1;
        track.is_used |= frame.weight.abs() > VMD_COMPATIBILITY_NEUTRAL_EPSILON;
    }
    used_morphs.retain(|_, track| track.is_used);

    let mut issues = Vec::new();
    let mut bones = VmdPmxCompatibilitySummary {
        used_tracks: used_bones.len(),
        unique_tracks: 0,
        missing_tracks: 0,
        ambiguous_tracks: 0,
    };
    for (name, track) in used_bones {
        match pmx_bones.get(name).map(Vec::as_slice).unwrap_or_default() {
            [] => {
                bones.missing_tracks += 1;
                issues.push(compatibility_issue(
                    VmdPmxCompatibilityIssueKind::MissingBone,
                    name,
                    track.keyframe_count,
                ));
            }
            [bone] => {
                bones.unique_tracks += 1;
                if track.uses_rotation && !(bone.flags.enabled && bone.flags.rotatable) {
                    issues.push(compatibility_issue(
                        VmdPmxCompatibilityIssueKind::RotationUnsupported,
                        name,
                        track.keyframe_count,
                    ));
                }
                if track.uses_translation && !(bone.flags.enabled && bone.flags.translatable) {
                    issues.push(compatibility_issue(
                        VmdPmxCompatibilityIssueKind::TranslationUnsupported,
                        name,
                        track.keyframe_count,
                    ));
                }
            }
            _ => {
                bones.ambiguous_tracks += 1;
                issues.push(compatibility_issue(
                    VmdPmxCompatibilityIssueKind::AmbiguousBone,
                    name,
                    track.keyframe_count,
                ));
            }
        }
    }

    let mut morphs = VmdPmxCompatibilitySummary {
        used_tracks: used_morphs.len(),
        unique_tracks: 0,
        missing_tracks: 0,
        ambiguous_tracks: 0,
    };
    for (name, track) in used_morphs {
        match pmx_morph_counts.get(name).copied().unwrap_or_default() {
            0 => {
                morphs.missing_tracks += 1;
                issues.push(compatibility_issue(
                    VmdPmxCompatibilityIssueKind::MissingMorph,
                    name,
                    track.keyframe_count,
                ));
            }
            1 => morphs.unique_tracks += 1,
            _ => {
                morphs.ambiguous_tracks += 1;
                issues.push(compatibility_issue(
                    VmdPmxCompatibilityIssueKind::AmbiguousMorph,
                    name,
                    track.keyframe_count,
                ));
            }
        }
    }
    issues.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.name.cmp(&right.name)));

    VmdPmxCompatibilityReport {
        recorded_model_name: vmd.metadata.model_name.clone(),
        pmx_model_name: pmx.metadata.name.clone(),
        bones,
        morphs,
        issues,
    }
}

fn compatibility_issue(
    kind: VmdPmxCompatibilityIssueKind,
    name: &str,
    keyframe_count: usize,
) -> VmdPmxCompatibilityIssue {
    VmdPmxCompatibilityIssue {
        kind,
        name: name.to_owned(),
        keyframe_count,
    }
}

fn is_neutral_vmd_rotation(rotation: [f32; 4]) -> bool {
    rotation[..3]
        .iter()
        .all(|component| component.abs() <= VMD_COMPATIBILITY_NEUTRAL_EPSILON)
        && (rotation[3].abs() - 1.0).abs() <= VMD_COMPATIBILITY_NEUTRAL_EPSILON
}

/// Derived-cache domain for baked VMD clips, shared with ADR 0079's baked
/// retarget outputs: both are rebuildable `AnimationClip` bytes, and cache
/// keys are content hashes that cannot collide across producers.
const VMD_CACHE_DOMAIN: &str = "anim";

/// File extension for a cached bake envelope.
const VMD_CACHE_EXTENSION: &str = "json";

/// Schema version of the cached bake envelope (`CachedBake`).
///
/// A cache entry written under a different version is treated as a miss and
/// rebaked, so this never needs a migration path.
const CACHED_BAKE_SCHEMA_VERSION: u32 = 1;

/// Largest translation difference, in meters, still treated as "unchanged"
/// when reducing a baked track to keyframes.
///
/// One hundredth of a millimeter: far below anything visible on a
/// character-scale rig, and far above the float noise a matrix decomposition
/// introduces.
const TRANSLATION_EPSILON: f32 = 1.0e-5;

/// Largest rotation difference, measured as `1 - |dot|` between two
/// quaternions, still treated as "unchanged" when reducing a baked track.
const ROTATION_EPSILON: f32 = 1.0e-7;

/// Largest rest-translation disagreement, in meters, between the PMX rig and
/// the bound [`SkeletonAsset`] before `vmd.rest_pose_mismatch` is reported.
///
/// Deliberately looser than [`TRANSLATION_EPSILON`]: this check exists to
/// catch a clip baked against the wrong model, not to police rounding.
const REST_POSE_TOLERANCE: f32 = 1.0e-3;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Reports a fatal error that prevents a VMD bake from completing.
///
/// Per-bone and per-curve problems are *not* errors: they are recorded as
/// [`Diagnostic`]s on [`VmdImportResult::diagnostics`] and the bake
/// continues, mirroring [`crate::gltf_import`] / [`crate::fbx_import`].
#[derive(Debug)]
pub enum VmdImportError {
    /// A source file could not be read.
    Io(std::io::Error),
    /// The `.pmx` bytes backing a [`VmdBakeRig`] are not a valid PMX rig.
    Rig(String),
    /// The `.vmd` bytes are not a valid motion document.
    Parse(String),
    /// The VMD contains only scene-level tracks and cannot produce a model
    /// [`AnimationClip`].
    SceneMotionUnsupported,
    /// The VMD contains no model or scene animation keys.
    EmptyMotion,
    /// The motion could not be bound to the rig's bone table.
    ClipBuild(String),
    /// The paired model source is not a `.pmx` file, so it carries no MMD rig
    /// to bake against.
    ModelNotPmx(std::path::PathBuf),
    /// The paired model source failed to import.
    Model(String),
    /// The paired model imported but produced no skinned skeleton, so a
    /// motion has nothing to bind its bone curves to.
    ModelHasNoSkeleton(std::path::PathBuf),
    /// A cached bake could not be written.
    Cache(std::io::Error),
    /// A cache key or cache entry could not be serialized.
    Serialize(serde_json::Error),
}

impl fmt::Display for VmdImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to read the VMD source: {error}"),
            Self::Rig(reason) => write!(f, "failed to load the PMX rig for VMD baking: {reason}"),
            Self::Parse(reason) => write!(f, "failed to parse the VMD motion: {reason}"),
            Self::SceneMotionUnsupported => write!(
                f,
                "the VMD contains camera, light, or self-shadow tracks but no model motion; scene motion playback is not implemented"
            ),
            Self::EmptyMotion => write!(f, "the VMD contains no animation keyframes"),
            Self::ClipBuild(reason) => {
                write!(f, "failed to bind the VMD motion to the rig: {reason}")
            }
            Self::ModelNotPmx(path) => write!(
                f,
                "the paired model `{}` is not a .pmx file, so it carries no MMD rig to bake against",
                path.display()
            ),
            Self::Model(reason) => write!(f, "failed to import the paired PMX model: {reason}"),
            Self::ModelHasNoSkeleton(path) => write!(
                f,
                "the paired model `{}` imported no skinned skeleton, so a motion has nothing to bind to",
                path.display()
            ),
            Self::Cache(error) => write!(f, "failed to write the baked VMD cache entry: {error}"),
            Self::Serialize(error) => write!(f, "failed to serialize baked VMD clip data: {error}"),
        }
    }
}

impl std::error::Error for VmdImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Cache(error) => Some(error),
            Self::Serialize(error) => Some(error),
            Self::Rig(_)
            | Self::Parse(_)
            | Self::SceneMotionUnsupported
            | Self::EmptyMotion
            | Self::ClipBuild(_)
            | Self::ModelNotPmx(_)
            | Self::Model(_)
            | Self::ModelHasNoSkeleton(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Rig
// ---------------------------------------------------------------------------

/// A PMX rig prepared for baking VMD motions onto one imported
/// [`SkeletonAsset`].
///
/// Holds everything a bake needs that a [`SkeletonAsset`] cannot express —
/// IK chains, appended-parent links, fixed/local axes, bone morphs, and the
/// MMD name tables — plus the PMX-bone-to-[`BoneId`] binding described in
/// the module documentation. Build it once per model and reuse it for every
/// `.vmd` baked against that model; constructing it reparses the `.pmx`,
/// which the per-motion entry points deliberately never do.
///
/// # Examples
///
/// ```no_run
/// use engine_rig::skeleton_asset::SkeletonAsset;
/// use engine_import::vmd_import::{import_vmd_path, VmdBakeOptions, VmdBakeRig};
/// use engine_authoring::id::AssetId;
/// use std::path::Path;
///
/// # fn run(skeleton: &SkeletonAsset) -> Result<(), Box<dyn std::error::Error>> {
/// let rig = VmdBakeRig::from_pmx_path(Path::new("character.pmx"), skeleton)?;
/// let motion_id = AssetId::generate();
/// let baked = import_vmd_path(
///     &motion_id,
///     Path::new("dance.vmd"),
///     &rig,
///     &VmdBakeOptions::default(),
/// )?;
/// assert_eq!(baked.clips.len(), 1);
/// # Ok(())
/// # }
/// ```
pub struct VmdBakeRig {
    /// The evaluator's model. `Arc` because [`RuntimeInstance`] requires
    /// shared ownership, and one rig may be baked against concurrently.
    model: Arc<ModelArena>,
    /// PMX bone name (raw and UTF-8 decoded) -> rig bone index, used by
    /// `mmd-anim-format` to bind VMD bone curves.
    bone_name_to_index: HashMap<Vec<u8>, BoneIndex>,
    /// PMX morph name -> rig morph index. Bone morphs move bones, so this is
    /// bound even though no morph channel is emitted (ADR 0097 §5).
    morph_name_to_index: HashMap<Vec<u8>, MorphIndex>,
    /// Renderable PMX morph names collected from the imported model. `None`
    /// is used by the low-level PMX-only constructor, whose caller has no
    /// mesh catalog and therefore cannot distinguish bone-only morphs.
    renderable_morph_names: Option<HashSet<String>>,
    /// PMX IK bone name -> IK solver index, used to apply the VMD's per-frame
    /// IK enable/disable property track.
    ik_solver_bone_name_to_index: HashMap<Vec<u8>, usize>,
    /// Parallel to the rig's bones: the engine [`BoneId`] each PMX bone
    /// drives, or `None` when the skeleton has no bone of that name.
    bone_targets: Vec<Option<BoneId>>,
    /// Parallel to [`Self::bone_targets`]: the bound bone's engine-space rest
    /// TRS, the reference every baked sample is reduced against.
    bone_rest: Vec<(Vec3, Quat)>,
    /// The skeleton every baked clip declares and is contact-detected against
    /// (ADR 0077, ADR 0080 §1).
    ///
    /// Owned rather than borrowed so a rig outlives the import call that
    /// built it and can bake any number of motions afterwards.
    skeleton: SkeletonAsset,
    /// Content hash of the `.pmx` bytes this rig was built from.
    ///
    /// Part of every cache key: two models can share a
    /// [`SkeletonIdentity`] (identical bone names, parents, and rest pose)
    /// while differing in IK chains or appended-parent links, which changes
    /// the bake but not the skeleton, so the identity alone cannot key the
    /// cache.
    fingerprint: u64,
    /// Binding-time diagnostics, replayed onto every clip baked with this rig
    /// so a mismatch is reported per import rather than only at rig
    /// construction.
    diagnostics: Vec<Diagnostic>,
}

/// The half-width and full-width spellings of `name`, whichever differs from
/// it.
///
/// Unicode's fullwidth forms `U+FF01..=U+FF5E` are the printable ASCII range
/// shifted by `0xFEE0`, so the two spellings convert into each other by that
/// one offset.
fn width_variants(name: &str) -> Vec<String> {
    const SHIFT: u32 = 0xFEE0;
    let narrow: String = name
        .chars()
        .map(|character| {
            let code = character as u32;
            if ('\u{FF01}'..='\u{FF5E}').contains(&character) {
                char::from_u32(code - SHIFT).unwrap_or(character)
            } else {
                character
            }
        })
        .collect();
    let wide: String = name
        .chars()
        .map(|character| {
            let code = character as u32;
            if ('!'..='~').contains(&character) {
                char::from_u32(code + SHIFT).unwrap_or(character)
            } else {
                character
            }
        })
        .collect();
    [narrow, wide]
        .into_iter()
        .filter(|variant| variant != name)
        .collect()
}

/// Registers the opposite-width spelling of every entry in a VMD-facing name
/// lookup, so a motion finds a bone the model spells the other way.
///
/// MMD treats `上半身2` and `上半身２` as the same bone, and motion files in
/// the wild mix the two freely — the same file names one bone `右足IK親` and
/// another `右足ＩＫ`. Matching on bytes alone therefore silently drops whole
/// tracks: this project's `man.vmd` keys the upper body as `上半身２` while
/// the model spells it `上半身2`, which left the character's torso unanimated.
///
/// An alias is only added where no bone already claims that spelling, so a
/// model that really does contain both spellings keeps its own binding.
fn insert_width_normalized_aliases<T: Copy>(lookup: &mut HashMap<Vec<u8>, T>) {
    let aliases = lookup
        .iter()
        .filter_map(|(name, index)| {
            let name = std::str::from_utf8(name).ok()?;
            Some(
                width_variants(name)
                    .into_iter()
                    .map(|variant| (variant.into_bytes(), *index))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect::<Vec<_>>();
    for (name, index) in aliases {
        lookup.entry(name).or_insert(index);
    }
}

impl VmdBakeRig {
    /// Prepares a rig from `.pmx` bytes and the [`SkeletonAsset`] that
    /// [`crate::pmx_import`] produced from those same bytes.
    ///
    /// Binding problems (a PMX bone the skeleton lacks, a rest pose that
    /// disagrees) are recorded on [`Self::diagnostics`] and replayed onto
    /// every baked clip; they never fail construction, since a partially
    /// bound rig still bakes every bone it could bind.
    ///
    /// # Errors
    ///
    /// Returns [`VmdImportError::Rig`] when `bytes` is not a valid PMX
    /// document or its bone table cannot be compiled into an evaluator model.
    pub fn from_pmx_bytes(bytes: &[u8], skeleton: &SkeletonAsset) -> Result<Self, VmdImportError> {
        let PmxRuntimeImport {
            model,
            bone_names,
            mut bone_name_to_index,
            morph_name_to_index,
            mut ik_solver_bone_name_to_index,
        } = import_pmx_runtime(bytes).map_err(|error| VmdImportError::Rig(error.to_string()))?;
        insert_width_normalized_aliases(&mut bone_name_to_index);
        insert_width_normalized_aliases(&mut ik_solver_bone_name_to_index);

        // PMX local-axis descriptors define the axes presented to an author
        // while manipulating a bone in local mode. They do not redefine the
        // coordinate frame used by the PMX IK link angle-limit vectors.
        //
        // mmd-anim-runtime 0.4 currently applies these descriptors as an IK
        // limit basis. On semi-standard MMD legs whose knee local X axis
        // points toward -X, that changes the usual negative-X knee limit into
        // a positive-X bend and makes the knee fold backwards.
        //
        // Clear only the evaluator's local-axis descriptors. Fixed-axis
        // constraints, IK links, IK angle limits, appended transforms, bone
        // hierarchy, rest transforms, and the imported SkeletonAsset remain
        // unchanged.
        let bone_count = model.bone_count();
        let model = model.with_local_axes(std::iter::repeat_with(|| None).take(bone_count));

        let mut diagnostics = Vec::new();
        let bone_targets = bind_bone_targets(&bone_names, skeleton, &mut diagnostics);
        let bone_rest = collect_bone_rest(&bone_targets, skeleton);
        validate_rest_pose(&model, &bone_targets, &bone_rest, &mut diagnostics);

        Ok(Self {
            model: Arc::new(model),
            bone_name_to_index,
            morph_name_to_index,
            renderable_morph_names: None,
            ik_solver_bone_name_to_index,
            bone_targets,
            bone_rest,
            skeleton: skeleton.clone(),
            fingerprint: fnv1a64(bytes),
            diagnostics,
        })
    }

    /// Prepares a rig from a `.pmx` file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`VmdImportError::Io`] when the file cannot be read, or
    /// [`VmdImportError::Rig`] when its contents are not a valid PMX rig.
    pub fn from_pmx_path(path: &Path, skeleton: &SkeletonAsset) -> Result<Self, VmdImportError> {
        let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
        Self::from_pmx_bytes(&bytes, skeleton)
    }

    /// Prepares a rig from an already-imported PMX model, reusing the
    /// skeleton that import resolved.
    ///
    /// This is the constructor to prefer whenever the caller holds a
    /// `GltfImportResult` already — the runtime scene bridge caches one per
    /// source — because it takes the skeleton from that result (so the baked
    /// clip's [`BoneId`]s are exactly the ones the model import assigned,
    /// including any dedupe or rebind the ADR 0077 rules applied) instead of
    /// importing the model a second time.
    ///
    /// `model_bytes` must be the same `.pmx` bytes `imported` was built from;
    /// they carry the IK chains, appended-parent links, and bone morphs that
    /// `GltfImportResult` deliberately does not (see [`crate::pmx_import`]'s
    /// normalization contract).
    ///
    /// # Errors
    ///
    /// Returns [`VmdImportError::ModelHasNoSkeleton`] when `imported` carries
    /// no skin, or [`VmdImportError::Rig`] when `model_bytes` is not a valid
    /// PMX document.
    pub fn from_model_import(
        model_path: &Path,
        model_bytes: &[u8],
        imported: &GltfImportResult,
    ) -> Result<Self, VmdImportError> {
        // Under `SkeletonScope::SharedAcrossDocument` (ADR 0097 §4a) every
        // split render part binds the same skeleton, so the first skin's is
        // the document's one rig rather than an arbitrary pick.
        let skeleton = imported
            .skins
            .first()
            .map(|skin| &skin.skeleton)
            .ok_or_else(|| VmdImportError::ModelHasNoSkeleton(model_path.to_path_buf()))?;
        let mut rig = Self::from_pmx_bytes(model_bytes, skeleton)?;
        rig.renderable_morph_names = Some(
            imported
                .meshes
                .iter()
                .flat_map(|mesh| mesh.morphs.iter().map(|morph| morph.name.clone()))
                .collect(),
        );
        Ok(rig)
    }

    /// Imports the `.pmx` at `model_path` and prepares a rig from it.
    ///
    /// Use [`Self::from_model_import`] instead when the model has already
    /// been imported; this constructor exists for the editor's import worker,
    /// which starts from nothing but a path.
    ///
    /// # Errors
    ///
    /// Returns [`VmdImportError::ModelNotPmx`] when `model_path` is not a
    /// `.pmx`, [`VmdImportError::Io`] when it cannot be read,
    /// [`VmdImportError::Model`] when it fails to import, or
    /// [`VmdImportError::ModelHasNoSkeleton`] when it carries no skinned rig.
    pub fn from_model_path(
        model_source_id: &AssetId,
        model_path: &Path,
        existing_skeletons: &[crate::asset::SkeletonRecord],
    ) -> Result<Self, VmdImportError> {
        if !model_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        {
            return Err(VmdImportError::ModelNotPmx(model_path.to_path_buf()));
        }
        let model_bytes = std::fs::read(model_path).map_err(VmdImportError::Io)?;
        let imported =
            crate::pmx_import::import_pmx_bytes(model_source_id, &model_bytes, existing_skeletons)
                .map_err(|error| VmdImportError::Model(error.to_string()))?;
        Self::from_model_import(model_path, &model_bytes, &imported)
    }

    /// Returns the diagnostics recorded while binding this rig to its
    /// skeleton. Every clip baked with this rig repeats them.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Returns the number of bones in the PMX rig.
    pub fn bone_count(&self) -> usize {
        self.model.bone_count()
    }

    /// Returns the number of PMX bones bound to a skeleton bone.
    pub fn bound_bone_count(&self) -> usize {
        self.bone_targets
            .iter()
            .filter(|target| target.is_some())
            .count()
    }

    /// Returns the skeleton every clip baked with this rig declares.
    pub fn skeleton(&self) -> &SkeletonAsset {
        &self.skeleton
    }
}

/// Matches every PMX bone name to a [`BoneId`] in `skeleton`, consuming
/// equal names in order so duplicate PMX bone names still bind one-to-one
/// (see the module documentation).
fn bind_bone_targets(
    bone_names: &[String],
    skeleton: &SkeletonAsset,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<BoneId>> {
    // Name -> the skeleton bone positions carrying it, in skeleton order, so
    // the n-th PMX bone of a repeated name takes the n-th skeleton bone.
    let mut by_name: HashMap<&str, std::collections::VecDeque<usize>> = HashMap::new();
    for (index, bone) in skeleton.bones.iter().enumerate() {
        by_name.entry(bone.name.as_str()).or_default().push_back(index);
    }

    let mut unbound = Vec::new();
    let targets: Vec<Option<BoneId>> = bone_names
        .iter()
        .map(|name| {
            let bone_index = by_name.get_mut(name.as_str()).and_then(|queue| queue.pop_front());
            match bone_index {
                Some(index) => Some(skeleton.bones[index].id),
                None => {
                    unbound.push(name.clone());
                    None
                }
            }
        })
        .collect();

    if !unbound.is_empty() {
        diagnostics.push(Diagnostic::warning(
            "vmd.bone_not_in_skeleton",
            format!(
                "{} PMX bones have no bone of the same name in the bound skeleton and drive no channel: {}",
                unbound.len(),
                summarize_names(&unbound)
            ),
        ));
    }

    targets
}

/// Collects each bound bone's engine-space rest TRS, in PMX bone order.
///
/// Unbound bones get an identity entry that is never read, keeping this list
/// index-aligned with `bone_targets` instead of needing a second lookup in
/// the sampling loop.
fn collect_bone_rest(bone_targets: &[Option<BoneId>], skeleton: &SkeletonAsset) -> Vec<(Vec3, Quat)> {
    bone_targets
        .iter()
        .map(|target| {
            target
                .and_then(|bone_id| skeleton.bone_index(bone_id))
                .and_then(|index| skeleton.bones.get(index))
                .map(|bone| (bone.rest_translation, bone.rest_rotation))
                .unwrap_or((Vec3::ZERO, Quat::IDENTITY))
        })
        .collect()
}

/// Reports `vmd.rest_pose_mismatch` when a bound bone's rest translation in
/// the skeleton disagrees with the PMX rig's own.
///
/// A VMD baked against the wrong model produces a plausible-looking but
/// wrong clip rather than an error, so this is the one check that can tell
/// an author the pairing itself is wrong.
fn validate_rest_pose(
    model: &ModelArena,
    bone_targets: &[Option<BoneId>],
    bone_rest: &[(Vec3, Quat)],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut mismatched = 0usize;
    let mut worst = 0.0_f32;
    for (index, target) in bone_targets.iter().enumerate() {
        if target.is_none() {
            continue;
        }
        let rig_rest = convert_position(model.rest_position(BoneIndex(index as u32)).to_array());
        let distance = (rig_rest - bone_rest[index].0).length();
        if distance > REST_POSE_TOLERANCE {
            mismatched += 1;
            worst = worst.max(distance);
        }
    }
    if mismatched > 0 {
        diagnostics.push(Diagnostic::warning(
            "vmd.rest_pose_mismatch",
            format!(
                "{mismatched} bones rest at a different position in the PMX rig than in the bound skeleton (worst {worst:.4} m); the motion is probably being baked against a different model"
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// Options and results
// ---------------------------------------------------------------------------

/// Tunables for one VMD bake.
#[derive(Debug, Clone)]
pub struct VmdBakeOptions {
    /// Poses sampled per second. Defaults to [`DEFAULT_VMD_SAMPLE_RATE`],
    /// which takes exactly one sample per source VMD frame; a higher rate
    /// resolves IK motion between source frames at proportional cost. A
    /// non-finite or non-positive value falls back to the default with a
    /// `vmd.sample_rate_invalid` diagnostic.
    pub sample_rate: f32,
    /// Ground-contact candidate bone names (ADR 0080 §1), forwarded to
    /// [`crate::contact_detect::detect_contact_intervals`] exactly as
    /// `crate::asset::ImportSettings::contact_bones` is for glTF/FBX. Empty
    /// keeps that module's built-in heuristic.
    pub contact_bone_names: Vec<String>,
}

impl Default for VmdBakeOptions {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_VMD_SAMPLE_RATE,
            contact_bone_names: Vec::new(),
        }
    }
}

/// One baked clip and the stable sub-asset identity it was assigned.
#[derive(Debug, Clone)]
pub struct VmdBakedClip {
    /// Deterministic zero-based selector within the source `.vmd`, and the
    /// index [`Self::id`] derives from.
    pub source_index: usize,
    /// Stable sub-asset ID, derived exactly like every other imported
    /// animation clip's (`crate::asset::imported_sub_asset_id`), so a
    /// re-import of the same file rebinds rather than orphaning.
    pub id: AssetId,
    /// Human-readable clip name.
    pub name: String,
    /// The baked clip: plain FK curves, ready for an Animation Set's Motion
    /// Slot with no MMD-specific runtime anywhere downstream.
    pub clip: AnimationClip,
}

/// Everything one `.vmd` import produced.
#[derive(Debug, Clone)]
pub struct VmdImportResult {
    /// The baked clips.
    ///
    /// A `Vec` because ADR 0097 §3 anticipates VMD revisions carrying several
    /// named motions; today's parser always yields exactly one, at
    /// `source_index` 0.
    pub clips: Vec<VmdBakedClip>,
    /// Non-fatal problems recorded while binding and baking, including the
    /// rig's own binding diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// PMX model source selected for the stable IDs in [`Self::clips`].
    ///
    /// Low-level VMD parsing leaves this unset. Asset-pipeline callers invoke
    /// [`Self::bind_model_source`] after choosing a PMX target.
    model_source: Option<AssetId>,
}

// ---------------------------------------------------------------------------
// Import entry points
// ---------------------------------------------------------------------------

/// Bakes a `.vmd` byte slice against `rig` into one or more
/// [`AnimationClip`]s.
///
/// `clip_name` names the produced clip; [`import_vmd_path`] passes the source
/// file's stem. Sub-asset IDs are deterministic: the same `source_id` and
/// selector always derive the same [`AssetId`], independent of `bytes`.
///
/// # Errors
///
/// Returns [`VmdImportError::Parse`] when `bytes` is not a valid VMD
/// document, or [`VmdImportError::ClipBuild`] when the motion cannot be bound
/// to the rig's bone table.
pub fn import_vmd_bytes(
    source_id: &AssetId,
    bytes: &[u8],
    clip_name: &str,
    rig: &VmdBakeRig,
    options: &VmdBakeOptions,
) -> Result<VmdImportResult, VmdImportError> {
    let context =
        parse_vmd_shared_context(bytes).map_err(|error| VmdImportError::Parse(error.to_string()))?;
    let content_kind = classify_vmd_summary(context.summary());
    match content_kind {
        VmdContentKind::Scene => return Err(VmdImportError::SceneMotionUnsupported),
        VmdContentKind::Empty => return Err(VmdImportError::EmptyMotion),
        VmdContentKind::Model | VmdContentKind::Mixed => {}
    }
    let mut diagnostics = rig.diagnostics.clone();
    if content_kind == VmdContentKind::Mixed {
        diagnostics.push(Diagnostic::warning(
            "vmd.scene_channels_ignored",
            "the VMD contains model and scene channels; camera, light, and self-shadow tracks were ignored while importing the model motion",
        ));
    }
    let morph_channels = build_morph_channels(
        &context.parsed_animation().morph_frames,
        rig.renderable_morph_names.as_ref(),
        &mut diagnostics,
    );
    let clip = bake_clip(
        rig,
        context.import_result(),
        morph_channels,
        clip_name,
        options,
        &mut diagnostics,
    )?;
    Ok(VmdImportResult {
        clips: vec![VmdBakedClip {
            source_index: 0,
            id: imported_sub_asset_id(source_id, ImportedSubAssetKind::Animation, 0),
            name: clip_name.to_owned(),
            clip,
        }],
        diagnostics,
        model_source: None,
    })
}

/// Bakes a `.vmd` file from disk against `rig`, naming the clip after the
/// file stem.
///
/// # Errors
///
/// Returns [`VmdImportError::Io`] when the file cannot be read; otherwise see
/// [`import_vmd_bytes`].
pub fn import_vmd_path(
    source_id: &AssetId,
    path: &Path,
    rig: &VmdBakeRig,
    options: &VmdBakeOptions,
) -> Result<VmdImportResult, VmdImportError> {
    let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
    import_vmd_bytes(source_id, &bytes, &clip_name_from_path(path), rig, options)
}

// ---------------------------------------------------------------------------
// Asset-pipeline entry points
// ---------------------------------------------------------------------------

/// Returns `true` when `path` names a motion source this module can import.
///
/// The same predicate `asset_path_matches_kind` applies
/// to `AssetKind::MotionSource`, kept here so importer
/// code can route without depending on the component metadata layer.
pub fn is_motion_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vmd"))
}

/// Returns the external files a motion source's import depends on.
///
/// A `.vmd` has no sidecars of its own, but its bake is a function of the
/// paired `.pmx` too, so that file is the dependency: recording it means
/// editing the model invalidates every motion baked against it, exactly the
/// way a glTF's `.bin`/image sidecars invalidate its own import
/// ([`crate::model_import::model_source_dependencies`]).
pub fn motion_source_dependencies(model_path: &Path) -> Vec<std::path::PathBuf> {
    vec![model_path.to_path_buf()]
}

/// Returns every PMX dependency for a VMD source with several bake targets.
///
/// Paths are deduplicated and sorted so authoring-only target list reordering
/// does not create a different persisted dependency list.
pub fn motion_source_dependencies_for_models(
    model_paths: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut dependencies = model_paths.to_vec();
    dependencies.sort();
    dependencies.dedup();
    dependencies
}

/// Computes a deterministic content fingerprint over a motion source and the
/// model it is baked against.
///
/// Both files are hashed because either one changing changes the baked clip.
/// Only file names (not absolute paths) enter the hash, so moving a project
/// does not force a reimport — parity with
/// [`crate::pmx_import::fingerprint_pmx_source`].
///
/// # Errors
///
/// Returns [`VmdImportError::Io`] when either file cannot be read.
pub fn fingerprint_motion_source(
    motion_path: &Path,
    model_path: &Path,
) -> Result<String, VmdImportError> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in [motion_path, model_path] {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        hash = fnv1a64_seeded(hash, label.as_bytes());
        let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
        hash = fnv1a64_seeded(hash, &bytes);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

/// Computes one deterministic fingerprint for a VMD and all selected PMX
/// bake targets.
///
/// Target paths are sorted before hashing because their UI order does not
/// affect any baked clip. Adding, removing, renaming, or editing a target PMX
/// still changes the fingerprint and invalidates dependent derived content.
pub fn fingerprint_motion_sources(
    motion_path: &Path,
    model_paths: &[std::path::PathBuf],
) -> Result<String, VmdImportError> {
    let mut paths = motion_source_dependencies_for_models(model_paths);
    paths.insert(0, motion_path.to_path_buf());
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for path in paths {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        hash = fnv1a64_seeded(hash, label.as_bytes());
        let bytes = std::fs::read(&path).map_err(VmdImportError::Io)?;
        hash = fnv1a64_seeded(hash, &bytes);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
}

/// Imports one `.vmd` motion source against its paired `.pmx` model, the way
/// the asset pipeline registers it (ADR 0097 §3).
///
/// This is the counterpart of
/// [`crate::model_import::import_model_path_with_contact_bones`] for
/// animation-only sources: it imports `model_path` to obtain the shared
/// [`SkeletonAsset`] the baked clip must bind to (so the clip's
/// [`crate::skeleton_asset::BoneId`]s are the same ones the model import
/// assigned, ADR 0077), builds the evaluator rig from the same file, and
/// bakes.
///
/// `model_source_id` must be the *model's* registered [`AssetId`], not the
/// motion's: skeleton sub-asset IDs derive from the source that owns the rig,
/// and passing the motion's ID would mint a second identity for the same
/// skeleton. `source_id` is the motion's own ID, from which the produced
/// clip sub-assets derive.
///
/// Callers that already hold the model's `GltfImportResult` — the runtime
/// scene bridge caches one per source — should call
/// [`VmdBakeRig::from_model_import`] and [`import_vmd_path`] directly instead,
/// so the model is not parsed twice.
///
/// # Errors
///
/// Returns [`VmdImportError::ModelNotPmx`] when `model_path` is not a `.pmx`,
/// [`VmdImportError::Model`] when it fails to import,
/// [`VmdImportError::ModelHasNoSkeleton`] when it carries no skinned rig, or
/// whatever [`import_vmd_path`] would.
pub fn import_motion_path(
    source_id: &AssetId,
    motion_path: &Path,
    model_source_id: &AssetId,
    model_path: &Path,
    existing_skeletons: &[crate::asset::SkeletonRecord],
    options: &VmdBakeOptions,
) -> Result<VmdImportResult, VmdImportError> {
    let rig = VmdBakeRig::from_model_path(model_source_id, model_path, existing_skeletons)?;
    let mut imported = import_vmd_path(source_id, motion_path, &rig, options)?;
    imported.bind_model_source(source_id, model_source_id);
    Ok(imported)
}

impl VmdImportResult {
    /// Rebinds this import result's sub-asset IDs to one PMX model source.
    ///
    /// The baked channel data already targets `rig`'s skeleton. This method
    /// only establishes the persistent identity required for several bakes of
    /// the same VMD to coexist in one manifest.
    pub fn bind_model_source(&mut self, motion_source: &AssetId, model_source: &AssetId) {
        for clip in &mut self.clips {
            clip.id = imported_motion_sub_asset_id(
                motion_source,
                model_source,
                clip.source_index,
            );
        }
        self.model_source = Some(model_source.clone());
    }

    /// Retargets every baked FK clip to `target_skeleton`, then assigns the
    /// same target-specific persistent IDs used by a direct VMD bake.
    ///
    /// The source VMD has already been evaluated against its original PMX at
    /// this point, so this operation contains no MMD-specific evaluation. It
    /// uses the engine's ordinary explicit retarget map and re-detects target
    /// contacts through [`crate::retarget::retarget_clip`].
    pub fn retarget_to_model_source(
        &mut self,
        motion_source: &AssetId,
        target_model_source: &AssetId,
        source_skeleton: &SkeletonAsset,
        target_skeleton: &SkeletonAsset,
        map: &crate::retarget::RetargetMap,
        target_contact_bone_names: &[String],
    ) -> Result<(), crate::retarget::RetargetError> {
        for baked in &mut self.clips {
            baked.clip = crate::retarget::retarget_clip(
                &baked.clip,
                source_skeleton,
                target_skeleton,
                map,
                target_contact_bone_names,
            )?;
        }
        self.bind_model_source(motion_source, target_model_source);
        Ok(())
    }

    /// Builds the persistent stable-ID catalog for this motion import, the
    /// same contract [`crate::model_import::GltfImportResult::
    /// imported_sub_assets`] provides for model sources.
    ///
    /// Every entry is an [`ImportedSubAssetKind::Animation`]: a motion source
    /// produces clips and nothing else.
    pub fn imported_sub_assets(&self) -> Vec<crate::asset::ImportedSubAsset> {
        self.clips
            .iter()
            .map(|baked| crate::asset::ImportedSubAsset {
                id: baked.id.as_str().to_owned(),
                kind: ImportedSubAssetKind::Animation,
                name: baked.name.clone(),
                index: u32::try_from(baked.source_index).unwrap_or(u32::MAX),
                target_model_source: self
                    .model_source
                    .as_ref()
                    .map(|model| model.as_str().to_owned()),
            })
            .collect()
    }
}

/// Names a clip after its source file stem, falling back to `"motion"` for a
/// path with no usable stem.
fn clip_name_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "motion".to_owned())
}

// ---------------------------------------------------------------------------
// Bake
// ---------------------------------------------------------------------------

/// Runs `mmd-anim-runtime`'s evaluator across the whole motion and reduces
/// the sampled poses to an [`AnimationClip`].
fn bake_clip(
    rig: &VmdBakeRig,
    motion: &MmdMotion,
    morph_channels: Vec<MorphChannel>,
    clip_name: &str,
    options: &VmdBakeOptions,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<AnimationClip, VmdImportError> {
    report_unbound_curves(rig, motion, diagnostics);

    let runtime_clip = build_mmd_registered_pair_clip(
        &rig.model,
        motion,
        &rig.bone_name_to_index,
        &rig.morph_name_to_index,
        &rig.ik_solver_bone_name_to_index,
        rig.model.ik_count(),
    )
    .map_err(|error| VmdImportError::ClipBuild(error.to_string()))?;

    let sample_rate = resolve_sample_rate(options.sample_rate, diagnostics);
    let end_frame = resolve_end_frame(&runtime_clip, clip_name, diagnostics);
    let frames_per_sample = MMD_FRAME_RATE / sample_rate;
    let sample_count = if end_frame <= 0.0 {
        1
    } else {
        (end_frame / frames_per_sample).ceil() as usize + 1
    };

    let mut tracks = BakedTracks::new(&rig.bone_targets, &rig.bone_rest, sample_count);
    let mut instance = RuntimeInstance::new(Arc::clone(&rig.model));
    for sample in 0..sample_count {
        instance.evaluate_clip_frame(&runtime_clip, sample as f32 * frames_per_sample);
        tracks.record(&instance, &rig.model);
    }

    let channels = tracks.into_channels(|sample| sample as f32 / sample_rate);

    // Resampling is a successful conversion result rather than data loss or
    // an invalid authoring state. Keep it discoverable without adding a
    // permanent warning to the default Problems view.
    diagnostics.push(Diagnostic::info(
        "vmd.curve_resampled",
        format!(
            "motion '{clip_name}' baked {} channels from {sample_count} evaluated poses at {sample_rate:.3} Hz; IK and appended-parent were resolved into plain keyframes",
            channels.len()
        ),
    ));

    let root_bone = crate::model_import::detect_root_bone(&rig.skeleton, &channels);
    let mut clip = AnimationClip {
        duration: end_frame / MMD_FRAME_RATE,
        channels,
        morph_channels,
        // VMD has no native event concept; events are added by code after
        // import, exactly as for glTF/FBX (Phase 59).
        events: Vec::new(),
        skeleton: Some(rig.skeleton.id.clone()),
        skeleton_identity: Some(rig.skeleton.identity),
        root_bone,
        contacts: Vec::new(),
    };
    // Ground-contact detection (ADR 0080 §1) runs after BoneId resolution
    // against the clip's own bound skeleton, exactly as
    // `crate::model_import::build_animations` does for glTF/FBX.
    clip.contacts = crate::contact_detect::detect_contact_intervals(
        &clip,
        &rig.skeleton,
        &options.contact_bone_names,
    );
    Ok(clip)
}

/// Reports VMD curves whose target name is absent from the rig.
///
/// Dropping such a curve is `mmd-anim-format`'s own behavior; this only makes
/// it visible, matching the "drop with a diagnostic" convention used
/// throughout [`crate::gltf_import`] / [`crate::fbx_import`].
fn report_unbound_curves(rig: &VmdBakeRig, motion: &MmdMotion, diagnostics: &mut Vec<Diagnostic>) {
    let mut missing_bones: Vec<String> = Vec::new();
    let mut seen: Vec<&[u8]> = Vec::new();
    for keyframe in &motion.bone_keyframes {
        let name = keyframe.bone_name_normalized.as_slice();
        if rig.bone_name_to_index.contains_key(name) || seen.contains(&name) {
            continue;
        }
        seen.push(name);
        missing_bones.push(String::from_utf8_lossy(name).into_owned());
    }
    if !missing_bones.is_empty() {
        diagnostics.push(Diagnostic::warning(
            "vmd.bone_not_found",
            format!(
                "{} bone curves name a bone this model does not have and were dropped: {}",
                missing_bones.len(),
                summarize_names(&missing_bones)
            ),
        ));
    }

}

/// Converts decoded VMD morph records into deterministic scalar channels.
fn build_morph_channels(
    frames: &[VmdParsedMorphFrame],
    renderable_names: Option<&HashSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<MorphChannel> {
    let mut tracks = BTreeMap::<String, BTreeMap<u32, f32>>::new();
    let mut unavailable = BTreeSet::<String>::new();
    for frame in frames {
        if renderable_names.is_some_and(|names| !names.contains(&frame.morph_name)) {
            unavailable.insert(frame.morph_name.clone());
            continue;
        }
        tracks
            .entry(frame.morph_name.clone())
            .or_default()
            .insert(frame.frame, frame.weight);
    }
    if !unavailable.is_empty() {
        // A distributed face motion can contain dozens of names that the
        // paired model implements only as bone morphs or does not implement at
        // all. Report the complete condition once so it does not bury distinct
        // import failures in the Problems panel.
        let names = unavailable.into_iter().collect::<Vec<_>>();
        diagnostics.push(Diagnostic::warning(
            "vmd.morph_runtime_channel_unavailable",
            format!(
                "{} VMD morphs have no renderable PMX morph target and were omitted from runtime morph channels: {}; bone-morph effects may already be represented by the baked bone pose",
                names.len(),
                summarize_names(&names)
            ),
        ));
    }
    tracks
        .into_iter()
        .map(|(target_name, keyframes)| MorphChannel {
            target_name,
            keyframes: keyframes
                .into_iter()
                .map(|(frame, weight)| Keyframe {
                    time: frame as f32 / MMD_FRAME_RATE,
                    value: [weight, 0.0, 0.0, 0.0],
                })
                .collect(),
        })
        .collect()
}

/// Validates the requested sample rate, falling back to the default.
fn resolve_sample_rate(requested: f32, diagnostics: &mut Vec<Diagnostic>) -> f32 {
    if requested.is_finite() && requested > 0.0 {
        return requested;
    }
    diagnostics.push(Diagnostic::warning(
        "vmd.sample_rate_invalid",
        format!("sample rate {requested} is not a positive finite value; baking at {DEFAULT_VMD_SAMPLE_RATE} Hz instead"),
    ));
    DEFAULT_VMD_SAMPLE_RATE
}

/// Returns the last MMD frame to bake, clamped to [`MAX_BAKED_FRAME`].
fn resolve_end_frame(
    runtime_clip: &mmd_anim_runtime::AnimationClip,
    clip_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> f32 {
    // Sampling always starts at frame 0 even when the first key is later:
    // MMD holds a track at its first key's value before that key, so the
    // leading hold is part of the motion, not padding to skip.
    let end = runtime_clip
        .frame_bounds()
        .map(|bounds| bounds.end.max(0.0))
        .unwrap_or(0.0);
    if end <= MAX_BAKED_FRAME {
        return end;
    }
    diagnostics.push(Diagnostic::warning(
        "vmd.motion_truncated",
        format!(
            "motion '{clip_name}' declares {end} frames, past the {MAX_BAKED_FRAME}-frame bake limit; it was truncated"
        ),
    ));
    MAX_BAKED_FRAME
}

/// Per-bone sampled poses, in engine space, accumulated across the bake.
///
/// Only bones bound to a [`BoneId`] get a slot, so an unbound bone costs
/// nothing beyond the `None` in `bone_targets`.
struct BakedTracks {
    /// One entry per rig bone, in PMX bone order: the slot it records into,
    /// or `None` for an unbound bone.
    slots: Vec<Option<usize>>,
    /// Per bound bone: the [`BoneId`] it drives and its engine-space rest TRS.
    bound: Vec<(BoneId, Vec3, Quat)>,
    /// Per bound bone, one entry per recorded sample.
    translations: Vec<Vec<Vec3>>,
    /// Per bound bone, one entry per recorded sample. Sign-canonicalized
    /// against the previous sample; see [`Self::record`].
    rotations: Vec<Vec<Quat>>,
}

impl BakedTracks {
    fn new(
        bone_targets: &[Option<BoneId>],
        bone_rest: &[(Vec3, Quat)],
        sample_count: usize,
    ) -> Self {
        let mut slots = Vec::with_capacity(bone_targets.len());
        let mut bound = Vec::new();
        for (index, target) in bone_targets.iter().enumerate() {
            match target {
                Some(bone_id) => {
                    slots.push(Some(bound.len()));
                    let (rest_translation, rest_rotation) = bone_rest[index];
                    bound.push((*bone_id, rest_translation, rest_rotation));
                }
                None => slots.push(None),
            }
        }
        let slot_count = bound.len();
        Self {
            slots,
            bound,
            translations: (0..slot_count)
                .map(|_| Vec::with_capacity(sample_count))
                .collect(),
            rotations: (0..slot_count)
                .map(|_| Vec::with_capacity(sample_count))
                .collect(),
        }
    }

    /// Records the evaluator's current pose as one engine-space sample per
    /// bound bone.
    ///
    /// The evaluator composes FK, IK, appended-parent, fixed-axis and bone
    /// morphs into world matrices only — `PoseArena`'s local rotations carry
    /// the IK result but never the appended-parent one — so the world matrix
    /// is the single place the fully resolved pose exists, and each bone's
    /// local transform is recovered from it against its parent.
    fn record(&mut self, instance: &RuntimeInstance, model: &ModelArena) {
        let world = instance.pose().world_matrices();
        for index in 0..self.slots.len() {
            let Some(slot) = self.slots[index] else {
                continue;
            };
            let bone = BoneIndex(index as u32);
            let matrix = world[index];
            let local = match model.parent_index(bone) {
                Some(parent) => world[parent.as_usize()].inverse() * matrix,
                None => matrix,
            };
            // VMD animates translation and rotation only, and PMX rest poses
            // carry no scale, so the decomposed scale is always 1 and is
            // dropped rather than emitted as a constant Scale channel.
            let (_, rotation, translation) = local.to_scale_rotation_translation();

            self.translations[slot].push(convert_position(translation.to_array()));

            // Consecutive samples must not straddle the quaternion double
            // cover: `to_scale_rotation_translation` may return either sign,
            // and a sign flip mid-track would make the reduction below see a
            // huge change and any linear blend take the long way around.
            let rotation = convert_rotation(rotation.to_array());
            let rotation = match self.rotations[slot].last() {
                Some(previous) if previous.dot(rotation) < 0.0 => -rotation,
                _ => rotation,
            };
            self.rotations[slot].push(rotation);
        }
    }

    /// Reduces every recorded track to the smallest channel list that
    /// reproduces it (see the module documentation).
    fn into_channels(self, sample_time: impl Fn(usize) -> f32) -> Vec<AnimChannel> {
        let mut channels = Vec::new();
        for (slot, &(target, rest_translation, rest_rotation)) in self.bound.iter().enumerate() {
            if let Some(keyframes) = reduce_track(
                &self.translations[slot],
                rest_translation,
                |value| [value.x, value.y, value.z, 1.0],
                |a, b| (a - b).length(),
                TRANSLATION_EPSILON,
                &sample_time,
            ) {
                channels.push(AnimChannel {
                    property: AnimProperty::Translation,
                    target_bone: Some(target),
                    keyframes,
                });
            }
            if let Some(keyframes) = reduce_track(
                &self.rotations[slot],
                rest_rotation,
                |value| value.to_array(),
                |a, b| 1.0 - a.dot(b).abs(),
                ROTATION_EPSILON,
                &sample_time,
            ) {
                channels.push(AnimChannel {
                    property: AnimProperty::Rotation,
                    target_bone: Some(target),
                    keyframes,
                });
            }
        }
        channels
    }
}

/// Reduces one sampled track to keyframes, or `None` when it never leaves
/// `rest`.
///
/// `difference` measures two samples in whatever unit `epsilon` is given in,
/// so the same reduction serves translations (meters) and rotations
/// (`1 - |dot|`).
fn reduce_track<T: Copy>(
    samples: &[T],
    rest: T,
    encode: impl Fn(T) -> [f32; 4],
    difference: impl Fn(T, T) -> f32,
    epsilon: f32,
    sample_time: impl Fn(usize) -> f32,
) -> Option<Vec<Keyframe>> {
    let first = *samples.first()?;
    if samples.iter().all(|sample| difference(*sample, rest) <= epsilon) {
        return None;
    }
    if samples.iter().all(|sample| difference(*sample, first) <= epsilon) {
        // Constant but not at rest: one keyframe holds for the whole clip.
        return Some(vec![Keyframe {
            time: sample_time(0),
            value: encode(first),
        }]);
    }
    Some(
        samples
            .iter()
            .enumerate()
            .map(|(index, sample)| Keyframe {
                time: sample_time(index),
                value: encode(*sample),
            })
            .collect(),
    )
}

/// Renders a name list for a diagnostic message, capping the visible portion
/// so one broken motion cannot produce a multi-megabyte log line.
fn summarize_names(names: &[String]) -> String {
    const VISIBLE: usize = 8;
    let shown = names.iter().take(VISIBLE).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > VISIBLE {
        format!("{shown}, ... ({} more)", names.len() - VISIBLE)
    } else {
        shown
    }
}

/// FNV-1a-64 over `bytes`, the same construction
/// [`crate::pmx_import::fingerprint_pmx_source`] uses.
fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_seeded(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continues an FNV-1a-64 hash over `bytes`, so several inputs can be folded
/// into one fingerprint in a caller-chosen order.
fn fnv1a64_seeded(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Derived cache (ADR 0079 §3)
// ---------------------------------------------------------------------------

/// One cached bake, stored under [`cache_key_for_baked_vmd`].
///
/// Sub-asset IDs are deliberately absent: they are a pure function of the
/// source [`AssetId`] and selector, so recomputing them on a hit is both
/// cheaper and impossible to serve stale.
#[derive(Serialize, Deserialize)]
struct CachedBake {
    schema_version: u32,
    clips: Vec<CachedClip>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Serialize, Deserialize)]
struct CachedClip {
    source_index: usize,
    name: String,
    clip: AnimationClip,
}

/// Computes the cache key for one VMD bake.
///
/// Every input that can change the baked output is hashed here: the
/// algorithm version, the motion bytes, the rig (its PMX content hash, which
/// covers IK chains and appended-parent links that the skeleton identity does
/// not), the bound skeleton's identity, the sample rate, the clip name (it is
/// stored in the entry), and the contact-bone override. Forgetting one would
/// serve a stale bake that never invalidates, so callers must go through this
/// function rather than composing a key by hand.
pub fn cache_key_for_baked_vmd(
    vmd_bytes: &[u8],
    clip_name: &str,
    rig: &VmdBakeRig,
    options: &VmdBakeOptions,
) -> CacheKey {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    mix(&VMD_BAKE_ALGORITHM_VERSION.to_le_bytes());
    mix(&fnv1a64(vmd_bytes).to_le_bytes());
    mix(&rig.fingerprint.to_le_bytes());
    mix(&rig.skeleton.identity.0.to_le_bytes());
    mix(&options.sample_rate.to_bits().to_le_bytes());
    mix(clip_name.as_bytes());
    for name in &options.contact_bone_names {
        mix(&(name.len() as u64).to_le_bytes());
        mix(name.as_bytes());
    }
    CacheKey(hash)
}

/// Resolves a baked VMD through the derived cache, baking on a miss (ADR
/// 0079 §3, ADR 0097 §3): re-registering the same motion against the same
/// model is free after the first bake.
///
/// A cache entry that fails to deserialize (corrupt, or written by a build
/// with a different envelope schema) is treated as a miss and rebaked rather
/// than propagated as an error.
///
/// # Errors
///
/// Returns whatever [`import_vmd_bytes`] would, plus
/// [`VmdImportError::Serialize`] or [`VmdImportError::Cache`] when a fresh
/// bake cannot be written to the cache.
pub fn resolve_or_bake_vmd_bytes(
    cache: &DerivedCache,
    source_id: &AssetId,
    bytes: &[u8],
    clip_name: &str,
    rig: &VmdBakeRig,
    options: &VmdBakeOptions,
) -> Result<VmdImportResult, VmdImportError> {
    let key = cache_key_for_baked_vmd(bytes, clip_name, rig, options);
    if let Some(cached) = cache.get(VMD_CACHE_DOMAIN, &key, VMD_CACHE_EXTENSION)
        && let Ok(entry) = serde_json::from_slice::<CachedBake>(&cached)
        && entry.schema_version == CACHED_BAKE_SCHEMA_VERSION
    {
        return Ok(VmdImportResult {
            clips: entry
                .clips
                .into_iter()
                .map(|cached| VmdBakedClip {
                    id: imported_sub_asset_id(
                        source_id,
                        ImportedSubAssetKind::Animation,
                        cached.source_index,
                    ),
                    source_index: cached.source_index,
                    name: cached.name,
                    clip: cached.clip,
                })
                .collect(),
            diagnostics: entry.diagnostics,
            model_source: None,
        });
    }

    let baked = import_vmd_bytes(source_id, bytes, clip_name, rig, options)?;
    let entry = CachedBake {
        schema_version: CACHED_BAKE_SCHEMA_VERSION,
        clips: baked
            .clips
            .iter()
            .map(|baked| CachedClip {
                source_index: baked.source_index,
                name: baked.name.clone(),
                clip: baked.clip.clone(),
            })
            .collect(),
        diagnostics: baked.diagnostics.clone(),
    };
    let encoded = serde_json::to_vec(&entry).map_err(VmdImportError::Serialize)?;
    cache
        .put(VMD_CACHE_DOMAIN, &key, VMD_CACHE_EXTENSION, &encoded)
        .map_err(VmdImportError::Cache)?;
    Ok(baked)
}

/// Resolves a baked VMD file through the derived cache, naming the clip after
/// the file stem.
///
/// # Errors
///
/// Returns [`VmdImportError::Io`] when the file cannot be read; otherwise see
/// [`resolve_or_bake_vmd_bytes`].
pub fn resolve_or_bake_vmd_path(
    cache: &DerivedCache,
    source_id: &AssetId,
    path: &Path,
    rig: &VmdBakeRig,
    options: &VmdBakeOptions,
) -> Result<VmdImportResult, VmdImportError> {
    let bytes = std::fs::read(path).map_err(VmdImportError::Io)?;
    resolve_or_bake_vmd_bytes(
        cache,
        source_id,
        &bytes,
        &clip_name_from_path(path),
        rig,
        options,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::lerp_channel;
    use crate::pmx_import::{import_pmx_bytes, PMX_TO_METERS};
    use mmd_anim_format::pmx::{
        export_pmx_model, PmxParsedAppendTransform, PmxParsedBone, PmxParsedBoneFlags,
        PmxParsedCounts, PmxParsedGeometry, PmxParsedIk, PmxParsedIkLimit, PmxParsedIkLink,
        PmxParsedIndexSizes, PmxParsedLocalAxis, PmxParsedMaterial, PmxParsedMaterialFlags,
        PmxParsedMetadata, PmxParsedModel, PmxParsedMorph, PmxParsedQdef, PmxParsedSdef,
        PmxParsedSkeleton,
    };
    use mmd_anim_format::vmd::{
        export_vmd_animation, VmdParsedAnimation, VmdParsedBoneFrame, VmdParsedCounts,
        VmdParsedMetadata,
    };
    use std::f32::consts::FRAC_PI_2;

    /// PMX bone indices in [`rigged_pmx_fixture`], in the order it declares
    /// them. Named so a test reads as the rig it exercises.
    const BONE_ROOT: &str = "root";
    const BONE_UPPER: &str = "upper";
    const BONE_LOWER: &str = "lower";
    const BONE_TIP: &str = "tip";
    const BONE_IK: &str = "ik";
    const BONE_ARM: &str = "arm";
    const BONE_APPEND: &str = "append";

    // -----------------------------------------------------------------
    // On-demand VMD/PMX compatibility analysis
    // -----------------------------------------------------------------

    #[test]
    fn compatibility_separates_unique_missing_and_ambiguous_names() {
        let mut pmx = parse_pmx_model(&rigged_pmx_fixture()).expect("fixture PMX must parse");
        let arm = pmx
            .skeleton
            .bones
            .iter_mut()
            .find(|bone| bone.name == BONE_ARM)
            .expect("fixture must contain arm");
        arm.flags.rotatable = false;
        arm.flags.translatable = false;
        pmx.skeleton
            .bones
            .push(plain_bone("ambiguous", 0, [0.0; 3]));
        pmx.skeleton
            .bones
            .push(plain_bone("ambiguous", 0, [0.0; 3]));
        pmx.morphs = vec![
            empty_morph("smile"),
            empty_morph("ambiguous_morph"),
            empty_morph("ambiguous_morph"),
        ];

        let vmd = parsed_vmd_fixture(
            vec![
                bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
                bone_frame(BONE_ARM, 1, [1.0, 0.0, 0.0], z_rotation(0.25)),
                bone_frame("missing", 0, [1.0, 0.0, 0.0], IDENTITY_ROTATION),
                bone_frame("ambiguous", 0, [1.0, 0.0, 0.0], IDENTITY_ROTATION),
            ],
            vec![
                morph_frame("smile", 0, 1.0),
                morph_frame("missing_morph", 0, 1.0),
                morph_frame("ambiguous_morph", 0, 1.0),
            ],
        );
        let report = analyze_vmd_pmx_compatibility(&vmd, &pmx);

        assert_eq!(
            report.bones,
            VmdPmxCompatibilitySummary {
                used_tracks: 3,
                unique_tracks: 1,
                missing_tracks: 1,
                ambiguous_tracks: 1,
            }
        );
        assert_eq!(
            report.morphs,
            VmdPmxCompatibilitySummary {
                used_tracks: 3,
                unique_tracks: 1,
                missing_tracks: 1,
                ambiguous_tracks: 1,
            }
        );
        assert!((report.bones.compatibility_percent().unwrap() - 100.0 / 3.0).abs() < 1.0e-5);
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::RotationUnsupported
                && issue.name == BONE_ARM
                && issue.keyframe_count == 2
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::TranslationUnsupported
                && issue.name == BONE_ARM
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::AmbiguousBone
                && issue.name == "ambiguous"
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::MissingBone && issue.name == "missing"
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::AmbiguousMorph
                && issue.name == "ambiguous_morph"
        }));
        assert!(report.issues.iter().any(|issue| {
            issue.kind == VmdPmxCompatibilityIssueKind::MissingMorph
                && issue.name == "missing_morph"
        }));
    }

    #[test]
    fn neutral_tracks_are_excluded_with_the_fixed_epsilon() {
        let pmx = parse_pmx_model(&rigged_pmx_fixture()).expect("fixture PMX must parse");
        let vmd = parsed_vmd_fixture(
            vec![
                bone_frame(
                    "neutral_positive",
                    0,
                    [VMD_COMPATIBILITY_NEUTRAL_EPSILON, 0.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ),
                bone_frame(
                    "neutral_negative_quaternion",
                    0,
                    [0.0; 3],
                    [0.0, 0.0, 0.0, -1.0],
                ),
            ],
            vec![morph_frame(
                "neutral_morph",
                0,
                -VMD_COMPATIBILITY_NEUTRAL_EPSILON,
            )],
        );

        let report = analyze_vmd_pmx_compatibility(&vmd, &pmx);
        assert_eq!(report.bones.used_tracks, 0);
        assert_eq!(report.morphs.used_tracks, 0);
        assert_eq!(report.bones.compatibility_percent(), None);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn unused_translation_does_not_warn_for_a_non_translatable_bone() {
        let mut pmx = parse_pmx_model(&rigged_pmx_fixture()).expect("fixture PMX must parse");
        let arm = pmx
            .skeleton
            .bones
            .iter_mut()
            .find(|bone| bone.name == BONE_ARM)
            .expect("fixture must contain arm");
        arm.flags.translatable = false;
        let vmd = parsed_vmd_fixture(
            vec![bone_frame(BONE_ARM, 0, [0.0; 3], z_rotation(0.25))],
            Vec::new(),
        );

        let report = analyze_vmd_pmx_compatibility(&vmd, &pmx);
        assert!(report.issues.is_empty());
        assert!(report.bones.is_unique_full_match());
    }

    // -----------------------------------------------------------------
    // IK baking
    // -----------------------------------------------------------------

    #[test]
    fn ik_driven_bones_get_channels_the_motion_never_names() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0, 0.0, 0.0], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 10, [1.0, 0.5, 0.0], IDENTITY_ROTATION),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;

        // The VMD names only the IK bone, yet the chain it drives must come
        // out as ordinary rotation curves: that is the whole point of §3.
        assert!(
            has_channel(clip, &rig, BONE_UPPER, AnimProperty::Rotation),
            "the IK chain's upper bone must be baked into a rotation channel"
        );
        assert!(
            has_channel(clip, &rig, BONE_LOWER, AnimProperty::Rotation),
            "the IK chain's lower bone must be baked into a rotation channel"
        );
    }

    #[test]
    fn baked_ik_chain_reaches_the_goal_in_engine_space() {
        let (rig, skeleton) = fixture_rig();
        let goal_pmx = [1.0, 0.5, 0.0];
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0, 0.0, 0.0], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 10, goal_pmx, IDENTITY_ROTATION),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;
        let end = clip.duration;

        let tip = model_position(&skeleton, clip, end, BONE_TIP);
        let goal = model_position(&skeleton, clip, end, BONE_IK);
        assert!(
            (tip - goal).length() < 5.0e-3,
            "the baked chain must put the IK target on its goal: tip {tip:?}, goal {goal:?}"
        );
        // Guards the axis conversion end to end: the goal read back out of
        // the clip must be the PMX-space goal mapped exactly the way
        // `pmx_import` maps the mesh (negate Z, then scale to meters).
        let expected = Vec3::new(goal_pmx[0], goal_pmx[1], -goal_pmx[2]) * PMX_TO_METERS;
        assert!(
            (goal - expected).length() < 1.0e-5,
            "got {goal:?}, expected {expected:?}"
        );
    }

    #[test]
    fn pmx_local_axis_operation_metadata_does_not_change_ik_bake() {
        // Build the same knee-like IK rig twice. The second PMX carries an
        // opposite local X operation axis, but both PMX files declare the
        // same actual IK link angle limit and therefore must bake the same
        // deformation.
        let (baseline_rig, baseline_skeleton) =
            rig_from(rigged_pmx_fixture_with_knee_limit(false));
        let (local_axis_rig, local_axis_skeleton) =
            rig_from(rigged_pmx_fixture_with_knee_limit(true));

        // Move the IK goal out of the straight rest pose so the lower link
        // must rotate. A neutral motion would not exercise the angle-limit
        // coordinate frame and could pass even with the regression present.
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0, 0.0, 0.0], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 10, [0.0, 0.5, 1.0], IDENTITY_ROTATION),
        ]);

        let baseline_bake = bake(&baseline_rig, &motion);
        let local_axis_bake = bake(&local_axis_rig, &motion);
        let baseline_clip = &baseline_bake.clips[0].clip;
        let local_axis_clip = &local_axis_bake.clips[0].clip;
        let end = baseline_clip.duration;

        // The fixture must actually bend the constrained link; otherwise the
        // comparison would not prove that the IK limit path was exercised.
        let baseline_rotation =
            local_rotation(&baseline_skeleton, baseline_clip, end, BONE_LOWER);
        assert!(
            quat_angle(baseline_rotation) > 0.1,
            "the regression fixture must bend its knee-like lower link"
        );

        // Local-axis operation metadata must not alter the baked FK rotation.
        // This is the direct regression assertion for backwards knees.
        let local_axis_rotation = local_rotation(
            &local_axis_skeleton,
            local_axis_clip,
            end,
            BONE_LOWER,
        );
        let rotation_delta =
            (baseline_rotation.inverse() * local_axis_rotation).normalize();
        assert!(
            quat_angle(rotation_delta) < 1.0e-4,
            "PMX local-axis operation metadata changed the baked IK result"
        );

        // Compare the observable model-space endpoint as well as the channel
        // quaternion so the test covers the final pose consumed by animation.
        let baseline_tip =
            model_position(&baseline_skeleton, baseline_clip, end, BONE_TIP);
        let local_axis_tip =
            model_position(&local_axis_skeleton, local_axis_clip, end, BONE_TIP);
        assert!(
            (baseline_tip - local_axis_tip).length() < 1.0e-5,
            "PMX local-axis metadata changed the baked IK endpoint"
        );

        // Confirm that the bake rig removed the metadata instead of merely
        // compensating for this one test pose with another rotation.
        assert_eq!(local_axis_rig.model.local_axis_count(), 0);
    }

    // -----------------------------------------------------------------
    // Appended-parent baking
    // -----------------------------------------------------------------

    #[test]
    fn appended_parent_bones_get_channels_the_motion_never_names() {
        let (rig, skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_ARM, 10, [0.0; 3], z_rotation(FRAC_PI_2)),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;
        let end = clip.duration;

        assert!(
            has_channel(clip, &rig, BONE_APPEND, AnimProperty::Rotation),
            "the appended-parent bone must be baked into a rotation channel"
        );
        // The fixture appends 50% of `arm`'s rotation, so the baked local
        // rotation must be half the source angle.
        let arm = local_rotation(&skeleton, clip, end, BONE_ARM);
        let append = local_rotation(&skeleton, clip, end, BONE_APPEND);
        let arm_angle = quat_angle(arm);
        let append_angle = quat_angle(append);
        assert!(
            (arm_angle - FRAC_PI_2).abs() < 1.0e-3,
            "the source bone must carry the authored angle, got {arm_angle}"
        );
        assert!(
            (append_angle - FRAC_PI_2 * 0.5).abs() < 1.0e-3,
            "the appended bone must carry half the source angle, got {append_angle}"
        );
    }

    // -----------------------------------------------------------------
    // Channel reduction
    // -----------------------------------------------------------------

    #[test]
    fn bones_that_never_leave_rest_emit_no_channel() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_ARM, 10, [0.0; 3], z_rotation(FRAC_PI_2)),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;

        for property in [AnimProperty::Translation, AnimProperty::Rotation] {
            assert!(
                !has_channel(clip, &rig, BONE_ROOT, property),
                "an untouched bone must not emit a {property:?} channel"
            );
            assert!(
                !has_channel(clip, &rig, BONE_LOWER, property),
                "a bone outside the animated chain must not emit a {property:?} channel"
            );
        }
    }

    #[test]
    fn a_constant_off_rest_track_collapses_to_one_keyframe() {
        let (rig, _skeleton) = fixture_rig();
        // Held at the same off-rest translation for the whole motion.
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0, 0.5, 0.0], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 10, [0.0, 0.5, 0.0], IDENTITY_ROTATION),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;
        let channel = channel(clip, &rig, BONE_IK, AnimProperty::Translation)
            .expect("a held off-rest translation must still emit a channel");
        assert_eq!(
            channel.keyframes.len(),
            1,
            "a constant track must collapse to a single held keyframe"
        );
    }

    // -----------------------------------------------------------------
    // Clip metadata
    // -----------------------------------------------------------------

    #[test]
    fn baked_clip_declares_its_skeleton_and_duration() {
        let (rig, skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 30, [1.0, 0.5, 0.0], IDENTITY_ROTATION),
        ]);

        let baked = bake(&rig, &motion);
        let clip = &baked.clips[0].clip;

        assert_eq!(clip.skeleton.as_ref(), Some(&skeleton.id));
        assert_eq!(clip.skeleton_identity, Some(skeleton.identity));
        assert!(clip.validate().is_none(), "the baked clip must validate");
        // 30 VMD frames at MMD's 30 Hz is exactly one second.
        assert!((clip.duration - 1.0).abs() < 1.0e-6, "got {}", clip.duration);
    }

    #[test]
    fn sub_asset_ids_are_deterministic_across_repeated_bakes() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![bone_frame(
            BONE_IK,
            10,
            [1.0, 0.5, 0.0],
            IDENTITY_ROTATION,
        )]);
        let source = AssetId::generate();

        let first = import_vmd_bytes(&source, &motion, "dance", &rig, &VmdBakeOptions::default())
            .expect("fixture must bake");
        let second = import_vmd_bytes(&source, &motion, "dance", &rig, &VmdBakeOptions::default())
            .expect("fixture must bake");

        assert_eq!(first.clips[0].id, second.clips[0].id);
        assert_eq!(
            first.clips[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Animation, 0)
        );
    }

    /// MMD treats the two spellings of a digit or a Latin letter in a bone
    /// name as the same bone, and motion files mix them: this project's
    /// `man.vmd` keys the upper body as `上半身２` while the model spells it
    /// `上半身2`, which used to drop the whole torso track.
    #[test]
    fn a_bone_name_matches_across_half_and_full_width_spellings() {
        assert_eq!(width_variants("上半身2"), vec!["上半身２".to_owned()]);
        assert_eq!(width_variants("上半身２"), vec!["上半身2".to_owned()]);
        // A name with nothing to convert produces no alias at all.
        assert!(width_variants("上半身").is_empty());

        let mut lookup: HashMap<Vec<u8>, usize> = HashMap::new();
        lookup.insert("上半身2".as_bytes().to_vec(), 7);
        lookup.insert("右足ＩＫ".as_bytes().to_vec(), 9);
        insert_width_normalized_aliases(&mut lookup);
        assert_eq!(lookup.get("上半身２".as_bytes()), Some(&7));
        assert_eq!(lookup.get("右足IK".as_bytes()), Some(&9));
    }

    /// A model that really does spell two different bones each way keeps both
    /// bindings; an alias must never displace a bone that owns its name.
    #[test]
    fn an_alias_never_displaces_a_bone_that_owns_that_spelling() {
        let mut lookup: HashMap<Vec<u8>, usize> = HashMap::new();
        lookup.insert("腕2".as_bytes().to_vec(), 1);
        lookup.insert("腕２".as_bytes().to_vec(), 2);
        insert_width_normalized_aliases(&mut lookup);
        assert_eq!(lookup.get("腕2".as_bytes()), Some(&1));
        assert_eq!(lookup.get("腕２".as_bytes()), Some(&2));
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    #[test]
    fn a_curve_naming_an_unknown_bone_is_dropped_with_a_diagnostic() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![bone_frame(
            "no_such_bone",
            10,
            [1.0, 0.0, 0.0],
            IDENTITY_ROTATION,
        )]);

        let baked = bake(&rig, &motion);
        assert!(
            has_diagnostic(&baked, "vmd.bone_not_found"),
            "an unmatched VMD bone name must be reported, not silently ignored"
        );
        assert!(
            baked.clips[0].clip.channels.is_empty(),
            "no bone moved, so the clip must carry no channels"
        );
    }

    #[test]
    fn every_bake_reports_resampling_as_information() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![bone_frame(
            BONE_IK,
            10,
            [1.0, 0.5, 0.0],
            IDENTITY_ROTATION,
        )]);
        let baked = bake(&rig, &motion);
        let diagnostic = baked
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "vmd.curve_resampled")
            .expect("every bake must report its resampling result");
        assert_eq!(diagnostic.severity, engine_authoring::Severity::Info);
    }

    #[test]
    fn binding_against_a_different_rest_pose_is_reported() {
        let skeleton = fixture_skeleton();
        let mut mismatched = skeleton.clone();
        mismatched.bones[1].rest_translation += Vec3::new(0.0, 0.5, 0.0);

        let rig = VmdBakeRig::from_pmx_bytes(&rigged_pmx_fixture(), &mismatched)
            .expect("fixture rig must load");
        assert!(
            rig.diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "vmd.rest_pose_mismatch"),
            "a skeleton whose rest pose disagrees with the rig must be reported"
        );
        // The rig's own binding diagnostics must reach every clip it bakes.
        let motion = vmd_fixture(vec![bone_frame(
            BONE_IK,
            10,
            [1.0, 0.5, 0.0],
            IDENTITY_ROTATION,
        )]);
        assert!(has_diagnostic(&bake(&rig, &motion), "vmd.rest_pose_mismatch"));
    }

    #[test]
    fn a_pmx_bone_missing_from_the_skeleton_is_reported_and_drives_nothing() {
        let mut skeleton = fixture_skeleton();
        let removed = skeleton
            .bones
            .iter()
            .position(|bone| bone.name == BONE_APPEND)
            .expect("fixture must contain the appended bone");
        let removed_id = skeleton.bones[removed].id;
        skeleton.bones.remove(removed);

        let rig = VmdBakeRig::from_pmx_bytes(&rigged_pmx_fixture(), &skeleton)
            .expect("fixture rig must load");
        assert!(
            rig.diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "vmd.bone_not_in_skeleton")
        );
        assert_eq!(rig.bound_bone_count(), rig.bone_count() - 1);

        let motion = vmd_fixture(vec![
            bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_ARM, 10, [0.0; 3], z_rotation(FRAC_PI_2)),
        ]);
        let baked = bake(&rig, &motion);
        assert!(
            baked.clips[0]
                .clip
                .channels
                .iter()
                .all(|channel| channel.target_bone != Some(removed_id)),
            "an unbound bone must drive no channel"
        );
    }

    #[test]
    fn an_invalid_sample_rate_falls_back_to_the_default() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 30, [1.0, 0.5, 0.0], IDENTITY_ROTATION),
        ]);
        let options = VmdBakeOptions {
            sample_rate: 0.0,
            ..VmdBakeOptions::default()
        };

        let baked = import_vmd_bytes(&AssetId::generate(), &motion, "dance", &rig, &options)
            .expect("fixture must bake");
        assert!(has_diagnostic(&baked, "vmd.sample_rate_invalid"));
        assert!((baked.clips[0].clip.duration - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn a_higher_sample_rate_produces_more_keyframes() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_ARM, 30, [0.0; 3], z_rotation(FRAC_PI_2)),
        ]);

        let coarse = bake(&rig, &motion);
        let fine = import_vmd_bytes(
            &AssetId::generate(),
            &motion,
            "dance",
            &rig,
            &VmdBakeOptions {
                sample_rate: DEFAULT_VMD_SAMPLE_RATE * 2.0,
                ..VmdBakeOptions::default()
            },
        )
        .expect("fixture must bake");

        let coarse_keys = channel(&coarse.clips[0].clip, &rig, BONE_ARM, AnimProperty::Rotation)
            .expect("the animated bone must emit a rotation channel")
            .keyframes
            .len();
        let fine_keys = channel(&fine.clips[0].clip, &rig, BONE_ARM, AnimProperty::Rotation)
            .expect("the animated bone must emit a rotation channel")
            .keyframes
            .len();
        assert_eq!(coarse_keys, 31, "one sample per source frame, plus frame 0");
        assert_eq!(fine_keys, 61, "double rate must double the sample count");
        // Both bakes describe the same motion, so their durations must agree.
        assert!((coarse.clips[0].clip.duration - fine.clips[0].clip.duration).abs() < 1.0e-6);
    }

    // -----------------------------------------------------------------
    // Derived cache
    // -----------------------------------------------------------------

    #[test]
    fn a_cached_bake_is_reused_and_matches_a_fresh_one() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![
            bone_frame(BONE_IK, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_IK, 10, [1.0, 0.5, 0.0], IDENTITY_ROTATION),
        ]);
        let project = tempfile::tempdir().expect("temp project root");
        let cache = DerivedCache::new(project.path());
        let source = AssetId::generate();
        let options = VmdBakeOptions::default();

        let baked = resolve_or_bake_vmd_bytes(&cache, &source, &motion, "dance", &rig, &options)
            .expect("first bake must succeed");
        let reused = resolve_or_bake_vmd_bytes(&cache, &source, &motion, "dance", &rig, &options)
            .expect("second resolve must hit the cache");

        assert_eq!(baked.clips[0].id, reused.clips[0].id);
        assert_eq!(baked.clips[0].name, reused.clips[0].name);
        assert_eq!(
            baked.diagnostics.len(),
            reused.diagnostics.len(),
            "a cache hit must replay the bake's diagnostics"
        );
        let fresh_channels = &baked.clips[0].clip.channels;
        let cached_channels = &reused.clips[0].clip.channels;
        assert_eq!(fresh_channels.len(), cached_channels.len());
        for (fresh, cached) in fresh_channels.iter().zip(cached_channels) {
            assert_eq!(fresh.property, cached.property);
            assert_eq!(fresh.target_bone, cached.target_bone);
            assert_eq!(fresh.keyframes.len(), cached.keyframes.len());
        }
    }

    #[test]
    fn a_different_sample_rate_does_not_reuse_a_cached_bake() {
        let (rig, _skeleton) = fixture_rig();
        let motion = vmd_fixture(vec![bone_frame(
            BONE_ARM,
            30,
            [0.0; 3],
            z_rotation(FRAC_PI_2),
        )]);
        let default_key = cache_key_for_baked_vmd(&motion, "dance", &rig, &VmdBakeOptions::default());
        let doubled_key = cache_key_for_baked_vmd(
            &motion,
            "dance",
            &rig,
            &VmdBakeOptions {
                sample_rate: DEFAULT_VMD_SAMPLE_RATE * 2.0,
                ..VmdBakeOptions::default()
            },
        );
        assert_ne!(
            default_key, doubled_key,
            "the sample rate changes the bake, so it must change the key"
        );
    }

    #[test]
    fn a_different_rig_does_not_reuse_a_cached_bake() {
        let (rig, _skeleton) = fixture_rig();
        // Same bones, same rest pose, same skeleton identity - only the
        // appended-parent ratio differs, which the skeleton cannot express.
        let (other_rig, _other_skeleton) = rig_from(rigged_pmx_fixture_with_append_ratio(1.0));
        let motion = vmd_fixture(vec![bone_frame(
            BONE_ARM,
            30,
            [0.0; 3],
            z_rotation(FRAC_PI_2),
        )]);
        let options = VmdBakeOptions::default();

        assert_eq!(
            rig.skeleton().identity,
            other_rig.skeleton().identity,
            "the two rigs must be indistinguishable by skeleton identity alone"
        );
        assert_ne!(
            cache_key_for_baked_vmd(&motion, "dance", &rig, &options),
            cache_key_for_baked_vmd(&motion, "dance", &other_rig, &options),
            "an appended-parent change must invalidate the cached bake"
        );
    }

    // -----------------------------------------------------------------
    // Asset-pipeline entry points
    // -----------------------------------------------------------------

    #[test]
    fn only_vmd_paths_are_motion_sources() {
        assert!(is_motion_source_path(Path::new("dance.vmd")));
        assert!(is_motion_source_path(Path::new("Dance.VMD")));
        assert!(!is_motion_source_path(Path::new("character.pmx")));
        assert!(!is_motion_source_path(Path::new("hero.gltf")));
    }

    #[test]
    fn importing_a_motion_from_disk_binds_the_models_own_skeleton() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let model_path = directory.path().join("character.pmx");
        let motion_path = directory.path().join("dance.vmd");
        std::fs::write(&model_path, rigged_pmx_fixture()).expect("model fixture write");
        std::fs::write(
            &motion_path,
            vmd_fixture(vec![
                bone_frame(BONE_IK, 0, [0.0; 3], IDENTITY_ROTATION),
                bone_frame(BONE_IK, 10, [1.0, 0.5, 0.0], IDENTITY_ROTATION),
            ]),
        )
        .expect("motion fixture write");

        let model_source = AssetId::generate();
        let motion_source = AssetId::generate();
        let baked = import_motion_path(
            &motion_source,
            &motion_path,
            &model_source,
            &model_path,
            &[],
            &VmdBakeOptions::default(),
        )
        .expect("motion must import");

        // The clip must declare the skeleton the *model's* import produced,
        // not a second identity minted from the motion's own source ID.
        let model_skeleton = skeleton_for(&model_source, &rigged_pmx_fixture());
        assert_eq!(baked.clips[0].clip.skeleton.as_ref(), Some(&model_skeleton.id));
        assert_eq!(
            baked.clips[0].clip.skeleton_identity,
            Some(model_skeleton.identity)
        );
        // Named after the file stem, so the Motion Slot picker shows
        // something recognizable.
        assert_eq!(baked.clips[0].name, "dance");
    }

    #[test]
    fn recorded_model_name_is_available_before_a_pmx_is_selected() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let motion_path = directory.path().join("dance.vmd");
        std::fs::write(&motion_path, vmd_fixture(Vec::new())).expect("motion fixture write");

        assert_eq!(
            vmd_recorded_model_name_path(&motion_path).expect("VMD header must parse"),
            "fixture"
        );
    }

    #[test]
    fn original_bake_can_be_retargeted_to_a_target_specific_clip() {
        let motion_source = AssetId::generate();
        let original_model = AssetId::generate();
        let target_model = AssetId::generate();
        let pmx = rigged_pmx_fixture();
        let original_skeleton = skeleton_for(&original_model, &pmx);
        let target_skeleton = skeleton_for(&target_model, &pmx);
        let rig = VmdBakeRig::from_pmx_bytes(&pmx, &original_skeleton)
            .expect("original PMX rig must build");
        let half_angle = FRAC_PI_2 * 0.5;
        let arm_rotation = [0.0, 0.0, half_angle.sin(), half_angle.cos()];
        let motion = vmd_fixture(vec![
            bone_frame(BONE_ARM, 0, [0.0; 3], IDENTITY_ROTATION),
            bone_frame(BONE_ARM, 10, [0.0; 3], arm_rotation),
        ]);
        let mut baked = import_vmd_bytes(
            &motion_source,
            &motion,
            "dance",
            &rig,
            &VmdBakeOptions::default(),
        )
        .expect("original bake must succeed");
        let mut map = crate::retarget::generate_retarget_map(&original_skeleton, &target_skeleton);
        map.translation.mode = crate::retarget::TranslationMode::None;

        baked
            .retarget_to_model_source(
                &motion_source,
                &target_model,
                &original_skeleton,
                &target_skeleton,
                &map,
                &[],
            )
            .expect("explicit retarget map must produce a target clip");

        assert_eq!(
            baked.clips[0].id,
            imported_motion_sub_asset_id(&motion_source, &target_model, 0)
        );
        assert_eq!(baked.clips[0].clip.skeleton.as_ref(), Some(&target_skeleton.id));
        assert_eq!(
            baked.imported_sub_assets()[0].target_model_source.as_deref(),
            Some(target_model.as_str())
        );
    }

    #[test]
    fn a_motion_catalogs_only_animation_sub_assets() {
        let (rig, _skeleton) = fixture_rig();
        let source = AssetId::generate();
        let motion = vmd_fixture(vec![bone_frame(
            BONE_IK,
            10,
            [1.0, 0.5, 0.0],
            IDENTITY_ROTATION,
        )]);

        let baked = import_vmd_bytes(&source, &motion, "dance", &rig, &VmdBakeOptions::default())
            .expect("fixture must bake");
        let catalog = baked.imported_sub_assets();

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].kind, ImportedSubAssetKind::Animation);
        assert_eq!(catalog[0].index, 0);
        assert_eq!(
            catalog[0].id,
            imported_sub_asset_id(&source, ImportedSubAssetKind::Animation, 0).as_str()
        );
    }

    #[test]
    fn a_motions_fingerprint_covers_the_model_it_is_baked_against() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let model_path = directory.path().join("character.pmx");
        let motion_path = directory.path().join("dance.vmd");
        std::fs::write(&model_path, rigged_pmx_fixture()).expect("model fixture write");
        std::fs::write(
            &motion_path,
            vmd_fixture(vec![bone_frame(
                BONE_ARM,
                10,
                [0.0; 3],
                z_rotation(FRAC_PI_2),
            )]),
        )
        .expect("motion fixture write");

        let before =
            fingerprint_motion_source(&motion_path, &model_path).expect("fingerprint must compute");
        // Editing the model must invalidate the motion: its appended-parent
        // ratio shapes every baked curve, but the `.vmd` bytes never changed.
        std::fs::write(&model_path, rigged_pmx_fixture_with_append_ratio(1.0))
            .expect("model fixture rewrite");
        let after =
            fingerprint_motion_source(&motion_path, &model_path).expect("fingerprint must compute");

        assert_ne!(before, after);
        // And the model is recorded as the dependency that carries it.
        assert_eq!(motion_source_dependencies(&model_path), vec![model_path]);
    }

    #[test]
    fn pairing_a_motion_with_a_non_pmx_model_is_rejected() {
        let directory = tempfile::tempdir().expect("temporary import project");
        let model_path = directory.path().join("hero.gltf");
        std::fs::write(&model_path, b"{}").expect("model fixture write");
        assert!(matches!(
            VmdBakeRig::from_model_path(&AssetId::generate(), &model_path, &[]),
            Err(VmdImportError::ModelNotPmx(_))
        ));
    }

    // -----------------------------------------------------------------
    // Errors
    // -----------------------------------------------------------------

    #[test]
    fn garbage_bytes_fail_to_parse() {
        let (rig, _skeleton) = fixture_rig();
        assert!(matches!(
            import_vmd_bytes(
                &AssetId::generate(),
                b"not a valid vmd document",
                "dance",
                &rig,
                &VmdBakeOptions::default(),
            ),
            Err(VmdImportError::Parse(_))
        ));
    }

    #[test]
    fn a_rig_rejects_bytes_that_are_not_a_pmx_model() {
        assert!(matches!(
            VmdBakeRig::from_pmx_bytes(b"not a pmx document", &fixture_skeleton()),
            Err(VmdImportError::Rig(_))
        ));
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    const IDENTITY_ROTATION: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// A quaternion rotating by `angle` about PMX-space +Z.
    fn z_rotation(angle: f32) -> [f32; 4] {
        let half = angle * 0.5;
        [0.0, 0.0, half.sin(), half.cos()]
    }

    /// The rotation angle a quaternion carries, in radians.
    fn quat_angle(rotation: Quat) -> f32 {
        2.0 * rotation.w.abs().clamp(-1.0, 1.0).acos()
    }

    fn bake(rig: &VmdBakeRig, motion: &[u8]) -> VmdImportResult {
        import_vmd_bytes(
            &AssetId::generate(),
            motion,
            "dance",
            rig,
            &VmdBakeOptions::default(),
        )
        .expect("fixture must bake")
    }

    fn has_diagnostic(result: &VmdImportResult, code: &str) -> bool {
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == code)
    }

    fn bone_id(skeleton: &SkeletonAsset, name: &str) -> BoneId {
        skeleton
            .bones
            .iter()
            .find(|bone| bone.name == name)
            .unwrap_or_else(|| panic!("fixture must contain a bone named {name}"))
            .id
    }

    fn channel<'a>(
        clip: &'a AnimationClip,
        rig: &VmdBakeRig,
        bone_name: &str,
        property: AnimProperty,
    ) -> Option<&'a AnimChannel> {
        let id = bone_id(rig.skeleton(), bone_name);
        clip.channels
            .iter()
            .find(|channel| channel.target_bone == Some(id) && channel.property == property)
    }

    fn has_channel(
        clip: &AnimationClip,
        rig: &VmdBakeRig,
        bone_name: &str,
        property: AnimProperty,
    ) -> bool {
        channel(clip, rig, bone_name, property).is_some()
    }

    /// Samples `clip` at `time` and returns one bone's local rotation,
    /// falling back to its rest rotation when the clip drives no channel.
    fn local_rotation(
        skeleton: &SkeletonAsset,
        clip: &AnimationClip,
        time: f32,
        bone_name: &str,
    ) -> Quat {
        let id = bone_id(skeleton, bone_name);
        let index = skeleton.bone_index(id).expect("bone must be in skeleton");
        clip.channels
            .iter()
            .find(|channel| channel.target_bone == Some(id) && channel.property == AnimProperty::Rotation)
            .and_then(|channel| lerp_channel(channel, time))
            .map(Quat::from_array)
            .unwrap_or(skeleton.bones[index].rest_rotation)
    }

    /// Replays the clip through the skeleton and returns one bone's
    /// model-space position, so a test can assert on the pose an ordinary
    /// animation consumer would see rather than on the channel values.
    fn model_position(
        skeleton: &SkeletonAsset,
        clip: &AnimationClip,
        time: f32,
        bone_name: &str,
    ) -> Vec3 {
        let mut world: Vec<(Vec3, Quat)> = Vec::with_capacity(skeleton.bones.len());
        for bone in &skeleton.bones {
            let mut translation = bone.rest_translation;
            let mut rotation = bone.rest_rotation;
            for channel in &clip.channels {
                if channel.target_bone != Some(bone.id) {
                    continue;
                }
                let Some(value) = lerp_channel(channel, time) else {
                    continue;
                };
                match channel.property {
                    AnimProperty::Translation => {
                        translation = Vec3::new(value[0], value[1], value[2]);
                    }
                    AnimProperty::Rotation => rotation = Quat::from_array(value),
                    AnimProperty::Scale => {}
                }
            }
            let (parent_position, parent_rotation) = bone
                .parent
                .map(|parent| world[parent])
                .unwrap_or((Vec3::ZERO, Quat::IDENTITY));
            world.push((
                parent_position + parent_rotation * translation,
                parent_rotation * rotation,
            ));
        }
        let id = bone_id(skeleton, bone_name);
        let index = skeleton.bone_index(id).expect("bone must be in skeleton");
        world[index].0
    }

    /// Imports [`rigged_pmx_fixture`] the way the editor does and returns the
    /// shared skeleton (ADR 0097 §4a) every bake binds to.
    fn fixture_skeleton() -> SkeletonAsset {
        skeleton_from(&rigged_pmx_fixture())
    }

    fn skeleton_from(pmx: &[u8]) -> SkeletonAsset {
        skeleton_for(&AssetId::generate(), pmx)
    }

    fn skeleton_for(source_id: &AssetId, pmx: &[u8]) -> SkeletonAsset {
        import_pmx_bytes(source_id, pmx, &[])
            .expect("fixture must import")
            .skins
            .first()
            .expect("fixture must produce one skin")
            .skeleton
            .clone()
    }

    fn fixture_rig() -> (VmdBakeRig, SkeletonAsset) {
        rig_from(rigged_pmx_fixture())
    }

    fn rig_from(pmx: Vec<u8>) -> (VmdBakeRig, SkeletonAsset) {
        let skeleton = skeleton_from(&pmx);
        let rig = VmdBakeRig::from_pmx_bytes(&pmx, &skeleton).expect("fixture rig must load");
        (rig, skeleton)
    }

    // -----------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------

    fn bone_frame(
        name: &str,
        frame: u32,
        translation: [f32; 3],
        rotation: [f32; 4],
    ) -> VmdParsedBoneFrame {
        VmdParsedBoneFrame {
            bone_name: name.to_owned(),
            bone_name_bytes: Vec::new(),
            frame,
            translation,
            rotation,
            // All-zero control points decode to `x1 == y1` and `x2 == y2`,
            // which `InterpolationScalar::evaluate` short-circuits to exact
            // linear interpolation - so a test's expected values never depend
            // on MMD's default easing curve.
            interpolation: vec![0u8; 64],
        }
    }

    fn morph_frame(name: &str, frame: u32, weight: f32) -> VmdParsedMorphFrame {
        VmdParsedMorphFrame {
            morph_name: name.to_owned(),
            morph_name_bytes: Vec::new(),
            frame,
            weight,
        }
    }

    fn parsed_vmd_fixture(
        bone_frames: Vec<VmdParsedBoneFrame>,
        morph_frames: Vec<VmdParsedMorphFrame>,
    ) -> VmdParsedAnimation {
        let max_frame = bone_frames
            .iter()
            .map(|frame| frame.frame)
            .chain(morph_frames.iter().map(|frame| frame.frame))
            .max()
            .unwrap_or(0);
        VmdParsedAnimation {
            kind: "vmd",
            metadata: VmdParsedMetadata {
                format: "vmd",
                model_name: "fixture".to_owned(),
                model_name_bytes: Vec::new(),
                counts: VmdParsedCounts {
                    bones: bone_frames.len(),
                    morphs: morph_frames.len(),
                    cameras: 0,
                    lights: 0,
                    self_shadows: 0,
                    properties: 0,
                },
                max_frame,
            },
            bone_frames,
            morph_frames,
            camera_frames: Vec::new(),
            light_frames: Vec::new(),
            self_shadow_frames: Vec::new(),
            property_frames: Vec::new(),
        }
    }

    fn empty_morph(name: &str) -> PmxParsedMorph {
        PmxParsedMorph {
            name: name.to_owned(),
            english_name: name.to_owned(),
            panel: "other".to_owned(),
            kind: "vertex".to_owned(),
            vertex_offsets: Vec::new(),
            group_offsets: Vec::new(),
            bone_offsets: Vec::new(),
            uv_offsets: Vec::new(),
            additional_uv_offsets: Vec::new(),
            material_offsets: Vec::new(),
            flip_offsets: Vec::new(),
            impulse_offsets: Vec::new(),
        }
    }

    fn vmd_fixture(bone_frames: Vec<VmdParsedBoneFrame>) -> Vec<u8> {
        export_vmd_animation(&parsed_vmd_fixture(bone_frames, Vec::new()))
    }

    /// A PMX rig exercising both constructs VMD baking must dissolve.
    ///
    /// A two-link chain (`upper` -> `lower` -> `tip`) driven by an IK bone
    /// that rests exactly on `tip`, so the rig is at its IK solution until a
    /// motion moves the IK bone; and an `append` bone taking half of `arm`'s
    /// rotation. `arm` sits outside the IK chain deliberately, so the two
    /// mechanisms can be tested without one perturbing the other.
    fn rigged_pmx_fixture() -> Vec<u8> {
        rigged_pmx_fixture_with_append_ratio(0.5)
    }

    /// Produces a rig with the same negative-X rotation limit commonly used
    /// by MMD knee IK links.
    ///
    /// When `has_local_axis` is true, the constrained lower bone also carries
    /// an operation-local X axis pointing toward -X. This descriptor must not
    /// reverse the coordinate frame of the stored IK angle limit.
    fn rigged_pmx_fixture_with_knee_limit(has_local_axis: bool) -> Vec<u8> {
        let mut model = parse_pmx_model(&rigged_pmx_fixture())
            .expect("base knee regression PMX must parse");

        {
            // Attach the optional local-axis descriptor to the constrained
            // lower link. The Z axis remains +Z, producing a valid
            // right-handed basis with the local X direction reversed.
            let lower = model
                .skeleton
                .bones
                .iter_mut()
                .find(|bone| bone.name == BONE_LOWER)
                .expect("fixture must contain the lower IK link");
            lower.flags.local_axis = has_local_axis;
            lower.local_axis = has_local_axis.then_some(PmxParsedLocalAxis {
                x: [-1.0, 0.0, 0.0],
                z: [0.0, 0.0, 1.0],
            });
        }

        {
            // Limit the lower link to negative X, matching the forward-only
            // knee constraint used by ordinary MMD leg IK chains.
            let ik = model
                .skeleton
                .bones
                .iter_mut()
                .find(|bone| bone.name == BONE_IK)
                .and_then(|bone| bone.ik.as_mut())
                .expect("fixture must contain its IK solver");
            let lower_link = ik
                .links
                .iter_mut()
                .find(|link| link.bone_index == 2)
                .expect("fixture IK must reference the lower link");
            lower_link.limits = Some(PmxParsedIkLimit {
                lower: [-FRAC_PI_2, 0.0, 0.0],
                upper: [0.0, 0.0, 0.0],
            });
        }

        export_pmx_model(&model)
    }

    fn rigged_pmx_fixture_with_append_ratio(append_ratio: f32) -> Vec<u8> {
        let bones = vec![
            plain_bone(BONE_ROOT, -1, [0.0, 0.0, 0.0]),
            plain_bone(BONE_UPPER, 0, [0.0, 2.0, 0.0]),
            plain_bone(BONE_LOWER, 1, [0.0, 1.0, 0.0]),
            plain_bone(BONE_TIP, 2, [0.0, 0.0, 0.0]),
            PmxParsedBone {
                ik: Some(PmxParsedIk {
                    target_index: 3,
                    loop_count: 40,
                    limit_angle: 1.0,
                    links: vec![
                        PmxParsedIkLink {
                            bone_index: 2,
                            limits: None,
                        },
                        PmxParsedIkLink {
                            bone_index: 1,
                            limits: None,
                        },
                    ],
                }),
                flags: PmxParsedBoneFlags {
                    ik: true,
                    ..plain_bone_flags()
                },
                ..plain_bone(BONE_IK, 0, [0.0, 0.0, 0.0])
            },
            plain_bone(BONE_ARM, 0, [1.0, 2.0, 0.0]),
            PmxParsedBone {
                append_transform: Some(PmxParsedAppendTransform {
                    parent_index: 5,
                    weight: append_ratio,
                }),
                flags: PmxParsedBoneFlags {
                    append_rotate: true,
                    ..plain_bone_flags()
                },
                ..plain_bone(BONE_APPEND, 5, [2.0, 2.0, 0.0])
            },
        ];

        // One triangle, fully weighted to `upper`, so the import produces a
        // skin and therefore the shared skeleton every bake binds to.
        let geometry = PmxParsedGeometry {
            positions: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            normals: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            uvs: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            additional_uvs: Vec::new(),
            indices: vec![0, 1, 2],
            skin_indices: [1u32, 0, 0, 0].repeat(3),
            skin_weights: [1.0f32, 0.0, 0.0, 0.0].repeat(3),
            edge_scale: Vec::new(),
            material_groups: Vec::new(),
            sdef: PmxParsedSdef::default(),
            qdef: PmxParsedQdef::default(),
        };

        let model = PmxParsedModel {
            metadata: PmxParsedMetadata {
                format: "pmx".to_owned(),
                version: 2.0,
                encoding: "utf-8".to_owned(),
                name: "rigged fixture".to_owned(),
                english_name: "rigged fixture".to_owned(),
                comment: String::new(),
                english_comment: String::new(),
                counts: PmxParsedCounts {
                    vertices: 3,
                    faces: 1,
                    materials: 1,
                    bones: bones.len(),
                    morphs: 0,
                    display_frames: 0,
                    rigid_bodies: 0,
                    joints: 0,
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
            geometry,
            materials: vec![PmxParsedMaterial {
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
                flags: PmxParsedMaterialFlags {
                    double_sided: false,
                    ground_shadow: false,
                    self_shadow_map: false,
                    self_shadow: false,
                    edge: false,
                    vertex_color: false,
                    point_draw: false,
                    line_draw: false,
                },
                face_count: 1,
            }],
            skeleton: PmxParsedSkeleton { bones },
            morphs: Vec::new(),
            display_frames: Vec::new(),
            rigid_bodies: Vec::new(),
            joints: Vec::new(),
            soft_bodies: Vec::new(),
            diagnostics: Vec::new(),
        };

        export_pmx_model(&model)
    }

    fn plain_bone(name: &str, parent_index: i32, position: [f32; 3]) -> PmxParsedBone {
        PmxParsedBone {
            name: name.to_owned(),
            english_name: name.to_owned(),
            parent_index,
            layer: 0,
            position,
            tail_index: -1,
            tail_position: None,
            flags: plain_bone_flags(),
            append_transform: None,
            fixed_axis: None,
            local_axis: None,
            external_parent_key: None,
            ik: None,
        }
    }

    fn plain_bone_flags() -> PmxParsedBoneFlags {
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

    #[test]
    fn content_classification_uses_actual_track_domains() {
        use mmd_anim_format::vmd::VmdSharedContextTrackSummary as Track;

        let summary = |model_keys, scene_keys| VmdSharedContextSummary {
            target_model_name_bytes: [0; 20],
            max_frame: 0,
            bones: Track {
                track_count: usize::from(model_keys != 0),
                key_count: model_keys,
            },
            morphs: Track {
                track_count: 0,
                key_count: 0,
            },
            cameras: Track {
                track_count: usize::from(scene_keys != 0),
                key_count: scene_keys,
            },
            lights: Track {
                track_count: 0,
                key_count: 0,
            },
            self_shadows: Track {
                track_count: 0,
                key_count: 0,
            },
            properties: Track {
                track_count: 0,
                key_count: 0,
            },
            property_ik_entry_count: 0,
        };

        assert_eq!(classify_vmd_summary(&summary(1, 0)), VmdContentKind::Model);
        assert_eq!(classify_vmd_summary(&summary(0, 1)), VmdContentKind::Scene);
        assert_eq!(classify_vmd_summary(&summary(1, 1)), VmdContentKind::Mixed);
        assert_eq!(classify_vmd_summary(&summary(0, 0)), VmdContentKind::Empty);
    }

    #[test]
    fn morph_frames_become_sorted_scalar_channels_and_unavailable_names_warn() {
        let frames = vec![
            VmdParsedMorphFrame {
                morph_name: "blink".to_owned(),
                morph_name_bytes: Vec::new(),
                frame: 30,
                weight: 1.0,
            },
            VmdParsedMorphFrame {
                morph_name: "blink".to_owned(),
                morph_name_bytes: Vec::new(),
                frame: 0,
                weight: 0.0,
            },
            VmdParsedMorphFrame {
                morph_name: "bone-only".to_owned(),
                morph_name_bytes: Vec::new(),
                frame: 0,
                weight: 1.0,
            },
        ];
        let names = HashSet::from(["blink".to_owned()]);
        let mut diagnostics = Vec::new();
        let channels = build_morph_channels(&frames, Some(&names), &mut diagnostics);

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].target_name, "blink");
        assert_eq!(channels[0].keyframes[0].time, 0.0);
        assert_eq!(channels[0].keyframes[1].time, 1.0);
        let unavailable = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "vmd.morph_runtime_channel_unavailable")
            .collect::<Vec<_>>();
        assert_eq!(
            unavailable.len(),
            1,
            "all unavailable morph names must be aggregated into one diagnostic"
        );
        assert!(unavailable[0].message.contains("bone-only"));
    }
}
