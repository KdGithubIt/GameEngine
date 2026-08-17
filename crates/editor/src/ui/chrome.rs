//! Editor chrome: menu bar, toolbar, docks, status bar, notifications, shortcuts.

use super::*;

impl EditorApp {
    /// Routes one operation failure to every surface at once: toast,
    /// Console diagnostic, and stderr so the exact text can be copied from
    /// the terminal as well.
    pub(super) fn report_error(&mut self, code: &str, message: impl Into<String>) {
        let message = message.into();
        eprintln!("[{code}] {message}");
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::error(code, message.clone()));
        self.push_notification(EditorNotificationLevel::Error, message);
    }

    pub(super) fn push_notification(&mut self, level: EditorNotificationLevel, message: String) {
        self.notifications.push(EditorNotification {
            message,
            level,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(6),
        });
    }

    /// Center-screen banner while a Play request waits on the game build.
    ///
    /// The toolbar spinner alone is easy to miss; a cargo build can take tens
    /// of seconds and the editor must not look unresponsive meanwhile.
    pub(super) fn show_play_build_overlay(&mut self, context: &egui::Context) {
        if !self.play_after_game_build || self.game_build.state() == GameBuildState::Idle {
            return;
        }
        context.request_repaint();
        let mut cancel = false;
        egui::Area::new(egui::Id::new("play_build_overlay"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(context, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(egui::Color32::from_rgba_unmultiplied(20, 26, 34, 235))
                    .show(ui, |ui| {
                        control_row(ui, |ui| {
                            ui.spinner();
                            ui.vertical(|ui| {
                                ui.strong("Building game code…");
                                ui.label("Play starts automatically when the build finishes.");
                            });
                            cancel = ui.button("Cancel Play").clicked();
                        });
                    });
            });
        if cancel {
            self.play_after_game_build = false;
        }
    }

    pub(super) fn show_notifications(&mut self, context: &egui::Context) {
        let now = std::time::Instant::now();
        self.notifications
            .retain(|notification| notification.expires_at > now);
        let Some(next_expiry) = self
            .notifications
            .iter()
            .map(|notification| notification.expires_at)
            .min()
        else {
            return;
        };

        context.request_repaint_after(next_expiry.saturating_duration_since(now));
        egui::Area::new(egui::Id::new("editor_notifications"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 76.0))
            .order(egui::Order::Foreground)
            .show(context, |ui| {
                ui.set_max_width(460.0);
                ui.vertical(|ui| {
                    for notification in self.notifications.iter().rev().take(4) {
                        let (fill, stroke) = match notification.level {
                            EditorNotificationLevel::Success => (
                                egui::Color32::from_rgb(32, 74, 52),
                                egui::Color32::from_rgb(82, 184, 122),
                            ),
                            EditorNotificationLevel::Info => (
                                egui::Color32::from_rgb(38, 54, 74),
                                egui::Color32::from_rgb(120, 170, 224),
                            ),
                            EditorNotificationLevel::Error => (
                                egui::Color32::from_rgb(88, 38, 38),
                                egui::Color32::from_rgb(224, 96, 96),
                            ),
                        };
                        egui::Frame::popup(ui.style())
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0_f32, stroke))
                            .show(ui, |ui| {
                                ui.label(&notification.message);
                            });
                        ui.add_space(4.0);
                    }
                });
            });
    }

    pub(super) fn show_project_settings_window(&mut self, context: &egui::Context) {
        if !self.show_project_settings {
            return;
        }
        let Some(panel) = self.project_settings_panel.as_mut() else {
            self.show_project_settings = false;
            return;
        };
        let mut open = self.show_project_settings;
        let mut save_requested = false;
        let mut revert_requested = false;
        egui::Window::new("Project Settings")
            .open(&mut open)
            .default_width(480.0)
            .default_height(560.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    show_project_settings_panel(panel, ui);
                });
                ui.separator();
                control_row(ui, |ui| {
                    save_requested = ui
                        .add_enabled(panel.is_dirty, egui::Button::new("Save Settings"))
                        .clicked();
                    revert_requested = ui
                        .add_enabled(panel.is_dirty, egui::Button::new("Revert"))
                        .clicked();
                });
            });
        self.show_project_settings = open;

        if save_requested {
            let settings = panel.settings.clone();
            let result = self
                .project_root
                .as_ref()
                .ok_or_else(|| "no project is open".to_owned())
                .and_then(|project| {
                    settings
                        .save(project.path())
                        .map_err(|error| error.to_string())
                });
            match result {
                Ok(()) => {
                    panel.commit();
                    self.project_layers = settings.layers;
                    self.refresh_scene_problems();
                }
                Err(error) => {
                    panel.is_dirty = true;
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.project_settings_save_failed",
                            error,
                        ));
                }
            }
        } else if revert_requested
            && let Some(project) = &self.project_root {
                match ProjectSettings::load(project.path()) {
                    Ok(settings) => {
                        self.project_layers = settings.layers.clone();
                        *panel = ProjectSettingsPanel::new(settings);
                    }
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.project_settings_reload_failed",
                                error.to_string(),
                            ))
                    }
                }
            }
    }

    /// Draws application-level commands that are independent of the selected
    /// document. Rare commands live in menus so they do not compete with the
    /// viewport for permanent screen space.
    pub(super) fn show_menu_bar(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Switch Project...").clicked() {
                    self.request_project_switch();
                    ui.close();
                }
                if ui.button("Open Document...    Ctrl+O").clicked() {
                    self.open_file();
                    ui.close();
                }
                if ui
                    .add_enabled(self.project_root.is_some(), egui::Button::new("New Scene"))
                    .on_disabled_hover_text("Open a project first")
                    .clicked()
                {
                    self.create_scene_document();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.project_root.is_some() && !self.is_playing(),
                        egui::Button::new("New Animation Graph"),
                    )
                    .on_disabled_hover_text("Open a project and stop Play mode first")
                    .clicked()
                {
                    self.create_animation_graph_document();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(!self.is_playing(), egui::Button::new("Save    Ctrl+S"))
                    .clicked()
                {
                    self.save();
                    ui.close();
                }
                if ui
                    .add_enabled(!self.is_playing(), egui::Button::new("Save All"))
                    .clicked()
                {
                    self.save_all();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !self.is_playing(),
                        egui::Button::new("Save As...    Ctrl+Shift+S"),
                    )
                    .clicked()
                {
                    self.save_as();
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(
                        !self.is_playing() && self.session.can_undo(),
                        egui::Button::new("Undo    Ctrl+Z"),
                    )
                    .clicked()
                {
                    self.apply_undo();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !self.is_playing() && self.session.can_redo(),
                        egui::Button::new("Redo    Ctrl+Y"),
                    )
                    .clicked()
                {
                    self.apply_redo();
                    ui.close();
                }
                if self.session.scene().is_some() {
                    ui.separator();
                    if ui
                        .add_enabled(
                            !self.is_playing() && self.selected_entity.is_some(),
                            egui::Button::new("Duplicate    Ctrl+D"),
                        )
                        .clicked()
                    {
                        self.duplicate_selected_entity();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.is_playing() && !self.last_duplicate_selection.is_empty(),
                            egui::Button::new("Repeat Last Duplicate    Ctrl+Shift+D"),
                        )
                        .clicked()
                    {
                        self.repeat_last_duplicate();
                        ui.close();
                    }
                    ui.menu_button("Align Selection", |ui| {
                        for (label, axis, alignment) in [
                            ("X Minimum", SceneAxis::X, SceneAlignment::Minimum),
                            ("X Center", SceneAxis::X, SceneAlignment::Center),
                            ("X Maximum", SceneAxis::X, SceneAlignment::Maximum),
                            ("Y Minimum", SceneAxis::Y, SceneAlignment::Minimum),
                            ("Y Center", SceneAxis::Y, SceneAlignment::Center),
                            ("Y Maximum", SceneAxis::Y, SceneAlignment::Maximum),
                            ("Z Minimum", SceneAxis::Z, SceneAlignment::Minimum),
                            ("Z Center", SceneAxis::Z, SceneAlignment::Center),
                            ("Z Maximum", SceneAxis::Z, SceneAlignment::Maximum),
                        ] {
                            if ui.button(label).clicked() {
                                self.align_selected(axis, alignment);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Distribute Evenly", |ui| {
                        for (label, axis) in [
                            ("Along X", SceneAxis::X),
                            ("Along Y", SceneAxis::Y),
                            ("Along Z", SceneAxis::Z),
                        ] {
                            if ui.button(label).clicked() {
                                self.distribute_selected(axis);
                                ui.close();
                            }
                        }
                    });
                    if ui
                        .add_enabled(
                            !self.is_playing() && self.selected_entity.is_some(),
                            egui::Button::new("Copy    Ctrl+C"),
                        )
                        .clicked()
                    {
                        self.copy_selected_entity();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.is_playing() && self.entity_clipboard.is_some(),
                            egui::Button::new("Paste    Ctrl+V"),
                        )
                        .clicked()
                    {
                        self.paste_entity();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.is_playing() && self.selected_entity.is_some(),
                            egui::Button::new("Delete"),
                        )
                        .clicked()
                    {
                        self.delete_selected_entity();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.is_playing()
                                && self.selected_entity.is_some()
                                && self.project_root.is_some(),
                            egui::Button::new("Create Prefab from Selection..."),
                        )
                        .clicked()
                    {
                        self.create_prefab_from_selected_entity();
                        ui.close();
                    }
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Animation Preview...").clicked() {
                    self.open_animation_preview_window();
                    ui.close();
                }
                ui.separator();
                self.bottom_panel_menu_item(ui, BottomPanelTab::Assets, "Assets");
                self.bottom_panel_menu_item(ui, BottomPanelTab::Console, "Console");
                self.bottom_panel_menu_item(ui, BottomPanelTab::Problems, "Problems");
                self.bottom_panel_menu_item(ui, BottomPanelTab::Input, "Input Debugger");
                self.bottom_panel_menu_item(ui, BottomPanelTab::Runtime, "Runtime Debugger");
                ui.separator();
                self.left_panel_menu_item(ui, LeftPanelTab::Systems, "Systems");
                if self.session.scene().is_some() {
                    self.left_panel_menu_item(ui, LeftPanelTab::Hierarchy, "Hierarchy");
                }
            });
            ui.menu_button("Project", |ui| {
                let Some(project) = self.project_root.clone() else {
                    ui.label("Open a project to access project tools");
                    return;
                };
                if ui.button("Project Settings...").clicked() {
                    self.show_project_settings = true;
                    ui.close();
                }
                if ui.button("Editor Preferences...").clicked() {
                    self.show_editor_preferences = true;
                    ui.close();
                }
                ui.separator();
                // The file watcher now unregisters clean external removals
                // automatically. This menu remains a fallback for orphaned
                // entries that predate the current watcher session or whose
                // removal was deferred because a dirty document was open.
                let orphans =
                    crate::asset_management::orphaned_assets(&project, &self.asset_manifest);
                if ui
                    .add_enabled(
                        !orphans.is_empty(),
                        egui::Button::new(format!(
                            "Unregister Missing Assets ({})",
                            orphans.len()
                        )),
                    )
                    .clicked()
                {
                    self.unregister_missing_assets(&project);
                    ui.close();
                }
                ui.separator();
                if ui.button("Resync Model Parts").clicked() {
                    self.resync_selected_model_parts(&project);
                    ui.close();
                }
                ui.separator();
                let initialized = project.game_dir().join("Cargo.toml").is_file();
                if !initialized && ui.button("Initialize Rust Game").clicked() {
                    self.initialize_rust_game(&project);
                    ui.close();
                }
                if ui
                    .add_enabled(initialized, egui::Button::new("Create Rust Script..."))
                    .clicked()
                {
                    self.show_new_rust_script = true;
                    ui.close();
                }
                if ui
                    .add_enabled(initialized, egui::Button::new("Open Rust File..."))
                    .clicked()
                {
                    self.open_rust_file(&project);
                    ui.close();
                }
                if ui
                    .add_enabled(initialized, egui::Button::new("Open Project Terminal"))
                    .clicked()
                {
                    let result = prepare_cargo_sdk_config(&project)
                        .and_then(|_| engine_sdk_root())
                        .map_err(|error| error.to_string())
                        .and_then(|sdk_root| {
                            std::process::Command::new("powershell.exe")
                                .args(["-NoExit", "-Command", "$Host.UI.RawUI.WindowTitle = 'Engine Game Project'; Write-Host 'SDK:' $env:GAMEENGINE_SDK_ROOT; Write-Host 'Validate with: cargo check --all-targets'"])
                                .current_dir(project.game_dir())
                                .env("GAMEENGINE_SDK_ROOT", sdk_root)
                                .spawn()
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        });
                    if let Err(error) = result {
                        self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.project_terminal_failed",
                            error,
                        ));
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(initialized, egui::Button::new("Copy Cargo Command"))
                    .clicked()
                {
                    match prepare_cargo_sdk_config(&project) {
                        Ok(_) => ui.ctx().copy_text("cargo check --all-targets".to_owned()),
                        Err(error) => self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.cargo_config_failed",
                            error.to_string(),
                        )),
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.session.scene().is_some() && !self.is_playing(),
                        egui::Button::new("Navigation..."),
                    )
                    .clicked()
                {
                    self.open_navigation_window();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.session.scene().is_some()
                            && !self.is_playing()
                            && !self.navigation_bake.is_running(),
                        egui::Button::new("Bake NavMesh"),
                    )
                    .clicked()
                {
                    self.bake_current_navmesh();
                    ui.close();
                }
                if self.navigation_bake.is_running()
                    && ui
                        .add_enabled(
                            !self.navigation_bake.is_cancelling(),
                            egui::Button::new("Cancel NavMesh Bake"),
                        )
                        .clicked()
                {
                    self.cancel_current_navmesh_bake();
                    ui.close();
                }
            });
            ui.menu_button("Build", |ui| {
                let can_build = self.project_root.is_some()
                    && !self.is_playing()
                    && self.game_build.state() == GameBuildState::Idle;
                if ui
                    .add_enabled(can_build, egui::Button::new("Check Rust Game"))
                    .clicked()
                {
                    self.start_game_build(GameBuildKind::Check);
                    ui.close();
                }
                if ui
                    .add_enabled(can_build, egui::Button::new("Build Now"))
                    .clicked()
                {
                    self.game_build_quiet_deadline = None;
                    self.start_game_build(GameBuildKind::Build);
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.project_root.is_some() && !self.is_playing(),
                        egui::Button::new("Package Project..."),
                    )
                    .clicked()
                {
                    self.package_open_project();
                    ui.close();
                }
                if self.game_build.state() != GameBuildState::Idle
                    && ui.button("Cancel Build").clicked()
                {
                    self.game_build.cancel();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label("Engine Editor");
                ui.label("Context-sensitive commands are available with right-click.");
            });

            if self.is_playing() && ui.button("Stop Play    Esc").clicked() {
                self.stop_play(frame.wgpu_render_state());
            }
        });
    }

    /// Draws the small set of commands used continuously during editing.
    pub(super) fn show_main_toolbar(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let has_document_tabs = !self.session.summaries().is_empty();

        show_main_toolbar_content(ui, |ui| {
            self.show_main_toolbar_actions(ui, frame);

            if has_document_tabs {
                ui.separator();
                show_toolbar_document_tab_strip(ui, |ui| self.show_document_tabs(ui));
            }
        });
    }

    /// Draws fixed document, build, and Play actions before the scrollable tabs.
    ///
    /// Keeping these actions outside the tab scroll area guarantees that Save,
    /// Play, Stop, and build progress remain reachable even when many documents
    /// are open.
    fn show_main_toolbar_actions(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if ui
            .add_enabled(!self.is_playing(), egui::Button::new("Save"))
            .on_hover_text("Save the current document (Ctrl+S)")
            .clicked()
        {
            self.save();
        }
        if ui
            .add_enabled(
                !self.is_playing() && self.session.can_undo(),
                egui::Button::new("Undo"),
            )
            .on_hover_text("Undo the last content edit (Ctrl+Z)")
            .clicked()
        {
            self.apply_undo();
        }
        if ui
            .add_enabled(
                !self.is_playing() && self.session.can_redo(),
                egui::Button::new("Redo"),
            )
            .on_hover_text("Redo the last undone edit (Ctrl+Y)")
            .clicked()
        {
            self.apply_redo();
        }

        ui.separator();
        if self.show_game_code_toolbar_control(ui) {
            ui.separator();
        }

        if self.is_playing() {
            if ui
                .button(egui::RichText::new("Stop").color(egui::Color32::LIGHT_RED))
                .on_hover_text("Stop Play (Esc)")
                .clicked()
            {
                self.stop_play(frame.wgpu_render_state());
            }

            if let Some(runtime) = &mut self.runtime_state {
                let paused = runtime.is_paused();
                if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                    runtime.set_paused(!paused);
                }
                if ui
                    .add_enabled(paused, egui::Button::new("Step"))
                    .on_hover_text("Run one fixed update and one frame update")
                    .clicked()
                {
                    runtime.request_single_step();
                }
            }

            if ui
                .add_enabled(
                    self.session.current_document_path().is_some(),
                    egui::Button::new("Reload"),
                )
                .on_hover_text("Reload the scene from disk without stopping Play")
                .clicked()
            {
                self.reload_scene(frame.wgpu_render_state());
            }
        } else {
            let build_state = self.game_build.state();
            let build_running = build_state != GameBuildState::Idle;

            if self.play_after_game_build && build_running {
                // Play was requested but the game module is still compiling.
                // Persistent feedback prevents the editor from looking idle.
                ui.spinner();
                ui.colored_label(egui::Color32::YELLOW, "Building…")
                    .on_hover_text(
                        "Game code is building; Play starts automatically when it finishes",
                    );

                if ui
                    .small_button("Cancel")
                    .on_hover_text("Keep building but do not start Play afterwards")
                    .clicked()
                {
                    self.play_after_game_build = false;
                }
            } else if ui
                .add_enabled(
                    self.session.scene().is_some() && !build_running,
                    egui::Button::new(
                        egui::RichText::new("Play").color(egui::Color32::LIGHT_GREEN),
                    ),
                )
                .on_hover_text(if build_running {
                    "Wait for the current game-code build to finish"
                } else {
                    "Run the open scene"
                })
                .clicked()
            {
                self.start_play();
            }
        }
    }

    /// Draws the project-wide Game Code build action beside Play.
    ///
    /// The compiled native module supplies both Play's gameplay systems and
    /// the Game Component schemas shown by the Inspector, so this control is
    /// intentionally owned by the application toolbar rather than by an
    /// entity-specific panel.
    fn show_game_code_toolbar_control(&mut self, ui: &mut egui::Ui) -> bool {
        let has_game_project = self
            .project_root
            .as_ref()
            .is_some_and(|project| project.game_dir().join("Cargo.toml").is_file());
        if !has_game_project || self.is_playing() {
            return false;
        }

        let state = self.game_build.state();
        let is_stale = state == GameBuildState::Idle && self.game_code_is_stale();
        let label = match state {
            GameBuildState::Idle if is_stale => "Build *",
            GameBuildState::Idle => "Build",
            GameBuildState::Checking => "Checking…",
            GameBuildState::Building => "Building…",
            GameBuildState::BuildingRelease => "Packaging…",
        };
        let text = if is_stale {
            egui::RichText::new(label).color(egui::Color32::YELLOW)
        } else {
            egui::RichText::new(label)
        };
        let hover = if is_stale {
            "Build and load changed Game Code and refresh Game Component schemas"
        } else {
            "Build and load Game Code used by Play and Game Components"
        };

        if ui
            .add_enabled(state == GameBuildState::Idle, egui::Button::new(text))
            .on_hover_text(hover)
            .clicked()
        {
            self.game_build_quiet_deadline = None;
            self.start_game_build(GameBuildKind::Build);
        }
        true
    }

    pub(super) fn show_workspace_header(&mut self, ui: &mut egui::Ui) {
        control_row(ui, |ui| {
            let title = if self.is_playing() {
                match self.preferences.play_mode_view {
                    PlayModeView::Game => "Game View",
                    PlayModeView::Scene => "Scene View (Play)",
                }
            } else if self.session.scene().is_some() {
                "Scene View"
            } else if self.session.ui_document().is_some() {
                "UI Builder"
            } else {
                "Graph"
            };
            ui.strong(title);
            ui.separator();
            let document = self
                .session
                .current_document_path()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            ui.label(document);
            if self.session.is_dirty() {
                ui.colored_label(egui::Color32::from_rgb(238, 197, 97), "Modified");
            }

            if !self.is_playing()
                && self.session.scene().is_none()
                && self.session.ui_document().is_none()
            {
                ui.separator();
                ui.menu_button("Add Node", |ui| self.show_add_node_menu(ui));
                if ui.button("Layout").clicked() {
                    let result = self.session.apply_incremental_layout();
                    self.apply_ui_result(result);
                }
                if ui
                    .button("Frame All")
                    .on_hover_text(
                        "Pan the canvas until every node is visible. Middle-drag also pans it.",
                    )
                    .clicked()
                {
                    self.canvas.request_frame_all();
                }
            }
        });
        ui.separator();
    }

    /// Draws open authoring documents as switchable workspace tabs.
    ///
    /// Tabs keep independent [`EditorSession`] values, so switching does not
    /// require saving or closing the Scene before opening UI Builder. The
    /// caller owns horizontal scrolling so this function keeps every tab on
    /// one row.
    pub(super) fn show_document_tabs(&mut self, ui: &mut egui::Ui) {
        let tabs = self.session.summaries();
        if tabs.is_empty() {
            return;
        }

        let mut activate = None;
        let mut close = None;
        ui.horizontal(|ui| {
            for tab in tabs {
                let dirty_marker = if tab.is_dirty { " ●" } else { "" };
                let title = format!("{}: {}{dirty_marker}", tab.kind.label(), tab.label);
                if ui
                    .add_enabled(
                        !self.is_playing() || tab.is_active,
                        egui::Button::selectable(tab.is_active, title),
                    )
                    .on_disabled_hover_text("Stop Play before switching documents")
                    .clicked()
                {
                    activate = Some(tab.id);
                }
                if ui
                    .add_enabled(!self.is_playing(), egui::Button::new("×").small())
                    .on_hover_text("Close tab (save modified documents first)")
                    .clicked()
                {
                    close = Some(tab.id);
                }
            }
        });

        if let Some(id) = activate {
            let previous = self.session.active_tab_id();
            if self.session.activate(id) {
                self.on_active_document_changed(Some(previous));
            }
        }
        if let Some(id) = close {
            if self.session.tab_is_dirty(id) {
                self.pending_unsaved_action = Some(PendingUnsavedAction::CloseTab(id));
            } else {
                let was_active = self.session.active_tab_id() == id;
                if self.session.close_if_clean(id) {
                    self.on_document_closed(id, was_active);
                }
            }
        }
    }

    pub(super) fn show_status_bar(&self, ui: &mut egui::Ui) {
        let project = self
            .project_root
            .as_ref()
            .and_then(|root| root.path().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No project".into());
        // Count Problems and Console diagnostics as one grouped collection so
        // mirrored persistent diagnostics do not inflate the global totals.
        let errors = self.problems_panel.active_count_with(
            engine_authoring::Severity::Error,
            self.session.diagnostics(),
        );
        let warnings = self.problems_panel.active_count_with(
            engine_authoring::Severity::Warning,
            self.session.diagnostics(),
        );
        control_row(ui, |ui| {
            ui.label(project);
            ui.separator();
            ui.label(if self.session.is_dirty() {
                "Unsaved"
            } else {
                "Saved"
            });
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(230, 92, 92),
                format!("{errors} errors"),
            );
            ui.colored_label(
                egui::Color32::from_rgb(230, 190, 78),
                format!("{warnings} warnings"),
            );
            ui.separator();
            let build_status = match self.game_build.state() {
                GameBuildState::Idle
                    if self.game_code_generation > self.built_game_code_generation =>
                {
                    if self.game_build_requested_after_edit {
                        format!("Build queued: generation {}", self.game_code_generation)
                    } else {
                        format!("Build dirty: generation {}", self.game_code_generation)
                    }
                }
                GameBuildState::Idle => format!(
                    "Build succeeded: generation {}",
                    self.built_game_code_generation
                ),
                GameBuildState::Checking => "Checking game code...".to_owned(),
                GameBuildState::Building => format!(
                    "Building generation {}...",
                    self.running_game_code_generation
                        .unwrap_or(self.game_code_generation)
                ),
                GameBuildState::BuildingRelease => "Building release...".to_owned(),
            };
            ui.label(build_status);
            if let Some(progress) = self.asset_import.progress() {
                ui.separator();
                ui.label(format!(
                    "Import: {} ({:.0}%)",
                    progress.stage,
                    progress.fraction * 100.0
                ));
            }
        });
    }

    pub(super) fn show_bottom_dock(&mut self, ui: &mut egui::Ui) {
        control_row(ui, |ui| {
            self.bottom_panel_tab_button(ui, BottomPanelTab::Assets, "Assets");
            self.bottom_panel_tab_button(ui, BottomPanelTab::Console, "Console");
            // Informational import notices are opt-in inside Problems, so the
            // tab badge counts only active errors and warnings.
            let problem_count = self.problems_panel.active_issue_count();
            self.bottom_panel_tab_button(
                ui,
                BottomPanelTab::Problems,
                &format!("Problems ({problem_count})"),
            );
            self.bottom_panel_tab_button(ui, BottomPanelTab::Input, "Input");
            self.bottom_panel_tab_button(ui, BottomPanelTab::Runtime, "Runtime");
        });
        if !self.bottom_panel_open {
            return;
        }
        ui.separator();
        // Resizable egui panels only preserve a dragged size when their
        // contents claim the available area. Without this, a short Console or
        // Asset list pulls the dock back down to its content height.
        ui.set_min_size(ui.available_size());
        match self.bottom_panel_tab {
            BottomPanelTab::Assets => {
                let pointer_over_asset_browser = ui.rect_contains_pointer(ui.max_rect());
                // OS file drops arrive through RawInput. Internal asset moves
                // continue to use AssetPathDragPayload, so the two drag
                // protocols cannot be mistaken for one another.
                let hovered_external_files = ui.ctx().input(|input| {
                    input
                        .raw
                        .hovered_files
                        .iter()
                        .filter_map(|file| file.path.clone())
                        .collect::<Vec<_>>()
                });
                // Windows does not guarantee ordinary pointer-motion events
                // while an OLE file drag owns the cursor. Since this branch is
                // only active for the visible Assets tab, every OS file drop
                // received here is unambiguously intended for the browser.
                let dropped_external_files = ui.ctx().input(|input| {
                    input
                        .raw
                        .dropped_files
                        .iter()
                        .filter_map(|file| file.path.clone())
                        .collect::<Vec<_>>()
                });
                if pointer_over_asset_browser && !hovered_external_files.is_empty() {
                    ui.colored_label(
                        egui::Color32::LIGHT_BLUE,
                        format!(
                            "Drop {} file(s) into Assets / {}",
                            hovered_external_files.len(),
                            if self.asset_browser.selected_folder().as_os_str().is_empty() {
                                "Assets".to_owned()
                            } else {
                                self.asset_browser.selected_folder().display().to_string()
                            }
                        ),
                    );
                    ui.separator();
                }
                if let Some(progress) = self.asset_import.progress().cloned() {
                    control_row(ui, |ui| {
                        ui.add(
                            egui::ProgressBar::new(progress.fraction)
                                .desired_width(180.0)
                                .text(progress.stage),
                        );
                        ui.label(
                            progress
                                .source_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                        );
                        if ui.button("Cancel Import").clicked() {
                            let _ = self.asset_import.cancel();
                        }
                    });
                    ui.separator();
                }
                let project = self.project_root.clone();
                let can_create_rust_script = project.is_some();
                // Do not wrap the complete browser in a ScrollArea. The asset
                // tree and grid own distinct scroll states, while Game Code is
                // a separate full-height sub-tab with its own scroll state.
                let action = show_project_browser(
                    ui,
                    &mut self.asset_browser,
                    &mut self.asset_search,
                    &mut self.asset_thumbnails,
                    &mut self.asset_content_scroll_reset,
                    &mut self.project_browser_tab,
                    project.as_ref(),
                    &self.asset_manifest,
                    can_create_rust_script,
                );
                match action {
                    Some(AssetBrowserAction::Open(index)) => {
                        self.open_from_browser(index, ui.ctx())
                    }
                    Some(AssetBrowserAction::Register(index)) => {
                        self.register_asset_from_browser(index)
                    }
                    Some(AssetBrowserAction::Reimport(index)) => {
                        self.reimport_asset_from_browser(index)
                    }
                    Some(AssetBrowserAction::InstantiatePrefab(index)) => {
                        self.instantiate_prefab_from_browser(index)
                    }
                    Some(AssetBrowserAction::InstantiateModel(index)) => {
                        if let Some(asset_id) = self
                            .asset_browser
                            .entries()
                            .get(index)
                            .and_then(|entry| {
                                self.asset_manifest.iter().find(|(_, registered)| {
                                    Path::new(&registered.path) == entry.relative_path
                                })
                            })
                            .map(|(id, _)| id.clone())
                        {
                            self.instantiate_model_source(&asset_id);
                        }
                    }
                    Some(AssetBrowserAction::CreatePrefabFromEntity {
                        entity,
                        destination_folder,
                    }) => self.create_prefab_from_entity_in_folder(entity, destination_folder),
                    Some(AssetBrowserAction::RenameAsset(index)) => {
                        self.begin_asset_mutation(index, AssetMutationKind::Rename)
                    }
                    Some(AssetBrowserAction::MoveAsset(index)) => {
                        self.begin_asset_mutation(index, AssetMutationKind::Move)
                    }
                    Some(AssetBrowserAction::TrashAsset(index)) => {
                        self.begin_asset_mutation(index, AssetMutationKind::Trash)
                    }
                    Some(AssetBrowserAction::NewUiDocument) => self.create_ui_document(),
                    Some(AssetBrowserAction::NewScene) => self.create_scene_document(),
                    Some(AssetBrowserAction::NewAnimationGraph { destination_folder }) => {
                        self.create_animation_graph_document_in_folder(&destination_folder)
                    }
                    Some(AssetBrowserAction::NewAnimationSet {
                        graph,
                        destination_folder,
                    }) => self.create_animation_set_document_in_folder(graph, &destination_folder),
                    Some(AssetBrowserAction::NewMaterial) => self.create_material_document(),
                    Some(AssetBrowserAction::ShowInExplorer(path)) => {
                        self.show_asset_in_explorer(&path)
                    }
                    Some(AssetBrowserAction::AddMeshToScene(index)) => {
                        self.add_mesh_asset_to_scene(index)
                    }
                    Some(AssetBrowserAction::NewRhaiScript) => {
                        self.show_new_rhai_script = true;
                    }
                    Some(AssetBrowserAction::NewRustScript) => {
                        self.show_new_rust_script = true;
                    }
                    Some(AssetBrowserAction::NewFolder) => self.begin_folder_create(),
                    Some(AssetBrowserAction::RenameFolder(path)) => {
                        self.begin_folder_mutation(path, AssetMutationKind::RenameFolder)
                    }
                    Some(AssetBrowserAction::TrashFolder(path)) => {
                        self.begin_folder_mutation(path, AssetMutationKind::TrashFolder)
                    }
                    Some(AssetBrowserAction::MoveSelectionToFolder(folder)) => {
                        self.move_selected_assets_to_folder(folder)
                    }
                    Some(AssetBrowserAction::EditImportSettings(index)) => {
                        self.open_import_settings_editor(index)
                    }
                    Some(AssetBrowserAction::CreateRetargetMap {
                        source,
                        target_source_id,
                    }) => self.create_retarget_map_from_browser(source, target_source_id),
                    None => {}
                }
                self.preferences.selected_asset_folder =
                    self.asset_browser.selected_folder().to_path_buf();
                if !dropped_external_files.is_empty() {
                    self.import_external_asset_files(dropped_external_files);
                }
            }
            BottomPanelTab::Console => {
                let console = self.console_panel.show(ui, self.session.diagnostics());
                if console.clear_requested {
                    self.session.set_diagnostics(Vec::new());
                }
                self.navigate_to_diagnostic_target(console.navigate_to);
            }
            BottomPanelTab::Problems => {
                let output = self.problems_panel.show(ui);
                if output.suppression_changed {
                    // Persist immediately so a recurring import warning stays
                    // hidden even if the editor exits before another document
                    // or panel-state save occurs.
                    self.preferences.suppressed_problem_codes =
                        self.problems_panel.suppressed_codes();
                    self.preferences.save();
                }
                if let Some(diagnostic) = output.clicked {
                    // The skeleton bind report (ADR 0077 §6, AP-5) is a
                    // detail view opened from its Problems entry rather than
                    // plain navigation, per docs/ANIMATION_PIPELINE_PLAN.md
                    // AP-5's chosen pattern.
                    if diagnostic.code == engine::SKELETON_REBIND_DIAGNOSTIC
                        && let Some(engine_authoring::DiagnosticTarget::Asset { id }) =
                            &diagnostic.target
                        {
                            self.show_skeleton_bind_report = Some(id.as_str().to_owned());
                        }
                    self.navigate_to_diagnostic_target(diagnostic.target);
                }
            }
            BottomPanelTab::Input => {
                let snapshot = self
                    .runtime_state
                    .as_ref()
                    .map(RuntimePlayState::input_debug_snapshot);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| show_input_debugger(ui, snapshot.as_ref()));
            }
            BottomPanelTab::Runtime => self.show_runtime_debugger(ui),
        }
    }

    /// Draws the primary left-side navigation dock.
    ///
    /// Hierarchy and Systems deliberately share this side panel rather than
    /// the bottom utility dock. This keeps the Inspector visible while either
    /// list is being used and makes the ECS schedule discoverable immediately
    /// after a project is opened.
    pub(super) fn show_left_dock(&mut self, ui: &mut egui::Ui) {
        if self.session.is_animation_graph() {
            self.show_animation_graph_left_dock(ui);
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if self.session.scene().is_some() {
                self.left_panel_tab_button(ui, LeftPanelTab::Hierarchy, "Hierarchy");
            }
            self.left_panel_tab_button(ui, LeftPanelTab::Systems, "Systems");
        });
        ui.separator();

        match self.left_panel_tab {
            LeftPanelTab::Hierarchy if self.session.scene().is_some() => {
                // Entity行、Scene Root行、末尾空白などの詳細なドロップ対象を先に描画する。
                // 詳細対象でペイロードが取得された場合は、対象Entityを親にした配置が
                // `show_scene_hierarchy`内で完了し、後段のフォールバックには残らない。
                let hierarchy_output =
                    egui::ScrollArea::vertical().show(ui, |ui| self.show_scene_hierarchy(ui));

                // 個別行以外のインデント、行間、背景へドロップした場合も、
                // ユーザーはHierarchyへの配置を意図しているためScene Rootへ生成する。
                // Play中はAuthoring Sceneを変更できないので、フォールバックも無効にする。
                if let Some(payload) = hierarchy_viewport_mesh_drop(
                    ui,
                    hierarchy_output.inner_rect,
                    !self.is_playing(),
                ) {
                    self.create_entity_from_dropped_asset(&payload, None);
                }
            }
            LeftPanelTab::Hierarchy | LeftPanelTab::Systems => {
                show_systems_panel(&mut self.systems_panel, ui);
            }
        }
    }

    /// Splits Animation Graph navigation into Motion Slots and related Sets.
    ///
    /// The lower panel is resizable and starts at half the available dock
    /// height. Motion Slot editing keeps the remaining upper region so the
    /// established graph workflow is unchanged.
    fn show_animation_graph_left_dock(&mut self, ui: &mut egui::Ui) {
        let available_height = ui.available_height();
        egui::Panel::bottom("animation_graph_sets_panel")
            .resizable(true)
            .default_size(available_height * 0.5)
            .min_size(ANIMATION_SETS_MIN_HEIGHT)
            .max_size(animation_sets_max_height(available_height))
            .show_inside(ui, |ui| self.show_animation_sets_for_current_graph(ui));
        self.show_motion_slots_panel(ui);
    }

    /// Draws Animation Sets that currently target the active Animation Graph.
    fn show_animation_sets_for_current_graph(&mut self, ui: &mut egui::Ui) {
        enum SetAction {
            Open(AssetId),
            Create(AssetId),
        }

        let graph = self.current_animation_graph_asset();
        let sets = graph
            .as_ref()
            .map(|graph| self.animation_sets_for_graph(graph))
            .unwrap_or_default();
        let mut action = None;

        dock_section_header(ui, "Animation Sets", |ui| {
            if ui
                .add_enabled(graph.is_some(), dock_action_button("Create", 64.0))
                .on_disabled_hover_text("Save and register this Graph before creating a Set")
                .clicked()
            {
                action = graph.clone().map(SetAction::Create);
            }
        });
        ui.small("Sets whose saved target is this graph.");
        ui.separator();

        match graph {
            None => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "This Graph is not registered in the project Asset Manifest.",
                );
            }
            Some(_) if sets.is_empty() => {
                ui.small("No Animation Sets target this Graph yet.");
            }
            Some(_) => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for set in &sets {
                            let path = self
                                .asset_manifest
                                .get(&set.id)
                                .map(|entry| entry.path.as_str())
                                .unwrap_or("(missing manifest entry)");
                            ui.group(|ui| {
                                if ui
                                    .add(egui::Button::new(&set.label).truncate().min_size(
                                        egui::vec2(
                                            ui.available_width(),
                                            ui.spacing().interact_size.y,
                                        ),
                                    ))
                                    .clicked()
                                {
                                    action = Some(SetAction::Open(set.id.clone()));
                                }
                                dock_secondary_line(ui, path);
                            });
                        }
                    });
            }
        }

        match action {
            Some(SetAction::Open(set)) => self.open_animation_set_asset(&set),
            Some(SetAction::Create(graph)) => self.create_animation_set_for_graph(graph),
            None => {}
        }
    }

    fn show_motion_slots_panel(&mut self, ui: &mut egui::Ui) {
        enum SlotAction {
            Add(String),
            Rename(engine_authoring::MotionSlotId, String),
            RequestDelete(engine_authoring::MotionSlotId),
            ConfirmDelete(engine_authoring::MotionSlotId),
            CancelDelete,
        }

        dock_section_header(ui, "Motion Slots", |_ui| {});
        ui.small("Slots belong to this graph. States select a stable slot; Animation Sets bind slots to clips.");
        ui.separator();

        let slots = match self.session.motion_slots() {
            Ok(slots) => slots,
            Err(error) => {
                ui.colored_label(egui::Color32::RED, error.to_string());
                return;
            }
        };
        self.motion_slot_name_buffers
            .retain(|id, _| slots.iter().any(|slot| &slot.id == id));
        for slot in &slots {
            self.motion_slot_name_buffers
                .entry(slot.id.clone())
                .or_insert_with(|| slot.display_name.clone());
        }

        let mut action = None;
        // The slot list grows without bound, so the editing controls occupy a
        // bottom panel of their own. A scroll area that claims the whole dock
        // would push them past the panel edge instead of scrolling the list.
        egui::Panel::bottom("motion_slot_editor_panel")
            .resizable(false)
            .show_inside(ui, |ui| {
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_motion_slot_name)
                        .hint_text("New slot name")
                        .desired_width(f32::INFINITY),
                )
                .on_hover_text("Display name for the new stable Motion Slot");
                if ui.add(dock_action_button("Add", 54.0)).clicked() {
                    action = Some(SlotAction::Add(self.new_motion_slot_name.clone()));
                }

                if let Some(id) = self.pending_motion_slot_delete.clone() {
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Delete this slot? The following States will become Unassigned:",
                    );
                    let usages = self.session.states_using_motion_slot(&id);
                    if usages.is_empty() {
                        ui.small("(No States use this slot.)");
                    } else {
                        // A slot may be used by any number of States. Scrolling
                        // this list keeps the strip's height bounded, which is
                        // what MOTION_SLOTS_MIN_HEIGHT is allowed to assume.
                        egui::ScrollArea::vertical()
                            .max_height(MOTION_SLOT_USAGE_LIST_HEIGHT)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (_, name) in usages {
                                    ui.label(format!("• {name}"));
                                }
                            });
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.add(dock_action_button("Delete Slot", 88.0)).clicked() {
                            action = Some(SlotAction::ConfirmDelete(id.clone()));
                        }
                        if ui.add(dock_action_button("Cancel", 62.0)).clicked() {
                            action = Some(SlotAction::CancelDelete);
                        }
                    });
                }
            });

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for slot in &slots {
                    ui.group(|ui| {
                        if let Some(buffer) = self.motion_slot_name_buffers.get_mut(&slot.id) {
                            ui.add(egui::TextEdit::singleline(buffer).desired_width(f32::INFINITY));
                            // Wrapping keeps both buttons inside a narrow dock;
                            // a right-to-left row overlaps them instead.
                            ui.horizontal_wrapped(|ui| {
                                if ui.add(dock_action_button("Rename", 68.0)).clicked() {
                                    action =
                                        Some(SlotAction::Rename(slot.id.clone(), buffer.clone()));
                                }
                                if ui.add(dock_action_button("Delete", 62.0)).clicked() {
                                    action = Some(SlotAction::RequestDelete(slot.id.clone()));
                                }
                            });
                        }
                        dock_secondary_line(ui, slot.id.as_str());
                    });
                }
            });

        match action {
            Some(SlotAction::Add(name)) => match self.session.add_motion_slot(name) {
                Ok(id) => {
                    self.new_motion_slot_name.clear();
                    if let Ok(slots) = self.session.motion_slots()
                        && let Some(slot) = slots.into_iter().find(|slot| slot.id == id) {
                            self.motion_slot_name_buffers
                                .insert(slot.id, slot.display_name);
                        }
                }
                Err(error) => self.apply_ui_result::<(), _>(Err(error)),
            },
            Some(SlotAction::Rename(id, name)) => {
                let result = self.session.rename_motion_slot(id, name);
                self.apply_ui_result(result);
            }
            Some(SlotAction::RequestDelete(id)) => {
                self.pending_motion_slot_delete = Some(id);
            }
            Some(SlotAction::ConfirmDelete(id)) => {
                let result = self.session.delete_motion_slot(id);
                self.pending_motion_slot_delete = None;
                self.apply_ui_result(result);
                self.sync_property_buffer();
            }
            Some(SlotAction::CancelDelete) => {
                self.pending_motion_slot_delete = None;
            }
            None => {}
        }
    }

    fn bottom_panel_tab_button(&mut self, ui: &mut egui::Ui, tab: BottomPanelTab, label: &str) {
        if ui
            .selectable_label(self.bottom_panel_tab == tab, label)
            .clicked()
        {
            if self.bottom_panel_tab == tab {
                self.bottom_panel_open = !self.bottom_panel_open;
            } else {
                self.bottom_panel_tab = tab;
                self.bottom_panel_open = true;
            }
        }
    }

    fn bottom_panel_menu_item(&mut self, ui: &mut egui::Ui, tab: BottomPanelTab, label: &str) {
        if ui.button(label).clicked() {
            self.bottom_panel_tab = tab;
            self.bottom_panel_open = true;
            ui.close();
        }
    }

    fn left_panel_tab_button(&mut self, ui: &mut egui::Ui, tab: LeftPanelTab, label: &str) {
        if ui
            .selectable_label(self.left_panel_tab == tab, label)
            .clicked()
        {
            self.left_panel_tab = tab;
        }
    }

    fn left_panel_menu_item(&mut self, ui: &mut egui::Ui, tab: LeftPanelTab, label: &str) {
        if ui.button(label).clicked() {
            self.left_panel_tab = tab;
            ui.close();
        }
    }

    fn navigate_to_diagnostic_target(
        &mut self,
        target: Option<engine_authoring::DiagnosticTarget>,
    ) {
        match target {
            Some(engine_authoring::DiagnosticTarget::Entity { id }) => {
                self.select_single_entity(Some(id));
            }
            Some(engine_authoring::DiagnosticTarget::Component { entity, .. }) => {
                self.select_single_entity(Some(entity));
            }
            Some(engine_authoring::DiagnosticTarget::Asset { id }) => {
                if let Some(path) = self.asset_manifest.get(&id).map(|entry| entry.path.clone()) {
                    self.reveal_asset_in_browser(Path::new(&path));
                }
            }
            Some(engine_authoring::DiagnosticTarget::Node { graph, node }) => {
                if graph == self.session.graph().id {
                    let result = self.session.select_node(Some(node));
                    self.apply_ui_result(result);
                }
            }
            Some(engine_authoring::DiagnosticTarget::Port { graph, node, .. }) => {
                if graph == self.session.graph().id {
                    let result = self.session.select_node(Some(node));
                    self.apply_ui_result(result);
                }
            }
            Some(engine_authoring::DiagnosticTarget::SourceFile { path, line }) => {
                let Some(project) = &self.project_root else {
                    return;
                };
                let candidate = if Path::new(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    project.game_dir().join(&path)
                };
                let resolved = fs::canonicalize(candidate).and_then(|candidate| {
                    let rust_root = fs::canonicalize(project.rust_scripts_dir())?;
                    let host_root = fs::canonicalize(project.game_dir().join("src"))?;
                    if candidate.starts_with(rust_root) || candidate.starts_with(host_root) {
                        Ok(candidate)
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "diagnostic source resolves outside project Rust roots",
                        ))
                    }
                });
                match resolved {
                    Ok(path) => {
                        if let Err(error) =
                            self.preferences.open_source(&path, line.unwrap_or(1), 1)
                        {
                            self.session
                                .push_diagnostic(engine_authoring::Diagnostic::error(
                                    "editor.external_editor_failed",
                                    format!("the OS could not open {}: {error}", path.display()),
                                ));
                        }
                    }
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.build_source_open_failed",
                                format!("could not open compiler source `{path}`: {error}"),
                            ))
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn show_summary(&self, ui: &mut egui::Ui) {
        if let Some(scene) = self.session.scene() {
            ui.heading("Scene");
            ui.label(format!("Entities: {}", scene.entity_count()));
            ui.label(format!(
                "Mode: {}",
                if self.is_playing() { "Playing" } else { "Edit" }
            ));
            if let Some(runtime) = &self.runtime_state {
                ui.label(format!("Runtime entities: {}", runtime.entity_count()));
                ui.label(format!("Runtime ticks: {}", runtime.ticks()));
            }
            let file_label = self
                .session
                .current_document_path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            ui.label(format!("File: {file_label}"));
            return;
        }

        ui.heading(&self.session.graph().display_name);
        ui.label(format!("Graph: {}", self.session.graph().name));
        ui.label(format!("Kind: {}", self.session.graph().kind.as_str()));
        ui.label(format!("Nodes: {}", self.session.graph().nodes.len()));
        ui.label(format!("Edges: {}", self.session.graph().edges.len()));
        ui.label(format!(
            "Graph view: {}",
            if self.session.graph_view().is_some() {
                "loaded"
            } else {
                "missing"
            }
        ));
        let file_label = self
            .session
            .current_document_path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        ui.label(format!("File: {file_label}"));
        if let Some(source) = &self.pending_connect_source {
            ui.label(format!("Connect from: {}", source.as_str()));
        }
    }

    pub(super) fn handle_keyboard_shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
    ) {
        let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);
        let shift = ui.input(|i| i.modifiers.shift);
        let z_pressed = ui.input(|i| i.key_pressed(egui::Key::Z));
        let y_pressed = ui.input(|i| i.key_pressed(egui::Key::Y));
        let s_pressed = ui.input(|i| i.key_pressed(egui::Key::S));
        let o_pressed = ui.input(|i| i.key_pressed(egui::Key::O));
        let t_pressed = ui.input(|i| i.key_pressed(egui::Key::T));
        let r_pressed = ui.input(|i| i.key_pressed(egui::Key::R));
        let d_pressed = ui.input(|i| i.key_pressed(egui::Key::D));
        let c_pressed = ui.input(|i| i.key_pressed(egui::Key::C));
        let v_pressed = ui.input(|i| i.key_pressed(egui::Key::V));
        let f_pressed = ui.input(|i| i.key_pressed(egui::Key::F));
        let delete_pressed = ui.input(|i| i.key_pressed(egui::Key::Delete));
        let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let in_scene = self.session.scene().is_some() && !self.is_playing();

        // Text fields own editing, clipboard, and undo shortcuts while they
        // have focus. This also prevents typing T/R/S from changing tools.
        if ui.ctx().egui_wants_keyboard_input() {
            if self.is_playing() && escape_pressed {
                self.stop_play(frame.wgpu_render_state());
            }
            return;
        }

        if self.is_playing() && escape_pressed {
            self.stop_play(frame.wgpu_render_state());
        } else if escape_pressed && self.prefab_placement_source.is_some() {
            self.prefab_placement_source = None;
        } else if ctrl && z_pressed && !shift {
            if !self.claim_animation_set_shortcut(ui.ctx(), DocumentShortcut::Undo) {
                self.apply_undo();
            }
        } else if ctrl && (y_pressed || (z_pressed && shift)) {
            if !self.claim_animation_set_shortcut(ui.ctx(), DocumentShortcut::Redo) {
                self.apply_redo();
            }
        } else if ctrl && s_pressed && shift {
            self.save_as();
        } else if ctrl && s_pressed {
            if !self.claim_animation_set_shortcut(ui.ctx(), DocumentShortcut::Save) {
                self.save();
            }
        } else if ctrl && o_pressed {
            self.open_file();
        } else if ctrl && shift && d_pressed && in_scene {
            self.repeat_last_duplicate();
        } else if ctrl && d_pressed && in_scene {
            self.duplicate_selected_entity();
        } else if ctrl && c_pressed && in_scene {
            self.copy_selected_entity();
        } else if ctrl && v_pressed && in_scene {
            self.paste_entity();
        } else if !ctrl && delete_pressed && in_scene {
            self.delete_selected_entity();
        } else if !ctrl && t_pressed && in_scene {
            self.gizmo_mode = GizmoMode::Translate;
        } else if !ctrl && r_pressed && in_scene {
            self.gizmo_mode = GizmoMode::Rotate;
        } else if !ctrl && s_pressed && in_scene {
            self.gizmo_mode = GizmoMode::Scale;
        } else if !ctrl && f_pressed && in_scene
            && let (Some(scene), Some(entity)) =
                (self.session.scene(), self.selected_entity.as_ref())
            {
                self.scene_view.focus_entity(scene, entity);
            }
    }

    pub(super) fn show_editor_preferences_window(&mut self, context: &egui::Context) {
        if !self.show_editor_preferences {
            return;
        }
        let mut open = true;
        let mut save = false;
        egui::Window::new("Editor Preferences")
            .open(&mut open)
            .resizable(true)
            .show(context, |ui| {
                ui.heading("External Code Editor");
                ui.label("Opening source only occurs after an explicit Open Script action.");
                let mut executable = self
                    .preferences
                    .external_editor
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label("Executable");
                    if ui.text_edit_singleline(&mut executable).changed() {
                        self.preferences.external_editor = if executable.trim().is_empty() {
                            None
                        } else {
                            Some(PathBuf::from(executable.trim()))
                        };
                    }
                    if ui.button("Browse...").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.preferences.external_editor = Some(path);
                        }
                });
                ui.horizontal(|ui| {
                    ui.label("Arguments");
                    ui.text_edit_singleline(&mut self.preferences.external_editor_arguments);
                });
                ui.small("Placeholders: {path}, {line}, {column}. Leave executable empty to use the OS association once.");
                ui.separator();
                if ui.button("Save Preferences").clicked() {
                    save = true;
                }
            });
        if save {
            self.preferences.save();
        }
        if !open {
            self.show_editor_preferences = false;
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum EditorNotificationLevel {
    Success,
    Info,
    Error,
}

pub(super) struct EditorNotification {
    pub(super) message: String,
    pub(super) level: EditorNotificationLevel,
    pub(super) expires_at: std::time::Instant,
}

/// Selects which auxiliary content occupies the bottom dock.
///
/// Project assets and diagnostics are transient editor utilities, so they stay
/// below the main scene/graph workspace rather than competing with the
/// Hierarchy, Systems, or Inspector panels.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BottomPanelTab {
    Assets,
    Console,
    Problems,
    Input,
    Runtime,
}

