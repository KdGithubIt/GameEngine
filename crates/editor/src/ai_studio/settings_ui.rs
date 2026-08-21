//! The AI Studio configuration tier.
//!
//! ADR 0162 §5 separates configuration from selection. Everything drawn here
//! changes what exists on this machine, what the project may reach, or what
//! credentials are stored; choosing among what is already configured belongs to
//! the composer and stays in the parent module. The sections are named so a
//! rare setup task never shares a scroll position with a frequent one.

use super::*;
use crate::external_agent_provider::{ExternalAgentAuthStatus, ExternalAgentDiscoveryStatus};

/// A named section of the AI Studio configuration tier.
///
/// ADR 0162 §5 organizes configuration by what a control changes rather than by
/// which subsystem owns it, so a rare setup task never shares a scroll position
/// with a frequent one. Nothing here selects what the next message uses; that
/// belongs to the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsSection {
    /// Registering, installing, removing, and resourcing models.
    Models,
    /// Installing, signing in, and confining external agent programs.
    ///
    /// ADR 0164 §3 names this after the thing the user is setting up rather
    /// than after the internal category that keeps two execution paths apart.
    Agents,
    /// Where an agent runs.
    Environment,
    /// Characterizing models and runtimes.
    Benchmarks,
    /// Reaching this studio from another device.
    Remote,
    /// Where the studio itself is drawn.
    Presentation,
}

impl SettingsSection {
    /// Every section, in the order the navigation lists them.
    pub(super) const ALL: [Self; 6] = [
        Self::Models,
        Self::Agents,
        Self::Environment,
        Self::Benchmarks,
        Self::Remote,
        Self::Presentation,
    ];

    /// Returns the navigation label.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Models => "Models",
            Self::Agents => "Agents",
            Self::Environment => "Environment",
            Self::Benchmarks => "Benchmarks",
            Self::Remote => "Remote",
            Self::Presentation => "Presentation",
        }
    }

    /// Returns what the section is for, as one line.
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Models => {
                "Register, install, remove, and resource the models this machine can run."
            }
            Self::Agents => {
                "Install and sign in the agent programs this machine can run, and decide how tightly they are confined."
            }
            Self::Environment => "Decide where an external agent process runs.",
            Self::Benchmarks => "Characterize models and runtimes on reproducible tasks.",
            Self::Remote => "Reach this studio from another device on the private network.",
            Self::Presentation => "Draw the studio inside the Editor or in its own OS window.",
        }
    }
}

/// What the selected external provider can do right now.
///
/// The studio used to report discovery and authentication as two independent
/// sentences and leave the reader to combine them into an answer. This is that
/// answer: one state, with the one action that changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderReadiness {
    /// A probe or a setup command is running, so any reported state is stale.
    Working,
    /// Installed and signed in; Ask and Build may use it.
    Ready,
    /// Installed, but the provider has no usable credential.
    SignInRequired,
    /// The provider program is not on this machine.
    NotInstalled,
    /// A generic compatible-agent command has not been entered.
    NotConfigured,
    /// Nothing has looked at this machine yet.
    NotChecked,
}

impl ProviderReadiness {
    /// Returns the state named in the fewest words that stay accurate.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Working => "Checking",
            Self::Ready => "Ready",
            Self::SignInRequired => "Sign-in required",
            Self::NotInstalled => "Not installed",
            Self::NotConfigured => "Not configured",
            Self::NotChecked => "Not checked",
        }
    }

    /// Returns the tone that carries this state.
    fn tone(self) -> theme::StatusTone {
        match self {
            Self::Working => theme::StatusTone::Busy,
            Self::Ready => theme::StatusTone::Ready,
            Self::SignInRequired | Self::NotConfigured => theme::StatusTone::Attention,
            Self::NotInstalled => theme::StatusTone::Blocked,
            Self::NotChecked => theme::StatusTone::Idle,
        }
    }

    /// Returns what the reader has to do next, or what is already true.
    fn next_step(self, provider: &str) -> String {
        match self {
            Self::Working => {
                "Checking this machine. The state below is from the last check.".to_owned()
            }
            Self::Ready => format!("{provider} can answer Ask and run Build."),
            Self::SignInRequired => {
                format!(
                    "Sign in below. The credential stays with {provider}; GameEngine never stores it."
                )
            }
            Self::NotInstalled => {
                format!(
                    "Install {provider} below, then sign in. Refresh status re-checks this machine."
                )
            }
            Self::NotConfigured => {
                "Enter the compatible agent program below, then use Refresh status.".to_owned()
            }
            Self::NotChecked => {
                "Use Refresh status to find out whether this provider is installed and signed in."
                    .to_owned()
            }
        }
    }
}

/// Returns why the Agents section needs attention in the given state.
fn section_attention_for(
    readiness: ProviderReadiness,
) -> Option<(theme::StatusTone, &'static str)> {
    match readiness {
        ProviderReadiness::NotInstalled => Some((
            theme::StatusTone::Blocked,
            "The agent selected on the composer is not installed on this machine.",
        )),
        ProviderReadiness::SignInRequired => Some((
            theme::StatusTone::Attention,
            "The agent selected on the composer is installed but not signed in.",
        )),
        ProviderReadiness::NotConfigured => Some((
            theme::StatusTone::Attention,
            "The compatible agent program has not been entered under Advanced.",
        )),
        ProviderReadiness::Working | ProviderReadiness::Ready | ProviderReadiness::NotChecked => {
            None
        }
    }
}

/// Returns what a provider can do right now, from what is known about it.
///
/// `working` means a probe or a setup command is in flight, so whatever the
/// status says is about the machine as it was before that command started.
fn provider_readiness(
    status: &ExternalAgentProviderStatus,
    kind: ExternalAgentProviderKind,
    working: bool,
) -> ProviderReadiness {
    if working {
        return ProviderReadiness::Working;
    }
    if status.ready() {
        return ProviderReadiness::Ready;
    }
    match status.discovery {
        ExternalAgentDiscoveryStatus::Unchecked => ProviderReadiness::NotChecked,
        ExternalAgentDiscoveryStatus::Unavailable => {
            if kind == ExternalAgentProviderKind::Generic {
                ProviderReadiness::NotConfigured
            } else {
                ProviderReadiness::NotInstalled
            }
        }
        ExternalAgentDiscoveryStatus::Available => match status.auth {
            ExternalAgentAuthStatus::Unchecked => ProviderReadiness::NotChecked,
            _ => ProviderReadiness::SignInRequired,
        },
    }
}

/// Returns how a discovery result is worded and toned in the studio.
///
/// The protocol wording (`available`, `not found`) stays on
/// [`ExternalAgentDiscoveryStatus::label`] for remote reporting; a person
/// reading a row labeled "Installed" wants a yes or a no.
fn discovery_presentation(
    status: ExternalAgentDiscoveryStatus,
) -> (&'static str, theme::StatusTone) {
    match status {
        ExternalAgentDiscoveryStatus::Available => ("Yes", theme::StatusTone::Ready),
        ExternalAgentDiscoveryStatus::Unavailable => ("No", theme::StatusTone::Blocked),
        ExternalAgentDiscoveryStatus::Unchecked => ("Not checked", theme::StatusTone::Idle),
    }
}

/// Returns how an authentication result is worded and toned in the studio.
fn auth_presentation(status: ExternalAgentAuthStatus) -> (&'static str, theme::StatusTone) {
    match status {
        ExternalAgentAuthStatus::Authenticated => ("Yes", theme::StatusTone::Ready),
        ExternalAgentAuthStatus::SignInRequired => ("No", theme::StatusTone::Attention),
        ExternalAgentAuthStatus::Unchecked => ("Not checked", theme::StatusTone::Idle),
        ExternalAgentAuthStatus::NotApplicable => ("Not applicable", theme::StatusTone::Idle),
        ExternalAgentAuthStatus::Unavailable => {
            ("Provider unavailable", theme::StatusTone::Blocked)
        }
    }
}

