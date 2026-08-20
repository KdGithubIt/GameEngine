//! The AI Studio configuration tier.
//!
//! ADR 0162 §5 separates configuration from selection. Everything drawn here
//! changes what exists on this machine, what the project may reach, or what
//! credentials are stored; choosing among what is already configured belongs to
//! the composer and stays in the parent module. The sections are named so a
//! rare setup task never shares a scroll position with a frequent one.

use super::*;

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
    /// Choosing, authenticating, and confining agent providers.
    Providers,
    /// Where a provider runs.
    Environment,
    /// Characterizing models and runtimes.
    Benchmarks,
    /// Reaching this studio from another device.
    Remote,
}

impl SettingsSection {
    /// Every section, in the order the navigation lists them.
    pub(super) const ALL: [Self; 5] = [
        Self::Models,
        Self::Providers,
        Self::Environment,
        Self::Benchmarks,
        Self::Remote,
    ];

    /// Returns the navigation label.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Models => "Models",
            Self::Providers => "Providers",
            Self::Environment => "Environment",
            Self::Benchmarks => "Benchmarks",
            Self::Remote => "Remote",
        }
    }

    /// Returns what the section is for, as one line.
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Models => {
                "Register, install, remove, and resource the models this machine can run."
            }
            Self::Providers => {
                "Choose an agent provider, sign it in, and decide how tightly it is confined."
            }
            Self::Environment => "Decide where an external provider process runs.",
            Self::Benchmarks => "Characterize models and runtimes on reproducible tasks.",
            Self::Remote => "Reach this studio from another device on the private network.",
        }
    }
}

