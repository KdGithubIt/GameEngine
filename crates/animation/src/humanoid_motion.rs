//! Skeleton-independent humanoid motion conversion and target baking (ADR 0110).
//!
//! Native clips are converted to portable model-space rotation deltas. Target
//! adaptation resolves those deltas against the target rest pose and emits an
//! ordinary target-bound `AnimationClip` before runtime playback.

use crate::animation::{lerp_channel, AnimChannel, AnimEvent, AnimProperty, AnimationClip, Keyframe};
use crate::derived_cache::{CacheKey, DerivedCache};
use crate::humanoid::{validate_humanoid_profile, HumanoidError};
use crate::skeleton_asset::{BoneId, SkeletonAsset};
use engine_assets::asset::{HumanoidBone, HumanoidProfile};
use engine_authoring::diagnostic::Diagnostic;
use glam::{Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Bumped whenever unchanged humanoid inputs would bake to different clip bytes.
pub const HUMANOID_RETARGET_ALGORITHM_VERSION: u32 = 1;

/// Schema version embedded in packaged humanoid baked-clip envelopes.
pub const HUMANOID_BAKED_CLIP_SCHEMA_VERSION: u32 = 1;
/// Derived-cache domain used for humanoid target bakes.
pub const HUMANOID_CACHE_DOMAIN: &str = "humanoid_anim";
/// File extension used for serialized humanoid target bakes.
pub const HUMANOID_BAKED_CLIP_FILE_EXTENSION: &str = "clip.json";
/// Diagnostic emitted when a packaged Humanoid source has no staged target bake.
pub const HUMANOID_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC: &str =
    "anim.humanoid_bake_missing_from_package";

const CACHE_SCHEMA_VERSION: u32 = HUMANOID_BAKED_CLIP_SCHEMA_VERSION;
const CACHE_DOMAIN: &str = HUMANOID_CACHE_DOMAIN;
const CACHE_EXTENSION: &str = HUMANOID_BAKED_CLIP_FILE_EXTENSION;
const QUAT_EPSILON: f32 = 1.0e-8;

/// One portable model-space rotation-delta channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanoidRotationChannel {
    /// Semantic body target, independent of any concrete skeleton.
    pub bone: HumanoidBone,
    /// Model-space delta samples stored as XYZW quaternions.
    pub keyframes: Vec<Keyframe>,
}

/// Portable root-motion translation sampled in source model space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanoidRootMotion {
    /// Model-space translation deltas from the source rest pose.
    pub keyframes: Vec<Keyframe>,
}

/// Skeleton-independent derivative of one native skeletal animation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanoidMotion {
    /// Total motion length in seconds.
    pub duration: f32,
    /// Portable model-space rotation deltas keyed by humanoid semantics.
    pub rotations: Vec<HumanoidRotationChannel>,
    /// Optional source-model locomotion metadata from the profile motion root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_motion: Option<HumanoidRootMotion>,
    /// Timeline markers that are independent of concrete bone IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AnimEvent>,
    /// Source skeleton identity retained for provenance and stale-data diagnostics.
    pub source_skeleton_identity: u64,
}

/// Successful native-to-humanoid conversion and its non-blocking diagnostics.
#[derive(Debug, Clone)]
pub struct HumanoidMotionBuildResult {
    /// Portable motion payload.
    pub motion: HumanoidMotion,
    /// Warnings for source-specific channels deliberately retained only by Native.
    pub diagnostics: Vec<Diagnostic>,
}

/// Successful humanoid-to-target bake and its non-blocking diagnostics.
#[derive(Debug, Clone)]
pub struct HumanoidBakeResult {
    /// Ordinary target-skeleton clip consumed by Animator.
    pub clip: AnimationClip,
    /// Warnings for portable data intentionally not applied by current policy.
    pub diagnostics: Vec<Diagnostic>,
}

/// Failure while converting or baking portable humanoid motion.
#[derive(Debug)]
pub enum HumanoidMotionError {
    /// Source or target profile is invalid for its skeleton.
    Profile(HumanoidError),
    /// Native clip does not match the source skeleton/profile pair.
    ClipSkeletonMismatch,
    /// Cache serialization or persistence failed.
    Cache(String),
}

