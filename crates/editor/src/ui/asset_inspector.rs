//! Inspector presentation and display-name authoring for Asset Browser selections.
//!
//! Imported sub-assets keep their deterministic `AssetId`; only their editor-facing
//! label is overridden. Overrides live below `.engine/editor/` so reimport can replace
//! the source catalog without erasing author intent or changing the runtime manifest
//! schema.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SUB_ASSET_NAMES_PATH: &str = ".engine/editor/sub_asset_names.json";
const SUB_ASSET_NAMES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSubAssetNames {
    #[serde(default = "sub_asset_names_schema_version")]
    schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    names: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    source_names: BTreeMap<String, String>,
}

fn sub_asset_names_schema_version() -> u32 {
    SUB_ASSET_NAMES_SCHEMA_VERSION
}

#[derive(Debug, Clone)]
struct AssetInspectorModel {
    relative_path: PathBuf,
    kind: AssetKind,
    registered: Option<RegisteredAssetInspectorModel>,
}

#[derive(Debug, Clone)]
struct RegisteredAssetInspectorModel {
    id: AssetId,
    display_name: String,
    sub_assets: Vec<SubAssetInspectorModel>,
}

#[derive(Debug, Clone)]
struct SubAssetInspectorModel {
    id: String,
    kind: engine::ImportedSubAssetKind,
    source_name: String,
    display_name: String,
    name_overridden: bool,
    /// Project or built-in asset replacing this imported sub-asset globally.
    override_target: Option<String>,
    /// Display label resolved for [`Self::override_target`].
    override_target_name: Option<String>,
    /// Whether a Material override points at an editable project file.
    override_target_editable_material: bool,
    /// Compatible standalone targets offered by the override picker.
    override_choices: Arc<Vec<AssetChoice>>,
    /// Human-readable references found in the currently open scene.
    current_scene_usages: Arc<Vec<String>>,
}

/// Cached expensive details for the one sub-asset currently selected.
///
/// Large PMX files can expose hundreds of children. Building project-wide
/// override choices and scanning every scene entity for every child on every
/// frame made merely opening the source asset unnecessarily expensive.
#[derive(Debug, Clone)]
struct SubAssetDetailCache {
    sub_asset_id: String,
    manifest_revision: u64,
    scene_revision: Option<u64>,
    assets_root: Option<PathBuf>,
    override_choices: Arc<Vec<AssetChoice>>,
    current_scene_usages: Arc<Vec<String>>,
}

/// Result of a display-name field once egui reported the focus state for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameEdit {
    /// The field still owns the focus, or the text matches the committed value.
    Pending,
    /// The author pressed `Escape`; the buffer must fall back to the committed value.
    Cancelled,
    /// The author confirmed a different value with `Enter` or by moving the focus away.
    Committed,
}

/// Decides what a display-name field means once it stops owning the keyboard focus.
///
/// Kept free of egui types so the confirm/cancel rules stay unit-testable.
fn name_edit_outcome(
    lost_focus: bool,
    escape_pressed: bool,
    buffer: &str,
    committed: &str,
) -> NameEdit {
    if !lost_focus {
        return NameEdit::Pending;
    }
    if escape_pressed {
        return NameEdit::Cancelled;
    }
    if buffer.trim() == committed {
        NameEdit::Pending
    } else {
        NameEdit::Committed
    }
}