/// Selects which project-wide or scene-wide list occupies the left dock.
///
/// Scene editing can switch between Hierarchy and Systems, while graph or
/// project-only work can still expose the project-wide Systems catalog. UI
/// Builder owns a dedicated palette and UI hierarchy in its central workspace,
/// so the outer left dock is omitted there to prioritize preview width.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftPanelTab {
    Hierarchy,
    Systems,
}

/// Initial width used before egui has persisted a user-resized left dock.
pub(super) const LEFT_DOCK_DEFAULT_WIDTH: f32 = 260.0;

/// Smallest practical width for hierarchy controls and short entity names.
pub(super) const LEFT_DOCK_MIN_WIDTH: f32 = 180.0;

/// Builds a single-line action button for narrow editor docks.
///
/// Dock descriptions may wrap, but action labels must remain horizontal so a
/// resized panel cannot turn "Create" or "Add" into a tall stack of letters.
pub(super) fn dock_action_button(label: &'static str, width: f32) -> egui::Button<'static> {
    egui::Button::new(label)
        .truncate()
        .min_size(egui::vec2(width, 26.0))
}

/// Lays out a row of controls that breaks between widgets instead of inside
/// them.
///
/// The Inspector and the left dock enable text wrapping so an unbreakable value
/// cannot widen the panel. A plain horizontal row combines that wrap mode with
/// the shrinking remainder of the row, so the first control that no longer fits
/// is folded into a one-glyph-per-line column instead of moving out of the way:
/// a narrow Inspector turned "Place Repeatedly" into a vertical stack of
/// letters. A wrapping row offers every child the full row width and starts a
/// new line when a widget does not fit, which keeps button, tab, and status
/// labels readable at any dock width.
///
/// Rows whose children size themselves from [`egui::Ui::available_width`] must
/// keep using [`egui::Ui::horizontal`]. A wrapping row reports the full row
/// width at every position, so such a child would claim the space its
/// neighbours already occupy and push the row past the panel.
pub(super) fn control_row<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal_wrapped(add_contents).inner
}

