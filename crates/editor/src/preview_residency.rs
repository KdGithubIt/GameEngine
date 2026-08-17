//! Project-scoped immutable asset residency for Editor preview surfaces.

use engine::{AssetManifest, ImportSettings, SharedGpuMeshCache, SourceStamp};
use engine_authoring::{AssetId, AuthoringScene, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Readiness of all model sources required by one preview scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewResidencyState {
    /// Every referenced model generation is resident and conversion-ready.
    Ready,
    /// At least one required generation is being materialized off-thread.
    Pending,
    /// A required generation failed to materialize.
    Failed(String),
}

/// Deterministic counters used by preview-residency regression tests and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewResidencyStats {
    pub(crate) cache_hits: u64,
    pub(crate) materializations_started: u64,
    pub(crate) deduplicated_requests: u64,
    pub(crate) generations_published: u64,
    pub(crate) materialization_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceGeneration {
    source_path: PathBuf,
    source_stamp: Option<SourceStamp>,
    import_settings: ImportSettings,
}

#[derive(Debug, Clone)]
struct SourceRequest {
    source_id: AssetId,
    generation: SourceGeneration,
    dependencies: Vec<PathBuf>,
}

#[derive(Debug)]
enum SourceState {
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
    sources: BTreeMap<AssetId, SourceState>,
    stats: PreviewResidencyStats,
}

impl Default for ResidencyInner {
    fn default() -> Self {
        Self {
            epoch: 1,
            revision: 1,
            next_ticket: 1,
            sources: BTreeMap::new(),
            stats: PreviewResidencyStats::default(),
        }
    }
}

/// CPU and GPU residency shared by every preview surface in one Editor project.
#[derive(Clone, Default)]
pub(crate) struct ProjectAssetResidency {
    model_cache: engine::scene_bridge::SharedGltfImportCache,
    gpu_mesh_cache: SharedGpuMeshCache,
    inner: Arc<Mutex<ResidencyInner>>,
}

impl ProjectAssetResidency {
    fn lock(&self) -> std::sync::MutexGuard<'_, ResidencyInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Returns the model cache injected into each preview conversion world.
    pub(crate) fn model_cache(&self) -> engine::scene_bridge::SharedGltfImportCache {
        self.model_cache.clone()
    }

    /// Returns the device-resident mesh cache shared by all preview worlds.
    pub(crate) fn gpu_mesh_cache(&self) -> SharedGpuMeshCache {
        self.gpu_mesh_cache.clone()
    }

    /// Returns a revision that changes whenever preview-ready residency changes.
    pub(crate) fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// Returns current deterministic materialization counters.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn stats(&self) -> PreviewResidencyStats {
        self.lock().stats
    }

    /// Drops all CPU/GPU generations owned by the current project.
    pub(crate) fn clear_project(&self) {
        {
            let mut inner = self.lock();
            inner.epoch = inner.epoch.saturating_add(1);
            inner.revision = inner.revision.saturating_add(1);
            inner.sources.clear();
        }
        self.model_cache.clear();
        self.gpu_mesh_cache.clear();
    }

    /// Ensures every model source referenced by `scene` is materializing or ready.
    ///
    /// This method performs only bounded metadata checks on the caller thread. Source
    /// reads, parser work, image decoding, and model construction run on workers.
    pub(crate) fn prepare_scene(
        &self,
        scene: &AuthoringScene,
        manifest: &AssetManifest,
        assets_root: Option<&Path>,
    ) -> PreviewResidencyState {
        let Some(assets_root) = assets_root else {
            return PreviewResidencyState::Ready;
        };
        let requests = source_requests(scene, manifest, assets_root);
        let mut pending = false;
        for request in requests {
            match self.prepare_source(request) {
                PreviewResidencyState::Ready => {}
                PreviewResidencyState::Pending => pending = true,
                PreviewResidencyState::Failed(message) => {
                    return PreviewResidencyState::Failed(message);
                }
            }
        }
        if pending {
            PreviewResidencyState::Pending
        } else {
            PreviewResidencyState::Ready
        }
    }

    fn prepare_source(&self, request: SourceRequest) -> PreviewResidencyState {
        if self
            .model_cache
            .lookup_source(
                &request.source_id,
                &request.generation.source_path,
                &request.generation.import_settings.skeleton_records,
                &request.generation.import_settings.contact_bones,
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

        let (epoch, ticket) = {
            let mut inner = self.lock();
            match inner.sources.get(&request.source_id) {
                Some(SourceState::Loading { generation, .. })
                    if generation == &request.generation =>
                {
                    inner.stats.deduplicated_requests =
                        inner.stats.deduplicated_requests.saturating_add(1);
                    return PreviewResidencyState::Pending;
                }
                Some(SourceState::Failed {
                    generation, message, ..
                }) if generation == &request.generation => {
                    return PreviewResidencyState::Failed(message.clone());
                }
                Some(SourceState::Ready { generation }) if generation == &request.generation => {
                    // The lower cache rejected the supposedly ready entry, normally because
                    // a dependency changed after the generation metadata was captured.
                }
                _ => {}
            }
            let ticket = inner.next_ticket;
            inner.next_ticket = inner.next_ticket.saturating_add(1);
            let epoch = inner.epoch;
            inner.stats.materializations_started =
                inner.stats.materializations_started.saturating_add(1);
            inner.sources.insert(
                request.source_id.clone(),
                SourceState::Loading {
                    generation: request.generation.clone(),
                    ticket,
                },
            );
            (epoch, ticket)
        };

        let model_cache = self.model_cache.clone();
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let result = materialize_source(&request);
            let mut state = inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let still_current = state.epoch == epoch
                && matches!(
                    state.sources.get(&request.source_id),
                    Some(SourceState::Loading { generation, ticket: current })
                        if generation == &request.generation && *current == ticket
                );
            if !still_current {
                return;
            }
            match result {
                Ok((imported, textures, dependencies)) => {
                    model_cache.publish_prepared_source(
                        &request.source_id,
                        &request.generation.source_path,
                        &dependencies,
                        &request.generation.import_settings.skeleton_records,
                        &request.generation.import_settings.contact_bones,
                        imported,
                        textures,
                    );
                    state.sources.insert(
                        request.source_id,
                        SourceState::Ready {
                            generation: request.generation,
                        },
                    );
                    state.revision = state.revision.saturating_add(1);
                    state.stats.generations_published =
                        state.stats.generations_published.saturating_add(1);
                }
                Err(message) => {
                    state.sources.insert(
                        request.source_id,
                        SourceState::Failed {
                            generation: request.generation,
                            message,
                        },
                    );
                    state.revision = state.revision.saturating_add(1);
                    state.stats.materialization_failures =
                        state.stats.materialization_failures.saturating_add(1);
                }
            }
        });
        PreviewResidencyState::Pending
    }

    /// Publishes a successful mutation-bearing background import for preview reuse.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_import_result(
        &self,
        source_id: &AssetId,
        source_path: &Path,
        dependencies: &[PathBuf],
        import_settings: &ImportSettings,
        imported: Arc<engine::GltfImportResult>,
        textures: Vec<(AssetId, Arc<engine::DecodedTexture>)>,
    ) {
        self.model_cache.publish_prepared_source(
            source_id,
            source_path,
            dependencies,
            &import_settings.skeleton_records,
            &import_settings.contact_bones,
            imported,
            textures,
        );
        let generation = SourceGeneration {
            source_path: source_path.to_path_buf(),
            source_stamp: SourceStamp::capture(source_path, dependencies).ok(),
            import_settings: import_settings.clone(),
        };
        let mut inner = self.lock();
        inner.sources.insert(
            source_id.clone(),
            SourceState::Ready { generation },
        );
        inner.revision = inner.revision.saturating_add(1);
        inner.stats.generations_published =
            inner.stats.generations_published.saturating_add(1);
    }
}

