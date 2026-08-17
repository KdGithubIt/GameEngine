//! Persistent glTF parse/decode cache shared across scene conversions
//! (ADR 0071).

use super::*;
use std::hash::Hasher;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// Identity stamp of one file on disk: modification time and byte length.
///
/// Cheap to compute (one `stat`, no file read), so cached entries can be
/// re-validated on every conversion without re-reading source bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

struct CachedGltfSource {
    /// Path the cached parse was produced from; a manifest entry that moved
    /// to a different file must not reuse the old parse.
    path: PathBuf,
    /// Import inputs that affect model construction without changing source bytes.
    settings_fingerprint: u64,
    /// Stamp of the source file plus every external sidecar at parse time.
    stamps: Vec<(PathBuf, Option<FileStamp>)>,
    imported: Arc<crate::model_import::GltfImportResult>,
}

fn import_settings_fingerprint(
    existing_skeletons: &[crate::asset::SkeletonRecord],
    contact_bone_names: &[String],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_u64(existing_skeletons.len() as u64);
    for record in existing_skeletons {
        hasher.write_u64(record.id.len() as u64);
        hasher.write(record.id.as_bytes());
        hasher.write_u64(record.identity);
        hasher.write_u32(record.next_bone_id);
        hasher.write_u64(record.bones.len() as u64);
        for bone in &record.bones {
            hasher.write_u32(bone.bone_id);
            hasher.write_u64(bone.name.len() as u64);
            hasher.write(bone.name.as_bytes());
        }
    }
    hasher.write_u64(contact_bone_names.len() as u64);
    for name in contact_bone_names {
        hasher.write_u64(name.len() as u64);
        hasher.write(name.as_bytes());
    }
    hasher.finish()
}

struct CachedGltfTexture {
    /// Pointer identity of the parsed document the texture was decoded from.
    ///
    /// The document `Arc` is kept alive by `sources`, and invalidating a
    /// source purges its textures, so this address cannot be reused by a
    /// different live document (no ABA hazard).
    document: usize,
    texture: Arc<DecodedTexture>,
}

#[derive(Default)]
struct SharedGltfImportCacheInner {
    sources: HashMap<AssetId, CachedGltfSource>,
    textures: HashMap<AssetId, CachedGltfTexture>,
}

/// Persistent, shareable cache of parsed glTF/GLB sources and decoded
/// textures (ADR 0071).
///
/// Scene conversion normally parses every referenced glTF/GLB source and
/// decodes its images from scratch because its internal caches live for
/// exactly one conversion. Hosts that convert
/// the same scene repeatedly (the editor Scene View rebuilds its preview
/// world every frame) can insert one `SharedGltfImportCache` as a world
/// resource before calling [`spawn_from_authoring_scene`] or
/// [`spawn_from_authoring_scene_best_effort`]; the bridge then reuses parsed
/// documents and decoded textures across conversions.
///
/// Entries are validated against the modification time and byte length of
/// the source file and each external `.bin`/image sidecar, so an edited or
/// replaced source is re-parsed automatically. Reusing the same
/// [`DecodedTexture`] allocation also lets renderer-side caches keyed by
/// pointer identity skip redundant GPU uploads.
///
/// Cloning is cheap and shares the same underlying cache.
#[derive(Clone, Default)]
pub struct SharedGltfImportCache {
    inner: Arc<Mutex<SharedGltfImportCacheInner>>,
}

impl fmt::Debug for SharedGltfImportCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        formatter
            .debug_struct("SharedGltfImportCache")
            .field("sources", &inner.sources.len())
            .field("textures", &inner.textures.len())
            .finish()
    }
}

