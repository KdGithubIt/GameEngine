//! Retarget maps, the pure FK retarget function, and the baked-clip cache
//! wiring built on [`crate::derived_cache`] (ADR 0079).
//!
//! [`RetargetMap`] is a persisted, reviewable authoring asset (`*.retarget.json`)
//! describing how one skeleton's animation channels map onto another.
//! [`retarget_clip`] is the pure function that actually performs the FK
//! rebind; [`resolve_or_bake_retargeted_clip`] adds the content-addressed
//! cache lookup on top so repeated resolutions are free after the first bake.

use crate::animation::{AnimChannel, AnimProperty, AnimationClip, Keyframe};
use crate::asset::AssetManifest;
use crate::derived_cache::{CacheKey, DerivedCache};
use crate::skeleton_asset::{
    fnv1a_seed, hash_bytes, hash_u64, BoneId, SkeletonAsset, SkeletonIdentity,
};
use engine_authoring::diagnostic::{Diagnostic, DiagnosticTarget};
use engine_authoring::id::AssetId;
use glam::{Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// RetargetMap asset (ADR 0079 §1)
// ---------------------------------------------------------------------------

/// Current schema version for `*.retarget.json` files.
pub const RETARGET_MAP_SCHEMA_VERSION: u32 = 1;

/// File suffix recognized for a persisted [`RetargetMap`].
pub const RETARGET_MAP_FILE_SUFFIX: &str = ".retarget.json";

/// Diagnostic code reported by [`RetargetMap::validate`] when a map's
/// recorded [`SkeletonIdentity`] no longer matches the skeleton asset it
/// refers to (ADR 0079 §1).
pub const RETARGET_MAP_STALE_DIAGNOSTIC: &str = "anim.retarget_map_stale";

/// Diagnostic code reported when an animator's clip targets a skeleton
/// different from its entity's skeleton and no [`RetargetMap`] resolves the
/// pair (ADR 0079 §4). The clip is never applied cross-skeleton silently.
pub const RETARGET_MAP_MISSING_DIAGNOSTIC: &str = "anim.retarget_map_missing";

/// Diagnostic code reported when cross-skeleton resolution needs a clip's
/// source `source_fingerprint` (for the derived-cache key, ADR 0079 §3) but
/// the manifest entry has none recorded (AP-6). Falling back to the clip
/// sub-asset ID alone is a same-session-only uniqueness guarantee that would
/// poison the on-disk cache across sessions, so this blocks resolution
/// instead of baking under a weak key. Reimporting the source records a
/// fresh fingerprint and clears the diagnostic.
pub const RETARGET_SOURCE_UNFINGERPRINTED_DIAGNOSTIC: &str = "anim.retarget_source_unfingerprinted";

/// Diagnostic code reported when the packaged player
/// ([`PackagedBakedClips`] present in the [`engine_ecs::World`]) needs a
/// cross-skeleton clip but no baked clip file exists at the expected path
/// under [`PackagedBakedClips::root`] (ADR 0079 §4). The player never bakes
/// at runtime, so this means the package's `baked_anim/` set is incomplete:
/// re-package the project, or set `always_package` on the retarget map if
/// the pair is only ever reached dynamically at runtime.
pub const RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC: &str =
    "anim.retarget_bake_missing_from_package";

/// One direct 1:1 bone mapping (fingers, weapon bones, single bones).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BonePair {
    /// Bone in the source skeleton.
    pub source: BoneId,
    /// Bone in the target skeleton driven by [`Self::source`].
    pub target: BoneId,
}

/// An ordered, root-to-tip chain mapping used when a chain's bone count
/// differs between the source and target rig (ADR 0079 §2 / §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainPair {
    /// Diagnostic label ("spine", "left_leg", ...). Not semantic; purely for
    /// human review of the persisted map.
    pub name: String,
    /// Root-to-tip, contiguous chain in the source skeleton.
    pub source_bones: Vec<BoneId>,
    /// Root-to-tip, contiguous chain in the target skeleton.
    pub target_bones: Vec<BoneId>,
}

/// Which bones carry translation keyframes through a retarget (ADR 0079 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationMode {
    /// Only the bone mapped from the clip's [`crate::animation::AnimationClip::root_bone`]
    /// carries translation. The default, and the usual choice for locomotion
    /// clips whose root bone carries the character's displacement.
    #[default]
    RootOnly,
    /// No bone carries translation; every target bone keeps its rest
    /// translation (a pure rotation retarget).
    None,
    /// Every mapped bone carries translation, scaled by [`TranslationScale`].
    All,
}

/// Scale factor applied to translation values carried through a retarget
/// (ADR 0079 §1 / §2).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationScale {
    /// `target rest hip height / source rest hip height`, where "hip" is the
    /// clip's `root_bone` mapping. The default: keeps locomotion distances
    /// proportional to each rig's own scale.
    #[default]
    HipHeightRatio,
    /// A fixed, author-chosen scale factor.
    Manual(f32),
}

/// Translation-carrying policy for a [`RetargetMap`] (ADR 0079 §1).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TranslationPolicy {
    /// Which bones carry translation keyframes.
    #[serde(default)]
    pub mode: TranslationMode,
    /// Scale applied to carried translation values.
    #[serde(default)]
    pub scale: TranslationScale,
}

/// A persisted, reviewable mapping from one skeleton's bones to another's
/// (ADR 0079 §1), stored as `*.retarget.json` with `schema_version: 1`.
///
/// [`Self::source_identity`] and [`Self::target_identity`] snapshot the
/// [`SkeletonIdentity`] of each skeleton at the time the map was authored, so
/// a later reimport that changes bone structure is detectable instead of
/// silently applying a stale mapping (see [`Self::validate`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetargetMap {
    /// Schema version for forward-compatibility detection.
    pub schema_version: u32,
    /// The skeleton clips are authored against.
    pub source_skeleton: AssetId,
    /// [`SkeletonIdentity`] of [`Self::source_skeleton`] at authoring time,
    /// as the raw `u64` carried by [`SkeletonIdentity`].
    pub source_identity: u64,
    /// The skeleton clips are retargeted onto.
    pub target_skeleton: AssetId,
    /// [`SkeletonIdentity`] of [`Self::target_skeleton`] at authoring time.
    pub target_identity: u64,
    /// Direct 1:1 mappings (fingers, weapon bones, single bones).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bone_pairs: Vec<BonePair>,
    /// Ordered chain mappings; the redistribution unit when bone counts differ.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain_pairs: Vec<ChainPair>,
    /// Translation-carrying policy applied by [`retarget_clip`].
    #[serde(default)]
    pub translation: TranslationPolicy,
    /// Forces packaging to bake and stage this map's clips even when the
    /// AP-7 reachability trace finds no scene or prefab content that needs
    /// its `(source_skeleton, target_skeleton)` pair.
    ///
    /// The escape hatch for clips assigned dynamically at runtime (crowd
    /// variation, script-driven clip swaps): such usage is invisible to the
    /// static scene/prefab walk in `crates/editor/src/build.rs`, so a map
    /// backing it would otherwise be skipped with a non-blocking
    /// `RetargetMapNotReached` build diagnostic and never staged into the
    /// package. Additive, `schema_version` stays `1`; absent on disk means
    /// `false`, matching every file written before this field existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub always_package: bool,
}

/// `serde`'s `skip_serializing_if` predicate for [`RetargetMap::always_package`]:
/// the field is omitted from JSON entirely when it is at its default `false`,
/// so files written before this field existed round-trip byte-for-byte.
fn is_false(value: &bool) -> bool {
    !value
}

/// Reports why a [`RetargetMap`] could not be parsed.
#[derive(Debug)]
pub enum RetargetMapError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The file uses a schema version newer than this build supports.
    UnsupportedVersion {
        /// The version number found in the file.
        found: u32,
    },
}

impl std::fmt::Display for RetargetMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "retarget map JSON error: {error}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "retarget map schema_version {found} is not supported (max: {RETARGET_MAP_SCHEMA_VERSION})"
            ),
        }
    }
}

impl std::error::Error for RetargetMapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedVersion { .. } => None,
        }
    }
}

impl RetargetMap {
    /// Parses a `*.retarget.json` string.
    ///
    /// # Errors
    ///
    /// Returns [`RetargetMapError::Json`] for malformed JSON, or
    /// [`RetargetMapError::UnsupportedVersion`] when `schema_version` exceeds
    /// [`RETARGET_MAP_SCHEMA_VERSION`].
    pub fn from_json(json: &str) -> Result<Self, RetargetMapError> {
        let map: RetargetMap = serde_json::from_str(json).map_err(RetargetMapError::Json)?;
        if map.schema_version > RETARGET_MAP_SCHEMA_VERSION {
            return Err(RetargetMapError::UnsupportedVersion {
                found: map.schema_version,
            });
        }
        Ok(map)
    }