/// Whether another device can be handed a URL that reaches this studio.
///
/// ADR 0164 §4 refuses to present the loopback URL as a substitute, so "not
/// ready" is a state this section has to be able to report, with the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhoneAccessReadiness {
    /// The loopback gateway is not running, so there is nothing to publish.
    GatewayUnavailable,
    /// The gateway is running, but no usable external origin is configured.
    BaseUnusable(PhoneUrlBaseError),
    /// A phone URL exists and can be copied.
    Ready,
}

impl PhoneAccessReadiness {
    /// Returns the state named in the fewest words that stay accurate.
    ///
    /// The ready label deliberately says that a URL exists, not that the phone
    /// can reach it: the external hop is user-owned and GameEngine cannot test
    /// it from here.
    const fn label(self) -> &'static str {
        match self {
            Self::GatewayUnavailable => "Gateway not running",
            Self::BaseUnusable(_) => "Not ready",
            Self::Ready => "URL ready",
        }
    }

    /// Returns the tone that carries this state.
    const fn tone(self) -> theme::StatusTone {
        match self {
            Self::GatewayUnavailable => theme::StatusTone::Blocked,
            Self::BaseUnusable(_) => theme::StatusTone::Attention,
            Self::Ready => theme::StatusTone::Ready,
        }
    }

    /// Returns what the reader has to do next, or what is already true.
    const fn next_step(self) -> &'static str {
        match self {
            Self::GatewayUnavailable => {
                "The companion gateway could not start on this machine, so there is no session to publish yet."
            }
            Self::BaseUnusable(error) => error.reason(),
            Self::Ready => {
                "Copy this URL and open it on your phone. GameEngine cannot test the connection from here: if the phone cannot open it, check that the phone is signed in to the same private network."
            }
        }
    }
}