/// Draws a display-name field that commits on `Enter` or focus loss and cancels on `Escape`.
///
/// A cancelled edit restores `committed` into `buffer` before returning, so the caller
/// only has to react to [`NameEdit::Committed`].
fn display_name_field(ui: &mut egui::Ui, buffer: &mut String, committed: &str) -> NameEdit {
    ui.label("Display Name");
    let response = ui.text_edit_singleline(buffer);
    let outcome = name_edit_outcome(
        response.lost_focus(),
        ui.input(|input| input.key_pressed(egui::Key::Escape)),
        buffer,
        committed,
    );
    if outcome == NameEdit::Cancelled {
        buffer.clear();
        buffer.push_str(committed);
    }
    outcome
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AssetInspectorAction {
    Rename { id: AssetId, name: String },
    SelectSub(String),
    RenameSub { id: String, name: String },
    ResetSub { id: String },
    /// Extracts an imported Material sub-asset into a standalone, editable
    /// `.material.json` file and remaps the sub-asset ID to it (ADR 0101).
    ExtractSubAssetMaterial { id: String },
    /// Opens the standalone Material a sub-asset was already extracted to.
    OpenRemappedMaterial { asset_id: AssetId },
    /// Assigns one compatible existing asset as a model-level override.
    SetSubAssetOverride {
        source_id: AssetId,
        id: String,
        kind: engine::ImportedSubAssetKind,
        target: AssetId,
    },
    /// Removes a model-level override, reverting to the imported value.
    ResetSubAssetOverride {
        source_id: AssetId,
        id: String,
        kind: engine::ImportedSubAssetKind,
    },
    /// Opens one folder of this asset's path in the Asset Browser.
    OpenFolder(PathBuf),
}

/// Persistent UI state for the Asset Browser-driven Inspector target.
pub(super) struct AssetInspectorState {
    active: bool,
    current_asset: Option<PathBuf>,
    selected_sub_asset: Option<String>,
    asset_name_buffer: String,
    sub_asset_name_buffer: String,
    source_names: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
    /// Name search over this asset's sub-asset list, cleared whenever the
    /// selected asset changes.
    sub_asset_search: String,
    /// Kind filter (Mesh/Material/Texture/...) over the same list.
    sub_asset_kind_filter: Option<engine::ImportedSubAssetKind>,
    /// Expensive picker and reverse-reference data for only the selected row.
    detail_cache: Option<SubAssetDetailCache>,
}

impl AssetInspectorState {
    pub(super) fn new() -> Self {
        Self {
            active: false,
            current_asset: None,
            selected_sub_asset: None,
            asset_name_buffer: String::new(),
            sub_asset_name_buffer: String::new(),
            source_names: BTreeMap::new(),
            overrides: BTreeMap::new(),
            sub_asset_search: String::new(),
            sub_asset_kind_filter: None,
            detail_cache: None,
        }
    }

    pub(super) fn open_project(&mut self, root: &ProjectRoot) -> Result<(), String> {
        self.active = false;
        self.current_asset = None;
        self.selected_sub_asset = None;
        self.asset_name_buffer.clear();
        self.sub_asset_name_buffer.clear();
        self.sub_asset_search.clear();
        self.sub_asset_kind_filter = None;
        self.detail_cache = None;
        let persisted = load_sub_asset_names(root)?;
        self.source_names = persisted.source_names;
        self.overrides = persisted.names;
        Ok(())
    }

    pub(super) fn activate(&mut self) {
        self.active = true;
    }

    pub(super) fn deactivate(&mut self) {
        self.active = false;
    }

    /// Captures importer-provided names before replacing them with persisted labels.
    ///
    /// Calling this every frame is intentional: a completed reimport writes a fresh
    /// `sub_assets` catalog, and this pass immediately reapplies the author override.
    pub(super) fn reconcile(&mut self, manifest: &mut engine::AssetManifest) -> bool {
        let previous_source_names = self.source_names.clone();
        let source_ids = manifest
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let mut present = std::collections::BTreeSet::new();
        for source_id in source_ids {
            let Some(entry) = manifest.get_mut(&source_id) else {
                continue;
            };
            for sub_asset in &mut entry.import_settings.sub_assets {
                present.insert(sub_asset.id.clone());
                match self.overrides.get(&sub_asset.id) {
                    Some(display_name) => {
                        if &sub_asset.name != display_name {
                            self.source_names
                                .insert(sub_asset.id.clone(), sub_asset.name.clone());
                            sub_asset.name = display_name.clone();
                        } else {
                            self.source_names
                                .entry(sub_asset.id.clone())
                                .or_insert_with(|| sub_asset.name.clone());
                        }
                    }
                    None => {
                        self.source_names
                            .insert(sub_asset.id.clone(), sub_asset.name.clone());
                    }
                }
            }
        }
        if self
            .selected_sub_asset
            .as_ref()
            .is_some_and(|id| !present.contains(id))
        {
            self.selected_sub_asset = None;
            self.sub_asset_name_buffer.clear();
        }
        self.source_names != previous_source_names
    }

    fn show(
        &mut self,
        ui: &mut egui::Ui,
        model: Option<&AssetInspectorModel>,
    ) -> Option<AssetInspectorAction> {
        if !self.active {
            return None;
        }
        let Some(model) = model else {
            ui.heading("Inspector");
            ui.label("Select one asset to inspect it.");
            return None;
        };

        if self.current_asset.as_ref() != Some(&model.relative_path) {
            self.current_asset = Some(model.relative_path.clone());
            self.selected_sub_asset = None;
            self.sub_asset_name_buffer.clear();
            self.sub_asset_search.clear();
            self.sub_asset_kind_filter = None;
            self.detail_cache = None;
            self.asset_name_buffer = model
                .registered
                .as_ref()
                .map(|registered| registered.display_name.clone())
                .unwrap_or_else(|| {
                    model
                        .relative_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
        }

        ui.heading("Asset");
        ui.strong(
            model
                .relative_path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_else(|| model.relative_path.to_string_lossy()),
        );
        ui.label(format!("Type: {}", model.kind.label()));
        ui.label("Path");
        let mut action = show_asset_path_navigation(ui, &model.relative_path);

        match &model.registered {
            Some(registered) => {
                ui.separator();
                if display_name_field(ui, &mut self.asset_name_buffer, &registered.display_name)
                    == NameEdit::Committed
                {
                    action = Some(AssetInspectorAction::Rename {
                        id: registered.id.clone(),
                        name: self.asset_name_buffer.clone(),
                    });
                }
                ui.label("Asset ID");
                ui.monospace(registered.id.as_str());
                ui.small("Renaming here changes only the display label, not the file path or ID.");

                if !registered.sub_assets.is_empty() {
                    ui.separator();
                    ui.heading("Sub-assets");

                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.sub_asset_search)
                                .hint_text("Search sub-assets...")
                                .desired_width(160.0),
                        );
                        if ui.small_button("Clear").clicked() {
                            self.sub_asset_search.clear();
                        }
                        egui::ComboBox::from_id_salt("asset_inspector_sub_asset_kind_filter")
                            .selected_text(
                                self.sub_asset_kind_filter
                                    .map_or("All kinds", imported_sub_asset_label),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.sub_asset_kind_filter,
                                    None,
                                    "All kinds",
                                );
                                for kind in present_sub_asset_kinds(&registered.sub_assets) {
                                    ui.selectable_value(
                                        &mut self.sub_asset_kind_filter,
                                        Some(kind),
                                        imported_sub_asset_label(kind),
                                    );
                                }
                            });
                    });

                    // Bounded so a source with dozens of sub-assets cannot
                    // push the detail panel below far out of view — the two
                    // stay close together in one panel instead of trading
                    // scroll position for panel-to-panel travel.
                    let search = self.sub_asset_search.trim().to_ascii_lowercase();
                    let mut visible_count = 0_usize;
                    egui::ScrollArea::vertical()
                        .id_salt("asset_inspector_sub_asset_list_scroll")
                        .max_height(180.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for sub_asset in &registered.sub_assets {
                                if self
                                    .sub_asset_kind_filter
                                    .is_some_and(|filter| filter != sub_asset.kind)
                                {
                                    continue;
                                }
                                if !search.is_empty()
                                    && !sub_asset
                                        .display_name
                                        .to_ascii_lowercase()
                                        .contains(&search)
                                {
                                    continue;
                                }
                                visible_count += 1;
                                let selected =
                                    self.selected_sub_asset.as_deref() == Some(&sub_asset.id);
                                let status = if sub_asset.override_target.is_some() {
                                    " [overridden]"
                                } else {
                                    ""
                                };
                                let label = format!(
                                    "{} {}{}",
                                    imported_sub_asset_badge(sub_asset.kind),
                                    sub_asset.display_name,
                                    status
                                );
                                if ui.selectable_label(selected, label).clicked() {
                                    action =
                                        Some(AssetInspectorAction::SelectSub(sub_asset.id.clone()));
                                }
                            }
                            if visible_count == 0 {
                                ui.weak("No sub-assets match the search/filter.");
                            }
                        });
                    ui.separator();

                    if let Some(selected) = self.selected_sub_asset.as_deref().and_then(|id| {
                        registered
                            .sub_assets
                            .iter()
                            .find(|sub_asset| sub_asset.id == id)
                    }) {
                        if display_name_field(
                            ui,
                            &mut self.sub_asset_name_buffer,
                            &selected.display_name,
                        ) == NameEdit::Committed
                        {
                            action = Some(AssetInspectorAction::RenameSub {
                                id: selected.id.clone(),
                                name: self.sub_asset_name_buffer.clone(),
                            });
                        }
                        if ui
                            .add_enabled(
                                selected.name_overridden,
                                egui::Button::new("Reset to Source Name"),
                            )
                            .clicked()
                        {
                            action = Some(AssetInspectorAction::ResetSub {
                                id: selected.id.clone(),
                            });
                        }
                        ui.label(format!("Type: {}", imported_sub_asset_label(selected.kind)));
                        ui.label("Source Name");
                        ui.monospace(&selected.source_name);
                        ui.label("Stable ID");
                        ui.monospace(&selected.id);
                        ui.small("The stable ID and every existing reference remain unchanged.");

                        if sub_asset_supports_override(selected.kind) {
                            ui.separator();
                            ui.heading("Model-level Override");
                            let mut selected_target = selected.override_target.as_deref().and_then(
                                |id| {
                                    AssetId::from_stable_id(engine_authoring::StableId::new(id)).ok()
                                }
                            );
                            let before = selected_target.clone();
                            egui::ComboBox::from_id_salt(("sub_asset_override", &selected.id))
                                .selected_text(match &selected.override_target {
                                    Some(_) => selected
                                        .override_target_name
                                        .as_deref()
                                        .unwrap_or("(missing override asset)"),
                                    None => "Imported",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut selected_target,
                                        None,
                                        "Imported (original)",
                                    );
                                    for choice in selected.override_choices.iter() {
                                        ui.selectable_value(
                                            &mut selected_target,
                                            Some(choice.id.clone()),
                                            &choice.label,
                                        );
                                    }
                                });
                            if selected_target != before {
                                action = match selected_target {
                                    Some(target) => Some(AssetInspectorAction::SetSubAssetOverride {
                                        source_id: registered.id.clone(),
                                        id: selected.id.clone(),
                                        kind: selected.kind,
                                        target,
                                    }),
                                    None => Some(AssetInspectorAction::ResetSubAssetOverride {
                                        source_id: registered.id.clone(),
                                        id: selected.id.clone(),
                                        kind: selected.kind,
                                    }),
                                };
                            }

                            if selected.kind == engine::ImportedSubAssetKind::Material {
                                ui.horizontal(|ui| {
                                    if selected.override_target_editable_material
                                        && ui.button("Open in Material Editor").clicked()
                                        && let Some(remap_id) = &selected.override_target
                                        && let Ok(asset_id) = AssetId::from_stable_id(
                                            engine_authoring::StableId::new(remap_id),
                                        )
                                    {
                                        action = Some(AssetInspectorAction::OpenRemappedMaterial {
                                            asset_id,
                                        });
                                    }
                                    if ui.button("Duplicate Material...").clicked() {
                                        action = Some(AssetInspectorAction::ExtractSubAssetMaterial {
                                            id: selected.id.clone(),
                                        });
                                    }
                                });
                            }
                            ui.small(
                                "This override belongs to the imported model asset and affects every entity that references this sub-asset.",
                            );
                        }

                        ui.separator();
                        ui.heading("Used by Current Scene");
                        if selected.current_scene_usages.is_empty() {
                            ui.weak("No references found in the currently open scene.");
                        } else {
                            for usage in selected.current_scene_usages.iter() {
                                ui.label(format!("- {usage}"));
                            }
                        }
                    } else {
                        ui.small("Select a sub-asset above to see it here.");
                    }
                }
            }
            None => {
                ui.separator();
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "This file is not registered in asset_manifest.json.",
                );
                ui.small("Register it before editing a project display name.");
            }
        }
        action
    }

    fn select_sub_asset(&mut self, model: &AssetInspectorModel, id: String) {
        let Some(sub_asset) = model.registered.as_ref().and_then(|registered| {
            registered
                .sub_assets
                .iter()
                .find(|sub_asset| sub_asset.id == id)
        }) else {
            return;
        };
        self.selected_sub_asset = Some(id);
        self.sub_asset_name_buffer = sub_asset.display_name.clone();
        self.detail_cache = None;
    }
}

