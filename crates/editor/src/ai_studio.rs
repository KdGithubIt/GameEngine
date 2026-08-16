//! egui frontend for the project-scoped conversational AI Studio.
//!
//! Provider/runtime orchestration and persistence live in [`crate::agent`].
//! This module only translates user interaction into those GUI-free contracts.

use crate::agent::{
    AgentCapability, AgentHost, AgentPermission, AgentProposalDraft, AgentRunState,
    AgentSessionId, ApprovalScope, AuthenticationClass, ConversationRole,
    ProviderConnectionState,
    ProviderDescriptor, ProviderRuntimeKind, SessionVisibility,
};
use eframe::egui;
use engine_authoring::AuthoringPermission;
use std::path::PathBuf;

/// Modeless AI Studio window for one Editor-open project.
pub struct AiStudioWindow {
    host: AgentHost,
    session_id: AgentSessionId,
    selected_provider: String,
    message_input: String,
    proposal_goal: String,
    proposal_requirements: String,
    proposal_assumptions: String,
    proposal_acceptance: String,
    proposal_changes: String,
    proposal_validation: String,
    status: Option<String>,
}

impl AiStudioWindow {
    /// Creates an AI Studio frontend for one project root.
    pub fn new(project_root: PathBuf) -> Self {
        let local_store_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("GameEngine")
            .join("ai");
        let mut host = AgentHost::new(project_root, local_store_root);
        let policy_status = host.load_project_policy().err().map(|error| {
            format!(
                "Could not restore AI project permissions ({}): {}",
                error.code(),
                error.message()
            )
        });

        host.register_provider(ProviderDescriptor {
            id: "external-agent".to_owned(),
            display_name: "External coding agent".to_owned(),
            runtime_kind: ProviderRuntimeKind::ExternalAgentRuntime,
            authentication: AuthenticationClass::ProviderManagedSession,
            connection_state: ProviderConnectionState::AuthenticationRequired,
        });
        host.register_provider(ProviderDescriptor {
            id: "native-model".to_owned(),
            display_name: "Native model runtime".to_owned(),
            runtime_kind: ProviderRuntimeKind::NativeAgentRuntime,
            authentication: AuthenticationClass::LocalNoAuth,
            connection_state: ProviderConnectionState::Disconnected,
        });

        let (session_id, session_status) = match host.restore_latest_session() {
            Ok(Some(session_id)) => (session_id, Some("Restored the latest AI session.".to_owned())),
            Ok(None) => {
                let session_id = host.create_session("AI Studio");
                let status = host.save_session(&session_id).err().map(|error| {
                    format!(
                        "Could not initialize AI session persistence ({}): {}",
                        error.code(),
                        error.message()
                    )
                });
                (session_id, status)
            }
            Err(error) => (
                host.create_session("AI Studio"),
                Some(format!(
                    "Could not restore AI session ({}): {}",
                    error.code(),
                    error.message()
                )),
            ),
        };
        Self {
            host,
            session_id,
            selected_provider: "external-agent".to_owned(),
            message_input: String::new(),
            proposal_goal: String::new(),
            proposal_requirements: String::new(),
            proposal_assumptions: String::new(),
            proposal_acceptance: String::new(),
            proposal_changes: String::new(),
            proposal_validation: String::new(),
            status: policy_status.or(session_status),
        }
    }

    /// Draws the conversational AI Studio surface.
    pub fn show(&mut self, context: &egui::Context, open: &mut bool) {
        egui::Window::new("AI Studio")
            .id(egui::Id::new("engine_ai_studio"))
            .open(open)
            .default_width(980.0)
            .default_height(780.0)
            .resizable(true)
            .show(context, |ui| {
                self.show_header(ui);
                ui.separator();
                self.show_conversation(ui);
                ui.separator();
                self.show_proposal(ui);
                ui.separator();
                self.show_permissions(ui);
                ui.separator();
                self.show_runs(ui);
                ui.separator();
                self.show_persistence(ui);
                if let Some(status) = &self.status {
                    ui.separator();
                    ui.small(status);
                }
            });
    }

