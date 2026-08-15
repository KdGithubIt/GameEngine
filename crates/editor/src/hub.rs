//! Project Hub startup screen.

use crate::preferences::EditorPreferences;
use eframe::egui;
use std::path::PathBuf;

/// Action requested from the Project Hub UI.
pub enum HubAction {
    /// Create a new project scaffold inside the given directory.
    NewProject(PathBuf),
    /// Open an existing project directory.
    OpenProject(PathBuf),
}

/// Draws the Project Hub and returns the action selected by the user, if any.
pub fn show_hub(ui: &mut egui::Ui, prefs: &EditorPreferences) -> Option<HubAction> {
    let mut action = None;

    ui.vertical_centered(|ui| {
        ui.add_space(60.0);
        ui.heading("Engine Editor");
        ui.add_space(8.0);
        ui.label("Create or open a project to get started.");
        ui.add_space(24.0);

        if ui.button("  New Project\u{2026}  ").clicked()
            && let Some(folder) = rfd::FileDialog::new().pick_folder() {
                action = Some(HubAction::NewProject(folder));
            }
        ui.add_space(8.0);
        if ui.button("  Open Project\u{2026}  ").clicked()
            && let Some(folder) = rfd::FileDialog::new().pick_folder() {
                action = Some(HubAction::OpenProject(folder));
            }

        if !prefs.recent_projects.is_empty() {
            ui.add_space(24.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label("Recent Projects");
            ui.add_space(8.0);
            for path in &prefs.recent_projects {
                let label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let tooltip = path.to_string_lossy().into_owned();
                if ui.button(&label).on_hover_text(tooltip).clicked() {
                    action = Some(HubAction::OpenProject(path.clone()));
                }
            }
        }
    });

    action
}
