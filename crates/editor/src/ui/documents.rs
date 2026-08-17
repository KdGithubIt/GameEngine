//! Project-scoped document lifecycle: open, save, filesystem sync, and undo history.

use super::*;

impl EditorApp {
    /// Creates the normal project-scoped Editor workspace.
    ///
    /// The Launcher or direct `--project` bootstrap has already validated and
    /// leased this concrete [`ProjectRoot`]; recent-project selection never
    /// participates in workspace construction.
    pub fn from_project(root: ProjectRoot) -> Self {
        let mut app = Self::new(EditorSession::empty_behavior_tree());
        app.preferences = EditorPreferences::load_for(&root);
        app.session.reset(EditorSession::empty_behavior_tree());
        app.initialize_project_root(root.clone());
        if app.restore_workspace_tabs(&root) {
            app.on_active_document_changed(None);
            app.restore_preferred_selection();
        } else {
            app.open_initial_project_scene(&root);
        }
        app.sync_workspace_preferences();
        #[cfg(feature = "visual-validation")]
        app.prepare_behavior_tree_visual_validation();
        app
    }

    /// Returns the concrete project root of this normal Editor workspace.
    pub fn project_root(&self) -> &ProjectRoot {
        self.project_root
            .as_ref()
            .expect("project-scoped Editor workspace must have a ProjectRoot")
    }

    pub(super) fn window_title(&self) -> String {
        let file_name = self
            .session
            .current_document_path()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        let dirty = if self.session.is_dirty() { " *" } else { "" };
        let mode = if self.is_playing() { " [Playing]" } else { "" };
        format!("Engine Editor - {file_name}{dirty}{mode}")
    }