    fn show_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("AI Studio");
            ui.label("Conversation → proposal → Go → validate/playtest");
        });

        let providers = self.host.providers().cloned().collect::<Vec<_>>();
        ui.horizontal(|ui| {
            ui.strong("Provider");
            let selected_name = providers
                .iter()
                .find(|provider| provider.id == self.selected_provider)
                .map(|provider| provider.display_name.as_str())
                .unwrap_or("Select provider");
            egui::ComboBox::from_id_salt("ai_studio_provider")
                .selected_text(selected_name)
                .show_ui(ui, |ui| {
                    for provider in &providers {
                        ui.selectable_value(
                            &mut self.selected_provider,
                            provider.id.clone(),
                            &provider.display_name,
                        );
                    }
                });
            if let Some(provider) = providers
                .iter()
                .find(|provider| provider.id == self.selected_provider)
            {
                ui.monospace(provider_state_label(&provider.connection_state));
                ui.small(format!(
                    "{:?} / {:?}",
                    provider.runtime_kind, provider.authentication
                ));
            }
        });
        ui.small(
            "Provider credentials are not stored in AI session history. External agent and native model runtimes remain separate integration contracts.",
        );
    }

    fn show_conversation(&mut self, ui: &mut egui::Ui) {
        ui.strong("Conversation");
        egui::ScrollArea::vertical()
            .id_salt("ai_studio_conversation")
            .max_height(180.0)
            .show(ui, |ui| {
                if let Some(session) = self.host.session(&self.session_id) {
                    if session.conversation().is_empty() {
                        ui.weak("Describe what you want to create. Conversation can continue before, during, and after a run.");
                    }
                    for message in session.conversation() {
                        ui.group(|ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(match message.role() {
                                    ConversationRole::User => "You",
                                    ConversationRole::Agent => "Agent",
                                    ConversationRole::System => "Host",
                                });
                                ui.label(message.text());
                            });
                        });
                    }
                }
            });

        ui.add(
            egui::TextEdit::multiline(&mut self.message_input)
                .desired_rows(3)
                .hint_text("Discuss the game, constraints, or a design decision…"),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.message_input.trim().is_empty(),
                    egui::Button::new("Add message"),
                )
                .clicked()
            {
                let message = std::mem::take(&mut self.message_input);
                match self
                    .host
                    .add_message(&self.session_id, ConversationRole::User, message.trim())
                {
                    Ok(()) => {
                        self.status = Some("Conversation updated.".to_owned());
                        self.autosave_session();
                    }
                    Err(error) => self.set_host_error(error),
                }
            }
            ui.small(
                "Runtime adapters append agent replies through the same session contract; raw terminal text is diagnostics only.",
            );
        });
    }

    fn show_proposal(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Structured proposal");
            if let Some(proposal) = self
                .host
                .session(&self.session_id)
                .and_then(|session| session.current_proposal())
            {
                ui.monospace(format!("v{}", proposal.version().get()));
            } else {
                ui.weak("not created");
            }
        });

        egui::Grid::new("ai_studio_proposal_fields")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Goal");
                ui.text_edit_singleline(&mut self.proposal_goal);
                ui.end_row();

                ui.label("Requirements");
                ui.add(multiline_lines(&mut self.proposal_requirements, 2));
                ui.end_row();

                ui.label("Assumptions");
                ui.add(multiline_lines(&mut self.proposal_assumptions, 2));
                ui.end_row();

                ui.label("Acceptance");
                ui.add(multiline_lines(&mut self.proposal_acceptance, 3));
                ui.end_row();

                ui.label("Planned changes");
                ui.add(multiline_lines(&mut self.proposal_changes, 3));
                ui.end_row();

                ui.label("Validation / playtest");
                ui.add(multiline_lines(&mut self.proposal_validation, 3));
                ui.end_row();
            });
        ui.small("Use one item per line. Updating creates a new immutable proposal revision; existing runs keep their original snapshot.");

        let can_update = !self.proposal_goal.trim().is_empty();
        let provider_ready = self.host.provider_is_ready(&self.selected_provider);
        let has_proposal = self
            .host
            .session(&self.session_id)
            .and_then(|session| session.current_proposal())
            .is_some();
        let writer_active = self.host.active_writer().is_some();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_update, egui::Button::new("Update proposal"))
                .clicked()
            {
                let draft = AgentProposalDraft {
                    goal: self.proposal_goal.trim().to_owned(),
                    agreed_requirements: lines(&self.proposal_requirements),
                    assumptions: lines(&self.proposal_assumptions),
                    acceptance_criteria: lines(&self.proposal_acceptance),
                    planned_changes: lines(&self.proposal_changes),
                    validation_and_playtest_plan: lines(&self.proposal_validation),
                    expected_capabilities: self
                        .host
                        .permissions()
                        .project_grants()
                        .iter()
                        .copied()
                        .collect(),
                };
                match self.host.revise_proposal(&self.session_id, draft) {
                    Ok(version) => {
                        self.status = Some(format!("Proposal v{} recorded.", version.get()));
                        self.autosave_session();
                    }
                    Err(error) => self.set_host_error(error),
                }
            }

            let go = ui.add_enabled(
                has_proposal && provider_ready && !writer_active,
                egui::Button::new("Go"),
            );
            if go.clicked() {
                match self.host.start_run(&self.session_id, &self.selected_provider) {
                    Ok(run) => {
                        self.status = Some(format!(
                            "Run {run} started from the current immutable proposal snapshot."
                        ));
                        self.autosave_session();
                    }
                    Err(error) => self.set_host_error(error),
                }
            }
            if !provider_ready {
                ui.small("Go is disabled until the selected runtime adapter reports Ready.");
            } else if writer_active {
                ui.small("Another AI run currently owns the project writer role.");
            }
        });
    }

    fn show_permissions(&mut self, ui: &mut egui::Ui) {
        ui.strong("Project permission policy");
        ui.small("Managed services are the default. These persistent toggles store capabilities only—never credentials or literal command strings.");

        let mut changed = false;
        egui::Grid::new("ai_studio_permissions")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (permission, label) in permission_options() {
                    let mut allowed = self.host.permissions().project_grants().contains(&permission);
                    if ui.checkbox(&mut allowed, label).changed() {
                        self.host
                            .permissions_mut()
                            .set_project_grant(permission, allowed);
                        changed = true;
                    }
                    ui.small(permission_risk_label(permission));
                    ui.end_row();
                }
            });

        let pending = self
            .host
            .permissions()
            .pending_requests()
            .cloned()
            .collect::<Vec<_>>();
        for request in pending {
            ui.group(|ui| {
                ui.strong("Permission escalation");
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(request.run_id().as_str());
                    ui.label(format!("{:?}", request.permission()));
                });
                ui.label(request.reason());
                ui.horizontal(|ui| {
                    for (scope, label) in [
                        (ApprovalScope::AllowOnce, "Allow once"),
                        (ApprovalScope::AllowForRun, "Allow for this run"),
                        (ApprovalScope::AllowForProject, "Allow for this project"),
                        (ApprovalScope::Deny, "Deny"),
                    ] {
                        if ui.button(label).clicked() {
                            match self.host.resolve_permission(&self.session_id, request.id(), scope) {
                                Ok(()) => {
                                    if scope == ApprovalScope::AllowForProject {
                                        if let Err(error) = self.host.save_project_policy() {
                                            self.set_host_error(error);
                                            return;
                                        }
                                    }
                                    self.status = Some(format!(
                                        "Permission request {} resolved as {:?}.",
                                        request.id(),
                                        scope
                                    ));
                                    self.autosave_session();
                                }
                                Err(error) => self.set_host_error(error),
                            }
                        }
                    }
                });
            });
        }

        if changed {
            match self.host.save_project_policy() {
                Ok(path) => {
                    self.status = Some(format!("AI permission policy saved to {}", path.display()))
                }
                Err(error) => self.set_host_error(error),
            }
        }
        ui.small("GameEngine application policy is not a universal OS sandbox. External processes may still have the user's normal operating-system rights unless their provider or OS supplies stronger isolation.");
    }

    fn show_runs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Run / audit timeline");
            if let Some((session, run)) = self.host.active_writer() {
                if session == &self.session_id {
                    ui.monospace(format!("active: {run}"));
                }
            }
        });

        let active_run = self.host.active_writer().and_then(|(session, run)| {
            if session == &self.session_id {
                Some(run.clone())
            } else {
                None
            }
        });
        if let Some(run) = active_run {
            if ui.button("Stop run").clicked() {
                match self.host.cancel_run(&self.session_id, &run) {
                    Ok(()) => {
                        self.status = Some(format!("Run {run} cancelled."));
                        self.autosave_session();
                    }
                    Err(error) => self.set_host_error(error),
                }
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("ai_studio_audit")
            .max_height(170.0)
            .show(ui, |ui| {
                if let Some(session) = self.host.session(&self.session_id) {
                    if session.runs().is_empty() {
                        ui.weak("No runs yet. Go snapshots the current proposal; provider execution, validation, playtest, repair, and completion all publish structured events here.");
                    }
                    for run in session.runs().iter().rev() {
                        ui.horizontal_wrapped(|ui| {
                            ui.monospace(run.input().run_id().as_str());
                            ui.label(run_state_label(run.state()));
                            ui.small(format!(
                                "proposal v{}",
                                run.input().proposal().version().get()
                            ));
                        });
                        let audit = run.audit();
                        ui.small(format!(
                            "audit: authoring {} · code {} · assets {} · raw fs {} · custom commands {} · permissions {} · validation {} · playtest {}",
                            audit.authoring_operations,
                            audit.code_changes,
                            audit.external_acquisitions,
                            audit.raw_filesystem_accesses,
                            audit.custom_commands,
                            audit.permission_escalations,
                            audit.validation_records,
                            audit.playtest_records
                        ));
                    }
                    for event in session.events().iter().rev().take(12).rev() {
                        ui.small(format!("#{:04} {:?}", event.sequence(), event.kind()));
                    }
                }
            });
    }

    fn show_persistence(&mut self, ui: &mut egui::Ui) {
        ui.strong("Session persistence");
        let current = self
            .host
            .session(&self.session_id)
            .map_or(SessionVisibility::LocalPrivate, |session| session.visibility());
        let mut selected = current;
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut selected,
                SessionVisibility::LocalPrivate,
                "Local / private",
            );
            ui.radio_value(
                &mut selected,
                SessionVisibility::ProjectShared,
                "Project-shared",
            );
            if ui.button("Save session").clicked() {
                match self.host.save_session(&self.session_id) {
                    Ok(path) => {
                        self.status = Some(format!("AI session saved to {}", path.display()))
                    }
                    Err(error) => self.set_host_error(error),
                }
            }
        });
        if selected != current {
            match self.host.set_visibility(&self.session_id, selected) {
                Ok(()) => match self.host.save_session(&self.session_id) {
                    Ok(path) => {
                        self.status = Some(format!(
                            "Session visibility changed and saved to {}",
                            path.display()
                        ))
                    }
                    Err(error) => self.set_host_error(error),
                },
                Err(error) => self.set_host_error(error),
            }
        }
        ui.small(match selected {
            SessionVisibility::LocalPrivate => {
                "Default: conversation/run history stays outside the project working tree."
            }
            SessionVisibility::ProjectShared => {
                "Shared: portable history is written under .gameengine/ai/sessions/<session-id>/; workspaces, credentials, ports, and process state remain local."
            }
        });
    }

    fn autosave_session(&mut self) {
        if let Err(error) = self.host.save_session(&self.session_id) {
            self.set_host_error(error);
        }
    }

    fn set_host_error(&mut self, error: crate::agent::AgentHostError) {
        self.status = Some(format!("{}: {}", error.code(), error.message()));
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn multiline_lines<'a>(text: &'a mut String, rows: usize) -> egui::TextEdit<'a> {
    egui::TextEdit::multiline(text)
        .desired_rows(rows)
        .hint_text("one item per line")
}

