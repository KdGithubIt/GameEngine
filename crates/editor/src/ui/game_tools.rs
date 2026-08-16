//! Rust game project tooling: builds, script scaffolding, navmesh baking, packaging.

use super::*;

impl EditorApp {
    pub(super) fn initialize_rust_game(&mut self, project: &ProjectRoot) {
        match engine_authoring::initialize_game_project(project) {
            Ok(()) => {
                self.refresh_game_code_browser(None);
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.game_project_initialized",
                        "project-local Rust game crate initialized",
                    ));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.game_project_init_failed",
                    error.to_string(),
                )),
        }
    }

    pub(super) fn open_rust_file(&mut self, project: &ProjectRoot) {
        let Some(path) = rfd::FileDialog::new()
            .set_directory(project.rust_scripts_dir())
            .add_filter("Rust", &["rs"])
            .pick_file()
        else {
            return;
        };
        if let Err(error) = self.preferences.open_source(&path, 1, 1) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.external_editor_failed",
                    format!("the OS could not open {}: {error}", path.display()),
                ));
        }
    }

    /// Refreshes game-code entries and keeps a newly created source selected.
    pub(super) fn refresh_game_code_browser(&mut self, selected_path: Option<&Path>) {
        let Some(project) = self.project_root.clone() else {
            self.asset_browser = AssetBrowser::new();
            return;
        };
        self.asset_browser.refresh(&project.assets_root());
        self.component_source_index = ComponentSourceIndex::build(&project.rust_scripts_dir());
        self.refresh_scene_problems();
        let Some(selected_path) = selected_path else {
            return;
        };
        if let Ok(relative) = selected_path.strip_prefix(project.assets_root()) {
            self.asset_browser.select_relative_path(relative);
        }
    }

    /// Starts the Unity-style compile step after creating project Rust code.
    ///
    /// Cargo builds are serialized and ADR 0050 forbids replacing a module in
    /// an active Play world. When either condition applies, the request stays
    /// queued and is started after the current operation becomes safe.
    pub(super) fn request_game_build_after_edit(&mut self) {
        self.game_code_generation = self.game_code_generation.saturating_add(1);
        self.game_build_requested_after_edit = true;
        self.game_build_quiet_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(900));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "editor.game_build.dirty",
                format!(
                "game code generation {} is dirty; automatic build waits for the edit quiet period",
                self.game_code_generation
            ),
            ));
    }

    pub(super) fn poll_coalesced_game_build(&mut self, context: &egui::Context) {
        let Some(deadline) = self.game_build_quiet_deadline else {
            return;
        };
        if std::time::Instant::now() < deadline {
            context.request_repaint_after(
                deadline.saturating_duration_since(std::time::Instant::now()),
            );
            return;
        }
        if self.is_playing() || self.game_build.state() != GameBuildState::Idle {
            self.game_build_requested_after_edit = true;
            context.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        self.game_build_quiet_deadline = None;
        self.game_build_requested_after_edit = false;
        if self.start_game_build(GameBuildKind::Build) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.game_build.coalesced",
                    format!(
                        "building newest game-code generation {}",
                        self.game_code_generation
                    ),
                ));
        }
    }

    pub(super) fn start_game_build(&mut self, kind: GameBuildKind) -> bool {
        let Some(project) = self.project_root.clone() else {
            return false;
        };
        self.game_build_problems.clear();
        self.refresh_scene_problems();
        if let Err(error) = self.game_build.start(&project, kind) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.game_build_start_failed",
                    error.to_string(),
                ));
            false
        } else {
            self.running_game_code_generation = Some(self.game_code_generation);
            true
        }
    }

    pub(super) fn handle_game_build_result(&mut self, result: GameBuildResult) {
        let GameBuildResult {
            kind,
            success,
            diagnostics,
            module_path,
            error,
        } = result;
        for diagnostic in diagnostics {
            let location = match (&diagnostic.file, diagnostic.line) {
                (Some(file), Some(line)) => format!("{}:{line}: ", file.display()),
                (Some(file), None) => format!("{}: ", file.display()),
                _ => String::new(),
            };
            let message = format!("{location}{}", diagnostic.message);
            let mut authoring_diagnostic = if diagnostic.level == "warning" {
                engine_authoring::Diagnostic::warning("editor.game_build.warning", message)
            } else {
                engine_authoring::Diagnostic::error("editor.game_build.error", message)
            };
            if let Some(file) = diagnostic.file {
                authoring_diagnostic = authoring_diagnostic.with_target(
                    engine_authoring::DiagnosticTarget::SourceFile {
                        path: file.to_string_lossy().replace('\\', "/"),
                        line: diagnostic.line,
                    },
                );
            }
            self.game_build_problems.push(authoring_diagnostic.clone());
            self.session.push_diagnostic(authoring_diagnostic);
        }
        if let Some(error) = error {
            let diagnostic = engine_authoring::Diagnostic::error("editor.game_build.failed", error);
            self.game_build_problems.push(diagnostic.clone());
            self.session.push_diagnostic(diagnostic);
        }
        let mut loaded = false;
        if success && kind == GameBuildKind::Build
            && let Some(path) = module_path.as_ref() {
                match engine::game_module::GameModule::load(path) {
                    Ok(module) => {
                        self.game_module = Some(Arc::new(module));
                        if let Some(project) = self.project_root.as_ref() {
                            self.systems_panel.open_project(
                                project,
                                self.game_module.as_ref().map(Arc::clone),
                                None,
                            );
                        }
                        loaded = true;
                        if let Some(generation) = self.running_game_code_generation {
                            self.built_game_code_generation = generation;
                        }
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::info(
                                "editor.game_build.succeeded",
                                format!("game module built and loaded from {}", path.display()),
                            ));
                    }
                    Err(error) => {
                        if let Some(project) = self.project_root.as_ref() {
                            self.systems_panel
                                .open_project(project, None, Some(error.to_string()));
                        }
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.game_module_load_failed",
                                error.to_string(),
                            ))
                    }
                }
            }
        if success && kind == GameBuildKind::Check {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.game_check.succeeded",
                    "game code check succeeded",
                ));
        }
        if kind == GameBuildKind::Release {
            if success {
                self.finish_pending_game_package(module_path.as_deref());
            } else {
                self.pending_game_package = None;
            }
        }
        if self.play_after_game_build {
            self.play_after_game_build = false;
            if loaded {
                self.start_play_ready();
            }
        }
        if self.game_build_requested_after_edit
            && !self.is_playing()
            && self.game_code_generation > self.built_game_code_generation
        {
            self.game_build_quiet_deadline = Some(std::time::Instant::now());
        }
        self.running_game_code_generation = None;
        self.refresh_scene_problems();
    }

    pub(super) fn show_new_rhai_script_modal(&mut self, ctx: &egui::Context) {
        if !self.show_new_rhai_script {
            return;
        }
        let mut open = true;
        egui::Window::new("Create Rhai Script")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("File name (without .rhai)");
                ui.text_edit_singleline(&mut self.new_rhai_script_name);
                control_row(ui, |ui| {
                    if ui.button("Create").clicked()
                        && let Some(project) = self.project_root.clone() {
                            let folder = self
                                .asset_browser
                                .selected_folder()
                                .strip_prefix("scripts/rhai")
                                .unwrap_or(Path::new(""));
                            match engine_authoring::create_rhai_script_in(
                                &project,
                                folder,
                                self.new_rhai_script_name.trim(),
                            ) {
                                Ok(path) => {
                                    self.asset_browser.refresh(&project.assets_root());
                                    if let Ok(relative) = path.strip_prefix(project.assets_root()) {
                                        self.asset_browser.select_relative_path(relative);
                                    }
                                    self.session.push_diagnostic(
                                        engine_authoring::Diagnostic::info(
                                            "editor.rhai_script_created",
                                            format!("created {}", path.display()),
                                        ),
                                    );
                                    self.show_new_rhai_script = false;
                                }
                                Err(error) => self.session.push_diagnostic(
                                    engine_authoring::Diagnostic::error(
                                        "editor.rhai_script_create_failed",
                                        error.to_string(),
                                    ),
                                ),
                            }
                        }
                    if ui.button("Cancel").clicked() {
                        self.show_new_rhai_script = false;
                    }
                });
            });
        if !open {
            self.show_new_rhai_script = false;
        }
    }

    pub(super) fn show_new_rust_script_modal(&mut self, ctx: &egui::Context) {
        if !self.show_new_rust_script {
            return;
        }
        let mut open = true;
        egui::Window::new("Create Rust Script")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ComboBox::from_label("Kind")
                    .selected_text(match self.new_rust_script_kind {
                        RustScriptKind::Component => "Component",
                        RustScriptKind::Resource => "Resource",
                        RustScriptKind::System => "System",
                        RustScriptKind::SharedModule => "Shared Module",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.new_rust_script_kind,
                            RustScriptKind::Component,
                            "Component",
                        );
                        ui.selectable_value(
                            &mut self.new_rust_script_kind,
                            RustScriptKind::Resource,
                            "Resource",
                        );
                        ui.selectable_value(
                            &mut self.new_rust_script_kind,
                            RustScriptKind::System,
                            "System",
                        );
                        ui.selectable_value(
                            &mut self.new_rust_script_kind,
                            RustScriptKind::SharedModule,
                            "Shared Module",
                        );
                    });
                ui.label("Rust type name");
                ui.text_edit_singleline(&mut self.new_rust_script_name);
                if self.new_rust_script_kind == RustScriptKind::System {
                    egui::ComboBox::from_label("Schedule")
                        .selected_text(match self.new_rust_script_schedule {
                            RustScriptSchedule::Update => "Update",
                            RustScriptSchedule::FixedUpdate => "Fixed Update",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.new_rust_script_schedule,
                                RustScriptSchedule::Update,
                                "Update",
                            );
                            ui.selectable_value(
                                &mut self.new_rust_script_schedule,
                                RustScriptSchedule::FixedUpdate,
                                "Fixed Update",
                            );
                        });
                }
                control_row(ui, |ui| {
                    if ui.button("Create").clicked()
                        && let Some(project) = self.project_root.clone() {
                            // Folders below the Rust root are free-form, so the
                            // browser's working folder wins and the recommended
                            // folder for the kind is only the fallback.
                            let script_folder = self
                                .asset_browser
                                .selected_folder()
                                .strip_prefix("scripts/rust")
                                .map(Path::to_path_buf)
                                .unwrap_or_else(|_| {
                                    PathBuf::from(self.new_rust_script_kind.recommended_folder())
                                });
                            match engine_authoring::create_rust_script_in(
                                &project,
                                self.new_rust_script_kind,
                                &script_folder,
                                &self.new_rust_script_name,
                                self.new_rust_script_schedule,
                            ) {
                                Ok(path) => {
                                    self.session.push_diagnostic(
                                        engine_authoring::Diagnostic::info(
                                            "editor.rust_script_created",
                                            format!("created {}", path.display()),
                                        ),
                                    );
                                    self.refresh_game_code_browser(Some(&path));
                                    self.bottom_panel_tab = BottomPanelTab::Assets;
                                    self.bottom_panel_open = true;
                                    self.request_game_build_after_edit();
                                    self.show_new_rust_script = false;
                                }
                                Err(error) => self.session.push_diagnostic(
                                    engine_authoring::Diagnostic::error(
                                        "editor.rust_script_create_failed",
                                        error.to_string(),
                                    ),
                                ),
                            }
                        }
                    if ui.button("Cancel").clicked() {
                        self.show_new_rust_script = false;
                    }
                });
            });
        if !open {
            self.show_new_rust_script = false;
        }
    }

    pub(super) fn show_source_viewer_window(&mut self, context: &egui::Context) {
        let Some(document) = self.source_viewer.as_mut() else {
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Built-in Source (Read-only) - {}", document.component_id))
            .open(&mut open)
            .default_size([760.0, 620.0])
            .show(context, |ui| {
                control_row(ui, |ui| {
                    ui.strong("Engine-owned / Read-only");
                    ui.label(format!("SDK {}", document.sdk_version));
                    ui.monospace(format!("{}:{}", document.relative_path.display(), document.line));
                });
                if let Some(reason) = &document.missing_reason {
                    ui.colored_label(egui::Color32::YELLOW, reason);
                    if ui.button("Show SDK Repair Instructions").clicked() {
                        ui.ctx().copy_text("Reinstall the matching GameEngine editor SDK source bundle or set GAMEENGINE_SDK_SOURCE_ROOT.".to_owned());
                    }
                    return;
                }
                if let Some(source) = document.source.as_mut() {
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(source)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
                }
            });
        if !open {
            self.source_viewer = None;
        }
    }

    pub(super) fn perform_component_source_action(&mut self, action: ComponentSourceAction) {
        match action {
            ComponentSourceAction::OpenProject(component_id) => {
                let Some(location) = self.component_source_index.resolve(&component_id).cloned()
                else {
                    let message = if self.component_source_index.is_ambiguous(&component_id) {
                        format!("component ID `{component_id}` is ambiguous; resolve duplicate declarations first")
                    } else {
                        format!("no sidecar-backed GameComponent source was indexed for `{component_id}`")
                    };
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.component_source.unavailable",
                            message,
                        ));
                    return;
                };
                let Some(project) = self.project_root.as_ref() else {
                    return;
                };
                let path = project.rust_scripts_dir().join(&location.relative_path);
                if let Err(error) = self.preferences.open_source(&path, location.line, 1) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.external_editor_failed",
                            format!("could not open {}: {error}", path.display()),
                        ));
                }
            }
            ComponentSourceAction::RevealProject(component_id) => {
                if let Some(relative) = self
                    .component_source_index
                    .resolve(&component_id)
                    .map(|location| Path::new("scripts/rust").join(&location.relative_path))
                {
                    self.reveal_asset_in_browser(&relative);
                }
            }
            ComponentSourceAction::ViewBuiltin(component_id) => {
                self.source_viewer = Some(ReadOnlySourceDocument::load(&component_id));
            }
        }
    }

    pub(super) fn bake_current_navmesh(&mut self) {
        if self.navigation_bake.is_running() {
            return;
        }
        let (Some(project), Some(scene)) =
            (self.project_root.clone(), self.session.scene().cloned())
        else {
            return;
        };
        let tab_id = self.session.active_tab_id();
        let scene_stem = self
            .session
            .current_document_path()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .unwrap_or("arena")
            .trim_end_matches(".scene")
            .to_owned();
        let document_path = project
            .assets_root()
            .join("navigation")
            .join(format!("{scene_stem}.navmesh.bake.json"));
        let document = std::fs::read_to_string(&document_path)
            .ok()
            .and_then(|json| crate::navmesh_bake::NavMeshBakeDocument::from_json(&json).ok())
            .unwrap_or_else(|| crate::navmesh_bake::NavMeshBakeDocument {
                output_asset: format!("navigation/{scene_stem}.navmesh.json"),
                ..crate::navmesh_bake::NavMeshBakeDocument::default()
            });
        match self.navigation_bake.start(
            tab_id,
            scene,
            project,
            self.asset_manifest.clone(),
            document,
            document_path,
        ) {
            Ok(()) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.navmesh_bake_started",
                    "navigation bake started in the background",
                )),
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.navmesh_bake_start_failed",
                    error,
                )),
        }
    }

    pub(super) fn cancel_current_navmesh_bake(&mut self) {
        if self.navigation_bake.cancel() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.navmesh_bake_cancelling",
                    "navigation bake cancellation requested",
                ));
        }
    }

    pub(super) fn handle_navigation_bake_completion(
        &mut self,
        completion: NavigationBakeCompletion,
    ) {
        match completion {
            NavigationBakeCompletion::Succeeded {
                tab_id,
                project,
                manifest,
                result,
            } => {
                self.asset_manifest = manifest;
                let component_type = engine_authoring::ComponentTypeId::new(
                    engine::scene_bridge::NAV_MESH_SURFACE_COMPONENT,
                );
                let Some(session) = self.session.tab_session_mut(tab_id) else {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.navmesh_bake_tab_closed",
                        "navigation bake completed after its source scene tab was closed",
                    ));
                    return;
                };
                let existing_surface = session.scene().and_then(|scene| {
                    scene
                        .entities()
                        .find(|(_, entity)| entity.components.contains_key(&component_type))
                        .map(|(id, _)| id.clone())
                });
                if existing_surface.is_none() {
                    let added = session
                        .create_scene_entity("navmesh_surface")
                        .and_then(|entity| {
                            session.add_scene_component(
                                entity,
                                component_type,
                                engine_authoring::Value::Object(std::collections::BTreeMap::from(
                                    [(
                                        "source".to_owned(),
                                        engine_authoring::Value::AssetRef(result.asset_id.clone()),
                                    )],
                                )),
                            )
                        });
                    if let Err(error) = added {
                        session.push_diagnostic(engine_authoring::Diagnostic::warning(
                            "editor.navmesh_surface_add_failed",
                            error.to_string(),
                        ));
                    }
                }
                self.asset_browser.refresh(&project.assets_root());
                session.push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.navmesh_bake_succeeded",
                    format!(
                        "baked NavMesh to {} ({} profiles, {} tiles, {} polygons, {} links)",
                        result.output_path.display(),
                        result.stats.profile_count,
                        result.stats.tile_count,
                        result.stats.polygon_count,
                        result.stats.link_count
                    ),
                ));
            }
            NavigationBakeCompletion::Cancelled { tab_id } => {
                if let Some(session) = self.session.tab_session_mut(tab_id) {
                    session.push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.navmesh_bake_cancelled",
                        "navigation bake cancelled; the previous valid artifact was preserved",
                    ));
                }
            }
            NavigationBakeCompletion::Failed { tab_id, error } => {
                let diagnostic = engine_authoring::Diagnostic::error(
                    "editor.navmesh_bake_failed",
                    error,
                );
                if let Some(session) = self.session.tab_session_mut(tab_id) {
                    session.push_diagnostic(diagnostic);
                } else {
                    self.session.push_diagnostic(diagnostic);
                }
            }
        }
    }

    pub(super) fn package_open_project(&mut self) {
        let Some(project_root) = self.project_root.clone() else {
            return;
        };
        let settings = match ProjectSettings::load(project_root.path()) {
            Ok(settings) => settings,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.package_settings_failed",
                        format!("cannot load project settings: {error}"),
                    ));
                return;
            }
        };
        let Some(player_binary) = find_player_binary() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.package_player_missing",
                    "build the player first with `cargo build -p engine --release --bin player`",
                ));
            return;
        };
        let Some(output_dir) = rfd::FileDialog::new()
            .set_title("Select package output folder")
            .pick_folder()
        else {
            return;
        };
        let config = BuildConfig {
            project_root: project_root.path().to_path_buf(),
            output_dir: output_dir.clone(),
            start_scene: settings.start_scene,
        };
        if project_root.game_dir().join("Cargo.toml").is_file() {
            self.pending_game_package = Some(PendingGamePackage {
                config,
                player_binary,
            });
            if !self.start_game_build(GameBuildKind::Release) {
                self.pending_game_package = None;
            }
            return;
        }
        match package_project(&config, &self.asset_manifest, &player_binary) {
            Ok(plan) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.package_succeeded",
                        format!(
                            "packaged {} project files to {}",
                            plan.copies.len() + 2,
                            output_dir.display()
                        ),
                    ));
            }
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.package_failed",
                        format!("package failed: {error}"),
                    ));
            }
        }
    }
}

pub(super) enum ComponentSourceAction {
    OpenProject(String),
    RevealProject(String),
    ViewBuiltin(String),
}