/// Draws a left dock section title with its actions aligned to the right edge.
///
/// The dock turns text wrapping on so a long value cannot widen the panel, but
/// egui wrapping inside a right-to-left layout mispositions and overlaps the
/// wrapped glyphs. The actions are therefore placed from the right first and
/// the title is laid out left to right in whatever space is left, truncating
/// rather than wrapping.
pub(super) fn dock_section_header<R>(
    ui: &mut egui::Ui,
    title: &str,
    add_actions: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let actions = add_actions(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(title).heading()).truncate());
            });
            actions
        })
        .inner
    })
    .inner
}

/// Draws a secondary identifier or path line inside the left dock.
///
/// Wrapping a stable ID or an asset path turns one row into a ragged block of
/// glyphs. Truncating with the full value on hover keeps every row one line
/// tall without hiding information.
pub(super) fn dock_secondary_line(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).monospace().weak()).truncate())
        .on_hover_text(text);
}

/// Horizontal space kept clear for the shell-owned Authoring Tools launcher.
///
/// The launcher is a foreground Area rather than an [`EditorApp`] child because
/// its modeless windows remain owned by the executable shell. Reserving its
/// complete button width plus the right margin prevents document tabs from
/// painting underneath it.
pub(super) const AUTHORING_TOOLS_LAUNCHER_RESERVED_WIDTH: f32 = 148.0;