impl fmt::Display for HumanoidMotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => write!(formatter, "invalid humanoid profile: {error}"),
            Self::ClipSkeletonMismatch => formatter.write_str(
                "native clip does not match the humanoid source skeleton",
            ),
            Self::Cache(message) => write!(formatter, "humanoid derived-cache error: {message}"),
        }
    }
}

impl std::error::Error for HumanoidMotionError {}

impl From<HumanoidError> for HumanoidMotionError {
    fn from(error: HumanoidError) -> Self {
        Self::Profile(error)
    }
}

/// Converts a source-bound native clip to portable humanoid body motion.
pub fn build_humanoid_motion(
    clip: &AnimationClip,
    skeleton: &SkeletonAsset,
    profile: &HumanoidProfile,
) -> Result<HumanoidMotionBuildResult, HumanoidMotionError> {
    validate_humanoid_profile(profile, skeleton)?;
    if clip.skeleton.as_ref() != Some(&skeleton.id)
        || clip.skeleton_identity != Some(skeleton.identity)
    {
        return Err(HumanoidMotionError::ClipSkeletonMismatch);
    }

    let rotation_channels = channels_by_bone(clip, AnimProperty::Rotation);
    let rest_model = rest_model_rotations(skeleton);
    let times = transform_sample_times(clip);
    let mut rotations = Vec::new();

    for (&semantic, &bone_value) in &profile.bones {
        let Some(index) = skeleton.bone_index(BoneId(bone_value)) else {
            continue;
        };
        let affected = ancestor_chain(skeleton, index)
            .into_iter()
            .any(|ancestor| rotation_channels.contains_key(&skeleton.bones[ancestor].id));
        if !affected {
            continue;
        }
        let keyframes = times
            .iter()
            .map(|&time| {
                let animated = animated_model_transforms(clip, skeleton, time).0;
                let delta = normalized(animated[index] * rest_model[index].inverse());
                Keyframe {
                    time,
                    value: quat_value(delta),
                }
            })
            .collect::<Vec<_>>();
        if !all_identity(&keyframes) {
            rotations.push(HumanoidRotationChannel {
                bone: semantic,
                keyframes,
            });
        }
    }
    rotations.sort_by_key(|channel| channel.bone);

    let mapped = profile.bones.values().copied().collect::<HashSet<_>>();
    let excluded = clip
        .channels
        .iter()
        .filter(|channel| {
            channel.property != AnimProperty::Rotation
                || channel
                    .target_bone
                    .is_none_or(|bone| !mapped.contains(&bone.0))
        })
        .count()
        + clip.morph_channels.len();
    let mut diagnostics = Vec::new();
    if excluded != 0 {
        diagnostics.push(Diagnostic::warning(
            "anim.humanoid_source_channels_excluded",
            format!(
                "Humanoid keeps portable body rotations; {excluded} source-specific transform/morph channels remain available on Native"
            ),
        ));
    }

    let root_motion = extract_model_space_root_motion(clip, skeleton, profile, &times);
    if root_motion.is_some() {
        diagnostics.push(Diagnostic::warning(
            "anim.humanoid_root_motion_scaling_unspecified",
            "portable root motion is recorded in source model space but target translation scaling is intentionally not inferred",
        ));
    }

    Ok(HumanoidMotionBuildResult {
        motion: HumanoidMotion {
            duration: clip.duration,
            rotations,
            root_motion,
            events: clip.events.clone(),
            source_skeleton_identity: skeleton.identity.0,
        },
        diagnostics,
    })
}