impl Default for AssetInspectorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorApp {
    pub(super) fn activate_asset_inspector(&mut self) {
        if selected_asset_path(&self.asset_browser).is_some() {
            self.asset_inspector.activate();
        }
    }

    pub(super) fn deactivate_asset_inspector(&mut self) {
        self.asset_inspector.deactivate();
    }

    pub(super) fn reconcile_sub_asset_display_names(&mut self) {
        let source_names_changed = self.asset_inspector.reconcile(&mut self.asset_manifest);
        if source_names_changed {
            let result = self.project_root.as_ref().map(|project| {
                save_sub_asset_names(
                    project,
                    &self.asset_inspector.overrides,
                    &self.asset_inspector.source_names,
                )
            });
            if let Some(Err(error)) = result {
                self.report_error("editor.sub_asset_names_save_failed", error);
            }
        }
    }

    /// Returns `true` while the Asset Browser owns the right-hand Inspector.
    pub(super) fn show_asset_inspector(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.asset_inspector.active {
            return false;
        }
        let model = build_asset_inspector_model(
            &self.asset_browser,
            &self.asset_manifest,
            &mut self.asset_inspector,
            self.session.scene(),
            self.project_root.as_ref().map(ProjectRoot::assets_root).as_deref(),
        );
        let action = self.asset_inspector.show(ui, model.as_ref());
        if let Some(action) = action {
            self.apply_asset_inspector_action(model.as_ref(), action);
        }
        true
    }

    /// Puts the committed label back in the buffer after a rejected edit.
    ///
    /// The field now commits on focus loss, so a buffer left holding the rejected text
    /// would raise the same error again on the next focus change.
    fn restore_asset_name_buffer(&mut self, model: Option<&AssetInspectorModel>) {
        if let Some(registered) = model.and_then(|model| model.registered.as_ref()) {
            self.asset_inspector
                .asset_name_buffer
                .clone_from(&registered.display_name);
        }
    }

    /// Puts the committed sub-asset label back in the buffer after a rejected edit.
    fn restore_sub_asset_name_buffer(&mut self, model: Option<&AssetInspectorModel>, id: &str) {
        if let Some(sub_asset) =
            model
                .and_then(|model| model.registered.as_ref())
                .and_then(|registered| {
                    registered
                        .sub_assets
                        .iter()
                        .find(|sub_asset| sub_asset.id == id)
                })
        {
            self.asset_inspector
                .sub_asset_name_buffer
                .clone_from(&sub_asset.display_name);
        }
    }