    /// Serializes this map to canonical pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Reports [`RETARGET_MAP_STALE_DIAGNOSTIC`] when the current
    /// [`SkeletonIdentity`] of either skeleton no longer matches the identity
    /// recorded when this map was authored (ADR 0079 §1).
    ///
    /// Callers look this map up by matching [`Self::source_skeleton`] /
    /// [`Self::target_skeleton`] against the skeletons in hand, so this
    /// method takes the current identities directly rather than whole
    /// [`SkeletonAsset`] values.
    pub fn validate(
        &self,
        current_source_identity: SkeletonIdentity,
        current_target_identity: SkeletonIdentity,
    ) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        if current_source_identity.0 != self.source_identity {
            diagnostics.push(
                Diagnostic::error(
                    RETARGET_MAP_STALE_DIAGNOSTIC,
                    format!(
                        "retarget map's source skeleton `{}` was reimported and its structure changed; \
                         review bone_pairs/chain_pairs before using this map",
                        self.source_skeleton.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: self.source_skeleton.clone(),
                }),
            );
        }
        if current_target_identity.0 != self.target_identity {
            diagnostics.push(
                Diagnostic::error(
                    RETARGET_MAP_STALE_DIAGNOSTIC,
                    format!(
                        "retarget map's target skeleton `{}` was reimported and its structure changed; \
                         review bone_pairs/chain_pairs before using this map",
                        self.target_skeleton.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: self.target_skeleton.clone(),
                }),
            );
        }
        diagnostics
    }
}

/// Loads every `*.retarget.json` asset registered in `manifest`, skipping (not
/// failing on) entries whose file is missing or fails to parse.
///
/// Best-effort by design: a malformed or since-deleted map should not stop
/// resolution for every other clip in the project; the malformed map simply
/// never matches a lookup, which surfaces later as
/// [`RETARGET_MAP_MISSING_DIAGNOSTIC`] for anything that needed it.
pub fn load_registered_retarget_maps(
    assets_root: &Path,
    manifest: &AssetManifest,
) -> Vec<(AssetId, RetargetMap)> {
    let mut maps = Vec::new();
    for (id, entry) in manifest.iter() {
        if !entry
            .path
            .to_ascii_lowercase()
            .ends_with(RETARGET_MAP_FILE_SUFFIX)
        {
            continue;
        }
        let Ok(json) = std::fs::read_to_string(assets_root.join(&entry.path)) else {
            continue;
        };
        let Ok(map) = RetargetMap::from_json(&json) else {
            continue;
        };
        maps.push((id.clone(), map));
    }
    maps
}

/// Finds the registered map whose `(source_skeleton, target_skeleton)` pair
/// matches exactly (ADR 0079 §4).
pub fn find_retarget_map_for_pair<'a>(
    maps: &'a [(AssetId, RetargetMap)],
    source_skeleton: &AssetId,
    target_skeleton: &AssetId,
) -> Option<&'a RetargetMap> {
    maps.iter()
        .find(|(_, map)| {
            &map.source_skeleton == source_skeleton && &map.target_skeleton == target_skeleton
        })
        .map(|(_, map)| map)
}

// ---------------------------------------------------------------------------
// generate_retarget_map: name-heuristic pre-fill (ADR 0079, editor assist)
// ---------------------------------------------------------------------------

/// Namespace prefixes stripped before the fallback name match (ADR 0079).
///
/// Left/right symmetry (`"L_"`, `"_r"`, ...) is deliberately left untouched:
/// stripping it would make e.g. a left-hand and a right-hand bone match,
/// which is always wrong.
const STRIPPED_NAME_PREFIXES: &[&str] = &["mixamorig:", "mixamorig_"];

/// Normalizes a bone name for the fallback heuristic pass: lowercased, with
/// one recognized namespace prefix stripped if present.
fn normalized_bone_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for prefix in STRIPPED_NAME_PREFIXES {
        if let Some(stripped) = lower.strip_prefix(prefix) {
            return stripped.to_owned();
        }
    }
    lower
}

/// Pre-fills a [`RetargetMap`]'s `bone_pairs` from bone names alone (ADR 0079,
/// editor map-creation assist).
///
/// Runs two passes over `target`'s bones, each source bone usable at most
/// once:
/// 1. Case-insensitive exact name match.
/// 2. A namespace-prefix-stripped match for anything left unresolved by pass 1
///    (e.g. `"mixamorig:LeftArm"` vs `"LeftArm"`).
///
/// Bones left unresolved by both passes are simply absent from `bone_pairs`;
/// this heuristic runs once at authoring time and the returned map stores
/// only its explicit results — nothing here is re-evaluated at resolve time.
/// `chain_pairs` and `translation` are left at their defaults for the author
/// to fill in.
pub fn generate_retarget_map(source: &SkeletonAsset, target: &SkeletonAsset) -> RetargetMap {
    let mut bone_pairs = Vec::new();
    let mut used_source: HashSet<BoneId> = HashSet::new();

    for target_bone in &target.bones {
        let target_lower = target_bone.name.to_ascii_lowercase();
        if let Some(source_bone) = source.bones.iter().find(|source_bone| {
            !used_source.contains(&source_bone.id)
                && source_bone.name.to_ascii_lowercase() == target_lower
        }) {
            bone_pairs.push(BonePair {
                source: source_bone.id,
                target: target_bone.id,
            });
            used_source.insert(source_bone.id);
        }
    }

    let mapped_targets: HashSet<BoneId> = bone_pairs.iter().map(|pair| pair.target).collect();
    for target_bone in &target.bones {
        if mapped_targets.contains(&target_bone.id) {
            continue;
        }
        let target_normalized = normalized_bone_name(&target_bone.name);
        if let Some(source_bone) = source.bones.iter().find(|source_bone| {
            !used_source.contains(&source_bone.id)
                && normalized_bone_name(&source_bone.name) == target_normalized
        }) {
            bone_pairs.push(BonePair {
                source: source_bone.id,
                target: target_bone.id,
            });
            used_source.insert(source_bone.id);
        }
    }

    RetargetMap {
        schema_version: RETARGET_MAP_SCHEMA_VERSION,
        source_skeleton: source.id.clone(),
        source_identity: source.identity.0,
        target_skeleton: target.id.clone(),
        target_identity: target.identity.0,
        bone_pairs,
        chain_pairs: Vec::new(),
        translation: TranslationPolicy::default(),
        always_package: false,
    }
}

// ---------------------------------------------------------------------------
// retarget_clip: the pure FK retarget function (ADR 0079 §2)
// ---------------------------------------------------------------------------

/// Bumped whenever [`retarget_clip`]'s math changes, so a cached bake from an
/// older algorithm version never survives as a stale hit (ADR 0079 §3).
///
/// `2`: baked contact-interval re-detection now honors the target source's
/// `contact_bones` override (previously hardcoded to the default heuristic),
/// changing baked output for any map whose target has a non-empty override.
pub const RETARGET_ALGORITHM_VERSION: u32 = 2;

/// Output keyframes per mapped bone are capped at this count before
/// [`retarget_clip`] falls back to uniform resampling (ADR 0079 §2).
const MAX_OUTPUT_KEYFRAMES: usize = 4096;

/// Uniform resampling rate used once [`MAX_OUTPUT_KEYFRAMES`] is exceeded.
const UNIFORM_RESAMPLE_HZ: f32 = 60.0;

/// Reports why [`retarget_clip`] could not produce an output clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetargetError {
    /// `map` resolves no bone at all (`bone_pairs` and `chain_pairs` both
    /// contributed nothing usable), so there is nothing to retarget.
    NoMappedBones,
    /// [`TranslationScale::HipHeightRatio`] requires both skeletons' rest hip
    /// height, which requires `clip.root_bone` to be set and mapped to a
    /// target bone; either was missing.
    MissingRootBoneForHipHeightRatio,
}

impl std::fmt::Display for RetargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMappedBones => write!(f, "retarget map resolves no bone in either skeleton"),
            Self::MissingRootBoneForHipHeightRatio => write!(
                f,
                "HipHeightRatio translation scale requires a root bone mapped in both skeletons"
            ),
        }
    }
}

impl std::error::Error for RetargetError {}

