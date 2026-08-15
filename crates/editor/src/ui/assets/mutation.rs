//! Renaming, moving, and deleting assets and asset folders.
//!
//! Every mutation is staged as a `PendingAssetMutation` and confirmed in a
//! modal window, because the operation also rewrites the manifest and the
//! generated Rust sources that reference the asset.

use crate::ui::*;

impl EditorApp {
    pub(in crate::ui) fn begin_asset_mutation(&mut self, index: usize, kind: AssetMutationKind) {
        let Some(source) = self
            .asset_browser
            .entries()
            .get(index)
            .map(|entry| entry.relative_path.clone())
        else {
            return;
        };
        let selection_contains_source = self
            .asset_browser
            .selected_paths()
            .any(|path| path == &source);
        if !selection_contains_source {
            self.asset_browser.select_path(&source, false);
        }
        let batch_operation = matches!(kind, AssetMutationKind::Move | AssetMutationKind::Trash)
            && self.asset_browser.selected_paths().count() > 1;
        let destination = if batch_operation {
            source
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned()
        } else {
            source.to_string_lossy().into_owned()
        };
        self.pending_asset_mutation = Some(PendingAssetMutation {
            source,
            destination,
            kind,
        });
    }

    pub(in crate::ui) fn begin_folder_create(&mut self) {
        let parent = self.asset_browser.selected_folder().to_path_buf();
        self.pending_asset_mutation = Some(PendingAssetMutation {
            source: parent.clone(),
            destination: parent.join("New Folder").to_string_lossy().into_owned(),
            kind: AssetMutationKind::CreateFolder,
        });
    }

    pub(in crate::ui) fn begin_folder_mutation(&mut self, source: PathBuf, kind: AssetMutationKind) {
        self.pending_asset_mutation = Some(PendingAssetMutation {
            destination: source.to_string_lossy().into_owned(),
            source,
            kind,
        });
    }