    fn apply_asset_inspector_action(
        &mut self,
        model: Option<&AssetInspectorModel>,
        action: AssetInspectorAction,
    ) {
        match action {
            AssetInspectorAction::Rename { id, name } => {
                let name = name.trim();
                if name.is_empty() {
                    self.restore_asset_name_buffer(model);
                    self.report_error(
                        "editor.asset_display_name_invalid",
                        "Display Name cannot be empty",
                    );
                    return;
                }
                if self.asset_manifest.iter().any(|(candidate_id, entry)| {
                    candidate_id != &id && entry.name.as_deref() == Some(name)
                }) {
                    let message = format!("Another registered asset already uses `{name}`");
                    self.restore_asset_name_buffer(model);
                    self.report_error("editor.asset_display_name_duplicate", message);
                    return;
                }
                let mut manifest = self.asset_manifest.clone();
                let Some(entry) = manifest.get_mut(&id) else {
                    self.restore_asset_name_buffer(model);
                    self.report_error(
                        "editor.asset_display_name_missing",
                        format!("Asset `{}` is no longer registered", id.as_str()),
                    );
                    return;
                };
                entry.name = Some(name.to_owned());
                match persist_asset_manifest(self.project_root.as_ref(), &manifest) {
                    Ok(()) => {
                        if let Some(watcher) = &mut self.file_watcher {
                            watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                        }
                        self.asset_manifest = manifest;
                        self.asset_inspector.asset_name_buffer = name.to_owned();
                        self.push_notification(
                            EditorNotificationLevel::Success,
                            format!("Asset display name changed to `{name}`"),
                        );
                    }
                    Err(error) => {
                        self.restore_asset_name_buffer(model);
                        self.report_error("editor.asset_display_name_save_failed", error);
                    }
                }
            }
            AssetInspectorAction::SelectSub(id) => {
                if let Some(model) = model {
                    self.asset_inspector.select_sub_asset(model, id);
                }
            }
            AssetInspectorAction::RenameSub { id, name } => {
                let name = name.trim();
                if name.is_empty() {
                    self.restore_sub_asset_name_buffer(model, &id);
                    self.report_error(
                        "editor.sub_asset_display_name_invalid",
                        "Display Name cannot be empty",
                    );
                    return;
                }
                let Some(project) = self.project_root.clone() else {
                    self.restore_sub_asset_name_buffer(model, &id);
                    return;
                };
                let mut overrides = self.asset_inspector.overrides.clone();
                overrides.insert(id.clone(), name.to_owned());
                match save_sub_asset_names(&project, &overrides, &self.asset_inspector.source_names)
                {
                    Ok(()) => {
                        self.asset_inspector.overrides = overrides;
                        self.asset_inspector.sub_asset_name_buffer = name.to_owned();
                        self.reconcile_sub_asset_display_names();
                        self.push_notification(
                            EditorNotificationLevel::Success,
                            format!("Sub-asset display name changed to `{name}`"),
                        );
                    }
                    Err(error) => {
                        self.restore_sub_asset_name_buffer(model, &id);
                        self.report_error("editor.sub_asset_name_save_failed", error);
                    }
                }
            }
            AssetInspectorAction::ResetSub { id } => {
                let Some(project) = self.project_root.as_ref() else {
                    return;
                };
                let mut overrides = self.asset_inspector.overrides.clone();
                overrides.remove(&id);
                match save_sub_asset_names(project, &overrides, &self.asset_inspector.source_names)
                {
                    Ok(()) => {
                        self.asset_inspector.overrides = overrides;
                        if let Some(source_name) =
                            self.asset_inspector.source_names.get(&id).cloned()
                        {
                            let source_ids = self
                                .asset_manifest
                                .iter()
                                .map(|(source_id, _)| source_id.clone())
                                .collect::<Vec<_>>();
                            for source_id in source_ids {
                                let Some(entry) = self.asset_manifest.get_mut(&source_id) else {
                                    continue;
                                };
                                if let Some(sub_asset) = entry
                                    .import_settings
                                    .sub_assets
                                    .iter_mut()
                                    .find(|sub_asset| sub_asset.id == id)
                                {
                                    sub_asset.name = source_name.clone();
                                    break;
                                }
                            }
                            self.asset_inspector.sub_asset_name_buffer = source_name;
                        }
                        self.push_notification(
                            EditorNotificationLevel::Success,
                            "Sub-asset display name reset to its source name".to_owned(),
                        );
                    }
                    Err(error) => self.report_error("editor.sub_asset_name_reset_failed", error),
                }
            }
            AssetInspectorAction::ExtractSubAssetMaterial { id } => {
                self.extract_sub_asset_material(model, id);
            }
            AssetInspectorAction::OpenRemappedMaterial { asset_id } => {
                self.open_remapped_material(asset_id);
            }
            AssetInspectorAction::SetSubAssetOverride {
                source_id,
                id,
                kind,
                target,
            } => {
                self.update_sub_asset_override(source_id, id, kind, Some(target));
            }
            AssetInspectorAction::ResetSubAssetOverride {
                source_id,
                id,
                kind,
            } => {
                self.update_sub_asset_override(source_id, id, kind, None);
            }
            AssetInspectorAction::OpenFolder(folder) => {
                // Opening the folder replaces the file selection this
                // Inspector was showing, so hand the panel back to the browser
                // rather than leaving a stale asset pane behind.
                if self.reveal_asset_folder_in_browser(&folder) {
                    self.deactivate_asset_inspector();
                }
            }
        }
    }

    /// Persists one model-level Material or Texture override.
    fn update_sub_asset_override(
        &mut self,
        source_id: AssetId,
        id: String,
        kind: engine::ImportedSubAssetKind,
        target: Option<AssetId>,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        if !sub_asset_supports_override(kind) {
            self.report_error(
                "editor.sub_asset_override_unsupported",
                format!("{} sub-assets cannot be overridden", imported_sub_asset_label(kind)),
            );
            return;
        }

        let mut manifest = self.asset_manifest.clone();
        if let Some(target) = target.as_ref()
            && !valid_override_target(kind, target, &manifest)
        {
            self.report_error(
                "editor.sub_asset_override_invalid_target",
                format!(
                    "`{}` is not a compatible standalone {} asset",
                    target.as_str(),
                    imported_sub_asset_label(kind)
                ),
            );
            return;
        }
        let Some(source) = manifest.get_mut(&source_id) else {
            self.report_error(
                "editor.sub_asset_override_missing_source",
                format!("source `{}` is no longer registered", source_id.as_str()),
            );
            return;
        };
        if !source
            .import_settings
            .sub_assets
            .iter()
            .any(|sub_asset| sub_asset.id == id && sub_asset.kind == kind)
        {
            self.report_error(
                "editor.sub_asset_override_missing",
                format!("sub-asset `{id}` is no longer present in the source"),
            );
            return;
        }

        let remaps = match kind {
            engine::ImportedSubAssetKind::Material => {
                &mut source.import_settings.material_remaps
            }
            engine::ImportedSubAssetKind::Texture => &mut source.import_settings.texture_remaps,
            _ => unreachable!("unsupported kinds returned above"),
        };
        let applying = target.is_some();
        match target {
            Some(target) => {
                remaps.insert(id, target.as_str().to_owned());
            }
            None => {
                remaps.remove(&id);
            }
        }

        match persist_asset_manifest(Some(&project), &manifest) {
            Ok(()) => {
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }
                self.asset_manifest = manifest;
                self.scene_view.invalidate_asset_preview();
                self.push_notification(
                    EditorNotificationLevel::Success,
                    if applying {
                        format!("{} override applied to every model instance", imported_sub_asset_label(kind))
                    } else {
                        format!("{} reverted to the imported value", imported_sub_asset_label(kind))
                    },
                );
            }
            Err(error) => self.report_error("editor.sub_asset_override_save_failed", error),
        }
    }

    /// Extracts an imported Material sub-asset (`id`) into a standalone,
    /// independently editable `.material.json` file and remaps the
    /// sub-asset ID to it (ADR 0101), so every existing reference keeps
    /// resolving through the original ID.
    fn extract_sub_asset_material(&mut self, model: Option<&AssetInspectorModel>, id: String) {
        let Some(project) = self.project_root.clone() else {
            self.report_error(
                "editor.material_extract_failed",
                "open a project before extracting a material".to_owned(),
            );
            return;
        };
        let Some(source_id) = model.and_then(|model| model.registered.as_ref()).map(|registered| registered.id.clone())
        else {
            return;
        };
        let Some(entry) = self.asset_manifest.get(&source_id) else {
            self.report_error(
                "editor.material_extract_failed",
                format!("source `{}` is no longer registered", source_id.as_str()),
            );
            return;
        };
        let Some(sub_asset) = entry
            .import_settings
            .sub_assets
            .iter()
            .find(|sub_asset| sub_asset.id == id)
        else {
            return;
        };
        let sub_asset_name = sub_asset.name.clone();
        let existing_override = entry.import_settings.material_remaps.get(&id).cloned();
        let directory = material_duplicate_directory(
            &project,
            &self.asset_manifest,
            entry,
            existing_override.as_deref(),
        );
        let material = if let Some(existing_override) = existing_override {
            match material_asset_for_override_duplication(
                &project,
                &self.asset_manifest,
                &existing_override,
            ) {
                Ok(material) => material,
                Err(error) => {
                    self.report_error("editor.material_extract_failed", error);
                    return;
                }
            }
        } else {
            let source_path = project.assets_root().join(&entry.path);
            let imported = match engine::import_model_path_with_contact_bones(
                &source_id,
                &source_path,
                &entry.import_settings.skeleton_records,
                &entry.import_settings.contact_bones,
            ) {
                Ok(imported) => imported,
                Err(error) => {
                    self.report_error(
                        "editor.material_extract_failed",
                        format!("could not re-read `{}`: {error}", source_path.display()),
                    );
                    return;
                }
            };
            let Some(material) = imported
                .materials
                .iter()
                .find(|material| material.id.as_str() == id)
                .map(|material| material.material.clone())
            else {
                self.report_error(
                    "editor.material_extract_failed",
                    "this material slot was not found in the latest import".to_owned(),
                );
                return;
            };
            material
        };
        if let Err(error) = fs::create_dir_all(&directory) {
            self.report_error(
                "editor.material_extract_failed",
                format!("could not create {}: {error}", directory.display()),
            );
            return;
        }
        let stem = asset_name_slug(&sub_asset_name);
        let path = unique_document_path(&directory, &stem, ".material.json");
        let write_result = material
            .to_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        if let Err(error) = write_result {
            self.report_error(
                "editor.material_extract_failed",
                format!("could not write {}: {error}", path.display()),
            );
            return;
        }
        let relative = path
            .strip_prefix(project.assets_root())
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.clone());
        let Some(relative_path) = asset_relative_path_string(&relative) else {
            self.report_error(
                "editor.material_extract_failed",
                "extracted material path contains non-UTF-8 characters".to_owned(),
            );
            return;
        };
        let new_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        let name = unique_asset_name(&sub_asset_name, &manifest);
        manifest.insert(
            new_id.clone(),
            engine::ManifestEntry {
                path: relative_path,
                name: Some(name),
                import_settings: engine::ImportSettings::default(),
            },
        );
        let Some(source_entry) = manifest.get_mut(&source_id) else {
            return;
        };
        source_entry
            .import_settings
            .material_remaps
            .insert(id, new_id.as_str().to_owned());
        match persist_asset_manifest(Some(&project), &manifest) {
            Ok(()) => {
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }
                self.asset_manifest = manifest;
                self.material_editor.open_material(relative, material);
                self.show_material_editor = true;
                self.scene_view.invalidate_asset_preview();
                self.push_notification(
                    EditorNotificationLevel::Success,
                    "Material duplicated; existing references now use the editable copy"
                        .to_owned(),
                );
            }
            Err(error) => self.report_error("editor.material_extract_failed", error),
        }
    }

    /// Opens the standalone Material `asset_id` in the Material Editor —
    /// used both for a freshly extracted material and for revisiting one
    /// that already has a remap.
    fn open_remapped_material(&mut self, asset_id: AssetId) {
        let Some(entry) = self.asset_manifest.get(&asset_id) else {
            self.report_error(
                "editor.material_open_failed",
                format!("`{}` is no longer registered", asset_id.as_str()),
            );
            return;
        };
        let Some(project) = self.project_root.as_ref() else {
            return;
        };
        let abs_path = project.assets_root().join(&entry.path);
        let result = fs::read_to_string(&abs_path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                engine_authoring::MaterialAsset::from_json(&json).map_err(|error| error.to_string())
            });
        match result {
            Ok(material) => {
                self.material_editor
                    .open_material(PathBuf::from(&entry.path), material);
                self.show_material_editor = true;
            }
            Err(error) => self.report_error(
                "editor.material_open_failed",
                format!("failed to open {}: {error}", abs_path.display()),
            ),
        }
    }
}