/// Retargets `clip` (authored against `source`) onto `target` using `map`.
///
/// Pure: the output depends only on the arguments (no I/O, no globals, no
/// time). See the ADR 0079 §2 rustdoc-level summary below for the exact math;
/// this implementation follows it step for step.
///
/// For every mapped target bone, per output keyframe time:
/// 1. Source local TRS is sampled and forward-kinematic'd to model space.
/// 2. `q_dst_model = q_src_model * inverse(q_src_rest_model) * q_dst_rest_model`
///    — the source's model-space delta from its own rest pose, applied onto
///    the target's rest orientation. This is independent of rest-pose
///    convention (T-pose vs A-pose) and of per-bone axis conventions.
/// 3. The result is converted back to target-local space against the target
///    parent's *already-retargeted* model-space rotation (bones are visited
///    parent before child).
/// 4. Translation is carried only for bones selected by `map.translation`;
///    every other target bone keeps its own rest translation (target bone
///    lengths / proportions are never squashed).
///
/// Chain pairs with equal bone counts map bone-by-bone in order. Unequal
/// counts map each target chain bone to the source bone at the nearest
/// normalized chain position (`i / (len - 1)`); this is a v1 limitation
/// (curvature is transferred stepwise, not redistributed smoothly).
///
/// Output key times are the union of the source clip's key times across
/// every mapped source bone, capped at 4096 per channel; beyond that, the
/// output is uniformly resampled at 60 Hz instead.
///
/// `target_contact_bone_names` is the **target** skeleton's source
/// (`ImportSettings::contact_bones`, ADR 0080 §1): ground-contact intervals
/// on the baked output are re-detected against `target`'s own proportions
/// (never copied from `clip`), so the override that matters here is the
/// target's, not the source clip's. An empty slice keeps the default
/// foot/ankle/toe name heuristic. This is part of the pure function's input
/// — omitting it from a cache key would let a stale bake outlive a changed
/// override, which is exactly the failure class ADR 0079 §3 calls out as the
/// worst this design can have.
///
/// # Errors
///
/// Returns [`RetargetError::NoMappedBones`] when `map` resolves no bone at
/// all, and [`RetargetError::MissingRootBoneForHipHeightRatio`] when
/// `map.translation` needs a root bone mapping that is not available.
pub fn retarget_clip(
    clip: &AnimationClip,
    source: &SkeletonAsset,
    target: &SkeletonAsset,
    map: &RetargetMap,
    target_contact_bone_names: &[String],
) -> Result<AnimationClip, RetargetError> {
    let target_to_source = build_bone_mapping(map);
    if target_to_source.is_empty() {
        return Err(RetargetError::NoMappedBones);
    }

    let source_rest_model = rest_model_pose(source);
    let target_rest_model = rest_model_pose(target);

    let target_root_bone = clip.root_bone.and_then(|source_root_id| {
        target_to_source
            .iter()
            .find_map(|(target_id, source_id)| (*source_id == source_root_id).then_some(*target_id))
    });

    let translation_scale = compute_translation_scale(
        &map.translation,
        clip.root_bone,
        target_root_bone,
        source,
        target,
        &source_rest_model,
        &target_rest_model,
    )?;

    let mapped_source_bones: HashSet<BoneId> = target_to_source.values().copied().collect();
    let source_key_times: Vec<f32> = clip
        .channels
        .iter()
        .filter(|channel| {
            channel
                .target_bone
                .is_some_and(|bone_id| mapped_source_bones.contains(&bone_id))
        })
        .flat_map(|channel| channel.keyframes.iter().map(|keyframe| keyframe.time))
        .collect();
    let output_times = output_key_times(&source_key_times, clip.duration);

    let mut rotation_samples: HashMap<BoneId, Vec<Keyframe>> = HashMap::new();
    let mut translation_samples: HashMap<BoneId, Vec<Keyframe>> = HashMap::new();

    for &t in &output_times {
        let source_local = sampled_local_pose(source, clip, t);
        let source_model = model_pose_from_local(source, &source_local);

        let mut target_model_rotation = vec![Quat::IDENTITY; target.bones.len()];
        for target_index in 0..target.bones.len() {
            let target_bone = &target.bones[target_index];
            let parent_model_rotation = target_bone
                .parent
                .and_then(|parent_index| target_model_rotation.get(parent_index).copied())
                .unwrap_or(Quat::IDENTITY);
            let mapped_source_index = target_to_source
                .get(&target_bone.id)
                .and_then(|source_bone_id| source.bone_index(*source_bone_id));

            let (local_rotation, model_rotation) = match mapped_source_index {
                Some(source_index) => {
                    let q_src_model = source_model[source_index].1;
                    let q_src_rest_model = source_rest_model[source_index].1;
                    let q_dst_rest_model = target_rest_model[target_index].1;
                    let model_rotation =
                        (q_src_model * q_src_rest_model.inverse() * q_dst_rest_model).normalize();
                    let local_rotation =
                        (parent_model_rotation.inverse() * model_rotation).normalize();
                    (local_rotation, model_rotation)
                }
                None => {
                    // Unmapped bones are left untouched: their local rotation
                    // stays at rest, so they simply follow their (possibly
                    // retargeted) parent rigidly.
                    let local_rotation = target_bone.rest_rotation;
                    let model_rotation = (parent_model_rotation * local_rotation).normalize();
                    (local_rotation, model_rotation)
                }
            };
            target_model_rotation[target_index] = model_rotation;

            let Some(source_index) = mapped_source_index else {
                continue;
            };
            rotation_samples
                .entry(target_bone.id)
                .or_default()
                .push(Keyframe {
                    time: t,
                    value: local_rotation.to_array(),
                });

            if carries_translation(map.translation.mode, target_bone.id, target_root_bone) {
                let local_translation = source_local[source_index].0 * translation_scale;
                translation_samples
                    .entry(target_bone.id)
                    .or_default()
                    .push(Keyframe {
                        time: t,
                        value: [
                            local_translation.x,
                            local_translation.y,
                            local_translation.z,
                            1.0,
                        ],
                    });
            }
        }
    }

    let mut channels: Vec<AnimChannel> =
        Vec::with_capacity(rotation_samples.len() + translation_samples.len());
    for (bone_id, keyframes) in rotation_samples {
        channels.push(AnimChannel {
            property: AnimProperty::Rotation,
            target_bone: Some(bone_id),
            keyframes,
        });
    }
    for (bone_id, keyframes) in translation_samples {
        channels.push(AnimChannel {
            property: AnimProperty::Translation,
            target_bone: Some(bone_id),
            keyframes,
        });
    }
    // Deterministic output regardless of hash map iteration order.
    channels.sort_by_key(|channel| {
        (
            channel
                .target_bone
                .map(|bone_id| bone_id.0)
                .unwrap_or(u32::MAX),
            channel.property as u8,
        )
    });

    let mut retargeted = AnimationClip {
        duration: clip.duration,
        channels,
        // Morph targets are model-semantic rather than skeleton-semantic.
        // Preserve their names so the target PMX can resolve compatible
        // expressions after the bone channels have been retargeted.
        morph_channels: clip.morph_channels.clone(),
        events: clip.events.clone(),
        skeleton: Some(target.id.clone()),
        skeleton_identity: Some(target.identity),
        root_bone: target_root_bone,
        contacts: Vec::new(),
    };
    // Contact intervals are re-detected against the *target* skeleton's own
    // proportions, never copied from the source clip (ADR 0080 §1): a
    // longer-legged target plants at a different point in the cycle than
    // the source did, so only the target's own FK sampling can say where.
    // The target source's own `contact_bones` override applies here (not the
    // source clip's), and is threaded through by every caller of this
    // function down to `cache_key_for_retargeted_clip` so a changed override
    // is never served a stale bake.
    retargeted.contacts = crate::contact_detect::detect_contact_intervals(
        &retargeted,
        target,
        target_contact_bone_names,
    );
    Ok(retargeted)
}

/// Whether `target_bone_id` carries translation keyframes under `mode`.
///
/// Only called for bones that are already known to be mapped, so
/// [`TranslationMode::All`] applying to every call site is correct.
fn carries_translation(
    mode: TranslationMode,
    target_bone_id: BoneId,
    target_root_bone: Option<BoneId>,
) -> bool {
    match mode {
        TranslationMode::None => false,
        TranslationMode::RootOnly => target_root_bone == Some(target_bone_id),
        TranslationMode::All => true,
    }
}

/// Resolves [`TranslationPolicy::scale`] to a concrete scalar.
fn compute_translation_scale(
    policy: &TranslationPolicy,
    source_root_bone: Option<BoneId>,
    target_root_bone: Option<BoneId>,
    source: &SkeletonAsset,
    target: &SkeletonAsset,
    source_rest_model: &[(Vec3, Quat)],
    target_rest_model: &[(Vec3, Quat)],
) -> Result<f32, RetargetError> {
    if policy.mode == TranslationMode::None {
        // No bone carries translation, so the scale is never applied.
        return Ok(1.0);
    }
    match policy.scale {
        TranslationScale::Manual(factor) => Ok(factor),
        TranslationScale::HipHeightRatio => {
            let source_index = source_root_bone
                .and_then(|bone_id| source.bone_index(bone_id))
                .ok_or(RetargetError::MissingRootBoneForHipHeightRatio)?;
            let target_index = target_root_bone
                .and_then(|bone_id| target.bone_index(bone_id))
                .ok_or(RetargetError::MissingRootBoneForHipHeightRatio)?;
            let source_height = source_rest_model[source_index].0.y;
            let target_height = target_rest_model[target_index].0.y;
            if source_height.abs() <= f32::EPSILON {
                // A hip at (model-space) ground height cannot form a ratio;
                // fall back to an unscaled carry rather than dividing by
                // (near) zero.
                Ok(1.0)
            } else {
                Ok(target_height / source_height)
            }
        }
    }
}

/// Builds the `target BoneId -> source BoneId` mapping from `map`'s
/// `bone_pairs` and expanded `chain_pairs` (ADR 0079 §2 / §5).
fn build_bone_mapping(map: &RetargetMap) -> HashMap<BoneId, BoneId> {
    let mut mapping = HashMap::new();
    for pair in &map.bone_pairs {
        mapping.insert(pair.target, pair.source);
    }
    for chain in &map.chain_pairs {
        let target_len = chain.target_bones.len();
        let source_len = chain.source_bones.len();
        if target_len == 0 || source_len == 0 {
            continue;
        }
        if target_len == source_len {
            for (target_bone, source_bone) in chain.target_bones.iter().zip(&chain.source_bones) {
                mapping.insert(*target_bone, *source_bone);
            }
        } else {
            for (target_index, &target_bone) in chain.target_bones.iter().enumerate() {
                let source_index = nearest_chain_source_index(target_index, target_len, source_len);
                mapping.insert(target_bone, chain.source_bones[source_index]);
            }
        }
    }
    mapping
}