/// Allocates the portion of the unified toolbar available to [`EditorApp`].
///
/// The returned response deliberately excludes the launcher reserve at the
/// right edge. The width is clamped for defensive headless tests and for any
/// transient viewport smaller than the native minimum size.
pub(super) fn show_main_toolbar_content<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let content_width =
        (ui.available_width() - AUTHORING_TOOLS_LAUNCHER_RESERVED_WIDTH).max(0.0);
    let content_height = ui.available_height();

    ui.allocate_ui_with_layout(
        egui::vec2(content_width, content_height),
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    )
}

/// Draws document tabs in the toolbar without creating a second visible row.
///
/// Overflow remains accessible through horizontal wheel or drag scrolling.
/// The scrollbar itself stays hidden because a visible horizontal scrollbar
/// would consume most of the 40-point toolbar height.
pub(super) fn show_toolbar_document_tab_strip(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let available_size = ui.available_size();

    egui::ScrollArea::horizontal()
        .id_salt("editor_document_tabs_scroll")
        .max_width(available_size.x)
        .max_height(available_size.y)
        .auto_shrink([false, true])
        .scroll_bar_visibility(
            egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
        )
        .show(ui, add_contents);
}

/// Smallest width retained for the active Scene View or authoring workspace.
pub(super) const WORKSPACE_MIN_WIDTH: f32 = 320.0;

