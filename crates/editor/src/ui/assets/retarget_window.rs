//! Retarget Map editor, its creation picker, and the skeleton bind report.
//!
//! A Retarget Map connects one source skeleton to one target skeleton. The
//! picker exists because either side may record more than one skeleton, in
//! which case the pair cannot be chosen automatically.

use crate::ui::*;
use super::manifest::save_asset_manifest;

impl EditorApp {
    /// Shows the skeleton bind report detail view opened from a Problems
    /// panel `anim.skeleton_rebind` entry (ADR 0077 §6, AP-5).
    pub(in crate::ui) fn show_skeleton_bind_report_window(&mut self, context: &egui::Context) {
        let Some(source_id) = self.show_skeleton_bind_report.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Skeleton Bind Report")
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                ui.label(format!("Source: {source_id}"));
                match self.skeleton_rebind_reports.get(&source_id) {
                    Some(reports) if !reports.is_empty() => {
                        for report in reports {
                            ui.separator();
                            ui.strong(format!("Skeleton {}", report.skeleton_id));
                            crate::anim_ux::show_skeleton_bind_report(ui, report);
                        }
                    }
                    _ => {
                        ui.label("No bind report is recorded for this source.");
                    }
                }
            });
        if !open {
            self.show_skeleton_bind_report = None;
        }
    }

    /// Opens the RetargetMap inspector window for a `*.retarget.json` asset
    /// (AP-5, ADR 0079 §1).
    pub(in crate::ui) fn open_retarget_map_editor(&mut self, relative_path: PathBuf, abs_path: PathBuf) {
        let result = fs::read_to_string(&abs_path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                engine::RetargetMap::from_json(&json).map_err(|error| error.to_string())
            });
        match result {
            Ok(map) => {
                self.retarget_map_editor = Some(RetargetMapEditorState { relative_path, map });
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_open_failed",
                    format!("failed to open {}: {error}", abs_path.display()),
                )),
        }
    }

    pub(in crate::ui) fn show_retarget_map_editor_window(&mut self, context: &egui::Context) {
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let model =
            crate::anim_ux::build_retarget_map_inspector_model(&state.map, &self.asset_manifest);
        let title = format!("Retarget Map: {}", state.relative_path.display());
        let mut open = true;
        let mut action = None;
        egui::Window::new(title)
            .open(&mut open)
            .default_width(480.0)
            .show(context, |ui| {
                action = crate::anim_ux::show_retarget_map_inspector(ui, &model);
            });
        match action {
            Some(crate::anim_ux::RetargetMapInspectorAction::RerunNameMatching) => {
                self.rerun_retarget_map_name_matching();
            }
            Some(crate::anim_ux::RetargetMapInspectorAction::SetAlwaysPackage(always_package)) => {
                self.set_retarget_map_always_package(always_package);
            }
            None => {}
        }
        if !open {
            self.retarget_map_editor = None;
        }
    }

    /// Handles the RetargetMap inspector's "Always package" checkbox (AP-7):
    /// writes [`engine::RetargetMap::always_package`] back to the open
    /// `*.retarget.json` file, the same write path as "Re-run name matching".
    pub(in crate::ui) fn set_retarget_map_always_package(&mut self, always_package: bool) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let mut updated = state.map.clone();
        updated.always_package = always_package;
        let json = match updated.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.retarget_map_save_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        let relative_path = state.relative_path.clone();
        let full_path = project.assets_root().join(&relative_path);
        if let Err(error) = replace_file_contents(&full_path, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_save_failed",
                    format!("failed to save {}: {error}", full_path.display()),
                ));
            return;
        }
        if let Some(state) = &mut self.retarget_map_editor {
            state.map = updated;
        }
        self.refresh_scene_problems();
    }

    /// Handles the RetargetMap inspector's "Re-run name matching" action:
    /// regenerates name-matched `bone_pairs` (explicit pairs win, per
    /// [`crate::anim_ux::merge_bone_pairs`]) and writes the result straight
    /// back to the open `*.retarget.json` file.
    fn rerun_retarget_map_name_matching(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let source_record = crate::anim_ux::find_skeleton_record(
            &self.asset_manifest,
            state.map.source_skeleton.as_str(),
        )
        .cloned();
        let target_record = crate::anim_ux::find_skeleton_record(
            &self.asset_manifest,
            state.map.target_skeleton.as_str(),
        )
        .cloned();
        let (Some(source_record), Some(target_record)) = (source_record, target_record) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.retarget_map_rerun_unresolvable",
                    "cannot re-run name matching: one of the mapped skeletons is not currently \
                     registered in this project",
                ));
            return;
        };
        let updated = crate::anim_ux::rerun_name_matching(
            &state.map,
            &source_record.bones,
            &target_record.bones,
        );
        let json = match updated.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.retarget_map_save_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        let relative_path = state.relative_path.clone();
        let full_path = project.assets_root().join(&relative_path);
        if let Err(error) = replace_file_contents(&full_path, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_save_failed",
                    format!("failed to save {}: {error}", full_path.display()),
                ));
            return;
        }
        if let Some(state) = &mut self.retarget_map_editor {
            state.map = updated;
        }
        self.refresh_scene_problems();
    }

    /// Creates a `*.retarget.json` map for the (source, target) skeleton pair
    /// and registers it like any other created asset (AP-5 creation flow for
    /// `anim.retarget_map_missing`).
    ///
    /// When a side binds to more than one skeleton (multiple skins in one
    /// imported file), the pair is ambiguous: rather than guessing via
    /// `skeleton_records.first()`, a picker window opens so the user chooses
    /// which skeleton on each side to map (AP-6 scope (b)). Exactly one
    /// record on both sides keeps the one-click behavior below.
    pub(in crate::ui) fn create_retarget_map_from_browser(
        &mut self,
        source_index: usize,
        target_source_id: AssetId,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(source_index) else {
            return;
        };
        let source_relative_path = entry.relative_path.clone();
        let Some((source_id, _)) = self.manifest_entry_for(&source_relative_path) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.retarget_map_create_unregistered",
                    "register the source asset before creating a retarget map for it",
                ));
            return;
        };
        let source_records = self
            .asset_manifest
            .get(&source_id)
            .map(|entry| entry.import_settings.skeleton_records.clone())
            .unwrap_or_default();
        if source_records.is_empty() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    "source has no recorded skeleton; import it before creating a retarget map",
                ));
            return;
        }
        let target_records = self
            .asset_manifest
            .get(&target_source_id)
            .map(|entry| entry.import_settings.skeleton_records.clone())
            .unwrap_or_default();
        if target_records.is_empty() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    "target has no recorded skeleton; import it before creating a retarget map",
                ));
            return;
        }

        if source_records.len() == 1 && target_records.len() == 1 {
            self.write_retarget_map_for_pair(
                &project,
                &source_relative_path,
                &source_records[0],
                &target_source_id,
                &target_records[0],
            );
            return;
        }

        self.retarget_map_creation_picker = Some(RetargetMapCreationPickerState {
            source_relative_path,
            target_source_id,
            source_records,
            target_records,
            selected_source: 0,
            selected_target: 0,
        });
    }

    /// Generates and registers a `*.retarget.json` map for one resolved
    /// (source, target) skeleton pair.
    ///
    /// Shared by [`Self::create_retarget_map_from_browser`]'s one-click path
    /// and the multi-skin creation picker's confirm action (AP-6 scope (b)),
    /// so both write the map through the same asset-registration logic.
    pub(in crate::ui) fn write_retarget_map_for_pair(
        &mut self,
        project: &ProjectRoot,
        source_relative_path: &Path,
        source_record: &engine::SkeletonRecord,
        target_source_id: &AssetId,
        target_record: &engine::SkeletonRecord,
    ) {
        let Some(source_skeleton_id) =
            AssetId::from_stable_id(engine_authoring::StableId::new(source_record.id.clone())).ok()
        else {
            return;
        };
        let Some(target_skeleton_id) =
            AssetId::from_stable_id(engine_authoring::StableId::new(target_record.id.clone())).ok()
        else {
            return;
        };
        let source_asset = crate::anim_ux::synthetic_skeleton_asset(
            source_skeleton_id,
            source_record.identity,
            &source_record.bones,
        );
        let target_asset = crate::anim_ux::synthetic_skeleton_asset(
            target_skeleton_id,
            target_record.identity,
            &target_record.bones,
        );
        let map = engine::generate_retarget_map(&source_asset, &target_asset);

        let source_stem = source_relative_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "source".to_owned());
        let target_stem = self
            .asset_manifest
            .get(target_source_id)
            .map(|entry| Path::new(&entry.path))
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "target".to_owned());
        let file_name = crate::anim_ux::retarget_map_file_name(&source_stem, &target_stem);
        let relative = file_name.clone();
        let destination = project.assets_root().join(&relative);
        if destination.exists() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.retarget_map_create_exists",
                    format!("retarget map destination already exists: {relative}"),
                ));
            return;
        }
        let json = match map.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.retarget_map_create_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        if let Err(error) = replace_file_contents(&destination, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    format!("failed to write {relative}: {error}"),
                ));
            return;
        }
        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        let name = unique_asset_name(&source_stem, &manifest);
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(name),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(project, &manifest) {
            let _ = fs::remove_file(&destination);
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;
        self.asset_browser.refresh(&project.assets_root());
        self.asset_browser
            .select_relative_path(Path::new(&relative));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.retarget_map_created",
                format!(
                    "created retarget map `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
    }

    /// Renders the multi-skin retarget-map creation picker window opened by
    /// [`Self::create_retarget_map_from_browser`] (AP-6 scope (b)). Confirm
    /// writes the map for the currently selected pair through
    /// [`Self::write_retarget_map_for_pair`]; Cancel and closing the window
    /// both discard the picker state without writing anything.
    pub(in crate::ui) fn show_retarget_map_creation_picker_window(&mut self, context: &egui::Context) {
        let Some(state) = &mut self.retarget_map_creation_picker else {
            return;
        };
        let model = crate::anim_ux::build_retarget_map_creation_picker_model(
            &self.asset_manifest,
            &state.source_records,
            &state.target_records,
        );
        let mut open = true;
        let mut action = None;
        egui::Window::new("Choose Retarget Skeletons")
            .open(&mut open)
            .default_width(360.0)
            .show(context, |ui| {
                action = crate::anim_ux::show_retarget_map_creation_picker(
                    ui,
                    &model,
                    &mut state.selected_source,
                    &mut state.selected_target,
                );
            });
        match action {
            Some(crate::anim_ux::RetargetMapCreationPickerAction::Confirm) => {
                let state = self
                    .retarget_map_creation_picker
                    .take()
                    .expect("state was matched as Some above");
                let Some(project) = self.project_root.clone() else {
                    return;
                };
                let Some(source_record) = state.source_records.get(state.selected_source).cloned()
                else {
                    return;
                };
                let Some(target_record) = state.target_records.get(state.selected_target).cloned()
                else {
                    return;
                };
                self.write_retarget_map_for_pair(
                    &project,
                    &state.source_relative_path,
                    &source_record,
                    &state.target_source_id,
                    &target_record,
                );
            }
            Some(crate::anim_ux::RetargetMapCreationPickerAction::Cancel) => {
                self.retarget_map_creation_picker = None;
            }
            None => {
                if !open {
                    self.retarget_map_creation_picker = None;
                }
            }
        }
    }

}

