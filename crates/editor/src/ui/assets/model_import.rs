//! Model and external asset import pipeline driven from the asset browser.
//!
//! Registration, queued imports, reimports, and the handling of each import
//! result, including the motions that must be re-baked when the model they
//! are paired with is imported again.

use crate::ui::*;
use super::instantiate::write_generated_prefab;
use super::manifest::{normalize_manifest_path, save_asset_manifest};

impl EditorApp {
    /// Registers a model source if needed and queues it for import.
    ///
    /// This is the single entry point every arrival path funnels through
    /// (ADR 0075), so a file copied in with the file manager, dropped on the
    /// editor, or pulled in by version control all end up importable without
    /// the author performing a separate step.
    pub(in crate::ui) fn auto_import_model_source(&mut self, relative_path: &Path) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let is_motion =
            engine::asset_path_matches_kind(engine::AssetKind::MotionSource, relative_path);
        if !is_motion
            && !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, relative_path)
        {
            return;
        }
        let source_path = project.assets_root().join(relative_path);
        if !source_path.is_file() {
            return;
        }
        if is_motion {
            match engine::classify_vmd_path(&source_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        format!(
                            "`{}` contains camera, light, or self-shadow motion and was not registered as a model MotionSource",
                            relative_path.display()
                        ),
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        format!("`{}` contains no animation keys", relative_path.display()),
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect `{}`: {error}", relative_path.display()),
                    ));
                    return;
                }
            }
        }
        let Some(relative_string) = asset_relative_path_string(relative_path) else {
            return;
        };
        let existing = self
            .asset_manifest
            .iter()
            .find(|(_, entry)| {
                normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative_string)
            })
            .map(|(id, _)| id.clone());

        let asset_id = match existing {
            Some(id) => id,
            None => {
                let id = engine_authoring::id::AssetId::generate();
                let name = unique_asset_name(
                    relative_path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(if is_motion { "motion" } else { "model" }),
                    &self.asset_manifest,
                );
                let mut import_settings = engine::ImportSettings::default();
                if is_motion {
                    // A motion cannot be imported without a paired PMX (ADR
                    // 0097 §3). Picking it automatically when the project
                    // holds exactly one keeps the common single-character
                    // case a drag-and-drop away; anything ambiguous is left
                    // unpaired for the author to choose in Import Settings,
                    // since a wrong guess bakes a plausible-looking but wrong
                    // clip.
                    import_settings.motion_model_sources = sole_pmx_model_source(&self.asset_manifest)
                        .map(|id| vec![id.as_str().to_owned()])
                        .unwrap_or_default();
                }
                let mut manifest = self.asset_manifest.clone();
                manifest.insert(
                    id.clone(),
                    engine::ManifestEntry {
                        path: relative_string.clone(),
                        name: Some(name),
                        import_settings,
                    },
                );
                if let Err(error) = save_asset_manifest(&project, &manifest) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "asset.manifest_save_failed",
                            error,
                        ));
                    return;
                }
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }
                self.asset_manifest = manifest;
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "asset.model_auto_registered",
                        format!("registered model `{relative_string}` as `{}`", id.as_str()),
                    ));
                id
            }
        };
        self.queue_model_import(asset_id, source_path);
    }

    /// Adds one source to the import queue, ignoring duplicates.
    pub(in crate::ui) fn queue_model_import(
        &mut self,
        asset_id: engine_authoring::AssetId,
        source_path: PathBuf,
    ) {
        // Deduplicate against the running job as well as the queue: a burst
        // of writes reports the same source many times, and re-importing what
        // is already in flight would repeat the whole parse for nothing.
        // `Reimport` stays available for a change that lands mid-import.
        if self.asset_import.active_source() == Some(&asset_id)
            || self
                .pending_model_imports
                .iter()
                .any(|(queued, _)| queued == &asset_id)
        {
            return;
        }
        self.pending_model_imports
            .push_back((asset_id, source_path));
        self.start_next_model_import();
    }

    /// Starts the next queued import when the single worker is free.
    pub(in crate::ui) fn start_next_model_import(&mut self) {
        if self.asset_import.is_running() {
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.pending_model_imports.clear();
            return;
        };
        let Some((asset_id, source_path)) = self.pending_model_imports.pop_front() else {
            return;
        };
        // Every registered source's ledger is offered so the dedupe rule
        // (ADR 0077 §4) can recognize a rig imported from a different
        // source, not just this source's own prior import.
        let existing_skeletons: Vec<engine::SkeletonRecord> = self
            .asset_manifest
            .iter()
            .flat_map(|(_, entry)| entry.import_settings.skeleton_records.iter().cloned())
            .collect();
        // This source's own contact-bone override (ADR 0080 §1, AP-5); empty
        // keeps the default foot/ankle/toe name heuristic.
        let contact_bones = self
            .asset_manifest
            .get(&asset_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();
        let started = if engine::asset_path_matches_kind(
            engine::AssetKind::MotionSource,
            &source_path,
        ) {
            match engine::classify_vmd_path(&source_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        format!(
                            "`{}` is scene-level VMD motion and was not paired with a PMX model",
                            source_path.display()
                        ),
                    ));
                    self.start_next_model_import();
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        format!("`{}` contains no animation keys", source_path.display()),
                    ));
                    self.start_next_model_import();
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect `{}`: {error}", source_path.display()),
                    ));
                    self.start_next_model_import();
                    return;
                }
            }
            let targets = self.paired_motion_models(&asset_id);
            if targets.is_empty() {
                // Not an error the author can act on from a background queue,
                // so it is reported once here and again by scene validation
                // (`scene.motion_source_unpaired`) if the motion is used.
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "asset.motion_source_unpaired",
                        format!(
                            "`{}` is not paired with a PMX model yet; choose one in Import Settings to bake its clips",
                            source_path.display()
                        ),
                    ));
                self.start_next_model_import();
                return;
            }
            let original_model_is_configured = self
                .asset_manifest
                .get(&asset_id)
                .is_some_and(|entry| {
                    entry
                        .import_settings
                        .motion_original_model_source
                        .is_some()
                });
            let original_model = self.original_motion_model(&asset_id).map(
                |(model_source_id, model_relative)| crate::asset_import::MotionImportTarget {
                    contact_bones: self
                        .asset_manifest
                        .get(&model_source_id)
                        .map(|entry| entry.import_settings.contact_bones.clone())
                        .unwrap_or_default(),
                    model_source_id,
                    model_path: project.assets_root().join(model_relative),
                },
            );
            if original_model_is_configured && original_model.is_none() {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.motion_original_model_unregistered",
                        format!(
                            "`{}` selects an original PMX that is no longer registered; choose another original model or select Direct bake in Import Settings",
                            source_path.display()
                        ),
                    ));
                self.start_next_model_import();
                return;
            }
            let retarget_maps = engine::load_registered_retarget_maps(
                &project.assets_root(),
                &self.asset_manifest,
            );
            self.asset_import
                .start_vmd(crate::asset_import::MotionImportJob {
                    project_path: project.path().to_path_buf(),
                    source_id: asset_id,
                    source_path: source_path.clone(),
                    targets: targets
                        .into_iter()
                        .map(|(model_source_id, model_relative)| {
                            let contact_bones = self
                                .asset_manifest
                                .get(&model_source_id)
                                .map(|entry| entry.import_settings.contact_bones.clone())
                                .unwrap_or_default();
                            crate::asset_import::MotionImportTarget {
                                model_source_id,
                                model_path: project.assets_root().join(model_relative),
                                contact_bones,
                            }
                        })
                        .collect(),
                    original_model,
                    retarget_maps,
                    existing_skeletons,
                    contact_bones,
                })
        } else {
            let existing_humanoid_profiles = self
                .asset_manifest
                .get(&asset_id)
                .map(|entry| entry.import_settings.humanoid_profiles.clone())
                .unwrap_or_default();
            self.asset_import.start_gltf(
                project.path().to_path_buf(),
                asset_id,
                source_path.clone(),
                existing_skeletons,
                existing_humanoid_profiles,
                contact_bones,
            )
        };
        if let Err(error) = started {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.gltf_import_start_failed",
                    format!(
                        "failed to start import of `{}`: {error}",
                        source_path.display()
                    ),
                ));
        }
    }

    /// Resolves a motion source's selected PMX models to stable IDs and
    /// project-relative paths, dropping malformed or dangling targets.
    fn paired_motion_models(
        &self,
        motion_id: &engine_authoring::AssetId,
    ) -> Vec<(engine_authoring::AssetId, String)> {
        let Some(entry) = self.asset_manifest.get(motion_id) else {
            return Vec::new();
        };
        let mut resolved = Vec::new();
        for paired in entry.import_settings.resolved_motion_model_sources() {
            let Ok(model_id) = engine_authoring::AssetId::from_stable_id(
                engine_authoring::StableId::new(paired),
            ) else {
                continue;
            };
            let Some(model_entry) = self.asset_manifest.get(&model_id) else {
                continue;
            };
            if !resolved.iter().any(|(existing, _)| existing == &model_id) {
                resolved.push((model_id, model_entry.path.clone()));
            }
        }
        resolved
    }

    /// Resolves the optional original PMX selected for a motion source.
    ///
    /// A missing, malformed, or dangling ID resolves to `None`; manifest and
    /// scene validation report those authoring errors separately, while the
    /// background queue remains robust against a concurrently edited asset.
    fn original_motion_model(
        &self,
        motion_id: &engine_authoring::AssetId,
    ) -> Option<(engine_authoring::AssetId, String)> {
        let original = self
            .asset_manifest
            .get(motion_id)?
            .import_settings
            .motion_original_model_source
            .as_deref()?;
        let model_id = engine_authoring::AssetId::from_stable_id(
            engine_authoring::StableId::new(original),
        )
        .ok()?;
        let entry = self.asset_manifest.get(&model_id)?;
        Some((model_id, entry.path.clone()))
    }

    /// Queues every registered or unregistered model in the project.
    ///
    /// The watcher only reports changes after its first snapshot, so opening
    /// a project would otherwise leave models that were added while the
    /// editor was closed without an import.
    pub(in crate::ui) fn import_models_missing_catalogs(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let assets_root = project.assets_root();
        let already_imported = |entry: &engine::ManifestEntry| {
            // A motion source draws nothing, so it never generates a
            // placement prefab; requiring one would requeue every `.vmd` on
            // every project open.
            let needs_prefab =
                !engine::asset_path_matches_kind(engine::AssetKind::MotionSource, Path::new(&entry.path));
            entry.import_settings.source_fingerprint.is_some()
                && (!needs_prefab
                    || entry
                        .import_settings
                        .generated_prefab
                        .as_ref()
                        .is_some_and(|relative| project.path().join(relative).is_file()))
        };
        let pending: Vec<(engine_authoring::AssetId, PathBuf)> = self
            .asset_manifest
            .iter()
            .filter(|(_, entry)| {
                is_importable_source_path(Path::new(&entry.path))
                    && assets_root.join(&entry.path).is_file()
                    && !already_imported(entry)
            })
            .map(|(id, entry)| (id.clone(), assets_root.join(&entry.path)))
            .collect();
        for (asset_id, source_path) in pending {
            self.queue_model_import(asset_id, source_path);
        }

        let unregistered: Vec<PathBuf> = self
            .asset_browser
            .entries()
            .iter()
            .filter(|entry| is_importable_source_path(&entry.relative_path))
            .map(|entry| entry.relative_path.clone())
            .filter(|relative| {
                asset_relative_path_string(relative).is_some_and(|relative| {
                    !self.asset_manifest.iter().any(|(_, entry)| {
                        normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative)
                    })
                })
            })
            .collect();
        for relative in unregistered {
            self.auto_import_model_source(&relative);
        }
    }

    pub(in crate::ui) fn register_asset_from_browser(&mut self, index: usize) {
        let Some(project_root) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.register_without_project",
                    "open a project before registering assets",
                ));
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(index).cloned() else {
            return;
        };
        if !is_registerable_asset(&entry.relative_path) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.unsupported_registration",
                    "this file type is not a supported runtime asset",
                ));
            return;
        }
        let motion_source =
            engine::asset_path_matches_kind(engine::AssetKind::MotionSource, &entry.relative_path);
        if motion_source {
            let motion_path = project_root.assets_root().join(&entry.relative_path);
            match engine::classify_vmd_path(&motion_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        "this VMD contains scene-level camera, light, or self-shadow motion and cannot be registered as a model MotionSource",
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        "this VMD contains no animation keys",
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect VMD: {error}"),
                    ));
                    return;
                }
            }
        }
        // Both kinds queue a background catalog job after registration; only
        // the importer they dispatch to differs.
        let import_source = motion_source || is_importable_source_path(&entry.relative_path);
        let Some(relative_path) = asset_relative_path_string(&entry.relative_path) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.register_failed",
                    "asset path contains non-UTF-8 characters",
                ));
            return;
        };
        if let Err(error) = project_root.resolve_asset(&relative_path) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.register_failed",
                    format!("asset path `{relative_path}` is not readable: {error}"),
                ));
            return;
        }

        if let Some((id, _)) = self.asset_manifest.iter().find(|(_, manifest_entry)| {
            normalize_manifest_path(&manifest_entry.path) == normalize_manifest_path(&relative_path)
        }) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.already_registered",
                    format!(
                        "asset `{relative_path}` is already registered as `{}`",
                        id.as_str()
                    ),
                ));
            return;
        }

        let asset_id = engine_authoring::id::AssetId::generate();
        let name = unique_asset_name(&entry.display_name, &self.asset_manifest);
        let mut import_settings = engine::ImportSettings::default();
        if motion_source {
            // See `auto_import_model_source` for why an ambiguous project is
            // left unpaired rather than guessed at.
            import_settings.motion_model_sources = sole_pmx_model_source(&self.asset_manifest)
                .map(|id| vec![id.as_str().to_owned()])
                .unwrap_or_default();
        }
        let mut manifest = self.asset_manifest.clone();
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative_path.clone(),
                name: Some(name.clone()),
                import_settings,
            },
        );
        if let Err(error) = save_asset_manifest(&project_root, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error.clone(),
                ));
            self.notify_asset_error(format!("Asset registration failed: {error}"));
            return;
        }

        self.asset_manifest = manifest;
        self.notify_registered_assets(std::slice::from_ref(&entry.relative_path));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.registered",
                format!(
                    "registered `{relative_path}` as `{}` ({name}){}",
                    asset_id.as_str(),
                    if import_source {
                        "; background import queued"
                    } else {
                        ""
                    }
                ),
            ));
        if import_source {
            let source_path = project_root.assets_root().join(&relative_path);
            self.queue_model_import(asset_id, source_path);
        }
    }

    pub(in crate::ui) fn import_external_asset_files(&mut self, sources: Vec<PathBuf>) {
        let Some(project) = self.project_root.clone() else {
            self.notify_asset_error("Asset import failed: no project is open");
            return;
        };
        let destination_folder = self.asset_browser.selected_folder().to_path_buf();
        match crate::asset_management::import_external_asset_files(
            &project,
            &mut self.asset_manifest,
            &sources,
            &destination_folder,
        ) {
            Ok(report) => {
                if !report.registered.is_empty() {
                    let destinations = report
                        .registered
                        .iter()
                        .map(|imported| imported.destination.clone())
                        .collect::<Vec<_>>();
                    if let Some(watcher) = &mut self.file_watcher {
                        watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                        for destination in &destinations {
                            watcher.suppress_once(PathBuf::from("assets").join(destination));
                        }
                    }
                    self.asset_browser.refresh(&project.assets_root());
                    self.asset_thumbnails
                        .retain(|path, _| project.assets_root().join(path).is_file());
                    self.notify_registered_assets(&destinations);
                    // Dropping a model used to register it without ever
                    // cataloging it, leaving a file that resolved to nothing
                    // (ADR 0075).
                    for imported in &report.registered {
                        let source_path = project.assets_root().join(&imported.destination);
                        if engine::asset_path_matches_kind(
                            engine::AssetKind::GltfSource,
                            &imported.destination,
                        ) {
                            self.queue_model_import(imported.asset_id.clone(), source_path);
                        }
                    }
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::info(
                            "asset.external_import_succeeded",
                            format!(
                                "registered {} externally dropped asset(s) in `{}`",
                                destinations.len(),
                                if destination_folder.as_os_str().is_empty() {
                                    "assets".to_owned()
                                } else {
                                    destination_folder.display().to_string()
                                }
                            ),
                        ));
                }

                if !report.failures.is_empty() {
                    let details = report
                        .failures
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    for failure in &report.failures {
                        let code = match failure.kind {
                            crate::asset_management::ExternalAssetImportFailureKind::UnsupportedFormat => {
                                "asset.external_unsupported_format"
                            }
                            crate::asset_management::ExternalAssetImportFailureKind::CopyFailed(_) => {
                                "asset.external_copy_failed"
                            }
                            crate::asset_management::ExternalAssetImportFailureKind::InvalidFileName => {
                                "asset.external_invalid_filename"
                            }
                        };
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                code,
                                failure.to_string(),
                            ));
                    }
                    self.notify_asset_error(format!(
                        "Failed to import {} file(s): {}",
                        details.len(),
                        details.join("; ")
                    ));
                }
                self.refresh_scene_problems();
            }
            Err(error) => {
                let message = format!("Asset import failed: {error}");
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.external_import_failed",
                        message.clone(),
                    ));
                self.notify_asset_error(message);
                self.asset_browser.refresh(&project.assets_root());
                self.refresh_scene_problems();
            }
        }
    }

    pub(in crate::ui) fn reimport_asset_from_browser(&mut self, index: usize) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(browser_entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let Some(relative_path) = asset_relative_path_string(&browser_entry.relative_path) else {
            return;
        };
        let Some(source_id) = self
            .asset_manifest
            .iter()
            .find(|(_, entry)| {
                normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative_path)
            })
            .map(|(id, _)| id.clone())
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.reimport_unregistered",
                    format!("register `{relative_path}` before reimporting it"),
                ));
            return;
        };
        let source_path = project.assets_root().join(&relative_path);
        self.queue_model_import(source_id, source_path);
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.reimport_started",
                format!("reimporting `{relative_path}` in the background"),
            ));
    }

    pub(in crate::ui) fn handle_asset_import_result(&mut self, mut result: AssetImportResult) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        if project.path() != result.project_path {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.import_project_changed",
                    "discarded an import result because a different project is open",
                ));
            return;
        }
        self.asset_import_problems.retain(|diagnostic| {
            !matches!(
                diagnostic.target.as_ref(),
                Some(engine_authoring::DiagnosticTarget::Asset { id }) if id == &result.source_id
            )
        });
        if result.cancelled {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "asset.import_cancelled",
                    format!("cancelled import of `{}`", result.source_path.display()),
                ));
            return;
        }
        if let Some(error) = result.error {
            let diagnostic = engine_authoring::Diagnostic::error(
                "asset.gltf_import_failed",
                format!(
                    "failed to import `{}`: {error}",
                    result.source_path.display()
                ),
            )
            .with_target(engine_authoring::DiagnosticTarget::Asset {
                id: result.source_id,
            });
            self.asset_import_problems.push(diagnostic.clone());
            self.session.push_diagnostic(diagnostic);
            self.refresh_scene_problems();
            return;
        }

        let assets_root = project.assets_root();
        let dependencies = result
            .source_dependencies
            .iter()
            .map(|dependency| {
                dependency
                    .strip_prefix(&assets_root)
                    .map_err(|_| {
                        format!(
                            "dependency `{}` escapes the project assets root",
                            dependency.display()
                        )
                    })
                    .and_then(|relative| {
                        asset_relative_path_string(relative).ok_or_else(|| {
                            format!("dependency `{}` has an invalid path", dependency.display())
                        })
                    })
            })
            .collect::<Result<Vec<_>, _>>();
        let dependencies = match dependencies {
            Ok(dependencies) => dependencies,
            Err(error) => {
                let diagnostic =
                    engine_authoring::Diagnostic::error("asset.import_dependency_invalid", error)
                        .with_target(engine_authoring::DiagnosticTarget::Asset {
                            id: result.source_id,
                        });
                self.asset_import_problems.push(diagnostic.clone());
                self.session.push_diagnostic(diagnostic);
                self.refresh_scene_problems();
                return;
            }
        };

        let mut manifest = self.asset_manifest.clone();
        let Some(entry) = manifest.get_mut(&result.source_id) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.import_source_removed",
                    "discarded an import result because its source is no longer registered",
                ));
            return;
        };
        // Captured before the overwrite so the skeleton bind report (ADR
        // 0077 §6, AP-5) can compare the previous and current bone ledgers.
        let previous_skeleton_records = entry.import_settings.skeleton_records.clone();
        entry.import_settings.source_fingerprint = result.source_fingerprint;
        entry.import_settings.source_stamp = result.source_stamp;
        entry.import_settings.source_dependencies = dependencies;
        entry.import_settings.sub_assets = result.sub_assets;
        entry.import_settings.skeleton_records = result.skeleton_records.clone();
        entry.import_settings.humanoid_profiles = result.humanoid_profiles.clone();
        let sub_asset_count = entry.import_settings.sub_assets.len();

        let prefab_path = match result.prefab {
            Some(prefab) => match write_generated_prefab(&project, &result.source_id, &prefab) {
                Ok(path) => Some(path),
                Err(error) => {
                    let diagnostic = engine_authoring::Diagnostic::error(
                        "asset.generated_prefab_failed",
                        format!(
                            "imported `{}` but could not write its placement prefab: {error}",
                            result.source_path.display()
                        ),
                    )
                    .with_target(engine_authoring::DiagnosticTarget::Asset {
                        id: result.source_id.clone(),
                    });
                    self.asset_import_problems.push(diagnostic.clone());
                    self.session.push_diagnostic(diagnostic);
                    None
                }
            },
            None => None,
        };
        let Some(entry) = manifest.get_mut(&result.source_id) else {
            return;
        };
        entry.import_settings.generated_prefab = prefab_path;
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;

        // Publish only after the manifest accepted this exact import generation.
        if let Some(imported) = result.conversion_ready_model.take() {
            let contact_bones = self
                .asset_manifest
                .get(&result.source_id)
                .map(|entry| entry.import_settings.contact_bones.clone())
                .unwrap_or_default();
            self.preview_residency.publish_import_result(
                &result.source_id,
                &result.source_path,
                &result.source_dependencies,
                &result.skeleton_records,
                &contact_bones,
                imported,
                std::mem::take(&mut result.conversion_ready_textures),
            );
        }

        // AP-5: rebuild this source's bind report and contact interval
        // summary from the fresh import result, in memory only.
        let source_key = result.source_id.as_str().to_owned();
        let bind_reports: Vec<crate::anim_ux::SkeletonBindReport> = result
            .skeleton_records
            .iter()
            .map(|record| {
                let previous_bones = previous_skeleton_records
                    .iter()
                    .find(|previous| previous.id == record.id)
                    .map_or(&[][..], |previous| previous.bones.as_slice());
                crate::anim_ux::build_skeleton_bind_report(
                    &record.id,
                    previous_bones,
                    &record.bones,
                )
            })
            .collect();
        if bind_reports.is_empty() {
            self.skeleton_rebind_reports.remove(&source_key);
        } else {
            self.skeleton_rebind_reports
                .insert(source_key.clone(), bind_reports);
        }
        if result.animation_contacts.is_empty() {
            self.clip_contact_summaries.remove(&source_key);
        } else {
            self.clip_contact_summaries
                .insert(source_key, result.animation_contacts);
        }

        for diagnostic in result.diagnostics {
            let diagnostic = diagnostic.with_target(engine_authoring::DiagnosticTarget::Asset {
                id: result.source_id.clone(),
            });
            self.asset_import_problems.push(diagnostic.clone());
            self.session.push_diagnostic(diagnostic);
        }
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.import_succeeded",
                format!(
                    "imported `{}` with {sub_asset_count} stable sub-assets",
                    result.source_path.display()
                ),
            ));
        self.requeue_motions_paired_with(&result.source_id);
        self.refresh_scene_problems();
    }

    /// Reimports every `.vmd` motion baked against `model_source_id`
    /// (ADR 0097 §3).
    ///
    /// A motion's clip is a function of the PMX too — its IK chains and
    /// appended-parent links shape every baked curve — so a reimported model
    /// leaves each paired motion's catalog stale. Requeuing here is what makes
    /// "edit the model, motions follow" hold without the author reimporting
    /// each motion by hand.
    fn requeue_motions_paired_with(&mut self, model_source_id: &engine_authoring::AssetId) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let paired_key = model_source_id.as_str();
        let paired: Vec<(engine_authoring::AssetId, PathBuf)> = self
            .asset_manifest
            .iter()
            .filter(|(id, entry)| {
                id != &model_source_id
                    && entry
                        .import_settings
                        .resolved_motion_model_sources()
                        .contains(&paired_key)
            })
            .map(|(id, entry)| (id.clone(), project.assets_root().join(&entry.path)))
            .collect();
        for (motion_id, motion_path) in paired {
            self.queue_model_import(motion_id, motion_path);
        }
    }
}