    fn default_save_file_name(&self) -> &'static str {
        match self.session.current_document() {
            crate::document::CurrentDocument::Scene { .. } => "untitled.scene.json",
            crate::document::CurrentDocument::Ui { .. } => "untitled.ui.json",
            crate::document::CurrentDocument::None
            | crate::document::CurrentDocument::Graph { .. } => "untitled.graph.json",
        }
    }

    /// Pushes a warning and reports `true` when Play mode blocks saving.
    ///
    /// Saving is blocked during Play so users cannot mistake a save for
    /// persisting runtime state; Save only writes the authoring document.
    fn save_blocked_while_playing(&mut self) -> bool {
        if !self.is_playing() {
            return false;
        }
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::warning(
                "editor.save_blocked_while_playing",
                "stop Play before saving; Save writes the authoring document, not runtime state",
            ));
        true
    }

    pub(super) fn save(&mut self) {
        if self.save_blocked_while_playing() {
            return;
        }
        if let Err(error) = self.session.save() {
            if matches!(error, EditorPersistError::NoDocument) {
                self.save_as();
                return;
            }
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.save_failed",
                    format!("save failed: {error}"),
                ));
        }
    }

    /// Returns whether any workspace or specialized editor owns unsaved authoring state.
    fn any_authoring_dirty(&self) -> bool {
        self.session.any_dirty()
            || self.material_editor.any_dirty()
            || self.animation_set_editor.as_ref().is_some_and(AnimationSetEditorState::is_dirty)
    }

    /// Persists every dirty working copy through its existing validated atomic adapter.
    fn save_all_authoring_documents(&mut self) -> Result<(), String> {
        self.session.save_all().map_err(|error| error.to_string())?;
        if self.animation_set_editor.as_ref().is_some_and(AnimationSetEditorState::is_dirty) {
            self.save_animation_set_document()?;
        }
        self.save_all_material_documents()?;
        Ok(())
    }

    /// Saves every dirty document, including specialized editor working copies.
    pub(super) fn save_all(&mut self) {
        if self.save_blocked_while_playing() {
            return;
        }
        if let Err(error) = self.save_all_authoring_documents() {
            self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                "editor.save_all_failed",
                format!("save all failed: {error}"),
            ));
        }
    }

    pub(super) fn save_as(&mut self) {
        if self.save_blocked_while_playing() {
            return;
        }
        let path = rfd::FileDialog::new()
            .set_file_name(self.default_save_file_name())
            .add_filter("JSON documents", &["json"])
            .save_file();
        let Some(path) = path else { return };
        match self.session.save_as(path) {
            Ok(()) => {}
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.save_failed",
                        format!("save failed: {error}"),
                    ));
            }
        }
    }

    pub(super) fn open_file(&mut self) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.open_blocked_while_playing",
                    "stop Play before opening another document",
                ));
            return;
        }

        self.request_open(PendingOpen::PickFile);
    }

    /// Starts a Launcher-coordinated project replacement.
    ///
    /// The current Editor never rebinds project-scoped state. Dirty documents
    /// are resolved here before the Launcher is allowed to select a target;
    /// the Launcher keeps this process alive until the target Editor is ready.
    pub(super) fn request_project_switch(&mut self) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.project_switch_blocked_while_playing",
                    "stop Play before switching projects",
                ));
            return;
        }
        if self.any_authoring_dirty() {
            self.pending_unsaved_action = Some(PendingUnsavedAction::SwitchProject);
        } else {
            self.activate_launcher_for_project_switch();
        }
    }

    fn activate_launcher_for_project_switch(&mut self) {
        self.persist_editor_local_state();
        let source = self.project_root().path().to_path_buf();
        if let Err(error) = engine_project_lifecycle::activate_launcher(Some(&source)) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.project_switch_launcher_failed",
                    format!("could not activate Launcher for project switch: {error}"),
                ));
        }
    }

    /// Opens one project-local document action after the Play-mode guard.
    pub(super) fn request_open(&mut self, pending: PendingOpen) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.open_blocked_while_playing",
                    "stop Play before opening another document",
                ));
            return;
        }
        self.perform_open(pending);
    }

    /// Executes a project-local document open.
    fn perform_open(&mut self, pending: PendingOpen) {
        let previous_tab = self.session.active_tab_id();
        let previous_path = self.session.current_document_path().map(Path::to_path_buf);
        let result = match pending {
            PendingOpen::PickFile => {
                let path = rfd::FileDialog::new()
                    .add_filter("JSON documents", &["json"])
                    .pick_file();
                let Some(path) = path else { return };

                let Some(kind) = workspace_document_kind(&path) else {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::warning(
                            "editor.open_unsupported_file",
                            "only .scene.json, .graph.json, and .ui.json documents can be opened",
                        ));
                    return;
                };
                self.session.open_document(kind, path)
            }
            PendingOpen::Scene(path) => self
                .session
                .open_document(WorkspaceDocumentKind::Scene, path),
            PendingOpen::Graph(path) => self
                .session
                .open_document(WorkspaceDocumentKind::Graph, path),
            PendingOpen::Ui(path) => self.session.open_document(WorkspaceDocumentKind::Ui, path),
        };

        match result {
            Ok(()) => {
                let active_tab = self.session.active_tab_id();
                if active_tab != previous_tab {
                    // Focus moved to another tab, which keeps its own state.
                    self.on_active_document_changed(Some(previous_tab));
                } else if self.session.current_document_path().map(Path::to_path_buf)
                    != previous_path
                {
                    // The same tab now holds a different document, so its
                    // selection and canvas no longer refer to anything.
                    self.on_active_document_changed(None);
                } else {
                    // Reopening the document already in front must not clear
                    // the selection the author is working with.
                    self.sync_workspace_preferences();
                }
                // A tab drawn for the first time starts without a selection;
                // the previous session's is restored when it still applies.
                if self.selected_entities.is_empty() {
                    self.restore_preferred_selection();
                }
            }
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.open_failed",
                        format!("open failed: {error}"),
                    ));
            }
        }
    }

    /// Settles editor state after the open tabs or the active tab changed.
    ///
    /// Persisting the workspace here rather than at each call site keeps the
    /// restored tab set in step with every way a tab can appear, disappear, or
    /// take focus; forgetting one call site is what used to make a reopened
    /// project show a stale document.
    ///
    /// `outgoing` names the tab the editor is leaving, whose selection and
    /// canvas are remembered until it is drawn again. Pass `None` when the
    /// state being replaced belongs to no open tab, such as when the same tab
    /// now holds a different document.
    pub(super) fn on_active_document_changed(&mut self, outgoing: Option<WorkspaceTabId>) {
        self.adopt_active_document_presentation(outgoing);
        self.sync_workspace_preferences();
    }

    /// Settles editor state after the tab `closed` was removed.
    ///
    /// `was_active` distinguishes the two cases the presentation store has to
    /// tell apart: a closed background tab leaves the drawn document alone,
    /// while closing the active tab hands the surface to a neighbor that must
    /// get its own selection back.
    pub(super) fn on_document_closed(&mut self, closed: WorkspaceTabId, was_active: bool) {
        self.adopt_presentation_after_close(closed, was_active);
        self.sync_workspace_preferences();
    }

    /// Selects the entities persisted by the previous session that the
    /// document now in front still declares.
    ///
    /// Identifiers belonging to other scenes are dropped rather than kept as
    /// a selection the Hierarchy could never show.
    fn restore_preferred_selection(&mut self) {
        self.selected_entities = self
            .preferences
            .selected_entity_ids
            .iter()
            .filter_map(|value| EntityId::from_stable_id(StableId::new(value.clone())).ok())
            .filter(|id| self.session.scene_entity(id).is_some())
            .collect();
        self.selected_entity = self.selected_entities.iter().next().cloned();
    }

    /// Copies the open tab set and the active tab into preferences.
    ///
    /// This is the only writer of those two fields, so a workspace change can
    /// never be recorded as a partial state.
    fn capture_workspace_preferences(&mut self) {
        self.preferences.open_documents = self.session.open_document_paths();
        self.preferences.last_document =
            self.session.current_document_path().map(Path::to_path_buf);
    }

    /// Persists the workspace immediately after the open tabs changed.
    ///
    /// The periodic [`Self::persist_editor_local_state`] pass is too coarse on
    /// its own: closing the editor within its interval used to drop the change
    /// and restore a stale document on the next start.
    pub(super) fn sync_workspace_preferences(&mut self) {
        self.capture_workspace_preferences();
        self.preferences.save();
    }

    pub(super) fn persist_editor_local_state(&mut self) {
        self.capture_workspace_preferences();
        self.preferences.selected_asset_folder = self.asset_browser.selected_folder().to_path_buf();
        self.preferences.selected_entity_ids = self
            .selected_entities
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        self.preferences.ui_preview_preset = self.ui_builder.preview_preset;
        self.preferences.bottom_panel_open = self.bottom_panel_open;
        self.preferences.bottom_panel_tab = match self.bottom_panel_tab {
            BottomPanelTab::Assets => "assets",
            BottomPanelTab::Console => "console",
            BottomPanelTab::Problems => "problems",
            BottomPanelTab::Input => "input",
            BottomPanelTab::Runtime => "runtime",
        }
        .to_owned();
        // Keep the ordinary editor-state persistence path consistent with the
        // immediate save performed when the Problems menu changes a code.
        self.preferences.suppressed_problem_codes = self.problems_panel.suppressed_codes();
        self.preferences.save();
    }

    /// Shows the save/discard/cancel dialog while a destructive action is pending.
    pub(super) fn show_unsaved_changes_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_unsaved_action.as_ref() else {
            return;
        };

        let message = match pending {
            PendingUnsavedAction::SwitchProject => {
                "One or more document tabs have unsaved changes. Save them before switching projects?"
            }
            PendingUnsavedAction::CloseTab(_) => {
                "This document has unsaved changes. Save it before closing the tab?"
            }
        };

        let response = egui::Modal::new(egui::Id::new("unsaved_changes_modal")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(message);
            let mut choice = None;
            control_row(ui, |ui| {
                if ui.button("Save").clicked() {
                    choice = Some(UnsavedChangesChoice::Save);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(UnsavedChangesChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(UnsavedChangesChoice::Cancel);
                }
            });
            choice
        });

        let choice = response.inner.or_else(|| {
            response
                .should_close()
                .then_some(UnsavedChangesChoice::Cancel)
        });
        match choice {
            Some(UnsavedChangesChoice::Save) => {
                if let Some(pending) = self.pending_unsaved_action.take() {
                    match pending {
                        PendingUnsavedAction::SwitchProject => {
                            if let Err(error) = self.save_all_authoring_documents() {
                                self.session
                                    .push_diagnostic(engine_authoring::Diagnostic::error(
                                        "editor.save_all_failed",
                                        format!("could not save every open document: {error}"),
                                    ));
                                // A failed save leaves at least one working copy dirty. Keep the
                                // dialog open so project replacement cannot proceed.
                                self.pending_unsaved_action =
                                    Some(PendingUnsavedAction::SwitchProject);
                            } else {
                                self.activate_launcher_for_project_switch();
                            }
                        }
                        PendingUnsavedAction::CloseTab(id) => {
                            match self.session.save_tab(id) {
                                Ok(true) => {
                                    let was_active = self.session.active_tab_id() == id;
                                    if self.session.close_if_clean(id) {
                                        self.on_document_closed(id, was_active);
                                    }
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    self.session.push_diagnostic(
                                        engine_authoring::Diagnostic::error(
                                            "editor.save_failed",
                                            format!("save failed: {error}"),
                                        ),
                                    );
                                    // Preserve the pending close so the user can retry the
                                    // save, explicitly discard, or cancel.
                                    self.pending_unsaved_action =
                                        Some(PendingUnsavedAction::CloseTab(id));
                                }
                            }
                        }
                    }
                }
            }
            Some(UnsavedChangesChoice::Discard) => {
                if let Some(pending) = self.pending_unsaved_action.take() {
                    match pending {
                        PendingUnsavedAction::SwitchProject => {
                            self.activate_launcher_for_project_switch();
                        }
                        PendingUnsavedAction::CloseTab(id) => {
                            let was_active = self.session.active_tab_id() == id;
                            if self.session.close_discarding_changes(id) {
                                self.on_document_closed(id, was_active);
                            }
                        }
                    }
                }
            }
            Some(UnsavedChangesChoice::Cancel) => {
                self.pending_unsaved_action = None;
            }
            None => {}
        }
    }

    /// Reopens the tab set the previous session left in `root`.
    ///
    /// Returns whether at least one document was restored, so the caller can
    /// fall back to the project's start scene for a first-time open.
    ///
    /// A document that was moved, deleted, or belongs to another project is
    /// skipped rather than reported: the workspace list is editor-local
    /// convenience state, not authoring data the user asked to open.
    fn restore_workspace_tabs(&mut self, root: &ProjectRoot) -> bool {
        let mut paths = self.preferences.open_documents.clone();
        // Preferences written before tab restore existed only name the active
        // document, so it doubles as the whole tab set for those projects.
        if paths.is_empty() {
            paths.extend(self.preferences.last_document.clone());
        }
        let active = self.preferences.last_document.clone();

        let mut restored = false;
        for path in paths {
            if !path.is_file() || !path.starts_with(root.path()) {
                continue;
            }
            let Some(kind) = workspace_document_kind(&path) else {
                continue;
            };
            restored |= self.session.open_document(kind, path).is_ok();
        }

        if let Some(id) = active.and_then(|path| self.session.tab_for_path(&path)) {
            self.session.activate(id);
        }
        restored
    }

    /// Opens the scene users expect to see after selecting a project.
    ///
    /// Project settings are authoritative when `start_scene` is present. When
    /// current settings omit it, the first sorted Scene asset is used
    /// as a reversible editor-only fallback without changing project files.
    fn open_initial_project_scene(&mut self, root: &ProjectRoot) {
        let configured_scene = match ProjectSettings::load(root.path()) {
            Ok(settings) => settings.start_scene,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.project_settings_load_failed",
                        format!("could not load project settings: {error}"),
                    ));
                None
            }
        };

        let fallback_scene = self
            .asset_browser
            .entries()
            .iter()
            .find(|entry| entry.kind == AssetKind::Scene)
            .map(|entry| entry.relative_path.clone());
        let relative_path = configured_scene.map(PathBuf::from).or(fallback_scene);
        let Some(relative_path) = relative_path else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.project_opened_without_scene",
                    "project opened, but it does not contain a Scene asset",
                ));
            return;
        };
        let Some(relative_path_text) = relative_path.to_str() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.project_scene_path_invalid",
                    "the project Scene path contains non-UTF-8 characters",
                ));
            return;
        };
        match root.resolve_asset(relative_path_text) {
            Ok(path) => self.perform_open(PendingOpen::Scene(path)),
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.project_scene_open_failed",
                    format!("could not resolve project Scene `{relative_path_text}`: {error}"),
                )),
        }
    }

    pub(super) fn poll_project_filesystem(&mut self, context: &egui::Context) {
        let (events, next_poll) = match self.file_watcher.as_mut() {
            Some(watcher) => (watcher.poll(), watcher.time_until_poll()),
            None => return,
        };
        context.request_repaint_after(next_poll);
        if events.is_empty() {
            return;
        }
        let game_source_changed = events.iter().any(|event| {
            event.area == FileSyncArea::GameSource
                || event
                    .relative_path
                    .starts_with(Path::new("assets/scripts/rust"))
        });
        for event in events {
            self.handle_file_sync_event(event);
        }
        if game_source_changed {
            if let Some(project) = self.project_root.clone()
                && let Err(error) = engine_authoring::refresh_game_module_indexes(&project) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.game_index_refresh_failed",
                            error.to_string(),
                        ));
                    return;
                }
            self.refresh_game_code_browser(None);
            self.request_game_build_after_edit();
        }
    }

    fn handle_file_sync_event(&mut self, event: FileSyncEvent) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        match event.area {
            FileSyncArea::Manifest => {
                let (manifest, diagnostic) = load_asset_manifest(&project);
                self.asset_manifest = manifest;
                if let Some(diagnostic) = diagnostic {
                    self.session.push_diagnostic(diagnostic);
                }
                self.refresh_scene_problems();
            }
            FileSyncArea::Assets => {
                let absolute = project.path().join(&event.relative_path);
                let material_relative = event
                    .relative_path
                    .strip_prefix(Path::new("assets"))
                    .ok()
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(Path::to_path_buf);
                let animation_set_open = self
                    .animation_set_editor
                    .as_ref()
                    .is_some_and(|state| state.absolute_path == absolute);
                let material_open = material_relative.as_ref().is_some_and(|path| {
                    self.material_editor.materials.contains_key(path)
                });
                let workspace_tab = self.session.tab_for_path(&absolute);
                let dirty_open_document = workspace_tab
                    .is_some_and(|tab_id| self.session.tab_is_dirty(tab_id))
                    || (animation_set_open
                        && self.animation_set_editor.as_ref().is_some_and(AnimationSetEditorState::is_dirty))
                    || material_relative
                        .as_ref()
                        .is_some_and(|path| material_open && self.material_editor.is_dirty(path));

                let owner = if animation_set_open {
                    Some(ExternalDocumentOwner::AnimationSet)
                } else if material_open {
                    material_relative.clone().map(ExternalDocumentOwner::Material)
                } else {
                    workspace_tab.map(ExternalDocumentOwner::Workspace)
                };
                if let Some(owner) = owner {
                    if dirty_open_document {
                        self.external_document_conflict = Some(ExternalDocumentConflict {
                            owner,
                            path: absolute.clone(),
                            kind: event.kind,
                        });
                    } else if event.kind != FileSyncKind::Removed {
                        self.reload_external_document_owner(&owner, &absolute);
                    } else {
                        self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                            "editor.file_sync.open_document_removed",
                            format!(
                                "the open document was removed externally: {}",
                                absolute.display()
                            ),
                        ));
                    }
                }
                self.asset_browser.refresh(&project.assets_root());
                match event.kind {
                    FileSyncKind::Removed => {
                        let removed_path = event
                            .relative_path
                            .strip_prefix(Path::new("assets"))
                            .ok();
                        if let Some(removed_path) = removed_path {
                            if !removed_path.as_os_str().is_empty() && !dirty_open_document {
                                self.unregister_removed_asset(&project, removed_path);
                            } else {
                                // An empty path would represent the complete
                                // assets root, and a dirty open document may
                                // still be saved back by the author. Keep the
                                // conservative orphan report in both cases.
                                self.report_orphaned_assets(&project);
                            }
                        } else {
                            // Unexpected watcher paths are not allowed to
                            // widen the automatic cleanup scope.
                            self.report_orphaned_assets(&project);
                        }
                    }
                    // A model that appears or changes imports itself, whether
                    // it arrived through the file manager, a drop, or version
                    // control (ADR 0075).
                    FileSyncKind::Created | FileSyncKind::Modified => {
                        if let Ok(relative) = event.relative_path.strip_prefix(Path::new("assets"))
                        {
                            let relative = relative.to_path_buf();
                            self.auto_import_model_source(&relative);
                        }
                    }
                }
            }
            FileSyncArea::GameSource => {}
        }
    }

    /// Warns about registered assets whose files disappeared from disk.
    ///
    /// The manifest entries are kept so a file that returns reconnects to its
    /// existing references; `Unregister Missing Assets` drops them once the
    /// author confirms the removal was intended.
    pub(super) fn report_orphaned_assets(&mut self, project: &engine_authoring::ProjectRoot) {
        let orphans = crate::asset_management::orphaned_assets(project, &self.asset_manifest);
        if orphans.is_empty() {
            return;
        }
        for orphan in &orphans {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.registered_file_missing",
                    format!(
                        "registered asset `{}` has no file at `{}`; restore it, or use Assets > Unregister Missing Assets",
                        orphan.id.as_str(),
                        orphan.path.display()
                    ),
                ));
        }
        self.refresh_scene_problems();
    }

    /// Unregisters manifest entries affected by one externally removed asset.
    ///
    /// This is the normal path for a clean external removal observed by the
    /// file watcher. Existing orphan cleanup remains available from the
    /// Project menu for missing entries that predate the current watcher
    /// session or whose removal could not be handled safely.
    fn unregister_removed_asset(
        &mut self,
        project: &engine_authoring::ProjectRoot,
        removed_path: &Path,
    ) {
        match crate::asset_management::unregister_removed_assets(
            project,
            &mut self.asset_manifest,
            removed_path,
        ) {
            Ok(removed) if removed.is_empty() => {
                // An unregistered file, a duplicate event, or a file restored
                // before the debounced event was handled needs no mutation.
            }
            Ok(removed) => {
                // Suppress the manifest event generated by this internal save;
                // the in-memory manifest already contains the same contents.
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }

                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "asset.external_remove_unregistered",
                        format!(
                            "unregistered {} asset(s) removed from `{}`",
                            removed.len(),
                            removed_path.display()
                        ),
                    ));
                self.refresh_scene_problems();
            }
            Err(error) => {
                // Keep the registration when persistence fails so the disk
                // manifest and the in-memory state remain recoverable.
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.external_remove_manifest_failed",
                        format!("could not update the asset manifest: {error}"),
                    ));
                self.report_orphaned_assets(project);
            }
        }
    }

    /// Drops manifest entries whose files are gone, at the author's request.
    ///
    /// References to the removed IDs remain in their scenes and are reported
    /// as unregistered, which is what an author who deliberately deleted the
    /// file needs to see next.
    /// Adds render parts for skinned meshes the selected model's source has
    /// gained since it was placed, and reports parts whose mesh is gone
    /// (ADR 0087 5).
    pub(super) fn resync_selected_model_parts(&mut self, project: &engine_authoring::ProjectRoot) {
        let Some(model) = self.selected_entity.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.model_resync_no_selection",
                    "select the Skinned Model entity to resync",
                ));
            return;
        };
        let Some(skeleton) = self.session.model_skeleton(&model) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.model_resync_not_a_model",
                    "the selected entity has no Skinned Model",
                ));
            return;
        };
        let Some((source, entry)) = self
            .asset_manifest
            .imported_sub_asset(&skeleton)
            .map(|(source, entry, _)| (source.clone(), entry.clone()))
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.model_resync_unresolved_source",
                    "this model's skeleton is not part of a registered model source",
                ));
            return;
        };
        let imported = match engine::import_gltf_path(
            &source,
            &project.assets_root().join(&entry.path),
            &entry.import_settings.skeleton_records,
        ) {
            Ok(imported) => imported,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.model_resync_import_failed",
                        format!("could not read the model source: {error}"),
                    ));
                return;
            }
        };

        let parts = self.session.model_render_parts(&model);
        let sync = engine::model_part_sync(&imported, &skeleton, &parts);
        for stale in &sync.stale {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "scene.model_parts_out_of_sync",
                    format!(
                        "render part `{}` draws a mesh this source no longer declares; it was kept, not deleted",
                        stale.as_str()
                    ),
                ));
        }
        if sync.is_in_sync() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.model_resync_none",
                    "this model already matches its source",
                ));
            return;
        }
        match self.session.resync_model_render_parts(model, &sync) {
            Ok(added) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.model_resync_succeeded",
                        format!("added {added} render part(s)"),
                    ));
                self.refresh_scene_problems();
            }
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.model_resync_failed",
                        format!("render parts could not be added: {error}"),
                    ));
            }
        }
    }

    pub(super) fn unregister_missing_assets(&mut self, project: &engine_authoring::ProjectRoot) {
        match crate::asset_management::unregister_orphaned_assets(project, &mut self.asset_manifest)
        {
            Ok(orphans) if orphans.is_empty() => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "asset.unregister_missing_none",
                        "every registered asset still has its file",
                    ));
            }
            Ok(orphans) => {
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "asset.unregister_missing_succeeded",
                        format!("unregistered {} asset(s) with no file", orphans.len()),
                    ));
                self.refresh_scene_problems();
            }
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.unregister_missing_failed",
                        format!("could not update the asset manifest: {error}"),
                    ));
            }
        }
    }

    fn reload_external_document(&mut self, tab_id: WorkspaceTabId, path: &Path) {
        let result = self.session.reload_tab(tab_id);
        match result {
            Ok(()) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.file_sync.document_reloaded",
                    format!("reloaded external change from {}", path.display()),
                )),
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.file_sync.reload_failed",
                    format!("could not reload {}: {error}", path.display()),
                )),
        }
    }

    fn reload_external_document_owner(
        &mut self,
        owner: &ExternalDocumentOwner,
        path: &Path,
    ) {
        let result = match owner {
            ExternalDocumentOwner::Workspace(tab_id) => {
                self.reload_external_document(*tab_id, path);
                return;
            }
            ExternalDocumentOwner::AnimationSet => fs::read_to_string(path)
                .map_err(|error| error.to_string())
                .and_then(|json| engine_authoring::AnimationSet::from_json(&json).map_err(|error| error.to_string()))
                .and_then(|document| {
                    self.animation_set_editor
                        .as_mut()
                        .ok_or_else(|| "Animation Set editor is closed".to_owned())
                        .map(|state| state.reload_discarding_changes(document))
                }),
            ExternalDocumentOwner::Material(relative) => fs::read_to_string(path)
                .map_err(|error| error.to_string())
                .and_then(|json| engine_authoring::MaterialAsset::from_json(&json).map_err(|error| error.to_string()))
                .and_then(|material| {
                    self.material_editor
                        .reload_discarding_changes(relative, material)
                        .then_some(())
                        .ok_or_else(|| format!("material {} is no longer open", relative.display()))
                }),
        };
        match result {
            Ok(()) => {
                self.scene_view.invalidate_asset_preview();
                self.refresh_scene_problems();
                self.session.push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.file_sync.document_reloaded",
                    format!("reloaded external change from {}", path.display()),
                ));
            }
            Err(error) => self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                "editor.file_sync.reload_failed",
                format!("could not reload {}: {error}", path.display()),
            )),
        }
    }

    pub(super) fn show_external_document_conflict(&mut self, context: &egui::Context) {
        let Some(conflict) = self.external_document_conflict.as_ref() else {
            return;
        };
        let path = conflict.path.clone();
        let owner = conflict.owner.clone();
        let kind = conflict.kind;
        let mut keep = false;
        let mut reload = false;
        egui::Modal::new(egui::Id::new("external_document_conflict")).show(context, |ui| {
            ui.heading("Document changed on disk");
            ui.label(format!("{} was {kind:?} externally.", path.display()));
            ui.label(
                "The editor version has unsaved changes. Choose which complete version to keep.",
            );
            control_row(ui, |ui| {
                keep = ui.button("Keep Editor Version").clicked();
                reload = ui
                    .add_enabled(
                        kind != FileSyncKind::Removed,
                        egui::Button::new("Reload Disk Version"),
                    )
                    .clicked();
            });
        });
        if keep {
            self.external_document_conflict = None;
        } else if reload {
            self.external_document_conflict = None;
            self.reload_external_document_owner(&owner, &path);
        }
    }

    pub(super) fn apply_undo(&mut self) {
        if self.is_playing() {
            return;
        }
        if self.session.undo() {
            self.property_node = None;
            self.prune_dead_selection();
            self.sync_property_buffer();
        }
    }

    pub(super) fn apply_redo(&mut self) {
        if self.is_playing() {
            return;
        }
        if self.session.redo() {
            self.property_node = None;
            self.prune_dead_selection();
            self.sync_property_buffer();
        }
    }

    /// Removes entities that no longer exist (after undo/redo) from the
    /// selection so Hierarchy highlights and multi-entity commands never
    /// target dead IDs.
    pub(super) fn prune_dead_selection(&mut self) {
        let dead: Vec<_> = self
            .selected_entities
            .iter()
            .filter(|id| self.session.scene_entity(id).is_none())
            .cloned()
            .collect();
        for id in dead {
            self.remove_entity_from_selection(&id);
        }
        if self
            .selected_entity
            .as_ref()
            .is_some_and(|id| self.session.scene_entity(id).is_none())
        {
            self.selected_entity = self.selected_entities.iter().next_back().cloned();
        }
    }
}

