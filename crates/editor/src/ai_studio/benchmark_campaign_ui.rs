//! AI Studio campaign launcher and progress matrix for ADR 0156.
//!
//! This panel composes a campaign, freezes it, reviews the exact **Download &
//! Run** candidate set, starts the frozen schedule, and reports progress as a
//! candidate-by-task matrix.
//!
//! Execution reuses the ADR 0142 process-isolated harness: the frozen plan is
//! lowered to a benchmark experiment specification, which the headless parent
//! executes one isolated Editor child at a time. The panel then admits each
//! on-disk result into the campaign state machine, which is what enforces
//! schedule order, retry policy, and identity matching.

use super::benchmark_experiment_ui::experiment_directory_name;
use super::*;
use crate::agent_benchmark_campaign::{
    CampaignCandidate, CampaignCandidateSource, CampaignComparisonClass,
    CampaignExecutionEnvironment, CampaignExecutionProfile, CampaignRepresentation,
    DEFAULT_CAMPAIGN_REPETITIONS,
};
use crate::benchmark_campaign::{
    CampaignBaselineReuse, CampaignEnvironmentProbe, CampaignEvidence, CampaignPlan,
    CampaignPolicy, CampaignRun, CampaignState,
};
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkRunOutcome, ENGINE_COMMIT_HEAD,
};
use crate::managed_local_runtime::{ManagedAcquisitionPlan, ManagedModelRegistration};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// A campaign whose schedule is being executed by the headless parent.
struct RunningCampaign {
    process: Child,
    experiment_root: PathBuf,
    admitted: BTreeSet<u64>,
    finished: bool,
}

/// Everything the campaign section needs between frames.
pub(super) struct BenchmarkCampaignPanel {
    campaign_id: String,
    comparison_class: CampaignComparisonClass,
    execution_profile: CampaignExecutionProfile,
    execution_environment: CampaignExecutionEnvironment,
    selected_models: BTreeSet<String>,
    selected_tasks: BTreeSet<String>,
    repetitions: u32,
    host_seed: u64,
    plan: Option<CampaignPlan>,
    prior_plan: Option<CampaignPlan>,
    acquisition: Option<ManagedAcquisitionPlan>,
    acquisition_approved: bool,
    run: Option<CampaignRun>,
    running: Option<RunningCampaign>,
    message: Option<String>,
}

impl Default for BenchmarkCampaignPanel {
    fn default() -> Self {
        Self {
            campaign_id: "local-model-campaign".to_owned(),
            comparison_class: CampaignComparisonClass::ModelComparison,
            execution_profile: CampaignExecutionProfile::Warm,
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            selected_models: BTreeSet::new(),
            selected_tasks: BENCHMARK_TASKS
                .iter()
                .map(|task| task.id.to_owned())
                .collect(),
            repetitions: DEFAULT_CAMPAIGN_REPETITIONS,
            host_seed: 1,
            plan: None,
            prior_plan: None,
            acquisition: None,
            acquisition_approved: false,
            run: None,
            running: None,
            message: None,
        }
    }
}

impl AiStudioPanel {
    #[cfg(feature = "visual-validation")]
    pub(super) fn prepare_managed_campaign_visual_validation(&mut self, model_id: &str) {
        self.benchmark_campaign.execution_environment = CampaignExecutionEnvironment::WindowsNative;
        self.benchmark_campaign.selected_models.clear();
        self.benchmark_campaign
            .selected_models
            .insert(model_id.to_owned());
    }

    /// Draws the campaign launcher, download review, and progress matrix.
    pub(super) fn show_benchmark_campaign(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Benchmark campaign")
            .default_open(cfg!(feature = "visual-validation"))
            .show(ui, |ui| {
                ui.small(
                    "Freezes an exact candidate set, per-task fixture identity, repetition policy, and one execution environment before the first measured run. Changing any of those starts a new campaign instead of extending this one.",
                );
                self.show_campaign_identity(ui);
                ui.separator();
                self.show_campaign_candidates(ui);
                ui.separator();
                self.show_campaign_tasks(ui);
                ui.separator();
                self.show_campaign_controls(ui);
                self.show_campaign_download_review(ui);
                self.show_campaign_progress_matrix(ui);
                self.show_campaign_report(ui);
                if let Some(message) = self.benchmark_campaign.message.clone() {
                    ui.small(message);
                }
            });
    }

