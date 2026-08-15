//! Project-wide ECS Systems panel and testable state transformations.

use eframe::egui;
use engine::ecs::{ScheduleDiagnostic, ScheduleEntryInfo, SystemId, SystemOrigin};
use engine::{register_runtime_systems, App};
use engine_authoring::project_settings::{
    ProjectSystemSchedule, SystemScheduleSettings, SystemSettings, SYSTEM_SETTINGS_SCHEMA_VERSION,
};
use engine_authoring::{ProjectRoot, ProjectSettings};
use std::path::PathBuf;
use std::sync::Arc;

/// Save state displayed next to project-wide system settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemsSaveState {
    /// No project is open.
    NoProject,
    /// In-memory settings match the atomic project file.
    Saved,
    /// A change has not yet been written successfully.
    Unsaved,
    /// The latest atomic save failed and can be retried.
    SaveFailed(String),
}

/// Editor state for querying and editing the same descriptors used by runtime.
pub struct SystemsPanel {
    catalog: Option<App>,
    settings: ProjectSettings,
    project_path: Option<PathBuf>,
    schedule: ProjectSystemSchedule,
    search: String,
    show_engine: bool,
    show_game: bool,
    save_state: SystemsSaveState,
    diagnostics: Vec<String>,
    game_module_issue: Option<String>,
    is_read_only: bool,
}

impl Default for SystemsPanel {
    fn default() -> Self {
        Self {
            catalog: None,
            settings: ProjectSettings::default(),
            project_path: None,
            schedule: ProjectSystemSchedule::Update,
            search: String::new(),
            show_engine: true,
            show_game: true,
            save_state: SystemsSaveState::NoProject,
            diagnostics: Vec::new(),
            game_module_issue: None,
            is_read_only: false,
        }
    }
}

impl SystemsPanel {
    /// Rebuilds the catalog after opening a project or loading a new module generation.
    pub fn open_project(
        &mut self,
        project: &ProjectRoot,
        game_module: Option<Arc<engine::game_module::GameModule>>,
        game_module_issue: Option<String>,
    ) {
        self.project_path = Some(project.path().to_path_buf());
        self.game_module_issue = game_module_issue;
        self.is_read_only =
            project.game_dir().join("Cargo.toml").is_file() && game_module.is_none();
        self.diagnostics.clear();
        self.settings = match ProjectSettings::load(project.path()) {
            Ok(settings) => settings,
            Err(error) => {
                self.diagnostics.push(format!(
                    "Could not load project settings; defaults are shown: {error}"
                ));
                self.is_read_only = true;
                ProjectSettings::default()
            }
        };

        let mut app = App::new();
        if let Err(error) = register_runtime_systems(&mut app) {
            self.diagnostics
                .push(format!("Could not build Engine system catalog: {error}"));
            self.catalog = None;
            self.save_state = SystemsSaveState::NoProject;
            return;
        }
        if let Some(module) = game_module
            && let Err(error) = app.try_register_game_module_systems(module) {
                self.diagnostics
                    .push(format!("Could not add Game systems: {error}"));
                self.is_read_only = true;
            }
        match app.apply_system_settings(&self.settings.system_settings) {
            Ok(report) => {
                self.diagnostics.extend(
                    report
                        .update
                        .into_iter()
                        .map(|item| format_schedule_diagnostic("Update", item)),
                );
                self.diagnostics.extend(
                    report
                        .fixed_update
                        .into_iter()
                        .map(|item| format_schedule_diagnostic("FixedUpdate", item)),
                );
                self.diagnostics.extend(
                    report
                        .invalid_ids
                        .into_iter()
                        .map(|id| format!("Ignored invalid configured system ID `{id}`")),
                );
                self.catalog = Some(app);
                self.save_state = SystemsSaveState::Saved;
            }
            Err(error) => {
                self.diagnostics
                    .push(format!("System constraint graph is invalid: {error}"));
                self.catalog = Some(app);
                self.is_read_only = true;
                self.save_state = SystemsSaveState::Unsaved;
            }
        }
    }

    /// Sets the active schedule shown by filtering and movement operations.
    pub fn set_schedule(&mut self, schedule: ProjectSystemSchedule) {
        self.schedule = schedule;
    }

    /// Sets case-insensitive text used to match display names and IDs.
    pub fn set_search(&mut self, search: impl Into<String>) {
        self.search = search.into();
    }

