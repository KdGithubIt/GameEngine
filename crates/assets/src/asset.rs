use engine_authoring::id::{AssetId, IdError};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies an asset in one running process.
///
/// Runtime asset IDs are not stable authoring identifiers and must not be
/// persisted in project files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeAssetId(u64);

impl RuntimeAssetId {
    fn generate() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let id = COUNTER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(1))
            .expect("runtime asset ID space must not be exhausted");
        Self(id)
    }

    /// Returns the process-local numeric value.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// A lightweight typed reference to an asset stored in [`Assets`].
pub struct Handle<T> {
    id: RuntimeAssetId,
    marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    /// Returns the runtime asset ID referenced by this handle.
    pub fn id(&self) -> RuntimeAssetId {
        self.id
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Handle<T> {}
impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T> Eq for Handle<T> {}
impl<T> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}
impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Handle<{}>({:?})", std::any::type_name::<T>(), self.id)
    }
}

/// Stores runtime assets of one Rust type.
pub struct Assets<T> {
    store: HashMap<RuntimeAssetId, T>,
}

impl<T> Assets<T> {
    /// Creates an empty asset store.
    pub fn new() -> Self {
        Self { store: HashMap::new() }
    }

    /// Adds an asset and returns a typed handle to it.
    ///
    /// # Panics
    ///
    /// Panics if all runtime asset IDs have been exhausted.
    pub fn add(&mut self, asset: T) -> Handle<T> {
        let id = RuntimeAssetId::generate();
        self.store.insert(id, asset);
        Handle { id, marker: PhantomData }
    }

    /// Returns the asset referenced by `handle`.
    pub fn get(&self, handle: &Handle<T>) -> Option<&T> {
        self.store.get(&handle.id)
    }

    /// Returns the asset referenced by `handle` for mutation.
    pub fn get_mut(&mut self, handle: &Handle<T>) -> Option<&mut T> {
        self.store.get_mut(&handle.id)
    }

    /// Returns a typed handle for an existing runtime asset ID.
    pub fn handle(&self, id: RuntimeAssetId) -> Option<Handle<T>> {
        self.store.contains_key(&id).then_some(Handle { id, marker: PhantomData })
    }

    /// Removes and returns the asset referenced by `handle`.
    pub fn remove(&mut self, handle: &Handle<T>) -> Option<T> {
        self.store.remove(&handle.id)
    }

    /// Returns the number of stored assets.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns `true` when no assets are stored.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Iterates over runtime asset IDs and shared asset references.
    pub fn iter(&self) -> impl Iterator<Item = (RuntimeAssetId, &T)> {
        self.store.iter().map(|(&id, asset)| (id, asset))
    }
}

impl<T> Default for Assets<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Texture compression target stored in import settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureCompression {
    /// No compression applied.
    #[default]
    None,
    /// Block-compressed format from the BC family.
    Bc,
}

/// Audio import format stored in import settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioImportFormat {
    /// Uncompressed PCM.
    #[default]
    Pcm,
    /// Ogg Vorbis encoding.
    Vorbis,
}

/// Cheap filesystem metadata recorded for one imported source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileStamp {
    /// Whole seconds since the Unix epoch, when the platform reports a time.
    pub modified_unix_seconds: Option<u64>,
    /// Nanosecond fraction paired with [`Self::modified_unix_seconds`].
    pub modified_subsec_nanos: u32,
    /// File length in bytes.
    pub length: u64,
}

impl SourceFileStamp {
    fn capture(path: &Path) -> Result<Self, std::io::Error> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        Ok(Self {
            modified_unix_seconds: modified.map(|duration| duration.as_secs()),
            modified_subsec_nanos: modified.map_or(0, |duration| duration.subsec_nanos()),
            length: metadata.len(),
        })
    }
}

/// Source and dependency stamps from the latest successful import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp {
    /// Stamp for the registered source itself.
    pub source: SourceFileStamp,
    /// Stamps in the same order as [`ImportSettings::source_dependencies`].
    pub dependencies: Vec<SourceFileStamp>,
}

