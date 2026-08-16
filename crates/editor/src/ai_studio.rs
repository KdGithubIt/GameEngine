//! Conversation-first AI Studio frontend.
//!
//! This module owns only presentation and direct user interaction. Agent
//! lifecycle, permissions, persistence, provider process management, and code
//! workspace rules live in the GUI-free `agent_host` module.

use crate::agent_host::{
    project_storage_key, AgentCapability, AgentEventKind, AgentHost, AgentProposal, AgentRunState,
    ApprovalScope, CodeChange, CodeWorkspace, CompletionStatus, ConversationRole,
    ExternalAgentProcess, PermissionCheck, ProcessStream,
};
use eframe::egui;
use engine_authoring::id::StableId;
use engine_authoring::ProjectRoot;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Ephemeral Editor-owned MCP connection injected into compatible agent runtimes.
///
/// The authorization token is intentionally private and is never serialized by
/// AI Studio. It is exposed to a launched child only through that child's
/// process environment.
pub struct AiStudioConnection {
    endpoint: String,
    authorization_token: String,
}

impl AiStudioConnection {
    /// Creates an in-memory connection descriptor for the active Editor MCP host.
    pub fn new(endpoint: impl Into<String>, authorization_token: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            authorization_token: authorization_token.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPermissionAction {
    LaunchExternalAgent,
    ApplyCodeChanges,
}

struct PendingPermission {
    run_id: String,
    capability: AgentCapability,
    action: PendingPermissionAction,
}

/// Project-scoped conversation-first AI Studio window.
///
/// The panel persists sessions outside canonical project data by default,
/// snapshots proposal versions before starting a run, and routes code writes
/// through a reviewable isolated workspace. Project-shared history is explicit.
pub struct AiStudioPanel {
    project_root: PathBuf,
    connection: AiStudioConnection,
    host: AgentHost,
    selected_session: String,
    proposal_draft: AgentProposal,
    message_draft: String,
    provider_program: String,
    provider_args: String,
    open: bool,
    active_run_id: Option<String>,
    process: Option<ExternalAgentProcess>,
    code_workspace: Option<CodeWorkspace>,
    pending_code_changes: Vec<CodeChange>,
    pending_permission: Option<PendingPermission>,
    status: Option<String>,
}

impl AiStudioPanel {
    /// Opens the project-scoped AI Studio state for an Editor project.
    pub fn new(project: &ProjectRoot, connection: AiStudioConnection) -> Result<Self, String> {
        let data_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("GameEngine")
            .join("ai")
            .join(project_storage_key(project.project_id().as_str(), project.path()));
        let mut host = AgentHost::open(project.path().to_path_buf(), data_root)
            .map_err(|error| error.to_string())?;
        let selected_session = match host.session_ids().into_iter().next_back() {
            Some(id) => id,
            None => host
                .create_session("New AI Studio session")
                .map_err(|error| error.to_string())?,
        };
        let proposal_draft = host
            .session(&selected_session)
            .map_err(|error| error.to_string())?
            .proposal
            .clone();
        Ok(Self {
            project_root: project.path().to_path_buf(),
            connection,
            host,
            selected_session,
            proposal_draft,
            message_draft: String::new(),
            provider_program: String::new(),
            provider_args: String::new(),
            open: true,
            active_run_id: None,
            process: None,
            code_workspace: None,
            pending_code_changes: Vec::new(),
            pending_permission: None,
            status: None,
        })
    }

    /// Makes the AI Studio window visible.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Draws the AI Studio window and advances any active external agent process.
    pub fn show(&mut self, context: &egui::Context) {
        self.poll_external_process(context);
        let mut open = self.open;
        egui::Window::new("AI Studio")
            .id(egui::Id::new("gameengine_ai_studio"))
            .open(&mut open)
            .default_pos(egui::pos2(940.0, 84.0))
            .default_size(egui::vec2(600.0, 760.0))
            .min_width(460.0)
            .min_height(520.0)
            .resizable(true)
            .show(context, |ui| self.show_contents(ui));
        self.open = open;
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Conversation-first project agent");
            ui.separator();
            ui.label("Structured authoring stays on the Editor MCP host.");
        });
        ui.small(
            "External agent processes are application-level integrations, not an OS sandbox. Code is prepared in an isolated managed workspace and must be reviewed before apply.",
        );
        ui.separator();

        self.show_session_header(ui);
        self.show_conversation(ui);
        ui.separator();
        self.show_proposal(ui);
        ui.separator();
        self.show_provider(ui);
        self.show_permission_prompt(ui);
        self.show_code_changes(ui);
        self.show_run_timeline(ui);

        if let Some(status) = &self.status {
            ui.separator();
            ui.label(status);
        }
    }