/// Selects the folder that owns the effective Material being duplicated.
fn material_duplicate_directory(
    project: &ProjectRoot,
    manifest: &engine::AssetManifest,
    source: &engine::ManifestEntry,
    override_target: Option<&str>,
) -> PathBuf {
    if let Some(target) = override_target {
        let is_builtin = matches!(
            target,
            engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID
                | engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID
                | engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID
        );
        if is_builtin {
            return project.assets_root().join("materials");
        }
        if let Ok(target_id) = AssetId::from_stable_id(engine_authoring::StableId::new(target))
            && let Some(target_entry) = manifest.get(&target_id)
        {
            let target_path = project.assets_root().join(&target_entry.path);
            return target_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| project.assets_root());
        }
    }

    let source_path = project.assets_root().join(&source.path);
    source_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.assets_root())
}

/// Loads the effective override value before making another editable copy.
fn material_asset_for_override_duplication(
    project: &ProjectRoot,
    manifest: &engine::AssetManifest,
    target: &str,
) -> Result<engine_authoring::MaterialAsset, String> {
    let builtin_color = match target {
        engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID => Some([1.0, 1.0, 1.0, 1.0]),
        engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID => Some([0.2, 0.5, 1.0, 1.0]),
        engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID => Some([0.9, 0.4, 0.1, 1.0]),
        _ => None,
    };
    if let Some([r, g, b, a]) = builtin_color {
        return Ok(engine_authoring::MaterialAsset {
            base_color: engine_authoring::material_asset::LinearRgba { r, g, b, a },
            ..engine_authoring::MaterialAsset::default()
        });
    }

    let target_id = AssetId::from_stable_id(engine_authoring::StableId::new(target))
        .map_err(|error| format!("override target `{target}` has an invalid ID: {error}"))?;
    let entry = manifest
        .get(&target_id)
        .ok_or_else(|| format!("override target `{target}` is no longer registered"))?;
    let path = project.assets_root().join(&entry.path);
    fs::read_to_string(&path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))
        .and_then(|json| {
            engine_authoring::MaterialAsset::from_json(&json)
                .map_err(|error| format!("could not parse `{}`: {error}", path.display()))
        })
}

/// Reports a primary press that this panel actually owns.
///
/// The layer check is required, not cosmetic: combo box popups, context menus,
/// and tooltips float above the docks in their own areas, so a purely
/// geometric hit test hands their clicks to whichever panel happens to sit
/// underneath. That let the Add Component popup activate the Asset Inspector
/// mid-click, which replaced the Entity Inspector before the popup button
/// could report the release.
pub(super) fn panel_received_primary_click(ui: &egui::Ui) -> bool {
    ui.input(|input| input.pointer.primary_pressed())
        && ui.ctx().rect_contains_pointer(ui.layer_id(), ui.max_rect())
}

/// Shows one asset's path with every folder step clickable.
///
/// The file name itself is inert: the row for it is already the Asset Browser
/// selection that produced this Inspector.
fn show_asset_path_navigation(
    ui: &mut egui::Ui,
    relative_path: &Path,
) -> Option<AssetInspectorAction> {
    let mut action = None;
    let folder = relative_path.parent().unwrap_or(Path::new(""));
    let file_name = relative_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        for breadcrumb in crate::asset_browser::folder_breadcrumbs(folder) {
            if ui
                .link(egui::RichText::new(&breadcrumb.label).monospace())
                .on_hover_text("Open this folder in the Asset Browser")
                .clicked()
            {
                action = Some(AssetInspectorAction::OpenFolder(breadcrumb.folder));
            }
            ui.label("/");
        }
        ui.monospace(file_name);
    });
    action
}

fn selected_asset_path(browser: &AssetBrowser) -> Option<PathBuf> {
    let mut selected = browser.selected_paths();
    let first = selected.next()?.clone();
    selected.next().is_none().then_some(first)
}