/// Bakes portable humanoid rotations onto one target profile and skeleton.
///
/// The result is an ordinary target-bound clip. Portable root translation is
/// not copied because ADR 0110 deliberately leaves cross-proportion scaling to
/// an explicit policy rather than silently guessing one.
pub fn bake_humanoid_motion(
    motion: &HumanoidMotion,
    target: &SkeletonAsset,
    profile: &HumanoidProfile,
) -> Result<HumanoidBakeResult, HumanoidMotionError> {
    validate_humanoid_profile(profile, target)?;
    let rest_model = rest_model_rotations(target);
    let source = motion
        .rotations
        .iter()
        .map(|channel| (channel.bone, channel))
        .collect::<BTreeMap<_, _>>();
    let target_semantics = profile
        .bones
        .iter()
        .filter_map(|(&semantic, &bone)| {
            target
                .bone_index(BoneId(bone))
                .map(|index| (index, semantic))
        })
        .collect::<HashMap<_, _>>();
    let times = humanoid_sample_times(motion);
    let mut output = BTreeMap::<HumanoidBone, Vec<Keyframe>>::new();

    for time in times {
        let mut model_rotations = Vec::<Quat>::with_capacity(target.bones.len());
        let mut local_rotations = Vec::<Quat>::with_capacity(target.bones.len());
        for (index, bone) in target.bones.iter().enumerate() {
            let model = if let Some(semantic) = target_semantics.get(&index)
                && let Some(channel) = source.get(semantic)
            {
                normalized(sample_humanoid_rotation(channel, time) * rest_model[index])
            } else {
                match bone.parent {
                    Some(parent) => normalized(model_rotations[parent] * bone.rest_rotation),
                    None => normalized(bone.rest_rotation),
                }
            };
            let local = match bone.parent {
                Some(parent) => normalized(model_rotations[parent].inverse() * model),
                None => model,
            };
            model_rotations.push(model);
            local_rotations.push(local);
        }

        for (&index, &semantic) in &target_semantics {
            if source.contains_key(&semantic) {
                output.entry(semantic).or_default().push(Keyframe {
                    time,
                    value: quat_value(local_rotations[index]),
                });
            }
        }
    }

    let mut channels = output
        .into_iter()
        .map(|(semantic, keyframes)| AnimChannel {
            property: AnimProperty::Rotation,
            target_bone: Some(BoneId(profile.bones[&semantic])),
            keyframes,
        })
        .collect::<Vec<_>>();
    channels.sort_by_key(|channel| channel.target_bone.map_or(u32::MAX, |bone| bone.0));

    let mut diagnostics = Vec::new();
    if motion.root_motion.is_some() {
        diagnostics.push(Diagnostic::warning(
            "anim.humanoid_root_motion_not_applied",
            "target bake omitted portable root translation because no cross-proportion translation policy is configured",
        ));
    }

    Ok(HumanoidBakeResult {
        clip: AnimationClip {
            duration: motion.duration,
            channels,
            morph_channels: Vec::new(),
            events: motion.events.clone(),
            skeleton: Some(target.id.clone()),
            skeleton_identity: Some(target.identity),
            root_bone: profile.motion_root.map(BoneId),
            contacts: Vec::new(),
        },
        diagnostics,
    })
}

/// Computes the content-addressed cache key for one humanoid target bake.
pub fn humanoid_bake_cache_key(
    motion: &HumanoidMotion,
    target: &SkeletonAsset,
    profile: &HumanoidProfile,
) -> Result<CacheKey, HumanoidMotionError> {
    let motion_json = serde_json::to_vec(motion).map_err(cache_error)?;
    let profile_json = serde_json::to_vec(profile).map_err(cache_error)?;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    hash_u64(&mut hash, u64::from(HUMANOID_RETARGET_ALGORITHM_VERSION));
    hash_bytes(&mut hash, &motion_json);
    hash_u64(&mut hash, target.identity.0);
    hash_bytes(&mut hash, &profile_json);
    Ok(CacheKey(hash))
}

/// Loads one target bake from Derived Cache or creates it on cache miss.
pub fn resolve_or_bake_humanoid_motion(
    cache: &DerivedCache,
    motion: &HumanoidMotion,
    target: &SkeletonAsset,
    profile: &HumanoidProfile,
) -> Result<HumanoidBakeResult, HumanoidMotionError> {
    validate_humanoid_profile(profile, target)?;
    let key = humanoid_bake_cache_key(motion, target, profile)?;
    if let Some(bytes) = cache.get(CACHE_DOMAIN, &key, CACHE_EXTENSION)
        && let Ok(clip) = deserialize_humanoid_baked_clip(&bytes)
    {
        return Ok(HumanoidBakeResult {
            clip,
            diagnostics: Vec::new(),
        });
    }

    let baked = bake_humanoid_motion(motion, target, profile)?;
    let bytes = serialize_humanoid_baked_clip(&baked.clip)?;
    cache
        .put(CACHE_DOMAIN, &key, CACHE_EXTENSION, &bytes)
        .map_err(|error| HumanoidMotionError::Cache(error.to_string()))?;
    Ok(baked)
}

