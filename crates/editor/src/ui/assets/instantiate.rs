//! Placing registered assets into the open scene, and creating prefabs.
//!
//! Both directions of the scene/asset boundary live here: spawning a mesh,
//! model source, or prefab into the current scene, and writing a selected
//! Entity back out as a prefab asset.

use crate::ui::*;
use super::manifest::save_asset_manifest;

impl EditorApp {
    /// Creates an entity from a registered mesh asset via the context menu,
    /// mirroring the Scene View drop behavior.
    pub(in crate::ui) fn add_mesh_asset_to_scene(&mut self, index: usize) {
        let Some(entry) = self.asset_browser.entries().get(index).cloned() else {
            return;
        };
        let Some((asset_id, _)) = self.manifest_entry_for(&entry.relative_path) else {
            self.push_notification(
                EditorNotificationLevel::Info,
                "Register the asset before adding it to the scene".into(),
            );
            return;
        };
        match self.session.create_entity_from_mesh_asset(asset_id, None) {
            Ok(entity) => {
                self.select_single_entity(Some(entity));
                self.refresh_scene_problems();
            }
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    pub(in crate::ui) fn instantiate_prefab_from_browser(&mut self, index: usize) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        if entry.kind != AssetKind::Prefab {
            return;
        }
        let source = project.assets_root().join(&entry.relative_path);
        match crate::prefab_workflow::instantiate_prefab(
            &mut self.session,
            &source,
            self.selected_entity.clone(),
        ) {
            Ok(root) => {
                self.select_single_entity(Some(root));
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_instantiate_failed",
                    error.to_string(),
                )),
        }
    }

    /// Instantiates the prefab generated for a registered glTF/GLB source.
    ///
    /// The source row is what the author places (ADR 0075), so this resolves
    /// the hidden artifact on their behalf and explains the two states where
    /// there is nothing to place yet: an import that has not run, and a
    /// document that draws nothing.
    pub(in crate::ui) fn instantiate_model_source(&mut self, asset_id: &engine_authoring::AssetId) {
        self.instantiate_model_source_under(asset_id, self.selected_entity.clone());
    }

    /// Instantiates a model's generated prefab under an explicit parent.
    pub(in crate::ui) fn instantiate_model_source_under(
        &mut self,
        asset_id: &engine_authoring::AssetId,
        parent: Option<EntityId>,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let relative = self
            .asset_manifest
            .get(asset_id)
            .and_then(|entry| entry.import_settings.generated_prefab.clone());
        let Some(relative) = relative else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.model_not_imported_yet",
                    format!(
                        "`{}` has no placeable content yet; wait for its import to finish, or use Reimport",
                        asset_id.as_str()
                    ),
                ));
            return;
        };
        let source = project.path().join(&relative);
        if !source.is_file() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.generated_prefab_missing",
                    format!(
                        "the generated content for `{}` is missing; use Reimport to rebuild it",
                        asset_id.as_str()
                    ),
                ));
            return;
        }
        match crate::prefab_workflow::instantiate_prefab(&mut self.session, &source, parent) {
            Ok(root) => {
                // A model arrives as a whole subtree; folding it keeps the
                // hierarchy reading as one placed object until the author
                // opens it.
                self.collapsed_entities.insert(root.clone());
                self.select_single_entity(Some(root));
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_instantiate_failed",
                    error.to_string(),
                )),
        }
    }

    pub(in crate::ui) fn create_prefab_from_selected_entity(&mut self) {
        let Some(selected) = self.selected_entity.clone() else {
            return;
        };
        self.create_prefab_from_selected_entity_id(selected);
    }

    /// Opens the destination picker for one hierarchy entity and persists the
    /// resulting prefab through the same manifest registration path used by
    /// drag-and-drop creation.
    pub(in crate::ui) fn create_prefab_from_selected_entity_id(&mut self, selected: EntityId) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entity) = self.session.scene_entity(&selected) else {
            return;
        };
        let default_name = format!("{}.prefab.json", prefab_file_stem(&entity.name));
        let directory = project
            .assets_root()
            .join(self.asset_browser.selected_folder());
        let Some(destination) = rfd::FileDialog::new()
            .set_directory(directory)
            .set_file_name(default_name)
            .add_filter("Prefab", &["json"])
            .save_file()
        else {
            return;
        };
        self.create_prefab_asset(&project, std::slice::from_ref(&selected), destination);
    }

    /// Creates a prefab directly in the folder that received an Entity drop.
    ///
    /// The generated filename is collision-free and the scene entity remains
    /// unchanged. This is the safe default for drag-and-drop; replacing the
    /// entity with an instance remains an explicit future operation.
    pub(in crate::ui) fn create_prefab_from_entity_in_folder(
        &mut self,
        entity: EntityId,
        destination_folder: PathBuf,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(source_entity) = self.session.scene_entity(&entity) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    format!("entity `{entity}` is no longer present in the open scene"),
                ));
            return;
        };
        let stem = prefab_file_stem(&source_entity.name);
        let mut counter = 1_u32;
        let relative = loop {
            let filename = if counter == 1 {
                format!("{stem}.prefab.json")
            } else {
                format!("{stem}_{counter}.prefab.json")
            };
            let candidate = destination_folder.join(filename);
            if !project.assets_root().join(&candidate).exists() {
                break candidate;
            }
            counter = counter.saturating_add(1);
        };
        let Some(relative_string) = asset_relative_path_string(&relative) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    "generated prefab path contains non-UTF-8 characters",
                ));
            return;
        };
        let destination = match project.resolve_asset_for_write(&relative_string) {
            Ok(path) => path,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.prefab_create_failed",
                        format!("prefab destination is not writable: {error}"),
                    ));
                return;
            }
        };
        self.create_prefab_asset(&project, std::slice::from_ref(&entity), destination);
    }

    /// Writes a prefab and registers it in the project manifest as one editor
    /// operation. Existing files are rejected to avoid silently destroying a
    /// user's authored asset when a drag creates a same-named prefab.
    fn create_prefab_asset(
        &mut self,
        project: &ProjectRoot,
        selection: &[EntityId],
        destination: PathBuf,
    ) {
        if destination.exists() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.prefab_create_exists",
                    format!(
                        "prefab destination already exists: {}",
                        destination.display()
                    ),
                ));
            return;
        }
        let Some(relative) = destination
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_outside_assets",
                    "prefabs must be created inside the project Assets folder",
                ));
            return;
        };
        if Path::new(&relative).starts_with("scripts") {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_script_folder",
                    "script folders only accept their declared script type",
                ));
            return;
        }
        let Some(scene) = self.session.scene() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.prefab_create_no_scene",
                    "open a scene before creating a prefab",
                ));
            return;
        };
        match crate::prefab_workflow::create_prefab_from_selection(scene, selection, &destination) {
            Ok(_) => {
                let asset_id = AssetId::generate();
                let mut manifest = self.asset_manifest.clone();
                let name = unique_asset_name(
                    destination
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("prefab"),
                    &manifest,
                );
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
                            "editor.prefab_manifest_save_failed",
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
                        "editor.prefab_created",
                        format!("created prefab `{relative}` as `{}`", asset_id.as_str()),
                    ));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    error.to_string(),
                )),
        }
    }

}