    pub(in crate::ui) fn move_selected_assets_to_folder(&mut self, folder: PathBuf) {
        let sources = self
            .asset_browser
            .selected_paths()
            .cloned()
            .collect::<Vec<_>>();
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let moved_rust = sources.iter().any(|path| path.starts_with("scripts/rust"))
            || folder.starts_with("scripts/rust");
        match crate::asset_management::move_asset_batch(
            &project,
            &mut self.asset_manifest,
            &sources,
            &folder,
        ) {
            Ok(report) => {
                self.asset_browser.refresh(&project.assets_root());
                self.asset_browser.set_selected_folder(folder);
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.asset_batch_moved",
                        format!(
                            "moved {} paths and updated {} manifest entries",
                            report.moves.len(),
                            report.manifest_entries
                        ),
                    ));
                self.refresh_scene_problems();
                if moved_rust {
                    self.refresh_rust_after_asset_mutation(&project);
                }
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.asset_batch_move_failed",
                    error.to_string(),
                )),
        }
    }

    pub(in crate::ui) fn show_asset_mutation_window(&mut self, context: &egui::Context) {
        let mut selected_sources = self
            .asset_browser
            .selected_paths()
            .cloned()
            .collect::<Vec<_>>();
        let Some(pending) = self.pending_asset_mutation.as_mut() else {
            return;
        };
        let supports_batch = matches!(
            pending.kind,
            AssetMutationKind::Move | AssetMutationKind::Trash
        );
        if !supports_batch || !selected_sources.iter().any(|path| path == &pending.source) {
            selected_sources.clear();
            selected_sources.push(pending.source.clone());
        }
        let source_count = selected_sources.len();
        let title = match pending.kind {
            AssetMutationKind::Rename => "Rename Asset",
            AssetMutationKind::Move if source_count > 1 => "Move Assets",
            AssetMutationKind::Move => "Move Asset",
            AssetMutationKind::Trash if source_count > 1 => "Delete Assets",
            AssetMutationKind::Trash => "Delete Asset",
            AssetMutationKind::CreateFolder => "Create Asset Folder",
            AssetMutationKind::RenameFolder => "Rename Asset Folder",
            AssetMutationKind::TrashFolder => "Delete Asset Folder",
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                if !matches!(pending.kind, AssetMutationKind::CreateFolder) {
                    if supports_batch && source_count > 1 {
                        ui.label(format!("Selected: {source_count} assets"));
                        for source in selected_sources.iter().take(4) {
                            ui.monospace(source.display().to_string());
                        }
                        if source_count > 4 {
                            ui.small(format!("...and {} more", source_count - 4));
                        }
                    } else {
                        ui.label(format!("Source: {}", pending.source.display()));
                    }
                }
                if !matches!(
                    pending.kind,
                    AssetMutationKind::Trash | AssetMutationKind::TrashFolder
                ) {
                    if matches!(pending.kind, AssetMutationKind::Move) && source_count > 1 {
                        ui.label("Destination folder relative to Assets");
                    } else {
                        ui.label("Destination path relative to Assets");
                    }
                    ui.text_edit_singleline(&mut pending.destination);
                } else {
                    ui.label(format!(
                        "Deleting {source_count} item(s) unregisters them and moves them to .engine/asset_trash, so they can still be recovered manually."
                    ));
                }
                control_row(ui, |ui| {
                    confirmed = ui.button("Apply").clicked();
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if !open || cancelled {
            self.pending_asset_mutation = None;
            return;
        }
        if !confirmed {
            return;
        }
        let pending = self
            .pending_asset_mutation
            .take()
            .expect("pending asset mutation exists while its window is open");
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let moved_rust = selected_sources
            .iter()
            .any(|source| source.starts_with("scripts/rust"))
            || Path::new(pending.destination.trim()).starts_with("scripts/rust");
        let result: Result<String, crate::asset_management::AssetManagementError> =
            match pending.kind {
                AssetMutationKind::Rename => crate::asset_management::move_asset(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "moved {} to {} ({} manifest entries updated)",
                        report.source.display(),
                        report.destination.display(),
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::Move if source_count > 1 => {
                    crate::asset_management::move_asset_batch(
                        &project,
                        &mut self.asset_manifest,
                        &selected_sources,
                        Path::new(pending.destination.trim()),
                    )
                    .map(|report| {
                        format!(
                            "moved {} assets and updated {} manifest entries",
                            report.moves.len(),
                            report.manifest_entries
                        )
                    })
                }
                AssetMutationKind::Move => crate::asset_management::move_asset(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "moved {} to {} ({} manifest entries updated)",
                        report.source.display(),
                        report.destination.display(),
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::Trash if source_count > 1 => {
                    crate::asset_management::move_asset_paths_to_trash(
                        &project,
                        &mut self.asset_manifest,
                        &selected_sources,
                    )
                    .map(|report| {
                        format!(
                            "deleted {} assets ({} registered assets, recoverable in .engine/asset_trash)",
                            source_count,
                            report.manifest_entries
                        )
                    })
                }
                AssetMutationKind::Trash => crate::asset_management::move_asset_to_trash(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                )
                .map(|report| {
                    format!(
                        "deleted {} (recoverable in .engine/asset_trash)",
                        report.source.display()
                    )
                }),
                AssetMutationKind::CreateFolder => crate::asset_management::create_asset_folder(
                    &project,
                    Path::new(pending.destination.trim()),
                )
                .map(|path| format!("created asset folder {}", path.display())),
                AssetMutationKind::RenameFolder => crate::asset_management::move_asset_path(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "renamed folder and updated {} manifest entries",
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::TrashFolder => {
                    crate::asset_management::move_asset_paths_to_trash(
                        &project,
                        &mut self.asset_manifest,
                        std::slice::from_ref(&pending.source),
                    )
                    .map(|report| {
                        format!(
                            "deleted folder ({} registered assets, recoverable in .engine/asset_trash)",
                            report.manifest_entries
                        )
                    })
                }
            };
        match result {
            Ok(message) => {
                self.asset_browser.refresh(&project.assets_root());
                self.refresh_scene_problems();
                if moved_rust {
                    self.refresh_rust_after_asset_mutation(&project);
                }
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.asset_moved",
                        message,
                    ));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.asset_move_failed",
                    error.to_string(),
                )),
        }
    }

    fn refresh_rust_after_asset_mutation(&mut self, project: &ProjectRoot) {
        match engine_authoring::refresh_game_module_indexes(project) {
            Ok(()) => {
                self.component_source_index =
                    ComponentSourceIndex::build(&project.rust_scripts_dir());
                // Relocating a source changes its Rust module path. Rewriting
                // `use` paths is a separate refactoring feature, so the build
                // that follows is what reports the references left behind.
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.rust_module_path_changed",
                        "Rust module paths changed; `use` paths are not updated automatically, so fix any reference the next build reports",
                    ));
                self.request_game_build_after_edit();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.game_index_refresh_failed",
                    error.to_string(),
                )),
        }
    }

}

#[derive(Clone, Copy)]
pub(in crate::ui) enum AssetMutationKind {
    Rename,
    Move,
    Trash,
    CreateFolder,
    RenameFolder,
    TrashFolder,
}

pub(in crate::ui) struct PendingAssetMutation {
    source: PathBuf,
    destination: String,
    kind: AssetMutationKind,
}