    fn show_campaign_identity(&mut self, ui: &mut egui::Ui) {
        let frozen = self.benchmark_campaign.plan.is_some();
        ui.horizontal_wrapped(|ui| {
            ui.label("Campaign");
            ui.add_enabled(
                !frozen,
                egui::TextEdit::singleline(&mut self.benchmark_campaign.campaign_id)
                    .desired_width(220.0),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Comparison");
            for class in [
                CampaignComparisonClass::ModelComparison,
                CampaignComparisonClass::RuntimeCharacterization,
            ] {
                let selected = self.benchmark_campaign.comparison_class == class;
                if ui
                    .add_enabled(!frozen, egui::Button::selectable(selected, class.label()))
                    .clicked()
                {
                    self.benchmark_campaign.comparison_class = class;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Profile");
            for profile in [
                CampaignExecutionProfile::Warm,
                CampaignExecutionProfile::Cold,
            ] {
                let selected = self.benchmark_campaign.execution_profile == profile;
                if ui
                    .add_enabled(!frozen, egui::Button::selectable(selected, profile.label()))
                    .clicked()
                {
                    self.benchmark_campaign.execution_profile = profile;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Environment");
            for environment in [
                CampaignExecutionEnvironment::CompatibleBackend,
                CampaignExecutionEnvironment::WindowsNative,
                CampaignExecutionEnvironment::Wsl2Linux,
            ] {
                let selected = self.benchmark_campaign.execution_environment == environment;
                if ui
                    .add_enabled(
                        !frozen,
                        egui::Button::selectable(selected, environment.label()),
                    )
                    .clicked()
                {
                    self.benchmark_campaign.execution_environment = environment;
                }
            }
        });
        if self.benchmark_campaign.comparison_class
            == CampaignComparisonClass::RuntimeCharacterization
        {
            ui.small(
                "Runtime characterization measures the platform, not the model. These records are labelled accordingly and are never consumed as model-comparison evidence.",
            );
        }
        if ENGINE_COMMIT_HEAD.is_empty() {
            ui.small(
                "GameEngine commit: unavailable. This Editor was built without repository identity, so no comparable campaign can be frozen from it.",
            );
        }
    }

    fn show_campaign_candidates(&mut self, ui: &mut egui::Ui) {
        let frozen = self.benchmark_campaign.plan.is_some();
        ui.strong("Candidates");
        if let Some(environment) = self
            .benchmark_campaign
            .execution_environment
            .managed_environment()
        {
            let models = match self.managed_local_runtime.registered_models() {
                Ok(models) => models,
                Err(error) => {
                    ui.small(format!("Managed model registry unavailable: {error}"));
                    return;
                }
            };
            if models.is_empty() {
                ui.small("No managed GGUF models are registered. Register exact GGUF files in Local AI first.");
                return;
            }
            ui.small(format!(
                "{} campaigns run these registered GGUF representations through GameEngine-managed llama.cpp.",
                environment.label()
            ));
            for model in &models {
                let exact = managed_model_is_campaign_candidate(model);
                let mut selected = self
                    .benchmark_campaign
                    .selected_models
                    .contains(&model.model_id);
                if ui
                    .add_enabled(
                        !frozen && exact,
                        egui::Checkbox::new(
                            &mut selected,
                            format!("{} · {}", model.display_name, model.model_id),
                        ),
                    )
                    .changed()
                {
                    if selected {
                        self.benchmark_campaign
                            .selected_models
                            .insert(model.model_id.clone());
                    } else {
                        self.benchmark_campaign
                            .selected_models
                            .remove(&model.model_id);
                    }
                }
                if let Some(representation) = model.exact_representation() {
                    ui.small(format!("Representation: {representation}"));
                } else {
                    ui.small(format!(
                        "{} cannot be a candidate: its exact digest, GGUF-derived representation, or byte size could not be measured from the registered file. Re-register this GGUF if it moved or changed since it was registered.",
                        model.display_name
                    ));
                }
            }
            return;
        }

        let Some(inventory) = self.installed_model_inventory.clone() else {
            ui.small("No compatible local model inventory yet. Discover installed models first.");
            return;
        };
        for model in &inventory.models {
            let exact = model.digest.is_some()
                && model.quantization_level.is_some()
                && model.size_bytes.is_some_and(|bytes| bytes > 0);
            let mut selected = self
                .benchmark_campaign
                .selected_models
                .contains(&model.name);
            if ui
                .add_enabled(
                    !frozen && exact,
                    egui::Checkbox::new(&mut selected, &model.name),
                )
                .changed()
            {
                if selected {
                    self.benchmark_campaign
                        .selected_models
                        .insert(model.name.clone());
                } else {
                    self.benchmark_campaign.selected_models.remove(&model.name);
                }
            }
            if !exact {
                ui.small(format!(
                    "{} cannot be a candidate: its exact digest, quantization, or byte size is not measured, so its evidence could not be attributed to one representation.",
                    model.name
                ));
            }
        }
    }

    fn show_campaign_tasks(&mut self, ui: &mut egui::Ui) {
        let frozen = self.benchmark_campaign.plan.is_some();
        ui.strong("Tasks");
        for task in BENCHMARK_TASKS.iter() {
            let mut selected = self.benchmark_campaign.selected_tasks.contains(task.id);
            if ui
                .add_enabled(!frozen, egui::Checkbox::new(&mut selected, task.label))
                .changed()
            {
                if selected {
                    self.benchmark_campaign
                        .selected_tasks
                        .insert(task.id.to_owned());
                } else {
                    self.benchmark_campaign.selected_tasks.remove(task.id);
                }
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Repetitions");
            let mut repetitions = self.benchmark_campaign.repetitions;
            if ui
                .add_enabled(
                    !frozen,
                    egui::DragValue::new(&mut repetitions).range(1..=10),
                )
                .changed()
            {
                self.benchmark_campaign.repetitions = repetitions;
            }
            ui.label("Fixture seed");
            let mut seed = self.benchmark_campaign.host_seed;
            if ui
                .add_enabled(!frozen, egui::DragValue::new(&mut seed))
                .changed()
            {
                self.benchmark_campaign.host_seed = seed;
            }
        });
        ui.small(
            "The fixture seed is host-owned. It selects a frozen parameterized instance; its hidden evaluation state is never placed in candidate-visible context.",
        );
    }

    fn show_campaign_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if self.benchmark_campaign.plan.is_none() {
                if ui.button("Freeze campaign").clicked() {
                    self.freeze_campaign();
                }
                return;
            }
            let state = self
                .benchmark_campaign
                .run
                .as_ref()
                .map(CampaignRun::state)
                .unwrap_or(CampaignState::Planned);
            match state {
                CampaignState::Planned => {
                    let blocked = self.benchmark_campaign.acquisition.is_some()
                        && !self.benchmark_campaign.acquisition_approved;
                    if ui
                        .add_enabled(!blocked, egui::Button::new("Start campaign"))
                        .clicked()
                    {
                        self.start_campaign();
                    }
                    if ui.button("Discard plan").clicked() {
                        self.benchmark_campaign.prior_plan = self.benchmark_campaign.plan.take();
                        self.benchmark_campaign.run = None;
                        self.benchmark_campaign.acquisition = None;
                        self.benchmark_campaign.acquisition_approved = false;
                        self.benchmark_campaign.message = None;
                    }
                }
                CampaignState::Running => {
                    if ui.button("Pause").clicked() {
                        if let Some(run) = self.benchmark_campaign.run.as_mut() {
                            run.pause();
                        }
                        self.benchmark_campaign.message =
                            Some("Campaign paused. Recorded evidence is retained.".to_owned());
                    }
                }
                CampaignState::Paused => {
                    if ui.button("Resume").clicked() {
                        self.resume_campaign();
                    }
                }
                CampaignState::Completed => {
                    ui.small("Campaign complete.");
                }
            }
        });
        if let Some(plan) = self.benchmark_campaign.plan.as_ref() {
            let runtime = plan.runtime_identity();
            ui.small(format!(
                "Frozen plan {} · {} scheduled runs · schedule {} · runtime {} {}",
                plan.plan_digest(),
                plan.schedule().len(),
                plan.schedule_version,
                runtime.execution_environment.label(),
                runtime.backend_runtime_version
            ));
            if self
                .benchmark_campaign
                .run
                .as_ref()
                .is_some_and(CampaignRun::has_acquisition_approval)
            {
                ui.small("Download & Run approval is held for this campaign.");
            }
            if let Some(prior) = self.benchmark_campaign.prior_plan.as_ref() {
                match plan.baseline_reuse(prior) {
                    CampaignBaselineReuse::Reusable { model_ids } if !model_ids.is_empty() => {
                        ui.small(format!(
                            "Baseline reuse: {} already has strictly comparable evidence and need not run again.",
                            model_ids.join(", ")
                        ));
                    }
                    CampaignBaselineReuse::Reusable { .. } => {}
                    CampaignBaselineReuse::RequiresBaselineRerun { changed } => {
                        ui.small(format!(
                            "Baseline re-run required: {} changed since the previous campaign, so prior evidence is not comparable.",
                            changed.join(", ")
                        ));
                    }
                }
            }
        }
    }

    fn show_campaign_download_review(&mut self, ui: &mut egui::Ui) {
        let Some(acquisition) = self.benchmark_campaign.acquisition.clone() else {
            return;
        };
        let review = acquisition.review();
        ui.separator();
        ui.strong("Download & Run review");
        ui.small(format!(
            "{} model representation(s) are missing. Transfer {} bytes, store {} bytes.",
            review.candidate_count, review.total_transfer_bytes, review.total_storage_bytes
        ));
        for candidate in &acquisition.candidates {
            ui.small(format!(
                "{} · {} · sha256 {} · license {}",
                candidate.candidate_id,
                candidate.representation,
                candidate.expected_sha256,
                candidate
                    .license
                    .clone()
                    .unwrap_or_else(|| "unstated".to_owned())
            ));
        }
        ui.small(
            "Approval covers exactly these representations. Content is verified against the expected digest before any measured run.",
        );
        let mut approved = self.benchmark_campaign.acquisition_approved;
        if ui
            .checkbox(&mut approved, "Approve this exact candidate set")
            .changed()
        {
            self.benchmark_campaign.acquisition_approved = approved;
        }
    }

    fn show_campaign_progress_matrix(&mut self, ui: &mut egui::Ui) {
        let Some(run) = self.benchmark_campaign.run.as_ref() else {
            return;
        };
        let plan = run.plan();
        ui.separator();
        ui.strong("Progress");
        egui::Grid::new("ai_studio_campaign_matrix")
            .striped(true)
            .show(ui, |ui| {
                ui.label("Candidate");
                for task_plan in &plan.task_plans {
                    ui.label(task_plan.task_id.as_str());
                }
                ui.end_row();
                for candidate in &plan.candidates {
                    ui.label(candidate.representation.model_id.as_str());
                    for task_plan in &plan.task_plans {
                        let recorded = run
                            .evidence()
                            .iter()
                            .filter(|evidence| {
                                evidence.scheduled.model_id == candidate.representation.model_id
                                    && evidence.scheduled.task_id == task_plan.task_id
                            })
                            .count();
                        let passed = run
                            .evidence()
                            .iter()
                            .filter(|evidence| {
                                evidence.scheduled.model_id == candidate.representation.model_id
                                    && evidence.scheduled.task_id == task_plan.task_id
                                    && evidence.outcome == BenchmarkRunOutcome::Passed
                            })
                            .count();
                        ui.label(format!("{passed}/{recorded} of {}", plan.repetitions));
                    }
                    ui.end_row();
                }
            });
        if let Some(next) = run.next_scheduled() {
            ui.small(format!(
                "Next: {} · {} · repetition {}. One measured local run executes at a time.",
                next.model_id, next.task_id, next.repetition
            ));
        }
    }

    fn show_campaign_report(&mut self, ui: &mut egui::Ui) {
        let Some(run) = self.benchmark_campaign.run.as_ref() else {
            return;
        };
        let report = run.report();
        if report.models.is_empty() {
            return;
        }
        ui.separator();
        ui.strong("Report");
        for model in &report.models {
            ui.small(format!(
                "{} · task success {}/{} · measured failures {} · unavailable {}",
                model.model_id,
                model.task_successes,
                model.planned_runs,
                model.measured_failures,
                model.unavailable_runs
            ));
            match model.aggregate_elapsed_ms {
                Some(elapsed) => ui.small(format!("    aggregate elapsed {elapsed} ms")),
                None => ui.small(
                    "    aggregate elapsed: unavailable. At least one run did not report measured timing, so no total is shown.",
                ),
            };
        }
        if report.evidence_complete {
            ui.small(format!(
                "Evidence is complete; {} record(s) qualify for the curated catalog and ADR 0150 routing.",
                run.qualified_records().len()
            ));
        } else {
            ui.small(
                "Evidence is incomplete. No curated-catalog recommendation is derived from a partial campaign.",
            );
        }
    }

    fn freeze_campaign(&mut self) {
        match self
            .build_campaign_policy()
            .and_then(CampaignPolicy::freeze)
        {
            Ok(plan) => {
                let installed = self.installed_campaign_model_ids();
                self.benchmark_campaign.acquisition = plan.acquisition_plan(&installed);
                self.benchmark_campaign.acquisition_approved = false;
                self.benchmark_campaign.message = Some(format!(
                    "Campaign frozen as {}. Editing any policy field now starts a new campaign.",
                    plan.plan_digest()
                ));
                self.benchmark_campaign.run = Some(CampaignRun::prepare(plan.clone()));
                self.benchmark_campaign.plan = Some(plan);
            }
            Err(error) => {
                self.benchmark_campaign.message = Some(format!("Cannot freeze: {error}"));
            }
        }
    }

    fn build_campaign_policy(&self) -> Result<CampaignPolicy, String> {
        if ENGINE_COMMIT_HEAD.is_empty() {
            return Err("this Editor build carries no exact GameEngine commit identity".to_owned());
        }
        let (backend_runtime_version, candidates) = if let Some(environment) = self
            .benchmark_campaign
            .execution_environment
            .managed_environment()
        {
            let installation = self
                .managed_local_runtime
                .active_installation(environment)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!(
                        "the GameEngine-managed {} runtime is not installed",
                        environment.label()
                    )
                })?;
            let models = self
                .managed_local_runtime
                .registered_models()
                .map_err(|error| error.to_string())?;
            let mut candidates = Vec::new();
            for model in models {
                if !self
                    .benchmark_campaign
                    .selected_models
                    .contains(&model.model_id)
                {
                    continue;
                }
                let representation =
                    model
                        .exact_representation()
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            format!(
                                "model `{}` has no GGUF-derived exact representation",
                                model.display_name
                            )
                        })?;
                if model.size_bytes == 0 {
                    return Err(format!(
                        "model `{}` has no measured size",
                        model.display_name
                    ));
                }
                candidates.push(CampaignCandidate {
                    representation: CampaignRepresentation {
                        backend_id: MANAGED_BACKEND_ID.to_owned(),
                        model_id: model.model_id,
                        model_version: model.content_sha256,
                        quantization: representation,
                        representation_size_bytes: model.size_bytes,
                    },
                    source: CampaignCandidateSource::installed(),
                });
            }
            (installation.benchmark_runtime_identity(), candidates)
        } else {
            let inventory = self
                .installed_model_inventory
                .as_ref()
                .ok_or_else(|| "no compatible local model inventory is available".to_owned())?;
            let backend_runtime_version = inventory.backend_version.clone().ok_or_else(|| {
                "the compatible local backend did not report an exact runtime version".to_owned()
            })?;
            let mut candidates = Vec::new();
            for model in &inventory.models {
                if !self
                    .benchmark_campaign
                    .selected_models
                    .contains(&model.name)
                {
                    continue;
                }
                candidates.push(CampaignCandidate {
                    representation: CampaignRepresentation {
                        backend_id: "ollama-compatible".to_owned(),
                        model_id: model.name.clone(),
                        model_version: model.digest.clone().ok_or_else(|| {
                            format!("model `{}` has no measured digest", model.name)
                        })?,
                        quantization: model.quantization_level.clone().ok_or_else(|| {
                            format!("model `{}` has no measured quantization", model.name)
                        })?,
                        representation_size_bytes: model.size_bytes.ok_or_else(|| {
                            format!("model `{}` has no measured size", model.name)
                        })?,
                    },
                    source: CampaignCandidateSource::installed(),
                });
            }
            (backend_runtime_version, candidates)
        };
        Ok(CampaignPolicy {
            campaign_id: self.benchmark_campaign.campaign_id.trim().to_owned(),
            engine_commit_head: ENGINE_COMMIT_HEAD.to_owned(),
            comparison_class: self.benchmark_campaign.comparison_class,
            execution_profile: self.benchmark_campaign.execution_profile,
            execution_environment: self.benchmark_campaign.execution_environment,
            backend_runtime_version,
            candidates,
            task_ids: BENCHMARK_TASKS
                .iter()
                .filter(|task| self.benchmark_campaign.selected_tasks.contains(task.id))
                .map(|task| task.id.to_owned())
                .collect(),
            repetitions: self.benchmark_campaign.repetitions,
            host_seed: self.benchmark_campaign.host_seed,
        })
    }

    fn campaign_backend_runtime_version(&self) -> Result<String, String> {
        if let Some(environment) = self
            .benchmark_campaign
            .execution_environment
            .managed_environment()
        {
            return self
                .managed_local_runtime
                .active_installation(environment)
                .map_err(|error| error.to_string())?
                .map(|installation| installation.benchmark_runtime_identity())
                .ok_or_else(|| {
                    format!(
                        "the GameEngine-managed {} runtime is not installed",
                        environment.label()
                    )
                });
        }
        self.installed_model_inventory
            .as_ref()
            .and_then(|inventory| inventory.backend_version.clone())
            .ok_or_else(|| "the compatible local backend runtime version is unavailable".to_owned())
    }

    fn installed_campaign_model_ids(&self) -> BTreeSet<String> {
        if self
            .benchmark_campaign
            .execution_environment
            .managed_environment()
            .is_some()
        {
            return self
                .managed_local_runtime
                .registered_models()
                .map(|models| models.into_iter().map(|model| model.model_id).collect())
                .unwrap_or_default();
        }
        self.installed_model_inventory
            .as_ref()
            .map(|inventory| {
                inventory
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn start_campaign(&mut self) {
        let installed = self.installed_campaign_model_ids();
        let approval = self
            .benchmark_campaign
            .acquisition
            .as_ref()
            .filter(|_| self.benchmark_campaign.acquisition_approved)
            .map(ManagedAcquisitionPlan::approve_exact);
        let Some(run) = self.benchmark_campaign.run.as_mut() else {
            return;
        };
        if let Err(rejection) = run.start(approval, &installed) {
            self.benchmark_campaign.message =
                Some(format!("{} ({})", rejection.message(), rejection.code()));
            return;
        }
        match self.spawn_campaign_execution() {
            Ok(running) => {
                self.benchmark_campaign.running = Some(running);
                self.benchmark_campaign.message =
                    Some("Campaign started. Recording is now active.".to_owned());
            }
            Err(error) => {
                self.benchmark_campaign.message = Some(format!("Could not start: {error}"));
            }
        }
    }

    fn spawn_campaign_execution(&mut self) -> Result<RunningCampaign, String> {
        let plan = self
            .benchmark_campaign
            .plan
            .as_ref()
            .ok_or_else(|| "campaign is not frozen".to_owned())?;
        let spec = plan.experiment_spec(self.benchmark_experiment_root.clone())?;
        let experiment_root = self
            .benchmark_experiment_root
            .join(experiment_directory_name(&spec.experiment_id));
        fs::create_dir_all(&experiment_root).map_err(|error| error.to_string())?;
        let spec_path = experiment_root.join("requested-campaign.json");
        let bytes = serde_json::to_vec_pretty(&spec).map_err(|error| error.to_string())?;
        fs::write(&spec_path, bytes).map_err(|error| error.to_string())?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(executable);
        command.arg("--benchmark-experiment").arg(&spec_path);
        if spec.backend_id != MANAGED_BACKEND_ID {
            command
                .arg("--benchmark-endpoint")
                .arg(self.local_model_endpoint.trim());
        }
        let process = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start the campaign parent: {error}"))?;
        Ok(RunningCampaign {
            process,
            experiment_root,
            admitted: BTreeSet::new(),
            finished: false,
        })
    }

    fn resume_campaign(&mut self) {
        let probe = CampaignEnvironmentProbe {
            execution_environment: self.benchmark_campaign.execution_environment,
            backend_runtime_version: self.campaign_backend_runtime_version().unwrap_or_default(),
            engine_commit_head: ENGINE_COMMIT_HEAD.to_owned(),
        };
        let Some(run) = self.benchmark_campaign.run.as_mut() else {
            return;
        };
        match run.resume(&probe) {
            Ok(()) => {
                self.benchmark_campaign.message = Some("Campaign resumed.".to_owned());
            }
            Err(rejection) => {
                self.benchmark_campaign.message =
                    Some(format!("{} ({})", rejection.message(), rejection.code()));
            }
        }
    }

    /// Admits newly written run results into the campaign state machine.
    pub(super) fn poll_benchmark_campaign(&mut self) {
        let Some(running) = self.benchmark_campaign.running.as_mut() else {
            return;
        };
        if let Ok(Some(_)) = running.process.try_wait() {
            running.finished = true;
        }
        let runs_root = running.experiment_root.join("runs");
        let Ok(entries) = fs::read_dir(&runs_root) else {
            return;
        };
        let mut results: Vec<BenchmarkExperimentResult> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| read_campaign_result(&path).ok())
            .collect();
        results.sort_by_key(|result| result.run.ordinal);
        let Some(run) = self.benchmark_campaign.run.as_mut() else {
            return;
        };
        for result in results {
            if !running.admitted.insert(result.run.ordinal) {
                continue;
            }
            let Some(scheduled) = run.next_scheduled() else {
                break;
            };
            let evidence = CampaignEvidence {
                scheduled,
                outcome: result.outcome,
                failure_kind: result.failure_kind,
                attempt: 0,
                record: result.record,
            };
            if let Err(rejection) = run.record(evidence) {
                self.benchmark_campaign.message = Some(format!(
                    "Run {} was not admitted: {} ({})",
                    result.run.ordinal,
                    rejection.message(),
                    rejection.code()
                ));
                break;
            }
        }
    }
}

fn managed_model_is_campaign_candidate(model: &ManagedModelRegistration) -> bool {
    model.has_exact_representation_identity()
}

fn read_campaign_result(path: &Path) -> Result<BenchmarkExperimentResult, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_registration(representation: Option<&str>) -> ManagedModelRegistration {
        ManagedModelRegistration {
            model_id: "gguf:test".to_owned(),
            display_name: "qwen3.8-27b-abliterated-3.69bpw.gguf".to_owned(),
            content_sha256: "a".repeat(64),
            source_path: PathBuf::from("model.gguf"),
            size_bytes: 1024,
            modified_unix_ms: Some(1),
            quantization: None,
            representation: representation.map(str::to_owned),
            source: None,
            license: None,
        }
    }

    #[test]
    fn managed_candidate_eligibility_requires_exact_gguf_representation() {
        let legacy = managed_registration(None);
        assert!(!managed_model_is_campaign_candidate(&legacy));

        let exact = managed_registration(Some(
            "gguf-repr-v1;gguf=3;file_type=none;quantization_version=2;types=Q4_K:2,Q6_K:1",
        ));
        assert!(managed_model_is_campaign_candidate(&exact));
    }
}
