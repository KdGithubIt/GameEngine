//! Skeleton asset identity: stable per-skeleton bone IDs and the canonical
//! structure hash used to dedupe and rebind skeletons across imports
//! (ADR 0077).
//!
//! [`BoneId`] is how every persisted binding (clip channels, the runtime
//! [`crate::skinning::Skeleton`] component) addresses a bone. Bone names stay
//! in the data only for first-import heuristics and human-facing
//! diagnostics; [`SkeletonIdentity`] is the canonical hash [`crate::gltf_import`]
//! uses to recognize when two imports describe the same rig.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// Rest-pose quantization step used by [`compute_skeleton_identity`].
///
/// `1e-4` is 0.1 mm at meter scale: far below authoring precision and far
/// above the float jitter a re-export of the same rig introduces, so jitter
/// alone cannot split identity (ADR 0077 §4).
const IDENTITY_QUANTIZATION: f32 = 1.0e-4;

/// Stable identity of one bone within one skeleton asset (ADR 0077).
///
/// Allocated sequentially at first import, persisted in the manifest as a
/// [`crate::asset::SkeletonRecord`], preserved across reimports, and never
/// reused once a bone is retired. `BoneId` is scoped to its skeleton asset —
/// it is not globally unique and is not a [`engine_authoring::StableId`]:
/// bones are always addressed through a skeleton reference in hand, so a
/// ULID per bone would bloat every channel and manifest record for no lookup
/// benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoneId(pub u32);

/// Canonical structure hash of a [`SkeletonAsset`] (ADR 0077 §4).
///
/// A 64-bit FNV-1a hash over a canonical byte stream: bone count, then per
/// bone in [`SkeletonAsset::bones`] order: UTF-8 name (length-prefixed),
/// parent index (`u32::MAX` for a root), and rest TRS quantized to this
/// module's private `IDENTITY_QUANTIZATION` step with the rotation
/// sign-canonicalized (`w >= 0`) before quantization so the two encodings of
/// the same rotation hash identically.
///
/// Equal identity implies equal bone order, names, and topology (up to the
/// accepted hash-collision risk documented on [`crate::gltf_import`]'s
/// dedupe rule), which is what lets that dedupe rule treat two hits as the
/// same rig without a bone-by-bone comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SkeletonIdentity(pub u64);

/// One bone in a [`SkeletonAsset`]'s rest pose (ADR 0077).
#[derive(Debug, Clone)]
pub struct BoneDef {
    /// Stable identity of this bone, unique within the owning [`SkeletonAsset`].
    pub id: BoneId,
    /// Human-readable name from the source document.
    ///
    /// Used only for first-import heuristics and diagnostics; every
    /// persisted binding uses [`Self::id`] instead.
    pub name: String,
    /// Index into the owning [`SkeletonAsset::bones`] of this bone's parent.
    /// `None` marks a root.
    pub parent: Option<usize>,
    /// Rest (bind-time local) translation captured from the source document.
    pub rest_translation: Vec3,
    /// Rest (bind-time local) rotation captured from the source document.
    pub rest_rotation: Quat,
    /// Rest (bind-time local) scale captured from the source document.
    pub rest_scale: Vec3,
}

/// A first-class runtime skeleton asset: the bone catalog and rest pose
/// shared by every skin bound to the same rig (ADR 0077).
///
/// Built by [`crate::gltf_import`] from a skin's joints and their ancestors,
/// in parent-before-child order (index `0` is always a root). Stored in
/// `Assets<SkeletonAsset>` and referenced by [`crate::skinning::Skeleton::asset`].
#[derive(Debug, Clone)]
pub struct SkeletonAsset {
    /// Imported sub-asset ID (existing `imported_sub_asset_id` derivation,
    /// unchanged by this type). Dedupe (ADR 0077 §4) may adopt an ID derived
    /// from a *different* source than the one this asset was just built
    /// from; see [`crate::gltf_import`] for the exact rule.
    pub id: engine_authoring::id::AssetId,
    /// Human-readable name from the source document.
    pub name: String,
    /// Parent-before-child bone list. Index `0` is a root.
    pub bones: Vec<BoneDef>,
    /// Canonical structure hash of [`Self::bones`] (see [`SkeletonIdentity`]).
    pub identity: SkeletonIdentity,
    /// Next [`BoneId`] value to allocate for a bone added by a future
    /// reimport. Monotonic; never decremented, so a retired bone's ID is
    /// never reused.
    pub next_bone_id: u32,
}

impl SkeletonAsset {
    /// Returns the index into [`Self::bones`] holding `id`, if present.
    pub fn bone_index(&self, id: BoneId) -> Option<usize> {
        self.bones.iter().position(|bone| bone.id == id)
    }
}

