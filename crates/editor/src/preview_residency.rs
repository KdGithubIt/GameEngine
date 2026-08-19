//! Project-scoped immutable asset residency for Editor preview surfaces.

use engine::{AssetManifest, SharedGpuMeshCache, SourceStamp};
use engine_authoring::{AssetId, AuthoringScene, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_MATERIALIZATION_WORKERS: usize = 2;
const AGING_DISPATCH_INTERVAL: u64 = 4;

/// Scheduling class for preview CPU materialization.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PreviewAssetPriority {
    FocusedVisible,
    Visible,
    Prefetch,
    Background,
}

impl PreviewAssetPriority {
    const fn rank(self) -> u8 {
        match self {
            Self::FocusedVisible => 0,
            Self::Visible => 1,
            Self::Prefetch => 2,
            Self::Background => 3,
        }
    }
}

/// Readiness of all model sources required by one preview scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewResidencyState {
    Ready,
    Pending,
    Failed(String),
}

/// Deterministic counters for preview residency diagnostics and regression tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewResidencyStats {
    pub(crate) cache_hits: u64,
    pub(crate) materializations_started: u64,
    pub(crate) deduplicated_requests: u64,
    pub(crate) generations_published: u64,
    pub(crate) decoded_textures_published: u64,
    pub(crate) materialization_failures: u64,
    pub(crate) gpu_reclaims: u64,
    pub(crate) device_rebinds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceGeneration {
    source_path: PathBuf,
    source_stamp: Option<SourceStamp>,
    source_fingerprint: Option<String>,
    settings_fingerprint: u64,
}

#[derive(Debug, Clone)]
struct SourceRequest {
    source_id: AssetId,
    generation: SourceGeneration,
    skeleton_records: Vec<engine::SkeletonRecord>,
    contact_bones: Vec<String>,
}

#[derive(Debug)]
enum SourceState {
    Queued {
        request: SourceRequest,
        priority: PreviewAssetPriority,
        sequence: u64,
        queued_dispatch: u64,
    },
    Loading {
        generation: SourceGeneration,
        ticket: u64,
    },
    Ready {
        generation: SourceGeneration,
    },
    Failed {
        generation: SourceGeneration,
        message: String,
    },
}

#[derive(Debug)]
struct ResidencyInner {
    epoch: u64,
    revision: u64,
    next_ticket: u64,
    next_sequence: u64,
    dispatch_count: u64,
    active_workers: usize,
    gpu_device: Option<usize>,
    sources: BTreeMap<AssetId, SourceState>,
    stats: PreviewResidencyStats,
}

impl Default for ResidencyInner {
    fn default() -> Self {
        Self {
            epoch: 1,
            revision: 1,
            next_ticket: 1,
            next_sequence: 1,
            dispatch_count: 0,
            active_workers: 0,
            gpu_device: None,
            sources: BTreeMap::new(),
            stats: PreviewResidencyStats::default(),
        }
    }
}

/// CPU and device-local GPU residency shared by every preview surface in one project.
#[derive(Clone, Default)]
pub(crate) struct ProjectAssetResidency {
    model_cache: engine::scene_bridge::SharedGltfImportCache,
    gpu_mesh_cache: SharedGpuMeshCache,
    gpu_texture_cache: engine::material::SharedGpuTextureCache,
    inner: Arc<Mutex<ResidencyInner>>,
}

