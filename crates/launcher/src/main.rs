use eframe::egui;
use engine_project_lifecycle::{
    acquire_launcher, create_standard_project, editor_is_ready, launch_or_activate_editor,
    request_editor_close, EditorLaunchOutcome, LauncherRequest, LauncherSession,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT_PROJECTS: usize = 10;

#[derive(Debug, Default, Serialize, Deserialize)]
struct LauncherPreferences {
    recent_projects: Vec<PathBuf>,
}

impl LauncherPreferences {
    fn load() -> Self {
        let Some(path) = preferences_path() else {
            return Self::default();
        };
        let Ok(data) = std::fs::read(path) else {
            return Self::default();
        };
        let Ok(mut preferences) = serde_json::from_slice::<Self>(&data) else {
            return Self::default();
        };
        preferences.recent_projects.retain(|path| path.is_dir());
        preferences
    }

    fn save(&self) {
        let Some(path) = preferences_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    fn push_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|candidate| candidate != path);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(MAX_RECENT_PROJECTS);
        self.save();
    }
}

fn preferences_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("engine_launcher").join("preferences.json"))
}

struct PendingSwitch {
    source: PathBuf,
    target: PathBuf,
}

struct LauncherApp {
    session: LauncherSession,
    preferences: LauncherPreferences,
    new_project_name: String,
    status: Option<String>,
    switch_from: Option<PathBuf>,
    pending_switch: Option<PendingSwitch>,
}

impl LauncherApp {
    fn new(session: LauncherSession) -> Self {
        Self {
            session,
            preferences: LauncherPreferences::load(),
            new_project_name: "NewGame".to_owned(),
            status: None,
            switch_from: None,
            pending_switch: None,
        }
    }

    fn apply_request(&mut self, request: LauncherRequest, context: &egui::Context) {
        self.switch_from = request.switch_from;
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn open_project(&mut self, path: PathBuf) {
        match launch_or_activate_editor(&path) {
            Ok(launch) => {
                self.preferences.push_recent(&launch.canonical_project);
                let verb = match launch.outcome {
                    EditorLaunchOutcome::Spawned => "Started",
                    EditorLaunchOutcome::Activated => "Activated",
                };
                self.status = Some(format!(
                    "{verb} Editor for {}",
                    launch.canonical_project.display()
                ));

                if let Some(source) = self.switch_from.take()
                    && source != launch.canonical_project
                {
                    self.pending_switch = Some(PendingSwitch {
                        source,
                        target: launch.canonical_project,
                    });
                }
            }
            Err(error) => {
                self.status = Some(format!("Could not open project: {error}"));
            }
        }
    }

    fn create_project(&mut self) {
        let name = self.new_project_name.trim();
        if name.is_empty() {
            self.status = Some("Enter a project name first.".to_owned());
            return;
        }
        let Some(parent) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let final_path = parent.join(name);
        match create_standard_project(&final_path, name) {
            Ok(project) => {
                let path = project.path().to_path_buf();
                self.preferences.push_recent(&path);
                self.status = Some(format!("Created {}", path.display()));
                self.open_project(path);
            }
            Err(error) => {
                self.status = Some(format!("Could not create project: {error}"));
            }
        }
    }

    fn poll_switch(&mut self, context: &egui::Context) {
        let Some(pending) = self.pending_switch.as_ref() else {
            return;
        };
        context.request_repaint_after(std::time::Duration::from_millis(100));
        match editor_is_ready(&pending.target) {
            Ok(true) => {
                let source = pending.source.clone();
                let target = pending.target.clone();
                self.pending_switch = None;
                match request_editor_close(&source) {
                    Ok(()) => {
                        self.status = Some(format!(
                            "Switched from {} to {}",
                            source.display(),
                            target.display()
                        ));
                    }
                    Err(error) => {
                        self.status = Some(format!(
                            "Target Editor is ready, but source close failed: {error}"
                        ));
                    }
                }
            }
            Ok(false) => {}
            Err(error) => {
                self.pending_switch = None;
                self.status = Some(format!("Target Editor readiness check failed: {error}"));
            }
        }
    }
}

impl eframe::App for LauncherApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(request) = self.session.take_request() {
            self.apply_request(request, context);
        }
        self.poll_switch(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(36.0);
                ui.heading("GameEngine Launcher");
                ui.label("Choose a project before starting the Editor.");
                ui.add_space(20.0);
            });

            ui.group(|ui| {
                ui.heading("New Project");
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.new_project_name);
                    if ui.button("Create...").clicked() {
                        self.create_project();
                    }
                });
                ui.small("Choose the parent folder after clicking Create.");
            });

            ui.add_space(12.0);
            if ui.button("Open Project...").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.open_project(path);
            }

            if !self.preferences.recent_projects.is_empty() {
                ui.add_space(18.0);
                ui.separator();
                ui.heading("Recent Projects");
                let recent = self.preferences.recent_projects.clone();
                for path in recent {
                    let label = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    if ui.button(label).on_hover_text(path.display().to_string()).clicked() {
                        self.open_project(path);
                    }
                }
            }

            if let Some(source) = &self.switch_from {
                ui.add_space(18.0);
                ui.separator();
                ui.label(format!(
                    "Switching from {}. The current Editor stays open until the target is ready.",
                    source.display()
                ));
                if ui.button("Cancel project switch").clicked() {
                    self.switch_from = None;
                    self.pending_switch = None;
                }
            }

            if let Some(pending) = &self.pending_switch {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Waiting for {} to finish opening...",
                        pending.target.display()
                    ));
                });
            }

            if let Some(status) = &self.status {
                ui.add_space(18.0);
                ui.separator();
                ui.label(status);
            }
        });
    }
}

fn parse_switch_from() -> Result<Option<PathBuf>, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut switch_from = None;
    while let Some(argument) = arguments.next() {
        if argument == "--switch-from" {
            let path = arguments
                .next()
                .ok_or_else(|| "--switch-from requires a project path".to_owned())?;
            switch_from = Some(PathBuf::from(path));
        } else {
            return Err(format!(
                "unknown Launcher argument `{}`",
                argument.to_string_lossy()
            ));
        }
    }
    Ok(switch_from)
}

fn run() -> Result<(), String> {
    let switch_from = parse_switch_from()?;
    let Some(session) = acquire_launcher(switch_from).map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([620.0, 480.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Launcher",
        options,
        Box::new(move |_creation_context| Ok(Box::new(LauncherApp::new(session)))),
    )
    .map_err(|error| error.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[launcher.startup_failed] {error}");
        std::process::exit(1);
    }
}