    fn show_session_header(&mut self, ui: &mut egui::Ui) {
        let session_ids = self.host.session_ids();
        let current_title = self
            .host
            .session(&self.selected_session)
            .map(|session| session.title.clone())
            .unwrap_or_else(|_| "Unavailable session".to_owned());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ai_studio_session")
                .selected_text(current_title)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for id in session_ids {
                        let title = self
                            .host
                            .session(&id)
                            .map(|session| session.title.as_str())
                            .unwrap_or("Unavailable session");
                        if ui
                            .selectable_value(&mut self.selected_session, id.clone(), title)
                            .changed()
                        {
                            if let Ok(session) = self.host.session(&id) {
                                self.proposal_draft = session.proposal.clone();
                            }
                        }
                    }
                });
            if ui.button("New session").clicked() {
                match self.host.create_session("New AI Studio session") {
                    Ok(id) => {
                        self.selected_session = id;
                        self.proposal_draft = AgentProposal::default();
                        self.status = Some("Created a private local AI session.".to_owned());
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            if ui.button("Share with project").clicked() {
                match self.host.export_shared_session(&self.selected_session) {
                    Ok(path) => {
                        let relative = path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(path.as_path());
                        self.status = Some(format!(
                            "Wrote sanitized project-shared history to {}.",
                            relative.display()
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
        });
    }

    fn show_conversation(&mut self, ui: &mut egui::Ui) {
        ui.heading("Conversation");
        let messages = self
            .host
            .session(&self.selected_session)
            .map(|session| session.messages.clone())
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .id_salt("ai_studio_conversation")
            .max_height(180.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if messages.is_empty() {
                    ui.weak("Describe what you want to build, change, inspect, or validate.");
                }
                for message in messages {
                    ui.group(|ui| {
                        ui.strong(match message.role {
                            ConversationRole::User => "You",
                            ConversationRole::Assistant => "Agent",
                            ConversationRole::System => "System",
                        });
                        ui.label(message.text);
                    });
                }
            });
        ui.add(
            egui::TextEdit::multiline(&mut self.message_draft)
                .desired_rows(2)
                .hint_text("Add a goal, clarification, constraint, or feedback…"),
        );
        ui.horizontal(|ui| {
            let can_send = !self.message_draft.trim().is_empty();
            if ui.add_enabled(can_send, egui::Button::new("Send")).clicked() {
                let text = self.message_draft.trim().to_owned();
                match self.host.append_message(
                    &self.selected_session,
                    ConversationRole::User,
                    text,
                ) {
                    Ok(()) => self.message_draft.clear(),
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            ui.small("Clarifications remain in the same session; no forced planning mode switch.");
        });
    }

    fn show_proposal(&mut self, ui: &mut egui::Ui) {
        let current_version = self
            .host
            .session(&self.selected_session)
            .map(|session| session.proposal.version)
            .unwrap_or(self.proposal_draft.version);
        egui::CollapsingHeader::new(format!("Structured proposal · v{current_version}"))
            .default_open(true)
            .show(ui, |ui| {
                ui.label("Goal");
                ui.text_edit_singleline(&mut self.proposal_draft.goal);
                edit_lines(ui, "Requirements", &mut self.proposal_draft.requirements);
                edit_lines(ui, "Assumptions", &mut self.proposal_draft.assumptions);
                edit_lines(
                    ui,
                    "Acceptance criteria",
                    &mut self.proposal_draft.acceptance_criteria,
                );
                edit_lines(
                    ui,
                    "Planned project changes",
                    &mut self.proposal_draft.planned_project_changes,
                );
                edit_lines(
                    ui,
                    "Planned code changes",
                    &mut self.proposal_draft.planned_code_changes,
                );
                edit_lines(ui, "Planned assets", &mut self.proposal_draft.planned_assets);
                edit_lines(ui, "Validation plan", &mut self.proposal_draft.validation_plan);
                edit_lines(ui, "Playtest plan", &mut self.proposal_draft.playtest_plan);
                if ui.button("Save proposal version").clicked() {
                    match self
                        .host
                        .update_proposal(&self.selected_session, self.proposal_draft.clone())
                    {
                        Ok(version) => {
                            self.proposal_draft.version = version;
                            self.status = Some(format!("Saved proposal version {version}."));
                        }
                        Err(error) => self.status = Some(error.to_string()),
                    }
                }
            });
    }

    fn show_provider(&mut self, ui: &mut egui::Ui) {
        ui.heading("Run");
        ui.horizontal(|ui| {
            ui.label("Compatible agent program");
            ui.text_edit_singleline(&mut self.provider_program);
        });
        ui.horizontal(|ui| {
            ui.label("Arguments");
            ui.text_edit_singleline(&mut self.provider_args);
        });
        ui.small(
            "The program is launched directly without a shell. GameEngine injects the immutable proposal and ephemeral MCP endpoint/token as process environment variables. Provider-specific Claude/Codex/native adapters can sit on this same runtime boundary without changing AI Studio semantics.",
        );
        let mut stop_requested = false;
        ui.horizontal(|ui| {
            let can_go = self.process.is_none()
                && self.pending_permission.is_none()
                && !self.provider_program.trim().is_empty();
            if ui.add_enabled(can_go, egui::Button::new("Go")).clicked() {
                self.begin_run();
            }
            if self.process.is_some() && ui.button("Stop").clicked() {
                stop_requested = true;
            }
        });
        if stop_requested {
            let run_id = self.active_run_id.clone();
            if let Some(process) = self.process.as_mut() {
                if let Err(error) = process.cancel() {
                    self.status = Some(format!("Could not stop agent process: {error}"));
                }
            }
            self.process = None;
            if let Some(run_id) = run_id {
                if let Err(error) = self.host.cancel_run(&run_id) {
                    self.status = Some(error.to_string());
                }
            }
        }
    }

    fn show_permission_prompt(&mut self, ui: &mut egui::Ui) {
        let Some(pending) = self.pending_permission.as_ref() else {
            return;
        };
        let run_id = pending.run_id.clone();
        let capability = pending.capability;
        let action = pending.action;
        ui.separator();
        ui.group(|ui| {
            ui.strong("Approval required");
            ui.label(capability.label());
            ui.small("Project-level approval persists as user application state; credentials never do.");
            ui.horizontal(|ui| {
                for (label, scope) in [
                    ("Allow once", ApprovalScope::Once),
                    ("This run", ApprovalScope::Run),
                    ("This project", ApprovalScope::Project),
                    ("Deny", ApprovalScope::Deny),
                ] {
                    if ui.button(label).clicked() {
                        self.resolve_pending_permission(&run_id, capability, action, scope);
                    }
                }
            });
        });
    }

    fn show_code_changes(&mut self, ui: &mut egui::Ui) {
        if self.pending_code_changes.is_empty() {
            return;
        }
        ui.separator();
        egui::CollapsingHeader::new(format!(
            "Managed code diff · {} file(s)",
            self.pending_code_changes.len()
        ))
        .default_open(true)
        .show(ui, |ui| {
            for change in &self.pending_code_changes {
                ui.horizontal(|ui| {
                    ui.monospace(change.relative_path.display().to_string());
                    ui.weak(change_summary(change));
                });
            }
            let can_apply = self.pending_permission.is_none()
                && self.code_workspace.is_some()
                && self.active_run_id.is_some();
            if ui
                .add_enabled(can_apply, egui::Button::new("Review complete — apply code changes"))
                .clicked()
            {
                self.request_code_apply();
            }
            ui.small(
                "Only game/** and assets/scripts/{rust,rhai}/** are eligible. Deletions and stale live files are rejected rather than forced.",
            );
        });
    }

    fn show_run_timeline(&mut self, ui: &mut egui::Ui) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let Ok(run) = self.host.run(&run_id).cloned() else {
            return;
        };
        ui.separator();
        egui::CollapsingHeader::new(format!("Run timeline · {:?}", run.state))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("Proposal v{}", run.proposal_snapshot.version));
                    ui.label(format!("Provider: {}", run.provider_label));
                });
                egui::ScrollArea::vertical()
                    .id_salt("ai_studio_timeline")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for event in run.events.iter().rev().take(120).rev() {
                            ui.horizontal_wrapped(|ui| {
                                ui.monospace(format!("#{:03}", event.sequence));
                                ui.strong(format!("{:?}", event.kind));
                                ui.label(&event.message);
                            });
                        }
                    });
                self.show_completion_contract(ui, &run_id, run.state, run.completion);
            });
    }

    fn show_completion_contract(
        &mut self,
        ui: &mut egui::Ui,
        run_id: &str,
        state: AgentRunState,
        mut report: crate::agent_host::CompletionReport,
    ) {
        ui.separator();
        ui.strong("Completion contract");
        let mut changed = false;
        changed |= completion_row(ui, "Acceptance criteria", &mut report.acceptance_criteria);
        changed |= completion_row(ui, "Authoring validation", &mut report.authoring_validation);
        changed |= completion_row(ui, "Source validation", &mut report.source_validation);
        changed |= completion_row(ui, "Play launch", &mut report.play_launch);
        changed |= completion_row(ui, "Frame capture", &mut report.frame_capture);
        changed |= completion_row(ui, "Visual evaluation", &mut report.visual_evaluation);
        changed |= completion_row(ui, "Interaction scenarios", &mut report.interaction_scenarios);
        if changed {
            let copy = report.clone();
            if let Err(error) = self.host.set_completion_status(run_id, move |target| *target = copy) {
                self.status = Some(error.to_string());
            }
        }
        if ui.button("Complete run").clicked() {
            let result = if state == AgentRunState::Validating {
                self.host.transition_run(
                    run_id,
                    AgentRunState::Evaluating,
                    "Human review advanced the run to final evaluation.",
                )
            } else {
                Ok(())
            }
            .and_then(|()| self.host.complete_run(run_id));
            match result {
                Ok(()) => self.status = Some("Run completion contract satisfied.".to_owned()),
                Err(error) => self.status = Some(error.to_string()),
            }
        }
    }

    fn begin_run(&mut self) {
        if let Err(error) = self
            .host
            .update_proposal(&self.selected_session, self.proposal_draft.clone())
        {
            self.status = Some(error.to_string());
            return;
        }
        let provider = self.provider_program.trim().to_owned();
        match self.host.start_run(&self.selected_session, provider) {
            Ok(run_id) => {
                self.active_run_id = Some(run_id.clone());
                self.request_permission(
                    run_id,
                    AgentCapability::ExternalAgentProcess,
                    PendingPermissionAction::LaunchExternalAgent,
                );
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn request_code_apply(&mut self) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        self.request_permission(
            run_id,
            AgentCapability::CodeWorkspaceApply,
            PendingPermissionAction::ApplyCodeChanges,
        );
    }

    fn request_permission(
        &mut self,
        run_id: String,
        capability: AgentCapability,
        action: PendingPermissionAction,
    ) {
        match self.host.check_permission(&run_id, capability) {
            Ok(PermissionCheck::Granted) => self.execute_permission_action(&run_id, action),
            Ok(PermissionCheck::RequiresApproval) => {
                self.pending_permission = Some(PendingPermission {
                    run_id,
                    capability,
                    action,
                });
            }
            Ok(PermissionCheck::Denied) => {
                self.status = Some(format!("Permission denied: {}.", capability.label()));
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn resolve_pending_permission(
        &mut self,
        run_id: &str,
        capability: AgentCapability,
        action: PendingPermissionAction,
        scope: ApprovalScope,
    ) {
        self.pending_permission = None;
        if let Err(error) = self
            .host
            .resolve_permission(run_id, capability, scope)
        {
            self.status = Some(error.to_string());
            return;
        }
        if scope == ApprovalScope::Deny {
            self.status = Some(format!("Denied {}.", capability.label()));
            return;
        }
        match self.host.check_permission(run_id, capability) {
            Ok(PermissionCheck::Granted) => self.execute_permission_action(run_id, action),
            Ok(_) => self.status = Some("Permission was not granted.".to_owned()),
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn execute_permission_action(&mut self, run_id: &str, action: PendingPermissionAction) {
        match action {
            PendingPermissionAction::LaunchExternalAgent => self.launch_external_agent(run_id),
            PendingPermissionAction::ApplyCodeChanges => self.apply_code_changes(run_id),
        }
    }

    fn launch_external_agent(&mut self, run_id: &str) {
        let workspace_root = self.host.workspace_root(run_id);
        let workspace = match CodeWorkspace::create(&self.project_root, workspace_root) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.fail_run(run_id, format!("Could not prepare code workspace: {error}"));
                return;
            }
        };
        if let Err(error) = self.host.record_event(
            run_id,
            AgentEventKind::CodeWorkspacePrepared,
            "Prepared isolated managed code workspace.",
        ) {
            self.status = Some(error.to_string());
        }
        let proposal_json = match self.host.run(run_id).and_then(|run| {
            serde_json::to_string(&run.proposal_snapshot).map_err(Into::into)
        }) {
            Ok(json) => json,
            Err(error) => {
                self.fail_run(run_id, format!("Could not serialize proposal: {error}"));
                return;
            }
        };
        let environment = vec![
            (
                OsString::from("GAMEENGINE_MCP_ENDPOINT"),
                OsString::from(&self.connection.endpoint),
            ),
            (
                OsString::from("GAMEENGINE_MCP_AUTH_TOKEN"),
                OsString::from(&self.connection.authorization_token),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_RUN_ID"),
                OsString::from(run_id),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_PROPOSAL_JSON"),
                OsString::from(proposal_json),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_AUTHORING_CONTRACT"),
                OsString::from(
                    "Use the injected Editor MCP endpoint for persisted authoring changes. Use this isolated workspace only for project code and return code changes for review.",
                ),
            ),
        ];
        let args = split_direct_args(&self.provider_args);
        match ExternalAgentProcess::spawn(
            OsStr::new(self.provider_program.trim()),
            &args,
            workspace.root(),
            &environment,
        ) {
            Ok(process) => {
                self.code_workspace = Some(workspace);
                self.process = Some(process);
                if let Err(error) = self.host.transition_run(
                    run_id,
                    AgentRunState::Executing,
                    "External agent runtime started in the isolated code workspace.",
                ) {
                    self.status = Some(error.to_string());
                }
                self.status = Some("External agent runtime started.".to_owned());
            }
            Err(error) => {
                self.fail_run(run_id, format!("Could not launch external agent: {error}"));
            }
        }
    }

    fn poll_external_process(&mut self, context: &egui::Context) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let output = self
            .process
            .as_ref()
            .map(ExternalAgentProcess::drain_output)
            .unwrap_or_default();
        for line in output {
            let stream = match line.stream {
                ProcessStream::Stdout => "stdout",
                ProcessStream::Stderr => "stderr",
            };
            if let Err(error) = self.host.record_event(
                &run_id,
                AgentEventKind::ProviderOutput,
                format!("{stream}: {}", line.text),
            ) {
                self.status = Some(error.to_string());
            }
        }
        let exit = match self.process.as_mut() {
            Some(process) => process.poll_exit(),
            None => return,
        };
        match exit {
            Ok(None) => context.request_repaint_after(std::time::Duration::from_millis(100)),
            Ok(Some(status)) => {
                self.process = None;
                if status.success() {
                    self.finish_provider_execution(&run_id, status.code());
                } else {
                    self.fail_run(
                        &run_id,
                        format!("External agent exited unsuccessfully with {:?}.", status.code()),
                    );
                }
            }
            Err(error) => {
                self.process = None;
                self.fail_run(&run_id, format!("Could not poll external agent: {error}"));
            }
        }
    }

    fn finish_provider_execution(&mut self, run_id: &str, exit_code: Option<i32>) {
        let changes = match self.code_workspace.as_ref() {
            Some(workspace) => match workspace.collect_changes() {
                Ok(changes) => changes,
                Err(error) => {
                    self.fail_run(run_id, format!("Could not inspect code workspace: {error}"));
                    return;
                }
            },
            None => Vec::new(),
        };
        if let Err(error) = self.host.record_event(
            run_id,
            AgentEventKind::CodeChangesDetected,
            format!("Detected {} managed code file change(s).", changes.len()),
        ) {
            self.status = Some(error.to_string());
        }
        self.pending_code_changes = changes;
        if let Err(error) = self.host.transition_run(
            run_id,
            AgentRunState::Validating,
            format!(
                "External agent exited with {:?}; validation and completion checks are still required.",
                exit_code
            ),
        ) {
            self.status = Some(error.to_string());
        } else {
            self.status = Some(
                "Provider execution finished. Review code changes and complete validation/playtest evidence before completion."
                    .to_owned(),
            );
        }
    }

    fn apply_code_changes(&mut self, run_id: &str) {
        let Some(workspace) = self.code_workspace.as_mut() else {
            self.status = Some("No managed code workspace is available.".to_owned());
            return;
        };
        match workspace.apply_changes(&self.pending_code_changes) {
            Ok(()) => {
                let count = self.pending_code_changes.len();
                self.pending_code_changes.clear();
                if let Err(error) = self.host.record_event(
                    run_id,
                    AgentEventKind::CodeChangesApplied,
                    format!("Applied {count} reviewed code file change(s) after stale checks."),
                ) {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(format!("Applied {count} reviewed code file change(s)."));
                }
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn fail_run(&mut self, run_id: &str, message: String) {
        let _ = self
            .host
            .record_event(run_id, AgentEventKind::Failure, message.clone());
        let _ = self
            .host
            .transition_run(run_id, AgentRunState::Failed, message.clone());
        self.status = Some(message);
    }
}

fn edit_lines(ui: &mut egui::Ui, label: &str, values: &mut Vec<String>) {
    let mut text = values.join("\n");
    ui.label(label);
    if ui
        .add(
            egui::TextEdit::multiline(&mut text)
                .desired_rows(2)
                .hint_text("One item per line"),
        )
        .changed()
    {
        *values = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }
}

fn split_direct_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(ToOwned::to_owned).collect()
}

fn change_summary(change: &CodeChange) -> &'static str {
    match (&change.before, &change.after) {
        (None, Some(_)) => "new",
        (Some(_), None) => "delete (apply blocked)",
        (Some(_), Some(_)) => "modified",
        (None, None) => "unchanged",
    }
}

fn completion_row(ui: &mut egui::Ui, label: &str, status: &mut CompletionStatus) -> bool {
    let before = *status;
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(("ai_completion", label))
            .selected_text(completion_label(*status))
            .show_ui(ui, |ui| {
                for candidate in [
                    CompletionStatus::Pending,
                    CompletionStatus::Passed,
                    CompletionStatus::Failed,
                    CompletionStatus::NotApplicable,
                ] {
                    ui.selectable_value(status, candidate, completion_label(candidate));
                }
            });
    });
    *status != before
}

fn completion_label(status: CompletionStatus) -> &'static str {
    match status {
        CompletionStatus::Pending => "Pending",
        CompletionStatus::Passed => "Passed",
        CompletionStatus::Failed => "Failed",
        CompletionStatus::NotApplicable => "N/A",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_args_do_not_invoke_shell_parsing() {
        assert_eq!(
            split_direct_args("--flag value ; echo nope"),
            ["--flag", "value", ";", "echo", "nope"]
        );
    }

    #[test]
    fn code_change_summary_keeps_deletion_blocked() {
        let change = CodeChange {
            relative_path: PathBuf::from("game/src/lib.rs"),
            before: Some("old".to_owned()),
            after: None,
        };
        assert_eq!(change_summary(&change), "delete (apply blocked)");
    }
}