/// Serializes one target-bound humanoid bake for package staging.
///
/// # Errors
///
/// Returns [`HumanoidMotionError::Cache`] when the baked clip cannot be encoded.
pub fn serialize_humanoid_baked_clip(
    clip: &AnimationClip,
) -> Result<Vec<u8>, HumanoidMotionError> {
    serde_json::to_vec(&HumanoidBakeEnvelope {
        schema_version: HUMANOID_BAKED_CLIP_SCHEMA_VERSION,
        clip: clip.clone(),
    })
    .map_err(cache_error)
}

/// Decodes one packaged humanoid target bake.
///
/// # Errors
///
/// Returns [`HumanoidMotionError::Cache`] when the envelope is malformed or uses
/// an unsupported schema version.
pub fn deserialize_humanoid_baked_clip(
    bytes: &[u8],
) -> Result<AnimationClip, HumanoidMotionError> {
    let envelope: HumanoidBakeEnvelope = serde_json::from_slice(bytes).map_err(cache_error)?;
    if envelope.schema_version != HUMANOID_BAKED_CLIP_SCHEMA_VERSION {
        return Err(HumanoidMotionError::Cache(format!(
            "unsupported humanoid baked-clip schema version {}",
            envelope.schema_version
        )));
    }
    Ok(envelope.clip)
}

#[derive(Serialize, Deserialize)]
struct HumanoidBakeEnvelope {
    schema_version: u32,
    clip: AnimationClip,
}

fn cache_error(error: serde_json::Error) -> HumanoidMotionError {
    HumanoidMotionError::Cache(error.to_string())
}

fn ancestor_chain(skeleton: &SkeletonAsset, mut index: usize) -> Vec<usize> {
    let mut chain = vec![index];
    while let Some(parent) = skeleton.bones[index].parent {
        chain.push(parent);
        index = parent;
    }
    chain
}

fn channels_by_bone(
    clip: &AnimationClip,
    property: AnimProperty,
) -> HashMap<BoneId, &AnimChannel> {
    clip.channels
        .iter()
        .filter(|channel| channel.property == property)
        .filter_map(|channel| channel.target_bone.map(|bone| (bone, channel)))
        .collect()
}

fn rest_model_rotations(skeleton: &SkeletonAsset) -> Vec<Quat> {
    let mut result = Vec::<Quat>::with_capacity(skeleton.bones.len());
    for bone in &skeleton.bones {
        let rotation = match bone.parent {
            Some(parent) => normalized(result[parent] * bone.rest_rotation),
            None => normalized(bone.rest_rotation),
        };
        result.push(rotation);
    }
    result
}

fn rest_model_translations(skeleton: &SkeletonAsset) -> Vec<Vec3> {
    let mut rotations = Vec::<Quat>::with_capacity(skeleton.bones.len());
    let mut translations = Vec::<Vec3>::with_capacity(skeleton.bones.len());
    for bone in &skeleton.bones {
        match bone.parent {
            Some(parent) => {
                translations.push(translations[parent] + rotations[parent] * bone.rest_translation);
                rotations.push(normalized(rotations[parent] * bone.rest_rotation));
            }
            None => {
                translations.push(bone.rest_translation);
                rotations.push(normalized(bone.rest_rotation));
            }
        }
    }
    translations
}