fn build_asset_inspector_model(
    browser: &AssetBrowser,
    manifest: &engine::AssetManifest,
    state: &mut AssetInspectorState,
    scene: Option<&AuthoringScene>,
    assets_root: Option<&Path>,
) -> Option<AssetInspectorModel> {
    let relative_path = selected_asset_path(browser)?;
    let kind = browser
        .entries()
        .iter()
        .find(|entry| entry.relative_path == relative_path)
        .map(|entry| entry.kind)?;
    let manifest_path = relative_path.to_string_lossy().replace('\\', "/");
    let registered = manifest
        .iter()
        .find(|(_, entry)| entry.path.replace('\\', "/") == manifest_path)
        .map(|(id, entry)| {
            refresh_selected_sub_asset_detail_cache(
                state,
                entry,
                manifest,
                scene,
                assets_root,
            );
            RegisteredAssetInspectorModel {
                id: id.clone(),
                display_name: entry.name.clone().unwrap_or_else(|| entry.path.clone()),
                sub_assets: entry
                    .import_settings
                    .sub_assets
                    .iter()
                    .map(|sub_asset| {
                    let override_target = sub_asset_override_target(entry, sub_asset).cloned();
                    let override_target_id = override_target.as_deref().and_then(|target| {
                        AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
                    });
                    let override_target_name = override_target_id
                        .as_ref()
                        .and_then(|target| override_target_label(target, manifest));
                    let selected_detail = state
                        .detail_cache
                        .as_ref()
                        .filter(|detail| detail.sub_asset_id == sub_asset.id);
                    SubAssetInspectorModel {
                        id: sub_asset.id.clone(),
                        kind: sub_asset.kind,
                        source_name: state
                            .source_names
                            .get(&sub_asset.id)
                            .cloned()
                            .unwrap_or_else(|| sub_asset.name.clone()),
                        display_name: sub_asset.name.clone(),
                        name_overridden: state.overrides.contains_key(&sub_asset.id),
                        override_target,
                        override_target_name,
                        override_target_editable_material: sub_asset.kind
                            == engine::ImportedSubAssetKind::Material
                            && override_target_id
                                .as_ref()
                                .is_some_and(|target| manifest.get(target).is_some()),
                        override_choices: selected_detail
                            .map(|detail| detail.override_choices.clone())
                            .unwrap_or_default(),
                        current_scene_usages: selected_detail
                            .map(|detail| detail.current_scene_usages.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect(),
            }
        });
    Some(AssetInspectorModel {
        relative_path,
        kind,
        registered,
    })
}

/// Refreshes expensive detail data only when its actual inputs changed.
fn refresh_selected_sub_asset_detail_cache(
    state: &mut AssetInspectorState,
    source: &engine::ManifestEntry,
    manifest: &engine::AssetManifest,
    scene: Option<&AuthoringScene>,
    assets_root: Option<&Path>,
) {
    let Some(selected_id) = state.selected_sub_asset.as_deref() else {
        state.detail_cache = None;
        return;
    };
    let Some(selected) = source
        .import_settings
        .sub_assets
        .iter()
        .find(|sub_asset| sub_asset.id == selected_id)
    else {
        state.detail_cache = None;
        return;
    };
    let manifest_revision = manifest.revision();
    let scene_revision = scene.map(AuthoringScene::revision);
    let assets_root = assets_root.map(Path::to_path_buf);
    let cache_is_current = state.detail_cache.as_ref().is_some_and(|cache| {
        cache.sub_asset_id == selected.id
            && cache.manifest_revision == manifest_revision
            && cache.scene_revision == scene_revision
            && cache.assets_root == assets_root
    });
    if cache_is_current {
        return;
    }

    let override_target = sub_asset_override_target(source, selected)
        .and_then(|target| AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok());
    state.detail_cache = Some(SubAssetDetailCache {
        sub_asset_id: selected.id.clone(),
        manifest_revision,
        scene_revision,
        assets_root: assets_root.clone(),
        override_choices: Arc::new(override_choices_for_kind(
            selected.kind,
            manifest,
            assets_root.as_deref(),
        )),
        current_scene_usages: Arc::new(current_scene_asset_usages(
            scene,
            &selected.id,
            override_target.as_ref(),
        )),
    });
}

fn sub_asset_supports_override(kind: engine::ImportedSubAssetKind) -> bool {
    matches!(
        kind,
        engine::ImportedSubAssetKind::Material | engine::ImportedSubAssetKind::Texture
    )
}

pub(super) fn sub_asset_override_target<'a>(
    entry: &'a engine::ManifestEntry,
    sub_asset: &engine::ImportedSubAsset,
) -> Option<&'a String> {
    match sub_asset.kind {
        engine::ImportedSubAssetKind::Material => {
            entry.import_settings.material_remaps.get(&sub_asset.id)
        }
        engine::ImportedSubAssetKind::Texture => {
            entry.import_settings.texture_remaps.get(&sub_asset.id)
        }
        _ => None,
    }
}

fn override_choices_for_kind(
    kind: engine::ImportedSubAssetKind,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AssetChoice> {
    let requested = match kind {
        engine::ImportedSubAssetKind::Material => engine::AssetKind::Material,
        engine::ImportedSubAssetKind::Texture => engine::AssetKind::Texture,
        _ => return Vec::new(),
    };
    asset_choices_for_kind(requested, manifest, assets_root)
        .into_iter()
        .filter(|choice| valid_override_target(kind, &choice.id, manifest))
        .collect()
}

fn valid_override_target(
    kind: engine::ImportedSubAssetKind,
    target: &AssetId,
    manifest: &engine::AssetManifest,
) -> bool {
    if kind == engine::ImportedSubAssetKind::Material
        && matches!(
            target.as_str(),
            engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID
                | engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID
                | engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID
        )
    {
        return true;
    }
    let Some(entry) = manifest.get(target) else {
        return false;
    };
    let requested = match kind {
        engine::ImportedSubAssetKind::Material => engine::AssetKind::Material,
        engine::ImportedSubAssetKind::Texture => engine::AssetKind::Texture,
        _ => return false,
    };
    engine::asset_path_matches_kind(requested, Path::new(&entry.path))
}

pub(super) fn override_target_label(
    target: &AssetId,
    manifest: &engine::AssetManifest,
) -> Option<String> {
    let builtin = match target.as_str() {
        engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID => Some("Built-in White"),
        engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID => Some("Built-in Blue"),
        engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID => Some("Built-in Orange"),
        _ => None,
    };
    builtin.map(str::to_owned).or_else(|| {
        manifest
            .get(target)
            .map(|entry| entry.name.clone().unwrap_or_else(|| entry.path.clone()))
    })
}

fn current_scene_asset_usages(
    scene: Option<&AuthoringScene>,
    source_id: &str,
    override_target: Option<&AssetId>,
) -> Vec<String> {
    let Ok(source_id) = AssetId::from_stable_id(engine_authoring::StableId::new(source_id)) else {
        return Vec::new();
    };
    let Some(scene) = scene else {
        return Vec::new();
    };
    let mut usages = Vec::new();
    for (_, entity) in scene.entities() {
        let entity_label = if entity.display_name.trim().is_empty() {
            &entity.name
        } else {
            &entity.display_name
        };
        for (component_type, value) in &entity.components {
            let relation = if value_references_asset(value, &source_id) {
                Some("model override applies")
            } else if override_target
                .is_some_and(|target| value_references_asset(value, target))
            {
                Some("override target used directly")
            } else {
                None
            };
            if let Some(relation) = relation {
                usages.push(format!(
                    "{entity_label} / {} ({relation})",
                    component_type.as_str()
                ));
            }
        }
    }
    usages
}

fn value_references_asset(value: &Value, target: &AssetId) -> bool {
    match value {
        Value::AssetRef(id) => id == target,
        Value::Array(values) => values
            .iter()
            .any(|value| value_references_asset(value, target)),
        Value::Object(fields) => fields
            .values()
            .any(|value| value_references_asset(value, target)),
        Value::Null
        | Value::Bool(_)
        | Value::I64(_)
        | Value::U64(_)
        | Value::F64(_)
        | Value::String(_)
        | Value::EntityRef(_) => false,
    }
}

fn persist_asset_manifest(
    project: Option<&ProjectRoot>,
    manifest: &engine::AssetManifest,
) -> Result<(), String> {
    let project = project.ok_or_else(|| "No project is open".to_owned())?;
    let json = manifest
        .to_canonical_json()
        .map_err(|error| error.to_string())?;
    replace_file_contents(&project.path().join("asset_manifest.json"), &json)
        .map_err(|error| error.to_string())
}

fn load_sub_asset_names(project: &ProjectRoot) -> Result<PersistedSubAssetNames, String> {
    let path = project.path().join(SUB_ASSET_NAMES_PATH);
    if !path.is_file() {
        return Ok(PersistedSubAssetNames {
            schema_version: SUB_ASSET_NAMES_SCHEMA_VERSION,
            ..PersistedSubAssetNames::default()
        });
    }
    let json = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let persisted: PersistedSubAssetNames = serde_json::from_str(&json)
        .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
    if persisted.schema_version != SUB_ASSET_NAMES_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported sub-asset name schema {} in {}",
            persisted.schema_version,
            path.display()
        ));
    }
    Ok(persisted)
}