impl SourceStamp {
    /// Captures metadata for a source and its ordered dependencies.
    ///
    /// # Errors
    ///
    /// Returns the first metadata error from the source or a dependency.
    pub fn capture(source: &Path, dependencies: &[PathBuf]) -> Result<Self, std::io::Error> {
        Ok(Self {
            source: SourceFileStamp::capture(source)?,
            dependencies: dependencies
                .iter()
                .map(|path| SourceFileStamp::capture(path))
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

/// Per-asset import settings persisted in the asset manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImportSettings {
    /// Texture compression target.
    #[serde(default, skip_serializing_if = "is_default")]
    pub texture_compression: TextureCompression,
    /// Number of generated mesh LOD levels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_lod_target_count: Option<u32>,
    /// Target encoding for audio assets.
    #[serde(default, skip_serializing_if = "is_default")]
    pub audio_format: AudioImportFormat,
    /// Stable content fingerprint of the source plus declared sidecars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    /// Cheap metadata hint from the latest successful import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_stamp: Option<SourceStamp>,
    /// Source-relative external files required by this asset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_dependencies: Vec<String>,
    /// Deterministic sub-assets discovered during the latest successful import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_assets: Vec<ImportedSubAsset>,
    /// Project-relative path of generated import output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_prefab: Option<String>,
    /// Bone-catalog ledger for skeletons bound to this source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skeleton_records: Vec<SkeletonRecord>,
    /// Model-owned humanoid semantic mappings keyed by stable skeleton/bone IDs (ADR 0110).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub humanoid_profiles: Vec<HumanoidProfile>,
    /// Bone names that replace the default contact-detection heuristic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contact_bones: Vec<String>,
    /// Model sources whose rigs receive clips from this motion source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motion_model_sources: Vec<String>,
    /// Optional original model used before retargeting a motion to output models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_original_model_source: Option<String>,
    /// Imported material sub-asset ID to standalone material asset ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub material_remaps: BTreeMap<String, String>,
    /// Imported texture sub-asset ID to standalone texture asset ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub texture_remaps: BTreeMap<String, String>,
}

impl ImportSettings {
    /// Returns the effective model targets for a motion source.
    pub fn resolved_motion_model_sources(&self) -> Vec<&str> {
        self.motion_model_sources.iter().map(String::as_str).collect()
    }

    /// Returns `true` when all fields are at their default values.
    pub fn is_all_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Persisted bone-catalog record for one skeleton bound to a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonRecord {
    /// Stable sub-asset ID string of the skeleton.
    pub id: String,
    /// Canonical skeleton structure hash.
    pub identity: u64,
    /// Next monotonic bone ID to allocate.
    pub next_bone_id: u32,
    /// Name-to-ID assignments from the latest successful import.
    pub bones: Vec<SkeletonBoneRecord>,
}

/// One bone's persisted name-to-ID assignment within a [`SkeletonRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonBoneRecord {
    /// Stable identity within its skeleton asset.
    pub bone_id: u32,
    /// Bone name used as the reimport matching heuristic.
    pub name: String,
}

/// Stable humanoid body semantics shared by model profiles and portable motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanoidBone {
    /// Pelvis / body root semantic.
    Hips,
    /// Lower torso semantic.
    Spine,
    /// Upper torso semantic.
    Chest,
    /// Uppermost torso semantic.
    UpperChest,
    /// Neck semantic.
    Neck,
    /// Head semantic.
    Head,
    /// Left shoulder semantic.
    LeftShoulder,
    /// Left upper arm semantic.
    LeftUpperArm,
    /// Left lower arm semantic.
    LeftLowerArm,
    /// Left hand semantic.
    LeftHand,
    /// Right shoulder semantic.
    RightShoulder,
    /// Right upper arm semantic.
    RightUpperArm,
    /// Right lower arm semantic.
    RightLowerArm,
    /// Right hand semantic.
    RightHand,
    /// Left upper leg semantic.
    LeftUpperLeg,
    /// Left lower leg semantic.
    LeftLowerLeg,
    /// Left foot semantic.
    LeftFoot,
    /// Left toe semantic.
    LeftToes,
    /// Right upper leg semantic.
    RightUpperLeg,
    /// Right lower leg semantic.
    RightLowerLeg,
    /// Right foot semantic.
    RightFoot,
    /// Right toe semantic.
    RightToes,
    /// Left thumb proximal semantic.
    LeftThumbProximal,
    /// Left index proximal semantic.
    LeftIndexProximal,
    /// Left middle proximal semantic.
    LeftMiddleProximal,
    /// Left ring proximal semantic.
    LeftRingProximal,
    /// Left little-finger proximal semantic.
    LeftLittleProximal,
    /// Right thumb proximal semantic.
    RightThumbProximal,
    /// Right index proximal semantic.
    RightIndexProximal,
    /// Right middle proximal semantic.
    RightMiddleProximal,
    /// Right ring proximal semantic.
    RightRingProximal,
    /// Right little-finger proximal semantic.
    RightLittleProximal,
    /// Left eye semantic.
    LeftEye,
    /// Right eye semantic.
    RightEye,
    /// Jaw semantic.
    Jaw,
}

impl HumanoidBone {
    /// Semantics required for a profile to be structurally usable for humanoid conversion.
    pub const REQUIRED: [Self; 15] = [
        Self::Hips,
        Self::Spine,
        Self::Head,
        Self::LeftUpperArm,
        Self::LeftLowerArm,
        Self::LeftHand,
        Self::RightUpperArm,
        Self::RightLowerArm,
        Self::RightHand,
        Self::LeftUpperLeg,
        Self::LeftLowerLeg,
        Self::LeftFoot,
        Self::RightUpperLeg,
        Self::RightLowerLeg,
        Self::RightFoot,
    ];
}

/// Whether a humanoid mapping was inferred by import or explicitly authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanoidProfileOrigin {
    /// Import-time convention/name/hierarchy detection produced the mapping.
    Automatic,
    /// An author explicitly confirmed or edited the mapping.
    Authored,
}

/// Model-owned mapping from portable humanoid semantics to one skeleton's stable bone IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanoidProfile {
    /// Stable `SkeletonAsset` sub-asset ID this profile belongs to.
    pub skeleton: String,
    /// Canonical skeleton identity the mapping was validated against.
    pub skeleton_identity: u64,
    /// Semantic-to-`BoneId` mapping. Values are stable numeric BoneId payloads.
    pub bones: BTreeMap<HumanoidBone, u32>,
    /// Optional bone whose translation carries locomotion; deliberately distinct from Hips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_root: Option<u32>,
    /// Semantics resolved with lower-confidence import heuristics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertain_bones: Vec<HumanoidBone>,
    /// Whether this mapping is import-generated or explicitly authored.
    pub origin: HumanoidProfileOrigin,
}