/// Open document state for the RetargetMap inspector window (AP-5).
pub(in crate::ui) struct RetargetMapEditorState {
    /// Asset-relative path of the open `*.retarget.json` file.
    pub(in crate::ui) relative_path: PathBuf,
    /// Currently edited map. Every "Re-run name matching" click writes
    /// straight back to disk (mirroring the Material Editor's
    /// edit-then-persist flow), so this is never a separate unsaved buffer.
    pub(in crate::ui) map: engine::RetargetMap,
}

/// Open state for the multi-skin retarget-map creation picker (AP-6 scope
/// (b)), shown when either side of a (source, target) pair records more than
/// one skeleton. Pure UI state: canceling or closing the window drops it
/// without writing anything.
pub(in crate::ui) struct RetargetMapCreationPickerState {
    /// Asset-relative path of the source file (used to derive the created
    /// map's file name and stem, same as the one-click path).
    pub(in crate::ui) source_relative_path: PathBuf,
    /// Manifest [`AssetId`] of the target source.
    pub(in crate::ui) target_source_id: AssetId,
    /// Every skeleton recorded on the source side.
    pub(in crate::ui) source_records: Vec<engine::SkeletonRecord>,
    /// Every skeleton recorded on the target side.
    pub(in crate::ui) target_records: Vec<engine::SkeletonRecord>,
    /// Index into `source_records` currently selected in the picker.
    pub(in crate::ui) selected_source: usize,
    /// Index into `target_records` currently selected in the picker.
    pub(in crate::ui) selected_target: usize,
}

