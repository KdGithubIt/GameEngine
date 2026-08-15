//! Creating and opening authoring documents from the asset browser.
//!
//! Covers every document kind the browser can produce - scenes, UI documents,
//! animation graphs, animation sets, and materials - so the naming, manifest
//! registration, and open-in-workspace steps stay in one place.

use crate::ui::*;
use super::manifest::save_asset_manifest;

impl EditorApp {
    /// Opens the asset browser entry at `index` based on its [`AssetKind`].
    pub(in crate::ui) fn open_from_browser(&mut self, index: usize, context: &egui::Context) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.open_blocked_while_playing",
                    "stop Play before opening another document",
                ));
            return;
        }

        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let kind = entry.kind;

        let relative = match entry.relative_path.to_str() {
            Some(s) => s.to_string(),
            None => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.open_asset_failed",
                        "asset path contains non-UTF-8 characters",
                    ));
                return;
            }
        };

        let Some(root) = &self.project_root else {
            return;
        };

        let abs_path = match root.resolve_asset(&relative) {
            Ok(p) => p,
            Err(e) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.open_asset_failed",
                        format!("path resolution failed: {e}"),
                    ));
                return;
            }
        };

        let pending = match kind {
            AssetKind::Scene => PendingOpen::Scene(abs_path),
            AssetKind::Graph => PendingOpen::Graph(abs_path),
            AssetKind::UiDocument => PendingOpen::Ui(abs_path),
            AssetKind::GraphView => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.open_asset_view_only",
                        "open the corresponding .graph.json file instead of the .graph.view.json",
                    ));
                return;
            }
            AssetKind::Material => {
                let result = fs::read_to_string(&abs_path)
                    .map_err(|error| error.to_string())
                    .and_then(|json| {
                        engine_authoring::MaterialAsset::from_json(&json)
                            .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(material) => {
                        self.material_editor
                            .open_material(PathBuf::from(relative), material);
                        self.show_material_editor = true;
                    }
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.material_open_failed",
                                format!("failed to open {}: {error}", abs_path.display()),
                            ))
                    }
                }
                return;
            }
            AssetKind::Texture => {
                match load_texture_preview(context, &abs_path, PathBuf::from(&relative)) {
                    Ok(preview) => self.texture_preview = Some(preview),
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.texture_preview_failed",
                                format!("failed to preview {}: {error}", abs_path.display()),
                            ))
                    }
                }
                return;
            }
            AssetKind::RetargetMap => {
                self.open_retarget_map_editor(PathBuf::from(&relative), abs_path);
                return;
            }
            AssetKind::AnimationSet => {
                self.open_animation_set_editor(PathBuf::from(&relative), abs_path);
                return;
            }
            AssetKind::Mesh
            | AssetKind::AnimationClip
            | AssetKind::MotionSource
            | AssetKind::Audio
            | AssetKind::Prefab
            | AssetKind::NavMesh
            | AssetKind::Script
            | AssetKind::RustComponent
            | AssetKind::RustResource
            | AssetKind::RustSystem
            | AssetKind::RustModule => {
                if let Err(error) = open::that(&abs_path) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.external_asset_open_failed",
                            format!(
                                "the OS could not open {} for editing or preview: {error}",
                                abs_path.display()
                            ),
                        ));
                }
                return;
            }
        };
        self.request_open(pending);
    }

    /// Creates a valid UI document at a collision-free project-relative path
    /// and opens it in the visual builder.
    pub(in crate::ui) fn create_ui_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let directory = project.assets_root().join("ui");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.ui_document_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = (1_u32..)
            .map(|suffix| {
                let name = if suffix == 1 {
                    "new_ui.ui.json".to_owned()
                } else {
                    format!("new_ui_{suffix}.ui.json")
                };
                directory.join(name)
            })
            .find(|candidate| !candidate.exists())
            .unwrap_or_else(|| directory.join("new_ui_document.ui.json"));
        let document = engine_authoring::UiDocument::default();
        let result = document
            .to_json_string()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                self.ui_builder.selected_node = Some("root".to_owned());
                self.request_open(PendingOpen::Ui(path));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.ui_document_create_failed",
                    format!("could not create UI document: {error}"),
                )),
        }
    }

    /// Creates an empty scene document at a collision-free path under
    /// `assets/scenes/` and opens it (respecting the unsaved-changes flow).
    pub(in crate::ui) fn create_scene_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.scene_create_without_project",
                    "open a project before creating scenes",
                ));
            return;
        };
        let directory = project.assets_root().join("scenes");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.scene_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_scene", ".scene.json");
        let scene_json = "{\n    \"schema_version\": 1,\n    \"entities\": []\n}\n";
        match replace_file_contents(&path, scene_json) {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                self.request_open(PendingOpen::Scene(path));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.scene_create_failed",
                    format!("could not create scene: {error}"),
                )),
        }
    }

    /// Creates a registered Animation Graph with the required Entry node.
    ///
    /// Both the semantic `*.anim.graph.json` file and its editor-only sibling
    /// view are written through `EditorSession`, then the semantic asset is
    /// registered in the project manifest before it is opened in a tab.
    pub(in crate::ui) fn create_animation_graph_document(&mut self) {
        self.create_animation_graph_document_in_folder(Path::new("animation"));
    }

    /// Creates an Animation Graph below the selected Asset Browser folder.
    ///
    /// The browser supplies an asset-relative folder, so the same graph
    /// creation path can be used by the File menu and folder context menus
    /// without duplicating graph serialization or manifest registration.
    pub(in crate::ui) fn create_animation_graph_document_in_folder(&mut self, destination_folder: &Path) {
        self.create_animation_graph_document_internal(destination_folder, None);
    }

    /// Creates a Graph for one Controller, assigns it while the Scene tab is
    /// still active, and only then opens the new Graph tab.
    pub(in crate::ui) fn create_animation_graph_for_controller(&mut self, entity: EntityId) {
        self.create_animation_graph_document_internal(Path::new("animation"), Some(entity));
    }

    fn create_animation_graph_document_internal(
        &mut self,
        destination_folder: &Path,
        controller: Option<EntityId>,
    ) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_graph_create_while_playing",
                    "stop Play mode before creating an Animation Graph",
                ));
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_graph_create_without_project",
                    "open a project before creating an Animation Graph",
                ));
            return;
        };
        if !destination_folder.as_os_str().is_empty()
            && asset_relative_path_string(destination_folder).is_none()
        {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    "Animation Graph destination must be an asset-relative folder",
                ));
            return;
        }
        let directory = project.assets_root().join(destination_folder);
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }

        let path = unique_document_path(&directory, "new_animation", ".anim.graph.json");
        let Some(relative) = path
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!(
                        "could not derive an asset-relative path for {}",
                        path.display()
                    ),
                ));
            return;
        };

        let mut graph_session = EditorSession::empty_animation_graph();
        if let Err(error) = graph_session.save_as(path.clone()) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!("could not create Animation Graph: {error}"),
                ));
            return;
        }

        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        let name = unique_asset_name("new_animation", &manifest);
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(name),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            let _ = fs::remove_file(&path);
            if let Some(view_path) = crate::document::derive_view_path(&path) {
                let _ = fs::remove_file(view_path);
            }
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_manifest_save_failed",
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
                "editor.animation_graph_created",
                format!(
                    "created Animation Graph `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
        if let Some(entity) = controller {
            self.assign_animation_controller_asset_reference(entity, "graph", asset_id);
        }
        self.request_open(PendingOpen::Graph(path));
    }

    /// Creates and registers an empty Animation Set for one Animation Graph.
    ///
    /// The set starts empty because imported clip choices are project-specific.
    /// It remains a valid editing document, while controller validation keeps
    /// playback inactive until every graph motion slot has a clip binding.
    pub(in crate::ui) fn create_animation_set_document_in_folder(
        &mut self,
        graph: Option<AssetId>,
        destination_folder: &Path,
    ) {
        self.create_animation_set_document_internal(graph, destination_folder, None);
    }

    /// Creates a Set beside its Graph, assigns it to the Controller, and opens
    /// the dedicated typed Animation Set editor.
    pub(in crate::ui) fn create_animation_set_for_controller(&mut self, entity: EntityId, graph: AssetId) {
        if let Err(error) = self.resolve_animation_asset(&graph, engine::AssetKind::AnimationGraph)
        {
            self.report_error("editor.animation_set_create_failed", error);
            return;
        }
        let destination = self
            .asset_manifest
            .get(&graph)
            .and_then(|entry| Path::new(&entry.path).parent())
            .unwrap_or(Path::new("animation"))
            .to_path_buf();
        self.create_animation_set_document_internal(Some(graph), &destination, Some(entity));
    }

    fn create_animation_set_document_internal(
        &mut self,
        graph: Option<AssetId>,
        destination_folder: &Path,
        controller: Option<EntityId>,
    ) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_set_create_while_playing",
                    "stop Play mode before creating an Animation Set",
                ));
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_set_create_without_project",
                    "open a project before creating an Animation Set",
                ));
            return;
        };
        if !destination_folder.as_os_str().is_empty()
            && asset_relative_path_string(destination_folder).is_none()
        {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    "Animation Set destination must be an asset-relative folder",
                ));
            return;
        }
        let directory = project.assets_root().join(destination_folder);
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_animation", ".animset.json");
        let Some(relative) = path
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!(
                        "could not derive an asset-relative path for {}",
                        path.display()
                    ),
                ));
            return;
        };
        let document = graph
            .map(engine_authoring::AnimationSet::new)
            .unwrap_or_else(engine_authoring::AnimationSet::empty);
        let result = document
            .to_canonical_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!("could not create Animation Set: {error}"),
                ));
            return;
        }

        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(unique_asset_name("new_animation_set", &manifest)),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            let _ = fs::remove_file(&path);
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_manifest_save_failed",
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
                "editor.animation_set_created",
                format!(
                    "created Animation Set `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
        if let Some(entity) = controller {
            self.assign_animation_controller_asset_reference(entity, "animation_set", asset_id);
        }
        self.open_animation_set_editor(PathBuf::from(&relative), path);
    }

    /// Opens a registered Animation Graph after checking its manifest kind.
    pub(in crate::ui) fn open_animation_graph_asset(&mut self, asset: &AssetId) {
        match self.resolve_animation_asset(asset, engine::AssetKind::AnimationGraph) {
            Ok((_, absolute)) => self.request_open(PendingOpen::Graph(absolute)),
            Err(error) => self.report_error("editor.animation_graph_open_failed", error),
        }
    }

    /// Opens a registered Animation Set in its dedicated typed editor.
    pub(in crate::ui) fn open_animation_set_asset(&mut self, asset: &AssetId) {
        match self.resolve_animation_asset(asset, engine::AssetKind::AnimationSet) {
            Ok((relative, absolute)) => self.open_animation_set_editor(relative, absolute),
            Err(error) => self.report_error("editor.animation_set_open_failed", error),
        }
    }

    /// Returns the manifest ID of the Animation Graph open in the active tab.
    ///
    /// Graph documents store their own [`engine_authoring::GraphId`], while
    /// Animation Sets refer to the project-level [`AssetId`] registered for
    /// the graph file. Resolving through the manifest keeps the reverse lookup
    /// rename-safe without adding a second persisted reference to the graph.
    pub(in crate::ui) fn current_animation_graph_asset(&self) -> Option<AssetId> {
        if !self.session.is_animation_graph() {
            return None;
        }
        let graph_path = self.session.current_document_path()?;
        let project = self.project_root.as_ref()?;
        self.asset_manifest
            .iter()
            .find(|(_, entry)| {
                project
                    .resolve_asset(&entry.path)
                    .is_ok_and(|candidate| candidate == graph_path)
            })
            .map(|(asset, _)| asset.clone())
    }

    /// Finds Animation Sets whose persisted target is `graph`.
    ///
    /// The relation is intentionally derived from each Set's forward
    /// reference. The Graph document therefore remains independent of asset
    /// creation, deletion, and reassignment, while this editor list always
    /// reflects the saved project state.
    pub(in crate::ui) fn animation_sets_for_graph(&self, graph: &AssetId) -> Vec<AssetChoice> {
        let Some(project) = self.project_root.as_ref() else {
            return Vec::new();
        };
        let assets_root = project.assets_root();
        let mut sets = self
            .asset_manifest
            .iter()
            .filter(|(_, entry)| {
                manifest_path_matches_asset_kind(
                    engine::AssetKind::AnimationSet,
                    Path::new(&entry.path),
                    Some(assets_root.as_path()),
                )
            })
            .filter_map(|(asset, entry)| {
                let path = project.resolve_asset(&entry.path).ok()?;
                let json = fs::read_to_string(path).ok()?;
                let set = engine_authoring::AnimationSet::from_json(&json).ok()?;
                (set.graph.as_ref() == Some(graph)).then(|| AssetChoice {
                    label: entry.name.clone().unwrap_or_else(|| entry.path.clone()),
                    id: asset.clone(),
                })
            })
            .collect::<Vec<_>>();
        sets.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        sets
    }

    /// Creates an Animation Set beside its target Graph and opens it.
    pub(in crate::ui) fn create_animation_set_for_graph(&mut self, graph: AssetId) {
        if let Err(error) = self.resolve_animation_asset(&graph, engine::AssetKind::AnimationGraph)
        {
            self.report_error("editor.animation_set_create_failed", error);
            return;
        }
        let destination = self
            .asset_manifest
            .get(&graph)
            .and_then(|entry| Path::new(&entry.path).parent())
            .unwrap_or(Path::new("animation"))
            .to_path_buf();
        self.create_animation_set_document_internal(Some(graph), &destination, None);
    }

    /// Resolves one registered author-owned animation asset without accepting
    /// a missing manifest row, a mismatched suffix, or a missing file.
    fn resolve_animation_asset(
        &self,
        asset: &AssetId,
        expected: engine::AssetKind,
    ) -> Result<(PathBuf, PathBuf), String> {
        let project = self
            .project_root
            .as_ref()
            .ok_or_else(|| "no project is open".to_owned())?;
        let entry = self
            .asset_manifest
            .get(asset)
            .ok_or_else(|| format!("asset `{}` is not registered", asset.as_str()))?;
        let relative = PathBuf::from(&entry.path);
        let assets_root = project.assets_root();
        if !manifest_path_matches_asset_kind(expected, &relative, Some(assets_root.as_path())) {
            return Err(format!(
                "asset `{}` is not the expected animation asset kind",
                asset.as_str()
            ));
        }
        let absolute = project
            .resolve_asset(&entry.path)
            .map_err(|error| error.to_string())?;
        if !absolute.is_file() {
            return Err(format!("asset file `{}` does not exist", entry.path));
        }
        Ok((relative, absolute))
    }

    /// Creates a default standalone material asset under `assets/materials/`
    /// and opens it in the Material Editor.
    pub(in crate::ui) fn create_material_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.material_create_without_project",
                    "open a project before creating materials",
                ));
            return;
        };
        let directory = project.assets_root().join("materials");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_material", ".material.json");
        let material = engine_authoring::MaterialAsset::default();
        let result = material
            .to_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                let relative = path
                    .strip_prefix(project.assets_root())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|_| path.clone());
                self.material_editor.open_material(relative, material);
                self.show_material_editor = true;
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_create_failed",
                    format!("could not create material: {error}"),
                )),
        }
    }

}

/// First free `name`, `name_2`, ... path with the given multi-part suffix.
pub(in crate::ui) fn unique_document_path(directory: &Path, stem: &str, suffix: &str) -> PathBuf {
    (1_u32..)
        .map(|counter| {
            let name = if counter == 1 {
                format!("{stem}{suffix}")
            } else {
                format!("{stem}_{counter}{suffix}")
            };
            directory.join(name)
        })
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| directory.join(format!("{stem}_new{suffix}")))
}