impl HumanoidProfile {
    /// Returns the persisted BoneId payload for `semantic`, when mapped.
    pub fn bone_id(&self, semantic: HumanoidBone) -> Option<u32> {
        self.bones.get(&semantic).copied()
    }
}

/// Persisted type of a deterministic imported sub-asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedSubAssetKind {
    /// Render mesh geometry.
    Mesh,
    /// Material document semantics.
    Material,
    /// Decoded texture pixels.
    Texture,
    /// Node hierarchy used as a skeleton definition.
    Skeleton,
    /// Skeleton skin and inverse-bind data.
    Skin,
    /// Skeletal animation clip.
    Animation,
    /// Named vertex or material deformation.
    Morph,
    /// Secondary-motion rigid-body rig.
    RigidBodyRig,
}

impl ImportedSubAssetKind {
    fn derivation_prefix(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Material => "material",
            Self::Texture => "texture",
            Self::Skeleton => "skeleton",
            Self::Skin => "skin",
            Self::Animation => "animation",
            Self::Morph => "morph",
            Self::RigidBodyRig => "rigidbodyrig",
        }
    }
}

/// Derives the canonical stable ID for one source selector and category.
pub fn imported_sub_asset_id(
    source: &AssetId,
    kind: ImportedSubAssetKind,
    index: usize,
) -> AssetId {
    AssetId::derive(source, &format!("{}:{index}", kind.derivation_prefix()))
}

/// Derives the stable ID for one model-specific motion clip.
pub fn imported_motion_sub_asset_id(
    motion_source: &AssetId,
    model_source: &AssetId,
    index: usize,
) -> AssetId {
    AssetId::derive(
        motion_source,
        &format!("animation:{}:{index}", model_source.as_str()),
    )
}

/// Stable metadata exposed to asset pickers after import or reimport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedSubAsset {
    /// Derived stable asset ID string.
    pub id: String,
    /// Imported data category.
    pub kind: ImportedSubAssetKind,
    /// Human-readable name from the source document.
    pub name: String,
    /// Deterministic zero-based source selector.
    pub index: u32,
    /// Model source whose rig a model-specific motion clip was baked against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_model_source: Option<String>,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

/// An entry in an [`AssetManifest`] mapping an [`AssetId`] to a project-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Relative path from the asset root to the file.
    pub path: String,
    /// Optional human-readable name for display.
    pub name: Option<String>,
    /// Import settings for this asset.
    pub import_settings: ImportSettings,
}