fn save_sub_asset_names(
    project: &ProjectRoot,
    names: &BTreeMap<String, String>,
    source_names: &BTreeMap<String, String>,
) -> Result<(), String> {
    let path = project.path().join(SUB_ASSET_NAMES_PATH);
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let persisted = PersistedSubAssetNames {
        schema_version: SUB_ASSET_NAMES_SCHEMA_VERSION,
        names: names.clone(),
        source_names: source_names.clone(),
    };
    let mut json = serde_json::to_string_pretty(&persisted).map_err(|error| error.to_string())?;
    json.push('\n');
    replace_file_contents(&path, &json).map_err(|error| error.to_string())
}

fn imported_sub_asset_badge(kind: engine::ImportedSubAssetKind) -> &'static str {
    match kind {
        engine::ImportedSubAssetKind::Mesh => "[mesh]",
        engine::ImportedSubAssetKind::Material => "[mat]",
        engine::ImportedSubAssetKind::Texture => "[tex]",
        engine::ImportedSubAssetKind::Skeleton => "[skeleton]",
        engine::ImportedSubAssetKind::Skin => "[skin]",
        engine::ImportedSubAssetKind::Animation => "[clip]",
        engine::ImportedSubAssetKind::Morph => "[morph]",
        engine::ImportedSubAssetKind::RigidBodyRig => "[physics]",
    }
}

fn imported_sub_asset_label(kind: engine::ImportedSubAssetKind) -> &'static str {
    match kind {
        engine::ImportedSubAssetKind::Mesh => "Mesh",
        engine::ImportedSubAssetKind::Material => "Material",
        engine::ImportedSubAssetKind::Texture => "Texture",
        engine::ImportedSubAssetKind::Skeleton => "Skeleton",
        engine::ImportedSubAssetKind::Skin => "Skin",
        engine::ImportedSubAssetKind::Animation => "Animation Clip",
        engine::ImportedSubAssetKind::Morph => "Morph",
        engine::ImportedSubAssetKind::RigidBodyRig => "Rigid Body Rig",
    }
}