/// Project-relative location of the prefab generated for one model source.
///
/// Keeping artifacts under `.engine/` puts them outside the Asset Browser,
/// which scans only `assets/`, so one model stays one row in the browser
/// (ADR 0075). Naming the file after the source asset ID means a reimport
/// overwrites the same artifact instead of accumulating one per import, and
/// survives renaming or moving the source file.
pub(in crate::ui) fn generated_prefab_relative_path(source_id: &engine_authoring::AssetId) -> PathBuf {
    Path::new(".engine")
        .join("imported")
        .join(format!("{}.prefab.json", source_id.as_str()))
}

/// Writes the prefab regenerated from one glTF/GLB source and returns its
/// project-relative path (ADR 0075).
///
/// The file is import output, not authoring data: it is rewritten on every
/// successful import and is deliberately absent from the asset manifest.
/// Authors keep their own copies by instantiating it and saving a separate
/// prefab under `assets/`.
pub(super) fn write_generated_prefab(
    project_root: &ProjectRoot,
    source_id: &engine_authoring::AssetId,
    prefab: &engine_authoring::prefab::PrefabAsset,
) -> Result<String, String> {
    let relative = generated_prefab_relative_path(source_id);
    let relative_string = relative
        .to_str()
        .ok_or_else(|| "generated prefab path is not representable".to_owned())?
        .to_owned();
    let json = prefab
        .to_json()
        .map_err(|error| format!("failed to serialize prefab: {error}"))?;
    let full_path = project_root.path().join(&relative);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    engine_authoring::persist::replace_file_contents(&full_path, &json)
        .map_err(|error| format!("failed to write {}: {error}", full_path.display()))?;
    Ok(relative_string)
}

/// Converts an entity name into a safe, readable prefab filename stem.
/// Slashes and other path separators are replaced so an Entity name can
/// never make a drag-and-drop operation escape its selected asset folder.
fn prefab_file_stem(name: &str) -> String {
    let mut stem = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            stem.push(character);
        } else {
            stem.push('_');
        }
    }
    let stem = stem.trim_matches('.').trim_matches('_');
    if stem.is_empty() {
        "new_prefab".to_owned()
    } else {
        stem.to_owned()
    }
}
