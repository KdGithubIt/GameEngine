mod mcp_transport;

#[cfg(feature = "visual-validation")]
use std::fs::File;
#[cfg(feature = "visual-validation")]
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
#[cfg(feature = "visual-validation")]
use std::time::Instant;

use engine_editor::benchmark_runner::{BenchmarkExperimentOptions, run_benchmark_experiment};
use engine_editor::{AiStudioConnection, AiStudioPanel, AuthoringTool, AuthoringWindows};
use engine_project_lifecycle::{EditorLease, acquire_editor_project};
use mcp_transport::{EditorMcpHostResult, EditorMcpRequest, EditorMcpServer, MCP_PROTOCOL_VERSION};

/// Keeps the native swapchain clear color aligned with the editor theme.
///
/// During document and scene switches egui can briefly have no painted shape
/// covering the root viewport. A fixed dark clear color prevents a transient
/// backend clear frame from flashing through.
struct EditorShell {
    app: engine_editor::EditorApp,
    ai_studio: AiStudioPanel,
    authoring_windows: AuthoringWindows,
    show_authoring_tools: bool,
    authoring_status: Option<String>,
    project_lease: EditorLease,
    _mcp_server: EditorMcpServer,
    mcp_requests: mpsc::Receiver<EditorMcpRequest>,
    #[cfg(feature = "visual-validation")]
    visual_capture_path: Option<PathBuf>,
    #[cfg(feature = "visual-validation")]
    visual_capture_requested_at: Option<Instant>,
    #[cfg(feature = "visual-validation")]
    visual_behavior_debug_capture: bool,
    #[cfg(feature = "visual-validation")]
    visual_ai_studio_detached_capture: bool,
}