/// Smallest height retained for the active Scene View or authoring workspace.
pub(super) const WORKSPACE_MIN_HEIGHT: f32 = 240.0;

/// Smallest usable height of the expanded bottom utility dock.
///
/// This must be at least what the dock's own chrome plus one row of content
/// occupies — the tab bar, its separator, the Assets header, and one asset
/// grid cell. A smaller value lets the drag pass below what the contents can
/// actually shrink to, and `show_bottom_dock` claims the available area, so
/// the panel springs back on release instead of staying where it was dropped.
pub(super) const BOTTOM_DOCK_MIN_HEIGHT: f32 = ASSET_GRID_CELL_HEIGHT + 76.0;

/// Initial width of the Inspector before egui restores a dragged size.
pub(super) const INSPECTOR_DEFAULT_WIDTH: f32 = 320.0;

/// Smallest usable width of the Inspector.
pub(super) const INSPECTOR_MIN_WIDTH: f32 = 220.0;

/// Calculates a window-relative bottom-dock limit while retaining a usable
/// authoring workspace. Unlike the previous fixed limit, this grows with the
/// editor window.
pub(super) fn bottom_dock_max_height(available_height: f32) -> f32 {
    (available_height - WORKSPACE_MIN_HEIGHT).max(BOTTOM_DOCK_MIN_HEIGHT)
}