impl AiStudioPanel {
    pub(super) fn show_remote_companion(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.remote_server.as_ref() else {
            return;
        };
        egui::CollapsingHeader::new("Remote companion")
            .default_open(false)
            .show(ui, |ui| {
                ui.small("Loopback-only companion gateway. Expose it only through a trusted private overlay or local reverse proxy. Remote authentication is separate from Agent Host permissions; MCP is never exposed remotely.");
                theme::selectable_text(ui, format!("Gateway: {}", server.endpoint()));
                theme::selectable_text(ui, egui::RichText::new(server.companion_url()).monospace());
            });
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
                            if ui
                                .selectable_label(selected, section.label())
                                .on_hover_text(section.description())
                                .clicked()
                            {
                                self.settings_section = section;
                            }
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
                                    SettingsSection::Providers => self.show_provider_settings(ui),
                                    SettingsSection::Environment => {
                                        self.show_environment_settings(ui);
                                    }
                                    SettingsSection::Benchmarks => self.show_agent_benchmark(ui),
                                    SettingsSection::Remote => self.show_remote_companion(ui),
                                }
                            });
                    });
            });
        self.settings_open = open;
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
                    ui.label("Backend");
                    let previous = self.model_backend;
                    egui::ComboBox::from_id_salt("ai_studio_model_backend")
                        .selected_text(match self.model_backend {
                            ModelBackendPreference::Local => "External local (Ollama-compatible)",
                            ModelBackendPreference::ManagedLocal => "Managed Local AI",
                            ModelBackendPreference::HostedApi => "Hosted API",
                            ModelBackendPreference::Enterprise => "Enterprise",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::ManagedLocal,
                                "Managed Local AI",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::Local,
                                "External local (Ollama-compatible)",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::HostedApi,
                                "Hosted API",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::Enterprise,
                                "Enterprise",
                            );
                        });
                    if self.model_backend != previous {
                        self.save_preferences();
                    }
                });
                ui.small(match self.model_backend {
                    ModelBackendPreference::Local => "Processing posture: external loopback local runtime; existing Ollama-compatible settings retain their original meaning.",
                    ModelBackendPreference::ManagedLocal => "Processing posture: GameEngine-managed llama.cpp on this machine; the inference server remains loopback-only and never gains authoring authority.",
                    ModelBackendPreference::HostedApi => "Processing posture: selected task context is sent to the configured remote HTTPS provider only after Network access approval.",
                    ModelBackendPreference::Enterprise => "Processing posture: selected task context is sent to the configured enterprise HTTPS endpoint only after Network access approval.",
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Quality");
                    let previous = self.quality_preference;
                    for quality in QualityPreference::ALL {
                        ui.selectable_value(&mut self.quality_preference, quality, quality.label());
                    }
                    if self.quality_preference != previous {
                        self.save_preferences();
                    }
                });
                ui.small(
                    "Quality is a machine-local latency/reasoning preference. Remote GPU controls are never projected as local residency controls.",
                );
                match self.model_backend {
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
                        if self.model_backend == ModelBackendPreference::HostedApi {
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
                let mut profile = match self.model_backend {
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
                            auth_mode: if self.model_backend == ModelBackendPreference::HostedApi {
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
                    self.model_backend,
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

    /// Draws provider selection and confinement configuration.
    ///
    /// ADR 0158 keeps configuration out of the transcript column, so this is
    /// drawn by the settings surface rather than beside the conversation.
    pub(super) fn show_provider_settings(&mut self, ui: &mut egui::Ui) {
        let previous_provider = self.external_provider_kind;
        let mut refresh_provider = false;
        ui.horizontal(|ui| {
            ui.label("External agent provider");
            egui::ComboBox::from_id_salt("ai_studio_external_agent_provider")
                .selected_text(self.external_provider_kind.label())
                .show_ui(ui, |ui| {
                    for provider in ExternalAgentProviderKind::ALL {
                        ui.selectable_value(
                            &mut self.external_provider_kind,
                            provider,
                            provider.label(),
                        );
                    }
                });
            if ui.button("Refresh status").clicked() {
                refresh_provider = true;
            }
        });
        if previous_provider != self.external_provider_kind {
            self.external_provider_status =
                ExternalAgentProviderStatus::unchecked(self.external_provider_kind);
            self.save_preferences();
        }
        if refresh_provider {
            self.refresh_external_provider_status();
        }
        let provider_status = self.current_external_provider_status();
        let capabilities = self.external_provider_kind.capabilities();
        ui.group(|ui| {
            ui.strong(format!("{} status", self.external_provider_kind.label()));
            ui.label(format!(
                "Discovery: {} · Authentication: {}",
                provider_status.discovery.label(),
                provider_status.auth.label(),
            ));
            ui.small(format!(
                "Capabilities: provider auth {} · MCP injection {} · structured events {} · host cancellation {}",
                capabilities.provider_managed_auth,
                capabilities.mcp_injection,
                capabilities.structured_events,
                capabilities.host_cancellation,
            ));
            ui.small(
                "Provider-managed login remains provider-owned. GameEngine stores no provider credential and reports only sanitized adapter status remotely.",
            );
        });
        #[cfg(feature = "visual-validation")]
        if self.visual_external_provider_evidence {
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
        if self.external_provider_kind == ExternalAgentProviderKind::Generic {
            ui.horizontal(|ui| {
                ui.label("Compatible agent program");
                ui.text_edit_singleline(&mut self.provider_program);
            });
            ui.horizontal(|ui| {
                ui.label("Arguments");
                ui.text_edit_singleline(&mut self.provider_args);
            });
        }
        let previous_confinement_requirement = self.confinement_requirement;
        ui.horizontal(|ui| {
            ui.label("External process confinement");
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
        });
        if previous_confinement_requirement != self.confinement_requirement {
            self.save_preferences();
        }
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
        ui.group(|ui| {
            ui.strong("Confinement status");
            ui.label(confinement_status);
            theme::spec_note(
                ui,
                "What confinement guarantees",
                "GameEngine application permissions remain authoritative. External providers are not treated as sandboxed unless their launch path reports enforceable provider/OS confinement.",
            );
            if self.confinement_requirement.requires_enforced_confinement() {
                ui.small(
                    "Fail-closed policy: an external agent will not start through the generic process runtime unless a provider/OS confinement adapter can satisfy this requirement.",
                );
            }
        });
        theme::spec_note(
            ui,
            "How Build selects a runtime",
            "Build uses the selected first-class external provider when it is ready, the Generic command when configured, or otherwise the selected Managed Local, external local, Hosted API, or Enterprise ModelBackend. External and managed adapters remain clients of the same immutable proposal, Agent Host permissions and work claims, code workspace, validation, Play/frame evidence, and completion contract.",
        );
    }
}