/// Reports why an asset manifest could not be loaded.
#[derive(Debug)]
pub enum AssetManifestError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The manifest declares an unsupported schema version.
    UnsupportedVersion {
        /// Version number found in the manifest.
        found: u64,
    },
    /// An asset ID string was invalid.
    InvalidAssetId {
        /// Raw string that failed to parse.
        id: String,
        /// Parse error.
        source: IdError,
    },
    /// A derived ID does not match its source, category, and selector.
    ImportedSubAssetIdMismatch {
        /// Registered source asset ID.
        source_id: AssetId,
        /// Persisted derived ID.
        actual: String,
        /// Canonical derived ID.
        expected: AssetId,
    },
}

impl fmt::Display for AssetManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(source) => write!(formatter, "manifest JSON error: {source}"),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "unsupported manifest version {found}; expected {ASSET_MANIFEST_SCHEMA_VERSION}"
            ),
            Self::InvalidAssetId { id, source } => {
                write!(formatter, "invalid asset ID {id:?} in manifest: {source}")
            }
            Self::ImportedSubAssetIdMismatch {
                source_id,
                actual,
                expected,
            } => write!(
                formatter,
                "imported sub-asset ID {actual:?} under `{}` must be `{}`",
                source_id.as_str(),
                expected.as_str()
            ),
        }
    }
}

impl std::error::Error for AssetManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::InvalidAssetId { source, .. } => Some(source),
            Self::UnsupportedVersion { .. } | Self::ImportedSubAssetIdMismatch { .. } => None,
        }
    }
}

/// Asset-manifest schema version read and written by this build.
pub const ASSET_MANIFEST_SCHEMA_VERSION: u64 = 2;

/// Maps stable asset IDs to project-relative source paths and import metadata.
#[derive(Debug, Clone)]
pub struct AssetManifest {
    entries: BTreeMap<AssetId, ManifestEntry>,
    revision: u64,
}

fn next_manifest_revision() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

impl Default for AssetManifest {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            revision: next_manifest_revision(),
        }
    }
}

impl AssetManifest {
    /// Parses an `asset_manifest.json` string.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed JSON, unsupported schema versions,
    /// invalid stable IDs, or inconsistent derived sub-asset IDs.
    pub fn from_json(json: &str) -> Result<Self, AssetManifestError> {
        #[derive(Deserialize)]
        struct RawEntry {
            path: String,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            import_settings: ImportSettings,
        }
        #[derive(Deserialize)]
        struct RawManifest {
            schema_version: u64,
            assets: BTreeMap<String, RawEntry>,
        }

        let raw: RawManifest = serde_json::from_str(json).map_err(AssetManifestError::Json)?;
        if raw.schema_version != ASSET_MANIFEST_SCHEMA_VERSION {
            return Err(AssetManifestError::UnsupportedVersion {
                found: raw.schema_version,
            });
        }

        let mut entries = BTreeMap::new();
        for (id_str, raw_entry) in raw.assets {
            let stable = engine_authoring::StableId::new(&id_str);
            let asset_id = AssetId::from_stable_id(stable)
                .map_err(|source| AssetManifestError::InvalidAssetId { id: id_str, source })?;
            for sub_asset in &raw_entry.import_settings.sub_assets {
                let expected = if let Some(target) = &sub_asset.target_model_source {
                    let target_id = AssetId::from_stable_id(engine_authoring::StableId::new(target))
                        .map_err(|source| AssetManifestError::InvalidAssetId {
                            id: target.clone(),
                            source,
                        })?;
                    imported_motion_sub_asset_id(&asset_id, &target_id, sub_asset.index as usize)
                } else {
                    imported_sub_asset_id(&asset_id, sub_asset.kind, sub_asset.index as usize)
                };
                if sub_asset.id != expected.as_str() {
                    return Err(AssetManifestError::ImportedSubAssetIdMismatch {
                        source_id: asset_id,
                        actual: sub_asset.id.clone(),
                        expected,
                    });
                }
            }
            entries.insert(
                asset_id,
                ManifestEntry {
                    path: raw_entry.path,
                    name: raw_entry.name,
                    import_settings: raw_entry.import_settings,
                },
            );
        }
        Ok(Self {
            entries,
            revision: next_manifest_revision(),
        })
    }