impl SharedGltfImportCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, SharedGltfImportCacheInner> {
        // Every write inserts or removes whole entries, so a panic while the
        // lock is held cannot leave a partially updated entry behind and the
        // poisoned state can be safely ignored.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Returns the cached parse for `source_id` when it is still current.
    ///
    /// A stale entry (path changed, file edited, or sidecar edited) is
    /// removed together with every texture decoded from it, so a later
    /// lookup cannot serve pixels from an outdated document.
    #[doc(hidden)]
    pub fn lookup_source(
        &self,
        source_id: &AssetId,
        source_path: &Path,
        existing_skeletons: &[crate::asset::SkeletonRecord],
        contact_bone_names: &[String],
    ) -> Option<Arc<crate::model_import::GltfImportResult>> {
        let mut inner = self.lock();
        let cached = inner.sources.get(source_id)?;
        let valid = cached.path == source_path
            && cached.settings_fingerprint
                == import_settings_fingerprint(existing_skeletons, contact_bone_names)
            && cached
                .stamps
                .iter()
                .all(|(path, stamp)| file_stamp(path) == *stamp);
        if valid {
            return Some(Arc::clone(&cached.imported));
        }
        let document = Arc::as_ptr(&cached.imported) as usize;
        inner.sources.remove(source_id);
        inner
            .textures
            .retain(|_, texture| texture.document != document);
        None
    }

    /// Records a fresh parse of `source_path` for reuse by later conversions.
    pub(super) fn store_source(
        &self,
        source_id: &AssetId,
        source_path: &Path,
        existing_skeletons: &[crate::asset::SkeletonRecord],
        contact_bone_names: &[String],
        imported: &Arc<crate::model_import::GltfImportResult>,
    ) {
        let dependencies =
            crate::model_import::model_source_dependencies(source_path).unwrap_or_default();
        self.publish_prepared_source(
            source_id,
            source_path,
            &dependencies,
            existing_skeletons,
            contact_bone_names,
            Arc::clone(imported),
            Vec::new(),
        );
    }

    /// Atomically publishes one completed model generation and decoded textures.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn publish_prepared_source(
        &self,
        source_id: &AssetId,
        source_path: &Path,
        dependencies: &[PathBuf],
        existing_skeletons: &[crate::asset::SkeletonRecord],
        contact_bone_names: &[String],
        imported: Arc<crate::model_import::GltfImportResult>,
        textures: Vec<(AssetId, Arc<DecodedTexture>)>,
    ) {
        let mut stamped_paths = vec![source_path.to_path_buf()];
        stamped_paths.extend(dependencies.iter().cloned());
        let stamps = stamped_paths
            .into_iter()
            .map(|path| {
                let stamp = file_stamp(&path);
                (path, stamp)
            })
            .collect();
        let document = Arc::as_ptr(&imported) as usize;
        let mut inner = self.lock();
        if let Some(previous) = inner.sources.remove(source_id) {
            let previous_document = Arc::as_ptr(&previous.imported) as usize;
            inner
                .textures
                .retain(|_, texture| texture.document != previous_document);
        }
        inner.sources.insert(
            source_id.clone(),
            CachedGltfSource {
                path: source_path.to_path_buf(),
                settings_fingerprint: import_settings_fingerprint(
                    existing_skeletons,
                    contact_bone_names,
                ),
                stamps,
                imported,
            },
        );
        for (texture_id, texture) in textures {
            inner.textures.insert(
                texture_id,
                CachedGltfTexture { document, texture },
            );
        }
    }

    /// Drops every reusable parse/decode generation owned by this cache.
    #[doc(hidden)]
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.sources.clear();
        inner.textures.clear();
    }

    /// Returns the cached decode for `texture_id` when it was produced from
    /// exactly this parsed `document`.
    pub(super) fn lookup_texture(
        &self,
        texture_id: &AssetId,
        document: &Arc<crate::model_import::GltfImportResult>,
    ) -> Option<Arc<DecodedTexture>> {
        let inner = self.lock();
        let cached = inner.textures.get(texture_id)?;
        (cached.document == Arc::as_ptr(document) as usize).then(|| Arc::clone(&cached.texture))
    }

    /// Records a texture decoded from `document` for reuse by later
    /// conversions.
    pub(super) fn store_texture(
        &self,
        texture_id: &AssetId,
        document: &Arc<crate::model_import::GltfImportResult>,
        texture: &Arc<DecodedTexture>,
    ) {
        self.lock().textures.insert(
            texture_id.clone(),
            CachedGltfTexture {
                document: Arc::as_ptr(document) as usize,
                texture: Arc::clone(texture),
            },
        );
    }
}