impl EditorShell {
    fn new(
        project_lease: EditorLease,
        context: &eframe::egui::Context,
        benchmark_run: Option<&Path>,
    ) -> Result<Self, String> {
        let root = project_lease.project_root().clone();
        #[cfg(feature = "visual-validation")]
        let mut app = engine_editor::EditorApp::from_project(root.clone());
        #[cfg(not(feature = "visual-validation"))]
        let app = engine_editor::EditorApp::from_project(root.clone());
        let (mcp_server, mcp_requests) =
            EditorMcpServer::start(context.clone()).map_err(|error| error.to_string())?;
        project_lease
            .publish_mcp_endpoint(
                mcp_server.endpoint(),
                MCP_PROTOCOL_VERSION,
                mcp_server.authorization_token(),
            )
            .map_err(|error| error.to_string())?;
        let ai_studio_connection = AiStudioConnection::new(
            mcp_server.endpoint().to_string(),
            mcp_server.agent_authorization_token().to_owned(),
            mcp_server.read_only_authorization_token().to_owned(),
        );
        let ai_studio = if let Some(benchmark_run) = benchmark_run {
            AiStudioPanel::new_benchmark_child(&root, ai_studio_connection, benchmark_run)?
        } else {
            AiStudioPanel::new(&root, ai_studio_connection)?
        };
        #[cfg(feature = "visual-validation")]
        let visual_scenario = visual_authoring_tool_scenario();
        #[cfg(feature = "visual-validation")]
        let adr_visual_scenario = requested_adr_visual_scenario();
        #[cfg(feature = "visual-validation")]
        let (ai_studio, visual_ai_studio_detached_capture) = {
            let mut ai_studio = ai_studio;
            let detached_capture = if let Some(scenario) = adr_visual_scenario.as_deref() {
                if adr_scenario_targets_ai_studio(scenario) {
                    ai_studio.prepare_adr_visual_validation(scenario);
                    true
                } else {
                    // Authoring-surface ADR scenarios prepare themselves when the
                    // documents area is built; AI Studio must stay out of their
                    // captures even when the sweep also uses a validation selector.
                    false
                }
            } else {
                match visual_scenario.as_deref() {
                    Some("ADR 0133 Remote AI Studio") => false,
                    Some("ADR 0143 Model Resources") => {
                        ai_studio.prepare_local_model_resources_visual_validation();
                        true
                    }
                    Some("ADR 0145 External Agent") => {
                        ai_studio.prepare_external_agent_visual_validation();
                        true
                    }
                    Some("ADR 0156 Benchmark Completed") => {
                        ai_studio.prepare_benchmark_campaign_completed_visual_validation()?;
                        true
                    }
                    Some("ADR 0156 Benchmark Running") => {
                        ai_studio.prepare_benchmark_campaign_running_visual_validation()?;
                        true
                    }
                    Some("ADR 0156 Benchmark Reset") => {
                        ai_studio.prepare_benchmark_campaign_reset_visual_validation()?;
                        true
                    }
                    Some(_) => false,
                    None => {
                        let touches_ai_studio = visual_validation_touches_ai_studio();
                        if touches_ai_studio {
                            if visual_validation_touches_acp_startup() {
                                ai_studio.prepare_acp_startup_visual_validation();
                            } else if visual_validation_touches_managed_local_runtime() {
                                ai_studio.prepare_managed_local_visual_validation()?;
                            } else {
                                ai_studio.prepare_hosted_backend_visual_validation();
                            }
                        }
                        touches_ai_studio
                    }
                }
            };
            if detached_capture {
                ai_studio.detach();
            }
            (ai_studio, detached_capture)
        };
        #[cfg(feature = "visual-validation")]
        let mut authoring_windows = AuthoringWindows::default();
        #[cfg(not(feature = "visual-validation"))]
        let authoring_windows = AuthoringWindows::default();
        #[cfg(feature = "visual-validation")]
        if let Some(requested) = visual_scenario.as_deref() {
            if matches!(
                requested,
                "ADR 0133 Remote AI Studio"
                    | "ADR 0143 Model Resources"
                    | "ADR 0145 External Agent"
                    | "ADR 0156 Benchmark Completed"
                    | "ADR 0156 Benchmark Running"
                    | "ADR 0156 Benchmark Reset"
            ) {
                // AI Studio scenarios are prepared above and use its detached native viewport.
            } else if requested == "ADR First Release" {
                // A scenario sweep, not an authoring tool: the surface to capture
                // comes from GAMEENGINE_VISUAL_SCENARIO, and each scenario prepares
                // itself. Opening an authoring window here would cover it.
            } else if requested == "ADR 0154 Animation Set" {
                app.prepare_animation_set_visual_validation();
            } else if requested == "Navigation" {
                app.prepare_navigation_visual_validation();
            } else if requested == "Spatial Audio" {
                app.prepare_spatial_audio_visual_validation();
            } else if requested == "Spatial Audio Details" {
                app.prepare_spatial_audio_details_visual_validation();
            } else if requested == "Spatial Audio Listener" {
                app.prepare_spatial_audio_listener_visual_validation();
            } else {
                if requested == "VFX Builder" {
                    app.prepare_vfx_visual_validation();
                }
                if requested == "Sequencer" {
                    let subject = app.prepare_sequencer_visual_validation();
                    authoring_windows.prepare_sequencer_visual_validation(subject);
                }
                let tool = AuthoringTool::ALL
                    .into_iter()
                    .find(|tool| tool.label() == requested)
                    .ok_or_else(|| {
                        format!(
                            "visual-validation authoring tool `{requested}` is not available in this Editor build"
                        )
                    })?;
                authoring_windows.open(tool);
            }
        }
        project_lease
            .mark_ready()
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "visual-validation")]
        let visual_behavior_debug_capture = visual_validation_touches_behavior_debug();
        Ok(Self {
            app,
            ai_studio,
            authoring_windows,
            show_authoring_tools: false,
            authoring_status: None,
            project_lease,
            _mcp_server: mcp_server,
            mcp_requests,
            #[cfg(feature = "visual-validation")]
            visual_capture_path: visual_screenshot_path(visual_scenario.as_deref()),
            #[cfg(feature = "visual-validation")]
            visual_capture_requested_at: None,
            #[cfg(feature = "visual-validation")]
            visual_behavior_debug_capture,
            #[cfg(feature = "visual-validation")]
            visual_ai_studio_detached_capture,
        })
    }

    fn handle_mcp_requests(&mut self) {
        // Execute at most one authoring request per frame. Provider calls may
        // queue concurrently, but they must not turn one frame into an
        // unbounded queue drain that stalls the Editor UI.
        if let Ok(request) = self.mcp_requests.try_recv() {
            if !request.try_begin() {
                request.respond(EditorMcpHostResult::ToolError {
                    code: "mcp.request_abandoned".to_owned(),
                    message: "The MCP caller disconnected before the Editor began this request."
                        .to_owned(),
                });
                return;
            }
            let run_id = request.agent_run_id().map(str::to_owned);
            let mutating = engine_mcp::tool_is_mutating(request.name());
            if let Some(run_id) = run_id.as_deref()
                && let Err(error) =
                    self.ai_studio
                        .begin_external_mcp_call(run_id, request.name(), mutating)
            {
                request.respond(EditorMcpHostResult::ToolError {
                    code: "agent.invalid_run_context".to_owned(),
                    message: error,
                });
                return;
            }
            let result = self
                .app
                .handle_mcp_tool_call(request.name(), request.arguments().clone());
            if let Some(run_id) = run_id.as_deref() {
                self.ai_studio.finish_external_mcp_call(
                    run_id,
                    request.name(),
                    mutating,
                    result.is_ok(),
                );
            }
            request.respond(match result {
                Ok(value) => EditorMcpHostResult::Success(value),
                Err(error) => EditorMcpHostResult::ToolError {
                    code: error.code().to_owned(),
                    message: error.message().to_owned(),
                },
            });
        }
    }

    #[cfg(feature = "visual-validation")]
    fn handle_visual_validation_capture(&mut self, context: &eframe::egui::Context) {
        let Some(path) = self.visual_capture_path.clone() else {
            return;
        };

        if self.visual_ai_studio_detached_capture {
            let requested_at = *self
                .visual_capture_requested_at
                .get_or_insert_with(Instant::now);
            if self.ai_studio.detached_visual_validation_capture_ready() {
                match capture_ai_studio_native_window(&path) {
                    Ok(()) => {
                        self.visual_capture_path = None;
                        context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        eprintln!("[editor.ai_studio_visual_validation_capture_failed] {error}");
                        self.visual_capture_path = None;
                        context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                    }
                }
                return;
            }
            if requested_at.elapsed() >= Duration::from_secs(5) {
                let _ = std::fs::remove_file(&path);
                eprintln!(
                    "[editor.ai_studio_visual_validation_capture_failed] detached native window did not reach capture-ready state"
                );
                self.visual_capture_path = None;
                context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            } else {
                context.request_repaint();
            }
            return;
        }

        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                eframe::egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = screenshot {
            if let Err(error) = write_visual_validation_png(&path, image.as_ref()) {
                let _ = std::fs::remove_file(&path);
                eprintln!("[editor.visual_validation_capture_failed] {error}");
            }
            self.visual_capture_path = None;
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            return;
        }

        match self.visual_capture_requested_at {
            None => {
                self.visual_capture_requested_at = Some(Instant::now());
                context.send_viewport_cmd(eframe::egui::ViewportCommand::Screenshot(
                    eframe::egui::UserData::default(),
                ));
                context.request_repaint();
            }
            Some(requested_at) if requested_at.elapsed() >= Duration::from_secs(5) => {
                let _ = std::fs::remove_file(&path);
                eprintln!(
                    "[editor.visual_validation_capture_failed] screenshot event was not returned"
                );
                self.visual_capture_path = None;
                context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            }
            Some(_) => context.request_repaint(),
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
            // Stay above the docked editor surface while allowing modeless
            // windows to cover the launcher when their bounds overlap it.
            .order(eframe::egui::Order::Middle)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("AI Studio").clicked() {
                        self.ai_studio.open();
                    }
                    if ui.button("Authoring Tools").clicked() {
                        self.show_authoring_tools = true;
                    }
                });
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
        self.ai_studio.observe_benchmark_hardware(frame);
        if self.project_lease.take_activation_request() {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Focus);
        }
        if self.project_lease.take_close_request() {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            return;
        }
        if let Some(relative) = self.app.take_sequencer_open_request() {
            let project = self.app.project_root().clone();
            self.authoring_windows.open(AuthoringTool::Sequencer);
            self.authoring_windows.open_timeline(&project, &relative);
        }
        if self.app.take_ai_studio_restore_completed() {
            self.ai_studio.report_runtime_result(
                context,
                engine_editor::ai_studio::AiStudioRuntimeResult::EditorRestored,
            );
        }
        if let Some(action) = self.ai_studio.take_runtime_action() {
            let result = self
                .app
                .handle_ai_studio_runtime_action(action, frame.wgpu_render_state());
            self.ai_studio.report_runtime_result(context, result);
        }
        if self.ai_studio.take_live_observation_capture_request() {
            let readback_started = std::time::Instant::now();
            let result = self
                .app
                .capture_ai_studio_live_observation(frame.wgpu_render_state());
            self.ai_studio
                .report_live_observation_capture(result, readback_started.elapsed());
        }
        let sequencer_delta = context.input(|input| input.stable_dt).clamp(0.0, 0.1);
        if self
            .authoring_windows
            .update_sequencer_preview(&mut self.app, sequencer_delta)
        {
            context.request_repaint();
        }
        eframe::App::logic(&mut self.app, context, frame);
        if self.ai_studio.waiting_for_playtest_start() && self.app.ai_studio_playtest_running() {
            self.ai_studio.report_runtime_result(
                context,
                engine_editor::ai_studio::AiStudioRuntimeResult::PlayStarted,
            );
        }
        if self.ai_studio.take_benchmark_child_exit_request() {
            context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        } else {
            #[cfg(feature = "visual-validation")]
            self.handle_visual_validation_capture(context);
        }
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        eframe::App::ui(&mut self.app, ui, frame);
        self.show_authoring_tools_launcher(&context);
        self.authoring_windows.show(
            &context,
            frame,
            self.app.project_root(),
            self.app.asset_manifest(),
        );
        #[cfg(not(feature = "visual-validation"))]
        self.ai_studio.show(&context);
        #[cfg(feature = "visual-validation")]
        {
            let visual_scenario = visual_authoring_tool_scenario();
            let ai_studio_scenario = matches!(
                visual_scenario.as_deref(),
                Some("ADR 0133 Remote AI Studio")
                    | Some("ADR 0143 Model Resources")
                    | Some("ADR 0145 External Agent")
            ) || requested_adr_visual_scenario()
                .is_some_and(|scenario| adr_scenario_targets_ai_studio(&scenario));
            // An ADR scenario that does not detach AI Studio is capturing an
            // authoring surface, so drawing AI Studio over it would replace the
            // evidence the scenario exists to produce.
            let adr_scenario_owns_capture = requested_adr_visual_scenario().is_some()
                && !self.visual_ai_studio_detached_capture;
            if !self.visual_behavior_debug_capture
                && !adr_scenario_owns_capture
                && (visual_scenario.is_none() || ai_studio_scenario)
            {
                self.ai_studio.show(&context);
            }
        }
    }

    fn clear_color(&self, _visuals: &eframe::egui::Visuals) -> [f32; 4] {
        eframe::egui::Color32::from_rgb(20, 22, 26).to_normalized_gamma_f32()
    }
}