impl ProjectAssetResidency {
    fn lock(&self) -> std::sync::MutexGuard<'_, ResidencyInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn model_cache(&self) -> engine::scene_bridge::SharedGltfImportCache {
        self.model_cache.clone()
    }

    /// Schedules one exact model source for non-blocking Editor preview materialization.
    ///
    /// Animation Set Target Preview uses this narrow entry point to resolve an imported
    /// animation's real skin on multi-skeleton sources without parsing the model on the
    /// UI thread. A source whose persisted import generation is stale stays pending
    /// until the ordinary import worker refreshes it.
    pub(crate) fn prepare_model_source(
        &self,
        source_id: &AssetId,
        manifest: &AssetManifest,
        assets_root: Option<&Path>,
        priority: PreviewAssetPriority,
    ) -> PreviewResidencyState {
        let Some(assets_root) = assets_root else {
            return PreviewResidencyState::Ready;
        };
        let Some(entry) = manifest.get(source_id) else {
            return PreviewResidencyState::Failed(format!(
                "model source `{}` is not registered",
                source_id.as_str()
            ));
        };
        if !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, Path::new(&entry.path)) {
            return PreviewResidencyState::Failed(format!(
                "asset `{}` is not a model source",
                source_id.as_str()
            ));
        }

        let source_path = assets_root.join(&entry.path);
        let dependencies = entry
            .import_settings
            .source_dependencies
            .iter()
            .map(|path| assets_root.join(path))
            .collect::<Vec<_>>();
        let current_stamp = SourceStamp::capture(&source_path, &dependencies).ok();
        if entry.import_settings.source_fingerprint.is_none()
            || entry
                .import_settings
                .source_stamp
                .as_ref()
                .is_some_and(|expected| current_stamp.as_ref() != Some(expected))
        {
            return PreviewResidencyState::Pending;
        }

        let skeleton_records = entry.import_settings.skeleton_records.clone();
        let contact_bones = entry.import_settings.contact_bones.clone();
        let state = self.prepare_source(
            SourceRequest {
                source_id: source_id.clone(),
                generation: SourceGeneration {
                    source_path,
                    source_stamp: current_stamp,
                    source_fingerprint: entry.import_settings.source_fingerprint.clone(),
                    settings_fingerprint: settings_fingerprint(&skeleton_records, &contact_bones),
                },
                skeleton_records,
                contact_bones,
            },
            priority,
        );
        self.launch_available();
        state
    }

    /// Returns the current resident parse for one exact imported model generation.
    pub(crate) fn cached_model_source(
        &self,
        source_id: &AssetId,
        manifest: &AssetManifest,
        assets_root: Option<&Path>,
    ) -> Option<Arc<engine::GltfImportResult>> {
        let assets_root = assets_root?;
        let entry = manifest.get(source_id)?;
        let source_path = assets_root.join(&entry.path);
        self.model_cache.lookup_source(
            source_id,
            &source_path,
            &entry.import_settings.skeleton_records,
            &entry.import_settings.contact_bones,
        )
    }

    pub(crate) fn gpu_mesh_cache(&self) -> SharedGpuMeshCache {
        self.gpu_mesh_cache.clone()
    }

    pub(crate) fn gpu_texture_cache(&self) -> engine::material::SharedGpuTextureCache {
        self.gpu_texture_cache.clone()
    }

    pub(crate) fn revision(&self) -> u64 {
        self.lock().revision
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn stats(&self) -> PreviewResidencyStats {
        self.lock().stats
    }

    /// Associates GPU residency with one exact device identity.
    pub(crate) fn bind_gpu_device(&self, device_identity: usize) {
        let changed = {
            let mut inner = self.lock();
            if inner.gpu_device == Some(device_identity) {
                false
            } else {
                inner.gpu_device = Some(device_identity);
                inner.revision = inner.revision.saturating_add(1);
                inner.stats.device_rebinds = inner.stats.device_rebinds.saturating_add(1);
                true
            }
        };
        if changed {
            self.gpu_mesh_cache.clear();
            self.gpu_texture_cache.clear();
        }
    }

    /// Drops all residency owned by the project being closed/replaced.
    pub(crate) fn clear_project(&self) {
        {
            let mut inner = self.lock();
            inner.epoch = inner.epoch.saturating_add(1);
            inner.revision = inner.revision.saturating_add(1);
            inner.active_workers = 0;
            inner.gpu_device = None;
            inner.sources.clear();
        }
        self.model_cache.clear();
        self.gpu_mesh_cache.clear();
        self.gpu_texture_cache.clear();
    }

    /// Releases recreatable GPU residency while keeping CPU materialization resident.
    pub(crate) fn release_gpu(&self) {
        self.gpu_mesh_cache.clear();
        self.gpu_texture_cache.clear();
        let mut inner = self.lock();
        inner.revision = inner.revision.saturating_add(1);
        inner.stats.gpu_reclaims = inner.stats.gpu_reclaims.saturating_add(1);
    }

    /// Ensures every model generation referenced by `scene` is resident or in flight.
    pub(crate) fn prepare_scene(
        &self,
        scene: &AuthoringScene,
        manifest: &AssetManifest,
        assets_root: Option<&Path>,
        priority: PreviewAssetPriority,
    ) -> PreviewResidencyState {
        let Some(assets_root) = assets_root else {
            return PreviewResidencyState::Ready;
        };
        let (requests, awaiting_import) = source_requests(scene, manifest, assets_root);
        let mut pending = awaiting_import;
        for request in requests {
            match self.prepare_source(request, priority) {
                PreviewResidencyState::Ready => {}
                PreviewResidencyState::Pending => pending = true,
                PreviewResidencyState::Failed(message) => {
                    return PreviewResidencyState::Failed(message);
                }
            }
        }
        self.launch_available();
        if pending {
            PreviewResidencyState::Pending
        } else {
            PreviewResidencyState::Ready
        }
    }

    fn prepare_source(
        &self,
        request: SourceRequest,
        priority: PreviewAssetPriority,
    ) -> PreviewResidencyState {
        {
            let mut inner = self.lock();
            match inner.sources.get_mut(&request.source_id) {
                Some(SourceState::Queued {
                    request: queued,
                    priority: queued_priority,
                    ..
                }) if queued.generation == request.generation => {
                    if priority < *queued_priority {
                        *queued_priority = priority;
                    }
                    inner.stats.deduplicated_requests =
                        inner.stats.deduplicated_requests.saturating_add(1);
                    return PreviewResidencyState::Pending;
                }
                Some(SourceState::Loading { generation, .. })
                    if generation == &request.generation =>
                {
                    inner.stats.deduplicated_requests =
                        inner.stats.deduplicated_requests.saturating_add(1);
                    return PreviewResidencyState::Pending;
                }
                Some(SourceState::Failed {
                    generation,
                    message,
                    ..
                }) if generation == &request.generation => {
                    return PreviewResidencyState::Failed(message.clone());
                }
                _ => {}
            }
        }

        if self
            .model_cache
            .lookup_source(
                &request.source_id,
                &request.generation.source_path,
                &request.skeleton_records,
                &request.contact_bones,
            )
            .is_some()
        {
            let mut inner = self.lock();
            inner.stats.cache_hits = inner.stats.cache_hits.saturating_add(1);
            inner.sources.insert(
                request.source_id,
                SourceState::Ready {
                    generation: request.generation,
                },
            );
            return PreviewResidencyState::Ready;
        }

        let mut inner = self.lock();
        if matches!(
            inner.sources.get(&request.source_id),
            Some(SourceState::Ready { generation }) if generation == &request.generation
        ) {
            return PreviewResidencyState::Ready;
        }
        let sequence = inner.next_sequence;
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        let queued_dispatch = inner.dispatch_count;
        inner.sources.insert(
            request.source_id.clone(),
            SourceState::Queued {
                request,
                priority,
                sequence,
                queued_dispatch,
            },
        );
        PreviewResidencyState::Pending
    }

    fn launch_available(&self) {
        loop {
            let launch = {
                let mut inner = self.lock();
                if inner.active_workers >= MAX_MATERIALIZATION_WORKERS {
                    return;
                }
                let dispatch_count = inner.dispatch_count;
                let next_id = inner
                    .sources
                    .iter()
                    .filter_map(|(id, state)| match state {
                        SourceState::Queued {
                            priority,
                            sequence,
                            queued_dispatch,
                            ..
                        } => {
                            let waited = dispatch_count.saturating_sub(*queued_dispatch);
                            let promotions = (waited / AGING_DISPATCH_INTERVAL).min(3) as u8;
                            Some((
                                priority.rank().saturating_sub(promotions),
                                *sequence,
                                id.clone(),
                            ))
                        }
                        _ => None,
                    })
                    .min_by_key(|(effective, sequence, _)| (*effective, *sequence))
                    .map(|(_, _, id)| id);
                let Some(source_id) = next_id else {
                    return;
                };
                let state = inner.sources.remove(&source_id);
                let Some(SourceState::Queued { request, .. }) = state else {
                    continue;
                };
                let ticket = inner.next_ticket;
                inner.next_ticket = inner.next_ticket.saturating_add(1);
                inner.dispatch_count = inner.dispatch_count.saturating_add(1);
                inner.active_workers += 1;
                inner.stats.materializations_started =
                    inner.stats.materializations_started.saturating_add(1);
                let epoch = inner.epoch;
                inner.sources.insert(
                    source_id,
                    SourceState::Loading {
                        generation: request.generation.clone(),
                        ticket,
                    },
                );
                Some((epoch, ticket, request))
            };
            let Some((epoch, ticket, request)) = launch else {
                return;
            };
            let residency = self.clone();
            std::thread::Builder::new()
                .name("preview-asset-materialize".to_owned())
                .spawn(move || {
                    let result = materialize_source(&request);
                    residency.finish_materialization(epoch, ticket, request, result);
                    residency.launch_available();
                })
                .expect("preview materialization worker must be spawnable");
        }
    }

    fn finish_materialization(
        &self,
        epoch: u64,
        ticket: u64,
        request: SourceRequest,
        result: MaterializationResult,
    ) {
        let mut inner = self.lock();
        if inner.epoch != epoch {
            return;
        }
        inner.active_workers = inner.active_workers.saturating_sub(1);
        let still_current = matches!(
            inner.sources.get(&request.source_id),
            Some(SourceState::Loading { generation, ticket: current })
                if generation == &request.generation && *current == ticket
        );
        if !still_current {
            return;
        }
        match result {
            Ok((imported, textures, dependencies)) => {
                let texture_count = textures.len() as u64;
                self.model_cache.publish_prepared_source(
                    &request.source_id,
                    &request.generation.source_path,
                    &dependencies,
                    &request.skeleton_records,
                    &request.contact_bones,
                    imported,
                    textures,
                );
                inner.sources.insert(
                    request.source_id,
                    SourceState::Ready {
                        generation: request.generation,
                    },
                );
                inner.revision = inner.revision.saturating_add(1);
                inner.stats.generations_published =
                    inner.stats.generations_published.saturating_add(1);
                inner.stats.decoded_textures_published = inner
                    .stats
                    .decoded_textures_published
                    .saturating_add(texture_count);
            }
            Err(message) => {
                inner.sources.insert(
                    request.source_id,
                    SourceState::Failed {
                        generation: request.generation,
                        message,
                    },
                );
                inner.revision = inner.revision.saturating_add(1);
                inner.stats.materialization_failures =
                    inner.stats.materialization_failures.saturating_add(1);
            }
        }
    }

    /// Atomically promotes one successful background import into preview residency.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_import_result(
        &self,
        source_id: &AssetId,
        source_path: &Path,
        dependencies: &[PathBuf],
        source_fingerprint: Option<String>,
        skeleton_records: &[engine::SkeletonRecord],
        contact_bones: &[String],
        imported: Arc<engine::GltfImportResult>,
        textures: Vec<(AssetId, Arc<engine::DecodedTexture>)>,
    ) {
        let texture_count = textures.len() as u64;
        let generation = SourceGeneration {
            source_path: source_path.to_path_buf(),
            source_stamp: SourceStamp::capture(source_path, dependencies).ok(),
            source_fingerprint,
            settings_fingerprint: settings_fingerprint(skeleton_records, contact_bones),
        };
        let mut inner = self.lock();
        self.model_cache.publish_prepared_source(
            source_id,
            source_path,
            dependencies,
            skeleton_records,
            contact_bones,
            imported,
            textures,
        );
        inner
            .sources
            .insert(source_id.clone(), SourceState::Ready { generation });
        inner.revision = inner.revision.saturating_add(1);
        inner.stats.generations_published = inner.stats.generations_published.saturating_add(1);
        inner.stats.decoded_textures_published = inner
            .stats
            .decoded_textures_published
            .saturating_add(texture_count);
    }
}