fn animated_model_transforms(
    clip: &AnimationClip,
    skeleton: &SkeletonAsset,
    time: f32,
) -> (Vec<Quat>, Vec<Vec3>) {
    let rotations = channels_by_bone(clip, AnimProperty::Rotation);
    let translations = channels_by_bone(clip, AnimProperty::Translation);
    let mut model_rotations = Vec::<Quat>::with_capacity(skeleton.bones.len());
    let mut model_translations = Vec::<Vec3>::with_capacity(skeleton.bones.len());
    for bone in &skeleton.bones {
        let local_rotation = rotations
            .get(&bone.id)
            .and_then(|channel| lerp_channel(channel, time))
            .map(quat_from_value)
            .unwrap_or(bone.rest_rotation);
        let local_translation = translations
            .get(&bone.id)
            .and_then(|channel| lerp_channel(channel, time))
            .map(vec3_from_value)
            .unwrap_or(bone.rest_translation);
        match bone.parent {
            Some(parent) => {
                model_translations
                    .push(model_translations[parent] + model_rotations[parent] * local_translation);
                model_rotations.push(normalized(model_rotations[parent] * local_rotation));
            }
            None => {
                model_translations.push(local_translation);
                model_rotations.push(normalized(local_rotation));
            }
        }
    }
    (model_rotations, model_translations)
}

fn transform_sample_times(clip: &AnimationClip) -> Vec<f32> {
    let mut times = BTreeSet::<OrderedTime>::new();
    times.insert(OrderedTime(0.0));
    if clip.duration.is_finite() && clip.duration > 0.0 {
        times.insert(OrderedTime(clip.duration));
    }
    for keyframe in clip.channels.iter().flat_map(|channel| &channel.keyframes) {
        if keyframe.time.is_finite() && keyframe.time >= 0.0 {
            times.insert(OrderedTime(keyframe.time));
        }
    }
    times.into_iter().map(|time| time.0).collect()
}

fn humanoid_sample_times(motion: &HumanoidMotion) -> Vec<f32> {
    let mut times = BTreeSet::<OrderedTime>::new();
    times.insert(OrderedTime(0.0));
    if motion.duration.is_finite() && motion.duration > 0.0 {
        times.insert(OrderedTime(motion.duration));
    }
    for keyframe in motion
        .rotations
        .iter()
        .flat_map(|channel| &channel.keyframes)
    {
        if keyframe.time.is_finite() && keyframe.time >= 0.0 {
            times.insert(OrderedTime(keyframe.time));
        }
    }
    times.into_iter().map(|time| time.0).collect()
}

fn extract_model_space_root_motion(
    clip: &AnimationClip,
    skeleton: &SkeletonAsset,
    profile: &HumanoidProfile,
    times: &[f32],
) -> Option<HumanoidRootMotion> {
    let index = skeleton.bone_index(BoneId(profile.motion_root?))?;
    let rest = rest_model_translations(skeleton)[index];
    let animated_root_exists = clip.channels.iter().any(|channel| {
        channel.property == AnimProperty::Translation
            && channel.target_bone == Some(skeleton.bones[index].id)
    });
    if !animated_root_exists {
        return None;
    }
    Some(HumanoidRootMotion {
        keyframes: times
            .iter()
            .map(|&time| {
                let (_, translations) = animated_model_transforms(clip, skeleton, time);
                let delta = translations[index] - rest;
                Keyframe {
                    time,
                    value: [delta.x, delta.y, delta.z, 0.0],
                }
            })
            .collect(),
    })
}

fn sample_humanoid_rotation(channel: &HumanoidRotationChannel, time: f32) -> Quat {
    let Some(first) = channel.keyframes.first() else {
        return Quat::IDENTITY;
    };
    if time <= first.time {
        return quat_from_value(first.value);
    }
    if let Some(last) = channel.keyframes.last()
        && time >= last.time
    {
        return quat_from_value(last.value);
    }
    for pair in channel.keyframes.windows(2) {
        if time <= pair[1].time {
            let span = pair[1].time - pair[0].time;
            let alpha = if span <= f32::EPSILON {
                0.0
            } else {
                (time - pair[0].time) / span
            };
            return normalized(
                quat_from_value(pair[0].value).slerp(quat_from_value(pair[1].value), alpha),
            );
        }
    }
    Quat::IDENTITY
}

fn all_identity(keyframes: &[Keyframe]) -> bool {
    keyframes.iter().all(|keyframe| {
        let rotation = quat_from_value(keyframe.value);
        rotation.x.abs() < 1.0e-6
            && rotation.y.abs() < 1.0e-6
            && rotation.z.abs() < 1.0e-6
            && (rotation.w.abs() - 1.0).abs() < 1.0e-6
    })
}