/// Runtime lookup from a skeleton's stable authoring
/// [`engine_authoring::id::AssetId`] to its full [`SkeletonAsset`] (bone
/// hierarchy, names, rest pose).
///
/// [`crate::skinning::Skeleton::asset`] stores only the stable ID (it is a
/// scene/prefab-persisted reference), while [`crate::asset::Assets`] keys its
/// store by process-local [`crate::asset::RuntimeAssetId`] via
/// [`crate::asset::Handle`] — neither gives a runtime system holding a
/// [`Skeleton`](crate::skinning::Skeleton) a way to reach the full bone
/// hierarchy. This registry is that missing lookup, kept deliberately
/// narrow rather than folded into `Assets<SkeletonAsset>`.
///
/// Consumed by [`crate::foot_ik::foot_ik_system`] (ADR 0080 §2), which needs
/// to walk `BoneId` parent links to resolve a leg chain. Populated
/// automatically by the scene bridge's `engine.skeleton` spawn callback
/// whenever that component spawns (creating the resource on demand,
/// mirroring `crate::scene_bridge`'s other on-demand asset stores), so
/// every host that goes through the scene bridge — Editor Play and the
/// packaged player alike — gets it for free. Absent it (never inserted at
/// all), or missing an entry for a particular skeleton, foot IK for that
/// character is a no-op rather than a guess.
#[derive(Debug, Default)]
pub struct SkeletonAssetRegistry {
    by_id: hashbrown::HashMap<engine_authoring::id::AssetId, SkeletonAsset>,
}

impl SkeletonAssetRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces the entry for `asset.id`.
    pub fn insert(&mut self, asset: SkeletonAsset) {
        self.by_id.insert(asset.id.clone(), asset);
    }

    /// Removes the entry for `id`, if any, returning it.
    ///
    /// Used to roll back entries added during a scene conversion that later
    /// fails atomically (`crate::scene_bridge::BridgeAssetState`).
    pub fn remove(&mut self, id: &engine_authoring::id::AssetId) -> Option<SkeletonAsset> {
        self.by_id.remove(id)
    }

    /// Returns the [`SkeletonAsset`] registered under `id`, if any.
    pub fn get(&self, id: &engine_authoring::id::AssetId) -> Option<&SkeletonAsset> {
        self.by_id.get(id)
    }

    /// Returns the number of registered skeletons.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Returns `true` when no skeletons are registered.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Computes the canonical [`SkeletonIdentity`] hash over `bones` (ADR 0077 §4).
///
/// The hash depends only on bone count, name, parent index, and quantized
/// rest TRS — never on [`BoneDef::id`], so recomputing identity after a
/// dedupe/rebind pass reassigns IDs is always a no-op.
pub fn compute_skeleton_identity(bones: &[BoneDef]) -> SkeletonIdentity {
    let mut hash: u64 = fnv1a_seed();
    hash_u64(&mut hash, bones.len() as u64);
    for bone in bones {
        hash_bytes(&mut hash, bone.name.as_bytes());
        let parent = bone.parent.map(|index| index as u32).unwrap_or(u32::MAX);
        hash_u64(&mut hash, u64::from(parent));
        hash_quantized_vec3(&mut hash, bone.rest_translation);
        hash_quantized_quat(&mut hash, bone.rest_rotation);
        hash_quantized_vec3(&mut hash, bone.rest_scale);
    }
    SkeletonIdentity(hash)
}

/// FNV-1a 64 offset basis, shared by every canonical hash in this crate
/// ([`compute_skeleton_identity`] and [`crate::derived_cache`]'s cache-key
/// composition) so there is exactly one FNV-1a implementation to audit.
#[doc(hidden)]
pub fn fnv1a_seed() -> u64 {
    0xcbf2_9ce4_8422_2325
}