/// Kinds actually present among `sub_assets`, in first-seen order — used to
/// build the kind filter's options from only what this asset actually has.
fn present_sub_asset_kinds(
    sub_assets: &[SubAssetInspectorModel],
) -> Vec<engine::ImportedSubAssetKind> {
    let mut kinds = Vec::new();
    for sub_asset in sub_assets {
        if !kinds.contains(&sub_asset.kind) {
            kinds.push(sub_asset.kind);
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_duplicate_folder_follows_the_effective_material_owner() {
        let directory = tempfile::tempdir().expect("temporary project root");
        let project = ProjectRoot::create(
            directory.path(),
            engine_authoring::ProjectConfig {
                name: "MaterialDuplicateFolderTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        let source = engine::ManifestEntry {
            path: "characters/miku/miku.pmx".into(),
            name: Some("Miku".into()),
            import_settings: engine::ImportSettings::default(),
        };
        let override_id = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest.insert(
            override_id.clone(),
            engine::ManifestEntry {
                path: "shared/costumes/body.material.json".into(),
                name: Some("Body".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );

        assert_eq!(
            material_duplicate_directory(&project, &manifest, &source, None),
            project.assets_root().join("characters/miku")
        );
        assert_eq!(
            material_duplicate_directory(
                &project,
                &manifest,
                &source,
                Some(override_id.as_str()),
            ),
            project.assets_root().join("shared/costumes")
        );
        assert_eq!(
            material_duplicate_directory(
                &project,
                &manifest,
                &source,
                Some(engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID),
            ),
            project.assets_root().join("materials")
        );
    }

    #[test]
    fn duplicating_an_overridden_material_reads_the_effective_value() {
        let directory = tempfile::tempdir().expect("temporary project root");
        let project = ProjectRoot::create(
            directory.path(),
            engine_authoring::ProjectConfig {
                name: "MaterialDuplicateTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project fixture");
        let material = engine_authoring::MaterialAsset {
            metallic: 0.73,
            roughness: 0.19,
            ..engine_authoring::MaterialAsset::default()
        };
        let path = project.assets_root().join("materials/source.material.json");
        fs::create_dir_all(path.parent().expect("material directory"))
            .expect("material directory fixture");
        fs::write(&path, material.to_json().expect("material JSON"))
            .expect("material fixture");
        let material_id = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest.insert(
            material_id.clone(),
            engine::ManifestEntry {
                path: "materials/source.material.json".into(),
                name: Some("Source".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );

        let duplicate = material_asset_for_override_duplication(
            &project,
            &manifest,
            material_id.as_str(),
        )
        .expect("effective override material must load");
        assert_eq!(duplicate, material);

        let builtin = material_asset_for_override_duplication(
            &project,
            &manifest,
            engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID,
        )
        .expect("built-in material must be duplicable");
        assert_eq!(builtin.base_color.r, 0.2);
        assert_eq!(builtin.base_color.g, 0.5);
        assert_eq!(builtin.base_color.b, 1.0);
    }

    #[test]
    fn override_choices_include_only_compatible_standalone_assets() {
        let source = AssetId::generate();
        let imported_material = engine::imported_sub_asset_id(
            &source,
            engine::ImportedSubAssetKind::Material,
            0,
        );
        let standalone_material = AssetId::generate();
        let standalone_texture = AssetId::generate();
        let mut manifest = engine::AssetManifest::default();
        manifest.insert(
            source,
            engine::ManifestEntry {
                path: "models/hero.glb".into(),
                name: Some("Hero".into()),
                import_settings: engine::ImportSettings {
                    sub_assets: vec![engine::ImportedSubAsset {
                        id: imported_material.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Material,
                        name: "Body".into(),
                        index: 0,
                        target_model_source: None,
                    }],
                    ..engine::ImportSettings::default()
                },
            },
        );
        manifest.insert(
            standalone_material.clone(),
            engine::ManifestEntry {
                path: "materials/body.material.json".into(),
                name: Some("Body Copy".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );
        manifest.insert(
            standalone_texture.clone(),
            engine::ManifestEntry {
                path: "textures/body.png".into(),
                name: Some("Body Texture".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );

        let material_choices = override_choices_for_kind(
            engine::ImportedSubAssetKind::Material,
            &manifest,
            None,
        );
        assert!(material_choices
            .iter()
            .any(|choice| choice.id == standalone_material));
        assert!(!material_choices
            .iter()
            .any(|choice| choice.id == imported_material));
        assert!(!material_choices
            .iter()
            .any(|choice| choice.id == standalone_texture));

        let texture_choices = override_choices_for_kind(
            engine::ImportedSubAssetKind::Texture,
            &manifest,
            None,
        );
        assert_eq!(texture_choices.len(), 1);
        assert_eq!(texture_choices[0].id, standalone_texture);
    }

    #[test]
    fn nested_asset_references_are_found_for_usage_display() {
        let target = AssetId::generate();
        let value = Value::Object(BTreeMap::from([(
            "material_slots".into(),
            Value::Array(vec![Value::Object(BTreeMap::from([(
                "material".into(),
                Value::AssetRef(target.clone()),
            )]))]),
        )]));

        assert!(value_references_asset(&value, &target));
        assert!(!value_references_asset(&value, &AssetId::generate()));
    }

    fn source_with_sub_asset(name: &str) -> engine::AssetManifest {
        let source = AssetId::generate();
        let sub_asset =
            engine::imported_sub_asset_id(&source, engine::ImportedSubAssetKind::Animation, 0);
        let mut manifest = engine::AssetManifest::default();
        manifest.insert(
            source,
            engine::ManifestEntry {
                path: "characters/hero.glb".to_owned(),
                name: Some("hero".to_owned()),
                import_settings: engine::ImportSettings {
                    sub_assets: vec![engine::ImportedSubAsset {
                        id: sub_asset.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: name.to_owned(),
                        index: 0,
                        target_model_source: None,
                    }],
                    ..engine::ImportSettings::default()
                },
            },
        );
        manifest
    }

    /// Runs one headless frame with a floating popup drawn above a bottom dock
    /// and reports what `panel_received_primary_click` saw inside the dock.
    fn bottom_dock_click_under_popup(
        context: &egui::Context,
        screen: egui::Rect,
        popup: egui::Rect,
        press: Option<egui::Pos2>,
    ) -> bool {
        let events = press
            .map(|position| {
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ]
            })
            .unwrap_or_default();
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..egui::RawInput::default()
        };
        let mut observed = false;
        let _ = context.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            // Stands in for the Add Component combo box list, which egui emits
            // as a foreground area rather than as panel content.
            egui::Area::new(egui::Id::new("floating_popup"))
                .order(egui::Order::Foreground)
                .fixed_pos(popup.min)
                .show(&ctx, |ui| {
                    ui.set_min_size(popup.size());
                    ui.label("popup entry");
                });
            egui::Panel::bottom("test_bottom_dock")
                .exact_size(screen.height() * 0.5)
                .show_inside(ui, |ui| {
                    observed = panel_received_primary_click(ui);
                });
        });
        observed
    }

    #[test]
    fn a_click_inside_a_floating_popup_does_not_belong_to_the_panel_below_it() {
        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let popup = egui::Rect::from_min_size(egui::pos2(40.0, 190.0), egui::vec2(140.0, 70.0));

        // The first frame registers the popup area so the second frame can
        // resolve which layer owns the pointer, exactly as an already-open
        // combo box behaves.
        assert!(!bottom_dock_click_under_popup(
            &context, screen, popup, None
        ));

        // Pressing on the popup used to activate the Asset Inspector, which
        // replaced the Entity Inspector before the popup button could report
        // its click and made Add Component unusable.
        assert!(!bottom_dock_click_under_popup(
            &context,
            screen,
            popup,
            Some(egui::pos2(70.0, 220.0))
        ));

        // A press on the uncovered part of the dock must still reach the dock.
        assert!(bottom_dock_click_under_popup(
            &context,
            screen,
            popup,
            Some(egui::pos2(320.0, 220.0))
        ));
    }

    #[test]
    fn reconcile_preserves_source_name_and_reapplies_override_after_reimport() {
        let mut manifest = source_with_sub_asset("Take 001");
        let (source_id, sub_asset_id) = {
            let (source_id, entry) = manifest.iter().next().unwrap();
            (
                source_id.clone(),
                entry.import_settings.sub_assets[0].id.clone(),
            )
        };
        let mut state = AssetInspectorState::new();
        state
            .overrides
            .insert(sub_asset_id.clone(), "Idle".to_owned());

        state.reconcile(&mut manifest);
        let entry = manifest.iter().next().unwrap().1;
        assert_eq!(entry.import_settings.sub_assets[0].name, "Idle");
        assert_eq!(state.source_names.get(&sub_asset_id).unwrap(), "Take 001");

        let entry = manifest.get_mut(&source_id).unwrap();
        entry.import_settings.sub_assets[0].name = "Idle Source".to_owned();
        state.reconcile(&mut manifest);
        let entry = manifest.iter().next().unwrap().1;
        assert_eq!(entry.import_settings.sub_assets[0].name, "Idle");
        assert_eq!(
            state.source_names.get(&sub_asset_id).unwrap(),
            "Idle Source"
        );
    }

    #[test]
    fn name_edit_stays_pending_while_the_field_keeps_focus() {
        assert_eq!(
            name_edit_outcome(false, false, "Idle", "Take 001"),
            NameEdit::Pending
        );
    }

    #[test]
    fn losing_focus_commits_a_changed_name_and_ignores_an_unchanged_one() {
        assert_eq!(
            name_edit_outcome(true, false, "Idle", "Take 001"),
            NameEdit::Committed
        );
        assert_eq!(
            name_edit_outcome(true, false, "  Idle  ", "Idle"),
            NameEdit::Pending
        );
    }

    #[test]
    fn escape_cancels_instead_of_committing() {
        assert_eq!(
            name_edit_outcome(true, true, "Idle", "Take 001"),
            NameEdit::Cancelled
        );
    }

    #[test]
    fn persisted_names_default_missing_fields() {
        let parsed: PersistedSubAssetNames =
            serde_json::from_str("{}").expect("empty object must remain compatible");
        assert_eq!(parsed.schema_version, SUB_ASSET_NAMES_SCHEMA_VERSION);
        assert!(parsed.names.is_empty());
        assert!(parsed.source_names.is_empty());
    }
}