fn vec3_from_value(value: [f32; 4]) -> Vec3 {
    Vec3::new(value[0], value[1], value[2])
}

fn quat_from_value(value: [f32; 4]) -> Quat {
    normalized(Quat::from_xyzw(value[0], value[1], value[2], value[3]))
}

fn quat_value(rotation: Quat) -> [f32; 4] {
    let rotation = normalized(rotation);
    [rotation.x, rotation.y, rotation.z, rotation.w]
}

fn normalized(rotation: Quat) -> Quat {
    if rotation.length_squared() <= QUAT_EPSILON {
        Quat::IDENTITY
    } else {
        rotation.normalize()
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    hash_u64(hash, bytes.len() as u64);
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[derive(Clone, Copy)]
struct OrderedTime(f32);

impl PartialEq for OrderedTime {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedTime {}

impl PartialOrd for OrderedTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::humanoid::detect_humanoid_profile;
    use crate::skeleton_asset::{compute_skeleton_identity, BoneDef};
    use engine_authoring::id::AssetId;

    fn skeleton(id: AssetId) -> SkeletonAsset {
        let definitions = [
            ("Hips", None), ("helper", Some(0)), ("Spine", Some(1)), ("Head", Some(2)),
            ("LeftArm", Some(2)), ("LeftForeArm", Some(4)), ("LeftHand", Some(5)),
            ("RightArm", Some(2)), ("RightForeArm", Some(7)), ("RightHand", Some(8)),
            ("LeftUpLeg", Some(0)), ("LeftLeg", Some(10)), ("LeftFoot", Some(11)),
            ("RightUpLeg", Some(0)), ("RightLeg", Some(13)), ("RightFoot", Some(14)),
        ];
        let bones = definitions.iter().enumerate().map(|(index, (name, parent))| BoneDef {
            id: BoneId(index as u32),
            name: (*name).to_owned(),
            parent: *parent,
            rest_translation: Vec3::ZERO,
            rest_rotation: Quat::IDENTITY,
            rest_scale: Vec3::ONE,
        }).collect::<Vec<_>>();
        SkeletonAsset {
            id,
            name: "test_humanoid".to_owned(),
            identity: compute_skeleton_identity(&bones),
            next_bone_id: bones.len() as u32,
            bones,
        }
    }

    #[test]
    fn model_space_rotation_delta_bakes_to_target() {
        let source = skeleton(AssetId::generate());
        let target = skeleton(AssetId::generate());
        let source_profile = detect_humanoid_profile(&source).profile.expect("source profile");
        let target_profile = detect_humanoid_profile(&target).profile.expect("target profile");
        let expected = Quat::from_rotation_y(0.6);
        let clip = AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Rotation,
                target_bone: Some(BoneId(source_profile.bones[&HumanoidBone::LeftUpperArm])),
                keyframes: vec![
                    Keyframe { time: 0.0, value: quat_value(Quat::IDENTITY) },
                    Keyframe { time: 1.0, value: quat_value(expected) },
                ],
            }],
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: Some(source.id.clone()),
            skeleton_identity: Some(source.identity),
            root_bone: None,
            contacts: Vec::new(),
        };
        let portable = build_humanoid_motion(&clip, &source, &source_profile).expect("portable").motion;
        let baked = bake_humanoid_motion(&portable, &target, &target_profile).expect("bake").clip;
        let target_bone = BoneId(target_profile.bones[&HumanoidBone::LeftUpperArm]);
        let channel = baked.channels.iter().find(|channel| channel.target_bone == Some(target_bone)).expect("target channel");
        let actual = quat_from_value(lerp_channel(channel, 1.0).expect("sample"));
        assert!(actual.dot(expected).abs() > 0.99999);

        let bytes = serialize_humanoid_baked_clip(&baked).expect("serialize bake");
        let decoded = deserialize_humanoid_baked_clip(&bytes).expect("deserialize bake");
        assert_eq!(decoded.skeleton, baked.skeleton);
        assert_eq!(decoded.skeleton_identity, baked.skeleton_identity);
        assert_eq!(decoded.channels.len(), baked.channels.len());
    }
}