    /// Serializes this manifest as deterministic schema-version-2 JSON.
    ///
    /// # Errors
    ///
    /// Returns a JSON serialization error when the document cannot be encoded.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct RawEntry<'a> {
            path: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            name: Option<&'a str>,
            #[serde(skip_serializing_if = "ImportSettings::is_all_default")]
            import_settings: &'a ImportSettings,
        }
        #[derive(Serialize)]
        struct RawManifest<'a> {
            schema_version: u64,
            assets: BTreeMap<&'a str, RawEntry<'a>>,
        }
        let assets = self
            .entries
            .iter()
            .map(|(id, entry)| {
                (
                    id.as_str(),
                    RawEntry {
                        path: entry.path.as_str(),
                        name: entry.name.as_deref(),
                        import_settings: &entry.import_settings,
                    },
                )
            })
            .collect();
        serde_json::to_string_pretty(&RawManifest {
            schema_version: ASSET_MANIFEST_SCHEMA_VERSION,
            assets,
        })
    }

    /// Returns the manifest entry for `id`, if registered.
    pub fn get(&self, id: &AssetId) -> Option<&ManifestEntry> {
        self.entries.get(id)
    }

    /// Returns a mutable manifest entry and advances the process-local revision.
    pub fn get_mut(&mut self, id: &AssetId) -> Option<&mut ManifestEntry> {
        if self.entries.contains_key(id) {
            self.revision = next_manifest_revision();
        }
        self.entries.get_mut(id)
    }

    /// Resolves a deterministic imported sub-asset back to its source entry.
    pub fn imported_sub_asset(
        &self,
        id: &AssetId,
    ) -> Option<(&AssetId, &ManifestEntry, &ImportedSubAsset)> {
        self.entries.iter().find_map(|(source_id, entry)| {
            entry
                .import_settings
                .sub_assets
                .iter()
                .find(|sub_asset| sub_asset.id == id.as_str())
                .map(|sub_asset| (source_id, entry, sub_asset))
        })
    }

    /// Inserts or replaces a manifest entry.
    pub fn insert(&mut self, id: AssetId, entry: ManifestEntry) -> Option<ManifestEntry> {
        self.revision = next_manifest_revision();
        self.entries.insert(id, entry)
    }

    /// Removes one registered asset while preserving all other stable IDs.
    pub fn remove(&mut self, id: &AssetId) -> Option<ManifestEntry> {
        if self.entries.contains_key(id) {
            self.revision = next_manifest_revision();
        }
        self.entries.remove(id)
    }

    /// Iterates registered manifest entries in deterministic ID order.
    pub fn iter(&self) -> impl Iterator<Item = (&AssetId, &ManifestEntry)> {
        self.entries.iter()
    }

    /// Returns the number of registered assets.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no assets are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the process-local content generation for derived editor caches.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_handles_reference_their_inserted_values() {
        let mut assets = Assets::new();
        let first = assets.add(String::from("first"));
        let second = assets.add(String::from("second"));
        assert_ne!(first, second);
        assert_eq!(assets.get(&first).map(String::as_str), Some("first"));
        assert_eq!(assets.remove(&first), Some(String::from("first")));
        assert!(assets.get(&first).is_none());
    }

    #[test]
    fn manifest_roundtrip_preserves_import_settings() {
        let id = AssetId::generate();
        let mut manifest = AssetManifest::default();
        manifest.insert(
            id.clone(),
            ManifestEntry {
                path: "models/hero.glb".into(),
                name: Some("hero".into()),
                import_settings: ImportSettings::default(),
            },
        );
        let json = manifest.to_canonical_json().expect("manifest must serialize");
        let parsed = AssetManifest::from_json(&json).expect("manifest must parse");
        assert_eq!(parsed.get(&id), manifest.get(&id));
    }

    #[test]
    fn motion_sub_asset_identity_includes_target_model() {
        let motion = AssetId::generate();
        let first = AssetId::generate();
        let second = AssetId::generate();
        assert_ne!(
            imported_motion_sub_asset_id(&motion, &first, 0),
            imported_motion_sub_asset_id(&motion, &second, 0)
        );
    }

    #[test]
    fn targeted_motion_sub_asset_rejects_untargeted_id_alias() {
        let motion = AssetId::generate();
        let target = AssetId::generate();
        let old_id = imported_sub_asset_id(&motion, ImportedSubAssetKind::Animation, 0);
        let mut manifest = AssetManifest::default();
        manifest.insert(
            motion,
            ManifestEntry {
                path: "motions/walk.vmd".into(),
                name: None,
                import_settings: ImportSettings {
                    sub_assets: vec![ImportedSubAsset {
                        id: old_id.as_str().to_owned(),
                        kind: ImportedSubAssetKind::Animation,
                        name: "walk".into(),
                        index: 0,
                        target_model_source: Some(target.as_str().to_owned()),
                    }],
                    ..ImportSettings::default()
                },
            },
        );
        let json = manifest.to_canonical_json().expect("fixture must serialize");
        assert!(matches!(
            AssetManifest::from_json(&json),
            Err(AssetManifestError::ImportedSubAssetIdMismatch { .. })
        ));
    }
}
