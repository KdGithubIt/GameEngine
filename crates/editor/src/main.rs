mod mcp_transport;

use std::path::PathBuf;
use std::sync::mpsc;

use engine_editor::{AuthoringTool, AuthoringWindows};
use engine_project_lifecycle::{acquire_editor_project, EditorLease};
use mcp_transport::{
    EditorMcpHostResult, EditorMcpRequest, EditorMcpServer, MCP_PROTOCOL_VERSION,
};

/// Keeps the native swapchain clear color aligned with the editor theme.
///
/// During document and scene switches egui can briefly have no painted shape
/// covering the root viewport. A fixed dark clear color prevents a transient
/// backend clear frame from flashing through.
struct EditorShell {
    app: engine_editor::EditorApp,
    authoring_windows: AuthoringWindows,
    show_authoring_tools: bool,
    authoring_status: Option<String>,
    project_lease: EditorLease,
    _mcp_server: EditorMcpServer,
    mcp_requests: mpsc::Receiver<EditorMcpRequest>,
}

impl EditorShell {
    fn new(
        project_lease: EditorLease,
        context: &eframe::egui::Context,
    ) -> Result<Self, String> {
        let root = project_lease.project_root().clone();
        let app = engine_editor::EditorApp::from_project(root);
        let (mcp_server, mcp_requests) =
            EditorMcpServer::start(context.clone()).map_err(|error| error.to_string())?;
        project_lease
            .publish_mcp_endpoint(
                mcp_server.endpoint(),
                MCP_PROTOCOL_VERSION,
                mcp_server.authorization_token(),
            )
            .map_err(|error| error.to_string())?;
        project_lease.mark_ready().map_err(|error| error.to_string())?;
        Ok(Self {
            app,
            authoring_windows: AuthoringWindows::default(),
            show_authoring_tools: false,
            authoring_status: None,
            project_lease,
            _mcp_server: mcp_server,
            mcp_requests,
        })
    }

    fn handle_mcp_requests(&mut self) {
        while let Ok(request) = self.mcp_requests.try_recv() {
            let result = self
                .app
                .handle_mcp_tool_call(request.name(), request.arguments().clone());
            request.respond(match result {
                Ok(value) => EditorMcpHostResult::Success(value),
                Err(error) => EditorMcpHostResult::ToolError {
                    code: error.code().to_owned(),
                    message: error.message().to_owned(),
                },
            });
        }
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

        let project = Some(self.app.project_root().path().to_path_buf());
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
                    "All authoring tools share this editor process. Runtime Event Timeline keeps its existing live-capture launch flow inside the embedded viewer window.",
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
        self.handle_mcp_requests();
        if self.project_lease.take_activation_request() {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
        }
        if self.project_lease.take_close_request() {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            return;
        }
        eframe::App::logic(&mut self.app, context, frame);
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        eframe::App::ui(&mut self.app, ui, frame);
        self.show_authoring_tools_launcher(&context);
        self.authoring_windows
            .show(&context, frame, self.app.project_root());
    }

    fn clear_color(&self, _visuals: &eframe::egui::Visuals) -> [f32; 4] {
        eframe::egui::Color32::from_rgb(20, 22, 26).to_normalized_gamma_f32()
    }
}

fn project_argument() -> Result<PathBuf, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut project = None;
    while let Some(argument) = arguments.next() {
        if argument == "--project" {
            if project.is_some() {
                return Err("--project may be specified only once".to_owned());
            }
            project = Some(PathBuf::from(
                arguments
                    .next()
                    .ok_or_else(|| "--project requires a path".to_owned())?,
            ));
        } else {
            return Err(format!(
                "unknown Editor argument `{}`",
                argument.to_string_lossy()
            ));
        }
    }
    project.ok_or_else(|| "Engine Editor requires `--project <path>`".to_owned())
}

fn run() -> Result<(), String> {
    let project = project_argument()?;
    let project_lease = acquire_editor_project(&project).map_err(|error| error.to_string())?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "Engine Editor",
        options,
        Box::new(move |creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(
                EditorShell::new(project_lease, &creation_context.egui_ctx)
                    .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                        Box::new(std::io::Error::other(error))
                    })?,
            ))
        }),
    )
    .map_err(|error| error.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[editor.startup_failed] {error}");
        std::process::exit(1);
    }
}