/// Finds the source chain index nearest `target_index`'s normalized position
/// (`i / (len - 1)`) along the chain (ADR 0079 §2, unequal chain counts).
///
/// Ties resolve to the lowest source index, which keeps the mapping
/// deterministic.
fn nearest_chain_source_index(target_index: usize, target_len: usize, source_len: usize) -> usize {
    if source_len <= 1 || target_len <= 1 {
        return 0;
    }
    let target_position = target_index as f32 / (target_len - 1) as f32;
    let mut best_index = 0usize;
    let mut best_distance = f32::MAX;
    for source_index in 0..source_len {
        let source_position = source_index as f32 / (source_len - 1) as f32;
        let distance = (target_position - source_position).abs();
        if distance < best_distance {
            best_distance = distance;
            best_index = source_index;
        }
    }
    best_index
}

/// Computes each bone's rest-pose model-space (translation, rotation) via
/// forward kinematics over `skeleton.bones`' parent-before-child order.
fn rest_model_pose(skeleton: &SkeletonAsset) -> Vec<(Vec3, Quat)> {
    let local: Vec<(Vec3, Quat)> = skeleton
        .bones
        .iter()
        .map(|bone| (bone.rest_translation, bone.rest_rotation))
        .collect();
    model_pose_from_local(skeleton, &local)
}

/// Samples every bone's local (translation, rotation) from `clip` at time
/// `t`, falling back to `skeleton`'s rest pose for bones without a sampled
/// channel. Scale channels are not retargeted (a documented v1 limitation:
/// skinned rigs rarely animate bone scale, and `retarget_clip`'s output never
/// emits a scale channel).
///
/// `pub(crate)` so [`crate::contact_detect::detect_contact_intervals`] reuses
/// the same forward-kinematics sampling instead of a second implementation.
pub(crate) fn sampled_local_pose(
    skeleton: &SkeletonAsset,
    clip: &AnimationClip,
    t: f32,
) -> Vec<(Vec3, Quat)> {
    let mut local: Vec<(Vec3, Quat)> = skeleton
        .bones
        .iter()
        .map(|bone| (bone.rest_translation, bone.rest_rotation))
        .collect();
    for channel in &clip.channels {
        let Some(bone_id) = channel.target_bone else {
            continue;
        };
        let Some(index) = skeleton.bone_index(bone_id) else {
            continue;
        };
        let Some(value) = crate::animation::lerp_channel(channel, t) else {
            continue;
        };
        match channel.property {
            AnimProperty::Translation => local[index].0 = Vec3::new(value[0], value[1], value[2]),
            AnimProperty::Rotation => local[index].1 = Quat::from_array(value),
            AnimProperty::Scale => {}
        }
    }
    local
}

/// Forward-kinematics `local` (one entry per `skeleton.bones` index) into
/// model space, using `skeleton.bones`' parent-before-child order.
///
/// `pub(crate)`, shared with [`crate::contact_detect::detect_contact_intervals`]
/// for the same reason as [`sampled_local_pose`].
pub(crate) fn model_pose_from_local(
    skeleton: &SkeletonAsset,
    local: &[(Vec3, Quat)],
) -> Vec<(Vec3, Quat)> {
    let mut model: Vec<(Vec3, Quat)> = Vec::with_capacity(skeleton.bones.len());
    for (index, bone) in skeleton.bones.iter().enumerate() {
        let (local_translation, local_rotation) = local[index];
        let (parent_translation, parent_rotation) = match bone.parent {
            Some(parent_index) => model
                .get(parent_index)
                .copied()
                .unwrap_or((Vec3::ZERO, Quat::IDENTITY)),
            None => (Vec3::ZERO, Quat::IDENTITY),
        };
        let translation = parent_translation + parent_rotation * local_translation;
        let rotation = (parent_rotation * local_rotation).normalize();
        model.push((translation, rotation));
    }
    model
}

/// Computes [`retarget_clip`]'s output key times: the sorted, deduplicated
/// union of `source_key_times`, or a uniform 60 Hz resampling of
/// `[0, duration]` once that union exceeds [`MAX_OUTPUT_KEYFRAMES`] (ADR 0079
/// §2). Falls back to a single sample at `t = 0` when `source_key_times` is
/// empty, so a mapped-but-unanimated clip still produces a valid (constant)
/// output instead of an empty channel.
fn output_key_times(source_key_times: &[f32], duration: f32) -> Vec<f32> {
    let mut times = source_key_times.to_vec();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup_by(|a, b| (*a - *b).abs() < 1.0e-6);

    if times.len() > MAX_OUTPUT_KEYFRAMES {
        let sample_count = ((duration.max(0.0) * UNIFORM_RESAMPLE_HZ).ceil() as usize).max(1) + 1;
        return (0..sample_count)
            .map(|index| (index as f32 / UNIFORM_RESAMPLE_HZ).min(duration.max(0.0)))
            .collect();
    }
    if times.is_empty() {
        times.push(0.0);
    }
    times
}

// ---------------------------------------------------------------------------
// Baked clip envelope + derived cache wiring (ADR 0079 §3)
// ---------------------------------------------------------------------------

/// Cache domain used for every baked retargeted clip.
pub const RETARGET_CACHE_DOMAIN: &str = "anim";

/// File extension for a baked clip cache entry (yields `<key>.clip.json`).
pub const BAKED_CLIP_FILE_EXTENSION: &str = "clip.json";

/// Schema version for the baked-clip cache envelope.
pub const BAKED_CLIP_SCHEMA_VERSION: u32 = 1;

/// Marks the current process as a packaged player that consumes pre-baked
/// retargeted clips instead of the on-disk derived cache (ADR 0079 §4).
///
/// Invariant: present if and only if the process is running from a package.
/// Its presence in the [`engine_ecs::World`] is the sole signal
/// `crate::scene_bridge`'s cross-skeleton clip resolution uses to pick its
/// mode:
///
/// - **Present** (inserted unconditionally by the shipped `player` binary): a
///   needed baked clip is looked up under [`Self::root`] by the same
///   cache-key file name packaging staged it under
///   (`<key.file_stem()>.<BAKED_CLIP_FILE_EXTENSION>`). A miss — the file is
///   absent or fails to deserialize — is reported as
///   [`RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC`] and the entity's
///   `Animator` is removed; the player never bakes at runtime and never
///   writes to the derived cache, so an end-user install never depends on a
///   writable install directory, and a miss means AP-7's reachability trace
///   or a map's `always_package` flag is wrong, which must surface loudly
///   rather than self-heal.
/// - **Absent** (editor Play, tests, tools): the existing bake-or-cache path
///   through [`resolve_or_bake_retargeted_clip`] and
///   [`crate::derived_cache::DerivedCache`] runs unchanged.
#[derive(Debug, Clone)]
pub struct PackagedBakedClips {
    /// Directory packaging staged baked clips into. Expected to be
    /// `<package_root>/baked_anim`, whether or not that directory exists: a
    /// package with no cross-skeleton retargets never consults it.
    pub root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct BakedClipEnvelope {
    schema_version: u32,
    clip: AnimationClip,
}

/// Reports why a baked clip cache entry could not be produced or read back.
#[derive(Debug)]
pub enum BakedClipError {
    /// The entry's JSON was malformed.
    Json(serde_json::Error),
    /// The entry uses a schema version newer than this build supports.
    UnsupportedVersion {
        /// The version number found in the entry.
        found: u32,
    },
}

impl std::fmt::Display for BakedClipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "baked clip JSON error: {error}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "baked clip schema_version {found} is not supported (max: {BAKED_CLIP_SCHEMA_VERSION})"
            ),
        }
    }
}

impl std::error::Error for BakedClipError {}

/// Serializes `clip` as a schema-versioned baked-clip cache entry.
///
/// # Errors
///
/// Returns a [`serde_json::Error`] if serialization fails.
pub fn serialize_baked_clip(clip: &AnimationClip) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&BakedClipEnvelope {
        schema_version: BAKED_CLIP_SCHEMA_VERSION,
        clip: clip.clone(),
    })
}

/// Deserializes a baked-clip cache entry produced by [`serialize_baked_clip`].
///
/// # Errors
///
/// Returns [`BakedClipError::Json`] for malformed bytes, or
/// [`BakedClipError::UnsupportedVersion`] for an entry newer than this build
/// supports (which forces a rebake instead of misreading unknown data).
pub fn deserialize_baked_clip(bytes: &[u8]) -> Result<AnimationClip, BakedClipError> {
    let envelope: BakedClipEnvelope =
        serde_json::from_slice(bytes).map_err(BakedClipError::Json)?;
    if envelope.schema_version > BAKED_CLIP_SCHEMA_VERSION {
        return Err(BakedClipError::UnsupportedVersion {
            found: envelope.schema_version,
        });
    }
    Ok(envelope.clip)
}

/// Folds a list of strings into an in-progress FNV-1a 64 hash: the count as
/// a `u64`, then each entry's UTF-8 bytes length-prefixed (via
/// [`hash_bytes`]), in the caller's order.
///
/// Order is part of the hash (this never sorts): `contact_bones` on
/// [`crate::asset::ImportSettings`] is consumed as an ordered list by
/// [`crate::contact_detect::detect_contact_intervals`],
/// so two lists with the same names in a different order are — correctly —
/// different cache keys. An empty slice hashes as count `0` with no further
/// bytes, so "no override" always hashes identically regardless of which
/// empty `Vec`/slice value produced it.
fn hash_string_list(hash: &mut u64, values: &[String]) {
    hash_u64(hash, values.len() as u64);
    for value in values {
        hash_bytes(hash, value.as_bytes());
    }
}