fn provider_state_label(state: &ProviderConnectionState) -> &'static str {
    match state {
        ProviderConnectionState::Disconnected => "Disconnected",
        ProviderConnectionState::AuthenticationRequired => "Authentication required",
        ProviderConnectionState::Ready => "Ready",
        ProviderConnectionState::Error(_) => "Error",
    }
}

fn run_state_label(state: AgentRunState) -> &'static str {
    match state {
        AgentRunState::Inspecting => "Inspecting",
        AgentRunState::Planning => "Planning",
        AgentRunState::Executing => "Executing",
        AgentRunState::AwaitingUser => "Awaiting user",
        AgentRunState::Validating => "Validating",
        AgentRunState::Playtesting => "Playtesting",
        AgentRunState::Evaluating => "Evaluating",
        AgentRunState::Repairing => "Repairing",
        AgentRunState::Completed => "Completed",
        AgentRunState::Failed => "Failed",
        AgentRunState::Cancelled => "Cancelled",
    }
}

fn permission_options() -> [(AgentPermission, &'static str); 10] {
    [
        (
            AgentPermission::Authoring(AuthoringPermission::ProjectDataWrite),
            "Project authoring writes",
        ),
        (
            AgentPermission::Authoring(AuthoringPermission::AssetWrite),
            "Asset writes",
        ),
        (
            AgentPermission::Authoring(AuthoringPermission::CodeWrite),
            "Apply source changes",
        ),
        (
            AgentPermission::Agent(AgentCapability::NetworkAccess),
            "Network access",
        ),
        (
            AgentPermission::Agent(AgentCapability::ExternalAssetAcquisition),
            "External asset acquisition",
        ),
        (
            AgentPermission::Agent(AgentCapability::RuntimeLaunch),
            "Runtime launch",
        ),
        (
            AgentPermission::Agent(AgentCapability::RuntimeControl),
            "Runtime input / control",
        ),
        (
            AgentPermission::Agent(AgentCapability::FrameCapture),
            "Frame capture",
        ),
        (
            AgentPermission::Agent(AgentCapability::WorkspaceFilesystem),
            "Raw workspace filesystem",
        ),
        (
            AgentPermission::Agent(AgentCapability::ArbitraryCommandExecution),
            "Arbitrary command execution",
        ),
    ]
}

fn permission_risk_label(permission: AgentPermission) -> &'static str {
    match permission {
        AgentPermission::Authoring(AuthoringPermission::ProjectDataWrite) => "Managed via MCP",
        AgentPermission::Authoring(AuthoringPermission::AssetWrite) => "Managed asset pipeline",
        AgentPermission::Authoring(AuthoringPermission::CodeWrite) => "Managed code workspace",
        AgentPermission::Authoring(AuthoringPermission::Read)
        | AgentPermission::Authoring(AuthoringPermission::Preview) => "Structured authoring",
        AgentPermission::Authoring(AuthoringPermission::CommandExec) => "Legacy authoring exec",
        AgentPermission::Agent(AgentCapability::NetworkAccess) => "External side effect",
        AgentPermission::Agent(AgentCapability::ExternalAssetAcquisition) => {
            "Provider + import pipeline"
        }
        AgentPermission::Agent(AgentCapability::RuntimeLaunch) => "Managed runtime",
        AgentPermission::Agent(AgentCapability::RuntimeControl) => "AI Agent Bridge",
        AgentPermission::Agent(AgentCapability::FrameCapture) => "AI Agent Bridge",
        AgentPermission::Agent(AgentCapability::WorkspaceFilesystem) => "Escape hatch",
        AgentPermission::Agent(AgentCapability::ArbitraryCommandExecution) => "Escape hatch",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_line_parser_discards_empty_lines() {
        assert_eq!(
            lines("first\n\n second \n"),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn dangerous_permissions_are_labeled_as_escape_hatches() {
        assert_eq!(
            permission_risk_label(AgentPermission::Agent(
                AgentCapability::ArbitraryCommandExecution
            )),
            "Escape hatch"
        );
    }
}