fn source_requests(
    scene: &AuthoringScene,
    manifest: &AssetManifest,
    assets_root: &Path,
) -> Vec<SourceRequest> {
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

    sources
        .into_iter()
        .map(|(source_id, entry)| {
            let source_path = assets_root.join(&entry.path);
            let dependencies = entry
                .import_settings
                .source_dependencies
                .iter()
                .map(|path| assets_root.join(path))
                .collect::<Vec<_>>();
            let generation = SourceGeneration {
                source_stamp: SourceStamp::capture(&source_path, &dependencies).ok(),
                source_path,
                import_settings: entry.import_settings,
            };
            SourceRequest {
                source_id,
                generation,
                dependencies,
            }
        })
        .collect()
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
    if let Some(expected_stamp) = request.generation.source_stamp.as_ref() {
        let current_stamp = SourceStamp::capture(
            &request.generation.source_path,
            &request.dependencies,
        )
        .map_err(|error| error.to_string())?;
        if &current_stamp != expected_stamp {
            return Err("model source changed before preview materialization started".into());
        }
    }
    let dependencies_before = engine::model_source_dependencies(&request.generation.source_path)
        .map_err(|error| error.to_string())?;
    let stamp_before = SourceStamp::capture(
        &request.generation.source_path,
        &dependencies_before,
    )
    .map_err(|error| error.to_string())?;
    let imported = engine::import_model_path_with_contact_bones(
        &request.source_id,
        &request.generation.source_path,
        &request.generation.import_settings.skeleton_records,
        &request.generation.import_settings.contact_bones,
    )
    .map_err(|error| error.to_string())?;
    let dependencies_after = engine::model_source_dependencies(&request.generation.source_path)
        .map_err(|error| error.to_string())?;
    let stamp_after = SourceStamp::capture(
        &request.generation.source_path,
        &dependencies_after,
    )
    .map_err(|error| error.to_string())?;
    if dependencies_before != dependencies_after || stamp_before != stamp_after {
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
    fn project_clear_invalidates_shared_residency_revision() {
        let residency = ProjectAssetResidency::default();
        let before = residency.revision();
        residency.clear_project();
        assert!(residency.revision() > before);
    }
}