/// Composes the cache key for one retargeted clip (ADR 0079 §3).
///
/// This is the single audited site combining every input that can change a
/// retarget's output: [`RETARGET_ALGORITHM_VERSION`], the clip source's
/// content fingerprint, the clip's sub-asset ID, both skeletons' current
/// [`SkeletonIdentity`], the map's canonical JSON bytes, and the *target*
/// source's `contact_bones` override (ADR 0080 §1) — baked contacts are
/// re-detected against the target, so that override, not the source clip's,
/// is what can change the baked output (see [`retarget_clip`]'s
/// `target_contact_bone_names` parameter). Forgetting an input here is the
/// worst bug class this cache design has (a stale result that never
/// invalidates), so every caller MUST go through this function rather than
/// composing a key by hand.
///
/// # Errors
///
/// Returns a [`serde_json::Error`] if `map` cannot be serialized.
pub fn cache_key_for_retargeted_clip(
    source_fingerprint: &str,
    clip_sub_asset_id: &str,
    source_identity: SkeletonIdentity,
    target_identity: SkeletonIdentity,
    map: &RetargetMap,
    target_contact_bone_names: &[String],
) -> Result<CacheKey, serde_json::Error> {
    let canonical_map_json = serde_json::to_vec(map)?;
    let mut hash = fnv1a_seed();
    hash_u64(&mut hash, u64::from(RETARGET_ALGORITHM_VERSION));
    hash_bytes(&mut hash, source_fingerprint.as_bytes());
    hash_bytes(&mut hash, clip_sub_asset_id.as_bytes());
    hash_u64(&mut hash, source_identity.0);
    hash_u64(&mut hash, target_identity.0);
    hash_bytes(&mut hash, &canonical_map_json);
    hash_string_list(&mut hash, target_contact_bone_names);
    Ok(CacheKey(hash))
}

/// Reports why [`resolve_or_bake_retargeted_clip`] could not produce a clip.
#[derive(Debug)]
pub enum RetargetResolveError {
    /// The cache key or a cache entry could not be serialized.
    Serialize(serde_json::Error),
    /// Writing the bake result to the cache failed.
    Cache(std::io::Error),
    /// The pure retarget function itself failed.
    Retarget(RetargetError),
}

impl std::fmt::Display for RetargetResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => write!(f, "failed to serialize retarget cache data: {error}"),
            Self::Cache(error) => write!(f, "failed to write the retarget cache entry: {error}"),
            Self::Retarget(error) => write!(f, "retarget failed: {error}"),
        }
    }
}

impl std::error::Error for RetargetResolveError {}