    /// Returns visible rows after schedule, search, and origin filtering.
    pub fn visible_systems(&self) -> Vec<ScheduleEntryInfo> {
        let search = self.search.trim().to_ascii_lowercase();
        self.catalog
            .as_ref()
            .map(|catalog| catalog.system_infos(self.schedule))
            .unwrap_or_default()
            .into_iter()
            .filter(|info| match info.descriptor.origin() {
                SystemOrigin::Engine | SystemOrigin::Unnamed => self.show_engine,
                SystemOrigin::Game => self.show_game,
            })
            .filter(|info| {
                search.is_empty()
                    || info
                        .descriptor
                        .display_name()
                        .to_ascii_lowercase()
                        .contains(&search)
                    || info
                        .descriptor
                        .id()
                        .as_str()
                        .to_ascii_lowercase()
                        .contains(&search)
            })
            .collect()
    }

    /// Changes one entry's enabled state and atomically saves project settings.
    pub fn set_enabled(&mut self, id: &SystemId, is_enabled: bool) -> Result<(), String> {
        self.ensure_editable()?;
        let catalog = self.catalog.as_mut().ok_or("No system catalog is loaded")?;
        match self.schedule {
            ProjectSystemSchedule::Update => {
                catalog.ecs_mut().set_update_system_enabled(id, is_enabled)
            }
            ProjectSystemSchedule::FixedUpdate => {
                catalog.ecs_mut().set_fixed_system_enabled(id, is_enabled)
            }
        }
        .map_err(|error| error.to_string())?;
        self.mark_dirty_and_save()
    }

    /// Moves one entry by a signed number of positions when constraints allow.
    pub fn move_by(&mut self, id: &SystemId, delta: isize) -> Result<(), String> {
        self.ensure_editable()?;
        let catalog = self.catalog.as_mut().ok_or("No system catalog is loaded")?;
        let infos = catalog.system_infos(self.schedule);
        let current = infos
            .iter()
            .find(|info| info.descriptor.id() == id)
            .map(|info| info.order)
            .ok_or_else(|| format!("System `{id}` is not registered"))?;
        let target = current
            .saturating_add_signed(delta)
            .min(infos.len().saturating_sub(1));
        match self.schedule {
            ProjectSystemSchedule::Update => catalog.ecs_mut().move_update_system(id, target),
            ProjectSystemSchedule::FixedUpdate => catalog.ecs_mut().move_fixed_system(id, target),
        }
        .map_err(|error| error.to_string())?;
        self.mark_dirty_and_save()
    }

    /// Restores one schedule to descriptor registration order and enabled state.
    pub fn reset_current_schedule(&mut self) -> Result<(), String> {
        self.ensure_editable()?;
        let catalog = self.catalog.as_mut().ok_or("No system catalog is loaded")?;
        match self.schedule {
            ProjectSystemSchedule::Update => catalog.ecs_mut().reset_update_systems(),
            ProjectSystemSchedule::FixedUpdate => catalog.ecs_mut().reset_fixed_systems(),
        }
        .map_err(|error| error.to_string())?;
        self.mark_dirty_and_save()
    }

    /// Retries an atomic save after an I/O failure.
    pub fn retry_save(&mut self) -> Result<(), String> {
        self.sync_settings_from_catalog();
        self.save()
    }

    fn ensure_editable(&self) -> Result<(), String> {
        if self.is_read_only {
            Err(
                "System settings are read-only until the project Game module loads successfully"
                    .to_owned(),
            )
        } else {
            Ok(())
        }
    }

    fn mark_dirty_and_save(&mut self) -> Result<(), String> {
        self.save_state = SystemsSaveState::Unsaved;
        self.sync_settings_from_catalog();
        self.save()
    }

    fn sync_settings_from_catalog(&mut self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        self.settings.system_settings = SystemSettings {
            schema_version: SYSTEM_SETTINGS_SCHEMA_VERSION,
            update: to_persisted(catalog.ecs().update_configuration()),
            fixed_update: to_persisted(catalog.ecs().fixed_configuration()),
        };
    }