impl AiStudioPanel {
    /// Draws how another device reaches this studio.
    ///
    /// ADR 0164 §4 makes one reachable URL the primary content. The gateway's
    /// loopback address is what the reverse proxy in front of it needs, not
    /// what a phone can open — `127.0.0.1` names the phone — so it is disclosed
    /// rather than displayed.
    pub(super) fn show_remote_companion(&mut self, ui: &mut egui::Ui) {
        let base = self.remote_phone_url_base.clone();
        let phone_url = self
            .remote_server
            .as_ref()
            .map(|server| server.phone_url(&base));
        let readiness = match phone_url.as_ref() {
            None => PhoneAccessReadiness::GatewayUnavailable,
            Some(Err(error)) => PhoneAccessReadiness::BaseUnusable(*error),
            Some(Ok(_)) => PhoneAccessReadiness::Ready,
        };
        let mut copy_phone_url = None;
        let mut base_edited = false;
        theme::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Phone access");
                theme::status_pill(ui, readiness.tone(), readiness.label());
            });
            theme::hint(ui, readiness.next_step());
            if let Some(Ok(url)) = phone_url.as_ref() {
                ui.add_space(6.0);
                theme::caption(ui, "Phone URL");
                match masked_phone_url(&base) {
                    Ok(masked) => {
                        theme::selectable_text(ui, egui::RichText::new(masked).monospace());
                    }
                    Err(error) => theme::hint(ui, error.reason()),
                }
                if ui.button("Copy phone URL").clicked() {
                    copy_phone_url = Some(url.clone());
                }
                theme::spec_note(
                    ui,
                    "The copied URL is a credential",
                    "It carries the access token that authorizes this session on top of your private network's device identity. Send it to your own device only, and copy it again after restarting the Editor.",
                );
            }
            ui.add_space(6.0);
            theme::caption(ui, "Address your private network publishes for this PC");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.remote_phone_url_base)
                    .desired_width(f32::INFINITY)
                    .hint_text("https://my-pc.example-tailnet.ts.net"),
            );
            if response.lost_focus() {
                base_edited = true;
            }
            ui.small("Your phone must be connected to that private network.");
        });
        if let Some(url) = copy_phone_url {
            ui.ctx().copy_text(url);
            self.status = Some("Phone URL copied. It carries the session access token.".to_owned());
        }
        if base_edited {
            self.save_preferences();
        }
        self.show_remote_advanced(ui);
    }

    /// Draws the gateway detail the reverse proxy needs, once per machine.
    ///
    /// ADR 0164 §4 keeps this collapsed: it is what produces the external
    /// origin entered above, and it is not the product.
    fn show_remote_advanced(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.remote_server.as_ref() else {
            return;
        };
        let endpoint = server.endpoint().to_owned();
        let companion_url = server.companion_url();
        let mut copy_local_url = None;
        egui::CollapsingHeader::new("Advanced")
            .id_salt("ai_studio_remote_advanced")
            .default_open(false)
            .show(ui, |ui| {
                theme::field_row(
                    ui,
                    "Local gateway",
                    egui::RichText::new(&endpoint).monospace(),
                );
                ui.small("Publish this loopback address to your private network with your own HTTPS reverse proxy, then enter the origin it serves above. GameEngine never binds the gateway to a LAN address, a public address, or a forwarded port.");
                if ui.button("Copy local companion URL").clicked() {
                    copy_local_url = Some(companion_url.clone());
                }
                ui.small("The local URL carries the same access token and opens only on this PC. It is useful for testing the gateway before the proxy exists.");
                theme::spec_note(
                    ui,
                    "What remote access does not reach",
                    "Remote authentication is separate from Agent Host permissions, and the Editor MCP endpoint is never exposed remotely.",
                );
            });
        if let Some(url) = copy_local_url {
            ui.ctx().copy_text(url);
            self.status = Some("Local companion URL copied. It opens only on this PC.".to_owned());
        }
    }

    /// Draws the configuration tier reached from the studio header.
    ///
    /// ADR 0162 §5 splits configuration from selection: everything that changes
    /// what exists on this machine, what the project may reach, or what
    /// credentials are stored lives here, in named sections rather than in one
    /// scrolling column. Choosing among what is already configured belongs to
    /// the composer instead.
    pub(super) fn show_settings_surface(&mut self, context: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = self.settings_open;
        egui::Window::new("AI Studio settings")
            .id(egui::Id::new("ai_studio_settings"))
            .open(&mut open)
            .default_width(720.0)
            .default_height(620.0)
            .resizable(true)
            .show(context, |ui| {
                theme::apply_studio_style(ui);
                egui::Panel::left("ai_studio_settings_nav")
                    .resizable(false)
                    .exact_size(168.0)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
                    .show_inside(ui, |ui| {
                        for section in SettingsSection::ALL {
                            let selected = self.settings_section == section;
                            let attention = self.section_attention(section);
                            let hover = match attention {
                                Some((_, reason)) => {
                                    format!(
                                        "{}

{reason}",
                                        section.description()
                                    )
                                }
                                None => section.description().to_owned(),
                            };
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(selected, section.label())
                                    .on_hover_text(hover)
                                    .clicked()
                                {
                                    self.settings_section = section;
                                }
                                if let Some((tone, _)) = attention {
                                    theme::status_dot(ui, tone.color());
                                }
                            });
                        }
                    });
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 4)))
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("ai_studio_settings_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let section = self.settings_section;
                                theme::card_header(ui, section.label());
                                theme::hint(ui, section.description());
                                ui.add_space(4.0);
                                match section {
                                    SettingsSection::Models => self.show_local_model_settings(ui),
                                    SettingsSection::Agents => self.show_agent_settings(ui),
                                    SettingsSection::Environment => {
                                        self.show_environment_settings(ui);
                                    }
                                    SettingsSection::Benchmarks => self.show_agent_benchmark(ui),
                                    SettingsSection::Remote => self.show_remote_companion(ui),
                                    SettingsSection::Presentation => {
                                        self.show_presentation_settings(ui);
                                    }
                                }
                            });
                    });
            });
        self.settings_open = open;
    }

    /// Draws the control that chooses where the studio is presented.
    ///
    /// ADR 0147 keeps detach and reattach presentation operations that never
    /// touch the session, the runtime, or the workspace behind them. They are
    /// read here rather than from a row above the transcript, where a control
    /// used once per machine cost the conversation a line of height on every
    /// frame.
    pub(super) fn show_presentation_settings(&mut self, ui: &mut egui::Ui) {
        let detached = self.presentation.mode == AiStudioPresentationMode::Detached;
        let (label, hint) = if detached {
            (
                "Reattach",
                "The studio is its own OS window. Reattach it to draw it inside the Editor.",
            )
        } else {
            (
                "Detach",
                "The studio is drawn inside the Editor. Detach it into its own OS window.",
            )
        };
        ui.horizontal(|ui| {
            if ui.button(label).clicked() {
                self.presentation_toggle_requested = true;
            }
            ui.small("Same project Agent Host either way.");
        });
        theme::hint(ui, hint);
    }

    /// Draws where an external agent provider runs.
    ///
    /// ADR 0160 makes placement a property of the provider launch rather than
    /// of the provider itself, and ADR 0162 §5 keeps it out of the section that
    /// chooses and authenticates providers.
    pub(super) fn show_environment_settings(&mut self, ui: &mut egui::Ui) {
        if self.external_provider_kind == ExternalAgentProviderKind::Generic {
            theme::hint(
                ui,
                "The Generic compatible-agent command runs where the Editor runs. Select a first-class provider to place it.",
            );
            return;
        }
        let previous_environment = self.external_provider_environment;
        let previous_distribution = self.external_provider_wsl_distribution.clone();
        ui.horizontal(|ui| {
            ui.label("Provider runs in");
            egui::ComboBox::from_id_salt("ai_studio_external_agent_environment")
                .selected_text(self.external_provider_environment.label())
                .show_ui(ui, |ui| {
                    for environment in ExternalAgentExecutionEnvironment::ALL {
                        ui.selectable_value(
                            &mut self.external_provider_environment,
                            environment,
                            environment.label(),
                        );
                    }
                });
            if self.external_provider_environment == ExternalAgentExecutionEnvironment::Wsl2Linux {
                ui.add(
                    egui::TextEdit::singleline(&mut self.external_provider_wsl_distribution)
                        .desired_width(160.0)
                        .hint_text("default distribution"),
                );
            }
        });
        if self.external_provider_environment == ExternalAgentExecutionEnvironment::Wsl2Linux {
            ui.small(
                "The provider CLI and its sign-in live inside the distribution. The Editor MCP endpoint stays bound to loopback, so the distribution must share the host loopback (WSL mirrored networking).",
            );
        }
        if previous_environment != self.external_provider_environment
            || previous_distribution != self.external_provider_wsl_distribution
        {
            self.external_provider_status =
                ExternalAgentProviderStatus::unchecked(self.external_provider_kind);
            self.save_preferences();
        }
    }

    pub(super) fn show_local_model_settings(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Model backend · questions and native runs")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    // ADR 0164 §1: who runs the next message is chosen on the
                    // composer. This only decides which backend's settings the
                    // rest of this section is about.
                    ui.label("Settings for");
                    egui::ComboBox::from_id_salt("ai_studio_model_backend")
                        .selected_text(match self.settings_model_view {
                            ModelBackendPreference::Local => "External local (Ollama-compatible)",
                            ModelBackendPreference::ManagedLocal => "Managed Local AI",
                            ModelBackendPreference::HostedApi => "Hosted API",
                            ModelBackendPreference::Enterprise => "Enterprise",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.settings_model_view,
                                ModelBackendPreference::ManagedLocal,
                                "Managed Local AI",
                            );
                            ui.selectable_value(
                                &mut self.settings_model_view,
                                ModelBackendPreference::Local,
                                "External local (Ollama-compatible)",
                            );
                            ui.selectable_value(
                                &mut self.settings_model_view,
                                ModelBackendPreference::HostedApi,
                                "Hosted API",
                            );
                            ui.selectable_value(
                                &mut self.settings_model_view,
                                ModelBackendPreference::Enterprise,
                                "Enterprise",
                            );
                        });
                });
                ui.small(match self.settings_model_view {
                    ModelBackendPreference::Local => "Processing posture: external loopback local runtime; existing Ollama-compatible settings retain their original meaning.",
                    ModelBackendPreference::ManagedLocal => "Processing posture: GameEngine-managed llama.cpp on this machine; the inference server remains loopback-only and never gains authoring authority.",
                    ModelBackendPreference::HostedApi => "Processing posture: selected task context is sent to the configured remote HTTPS provider only after Network access approval.",
                    ModelBackendPreference::Enterprise => "Processing posture: selected task context is sent to the configured enterprise HTTPS endpoint only after Network access approval.",
                });
                // ADR 0164 §1 keeps Effort on the composer beside Mode and AI,
                // so it is described here rather than offered a second time.
                ui.small(
                    "Effort is chosen on the composer. It is a machine-local latency/reasoning preference, and remote GPU controls are never projected as local residency controls.",
                );
                match self.settings_model_view {
                    ModelBackendPreference::ManagedLocal => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Execution environment");
                            let previous = self.managed_execution_environment;
                            egui::ComboBox::from_id_salt("ai_studio_managed_environment")
                                .selected_text(self.managed_execution_environment.label())
                                .show_ui(ui, |ui| {
                                    for environment in ManagedExecutionEnvironment::ALL {
                                        ui.selectable_value(
                                            &mut self.managed_execution_environment,
                                            environment,
                                            environment.label(),
                                        );
                                    }
                                });
                            if self.managed_execution_environment != previous {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        ui.small(
                            "Managed Local AI remains an engineering path until Windows-native versus WSL2 characterization selects the normal product default. Windows native is the currently selected candidate; WSL2 remains available for characterization and fallback in the dedicated GameEngine-LocalAI distribution.",
                        );
                        let probe = self.managed_probe_for_panel();
                        let setup_status = probe.as_ref().map(|probe| probe.setup_status.clone());
                        let setup_busy = self.managed_setup_task.is_some();
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Runtime");
                            ui.monospace(format!(
                                "llama.cpp {PINNED_LLAMA_CPP_TAG} @ {PINNED_LLAMA_CPP_REVISION}"
                            ));
                            match setup_status.as_ref() {
                                None => {
                                    ui.weak("Checking...");
                                }
                                Some(ManagedSetupStatus::Ready) => {
                                    ui.strong("Ready");
                                }
                                Some(ManagedSetupStatus::RuntimeNotInstalled) => {
                                    if ui
                                        .add_enabled(
                                            !setup_busy,
                                            egui::Button::new("Set up Local AI"),
                                        )
                                        .clicked()
                                    {
                                        self.start_managed_setup(
                                            ManagedSetupOperation::InstallRuntime(
                                                self.managed_execution_environment,
                                            ),
                                            "Downloading and verifying the pinned GameEngine llama.cpp runtime...",
                                        );
                                    }
                                }
                                Some(ManagedSetupStatus::WslDistributionMissing) => {
                                    if ui
                                        .add_enabled(
                                            !setup_busy,
                                            egui::Button::new("Set up WSL2 Local AI"),
                                        )
                                        .clicked()
                                    {
                                        self.start_managed_setup(
                                            ManagedSetupOperation::ProvisionWsl,
                                            "Provisioning the dedicated GameEngine-LocalAI WSL environment. Windows may require explicit elevation or restart; GameEngine never bypasses either boundary.",
                                        );
                                    }
                                }
                                Some(ManagedSetupStatus::RestartRequired) => {
                                    ui.strong("Restart required");
                                }
                                Some(ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(
                                    message,
                                )) => {
                                    ui.strong("Unavailable");
                                    ui.small(message);
                                }
                            }
                            if setup_busy || setup_status.is_none() {
                                ui.spinner();
                            }
                        });
                        let mut goose_status =
                            self.managed_local_runtime.managed_goose_setup_status();
                        if let Some(error) = self.status.as_deref().filter(|status| {
                            status.contains("Goose setup failed")
                                || status.contains("Managed Local ACP cannot start")
                        }) {
                            goose_status = ManagedGooseSetupStatus::Invalid(error.to_owned());
                        }
                        theme::card(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong("ACP agent runtime");
                                ui.monospace(format!("Goose {PINNED_GOOSE_VERSION}"));
                                match &goose_status {
                                    ManagedGooseSetupStatus::Ready => {
                                        theme::status_pill(ui, theme::StatusTone::Ready, "Ready");
                                    }
                                    ManagedGooseSetupStatus::NotInstalled => {
                                        theme::status_pill(
                                            ui,
                                            theme::StatusTone::Blocked,
                                            "Not installed",
                                        );
                                    }
                                    ManagedGooseSetupStatus::Invalid(_) => {
                                        theme::status_pill(
                                            ui,
                                            theme::StatusTone::Blocked,
                                            "Needs repair",
                                        );
                                    }
                                }
                                if setup_busy {
                                    ui.spinner();
                                    ui.small("Setup in progress");
                                }
                            });
                            match &goose_status {
                                ManagedGooseSetupStatus::Ready => {
                                    ui.small(
                                        "GameEngine will use this managed Goose first for AI Studio, Benchmark Campaign, and other Managed Local ACP sessions.",
                                    );
                                }
                                ManagedGooseSetupStatus::NotInstalled => {
                                    ui.small(
                                        "Managed Local conversations use Goose over ACP. GameEngine can download and verify the pinned Goose runtime; PATH and environment-variable setup are not required.",
                                    );
                                }
                                ManagedGooseSetupStatus::Invalid(error) => {
                                    ui.small(format!("Managed Goose needs repair: {error}"));
                                }
                            }
                            if !matches!(&goose_status, ManagedGooseSetupStatus::Ready)
                                && ui
                                    .add_enabled(
                                        !setup_busy,
                                        egui::Button::new(if matches!(
                                            &goose_status,
                                            ManagedGooseSetupStatus::Invalid(_)
                                        ) {
                                            "Retry Goose setup"
                                        } else {
                                            "Install Goose"
                                        }),
                                    )
                                    .clicked()
                            {
                                self.start_managed_setup(
                                    ManagedSetupOperation::InstallGoose,
                                    "Downloading, verifying, and activating the pinned Goose ACP runtime...",
                                );
                            }
                            egui::CollapsingHeader::new("Advanced Goose executable override")
                                .default_open(false)
                                .show(ui, |ui| {
                                    let override_path = self
                                        .managed_local_runtime
                                        .goose_executable_override()
                                        .ok()
                                        .flatten();
                                    match override_path.as_ref() {
                                        Some(path) => ui.small(format!(
                                            "Machine-local override: {}",
                                            path.display()
                                        )),
                                        None => ui.small(
                                            "No machine-local override. Managed Goose remains the normal path.",
                                        ),
                                    };
                                    ui.horizontal_wrapped(|ui| {
                                        if ui
                                            .add_enabled(
                                                !setup_busy,
                                                egui::Button::new("Choose Goose executable..."),
                                            )
                                            .clicked()
                                            && let Some(path) = rfd::FileDialog::new()
                                                .add_filter("Goose executable", &["exe"])
                                                .pick_file()
                                        {
                                            match self
                                                .managed_local_runtime
                                                .set_goose_executable_override(Some(path))
                                            {
                                                Ok(()) => {
                                                    self.status = Some(
                                                        "Saved the machine-local Goose executable override."
                                                            .to_owned(),
                                                    );
                                                }
                                                Err(error) => {
                                                    self.status = Some(format!(
                                                        "Could not save Goose override: {error}"
                                                    ));
                                                }
                                            }
                                        }
                                        if override_path.is_some()
                                            && ui
                                                .add_enabled(
                                                    !setup_busy,
                                                    egui::Button::new("Clear override"),
                                                )
                                                .clicked()
                                        {
                                            match self
                                                .managed_local_runtime
                                                .set_goose_executable_override(None)
                                            {
                                                Ok(()) => {
                                                    self.status = Some(
                                                        "Cleared the machine-local Goose executable override."
                                                            .to_owned(),
                                                    );
                                                }
                                                Err(error) => {
                                                    self.status = Some(format!(
                                                        "Could not clear Goose override: {error}"
                                                    ));
                                                }
                                            }
                                        }
                                    });
                                    ui.small(
                                        "Discovery order is GameEngine-managed Goose, this machine-local override, GAMEENGINE_GOOSE_EXECUTABLE, PATH, then legacy home locations.",
                                    );
                                });
                        });
                        if matches!(setup_status.as_ref(), Some(ManagedSetupStatus::RestartRequired))
                        {
                            ui.small(
                                "Windows reported that setup requires a restart. GameEngine persists only a machine-local continuation marker and does not reboot automatically. Reopen the Editor after the restart to continue.",
                            );
                        }
                        if matches!(
                            setup_status.as_ref(),
                            Some(
                                ManagedSetupStatus::Ready
                                    | ManagedSetupStatus::WslDistributionMissing
                                    | ManagedSetupStatus::RestartRequired
                            )
                        ) {
                            ui.horizontal_wrapped(|ui| {
                                ui.small(
                                    "Removal deletes GameEngine-owned runtime/cache state and unregisters only the dedicated GameEngine-LocalAI WSL distribution. User-owned GGUF source files are preserved.",
                                );
                                if ui
                                    .add_enabled(
                                        !setup_busy,
                                        egui::Button::new("Remove managed environment"),
                                    )
                                    .clicked()
                                {
                                    self.start_managed_setup(
                                        ManagedSetupOperation::RemoveEnvironment(
                                            self.managed_execution_environment,
                                        ),
                                        "Removing the selected GameEngine-managed Local AI environment...",
                                    );
                                }
                            });
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Managed GGUF");
                            let models = self
                                .managed_local_runtime
                                .registered_models()
                                .unwrap_or_default();
                            egui::ComboBox::from_id_salt("ai_studio_managed_model")
                                .selected_text(
                                    models
                                        .iter()
                                        .find(|model| model.model_id == self.managed_model_id)
                                        .map(|model| model.display_name.as_str())
                                        .unwrap_or("Select registered GGUF"),
                                )
                                .width(260.0)
                                .show_ui(ui, |ui| {
                                    for model in &models {
                                        if ui
                                            .selectable_label(
                                                self.managed_model_id == model.model_id,
                                                &model.display_name,
                                            )
                                            .clicked()
                                        {
                                            self.managed_model_id = model.model_id.clone();
                                            self.last_model_resource_telemetry =
                                                ModelResourceTelemetry::default();
                                            self.save_preferences();
                                        }
                                    }
                                });
                            if ui
                                .add_enabled(
                                    !setup_busy,
                                    egui::Button::new("Register existing GGUF..."),
                                )
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("GGUF model", &["gguf"])
                                    .pick_file()
                            {
                                self.start_managed_setup(
                                    ManagedSetupOperation::RegisterModel(path),
                                    "Hashing and registering the exact GGUF bytes without modifying the source file...",
                                );
                            }
                        });
                        let selected_model = self
                            .managed_local_runtime
                            .registered_models()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|model| model.model_id == self.managed_model_id);
                        if let Some(model) = selected_model {
                            ui.small(format!(
                                "Representation: sha256={} · size={} · quantization={}",
                                model.content_sha256,
                                format_model_bytes(model.size_bytes),
                                optional_text(model.quantization.as_deref()),
                            ));
                            ui.horizontal_wrapped(|ui| {
                                match model.projector.as_ref() {
                                    Some(projector) => {
                                        ui.small(format!(
                                            "Vision projector: {} · sha256={}",
                                            format_model_bytes(projector.size_bytes),
                                            projector.content_sha256,
                                        ));
                                        if ui
                                            .add_enabled(
                                                !setup_busy,
                                                egui::Button::new("Remove projector"),
                                            )
                                            .clicked()
                                        {
                                            self.start_managed_setup(
                                                ManagedSetupOperation::RemoveProjector {
                                                    model_id: model.model_id.clone(),
                                                },
                                                "Removing the vision projector registration; the model returns to text-only input.",
                                            );
                                        }
                                    }
                                    None => {
                                        ui.small("Vision projector: none (text input only).");
                                        if ui
                                            .add_enabled(
                                                !setup_busy,
                                                egui::Button::new("Register projector..."),
                                            )
                                            .clicked()
                                            && let Some(path) = rfd::FileDialog::new()
                                                .add_filter("GGUF projector", &["gguf"])
                                                .pick_file()
                                        {
                                            self.start_managed_setup(
                                                ManagedSetupOperation::RegisterProjector {
                                                    model_id: model.model_id.clone(),
                                                    path,
                                                },
                                                "Hashing and registering the multimodal projector that gives this model image input...",
                                            );
                                        }
                                    }
                                }
                            });
                            if self.managed_execution_environment
                                == ManagedExecutionEnvironment::Wsl2Linux
                            {
                                match probe.as_ref().map(|probe| &probe.additional_storage_bytes) {
                                    None => {
                                        ui.small("Checking the Linux-native WSL model copy...");
                                    }
                                    Some(Ok(0)) => {
                                        ui.small("Linux-native WSL model copy: verified/present.");
                                    }
                                    Some(Ok(bytes)) => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.small(format!(
                                                "WSL2 needs an additional {} Linux-native copy of these same model bytes.",
                                                format_model_bytes(*bytes)
                                            ));
                                            if ui
                                                .add_enabled(
                                                    !setup_busy,
                                                    egui::Button::new("Approve copy"),
                                                )
                                                .clicked()
                                            {
                                                self.start_managed_setup(
                                                    ManagedSetupOperation::PrepareModel {
                                                        model_id: model.model_id.clone(),
                                                        environment: self.managed_execution_environment,
                                                        duplicate_storage_approved: true,
                                                    },
                                                    "Copying the exact verified GGUF bytes into the dedicated Linux-native WSL model store...",
                                                );
                                            }
                                        });
                                    }
                                    Some(Err(error)) => {
                                        ui.small(format!(
                                            "WSL model preparation unavailable: {error}"
                                        ));
                                    }
                                }
                            }
                        } else {
                            ui.small(
                                "No managed model selected. Model weights are never downloaded merely because a model is recommended; register an existing GGUF or use an explicit future catalog acquisition action.",
                            );
                        }
                    }
                    ModelBackendPreference::Local => {
                        ui.horizontal(|ui| {
                            ui.label("Endpoint");
                            if ui
                                .add(egui::TextEdit::singleline(&mut self.local_model_endpoint).desired_width(300.0))
                                .changed()
                            {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Installed models");
                            if ui
                                .add_enabled(
                                    self.model_discovery.is_none(),
                                    egui::Button::new("Discover"),
                                )
                                .clicked()
                            {
                                self.start_model_discovery();
                            }
                            if self.model_discovery.is_some() {
                                ui.spinner();
                            }
                        });
                        let inventory = self.current_installed_inventory().cloned();
                        if let Some(inventory) = inventory.as_ref() {
                            ui.horizontal(|ui| {
                                ui.label("Detected model");
                                egui::ComboBox::from_id_salt("ai_studio_installed_model")
                                    .selected_text(if self.local_model_name.trim().is_empty() {
                                        "Select discovered model"
                                    } else {
                                        self.local_model_name.trim()
                                    })
                                    .width(260.0)
                                    .show_ui(ui, |ui| {
                                        for model in &inventory.models {
                                            if ui
                                                .selectable_label(
                                                    self.local_model_name == model.name,
                                                    &model.name,
                                                )
                                                .clicked()
                                            {
                                                self.local_model_name = model.name.clone();
                                                self.last_model_resource_telemetry =
                                                    ModelResourceTelemetry::default();
                                                self.save_preferences();
                                            }
                                        }
                                    });
                                ui.small(format!("{} found", inventory.models.len()));
                            });
                        } else {
                            ui.small(
                                "No installed-model inventory is loaded. Discovery is explicit and loopback-only.",
                            );
                        }
                        ui.horizontal(|ui| {
                            ui.label("Custom / exact ID");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.local_model_name)
                                        .desired_width(260.0)
                                        .hint_text("model:tag"),
                                )
                                .changed()
                            {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        if let Some(inventory) = inventory.as_ref()
                            && let Some(model) = inventory
                                .models
                                .iter()
                                .find(|model| model.name == self.local_model_name)
                        {
                            ui.small(format!(
                                "Installed evidence: digest={} · size={} · parameters={} · quantization={} · family={} · backend={}",
                                optional_text(model.digest.as_deref()),
                                model
                                    .size_bytes
                                    .map(format_model_bytes)
                                    .unwrap_or_else(|| "n/a".to_owned()),
                                optional_text(model.parameter_size.as_deref()),
                                optional_text(model.quantization_level.as_deref()),
                                optional_text(model.family.as_deref()),
                                optional_text(inventory.backend_version.as_deref()),
                            ));
                        }
                    }
                    ModelBackendPreference::HostedApi | ModelBackendPreference::Enterprise => {
                        ui.horizontal(|ui| {
                            ui.label("HTTPS chat endpoint");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.hosted_model_endpoint)
                                        .desired_width(320.0)
                                        .hint_text("https://…/v1/chat/completions"),
                                )
                                .changed()
                            {
                                self.save_preferences();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Model");
                            if ui
                                .add(egui::TextEdit::singleline(&mut self.hosted_model_name).desired_width(260.0))
                                .changed()
                            {
                                self.save_preferences();
                            }
                        });
                        if self.settings_model_view == ModelBackendPreference::HostedApi {
                            ui.horizontal(|ui| {
                                ui.label("API credential");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.hosted_secret_draft)
                                        .password(true)
                                        .desired_width(220.0)
                                        .hint_text("stored with Windows DPAPI"),
                                );
                                if ui.button("Store securely").clicked() {
                                    match hosted_model_backend::store_api_key(
                                        &self.hosted_secret_path,
                                        &self.hosted_secret_draft,
                                    ) {
                                        Ok(()) => {
                                            self.hosted_secret_draft.clear();
                                            self.status = Some(
                                                "Hosted API credential stored in the machine-local OS-protected secret store.".to_owned(),
                                            );
                                        }
                                        Err(error) => self.status = Some(error.to_string()),
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    match hosted_model_backend::remove_api_key(&self.hosted_secret_path) {
                                        Ok(()) => {
                                            self.status = Some("Hosted API credential removed.".to_owned())
                                        }
                                        Err(error) => self.status = Some(error.to_string()),
                                    }
                                }
                            });
                            ui.small(if hosted_model_backend::credential_is_configured(
                                &self.hosted_secret_path,
                            ) {
                                "Credential status: configured. Secret value is never serialized or exposed to Remote AI Studio."
                            } else {
                                "Credential status: not configured."
                            });
                        } else {
                            ui.small(
                                "Enterprise authentication uses the organization-managed Windows identity/session; GameEngine stores no API key.",
                            );
                        }
                    }
                }
                let mut profile = match self.settings_model_view {
                    ModelBackendPreference::Local => NativeModelConfig::Local(LocalModelConfig {
                        endpoint: self.local_model_endpoint.clone(),
                        model: self.local_model_name.clone(),
                    })
                    .capability_profile(),
                    ModelBackendPreference::ManagedLocal => self
                        .described_managed_model_config()
                        .map(|config| NativeModelConfig::Managed(Box::new(config)))
                        .map(|config| config.capability_profile())
                        .unwrap_or_else(|_| {
                            NativeModelConfig::Managed(Box::new(ManagedLocalModelConfig {
                                state_root: self.managed_local_runtime.root().to_path_buf(),
                                environment: self.managed_execution_environment,
                                model_id: self.managed_model_id.clone(),
                                model_content_sha256: String::new(),
                                model_path: PathBuf::new(),
                                model_size_bytes: 0,
                                quantization: None,
                                model_representation: None,
                                capability: GgufModelCapability::default(),
                                projector_path: None,
                                runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
                                runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
                                runtime_artifact_sha256: String::new(),
                                runtime_compatibility_version: "llama-server-openai-v1".to_owned(),
                            }))
                            .capability_profile()
                        }),
                    ModelBackendPreference::HostedApi | ModelBackendPreference::Enterprise => {
                        NativeModelConfig::Hosted(HostedModelConfig {
                            endpoint: self.hosted_model_endpoint.clone(),
                            model: self.hosted_model_name.clone(),
                            auth_mode: if self.settings_model_view == ModelBackendPreference::HostedApi {
                                HostedAuthMode::ApiKey
                            } else {
                                HostedAuthMode::EnterpriseManaged
                            },
                            encrypted_secret_path: self.hosted_secret_path.clone(),
                        })
                        .capability_profile()
                    }
                };
                let recommendation_profiles = self
                    .model_catalog
                    .profiles_for_model(profile.backend_id, &profile.model_id);
                profile.benchmark_verified = !recommendation_profiles.is_empty();
                ui.small(model_capability_summary(&profile));
                if profile.model_id.trim().is_empty() {
                    ui.small("GameEngine status: no model selected.");
                } else if recommendation_profiles.is_empty() {
                    ui.small(
                        "GameEngine status: Compatible / unverified — this exact backend/model representation has no complete benchmark-qualified recommendation.",
                    );
                } else {
                    let labels = recommendation_profiles
                        .iter()
                        .map(|profile| profile.label())
                        .collect::<Vec<_>>()
                        .join(", " );
                    ui.small(format!(
                        "GameEngine status: Recommended · {labels} · corpus {BENCHMARK_CORPUS_VERSION}"
                    ));
                }
                if matches!(
                    self.settings_model_view,
                    ModelBackendPreference::Local | ModelBackendPreference::ManagedLocal
                ) {
                    ui.small(format!(
                        "Resource controls: unload/reload {} · CPU offload {} · GPU residency telemetry {} · memory telemetry {}",
                        capability_label(profile.resource_capabilities.unload_reload),
                        capability_label(profile.resource_capabilities.cpu_gpu_offload),
                        capability_label(profile.resource_capabilities.gpu_residency),
                        capability_label(profile.resource_capabilities.backend_memory_telemetry),
                    ));
                    ui.small(format!(
                        "Observed model resources: resident {} · model size {} · GPU residency {} · context {}",
                        telemetry_bool_label(&self.last_model_resource_telemetry.resident),
                        telemetry_bytes_label(
                            &self.last_model_resource_telemetry.representation_size_bytes
                        ),
                        telemetry_bytes_label(&self.last_model_resource_telemetry.gpu_residency_bytes),
                        telemetry_count_label(
                            &self.last_model_resource_telemetry.context_length_tokens,
                            "tokens"
                        ),
                    ));
                    ui.small(
                        "Provider-reported local model residency is shown with provenance; device-wide free VRAM and TTFT are never fabricated.",
                    );
                } else {
                    ui.small(
                        "Local GPU residency controls are unavailable for Hosted API and Enterprise backends; remote GPU state is not projected into local resource controls.",
                    );
                }
                ui.small(format!(
                    "Resource posture: {:?} · workload {:?} · reclaim {:?}",
                    self.resource_plan.presentation,
                    self.resolved_workload,
                    self.resource_plan.reclaim
                ));
                ui.small(self.model_routing_status());
            });
    }

    pub(super) fn show_agent_benchmark(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(format!(
            "GameEngine Agent Benchmark · {} record(s) · {}",
            self.benchmark_records.len(),
            self.model_catalog.catalog_version
        ))
        .default_open(cfg!(feature = "visual-validation"))
        .show(ui, |ui| {
            ui.small(format!(
                "Versioned corpus: {BENCHMARK_CORPUS_VERSION}. Recommendations require complete, comparable GameEngine task evidence; third-party scores alone never qualify a model."
            ));
            for catalog_profile in CatalogProfile::ALL {
                if let Some(recommendation) = self.model_catalog.recommendation(catalog_profile) {
                    ui.group(|ui| {
                        ui.strong(format!(
                            "{} · {}",
                            catalog_profile.label(),
                            recommendation.candidate.model_id
                        ));
                        ui.small(format!(
                            "evidence={} runs · aggregate={} ms · benchmark={}",
                            recommendation.evidence_runs,
                            recommendation.aggregate_elapsed_ms,
                            recommendation.benchmark_version
                        ));
                        ui.small(format!(
                            "source={} · license={} · transfer={} · storage={}",
                            recommendation.candidate.source,
                            recommendation.candidate.license,
                            format_model_bytes(recommendation.candidate.transfer_size_bytes),
                            format_model_bytes(recommendation.candidate.storage_size_bytes),
                        ));
                        ui.small(format!(
                            "memory={} · context={} · modalities={} · tools={}",
                            recommendation.candidate.memory_guidance,
                            recommendation
                                .candidate
                                .context_limit
                                .map(|limit| limit.to_string())
                                .unwrap_or_else(|| "n/a".to_owned()),
                            list_or_none(&recommendation.candidate.modalities),
                            list_or_none(&recommendation.candidate.tool_capabilities),
                        ));
                    });
                } else {
                    ui.small(format!(
                        "{}: No benchmark-qualified recommendation yet.",
                        catalog_profile.label()
                    ));
                }
            }
            ui.small(
                "Model weights are never bundled or downloaded automatically. A future catalog acquisition flow must show source plus transfer/storage size before an explicit user action.",
            );
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("Evidence task");
                let selected_label = benchmark_task(&self.benchmark_task_id)
                    .map(|task| task.label)
                    .unwrap_or("Unknown task");
                egui::ComboBox::from_id_salt("ai_studio_benchmark_task")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for task in BENCHMARK_TASKS {
                            ui.selectable_value(
                                &mut self.benchmark_task_id,
                                task.id.to_owned(),
                                task.label,
                            );
                        }
                    });
                if ui
                    .add_enabled(
                        self.benchmark_task_record_available(),
                        egui::Button::new("Record current evidence"),
                    )
                    .clicked()
                {
                    self.record_selected_benchmark();
                }
            });
            ui.small(
                "Choose the Evidence task before starting inference or a native run; its versioned identity is frozen at execution start. Record only when that result intentionally executes the frozen corpus task. Records are machine-local and omit prompts, conversation history, retrieved source text, project paths, and credentials; this feature never uploads private projects.",
            );
            ui.separator();
            self.show_benchmark_experiment(ui);
            ui.separator();
            self.show_benchmark_campaign(ui);
        });
    }

    /// Returns why a section needs the reader's attention, if it does.
    ///
    /// The navigation is the only part of the configuration tier that is always
    /// visible, so a section that cannot do its job says so there rather than
    /// waiting to be opened. Only states the user can act on are marked; a
    /// section that has simply never been configured is not a fault.
    fn section_attention(
        &self,
        section: SettingsSection,
    ) -> Option<(theme::StatusTone, &'static str)> {
        if section != SettingsSection::Agents {
            return None;
        }
        // ADR 0164 §1: an unfinished agent setup is only a fault for the user
        // who selected that agent. Someone running a local model is not asked
        // to fix something they are not using.
        let SelectedAi::Agent(kind) = self.selected_ai() else {
            return None;
        };
        section_attention_for(self.agent_readiness(kind))
    }

    /// Returns what is known about one agent, whether or not it is selected.
    ///
    /// ADR 0164 §3 lists every agent in one place, so readiness cannot be read
    /// from the single selected-provider status alone. A status produced by an
    /// explicit refresh wins over an older background probe report.
    pub(super) fn agent_status(
        &self,
        kind: ExternalAgentProviderKind,
    ) -> ExternalAgentProviderStatus {
        if kind == ExternalAgentProviderKind::Generic {
            return ExternalAgentProviderStatus::generic(!self.provider_program.trim().is_empty());
        }
        if self.external_provider_status.kind == kind
            && self.external_provider_status.discovery != ExternalAgentDiscoveryStatus::Unchecked
        {
            return self.external_provider_status.clone();
        }
        self.external_provider_report(kind)
            .map(|report| report.status.clone())
            .unwrap_or_else(|| ExternalAgentProviderStatus::unchecked(kind))
    }

    /// Returns what one agent can do right now.
    pub(super) fn agent_readiness(&self, kind: ExternalAgentProviderKind) -> ProviderReadiness {
        let working = self.external_provider_probe.is_some()
            || self
                .external_setup
                .as_ref()
                .is_some_and(|task| task.kind() == kind);
        provider_readiness(&self.agent_status(kind), kind, working)
    }

    /// Draws the agent programs this machine can run.
    ///
    /// ADR 0164 §3: an entry presents its name, one readiness state, and the
    /// one action that changes that state. Discovery detail, the capability
    /// matrix, and the resolved executable path are diagnosis, and are read
    /// only when the state above them is not the expected one.
    pub(super) fn show_agent_settings(&mut self, ui: &mut egui::Ui) {
        let mut refresh_requested = false;
        let mut setup_requested = None;
        let mut cancel_setup_requested = false;
        ui.horizontal(|ui| {
            if ui.button("Refresh status").clicked() {
                refresh_requested = true;
            }
            if self.external_provider_probe.is_some() {
                ui.spinner();
                ui.small("Checking which agents are installed and signed in…");
            }
        });
        for kind in ExternalAgentProviderKind::ALL
            .into_iter()
            .filter(|kind| kind.can_sign_in())
        {
            self.show_agent_card(ui, kind, &mut setup_requested, &mut cancel_setup_requested);
        }
        self.show_agent_advanced(ui);
        if refresh_requested {
            self.begin_external_provider_probe();
            self.refresh_external_provider_status();
        }
        if let Some((kind, action)) = setup_requested {
            self.begin_external_provider_setup(kind, action);
        }
        if cancel_setup_requested {
            self.cancel_external_provider_setup();
        }
        #[cfg(feature = "visual-validation")]
        self.show_external_provider_visual_evidence(ui);
    }

    /// Draws one agent: what it is, whether it is ready, and what to press.
    fn show_agent_card(
        &mut self,
        ui: &mut egui::Ui,
        kind: ExternalAgentProviderKind,
        setup_requested: &mut Option<(ExternalAgentProviderKind, ExternalAgentSetupAction)>,
        cancel_setup_requested: &mut bool,
    ) {
        let readiness = self.agent_readiness(kind);
        let label = kind.label();
        let setup_running = self
            .external_setup
            .as_ref()
            .filter(|task| task.kind() == kind)
            .map(ExternalAgentSetupTask::action);
        let installer_available = self
            .external_provider_report(kind)
            .is_none_or(|report| report.installer_available);
        theme::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(label);
                theme::status_pill(ui, readiness.tone(), readiness.label());
            });
            theme::hint(ui, readiness.next_step(label));
            match setup_running {
                Some(action) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.small(format!("{}…", action.progress_label()));
                        if ui.button("Cancel").clicked() {
                            *cancel_setup_requested = true;
                        }
                    });
                }
                None => {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                installer_available,
                                egui::Button::new(format!(
                                    "{} {label}",
                                    ExternalAgentSetupAction::Install.label()
                                )),
                            )
                            .clicked()
                        {
                            *setup_requested = Some((kind, ExternalAgentSetupAction::Install));
                        }
                        if ui
                            .button(format!(
                                "{} to {label}",
                                ExternalAgentSetupAction::SignIn.label()
                            ))
                            .clicked()
                        {
                            *setup_requested = Some((kind, ExternalAgentSetupAction::SignIn));
                        }
                    });
                    if !installer_available {
                        ui.label(
                            egui::RichText::new(
                                "npm was not found, so GameEngine cannot install this agent for you. Install Node.js, or install the agent's CLI yourself, then use Refresh status.",
                            )
                            .small()
                            .color(theme::WARNING),
                        );
                    }
                }
            }
            if let Some(url) = self
                .external_sign_in_url
                .clone()
                .filter(|_| setup_running.is_some() || self.external_provider_kind == kind)
            {
                ui.hyperlink_to("Open the sign-in page", &url);
                ui.small("Open this only if the agent could not open your browser by itself.");
            }
            if self.external_setup.as_ref().is_some_and(|task| {
                task.kind() == kind && task.action() == ExternalAgentSetupAction::SignIn
            }) {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.external_setup_input)
                            .hint_text("Confirmation or device code"),
                    );
                    if ui.button("Send to sign-in").clicked() {
                        self.submit_external_setup_input();
                    }
                });
                ui.small("Use this only when the provider asks for terminal input.");
            }
            self.show_agent_diagnostics(ui, kind);
        });
    }

    /// Draws the evidence behind one agent's reported state.
    ///
    /// ADR 0164 §3 keeps this collapsed. It answers "why does it say that",
    /// which is a question only asked when the state above is unexpected.
    fn show_agent_diagnostics(&mut self, ui: &mut egui::Ui, kind: ExternalAgentProviderKind) {
        let status = self.agent_status(kind);
        let capabilities = kind.capabilities();
        let placement = self.external_agent_placement();
        let report_locations = self
            .external_provider_report(kind)
            .map(|report| (report.locations.join("  ·  "), report.has_shadowed_copies()));
        let setup_log_belongs_here = self
            .external_setup
            .as_ref()
            .is_some_and(|task| task.kind() == kind);
        egui::CollapsingHeader::new("Diagnostics")
            .id_salt(("ai_studio_agent_diagnostics", kind.run_label()))
            .default_open(false)
            .show(ui, |ui| {
                let (discovery_value, discovery_tone) = discovery_presentation(status.discovery);
                theme::field_row_pill(ui, "Installed", discovery_tone, discovery_value);
                let (auth_value, auth_tone) = auth_presentation(status.auth);
                theme::field_row_pill(ui, "Signed in", auth_tone, auth_value);
                if let Some((locations, shadowed)) = report_locations {
                    if !locations.is_empty() {
                        theme::field_row(
                            ui,
                            "Resolved to",
                            egui::RichText::new(locations)
                                .small()
                                .monospace()
                                .color(theme::TEXT_MUTED),
                        );
                    }
                    if shadowed {
                        ui.label(
                            egui::RichText::new(
                                "More than one directory on PATH provides this program. The first one is the copy that runs, so an update installed elsewhere will not take effect until PATH is changed.",
                            )
                            .small()
                            .color(theme::WARNING),
                        );
                    }
                }
                for action in [
                    ExternalAgentSetupAction::Install,
                    ExternalAgentSetupAction::SignIn,
                ] {
                    if let Some(command) = setup_command_text(action, kind, &placement) {
                        theme::field_row(
                            ui,
                            action.label(),
                            egui::RichText::new(command)
                                .small()
                                .monospace()
                                .color(theme::TEXT_MUTED),
                        );
                    }
                }
                theme::caption(ui, "Adapter capabilities");
                theme::capability_chips(
                    ui,
                    &[
                        ("Provider sign-in", capabilities.provider_managed_auth),
                        ("MCP injection", capabilities.mcp_injection),
                        ("Structured events", capabilities.structured_events),
                        ("Host cancellation", capabilities.host_cancellation),
                    ],
                );
                theme::spec_note(
                    ui,
                    "Where the credential lives",
                    "Provider-managed login remains provider-owned. GameEngine stores no agent credential and reports only sanitized adapter status remotely.",
                );
                if setup_log_belongs_here && !self.external_setup_log.is_empty() {
                    theme::caption(ui, "Setup output");
                    for line in &self.external_setup_log {
                        theme::selectable_text(
                            ui,
                            egui::RichText::new(line)
                                .small()
                                .monospace()
                                .color(theme::TEXT_MUTED),
                        );
                    }
                }
            });
    }

    /// Draws the entries no reader reaches without already knowing them.
    ///
    /// ADR 0164 §3 puts the compatible-agent command here, because it is the
    /// escape hatch for an agent GameEngine does not adapt, and confinement
    /// here, because it is a policy decision made once per machine.
    fn show_agent_advanced(&mut self, ui: &mut egui::Ui) {
        let confinement_status = self
            .active_run_id
            .as_deref()
            .and_then(|run_id| self.host.run(run_id).ok())
            .and_then(|run| run.confinement_profile.as_ref())
            .map(|profile| profile.summary())
            .unwrap_or_else(|| {
                "No external process confinement profile has been recorded. Generic external launches are application-policy-only; the native AgentRuntime is not an external child-process sandbox."
                    .to_owned()
            });
        let previous_confinement_requirement = self.confinement_requirement;
        let mut confinement_changed = false;
        egui::CollapsingHeader::new("Advanced")
            .id_salt("ai_studio_agents_advanced")
            .default_open(false)
            .show(ui, |ui| {
                theme::card(ui, |ui| {
                    theme::card_header(ui, "Compatible agent program");
                    theme::hint(
                        ui,
                        "Runs an agent GameEngine does not adapt. It reports no structured events and owns its own authentication.",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Program");
                        ui.text_edit_singleline(&mut self.provider_program);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Arguments");
                        ui.text_edit_singleline(&mut self.provider_args);
                    });
                });
                theme::card(ui, |ui| {
                    theme::card_header(ui, "External process confinement");
                    egui::ComboBox::from_id_salt("ai_studio_process_confinement")
                        .selected_text(self.confinement_requirement.label())
                        .show_ui(ui, |ui| {
                            for requirement in [
                                AgentConfinementRequirement::AllowApplicationPolicyOnly,
                                AgentConfinementRequirement::RequireProviderOrOsConfinement,
                            ] {
                                ui.selectable_value(
                                    &mut self.confinement_requirement,
                                    requirement,
                                    requirement.label(),
                                );
                            }
                        });
                    ui.label(confinement_status);
                    theme::spec_note(
                        ui,
                        "What confinement guarantees",
                        "GameEngine application permissions remain authoritative. External agents are not treated as sandboxed unless their launch path reports enforceable provider/OS confinement.",
                    );
                    if self.confinement_requirement.requires_enforced_confinement() {
                        ui.small(
                            "Fail-closed policy: an external agent will not start through the generic process runtime unless a provider/OS confinement adapter can satisfy this requirement.",
                        );
                    }
                });
                if previous_confinement_requirement != self.confinement_requirement {
                    confinement_changed = true;
                }
            });
        if confinement_changed {
            self.save_preferences();
        }
    }

    /// Draws the fixture evidence the ADR 0145 visual validation captures.
    #[cfg(feature = "visual-validation")]
    fn show_external_provider_visual_evidence(&mut self, ui: &mut egui::Ui) {
        if !self.visual_external_provider_evidence {
            return;
        }
        ui.group(|ui| {
            ui.strong("First-class AgentRuntime provider evidence");
            for provider in ExternalAgentProviderKind::ALL {
                let status = ExternalAgentProviderStatus::visual_fixture(provider);
                ui.label(format!(
                    "{} · discovery {} · authentication {}",
                    provider.label(),
                    status.discovery.label(),
                    status.auth.label(),
                ));
            }
            ui.small(
                "Claude Code and Codex keep provider-owned credentials; Generic command remains the explicit compatibility fallback.",
            );
            ui.small(
                "MCP bearer: ephemeral environment reference only. Secret values are never displayed, serialized, or copied into provider arguments.",
            );
            ui.small(
                "Sanitized error presentation: provider failures report adapter/status context without credential or bearer contents.",
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader must be told one state, not two facts to combine themselves.
    ///
    /// Reported as a configuration surface where an installed-but-signed-out
    /// provider and a missing provider looked the same: two neutral sentences
    /// of the same weight, neither of which said what to do next.
    #[test]
    fn provider_readiness_names_one_state_and_one_next_step() {
        let claude = ExternalAgentProviderKind::ClaudeCode;
        let missing = ExternalAgentProviderStatus {
            kind: claude,
            discovery: ExternalAgentDiscoveryStatus::Unavailable,
            auth: ExternalAgentAuthStatus::Unavailable,
        };
        assert_eq!(
            provider_readiness(&missing, claude, false),
            ProviderReadiness::NotInstalled
        );
        assert_eq!(
            provider_readiness(&missing, claude, false).tone(),
            theme::StatusTone::Blocked
        );

        let signed_out = ExternalAgentProviderStatus {
            kind: claude,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: ExternalAgentAuthStatus::SignInRequired,
        };
        assert_eq!(
            provider_readiness(&signed_out, claude, false),
            ProviderReadiness::SignInRequired
        );
        assert!(
            provider_readiness(&signed_out, claude, false)
                .next_step(claude.label())
                .contains("Sign in")
        );

        let ready = ExternalAgentProviderStatus {
            kind: claude,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: ExternalAgentAuthStatus::Authenticated,
        };
        assert_eq!(
            provider_readiness(&ready, claude, false),
            ProviderReadiness::Ready
        );
        // A probe in flight describes the machine as it was, so a stale ready
        // state must not be reported as the current one.
        assert_eq!(
            provider_readiness(&ready, claude, true),
            ProviderReadiness::Working
        );

        // A generic command is configuration, not an installation, so the step
        // it asks for is entering one rather than installing anything.
        let generic = ExternalAgentProviderStatus::generic(false);
        assert_eq!(
            provider_readiness(&generic, ExternalAgentProviderKind::Generic, false),
            ProviderReadiness::NotConfigured
        );
    }

    /// A section that cannot do its job says so where the reader can see it.
    #[test]
    fn only_actionable_provider_states_mark_the_navigation() {
        for (readiness, marked) in [
            (ProviderReadiness::NotInstalled, true),
            (ProviderReadiness::SignInRequired, true),
            (ProviderReadiness::NotConfigured, true),
            (ProviderReadiness::Ready, false),
            (ProviderReadiness::Working, false),
            // Never having been checked is not a fault to report.
            (ProviderReadiness::NotChecked, false),
        ] {
            assert_eq!(
                section_attention_for(readiness).is_some(),
                marked,
                "{readiness:?} navigation marking"
            );
        }
    }
}