/// Calculates the left-dock limit while reserving both the Inspector's minimum
/// width and a usable central authoring workspace.
pub(super) fn left_dock_max_width(available_width: f32) -> f32 {
    (available_width - INSPECTOR_MIN_WIDTH - WORKSPACE_MIN_WIDTH).max(LEFT_DOCK_MIN_WIDTH)
}

/// Calculates the Inspector limit from the space remaining after the optional
/// left dock has been laid out.
pub(super) fn inspector_max_width(available_width: f32) -> f32 {
    (available_width - WORKSPACE_MIN_WIDTH).max(INSPECTOR_MIN_WIDTH)
}

/// Smallest usable height of the Animation Sets section in the left dock.
pub(super) const ANIMATION_SETS_MIN_HEIGHT: f32 = 120.0;

/// Tallest the delete-confirmation usage list grows before it scrolls.
pub(super) const MOTION_SLOT_USAGE_LIST_HEIGHT: f32 = 84.0;

/// Smallest height that keeps the Motion Slots section usable.
///
/// The section reserves a bottom strip of its own for the new-slot field, the
/// Add button, and the delete confirmation, and it draws a header, a hint line,
/// and a separator above the list. A bottom panel is placed relative to the
/// bottom edge and does not shrink to fit, so once the region left over above
/// the Animation Sets panel is shorter than its own chrome, the strip lands on
/// top of the header instead of below the list. Reserving that chrome plus one
/// slot row keeps the drag inside what the section can actually render.
pub(super) const MOTION_SLOTS_MIN_HEIGHT: f32 = MOTION_SLOT_USAGE_LIST_HEIGHT + 180.0;