/// Document open deferred until the unsaved-changes dialog is resolved.
pub(super) enum PendingOpen {
    /// Show the file picker once the dialog resolves.
    PickFile,
    /// Open this resolved `*.scene.json` document.
    Scene(PathBuf),
    /// Open this resolved `*.graph.json` document.
    Graph(PathBuf),
    /// Open this resolved `*.ui.json` document.
    Ui(PathBuf),
}

/// Action deferred until the user resolves unsaved document changes.
pub(super) enum PendingUnsavedAction {
    /// Activate the Launcher to choose/create the replacement project.
    SwitchProject,
    /// Close one document tab after saving or explicitly discarding it.
    CloseTab(WorkspaceTabId),
}

/// User choice in the unsaved-changes dialog.
#[derive(Clone, Copy)]
enum UnsavedChangesChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone)]
enum ExternalDocumentOwner {
    Workspace(WorkspaceTabId),
    AnimationSet,
    Material(PathBuf),
}

pub(super) struct ExternalDocumentConflict {
    owner: ExternalDocumentOwner,
    path: PathBuf,
    kind: FileSyncKind,
}

pub(super) fn newest_file_time(root: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        let modified = if path.is_dir() {
            newest_file_time(&path)
        } else {
            fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
        };
        if let Some(modified) = modified {
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    }

    newest
}

/// Maps an authoring document path to the workspace tab kind that opens it.
///
/// Returns `None` for any other file, including a `.json` asset the workspace
/// has no editor for.
fn workspace_document_kind(path: &Path) -> Option<WorkspaceDocumentKind> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if file_name.ends_with(".scene.json") {
        Some(WorkspaceDocumentKind::Scene)
    } else if file_name.ends_with(".graph.json") {
        Some(WorkspaceDocumentKind::Graph)
    } else if file_name.ends_with(".ui.json") {
        Some(WorkspaceDocumentKind::Ui)
    } else {
        None
    }
}