/// Folds a length-prefixed byte string into an in-progress FNV-1a 64 hash.
///
/// Length-prefixing every variable-length field means, e.g., names "ab" +
/// "c" cannot hash the same as "a" + "bc" across a field boundary.
#[doc(hidden)]
pub fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    hash_u64(hash, bytes.len() as u64);
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Folds one `u64` (little-endian bytes) into an in-progress FNV-1a 64 hash.
#[doc(hidden)]
pub fn hash_u64(hash: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Quantizes one scalar to an `i64` step count of [`IDENTITY_QUANTIZATION`].
///
/// Negative zero and positive zero quantize identically (`round()` on `-0.0 /
/// step` yields `0`), and values on a quantization boundary always round the
/// same direction regardless of tiny float jitter below the step size.
fn quantize(value: f32) -> i64 {
    (value / IDENTITY_QUANTIZATION).round() as i64
}

fn hash_quantized_vec3(hash: &mut u64, value: Vec3) {
    for component in [value.x, value.y, value.z] {
        hash_u64(hash, quantize(component) as u64);
    }
}

/// Hashes a quaternion after canonicalizing its sign (`w >= 0`) so the two
/// float encodings of the same rotation (`q` and `-q`) quantize and hash
/// identically.
fn hash_quantized_quat(hash: &mut u64, value: Quat) {
    let canonical = if value.w < 0.0 { -value } else { value };
    for component in [canonical.x, canonical.y, canonical.z, canonical.w] {
        hash_u64(hash, quantize(component) as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn sample_bones() -> Vec<BoneDef> {
        vec![
            bone(0, "root", None, Vec3::ZERO, Quat::IDENTITY),
            bone(1, "tip", Some(0), Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
        ]
    }

    #[test]
    fn identity_is_stable_under_jitter_below_the_quantization_step() {
        let base = compute_skeleton_identity(&sample_bones());

        let mut jittered = sample_bones();
        jittered[1].rest_translation += Vec3::splat(1.0e-6);
        let jittered_identity = compute_skeleton_identity(&jittered);

        assert_eq!(
            base, jittered_identity,
            "jitter far below the 1e-4 quantization step must not change identity"
        );
    }

    #[test]
    fn identity_changes_when_a_bone_is_renamed() {
        let base = compute_skeleton_identity(&sample_bones());

        let mut renamed = sample_bones();
        renamed[1].name = "renamed_tip".to_owned();
        let renamed_identity = compute_skeleton_identity(&renamed);

        assert_ne!(
            base, renamed_identity,
            "a bone rename must change the canonical identity"
        );
    }

    #[test]
    fn identity_changes_when_rest_pose_moves_beyond_the_quantization_step() {
        let base = compute_skeleton_identity(&sample_bones());

        let mut moved = sample_bones();
        moved[1].rest_translation += Vec3::new(0.01, 0.0, 0.0);
        let moved_identity = compute_skeleton_identity(&moved);

        assert_ne!(
            base, moved_identity,
            "a rest-pose change beyond the quantization step must change identity"
        );
    }

    #[test]
    fn identity_is_insensitive_to_quaternion_sign() {
        let mut negated = sample_bones();
        negated[1].rest_rotation = Quat::from_xyzw(0.0, 0.0, 0.0, -1.0);
        let mut positive = sample_bones();
        positive[1].rest_rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 1.0);

        assert_eq!(
            compute_skeleton_identity(&negated),
            compute_skeleton_identity(&positive),
            "the two float encodings of the same rotation must hash identically"
        );
    }

    #[test]
    fn identity_does_not_depend_on_bone_id_assignment() {
        let base = compute_skeleton_identity(&sample_bones());

        let mut renumbered = sample_bones();
        for bone in &mut renumbered {
            bone.id = BoneId(bone.id.0 + 100);
        }
        let renumbered_identity = compute_skeleton_identity(&renumbered);

        assert_eq!(
            base, renumbered_identity,
            "identity must depend only on structure, not on assigned bone IDs"
        );
    }

    #[test]
    fn skeleton_asset_registry_resolves_by_stable_id_and_reports_missing_entries() {
        let asset = SkeletonAsset {
            id: engine_authoring::id::AssetId::generate(),
            name: "rig".to_owned(),
            bones: sample_bones(),
            identity: compute_skeleton_identity(&sample_bones()),
            next_bone_id: 2,
        };
        let mut registry = SkeletonAssetRegistry::new();
        assert!(registry.is_empty());
        registry.insert(asset.clone());

        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.get(&asset.id).map(|found| found.identity),
            Some(asset.identity)
        );
        assert!(registry
            .get(&engine_authoring::id::AssetId::generate())
            .is_none());

        let removed = registry.remove(&asset.id);
        assert_eq!(removed.map(|found| found.identity), Some(asset.identity));
        assert!(registry.is_empty());
        assert!(registry.remove(&asset.id).is_none());
    }

    #[test]
    fn bone_index_finds_the_matching_bone_id() {
        let asset = SkeletonAsset {
            id: engine_authoring::id::AssetId::generate(),
            name: "rig".to_owned(),
            bones: sample_bones(),
            identity: compute_skeleton_identity(&sample_bones()),
            next_bone_id: 2,
        };

        assert_eq!(asset.bone_index(BoneId(1)), Some(1));
        assert_eq!(asset.bone_index(BoneId(9)), None);
    }
}