/// Calculates the Animation Sets limit while reserving a usable Motion Slots
/// section in the same dock.
pub(super) fn animation_sets_max_height(available_height: f32) -> f32 {
    (available_height - MOTION_SLOTS_MIN_HEIGHT).max(ANIMATION_SETS_MIN_HEIGHT)
}

/// Returns whether the active authoring surface needs the outer left dock.
///
/// UI Builder has its own hierarchy and should not surrender preview width to
/// the project-wide Systems list. Scene editing always needs Hierarchy, while a
/// project without an active scene still keeps Systems reachable.
pub(super) fn should_show_left_dock(
    has_project: bool,
    has_scene: bool,
    has_ui_document: bool,
) -> bool {
    !has_ui_document && (has_project || has_scene)
}

/// Returns the id scope that keeps one dock surface's egui state, above all
/// its scroll offset, separate per document tab.
///
/// egui derives a scroll area's id from its parent rather than from a draw
/// counter, so every surface sharing a dock would otherwise share one offset.
/// Two documents of different lengths then clamp that shared offset to the
/// shorter one, which is what used to send the Inspector back to the top
/// whenever the author returned from an Animation Graph.
pub(super) fn dock_surface_id(surface: &'static str, tab: WorkspaceTabId) -> egui::Id {
    egui::Id::new((surface, tab))
}