    fn save(&mut self) -> Result<(), String> {
        let Some(project_path) = self.project_path.as_ref() else {
            self.save_state = SystemsSaveState::NoProject;
            return Err("No project is open".to_owned());
        };
        match self.settings.save(project_path) {
            Ok(()) => {
                self.save_state = SystemsSaveState::Saved;
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                self.save_state = SystemsSaveState::SaveFailed(message.clone());
                Err(message)
            }
        }
    }
}

/// Draws the left-dock Systems tab.
pub fn show_systems_panel(panel: &mut SystemsPanel, ui: &mut egui::Ui) {
    ui.small(
        "Systems run only when matching components exist; disabling here is for \
         debugging and profiling. Settings are project-wide, not per-scene.",
    );
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut panel.schedule, ProjectSystemSchedule::Update, "Update");
        ui.selectable_value(
            &mut panel.schedule,
            ProjectSystemSchedule::FixedUpdate,
            "FixedUpdate",
        );
        ui.checkbox(&mut panel.show_engine, "Engine");
        ui.checkbox(&mut panel.show_game, "Game");
    });
    ui.add(
        egui::TextEdit::singleline(&mut panel.search)
            .hint_text("Search systems...")
            .desired_width(ui.available_width()),
    );
    ui.horizontal_wrapped(|ui| {
        ui.small("Changes apply to the next Play runtime.");
        ui.separator();
        match &panel.save_state {
            SystemsSaveState::NoProject => {
                ui.label("No project");
            }
            SystemsSaveState::Saved => {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Saved");
            }
            SystemsSaveState::Unsaved => {
                ui.colored_label(egui::Color32::YELLOW, "Unsaved");
            }
            SystemsSaveState::SaveFailed(error) => {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Save failed: {error}"));
                if ui.small_button("Retry Save").clicked() {
                    let _ = panel.retry_save();
                }
            }
        }
        if ui
            .add_enabled(!panel.is_read_only, egui::Button::new("Reset to Default"))
            .clicked()
            && let Err(error) = panel.reset_current_schedule() {
                panel.diagnostics.push(error);
            }
    });
    if let Some(issue) = &panel.game_module_issue {
        ui.colored_label(
            egui::Color32::YELLOW,
            format!("Game systems unavailable: {issue}"),
        );
    }
    for diagnostic in &panel.diagnostics {
        ui.colored_label(egui::Color32::YELLOW, diagnostic);
    }
    ui.separator();

    let rows = panel.visible_systems();
    // Row positions are schedule-global, even when search/origin filters hide
    // entries. Use the full schedule length so a visible row can still move
    // below a hidden neighbor.
    let count = panel
        .catalog
        .as_ref()
        .map(|catalog| catalog.system_infos(panel.schedule).len())
        .unwrap_or_default();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for info in rows {
                let id = info.descriptor.id().clone();
                let mut enabled = info.is_enabled;
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:>3}", info.order + 1));
                    if ui
                        .add_enabled(
                            !panel.is_read_only,
                            egui::Checkbox::without_text(&mut enabled),
                        )
                        .changed()
                        && let Err(error) = panel.set_enabled(&id, enabled) {
                            panel.diagnostics.push(error);
                        }
                    let origin = match info.descriptor.origin() {
                        SystemOrigin::Engine => "Engine",
                        SystemOrigin::Game => "Game",
                        SystemOrigin::Unnamed => "Unnamed",
                    };
                    ui.label(origin);
                    ui.strong(info.descriptor.display_name());
                    ui.monospace(info.descriptor.id().as_str());
                    if ui
                        .add_enabled(
                            !panel.is_read_only && info.order > 0,
                            egui::Button::new("↑"),
                        )
                        .clicked()
                        && let Err(error) = panel.move_by(&id, -1) {
                            panel.diagnostics.push(error);
                        }
                    if ui
                        .add_enabled(
                            !panel.is_read_only && info.order + 1 < count,
                            egui::Button::new("↓"),
                        )
                        .clicked()
                        && let Err(error) = panel.move_by(&id, 1) {
                            panel.diagnostics.push(error);
                        }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(72.0);
                    if !info.descriptor.description().is_empty() {
                        ui.label(info.descriptor.description());
                    }
                    if !info.descriptor.before().is_empty() {
                        ui.monospace(format!(
                            "before: {}",
                            info.descriptor
                                .before()
                                .iter()
                                .map(SystemId::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    if !info.descriptor.after().is_empty() {
                        ui.monospace(format!(
                            "after: {}",
                            info.descriptor
                                .after()
                                .iter()
                                .map(SystemId::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                });
                ui.separator();
            }
        });
}

fn to_persisted(configuration: engine::ecs::ScheduleConfiguration) -> SystemScheduleSettings {
    SystemScheduleSettings {
        order: configuration
            .order
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
        disabled: configuration
            .disabled
            .into_iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
    }
}

fn format_schedule_diagnostic(schedule: &str, diagnostic: ScheduleDiagnostic) -> String {
    match diagnostic {
        ScheduleDiagnostic::UnknownConfiguredSystem(id) => {
            format!("{schedule}: saved system `{id}` is not currently registered")
        }
        ScheduleDiagnostic::MissingConstraintTarget { system, target } => {
            format!("{schedule}: `{system}` references missing constraint target `{target}`")
        }
        ScheduleDiagnostic::MigratedAlias { from, to } => {
            format!("{schedule}: migrated system ID `{from}` to `{to}`")
        }
        ScheduleDiagnostic::ConstraintAdjusted => {
            format!("{schedule}: saved order was adjusted to satisfy constraints")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_and_schedule_filter_use_runtime_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "SystemsPanelTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let mut panel = SystemsPanel::default();
        panel.open_project(&project, None, None);
        panel.set_search("transform");
        let update = panel.visible_systems();
        assert_eq!(update.len(), 1);
        assert_eq!(
            update[0].descriptor.id().as_str(),
            "engine.transform_propagation"
        );
        panel.set_schedule(ProjectSystemSchedule::FixedUpdate);
        let fixed = panel.visible_systems();
        let fixed_ids: Vec<_> = fixed
            .iter()
            .map(|system| system.descriptor.id().as_str())
            .collect();
        assert_eq!(
            fixed_ids,
            [
                "engine.fixed_transform_propagation",
                "engine.mmd_physics_transform_propagation"
            ]
        );
    }

    #[test]
    fn origin_filter_can_hide_engine_systems() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "SystemsPanelOriginTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let mut panel = SystemsPanel::default();
        panel.open_project(&project, None, None);

        assert!(!panel.visible_systems().is_empty());
        panel.show_engine = false;
        assert!(panel.visible_systems().is_empty());
    }

    #[test]
    fn movement_saves_valid_order_and_reports_constraint_violation() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "SystemsPanelMoveTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let mut panel = SystemsPanel::default();
        panel.open_project(&project, None, None);

        let camera = SystemId::try_new("engine.camera_aspect").unwrap();
        let old_position = panel
            .visible_systems()
            .into_iter()
            .find(|info| info.descriptor.id() == &camera)
            .unwrap()
            .order;
        panel.move_by(&camera, -1).unwrap();
        let saved = ProjectSettings::load(project.path()).unwrap();
        assert_eq!(
            saved.system_settings.update.order[old_position - 1],
            camera.as_str()
        );

        let joint_palette = SystemId::try_new("engine.joint_palette").unwrap();
        let error = panel.move_by(&joint_palette, -1).unwrap_err();
        assert!(error.contains("violates a before/after constraint"));
    }

    #[test]
    fn save_failure_preserves_dirty_error_state() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "SystemsPanelSaveFailureTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let mut panel = SystemsPanel::default();
        panel.open_project(&project, None, None);
        panel.project_path = Some(dir.path().join("missing").join("project"));

        let id = SystemId::try_new("engine.transform_propagation").unwrap();
        assert!(panel.set_enabled(&id, false).is_err());
        assert!(matches!(panel.save_state, SystemsSaveState::SaveFailed(_)));
    }

    #[test]
    fn enabled_change_saves_project_settings_and_reset_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "SystemsPanelSaveTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let mut panel = SystemsPanel::default();
        panel.open_project(&project, None, None);
        let id = SystemId::try_new("engine.transform_propagation").unwrap();
        panel.set_enabled(&id, false).unwrap();
        assert_eq!(panel.save_state, SystemsSaveState::Saved);
        let saved = ProjectSettings::load(project.path()).unwrap();
        assert!(saved
            .system_settings
            .update
            .disabled
            .contains(&id.as_str().to_owned()));

        panel.reset_current_schedule().unwrap();
        let saved = ProjectSettings::load(project.path()).unwrap();
        assert!(saved.system_settings.update.disabled.is_empty());
    }
}
