//! Per-source Import Settings window.
//!
//! Edits the contact-bone overrides stored on an imported source, plus the
//! paired-model picker shown for a `*.vmd` motion source.

use crate::ui::*;
use super::manifest::save_asset_manifest;
use super::motion_pairing::show_motion_pairing_editor;
use super::retarget_window::{pmx_model_paths, pmx_model_sources, registered_model_retarget_pairs};

impl EditorApp {
    /// Opens the Import Settings window for a registered glTF/GLB source row
    /// (contact-bones override editing + contact interval display, AP-5).
    pub(in crate::ui) fn open_import_settings_editor(&mut self, index: usize) {
        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let Some((source_id, _)) = self.manifest_entry_for(&entry.relative_path) else {
            return;
        };
        let contact_bones = self
            .asset_manifest
            .get(&source_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();
        let motion_pairing = engine::asset_path_matches_kind(
            engine::AssetKind::MotionSource,
            &entry.relative_path,
        )
        .then(|| MotionPairingState {
            motion_path: self
                .project_root
                .as_ref()
                .map(|project| project.assets_root().join(&entry.relative_path)),
            original: self
                .asset_manifest
                .get(&source_id)
                .and_then(|entry| entry.import_settings.motion_original_model_source.as_deref())
                .and_then(|original| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(original)).ok()
                }),
            selected: self
                .asset_manifest
                .get(&source_id)
                .map(|entry| {
                    entry
                        .import_settings
                        .resolved_motion_model_sources()
                        .into_iter()
                        .filter_map(|paired| {
                            AssetId::from_stable_id(engine_authoring::StableId::new(paired)).ok()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            candidates: pmx_model_sources(&self.asset_manifest),
            candidate_paths: self
                .project_root
                .as_ref()
                .map(|project| pmx_model_paths(&self.asset_manifest, &project.assets_root()))
                .unwrap_or_default(),
            retarget_pairs: self
                .project_root
                .as_ref()
                .map(|project| {
                    registered_model_retarget_pairs(
                        &self.asset_manifest,
                        &project.assets_root(),
                    )
                })
                .unwrap_or_default(),
            recorded_model_name: self.project_root.as_ref().and_then(|project| {
                engine::vmd_recorded_model_name_path(
                    &project.assets_root().join(&entry.relative_path),
                )
                .ok()
                    .filter(|name| !name.trim().is_empty())
            }),
            compatibility_reports: Vec::new(),
        });
        self.import_settings_editor = Some(ImportSettingsEditorState {
            source_id,
            relative_path: entry.relative_path.clone(),
            contact_bones,
            motion_pairing,
        });
    }

    pub(in crate::ui) fn show_import_settings_editor_window(&mut self, context: &egui::Context) {
        let Some(state) = &mut self.import_settings_editor else {
            return;
        };
        let mut open = true;
        let mut save_requested = false;
        let source_id_str = state.source_id.as_str().to_owned();
        let clip_contacts = self.clip_contact_summaries.get(&source_id_str).cloned();
        let title = format!("Import Settings: {}", state.relative_path.display());
        egui::Window::new(title)
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                // Edited in place; persisted only when Save is clicked so an
                // in-progress edit is never written half-typed.
                if let Some(pairing) = &mut state.motion_pairing {
                    show_motion_pairing_editor(ui, pairing);
                    ui.separator();
                }
                let _ = crate::anim_ux::show_contact_bones_editor(ui, &mut state.contact_bones);
                save_requested = ui
                    .button("Save and Reimport")
                    .on_hover_text(
                        "Writes the contact-bones override and re-detects contact intervals \
                         (ADR 0080 §1) by reimporting this source",
                    )
                    .clicked();
                ui.separator();
                ui.strong("Detected contact intervals (latest import)");
                match clip_contacts {
                    Some(clips) if !clips.is_empty() => {
                        for clip in &clips {
                            ui.label(format!("Clip: {}", clip.clip_name));
                            if clip.intervals.is_empty() {
                                ui.label("  (no contact intervals detected)");
                            }
                            for interval in &clip.intervals {
                                ui.label(format!(
                                    "  {} : {:.3}s - {:.3}s",
                                    interval.bone_name, interval.start, interval.end
                                ));
                            }
                        }
                    }
                    _ => {
                        ui.label("No contact interval data yet; run Reimport to detect intervals.");
                    }
                }
            });
        if save_requested {
            self.save_import_settings_editor();
        }
        if !open {
            self.import_settings_editor = None;
        }
    }

    /// Persists the Import Settings window's edited `contact_bones` back into
    /// the manifest and queues a reimport so the displayed contact intervals
    /// refresh against the new override (ADR 0080 §1, AP-5).
    fn save_import_settings_editor(&mut self) {
        let Some(state) = &self.import_settings_editor else {
            return;
        };
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let source_id = state.source_id.clone();
        let contact_bones = state.contact_bones.clone();
        let relative_path = state.relative_path.clone();
        let motion_model_sources = state.motion_pairing.as_ref().map(|pairing| {
            pairing
                .selected
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        });
        let motion_original_model_source = state.motion_pairing.as_ref().and_then(|pairing| {
            pairing
                .original
                .as_ref()
                .map(|id| id.as_str().to_owned())
        });
        let mut manifest = self.asset_manifest.clone();
        let Some(entry) = manifest.get_mut(&source_id) else {
            return;
        };
        entry.import_settings.contact_bones = contact_bones;
        // Only a motion source carries a pairing at all; the outer `Option`
        // distinguishes "this source has no pairing field" from "the author
        // cleared the pairing".
        if let Some(paired) = motion_model_sources {
            entry.import_settings.motion_model_sources = paired;
            entry.import_settings.motion_original_model_source = motion_original_model_source;
        }
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;
        let source_path = project.assets_root().join(&relative_path);
        self.queue_model_import(source_id, source_path);
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.reimport_started",
                format!(
                    "reimporting `{}` in the background",
                    relative_path.display()
                ),
            ));
    }

}

/// Open document state for a glTF/GLB source's Import Settings window
/// (contact-bones override editing + contact interval display, AP-5).
pub(in crate::ui) struct ImportSettingsEditorState {
    /// The source's stable manifest [`AssetId`].
    pub(in crate::ui) source_id: AssetId,
    /// Asset-relative path of the source file, kept for the window title.
    pub(in crate::ui) relative_path: PathBuf,
    /// Editable copy of `ImportSettings::contact_bones`; written back to the
    /// manifest and reimported when the user clicks Save.
    pub(in crate::ui) contact_bones: Vec<String>,
    /// For a `*.vmd` motion source: the pairing UI's state (ADR 0097 §3).
    /// `None` for every other source kind, which hides the picker entirely
    /// rather than showing a control that cannot apply.
    pub(in crate::ui) motion_pairing: Option<MotionPairingState>,
}