fn settings_fingerprint(
    skeleton_records: &[engine::SkeletonRecord],
    contact_bones: &[String],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_u64(skeleton_records.len() as u64);
    for record in skeleton_records {
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
    hasher.write_u64(contact_bones.len() as u64);
    for name in contact_bones {
        hasher.write_u64(name.len() as u64);
        hasher.write(name.as_bytes());
    }
    hasher.finish()
}

fn source_requests(
    scene: &AuthoringScene,
    manifest: &AssetManifest,
    assets_root: &Path,
) -> (Vec<SourceRequest>, bool) {
    let mut referenced = BTreeSet::new();
    for (_, entity) in scene.entities() {
        for value in entity.components.values() {
            collect_asset_refs(value, &mut referenced);
        }
    }

    let mut sources = BTreeMap::<AssetId, engine::ManifestEntry>::new();
    for asset in referenced {
        if let Some((source_id, entry, _)) = manifest.imported_sub_asset(&asset) {
            if engine::asset_path_matches_kind(
                engine::AssetKind::GltfSource,
                Path::new(&entry.path),
            ) {
                sources
                    .entry(source_id.clone())
                    .or_insert_with(|| entry.clone());
            }
            continue;
        }
        if let Some(entry) = manifest.get(&asset)
            && engine::asset_path_matches_kind(
                engine::AssetKind::GltfSource,
                Path::new(&entry.path),
            )
        {
            sources.entry(asset).or_insert_with(|| entry.clone());
        }
    }

    let mut awaiting_import = false;
    let mut requests = Vec::new();
    for (source_id, entry) in sources {
        let source_path = assets_root.join(&entry.path);
        let dependencies = entry
            .import_settings
            .source_dependencies
            .iter()
            .map(|path| assets_root.join(path))
            .collect::<Vec<_>>();
        let current_stamp = SourceStamp::capture(&source_path, &dependencies).ok();
        if entry.import_settings.source_fingerprint.is_none()
            || entry
                .import_settings
                .source_stamp
                .as_ref()
                .is_some_and(|expected| current_stamp.as_ref() != Some(expected))
        {
            awaiting_import = true;
            continue;
        }
        let skeleton_records = entry.import_settings.skeleton_records;
        let contact_bones = entry.import_settings.contact_bones;
        requests.push(SourceRequest {
            source_id,
            generation: SourceGeneration {
                source_path,
                source_stamp: current_stamp,
                source_fingerprint: entry.import_settings.source_fingerprint,
                settings_fingerprint: settings_fingerprint(&skeleton_records, &contact_bones),
            },
            skeleton_records,
            contact_bones,
        });
    }
    (requests, awaiting_import)
}

fn collect_asset_refs(value: &Value, assets: &mut BTreeSet<AssetId>) {
    match value {
        Value::AssetRef(asset) => {
            assets.insert(asset.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_asset_refs(value, assets);
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                collect_asset_refs(value, assets);
            }
        }
        Value::Null
        | Value::Bool(_)
        | Value::I64(_)
        | Value::U64(_)
        | Value::F64(_)
        | Value::String(_)
        | Value::EntityRef(_) => {}
    }
}

type MaterializationResult = Result<
    (
        Arc<engine::GltfImportResult>,
        Vec<(AssetId, Arc<engine::DecodedTexture>)>,
        Vec<PathBuf>,
    ),
    String,
>;

fn materialize_source(request: &SourceRequest) -> MaterializationResult {
    let dependencies_before = engine::model_source_dependencies(&request.generation.source_path)
        .map_err(|error| error.to_string())?;
    let stamp_before = SourceStamp::capture(&request.generation.source_path, &dependencies_before)
        .map_err(|error| error.to_string())?;
    if request
        .generation
        .source_stamp
        .as_ref()
        .is_some_and(|expected| expected != &stamp_before)
    {
        return Err("model source changed after its last successful import".into());
    }
    let fingerprint_before =
        engine::fingerprint_model_source(&request.generation.source_path, &dependencies_before)
            .map_err(|error| error.to_string())?;
    if request
        .generation
        .source_fingerprint
        .as_ref()
        .is_some_and(|expected| expected != &fingerprint_before)
    {
        return Err("model source content no longer matches its imported generation".into());
    }

    let imported = engine::import_model_path_with_contact_bones(
        &request.source_id,
        &request.generation.source_path,
        &request.skeleton_records,
        &request.contact_bones,
    )
    .map_err(|error| error.to_string())?;
    let dependencies_after = engine::model_source_dependencies(&request.generation.source_path)
        .map_err(|error| error.to_string())?;
    let stamp_after = SourceStamp::capture(&request.generation.source_path, &dependencies_after)
        .map_err(|error| error.to_string())?;
    let fingerprint_after =
        engine::fingerprint_model_source(&request.generation.source_path, &dependencies_after)
            .map_err(|error| error.to_string())?;
    if dependencies_before != dependencies_after
        || stamp_before != stamp_after
        || fingerprint_before != fingerprint_after
    {
        return Err("model source changed while preview materialization was running".into());
    }

    let imported = Arc::new(imported);
    let textures = imported
        .textures
        .iter()
        .map(|texture| {
            (
                texture.id.clone(),
                Arc::new(engine::DecodedTexture {
                    label: format!(
                        "{} / {}",
                        request.generation.source_path.display(),
                        texture.name
                    ),
                    width: texture.width,
                    height: texture.height,
                    rgba8: texture.rgba8.clone(),
                }),
            )
        })
        .collect();
    Ok((imported, textures, dependencies_after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_asset_references_are_collected_once() {
        let first = AssetId::generate();
        let second = AssetId::generate();
        let value = Value::Array(vec![
            Value::AssetRef(first.clone()),
            Value::Object(std::collections::BTreeMap::from([
                ("first".into(), Value::AssetRef(first.clone())),
                ("second".into(), Value::AssetRef(second.clone())),
            ])),
        ]);
        let mut assets = BTreeSet::new();
        collect_asset_refs(&value, &mut assets);
        assert_eq!(assets, BTreeSet::from([first, second]));
    }

    #[test]
    fn same_generation_requests_are_deduplicated_before_worker_launch() {
        let residency = ProjectAssetResidency::default();
        let request = SourceRequest {
            source_id: AssetId::generate(),
            generation: SourceGeneration {
                source_path: PathBuf::from("missing.glb"),
                source_stamp: None,
                source_fingerprint: None,
                settings_fingerprint: 0,
            },
            skeleton_records: Vec::new(),
            contact_bones: Vec::new(),
        };

        assert_eq!(
            residency.prepare_source(request.clone(), PreviewAssetPriority::FocusedVisible),
            PreviewResidencyState::Pending
        );
        assert_eq!(
            residency.prepare_source(request, PreviewAssetPriority::Visible),
            PreviewResidencyState::Pending
        );
        let stats = residency.stats();
        assert_eq!(stats.deduplicated_requests, 1);
        assert_eq!(stats.materializations_started, 0);
    }

    #[test]
    fn priority_aging_eventually_promotes_background_work() {
        assert_eq!(PreviewAssetPriority::FocusedVisible.rank(), 0);
        assert_eq!(PreviewAssetPriority::Visible.rank(), 1);
        assert_eq!(
            PreviewAssetPriority::Background
                .rank()
                .saturating_sub((12 / AGING_DISPATCH_INTERVAL) as u8),
            0
        );
    }

    #[test]
    fn project_clear_and_device_rebind_advance_revision() {
        let residency = ProjectAssetResidency::default();
        let initial = residency.revision();
        residency.bind_gpu_device(7);
        assert!(residency.revision() > initial);
        let rebound = residency.revision();
        residency.bind_gpu_device(7);
        assert_eq!(residency.revision(), rebound);
        residency.release_gpu();
        assert!(residency.revision() > rebound);
        assert_eq!(residency.stats().gpu_reclaims, 1);
        let reclaimed = residency.revision();
        residency.clear_project();
        assert!(residency.revision() > reclaimed);
    }
}