/// Resolves registered skeleton-to-skeleton Retarget Maps back to their PMX
/// model sources for the VMD Import Settings readiness summary.
/// Lists registered model sources that own at least one recorded skeleton.
///
/// VMD motion sources may repeat the skeleton records of the PMX models they
/// were baked against. They do not own those rigs, so accepting every manifest
/// entry with a skeleton record would incorrectly expose motions as retarget
/// map targets.
pub(in crate::ui) fn retarget_map_model_source_choices(
    manifest: &engine::AssetManifest,
) -> Vec<(AssetId, String)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            // GltfSource is the legacy category name shared by all supported
            // model documents: glTF, GLB, FBX, and PMX.
            engine::asset_path_matches_kind(
                engine::AssetKind::GltfSource,
                Path::new(&entry.path),
            ) && !entry.import_settings.skeleton_records.is_empty()
        })
        .map(|(id, entry)| {
            (
                id.clone(),
                entry
                    .name
                    .clone()
                    .unwrap_or_else(|| entry.path.clone()),
            )
        })
        .collect()
}

pub(super) fn registered_model_retarget_pairs(
    manifest: &engine::AssetManifest,
    assets_root: &Path,
) -> Vec<(AssetId, AssetId)> {
    let maps = engine::load_registered_retarget_maps(assets_root, manifest);
    let owner = |skeleton: &AssetId| {
        manifest.iter().find_map(|(model, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
                .then_some(())?;
            entry
                .import_settings
                .skeleton_records
                .iter()
                .any(|record| record.id == skeleton.as_str())
                .then(|| model.clone())
        })
    };
    let mut pairs = maps
        .iter()
        .filter_map(|(_, map)| Some((owner(&map.source_skeleton)?, owner(&map.target_skeleton)?)))
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    pairs.dedup();
    pairs
}

/// Lists every registered `.pmx` source as `(id, display label)` for the
/// motion pairing picker, in the manifest's own stable order.
pub(super) fn pmx_model_sources(manifest: &engine::AssetManifest) -> Vec<(AssetId, String)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        })
        .map(|(id, entry)| {
            let label = entry
                .name
                .clone()
                .unwrap_or_else(|| entry.path.clone());
            (id.clone(), label)
        })
        .collect()
}

/// Resolves registered PMX source IDs to absolute project asset paths for the
/// on-demand compatibility checker. The manifest remains the source of truth;
/// no filesystem scan or filename-based pairing is performed.
pub(super) fn pmx_model_paths(
    manifest: &engine::AssetManifest,
    assets_root: &Path,
) -> Vec<(AssetId, PathBuf)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        })
        .map(|(id, entry)| (id.clone(), assets_root.join(&entry.path)))
        .collect()
}