/// Returns whether an ADR visual scenario is captured from AI Studio.
///
/// The remaining ADR scenarios are captured from authoring surfaces, which
/// prepare themselves when the documents area is built.
#[cfg(feature = "visual-validation")]
fn adr_scenario_targets_ai_studio(scenario: &str) -> bool {
    scenario.starts_with("adr0144-")
        || scenario.starts_with("adr0149-")
        || scenario.starts_with("adr0153-")
        || scenario.starts_with("adr0158-")
}

/// Returns the requested ADR visual scenario, if one was named.
///
/// ADR scenarios use their own environment variable so an authoring-tool
/// scenario and an ADR scenario can never be confused for one another.
#[cfg(feature = "visual-validation")]
fn requested_adr_visual_scenario() -> Option<String> {
    std::env::var("GAMEENGINE_VISUAL_SCENARIO")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(feature = "visual-validation")]
fn visual_authoring_tool_scenario() -> Option<String> {
    std::env::var("GAMEENGINE_VISUAL_AUTHORING_TOOL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(feature = "visual-validation")]
fn visual_screenshot_path(visual_scenario: Option<&str>) -> Option<PathBuf> {
    if visual_scenario == Some("ADR 0133 Remote AI Studio") {
        return None;
    }
    let value = std::env::var_os("GAMEENGINE_SCREENSHOT_TO")?;
    if value.to_string_lossy().trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

#[cfg(feature = "visual-validation")]
fn visual_validation_touches_behavior_debug() -> bool {
    let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_else(|_| "main".into());
    let base = format!("origin/{base_ref}...HEAD");
    std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &base,
            "--",
            "crates/editor/src/ui/behavior_debug.rs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(feature = "visual-validation")]
fn visual_validation_touches_ai_studio() -> bool {
    if let Ok(scenario) = std::env::var("GAMEENGINE_VISUAL_SCENARIO") {
        return matches!(
            scenario.as_str(),
            "adr0144-hosted-backend"
                | "adr0144-enterprise-backend"
                | "adr0149-live-observation"
                | "adr0153-confinement"
                | "adr0164-ai-selection"
                | "adr0164-agents-section"
                | "adr0164-remote-phone-url"
        );
    }
    let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_else(|_| "main".into());
    let base = format!("origin/{base_ref}...HEAD");
    std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &base,
            "--",
            "crates/editor/src/ai_studio.rs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(feature = "visual-validation")]
fn visual_validation_touches_acp_startup() -> bool {
    let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_else(|_| "main".into());
    let base = format!("origin/{base_ref}...HEAD");
    std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &base,
            "--",
            "crates/editor/src/ai_studio/acp_startup.rs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(feature = "visual-validation")]
fn visual_validation_touches_managed_local_runtime() -> bool {
    let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_else(|_| "main".into());
    let base = format!("origin/{base_ref}...HEAD");
    std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &base,
            "--",
            "crates/editor/src/managed_local_runtime.rs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(all(feature = "visual-validation", target_os = "windows"))]
fn capture_ai_studio_native_window(path: &Path) -> Result<(), String> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class GameEngineNativeWindowCapture
{
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int maxCount);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
}
'@

$targetPid = [uint32]$env:GAMEENGINE_VISUAL_CAPTURE_PID
$targetTitle = $env:GAMEENGINE_VISUAL_CAPTURE_TITLE
$outputPath = $env:GAMEENGINE_VISUAL_CAPTURE_PATH
$windowHandle = [IntPtr]::Zero

for ($attempt = 0; $attempt -lt 40 -and $windowHandle -eq [IntPtr]::Zero; $attempt++) {
    $script:gameEngineWindowHandle = [IntPtr]::Zero
    $callback = [GameEngineNativeWindowCapture+EnumWindowsProc] {
        param([IntPtr]$hWnd, [IntPtr]$lParam)

        if (-not [GameEngineNativeWindowCapture]::IsWindowVisible($hWnd)) {
            return $true
        }

        [uint32]$candidatePid = 0
        [void][GameEngineNativeWindowCapture]::GetWindowThreadProcessId($hWnd, [ref]$candidatePid)
        if ($candidatePid -ne $targetPid) {
            return $true
        }

        $title = [System.Text.StringBuilder]::new(512)
        [void][GameEngineNativeWindowCapture]::GetWindowText($hWnd, $title, $title.Capacity)
        if ($title.ToString() -eq $targetTitle) {
            $script:gameEngineWindowHandle = $hWnd
            return $false
        }

        return $true
    }

    [void][GameEngineNativeWindowCapture]::EnumWindows($callback, [IntPtr]::Zero)
    $windowHandle = $script:gameEngineWindowHandle
    if ($windowHandle -eq [IntPtr]::Zero) {
        Start-Sleep -Milliseconds 50
    }
}

if ($windowHandle -eq [IntPtr]::Zero) {
    throw "AI Studio native window was not found for process $targetPid."
}

$rect = New-Object -TypeName 'GameEngineNativeWindowCapture+RECT'
if (-not [GameEngineNativeWindowCapture]::GetWindowRect($windowHandle, [ref]$rect)) {
    throw 'GetWindowRect failed for the AI Studio native window.'
}

$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
if ($width -le 0 -or $height -le 0) {
    throw "AI Studio native window has invalid bounds ${width}x${height}."
}

$bitmap = [System.Drawing.Bitmap]::new($width, $height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
try {
    $size = [System.Drawing.Size]::new($width, $height)
    $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $size)
    $bitmap.Save($outputPath, [System.Drawing.Imaging.ImageFormat]::Png)
}
finally {
    $graphics.Dispose()
    $bitmap.Dispose()
}
"#;

    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env(
            "GAMEENGINE_VISUAL_CAPTURE_PID",
            std::process::id().to_string(),
        )
        .env("GAMEENGINE_VISUAL_CAPTURE_TITLE", "AI Studio")
        .env("GAMEENGINE_VISUAL_CAPTURE_PATH", path.as_os_str())
        .output()
        .map_err(|error| format!("failed to start native-window capture: {error}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if stderr.is_empty() {
            format!("native-window capture exited with status {}", output.status)
        } else {
            format!("native-window capture failed: {stderr}")
        })
    }
}

#[cfg(all(feature = "visual-validation", not(target_os = "windows")))]
fn capture_ai_studio_native_window(_path: &Path) -> Result<(), String> {
    Err("detached AI Studio visual capture requires Windows".to_owned())
}

#[cfg(feature = "visual-validation")]
fn write_visual_validation_png(
    path: &Path,
    image: &eframe::egui::ColorImage,
) -> Result<(), String> {
    let [width, height] = image.size;
    let file = File::create(path).map_err(|error| error.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let rgba = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect::<Vec<_>>();
    writer
        .write_image_data(&rgba)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())
}

/// How the Editor binary was invoked.
///
/// The same executable is the human Editor, one isolated benchmark child, and
/// the headless parent of a benchmark suite. Separating the three at argument
/// parsing keeps the benchmark parent from acquiring a project lease or opening
/// a window it does not need.
enum EditorInvocation {
    /// Normal windowed Editor, optionally acting as one benchmark child.
    Editor {
        project: PathBuf,
        benchmark_run: Option<PathBuf>,
    },
    /// Headless parent that executes one whole benchmark experiment.
    BenchmarkExperiment(Box<BenchmarkExperimentOptions>),
}

/// Reads the value that follows a flag, or reports the flag that lacks one.
fn next_argument_value<I: Iterator<Item = std::ffi::OsString>>(
    arguments: &mut I,
    flag: &str,
) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn editor_invocation() -> Result<EditorInvocation, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut project = None;
    let mut benchmark_run = None;
    let mut benchmark_experiment = None;
    let mut benchmark_endpoint = None;
    let mut benchmark_fixture = None;
    let mut benchmark_run_timeout = None;
    let mut benchmark_resume_from = None;
    let mut benchmark_pause_file = None;
    while let Some(argument) = arguments.next() {
        if argument == "--project" {
            if project.is_some() {
                return Err("--project may be specified only once".to_owned());
            }
            project = Some(next_argument_value(&mut arguments, "--project")?);
        } else if argument == "--benchmark-run" {
            if benchmark_run.is_some() {
                return Err("--benchmark-run may be specified only once".to_owned());
            }
            benchmark_run = Some(next_argument_value(&mut arguments, "--benchmark-run")?);
        } else if argument == "--benchmark-experiment" {
            if benchmark_experiment.is_some() {
                return Err("--benchmark-experiment may be specified only once".to_owned());
            }
            benchmark_experiment = Some(next_argument_value(
                &mut arguments,
                "--benchmark-experiment",
            )?);
        } else if argument == "--benchmark-endpoint" {
            if benchmark_endpoint.is_some() {
                return Err("--benchmark-endpoint may be specified only once".to_owned());
            }
            benchmark_endpoint = Some(
                next_argument_value(&mut arguments, "--benchmark-endpoint")?
                    .to_string_lossy()
                    .into_owned(),
            );
        } else if argument == "--benchmark-fixture" {
            if benchmark_fixture.is_some() {
                return Err("--benchmark-fixture may be specified only once".to_owned());
            }
            benchmark_fixture = Some(next_argument_value(&mut arguments, "--benchmark-fixture")?);
        } else if argument == "--benchmark-run-timeout" {
            if benchmark_run_timeout.is_some() {
                return Err("--benchmark-run-timeout may be specified only once".to_owned());
            }
            let seconds = next_argument_value(&mut arguments, "--benchmark-run-timeout")?
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|error| format!("--benchmark-run-timeout requires seconds: {error}"))?;
            benchmark_run_timeout = Some(Duration::from_secs(seconds));
        } else if argument == "--benchmark-resume-from" {
            if benchmark_resume_from.is_some() {
                return Err("--benchmark-resume-from may be specified only once".to_owned());
            }
            let ordinal = next_argument_value(&mut arguments, "--benchmark-resume-from")?
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|error| format!("--benchmark-resume-from requires an ordinal: {error}"))?;
            benchmark_resume_from = Some(ordinal);
        } else if argument == "--benchmark-pause-file" {
            if benchmark_pause_file.is_some() {
                return Err("--benchmark-pause-file may be specified only once".to_owned());
            }
            benchmark_pause_file = Some(next_argument_value(
                &mut arguments,
                "--benchmark-pause-file",
            )?);
        } else {
            return Err(format!(
                "unknown Editor argument `{}`",
                argument.to_string_lossy()
            ));
        }
    }

    if let Some(spec_path) = benchmark_experiment {
        if project.is_some() || benchmark_run.is_some() {
            return Err(
                "--benchmark-experiment runs the suite parent and cannot be combined with --project or --benchmark-run"
                    .to_owned(),
            );
        }
        let mut options = BenchmarkExperimentOptions::new(spec_path);
        if let Some(endpoint) = benchmark_endpoint {
            options.endpoint = endpoint;
        }
        options.fixture_template_root = benchmark_fixture;
        if let Some(timeout) = benchmark_run_timeout {
            options.run_timeout = Some(timeout);
        }
        options.resume_from_ordinal = benchmark_resume_from;
        options.pause_file = benchmark_pause_file;
        return Ok(EditorInvocation::BenchmarkExperiment(Box::new(options)));
    }
    if benchmark_endpoint.is_some()
        || benchmark_fixture.is_some()
        || benchmark_run_timeout.is_some()
        || benchmark_resume_from.is_some()
        || benchmark_pause_file.is_some()
    {
        return Err(
            "benchmark endpoint, fixture, timeout, resume, and pause options apply only to --benchmark-experiment"
                .to_owned(),
        );
    }
    Ok(EditorInvocation::Editor {
        project: project.ok_or_else(|| "Engine Editor requires `--project <path>`".to_owned())?,
        benchmark_run,
    })
}

fn run() -> Result<(), String> {
    match editor_invocation()? {
        EditorInvocation::BenchmarkExperiment(options) => run_benchmark_suite(*options),
        EditorInvocation::Editor {
            project,
            benchmark_run,
        } => run_editor(project, benchmark_run),
    }
}

/// Executes one benchmark experiment and prints its comparison report.
fn run_benchmark_suite(options: BenchmarkExperimentOptions) -> Result<(), String> {
    let outcome = run_benchmark_experiment(options)?;
    print!("{}", outcome.report);
    println!(
        "
{} of {} planned runs recorded, {} passed; comparison written to {}",
        outcome.completed_runs,
        outcome.planned_runs,
        outcome.passed_runs,
        outcome.comparison_path.display()
    );
    if let Some(reason) = outcome.stopped_early
        && !outcome.paused
    {
        return Err(format!("benchmark experiment stopped early: {reason}"));
    }
    Ok(())
}

fn run_editor(project: PathBuf, benchmark_run: Option<PathBuf>) -> Result<(), String> {
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
                EditorShell::new(
                    project_lease,
                    &creation_context.egui_ctx,
                    benchmark_run.as_deref(),
                )
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