/// Resolves a retargeted clip through the derived cache, baking on a miss
/// (ADR 0079 §3 / §4): cache hit loads the baked clip; a miss calls
/// [`retarget_clip`] synchronously, stores the result, and returns it.
///
/// A cache entry that fails to deserialize (corrupt, or written by a future
/// build) is treated as a miss and rebaked, rather than propagated as an
/// error.
///
/// `target_contact_bone_names` is the target source's `contact_bones`
/// override (see [`retarget_clip`] and [`cache_key_for_retargeted_clip`]);
/// threaded through to both the cache key and the bake itself so a changed
/// override is never served a bake produced under the old one.
///
/// # Errors
///
/// Returns [`RetargetResolveError::Retarget`] when [`retarget_clip`] fails,
/// or [`RetargetResolveError::Serialize`] / [`RetargetResolveError::Cache`]
/// when the cache key or entry cannot be produced.
// Every parameter is an independently audited cache-key input (ADR 0079
// §3); grouping any of them into a bundle type would hide which ones a
// future caller forgot to pass through.
#[allow(clippy::too_many_arguments)]
pub fn resolve_or_bake_retargeted_clip(
    cache: &DerivedCache,
    clip: &AnimationClip,
    source_fingerprint: &str,
    clip_sub_asset_id: &str,
    source: &SkeletonAsset,
    target: &SkeletonAsset,
    map: &RetargetMap,
    target_contact_bone_names: &[String],
) -> Result<AnimationClip, RetargetResolveError> {
    let key = cache_key_for_retargeted_clip(
        source_fingerprint,
        clip_sub_asset_id,
        source.identity,
        target.identity,
        map,
        target_contact_bone_names,
    )
    .map_err(RetargetResolveError::Serialize)?;

    if let Some(bytes) = cache.get(RETARGET_CACHE_DOMAIN, &key, BAKED_CLIP_FILE_EXTENSION)
        && let Ok(clip) = deserialize_baked_clip(&bytes) {
            return Ok(clip);
        }

    let baked = retarget_clip(clip, source, target, map, target_contact_bone_names)
        .map_err(RetargetResolveError::Retarget)?;
    let bytes = serialize_baked_clip(&baked).map_err(RetargetResolveError::Serialize)?;
    cache
        .put(
            RETARGET_CACHE_DOMAIN,
            &key,
            BAKED_CLIP_FILE_EXTENSION,
            &bytes,
        )
        .map_err(RetargetResolveError::Cache)?;
    Ok(baked)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::AnimEvent;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};
    use std::f32::consts::FRAC_PI_2;

    fn bone(
        id: u32,
        name: &str,
        parent: Option<usize>,
        translation: Vec3,
        rotation: Quat,
    ) -> BoneDef {
        BoneDef {
            id: BoneId(id),
            name: name.to_owned(),
            parent,
            rest_translation: translation,
            rest_rotation: rotation,
            rest_scale: Vec3::ONE,
        }
    }

    /// Two-bone rig: `root` (index 0) -> `tip` (index 1).
    fn two_bone_skeleton(id: AssetId, tip_rest_rotation: Quat) -> SkeletonAsset {
        let bones = vec![
            bone(0, "root", None, Vec3::ZERO, Quat::IDENTITY),
            bone(
                1,
                "tip",
                Some(0),
                Vec3::new(0.0, 1.0, 0.0),
                tip_rest_rotation,
            ),
        ];
        let identity = compute_skeleton_identity(&bones);
        SkeletonAsset {
            id,
            name: "rig".to_owned(),
            bones,
            identity,
            next_bone_id: 2,
        }
    }

    fn identity_bone_pair_map(source: &SkeletonAsset, target: &SkeletonAsset) -> RetargetMap {
        RetargetMap {
            schema_version: RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: source.id.clone(),
            source_identity: source.identity.0,
            target_skeleton: target.id.clone(),
            target_identity: target.identity.0,
            bone_pairs: vec![
                BonePair {
                    source: BoneId(0),
                    target: BoneId(0),
                },
                BonePair {
                    source: BoneId(1),
                    target: BoneId(1),
                },
            ],
            chain_pairs: Vec::new(),
            translation: TranslationPolicy::default(),
            always_package: false,
        }
    }

    fn rotation_channel(bone_id: BoneId, keyframes: Vec<(f32, Quat)>) -> AnimChannel {
        AnimChannel {
            property: AnimProperty::Rotation,
            target_bone: Some(bone_id),
            keyframes: keyframes
                .into_iter()
                .map(|(time, rotation)| Keyframe {
                    time,
                    value: rotation.to_array(),
                })
                .collect(),
        }
    }

    fn translation_channel(bone_id: BoneId, keyframes: Vec<(f32, Vec3)>) -> AnimChannel {
        AnimChannel {
            property: AnimProperty::Translation,
            target_bone: Some(bone_id),
            keyframes: keyframes
                .into_iter()
                .map(|(time, translation)| Keyframe {
                    time,
                    value: [translation.x, translation.y, translation.z, 1.0],
                })
                .collect(),
        }
    }

    fn assert_quat_close(actual: Quat, expected: Quat, tolerance: f32, message: &str) {
        // Quaternions q and -q represent the same rotation.
        let dot = actual.dot(expected).abs();
        assert!(
            dot >= 1.0 - tolerance,
            "{message}: expected {expected:?}, got {actual:?} (dot={dot})"
        );
    }

    // --- RetargetMap schema / round-trip ------------------------------------

    #[test]
    fn retarget_map_v1_round_trips_through_json() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = identity_bone_pair_map(&source, &target);

        let json = map.to_json().expect("must serialize");
        let parsed = RetargetMap::from_json(&json).expect("must parse");

        assert_eq!(parsed, map);
    }

    #[test]
    fn a_map_file_without_always_package_parses_as_false_and_omits_the_key_on_resave() {
        // A file written before AP-7 (or by hand) never mentions
        // `always_package` at all; it must still parse, default to `false`,
        // and re-serializing it must not introduce the key, so every
        // existing `*.retarget.json` on disk stays byte-stable.
        let json = r#"{
            "schema_version": 1,
            "source_skeleton": "asset_01JP0000000000000000000001",
            "source_identity": 0,
            "target_skeleton": "asset_01JP0000000000000000000002",
            "target_identity": 0
        }"#;
        let parsed = RetargetMap::from_json(json).expect("must parse without always_package");
        assert!(!parsed.always_package);

        let resaved = parsed.to_json().expect("must serialize");
        assert!(
            !resaved.contains("always_package"),
            "a false always_package must be omitted from resaved JSON: {resaved}"
        );
    }

    #[test]
    fn always_package_true_round_trips_through_json() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let mut map = identity_bone_pair_map(&source, &target);
        map.always_package = true;

        let json = map.to_json().expect("must serialize");
        assert!(json.contains("always_package"));
        let parsed = RetargetMap::from_json(&json).expect("must parse");

        assert_eq!(parsed, map);
    }

    #[test]
    fn from_json_rejects_a_schema_version_newer_than_supported() {
        let json = r#"{
            "schema_version": 99,
            "source_skeleton": "asset_01JP0000000000000000000001",
            "source_identity": 0,
            "target_skeleton": "asset_01JP0000000000000000000002",
            "target_identity": 0
        }"#;
        let error = RetargetMap::from_json(json).expect_err("future version must be rejected");
        assert!(matches!(
            error,
            RetargetMapError::UnsupportedVersion { found: 99 }
        ));
    }

    #[test]
    fn default_translation_policy_is_root_only_hip_height_ratio() {
        let policy = TranslationPolicy::default();
        assert_eq!(policy.mode, TranslationMode::RootOnly);
        assert_eq!(policy.scale, TranslationScale::HipHeightRatio);
    }

    // --- RetargetMap::validate (staleness) ----------------------------------

    #[test]
    fn validate_reports_stale_source_identity() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = identity_bone_pair_map(&source, &target);

        let changed_identity = SkeletonIdentity(map.source_identity.wrapping_add(1));
        let diagnostics = map.validate(changed_identity, SkeletonIdentity(map.target_identity));

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, RETARGET_MAP_STALE_DIAGNOSTIC);
        assert!(diagnostics[0].is_blocking());
    }

    #[test]
    fn validate_reports_nothing_when_identities_still_match() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = identity_bone_pair_map(&source, &target);

        let diagnostics = map.validate(
            SkeletonIdentity(map.source_identity),
            SkeletonIdentity(map.target_identity),
        );

        assert!(diagnostics.is_empty());
    }

    // --- generate_retarget_map -----------------------------------------------

    #[test]
    fn generate_retarget_map_matches_exact_names_case_insensitively() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let mut target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        target.bones[0].name = "ROOT".to_owned();
        target.bones[1].name = "Tip".to_owned();

        let map = generate_retarget_map(&source, &target);

        assert_eq!(map.bone_pairs.len(), 2);
        assert!(map.bone_pairs.contains(&BonePair {
            source: BoneId(0),
            target: BoneId(0)
        }));
        assert!(map.bone_pairs.contains(&BonePair {
            source: BoneId(1),
            target: BoneId(1)
        }));
    }

    #[test]
    fn generate_retarget_map_matches_after_stripping_a_namespace_prefix() {
        let mut source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        source.bones[1].name = "mixamorig:LeftArm".to_owned();
        let mut target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        target.bones[1].name = "LeftArm".to_owned();

        let map = generate_retarget_map(&source, &target);

        assert!(map.bone_pairs.contains(&BonePair {
            source: BoneId(1),
            target: BoneId(1)
        }));
    }

    #[test]
    fn generate_retarget_map_leaves_unresolved_bones_out() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let mut target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        target.bones[1].name = "completely_unrelated_name".to_owned();

        let map = generate_retarget_map(&source, &target);

        assert_eq!(map.bone_pairs.len(), 1, "only the root bone must match");
        assert_eq!(map.bone_pairs[0].target, BoneId(0));
    }

    // --- retarget_clip: identity retarget ------------------------------------

    #[test]
    fn identity_retarget_on_the_same_skeleton_is_lossless() {
        let skeleton = two_bone_skeleton(AssetId::generate(), Quat::from_rotation_x(0.3));
        let map = identity_bone_pair_map(&skeleton, &skeleton);
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![
                rotation_channel(
                    BoneId(1),
                    vec![(0.0, Quat::IDENTITY), (1.0, Quat::from_rotation_y(1.2))],
                ),
                translation_channel(
                    BoneId(0),
                    vec![(0.0, Vec3::ZERO), (1.0, Vec3::new(2.0, 0.0, 0.0))],
                ),
            ],
            morph_channels: Vec::new(),
            events: vec![AnimEvent {
                time: 0.5,
                name: "step".to_owned(),
            }],
            skeleton: Some(skeleton.id.clone()),
            skeleton_identity: Some(skeleton.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let retargeted = retarget_clip(&clip, &skeleton, &skeleton, &map, &[])
            .expect("identity retarget must succeed");

        assert_eq!(retargeted.skeleton, Some(skeleton.id));
        assert_eq!(retargeted.root_bone, Some(BoneId(0)));
        assert_eq!(retargeted.events, clip.events);

        for &t in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let original = crate::animation::lerp_channel(&clip.channels[0], t).unwrap();
            let output_channel = retargeted
                .channels
                .iter()
                .find(|channel| {
                    channel.target_bone == Some(BoneId(1))
                        && channel.property == AnimProperty::Rotation
                })
                .expect("tip rotation channel must exist");
            let output = crate::animation::lerp_channel(output_channel, t).unwrap();
            assert_quat_close(
                Quat::from_array(output),
                Quat::from_array(original),
                1.0e-5,
                "identity retarget must reproduce the source rotation",
            );
        }

        let root_translation_channel = retargeted
            .channels
            .iter()
            .find(|channel| {
                channel.target_bone == Some(BoneId(0))
                    && channel.property == AnimProperty::Translation
            })
            .expect("root translation must be carried under RootOnly");
        let value = crate::animation::lerp_channel(root_translation_channel, 1.0).unwrap();
        assert!(
            (value[0] - 2.0).abs() < 1.0e-5,
            "hip height ratio is 1.0 for an identity retarget"
        );
    }

    // --- retarget_clip: contacts are re-detected, never copied (ADR 0080) ---

    #[test]
    fn retarget_clip_redetects_contacts_against_the_target_skeleton_instead_of_copying() {
        // A two-bone "root" -> "foot" rig where the foot is planted (zero
        // model-space motion) for the whole clip, on both source and
        // target -- but the two skeletons have different foot rest heights,
        // so detection must run again on the target's own proportions.
        let foot_bone = |id: u32, rest_translation: Vec3| BoneDef {
            id: BoneId(id),
            name: if id == 1 {
                "foot".to_owned()
            } else {
                "root".to_owned()
            },
            parent: if id == 1 { Some(0) } else { None },
            rest_translation,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        };
        let source_bones = vec![
            foot_bone(0, Vec3::ZERO),
            foot_bone(1, Vec3::new(0.0, -1.0, 0.0)),
        ];
        let source_identity = compute_skeleton_identity(&source_bones);
        let source = SkeletonAsset {
            id: AssetId::generate(),
            name: "source".to_owned(),
            bones: source_bones,
            identity: source_identity,
            next_bone_id: 2,
        };
        let target_bones = vec![
            foot_bone(0, Vec3::ZERO),
            foot_bone(1, Vec3::new(0.0, -1.5, 0.0)),
        ];
        let target_identity = compute_skeleton_identity(&target_bones);
        let target = SkeletonAsset {
            id: AssetId::generate(),
            name: "target".to_owned(),
            bones: target_bones,
            identity: target_identity,
            next_bone_id: 2,
        };
        let map = identity_bone_pair_map(&source, &target);

        // A bogus, deliberately-wrong source-side contact interval: proves
        // the output isn't simply carried over from `clip.contacts`.
        let bogus_source_contact = crate::contact_detect::ContactInterval {
            bone: BoneId(1),
            start: 0.6,
            end: 0.6001,
        };
        let clip = AnimationClip {
            duration: 1.0,
            // No channels at all: the foot bone stays at its rest
            // translation for the whole clip on both rigs, which is
            // trivially "planted" (zero speed, zero height above its own
            // minimum) for the entire duration.
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: vec![bogus_source_contact],
        };

        let retargeted =
            retarget_clip(&clip, &source, &target, &map, &[]).expect("retarget must succeed");

        assert_ne!(
            retargeted.contacts,
            vec![bogus_source_contact],
            "the bogus source-side interval must not be carried through unchanged"
        );
        assert!(
            retargeted
                .contacts
                .iter()
                .any(|interval| interval.bone == BoneId(1)
                    && interval.start <= 0.0
                    && interval.end >= 1.0 - 1.0e-3),
            "a foot planted for the whole clip must detect a full-clip interval on the target, got {:?}",
            retargeted.contacts
        );
    }

    #[test]
    fn retarget_clip_honors_the_target_contact_bones_override() {
        // Same planted two-bone rig as above, but the bone is named "tip"
        // (matches none of the default foot/ankle/toe heuristic patterns) on
        // *both* rigs. Without an override, retargeting must detect no
        // contacts at all; with the target's own `contact_bones` override
        // naming it, the baked output must carry an interval for it.
        let tip_bone = |id: u32, rest_translation: Vec3| BoneDef {
            id: BoneId(id),
            name: if id == 1 {
                "tip".to_owned()
            } else {
                "root".to_owned()
            },
            parent: if id == 1 { Some(0) } else { None },
            rest_translation,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        };
        let source_bones = vec![
            tip_bone(0, Vec3::ZERO),
            tip_bone(1, Vec3::new(0.0, -1.0, 0.0)),
        ];
        let source = SkeletonAsset {
            id: AssetId::generate(),
            name: "source".to_owned(),
            identity: compute_skeleton_identity(&source_bones),
            bones: source_bones,
            next_bone_id: 2,
        };
        let target_bones = vec![
            tip_bone(0, Vec3::ZERO),
            tip_bone(1, Vec3::new(0.0, -1.5, 0.0)),
        ];
        let target = SkeletonAsset {
            id: AssetId::generate(),
            name: "target".to_owned(),
            identity: compute_skeleton_identity(&target_bones),
            bones: target_bones,
            next_bone_id: 2,
        };
        let map = identity_bone_pair_map(&source, &target);
        let clip = AnimationClip {
            duration: 1.0,
            // No channels: "tip" stays at its rest translation for the whole
            // clip on both rigs (trivially planted), same as the sibling
            // test above.
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let without_override =
            retarget_clip(&clip, &source, &target, &map, &[]).expect("retarget must succeed");
        assert!(
            without_override.contacts.is_empty(),
            "\"tip\" matches no default heuristic pattern, so no override must detect nothing, got {:?}",
            without_override.contacts
        );

        let with_override = retarget_clip(&clip, &source, &target, &map, &["tip".to_owned()])
            .expect("retarget must succeed");
        assert!(
            with_override
                .contacts
                .iter()
                .any(|interval| interval.bone == BoneId(1)
                    && interval.start <= 0.0
                    && interval.end >= 1.0 - 1.0e-3),
            "the target source's contact_bones override must select \"tip\" and detect a full-clip interval, got {:?}",
            with_override.contacts
        );
    }

    // --- retarget_clip: T-pose -> A-pose rotation transfer (hand-computed) --

    #[test]
    fn t_pose_to_a_pose_rotation_transfer_matches_hand_computed_value() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        // A-pose target: the tip bone rests rotated -45 degrees around Z
        // relative to the source's T-pose rest.
        let target_tip_rest = Quat::from_rotation_z(-FRAC_PI_2 / 2.0);
        let target = two_bone_skeleton(AssetId::generate(), target_tip_rest);
        let map = identity_bone_pair_map(&source, &target);

        let animated_rotation = Quat::from_rotation_z(FRAC_PI_2 / 3.0); // 30 degrees
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(
                BoneId(1),
                vec![(0.0, Quat::IDENTITY), (1.0, animated_rotation)],
            )],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let retargeted = retarget_clip(&clip, &source, &target, &map, &[])
            .expect("T-pose to A-pose retarget must succeed");

        let tip_channel = retargeted
            .channels
            .iter()
            .find(|channel| {
                channel.target_bone == Some(BoneId(1)) && channel.property == AnimProperty::Rotation
            })
            .expect("tip rotation channel must exist");
        let output = Quat::from_array(crate::animation::lerp_channel(tip_channel, 1.0).unwrap());

        // Hand-computed: root stays at rest (identity) on both sides, so
        // q_dst_local(tip) = q_src_model * inverse(q_src_rest) * q_dst_rest
        //                   = rotate_z(30) * identity * rotate_z(-45)
        //                   = rotate_z(-15).
        let expected = Quat::from_rotation_z(-FRAC_PI_2 / 6.0);
        assert_quat_close(
            output,
            expected,
            1.0e-4,
            "T-pose to A-pose retarget must match the hand-computed -15 degree rotation",
        );
    }

    // --- retarget_clip: unequal chain counts --------------------------------

    #[test]
    fn nearest_chain_source_index_maps_endpoints_and_middle_by_normalized_position() {
        // source has 3 positions (0, 0.5, 1.0); target has 2 (0, 1.0).
        assert_eq!(nearest_chain_source_index(0, 2, 3), 0);
        assert_eq!(nearest_chain_source_index(1, 2, 3), 2);
    }

    #[test]
    fn unequal_chain_counts_retarget_by_nearest_normalized_position() {
        // Source: root -> mid -> tip (3 bones). Target: root -> tip (2 bones).
        let source_bones = vec![
            bone(0, "root", None, Vec3::ZERO, Quat::IDENTITY),
            bone(1, "mid", Some(0), Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
            bone(2, "tip", Some(1), Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
        ];
        let source_identity = compute_skeleton_identity(&source_bones);
        let source = SkeletonAsset {
            id: AssetId::generate(),
            name: "source".to_owned(),
            bones: source_bones,
            identity: source_identity,
            next_bone_id: 3,
        };

        let target_bones = vec![
            bone(0, "root", None, Vec3::ZERO, Quat::IDENTITY),
            bone(1, "tip", Some(0), Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
        ];
        let target_identity = compute_skeleton_identity(&target_bones);
        let target = SkeletonAsset {
            id: AssetId::generate(),
            name: "target".to_owned(),
            bones: target_bones,
            identity: target_identity,
            next_bone_id: 2,
        };

        let map = RetargetMap {
            schema_version: RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: source.id.clone(),
            source_identity: source.identity.0,
            target_skeleton: target.id.clone(),
            target_identity: target.identity.0,
            bone_pairs: Vec::new(),
            chain_pairs: vec![ChainPair {
                name: "spine".to_owned(),
                source_bones: vec![BoneId(0), BoneId(1), BoneId(2)],
                target_bones: vec![BoneId(0), BoneId(1)],
            }],
            translation: TranslationPolicy {
                mode: TranslationMode::None,
                scale: TranslationScale::Manual(1.0),
            },
            always_package: false,
        };

        let animated = Quat::from_rotation_y(0.4);
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(BoneId(2), vec![(0.0, animated)])],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let retargeted = retarget_clip(&clip, &source, &target, &map, &[])
            .expect("unequal chain count retarget must succeed");

        // Target tip (normalized position 1.0) must map to source tip
        // (index 2), not source mid.
        let tip_channel = retargeted.channels.iter().find(|channel| {
            channel.target_bone == Some(BoneId(1)) && channel.property == AnimProperty::Rotation
        });
        assert!(
            tip_channel.is_some(),
            "target tip must receive a channel from the mapped source tip"
        );
    }

    // --- retarget_clip: translation policy -----------------------------------

    #[test]
    fn hip_height_ratio_scales_root_translation_and_other_bones_keep_target_rest() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        // Target tip rest translation is twice the source's (double hip height
        // when the tip stands in for the hip via BoneId(1) in this synthetic
        // two-bone rig... use BoneId(0) as the actual root/hip instead).
        let mut target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        target.bones[1].rest_translation = Vec3::new(0.0, 4.0, 0.0);
        let map = identity_bone_pair_map(&source, &target);

        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![translation_channel(
                BoneId(0),
                vec![(0.0, Vec3::ZERO), (1.0, Vec3::new(0.0, 1.0, 0.0))],
            )],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            // Use the tip as the "hip" so its rest-height difference between
            // source (1.0) and target (4.0) produces a non-trivial ratio.
            root_bone: Some(BoneId(1)),
            contacts: Vec::new(),
        };

        let retargeted =
            retarget_clip(&clip, &source, &target, &map, &[]).expect("retarget must succeed");

        // BoneId(1) has no translation channel on the source clip, so its
        // sampled local translation is its rest value (0, 1, 0); scaled by
        // ratio 4.0 -> (0, 4, 0). The ratio is target_rest_hip_y / source_rest_hip_y
        // = 4.0 / 1.0 = 4.0.
        let carried_channel = retargeted
            .channels
            .iter()
            .find(|channel| {
                channel.target_bone == Some(BoneId(1))
                    && channel.property == AnimProperty::Translation
            })
            .expect("root-mapped bone must carry a translation channel");
        let value = crate::animation::lerp_channel(carried_channel, 0.0).unwrap();
        assert!(
            (value[1] - 4.0).abs() < 1.0e-4,
            "expected scaled hip height 4.0, got {}",
            value[1]
        );

        // BoneId(0) (not the mapped root here) must not carry translation.
        assert!(
            !retargeted.channels.iter().any(|channel| {
                channel.target_bone == Some(BoneId(0))
                    && channel.property == AnimProperty::Translation
            }),
            "under RootOnly, only the root-mapped bone may carry translation"
        );
    }

    #[test]
    fn missing_root_bone_mapping_errors_under_hip_height_ratio() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = RetargetMap {
            schema_version: RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: source.id.clone(),
            source_identity: source.identity.0,
            target_skeleton: target.id.clone(),
            target_identity: target.identity.0,
            // Only the tip is mapped; the clip's root_bone (BoneId(0)) has no
            // counterpart in the target.
            bone_pairs: vec![BonePair {
                source: BoneId(1),
                target: BoneId(1),
            }],
            chain_pairs: Vec::new(),
            translation: TranslationPolicy::default(),
            always_package: false,
        };
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(BoneId(1), vec![(0.0, Quat::IDENTITY)])],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let error = retarget_clip(&clip, &source, &target, &map, &[])
            .expect_err("missing root bone mapping must error under HipHeightRatio");
        assert_eq!(error, RetargetError::MissingRootBoneForHipHeightRatio);
    }

    #[test]
    fn empty_mapping_is_rejected() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = RetargetMap {
            schema_version: RETARGET_MAP_SCHEMA_VERSION,
            source_skeleton: source.id.clone(),
            source_identity: source.identity.0,
            target_skeleton: target.id.clone(),
            target_identity: target.identity.0,
            bone_pairs: Vec::new(),
            chain_pairs: Vec::new(),
            translation: TranslationPolicy::default(),
            always_package: false,
        };
        let clip = AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: None,
            contacts: Vec::new(),
        };

        let error =
            retarget_clip(&clip, &source, &target, &map, &[]).expect_err("empty map must error");
        assert_eq!(error, RetargetError::NoMappedBones);
    }

    // --- Baked clip envelope --------------------------------------------------

    #[test]
    fn baked_clip_round_trips_through_json() {
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(BoneId(1), vec![(0.0, Quat::IDENTITY)])],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(AssetId::generate()),
            skeleton_identity: Some(SkeletonIdentity(42)),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let bytes = serialize_baked_clip(&clip).expect("must serialize");
        let parsed = deserialize_baked_clip(&bytes).expect("must deserialize");

        assert_eq!(parsed.duration, clip.duration);
        assert_eq!(parsed.skeleton, clip.skeleton);
        assert_eq!(parsed.root_bone, clip.root_bone);
    }

    #[test]
    fn baked_clip_round_trips_contacts() {
        let clip = AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(AssetId::generate()),
            skeleton_identity: Some(SkeletonIdentity(7)),
            root_bone: Some(BoneId(0)),
            contacts: vec![crate::contact_detect::ContactInterval {
                bone: BoneId(3),
                start: 0.1,
                end: 0.4,
            }],
        };

        let bytes = serialize_baked_clip(&clip).expect("must serialize");
        let parsed = deserialize_baked_clip(&bytes).expect("must deserialize");

        assert_eq!(parsed.contacts, clip.contacts);
    }

    #[test]
    fn baked_clip_missing_contacts_field_defaults_to_empty() {
        // Simulates a cache entry baked before ADR 0080 added `contacts`:
        // baked entries are disposable, so a missing field must default
        // instead of failing to deserialize (no migration needed).
        let json = serde_json::json!({
            "schema_version": BAKED_CLIP_SCHEMA_VERSION,
            "clip": {
                "duration": 1.0,
                "channels": [],
                "events": [],
                "skeleton": null,
                "skeleton_identity": null,
                "root_bone": null,
            },
        });
        let bytes = serde_json::to_vec(&json).expect("must serialize");

        let parsed = deserialize_baked_clip(&bytes).expect("must deserialize without `contacts`");

        assert!(parsed.contacts.is_empty());
    }

    #[test]
    fn baked_clip_rejects_a_future_schema_version() {
        let clip = AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        };
        let bytes = serialize_baked_clip(&clip).expect("must serialize");
        let mut value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("must parse as JSON");
        value["schema_version"] = serde_json::json!(99);
        let mutated = serde_json::to_vec(&value).expect("must re-serialize");

        let error = deserialize_baked_clip(&mutated).expect_err("future version must be rejected");
        assert!(matches!(
            error,
            BakedClipError::UnsupportedVersion { found: 99 }
        ));
    }

    // --- Cache key composition -------------------------------------------------

    fn sample_map_for_key(source: &SkeletonAsset, target: &SkeletonAsset) -> RetargetMap {
        identity_bone_pair_map(source, target)
    }

    #[test]
    fn cache_key_changes_when_the_source_fingerprint_changes() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let base = cache_key_for_retargeted_clip(
            "fp-a",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let changed = cache_key_for_retargeted_clip(
            "fp-b",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_key_changes_when_the_clip_sub_asset_id_changes() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let base = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let changed = cache_key_for_retargeted_clip(
            "fp",
            "clip:1",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_key_changes_when_the_source_identity_changes() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let base = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let changed = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            SkeletonIdentity(source.identity.0 ^ 1),
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_key_changes_when_the_target_identity_changes() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let base = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let changed = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            SkeletonIdentity(target.identity.0 ^ 1),
            &map,
            &[],
        )
        .expect("key must compute");
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_key_changes_when_the_algorithm_version_would_change() {
        // RETARGET_ALGORITHM_VERSION is a compile-time constant, so this test
        // hashes it explicitly the same way cache_key_for_retargeted_clip does
        // to prove a version bump changes the key, without needing a second
        // build.
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let canonical_map_json = serde_json::to_vec(&map).expect("map must serialize");

        let hash_with = |algorithm_version: u32| {
            let mut hash = fnv1a_seed();
            hash_u64(&mut hash, u64::from(algorithm_version));
            hash_bytes(&mut hash, b"fp");
            hash_bytes(&mut hash, b"clip:0");
            hash_u64(&mut hash, source.identity.0);
            hash_u64(&mut hash, target.identity.0);
            hash_bytes(&mut hash, &canonical_map_json);
            hash_string_list(&mut hash, &[]);
            hash
        };

        assert_ne!(
            hash_with(RETARGET_ALGORITHM_VERSION),
            hash_with(RETARGET_ALGORITHM_VERSION + 1)
        );
    }

    #[test]
    fn cache_key_changes_when_the_map_json_changes() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);
        let mut changed_map = map.clone();
        changed_map.translation.scale = TranslationScale::Manual(2.0);

        let base = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let changed = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &changed_map,
            &[],
        )
        .expect("key must compute");
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_key_changes_when_the_target_contact_bones_override_changes() {
        // The worst bug class this cache design has (ADR 0079 §3): baked
        // contacts are re-detected against the *target* skeleton's own
        // `contact_bones` override, so forgetting it from the key would let
        // a stale bake outlive a changed override.
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = sample_map_for_key(&source, &target);

        let base = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &[],
        )
        .expect("key must compute");
        let with_override = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &["tip".to_owned()],
        )
        .expect("key must compute");
        assert_ne!(
            base, with_override,
            "a contact_bones override must change the cache key"
        );

        // An empty override built as a fresh `Vec` and the literal `&[]`
        // slice used everywhere else must hash identically: "no override" is
        // one value, not a None/Some(empty) distinction that could
        // spuriously bust the cache between equivalent callers.
        let empty_vec: Vec<String> = Vec::new();
        let via_empty_vec = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &empty_vec,
        )
        .expect("key must compute");
        assert_eq!(
            base, via_empty_vec,
            "an empty Vec and an empty slice literal must produce the same key"
        );

        // Order is part of the hash: the same two names in a different
        // order must not collide.
        let ordered_a = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &["left_foot".to_owned(), "right_foot".to_owned()],
        )
        .expect("key must compute");
        let ordered_b = cache_key_for_retargeted_clip(
            "fp",
            "clip:0",
            source.identity,
            target.identity,
            &map,
            &["right_foot".to_owned(), "left_foot".to_owned()],
        )
        .expect("key must compute");
        assert_ne!(
            ordered_a, ordered_b,
            "reordering the contact_bones override must change the key"
        );
    }

    // --- resolve_or_bake_retargeted_clip: cache round trip ---------------------

    #[test]
    fn resolve_or_bake_bakes_once_and_reuses_the_cache_on_a_second_call() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = DerivedCache::new(dir.path());
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::from_rotation_z(-0.2));
        let map = identity_bone_pair_map(&source, &target);
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(
                BoneId(1),
                vec![(0.0, Quat::from_rotation_y(0.5))],
            )],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        let first = resolve_or_bake_retargeted_clip(
            &cache,
            &clip,
            "fp",
            "clip:0",
            &source,
            &target,
            &map,
            &[],
        )
        .expect("first resolution must bake");
        let second = resolve_or_bake_retargeted_clip(
            &cache,
            &clip,
            "fp",
            "clip:0",
            &source,
            &target,
            &map,
            &[],
        )
        .expect("second resolution must hit the cache");

        assert_eq!(first.duration, second.duration);
        assert_eq!(first.skeleton, second.skeleton);
        assert_eq!(first.channels.len(), second.channels.len());
    }

    #[test]
    fn deleting_the_cache_recovers_by_rebaking() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = DerivedCache::new(dir.path());
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = identity_bone_pair_map(&source, &target);
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![rotation_channel(
                BoneId(1),
                vec![(0.0, Quat::from_rotation_y(0.3))],
            )],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: Some(BoneId(0)),
            contacts: Vec::new(),
        };

        resolve_or_bake_retargeted_clip(&cache, &clip, "fp", "clip:0", &source, &target, &map, &[])
            .expect("initial bake must succeed");
        cache.clear().expect("clear must succeed");
        let after_clear = resolve_or_bake_retargeted_clip(
            &cache,
            &clip,
            "fp",
            "clip:0",
            &source,
            &target,
            &map,
            &[],
        )
        .expect("resolution after clearing the cache must rebake successfully");

        assert_eq!(after_clear.skeleton, Some(target.id));
    }

    // --- load_registered_retarget_maps / find_retarget_map_for_pair -----------

    #[test]
    fn find_retarget_map_for_pair_matches_source_and_target_exactly() {
        let source = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let target = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let other = two_bone_skeleton(AssetId::generate(), Quat::IDENTITY);
        let map = identity_bone_pair_map(&source, &target);
        let maps = vec![(AssetId::generate(), map.clone())];

        let found = find_retarget_map_for_pair(&maps, &source.id, &target.id);
        assert_eq!(found, Some(&map));

        let not_found = find_retarget_map_for_pair(&maps, &source.id, &other.id);
        assert!(not_found.is_none());
    }
}