/// Asset Browserからのドラッグ中に、マウスへ追従する視覚的なプレビューを表示する。
///
/// 表示は入力を受け取らないTooltipレイヤーへ配置するため、Hierarchy、Scene View、
/// Inspectorなどの本来のドロップ対象を遮らない。ペイロードがリリースまたはキャンセルされると、
/// eguiのD&Dストレージから消えるため、この表示も自動的に終了する。
pub(super) fn show_asset_drag_preview(context: &egui::Context) {
    // 登録済みアセットのドラッグ中だけ表示する。
    // ファイル移動用の別ペイロードとは混同しない。
    let Some(payload) = egui::DragAndDrop::payload::<DragPayload>(context) else {
        return;
    };

    // ウィンドウ外へカーソルが出た場合は、有効な表示位置がないため描画しない。
    let Some(pointer_position) = context.pointer_hover_pos() else {
        return;
    };

    // ドラッグ中はポインター移動に合わせて毎フレーム位置を更新する。
    context.request_repaint();

    // カーソル自体やドロップ対象の強調表示を隠さないよう、右下へ少しずらす。
    let preview_position = pointer_position + egui::vec2(18.0, 18.0);
    let text = asset_drag_preview_text(payload.as_ref());

    // Tooltipレイヤーは通常パネルより上へ描画され、interactable(false)により
    // ドロップ先のcontains_pointer判定を奪わない。
    egui::Area::new(egui::Id::new("asset_drag_preview"))
        .fixed_pos(preview_position)
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(context, |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_rgba_unmultiplied(28, 40, 56, 242))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgb(90, 180, 255),
                ))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // 矢印は「現在移動中」であることを短い表示面積で伝える。
                        ui.colored_label(egui::Color32::from_rgb(120, 195, 255), "↗");
                        ui.strong(text);
                    });
                });
        });
}

/// ドラッグプレビューへ表示する短い説明文を生成する。
///
/// AssetIdは人がドラッグ対象を識別するには長いため表示せず、
/// Asset Browserと同じカテゴリー記号とファイル名を使用する。
pub(super) fn asset_drag_preview_text(payload: &DragPayload) -> String {
    // 通常はファイル名だけを表示し、万一ファイル名を取得できないパスでは
    // アセット相対パス全体へフォールバックする。
    let name = payload
        .relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| payload.relative_path.to_str().unwrap_or("(asset)"));

    format!("Dragging {} {name}", payload.kind.label())
}

/// Hierarchyの可視スクロール領域全体を、Mesh配置のフォールバック対象として扱う。
///
/// 個別Entity行は、この関数より先に`Response::dnd_release_payload`を呼び出す。
/// そのため、行へ落とされたペイロードはすでに消費されており、この関数が同じモデルを
/// Scene Rootへ二重生成することはない。個別行が取得しなかった背景上のドロップだけを扱う。
///
/// `enabled`は編集可能状態を表し、Play中には`false`が渡される。
/// 戻り値はScene Rootへ配置すべきMeshペイロードで、ドラッグ中または対象外では`None`となる。
pub(super) fn hierarchy_viewport_mesh_drop(
    ui: &egui::Ui,
    viewport: egui::Rect,
    enabled: bool,
) -> Option<DragPayload> {
    // Play中やScene編集不可状態では、見た目の強調もペイロード取得も行わない。
    if !enabled {
        return None;
    }

    // ScrollAreaの可視領域外では、Hierarchyへのドロップとして扱わない。
    // clip rectを考慮する`rect_contains_pointer`により、スクロール外の隠れた範囲も除外する。
    if !ui.rect_contains_pointer(viewport) {
        return None;
    }

    // Hierarchyへ配置できるのはMeshペイロードだけに限定する。
    // FBX、glTF、GLBはAsset Browser上ではMeshとして分類され、
    // 後段の`create_entity_from_dropped_asset`がモデルソースか単体Meshかを判別する。
    let payload = egui::DragAndDrop::payload::<DragPayload>(ui.ctx())?;
    if payload.kind != AssetKind::Mesh {
        return None;
    }

    // ドロップ可能領域を明示し、行間や背景でも受け取れることをユーザーへ伝える。
    // ペイロードをまだ取得しないため、ドラッグ中の状態は維持される。
    ui.painter().rect_stroke(
        viewport,
        0.0,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 180, 255)),
        egui::StrokeKind::Inside,
    );

    // ボタンを離すまではプレビュー表示だけを行い、Authoring Sceneを変更しない。
    if !ui.input(|input| input.pointer.any_released()) {
        return None;
    }

    // リリース時にペイロードを原子的に取得して消去する。
    // `take_payload`を使うことで、同じフレーム内の別ドロップ対象による二重処理を防止する。
    egui::DragAndDrop::take_payload::<DragPayload>(ui.ctx()).map(|payload| payload.as_ref().clone())
}

/// Creates the primary left dock without allowing clipped content to reserve
/// invisible horizontal space.
///
/// egui clips a panel to its configured maximum width, but an unwrapped child
/// can still enlarge the response rectangle that advances the parent cursor.
/// The resulting difference is an unpainted strip between this dock and the
/// central panel. Applying a dock-local wrap mode keeps long hierarchy names,
/// system IDs, descriptions, and diagnostics inside the visible panel bounds.
pub(super) fn show_primary_left_dock_panel<R>(
    ui: &mut egui::Ui,
    max_width: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Panel::left("editor_left_dock")
        .resizable(true)
        .default_size(LEFT_DOCK_DEFAULT_WIDTH)
        .min_size(LEFT_DOCK_MIN_WIDTH)
        .max_size(max_width)
        .show_inside(ui, |ui| {
            // Horizontal child layouts normally extend labels onto one line.
            // Override that behavior only inside this dock so long text wraps
            // before it can enlarge the panel response beyond the clip rect.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

            // Preserve the visible panel interior as the authoritative width
            // even when an individual widget advertises a larger desired size.
            let available_width = ui.available_width();
            ui.set_width(available_width);

            // Selection, schedule edits, and persistence remain owned by the
            // existing dock contents; this helper changes layout bounds only.
            add_contents(ui)
        })
}

/// Creates the Inspector dock so that it paints and claims the whole width the
/// user dragged it to.
///
/// egui derives a panel's painted frame and its persisted size from the
/// contents' response rectangle, not from the rectangle the panel reserved. A
/// right-hand panel is anchored to its right edge while its contents grow from
/// the left, so an Inspector whose body is narrower than the dragged width —
/// a graph document with nothing selected, for example — leaves the strip
/// between the contents and the window edge unpainted and springs back to its
/// minimum on the next frame. Claiming the full interior width fixes both.
/// A manually bounded child now provides the stronger mirror guard: its desired
/// size never contributes to the panel response. Dock-local truncation also
/// keeps narrow horizontal controls on one line.
#[cfg(test)]
pub(super) fn show_inspector_panel<R>(
    ui: &mut egui::Ui,
    max_width: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    show_inspector_panel_at_offset(ui, max_width, None, add_contents)
}

pub(super) fn show_inspector_panel_at_offset<R>(
    ui: &mut egui::Ui,
    max_width: f32,
    vertical_scroll_offset: Option<f32>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Panel::right("inspector_panel")
        .resizable(true)
        .default_size(INSPECTOR_DEFAULT_WIDTH)
        .min_size(INSPECTOR_MIN_WIDTH)
        .max_size(max_width)
        .show_inside(ui, |ui| {
            let viewport_rect = ui.available_rect_before_wrap();
            let viewport_clip = ui.clip_rect().intersect(viewport_rect);
            let mut viewport_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("inspector_viewport")
                    .max_rect(viewport_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            viewport_ui.set_clip_rect(viewport_clip);

            // Single-line truncation is the safe dock default: long IDs cannot
            // enlarge the viewport, and narrow horizontal rows never collapse
            // into one-glyph-per-line text. Labels expose elided text on hover.
            viewport_ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);

            let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(offset) = vertical_scroll_offset {
                scroll_area = scroll_area.vertical_scroll_offset(offset);
            }
            let result = scroll_area
                .show(&mut viewport_ui, |ui| {
                    let viewport_width = ui.available_width();
                    ui.set_width(viewport_width);
                    add_contents(ui)
                })
                .inner;

            // `new_child` does not allocate its desired rectangle in the
            // parent. Advance by the authoritative viewport only after the
            // child is dropped so oversized descendants cannot affect it.
            drop(viewport_ui);
            ui.advance_cursor_after_rect(viewport_rect);
            result
        })
}

/// Applies the editor's restrained dark visual system.
///
/// The three close background values preserve panel hierarchy without bright
/// borders, while the blue selection color keeps selection legible in the
/// Scene, Hierarchy, graph, and asset surfaces.
pub(super) fn apply_editor_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(31, 34, 39);
    visuals.window_fill = egui::Color32::from_rgb(36, 40, 46);
    visuals.extreme_bg_color = egui::Color32::from_rgb(23, 26, 30);
    visuals.faint_bg_color = egui::Color32::from_rgb(42, 47, 54);
    visuals.selection.bg_fill = egui::Color32::from_rgb(46, 103, 168);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(126, 184, 255));
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.indent = 16.0;
    });
}
