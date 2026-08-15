use std::path::PathBuf;

use engine_editor::{AuthoringTool, AuthoringWindows};

/// Keeps the native swapchain clear color aligned with the editor theme.
///
/// During document and scene switches egui can briefly have no painted shape
/// covering the root viewport. A fixed dark clear color prevents a transient
/// backend clear frame from flashing through.
#[derive(Default)]
struct EditorShell {
    app: engine_editor::EditorApp,
    authoring_windows: AuthoringWindows,
    show_authoring_tools: bool,
    authoring_status: Option<String>,
}

impl EditorShell {
    fn authoring_project() -> Option<PathBuf> {
        let preferences = engine_editor::preferences::EditorPreferences::load();
        preferences
            .last_project
            .or_else(|| preferences.recent_projects.into_iter().next())
            .filter(|path| path.is_dir())
    }

    fn show_authoring_tools_launcher(&mut self, context: &eframe::egui::Context) {
        eframe::egui::Area::new(eframe::egui::Id::new("authoring_tools_launcher"))
            .anchor(
                eframe::egui::Align2::RIGHT_TOP,
                // Keep the launcher vertically centered in the unified
                // 40-point toolbar below the 28-point menu bar.
                eframe::egui::vec2(-12.0, 36.0),
            )
            .order(eframe::egui::Order::Foreground)
            .show(context, |ui| {
                if ui.button("Authoring Tools").clicked() {
                    self.show_authoring_tools = true;
                }
            });

        if !self.show_authoring_tools {
            return;
        }

        let project = Self::authoring_project();
        let mut requested_tool = None;
        let mut open = self.show_authoring_tools;
        eframe::egui::Window::new("Project Authoring Tools")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .show(context, |ui| {
                ui.label("Open modeless authoring windows inside the current Engine Editor.");
                match &project {
                    Some(path) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("Project");
                            ui.monospace(path.display().to_string());
                        });
                    }
                    None => {
                        ui.colored_label(
                            eframe::egui::Color32::YELLOW,
                            "Open a project in Engine Editor before opening an authoring window.",
                        );
                    }
                }
                ui.separator();

                for tool in AuthoringTool::ALL {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    project.is_some(),
                                    eframe::egui::Button::new(tool.label()),
                                )
                                .clicked()
                            {
                                requested_tool = Some(tool);
                            }
                            ui.label(tool.description());
                        });
                    });
                }

                ui.separator();
                ui.small(
                    "All four tools now share this editor process. Runtime Event Timeline keeps its existing live-capture launch flow inside the embedded viewer window.",
                );
                if let Some(status) = &self.authoring_status {
                    ui.separator();
                    ui.label(status);
                }
            });
        self.show_authoring_tools = open;

        if let (Some(tool), Some(project)) = (requested_tool, project.as_deref()) {
            self.authoring_windows.open(tool);
            self.authoring_status = Some(format!(
                "Opened {} inside Engine Editor for {}",
                tool.label(),
                project.display()
            ));
        }
    }
}

impl eframe::App for EditorShell {
    fn logic(&mut self, context: &eframe::egui::Context, frame: &mut eframe::Frame) {
        eframe::App::logic(&mut self.app, context, frame);
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        eframe::App::ui(&mut self.app, ui, frame);
        self.show_authoring_tools_launcher(&context);
        self.authoring_windows.show(&context, frame);
    }

    fn clear_color(&self, _visuals: &eframe::egui::Visuals) -> [f32; 4] {
        eframe::egui::Color32::from_rgb(20, 22, 26).to_normalized_gamma_f32()
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "Engine Editor",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(EditorShell::default()))
        }),
    )
}
