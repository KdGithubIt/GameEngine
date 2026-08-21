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
//! schedule order, failure retention, and identity matching.

use super::benchmark_child::unix_ms;
use super::benchmark_experiment_ui::experiment_directory_name;
use super::*;
use crate::agent_benchmark::{BenchmarkLane, BenchmarkRuntimeIdentity};
use crate::agent_benchmark_campaign::{
    CampaignCandidate, CampaignCandidateSource, CampaignComparisonClass,
    CampaignExecutionEnvironment, CampaignExecutionProfile, CampaignRepresentation,
    DEFAULT_CAMPAIGN_REPETITIONS, campaign_task_agent_inclusive_runtime_support,
};
use crate::benchmark_campaign::{
    CampaignBaselineReuse, CampaignEnvironmentProbe, CampaignEvidence, CampaignPlan,
    CampaignPolicy, CampaignRun, CampaignScheduledRun, CampaignState,
};
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkRunOutcome, ENGINE_COMMIT_HEAD,
};
use crate::benchmark_runner::DEFAULT_BENCHMARK_RUN_TIMEOUT;
use crate::managed_local_runtime::{
    ManagedAcquisitionPlan, ManagedIntegrityCheck, ManagedModelRegistration,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const CAMPAIGN_LIVE_LOG_REFRESH_MS: u64 = 750;
const CAMPAIGN_LIVE_LOG_LINES_PER_SOURCE: usize = 8;
const CAMPAIGN_LIVE_LOG_TAIL_BYTES: u64 = 64 * 1024;

struct CampaignBackendLogTask {
    receiver: std::sync::mpsc::Receiver<Result<Vec<String>, String>>,
}

impl CampaignBackendLogTask {
    fn spawn(runtime: ManagedLocalRuntime, environment: ManagedExecutionEnvironment) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = runtime
                .recent_server_log_lines(environment, CAMPAIGN_LIVE_LOG_LINES_PER_SOURCE)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        Self { receiver }
    }

    fn poll(&self) -> Option<Result<Vec<String>, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("managed backend log reader disconnected".to_owned()))
            }
        }
    }
}

#[derive(Default)]
struct CampaignLiveActivity {
    last_refresh_unix_ms: u64,
    last_activity_unix_ms: Option<u64>,
    initialized: bool,
    child_lines: Vec<String>,
    backend_lines: Vec<String>,
    backend_task: Option<CampaignBackendLogTask>,
    error: Option<String>,
}

impl CampaignLiveActivity {
    fn update(
        &mut self,
        now_unix_ms: u64,
        child_lines: Vec<String>,
        backend_lines: Vec<String>,
        error: Option<String>,
    ) {
        let changed = if self.initialized {
            self.child_lines != child_lines || self.backend_lines != backend_lines
        } else {
            !child_lines.is_empty()
        };
        self.initialized = true;
        self.last_refresh_unix_ms = now_unix_ms;
        if changed {
            self.last_activity_unix_ms = Some(now_unix_ms);
        }
        self.child_lines = child_lines;
        self.backend_lines = backend_lines;
        self.error = error;
    }
}

/// A campaign whose schedule is being executed by the headless parent.
struct RunningCampaign {
    process: Child,
    experiment_root: PathBuf,
    pause_file: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    /// When this execution started, so results left by an earlier execution of
    /// the same frozen plan are never admitted as this one's evidence.
    started_unix_ms: u64,
    admitted: BTreeSet<u64>,
    result_errors: BTreeSet<PathBuf>,
    pause_requested: bool,
    exit_status: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CampaignCheckpoint {
    plan: CampaignPlan,
    run: CampaignRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CampaignHarnessSelection {
    GooseAcpAgentHarness,
    #[default]
    LegacyNativeHarness,
}

impl CampaignHarnessSelection {
    const ALL: [Self; 2] = [Self::GooseAcpAgentHarness, Self::LegacyNativeHarness];

    const fn label(self) -> &'static str {
        match self {
            Self::GooseAcpAgentHarness => "Goose ACP Agent Harness (recommended)",
            Self::LegacyNativeHarness => "Legacy Native Harness",
        }
    }

    const fn is_goose_acp(self) -> bool {
        matches!(self, Self::GooseAcpAgentHarness)
    }

    fn recommended_for_environment(environment: CampaignExecutionEnvironment) -> Self {
        if environment.managed_environment().is_some() {
            Self::GooseAcpAgentHarness
        } else {
            Self::LegacyNativeHarness
        }
    }

    fn from_plan(plan: &CampaignPlan) -> Self {
        if plan.benchmark_runtime().is_some() {
            Self::GooseAcpAgentHarness
        } else {
            Self::LegacyNativeHarness
        }
    }
}

/// Everything the campaign section needs between frames.
pub(super) struct BenchmarkCampaignPanel {
    campaign_id: String,
    comparison_class: CampaignComparisonClass,
    execution_profile: CampaignExecutionProfile,
    execution_environment: CampaignExecutionEnvironment,
    harness_selection: CampaignHarnessSelection,
    quality: QualityPreference,
    selected_models: BTreeSet<String>,
    selected_tasks: BTreeSet<String>,
    repetitions: u32,
    host_seed: u64,
    run_timeout_seconds: u64,
    plan: Option<CampaignPlan>,
    prior_plan: Option<CampaignPlan>,
    acquisition: Option<ManagedAcquisitionPlan>,
    acquisition_approved: bool,
    run: Option<CampaignRun>,
    running: Option<RunningCampaign>,
    live_activity: CampaignLiveActivity,
    message: Option<String>,
}

impl Default for BenchmarkCampaignPanel {
    fn default() -> Self {
        Self {
            campaign_id: "local-model-campaign".to_owned(),
            comparison_class: CampaignComparisonClass::ModelComparison,
            execution_profile: CampaignExecutionProfile::Warm,
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            harness_selection: CampaignHarnessSelection::recommended_for_environment(
                CampaignExecutionEnvironment::CompatibleBackend,
            ),
            quality: QualityPreference::Balanced,
            selected_models: BTreeSet::new(),
            selected_tasks: BENCHMARK_TASKS
                .iter()
                .map(|task| task.id.to_owned())
                .collect(),
            repetitions: DEFAULT_CAMPAIGN_REPETITIONS,
            host_seed: 1,
            run_timeout_seconds: DEFAULT_BENCHMARK_RUN_TIMEOUT.as_secs(),
            plan: None,
            prior_plan: None,
            acquisition: None,
            acquisition_approved: false,
            run: None,
            running: None,
            live_activity: CampaignLiveActivity::default(),
            message: None,
        }
    }
}

impl BenchmarkCampaignPanel {
    fn set_execution_environment(&mut self, environment: CampaignExecutionEnvironment) {
        let was_managed = self.execution_environment.managed_environment().is_some();
        let is_managed = environment.managed_environment().is_some();
        self.execution_environment = environment;
        if was_managed != is_managed {
            self.set_harness_selection(CampaignHarnessSelection::recommended_for_environment(
                environment,
            ));
        }
    }

    fn set_harness_selection(&mut self, selection: CampaignHarnessSelection) {
        self.harness_selection = selection;
        if !selection.is_goose_acp() {
            return;
        }
        let before = self.selected_tasks.len();
        self.selected_tasks
            .retain(|task_id| campaign_task_agent_inclusive_runtime_support(task_id).is_ok());
        if self.selected_tasks.len() != before {
            self.message = Some(
                "Goose ACP Agent Harness does not currently support Read question or Visual evaluation benchmark evidence, so those tasks were removed before freeze."
                    .to_owned(),
            );
        }
    }

    fn uses_goose_acp(&self) -> bool {
        self.execution_environment.managed_environment().is_some()
            && self.harness_selection.is_goose_acp()
    }

    fn settings_editable(&self) -> bool {
        self.plan.is_none()
    }

    fn control_state(&self) -> Option<CampaignState> {
        self.plan.as_ref()?;
        Some(
            self.run
                .as_ref()
                .map(CampaignRun::state)
                .unwrap_or(CampaignState::Planned),
        )
    }

    fn reset_for_new_campaign(&mut self, checkpoint_root: &Path) {
        self.prior_plan = self.plan.take();
        self.run = None;
        self.running = None;
        self.live_activity = CampaignLiveActivity::default();
        self.acquisition = None;
        self.acquisition_approved = false;
        self.message = None;

        let path = campaign_checkpoint_path(checkpoint_root);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                self.message = Some(format!("Could not remove campaign checkpoint: {error}"));
            }
        }
    }

    pub(super) fn load_checkpoint(root: &Path) -> Self {
        let path = campaign_checkpoint_path(root);
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        let Ok(mut checkpoint) = serde_json::from_slice::<CampaignCheckpoint>(&bytes) else {
            return Self::default();
        };
        if checkpoint.run.plan() != &checkpoint.plan {
            return Self {
                message: Some(
                    "The machine-local campaign checkpoint contains inconsistent plans and was not restored."
                        .to_owned(),
                ),
                ..Self::default()
            };
        }
        if checkpoint.run.state() == CampaignState::Running {
            let experiment_id = format!(
                "{}-{}",
                checkpoint.plan.campaign_id,
                checkpoint.plan.plan_digest()
            );
            let pause_file = root
                .join(experiment_directory_name(&experiment_id))
                .join("pause.requested");
            let _ = fs::write(pause_file, b"pause after Editor restart\n");
            checkpoint.run.pause();
        }
        let selected_models = checkpoint
            .plan
            .candidates
            .iter()
            .map(|candidate| candidate.representation.model_id.clone())
            .collect();
        let selected_tasks = checkpoint
            .plan
            .task_plans
            .iter()
            .map(|task| task.task_id.clone())
            .collect();
        Self {
            campaign_id: checkpoint.plan.campaign_id.clone(),
            comparison_class: checkpoint.plan.comparison_class,
            execution_profile: checkpoint.plan.execution_profile,
            execution_environment: checkpoint.plan.execution_environment,
            harness_selection: CampaignHarnessSelection::from_plan(&checkpoint.plan),
            quality: checkpoint.plan.quality,
            selected_models,
            selected_tasks,
            repetitions: checkpoint.plan.repetitions,
            run_timeout_seconds: checkpoint.plan.run_timeout_seconds,
            plan: Some(checkpoint.plan),
            run: Some(checkpoint.run),
            message: Some(
                "Restored a machine-local campaign checkpoint. Resume revalidates hardware and runtime identity before execution."
                    .to_owned(),
            ),
            ..Self::default()
        }
    }
}

fn campaign_checkpoint_path(root: &Path) -> PathBuf {
    root.join("campaign-checkpoint.json")
}

impl AiStudioPanel {
    #[cfg(feature = "visual-validation")]
    pub(super) fn prepare_managed_campaign_visual_validation(&mut self, model_id: &str) {
        self.benchmark_campaign.execution_environment = CampaignExecutionEnvironment::WindowsNative;
        self.benchmark_campaign.harness_selection = CampaignHarnessSelection::GooseAcpAgentHarness;
        self.benchmark_campaign
            .selected_tasks
            .retain(|task_id| campaign_task_agent_inclusive_runtime_support(task_id).is_ok());
        self.benchmark_campaign.selected_models.clear();
        self.benchmark_campaign
            .selected_models
            .insert(model_id.to_owned());
    }

    #[cfg(feature = "visual-validation")]
    pub fn prepare_benchmark_campaign_completed_visual_validation(&mut self) -> Result<(), String> {
        self.prepare_managed_local_visual_validation()?;
        self.settings_open = true;
        self.settings_section = SettingsSection::Benchmarks;

        let model = self
            .managed_local_runtime
            .registered_models()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|model| {
                self.benchmark_campaign
                    .selected_models
                    .contains(&model.model_id)
            })
            .ok_or_else(|| {
                "Benchmark campaign visual fixture has no registered model.".to_owned()
            })?;
        let representation = model
            .exact_representation()
            .map(str::to_owned)
            .ok_or_else(|| {
                "Benchmark campaign visual fixture has no exact GGUF identity.".to_owned()
            })?;
        let model_id = model.model_id.clone();

        self.benchmark_campaign.campaign_id = "managed-local-completed".to_owned();
        self.benchmark_campaign.comparison_class = CampaignComparisonClass::ModelComparison;
        self.benchmark_campaign.execution_profile = CampaignExecutionProfile::Warm;
        self.benchmark_campaign.execution_environment = CampaignExecutionEnvironment::WindowsNative;
        self.benchmark_campaign.harness_selection = CampaignHarnessSelection::GooseAcpAgentHarness;
        self.benchmark_campaign.quality = QualityPreference::Balanced;
        self.benchmark_campaign.selected_models = BTreeSet::from([model_id.clone()]);
        self.benchmark_campaign.selected_tasks =
            BTreeSet::from(["validation_repair_v1".to_owned()]);
        self.benchmark_campaign.repetitions = 1;
        self.benchmark_campaign.host_seed = 7;
        self.benchmark_campaign.run_timeout_seconds = 600;
        self.benchmark_campaign.prior_plan = None;
        self.benchmark_campaign.acquisition = None;
        self.benchmark_campaign.acquisition_approved = false;
        self.benchmark_campaign.running = None;
        self.benchmark_campaign.message = None;

        let acp_identity = crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
            GOOSE_ACP_AGENT_NAME,
            Some("visual-validation".to_owned()),
        );
        let plan = CampaignPolicy {
            campaign_id: self.benchmark_campaign.campaign_id.clone(),
            engine_commit_head: if ENGINE_COMMIT_HEAD.is_empty() {
                "0123456789abcdef0123456789abcdef01234567".to_owned()
            } else {
                ENGINE_COMMIT_HEAD.to_owned()
            },
            comparison_class: self.benchmark_campaign.comparison_class,
            execution_profile: self.benchmark_campaign.execution_profile,
            execution_environment: self.benchmark_campaign.execution_environment,
            backend_runtime_version: "llama.cpp-visual-validation".to_owned(),
            hardware: self.benchmark_hardware.clone(),
            quality: self.benchmark_campaign.quality,
            run_timeout_seconds: self.benchmark_campaign.run_timeout_seconds,
            candidates: vec![CampaignCandidate {
                representation: CampaignRepresentation {
                    backend_id: MANAGED_BACKEND_ID.to_owned(),
                    model_id: model_id.clone(),
                    model_version: model.content_sha256,
                    quantization: representation,
                    representation_size_bytes: model.size_bytes,
                },
                source: CampaignCandidateSource::installed(),
            }],
            task_ids: vec!["validation_repair_v1".to_owned()],
            benchmark_runtime: Some(BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
                &acp_identity,
            )),
            repetitions: 1,
            host_seed: self.benchmark_campaign.host_seed,
        }
        .freeze()?;

        let mut run = CampaignRun::prepare(plan.clone());
        run.start(None, &BTreeSet::from([model_id]))
            .map_err(|rejection| format!("{} ({})", rejection.message(), rejection.code()))?;
        let scheduled = run
            .next_scheduled()
            .ok_or_else(|| "Benchmark campaign visual fixture has no scheduled run.".to_owned())?;
        run.record(CampaignEvidence {
            scheduled,
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: None,
            attempt: 0,
            record: None,
        })
        .map_err(|rejection| format!("{} ({})", rejection.message(), rejection.code()))?;
        if run.state() != CampaignState::Completed {
            return Err("Benchmark campaign visual fixture did not reach Completed.".to_owned());
        }

        let fixture_root = std::env::temp_dir().join(format!(
            "gameengine-benchmark-campaign-visual-{}-{}",
            std::process::id(),
            unix_ms()
        ));
        fs::create_dir_all(&fixture_root).map_err(|error| error.to_string())?;
        self.benchmark_experiment_root = fixture_root;
        self.benchmark_campaign.plan = Some(plan);
        self.benchmark_campaign.run = Some(run);
        Ok(())
    }

    #[cfg(feature = "visual-validation")]
    pub fn prepare_benchmark_campaign_running_visual_validation(&mut self) -> Result<(), String> {
        self.prepare_benchmark_campaign_completed_visual_validation()?;
        let plan =
            self.benchmark_campaign.plan.clone().ok_or_else(|| {
                "Benchmark campaign visual fixture lost its frozen plan.".to_owned()
            })?;
        let installed = plan
            .candidates
            .iter()
            .map(|candidate| candidate.representation.model_id.clone())
            .collect::<BTreeSet<_>>();
        let mut run = CampaignRun::prepare(plan);
        run.start(None, &installed)
            .map_err(|rejection| format!("{} ({})", rejection.message(), rejection.code()))?;
        let now_unix_ms = unix_ms();
        self.benchmark_campaign.run = Some(run);
        self.benchmark_campaign.live_activity = CampaignLiveActivity {
            last_refresh_unix_ms: now_unix_ms,
            last_activity_unix_ms: Some(now_unix_ms.saturating_sub(700)),
            initialized: true,
            child_lines: vec![
                "stderr | [benchmark.acp] session metadata updated".to_owned(),
                "stderr | [benchmark.acp] tool call is InProgress".to_owned(),
            ],
            backend_lines: vec![
                "srv update_slots: id 0 | new prompt, n_ctx_slot = 32768".to_owned(),
                "srv update_slots: prompt processing in progress, n_past = 6144".to_owned(),
            ],
            backend_task: None,
            error: None,
        };
        self.benchmark_campaign.message =
            Some("Campaign started. Recording is now active.".to_owned());
        Ok(())
    }

    #[cfg(feature = "visual-validation")]
    pub fn prepare_benchmark_campaign_reset_visual_validation(&mut self) -> Result<(), String> {
        self.prepare_benchmark_campaign_completed_visual_validation()?;
        self.discard_campaign_plan();
        if let Some(message) = self.benchmark_campaign.message.clone() {
            return Err(message);
        }
        Ok(())
    }

    /// Draws the campaign launcher, download review, and progress matrix.
    pub(super) fn show_benchmark_campaign(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Benchmark campaign")
            .default_open(cfg!(feature = "visual-validation"))
            .show(ui, |ui| {
                #[cfg(feature = "visual-validation")]
                if std::env::var("GAMEENGINE_VISUAL_AUTHORING_TOOL")
                    .ok()
                    .is_some_and(|scenario| {
                        matches!(
                            scenario.as_str(),
                            "ADR 0156 Benchmark Completed"
                                | "ADR 0156 Benchmark Running"
                                | "ADR 0156 Benchmark Reset"
                        )
                    })
                {
                    ui.scroll_to_cursor(Some(egui::Align::TOP));
                }
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
                if let Some(message) = self.benchmark_campaign.message.clone() {
                    ui.small(message);
                }
                self.show_campaign_download_review(ui);
                self.show_campaign_progress_matrix(ui);
                self.show_campaign_live_activity(ui);
                self.show_campaign_report(ui);
            });
    }

    fn show_campaign_identity(&mut self, ui: &mut egui::Ui) {
        let frozen = !self.benchmark_campaign.settings_editable();
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
            ui.label("Quality");
            for quality in [
                QualityPreference::Fast,
                QualityPreference::Balanced,
                QualityPreference::Deep,
            ] {
                let selected = self.benchmark_campaign.quality == quality;
                if ui
                    .add_enabled(!frozen, egui::Button::selectable(selected, quality.label()))
                    .clicked()
                {
                    self.benchmark_campaign.quality = quality;
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
                    self.benchmark_campaign
                        .set_execution_environment(environment);
                }
            }
        });
        let managed = self
            .benchmark_campaign
            .execution_environment
            .managed_environment()
            .is_some();
        ui.horizontal_wrapped(|ui| {
            ui.label("Runtime / Harness");
            for harness in CampaignHarnessSelection::ALL {
                let selected = self.benchmark_campaign.harness_selection == harness;
                let enabled = !frozen
                    && (managed || harness == CampaignHarnessSelection::LegacyNativeHarness);
                if ui
                    .add_enabled(enabled, egui::Button::selectable(selected, harness.label()))
                    .clicked()
                {
                    self.benchmark_campaign.set_harness_selection(harness);
                }
            }
        });
        if managed && self.benchmark_campaign.harness_selection.is_goose_acp() {
            ui.small(
                "Goose ACP Agent Harness is the recommended Managed Local path. Its runtime identity is discovered and frozen before the campaign can start; failures never fall back to Legacy Native.",
            );
        } else if !managed {
            ui.small(
                "Compatible backend campaigns use the Legacy Native Harness. Goose ACP Agent Harness becomes selectable with a Managed Local execution environment.",
            );
        }
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
        let frozen = !self.benchmark_campaign.settings_editable();
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
        let frozen = !self.benchmark_campaign.settings_editable();
        ui.strong("Tasks");
        let goose_acp = self.benchmark_campaign.uses_goose_acp();
        for task in BENCHMARK_TASKS.iter() {
            let unsupported = goose_acp
                .then(|| campaign_task_agent_inclusive_runtime_support(task.id).err())
                .flatten();
            let mut selected = self.benchmark_campaign.selected_tasks.contains(task.id);
            if ui
                .add_enabled(
                    !frozen && unsupported.is_none(),
                    egui::Checkbox::new(&mut selected, task.label),
                )
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
            if let Some(reason) = unsupported {
                ui.small(format!(
                    "{} unavailable with Goose ACP: {reason}",
                    task.label
                ));
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
            ui.label("Run timeout (seconds)");
            let mut timeout = self.benchmark_campaign.run_timeout_seconds;
            if ui
                .add_enabled(
                    !frozen,
                    egui::DragValue::new(&mut timeout).range(60..=7_200),
                )
                .changed()
            {
                self.benchmark_campaign.run_timeout_seconds = timeout;
            }
        });
        ui.small(
            "The fixture seed is host-owned. It selects a frozen parameterized instance; its hidden evaluation state is never placed in candidate-visible context.",
        );
    }

    fn show_campaign_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let Some(state) = self.benchmark_campaign.control_state() else {
                if ui.button("Freeze campaign").clicked() {
                    self.freeze_campaign();
                }
                return;
            };
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
                        self.discard_campaign_plan();
                    }
                }
                CampaignState::Running => {
                    if ui.button("Pause").clicked() {
                        self.request_campaign_pause();
                    }
                }
                CampaignState::Paused => {
                    if ui.button("Resume").clicked() {
                        self.resume_campaign();
                    }
                    if ui.button("Start new campaign").clicked() {
                        self.discard_campaign_plan();
                    }
                }
                CampaignState::Completed => {
                    ui.small("Campaign complete.");
                    if ui.button("Start new campaign").clicked() {
                        self.discard_campaign_plan();
                    }
                }
            }
        });
        if let Some(plan) = self.benchmark_campaign.plan.as_ref() {
            let runtime = plan.runtime_identity();
            let (lane, harness, agent_runtime) = frozen_harness_identity(plan.benchmark_runtime());
            let models = plan
                .candidates
                .iter()
                .map(|candidate| candidate.representation.model_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            ui.small(format!(
                "Frozen plan {} · {} scheduled runs · schedule {} · quality {} · timeout {}s",
                plan.plan_digest(),
                plan.schedule().len(),
                plan.schedule_version,
                plan.quality.label(),
                plan.run_timeout_seconds,
            ));
            ui.small(format!(
                "Lane: {lane} · Harness: {harness} · Agent/runtime: {agent_runtime}"
            ));
            ui.small(format!(
                "Model(s): {models} · Execution environment: {} · Backend runtime: {}",
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

    fn discard_campaign_plan(&mut self) {
        self.benchmark_campaign
            .reset_for_new_campaign(&self.benchmark_experiment_root);
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

    fn show_campaign_live_activity(&self, ui: &mut egui::Ui) {
        let Some(run) = self.benchmark_campaign.run.as_ref() else {
            return;
        };
        if run.state() != CampaignState::Running {
            return;
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(
                CAMPAIGN_LIVE_LOG_REFRESH_MS,
            ));
        let activity = &self.benchmark_campaign.live_activity;
        ui.separator();
        ui.strong("Live activity");
        if let Some(next) = run.next_scheduled() {
            ui.small(format!(
                "Run {} · {} · {} · repetition {}",
                next.ordinal, next.model_id, next.task_id, next.repetition
            ));
        }
        match activity.last_activity_unix_ms {
            Some(timestamp) => {
                let age_ms = unix_ms().saturating_sub(timestamp);
                ui.small(format!(
                    "Last observed activity: Unix {timestamp} ms · {:.1}s ago",
                    age_ms as f64 / 1_000.0
                ));
            }
            None => {
                ui.small("Waiting for the first new log line from this run.");
            }
        }
        if let Some(error) = activity.error.as_deref() {
            ui.small(format!("Live log read warning: {error}"));
        }

        let child_label = if run.plan().benchmark_runtime().is_some() {
            "Benchmark child / Goose ACP progress"
        } else {
            "Benchmark child"
        };
        let backend_environment = run.plan().execution_environment.managed_environment();
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if activity.child_lines.is_empty() && activity.backend_lines.is_empty() {
                    ui.monospace("No log lines observed yet.");
                    return;
                }
                if !activity.child_lines.is_empty() {
                    ui.small(child_label);
                    for line in &activity.child_lines {
                        ui.monospace(line);
                    }
                }
                if !activity.backend_lines.is_empty() {
                    if !activity.child_lines.is_empty() {
                        ui.add_space(4.0);
                    }
                    if let Some(environment) = backend_environment {
                        ui.small(format!("Managed llama-server · {}", environment.label()));
                    } else {
                        ui.small("Managed llama-server");
                    }
                    for line in &activity.backend_lines {
                        ui.monospace(line);
                    }
                }
            });
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
                self.save_campaign_checkpoint();
            }
            Err(error) => {
                self.benchmark_campaign.message = Some(format!("Cannot freeze: {error}"));
            }
        }
    }

    fn selected_campaign_benchmark_runtime(
        &self,
        candidates: &[CampaignCandidate],
    ) -> Result<Option<BenchmarkRuntimeIdentity>, String> {
        match self.benchmark_campaign.harness_selection {
            CampaignHarnessSelection::LegacyNativeHarness => Ok(None),
            CampaignHarnessSelection::GooseAcpAgentHarness => {
                let environment = self
                    .benchmark_campaign
                    .execution_environment
                    .managed_environment()
                    .ok_or_else(|| {
                        "Goose ACP Agent Harness requires a Managed Local execution environment; select Legacy Native for compatible-backend campaigns"
                            .to_owned()
                    })?;
                let first = candidates.first().ok_or_else(|| {
                    "Goose ACP Agent Harness requires at least one exact Managed Local model candidate"
                        .to_owned()
                })?;
                let first_config = self
                    .managed_local_runtime
                    .configuration_for(
                        &first.representation.model_id,
                        environment,
                        ManagedIntegrityCheck::Skipped,
                    )
                    .map_err(|error| {
                        format!(
                            "Managed Local model `{}` is unavailable for Goose ACP: {error}",
                            first.representation.model_id
                        )
                    })?;
                for candidate in &candidates[1..] {
                    self.managed_local_runtime
                        .configuration_for(
                            &candidate.representation.model_id,
                            environment,
                            ManagedIntegrityCheck::Skipped,
                        )
                        .map_err(|error| {
                            format!(
                                "Managed Local model `{}` is unavailable for Goose ACP: {error}",
                                candidate.representation.model_id
                            )
                        })?;
                }
                let config =
                    GooseLocalAcpConfig::new(first_config).map_err(|error| error.to_string())?;
                let goose = GooseLocalAcpRuntime::discover(config)
                    .map_err(|error| format!("Goose ACP Agent Harness unavailable: {error}"))?;
                Ok(Some(
                    BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
                        &goose.runtime_identity().acp,
                    ),
                ))
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
        let benchmark_runtime = self.selected_campaign_benchmark_runtime(&candidates)?;
        Ok(CampaignPolicy {
            campaign_id: self.benchmark_campaign.campaign_id.trim().to_owned(),
            engine_commit_head: ENGINE_COMMIT_HEAD.to_owned(),
            comparison_class: self.benchmark_campaign.comparison_class,
            execution_profile: self.benchmark_campaign.execution_profile,
            execution_environment: self.benchmark_campaign.execution_environment,
            backend_runtime_version,
            hardware: self.benchmark_hardware.clone(),
            quality: self.benchmark_campaign.quality,
            run_timeout_seconds: self.benchmark_campaign.run_timeout_seconds,
            candidates,
            task_ids: BENCHMARK_TASKS
                .iter()
                .filter(|task| self.benchmark_campaign.selected_tasks.contains(task.id))
                .map(|task| task.id.to_owned())
                .collect(),
            benchmark_runtime,
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
        match self.spawn_campaign_execution(false) {
            Ok(running) => {
                self.benchmark_campaign.live_activity = CampaignLiveActivity::default();
                self.benchmark_campaign.running = Some(running);
                self.benchmark_campaign.message =
                    Some("Campaign started. Recording is now active.".to_owned());
                self.save_campaign_checkpoint();
            }
            Err(error) => {
                if let Some(run) = self.benchmark_campaign.run.as_mut() {
                    run.pause();
                }
                self.benchmark_campaign.message = Some(format!("Could not start: {error}"));
                self.save_campaign_checkpoint();
            }
        }
    }

    fn spawn_campaign_execution(&mut self, resumed: bool) -> Result<RunningCampaign, String> {
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
        let pause_file = experiment_root.join("pause.requested");
        match fs::remove_file(&pause_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let stdout_path = experiment_root.join("campaign-parent-stdout.log");
        let stderr_path = experiment_root.join("campaign-parent-stderr.log");
        let stdout = File::create(&stdout_path).map_err(|error| error.to_string())?;
        let stderr = File::create(&stderr_path).map_err(|error| error.to_string())?;
        let mut command = Command::new(executable);
        command
            .arg("--benchmark-experiment")
            .arg(&spec_path)
            .arg("--benchmark-run-timeout")
            .arg(plan.run_timeout_seconds.to_string())
            .arg("--benchmark-pause-file")
            .arg(&pause_file);
        if resumed {
            let next_ordinal = self
                .benchmark_campaign
                .run
                .as_ref()
                .map(CampaignRun::next_ordinal)
                .ok_or_else(|| "campaign run is unavailable".to_owned())?;
            command
                .arg("--benchmark-resume-from")
                .arg(next_ordinal.to_string());
        }
        if spec.backend_id != MANAGED_BACKEND_ID {
            command
                .arg("--benchmark-endpoint")
                .arg(self.local_model_endpoint.trim());
        }
        let started_unix_ms = unix_ms();
        let process = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| format!("could not start the campaign parent: {error}"))?;
        Ok(RunningCampaign {
            process,
            experiment_root,
            pause_file,
            stdout_path,
            stderr_path,
            started_unix_ms,
            admitted: self
                .benchmark_campaign
                .run
                .as_ref()
                .map(|run| {
                    run.evidence()
                        .iter()
                        .map(|evidence| evidence.scheduled.ordinal)
                        .collect()
                })
                .unwrap_or_default(),
            result_errors: BTreeSet::new(),
            pause_requested: false,
            exit_status: None,
        })
    }

    fn request_campaign_pause(&mut self) {
        let Some(running) = self.benchmark_campaign.running.as_mut() else {
            if let Some(run) = self.benchmark_campaign.run.as_mut() {
                run.pause();
            }
            self.benchmark_campaign.message =
                Some("Campaign paused. Recorded evidence is retained.".to_owned());
            self.save_campaign_checkpoint();
            return;
        };
        match fs::write(&running.pause_file, b"pause\n") {
            Ok(()) => {
                running.pause_requested = true;
                self.benchmark_campaign.message = Some(
                    "Pausing campaign: stopping the active benchmark child at a safe process boundary."
                        .to_owned(),
                );
            }
            Err(error) => {
                self.benchmark_campaign.message =
                    Some(format!("Could not request campaign pause: {error}"));
            }
        }
    }

    fn resume_campaign(&mut self) {
        let Some(frozen_engine_commit) = self
            .benchmark_campaign
            .plan
            .as_ref()
            .map(|plan| plan.engine_commit_head.clone())
        else {
            return;
        };
        if engine_commit_changed(&frozen_engine_commit, ENGINE_COMMIT_HEAD) {
            self.benchmark_campaign.message = Some(format!(
                "Cannot resume this frozen campaign: it was created with GameEngine {frozen_engine_commit}, but this Editor was built from {ENGINE_COMMIT_HEAD}. Completed evidence is retained; choose Start new campaign to continue with this Editor."
            ));
            return;
        }

        let backend_runtime_version = match self.campaign_backend_runtime_version() {
            Ok(version) => version,
            Err(error) => {
                self.benchmark_campaign.message =
                    Some(format!("Cannot revalidate campaign runtime: {error}"));
                return;
            }
        };
        let representations = match self.build_campaign_policy() {
            Ok(policy) => policy
                .candidates
                .into_iter()
                .map(|candidate| candidate.representation)
                .collect(),
            Err(error) => {
                self.benchmark_campaign.message =
                    Some(format!("Cannot revalidate campaign models: {error}"));
                return;
            }
        };
        let probe = CampaignEnvironmentProbe {
            execution_environment: self.benchmark_campaign.execution_environment,
            backend_runtime_version,
            engine_commit_head: ENGINE_COMMIT_HEAD.to_owned(),
            hardware: self.benchmark_hardware.clone(),
            representations,
        };
        let Some(run) = self.benchmark_campaign.run.as_mut() else {
            return;
        };
        match run.resume(&probe) {
            Ok(()) => match self.spawn_campaign_execution(true) {
                Ok(running) => {
                    self.benchmark_campaign.live_activity = CampaignLiveActivity::default();
                    self.benchmark_campaign.running = Some(running);
                    self.benchmark_campaign.message = Some(
                        "Campaign resumed from its persisted completed-run prefix.".to_owned(),
                    );
                    self.save_campaign_checkpoint();
                }
                Err(error) => {
                    if let Some(run) = self.benchmark_campaign.run.as_mut() {
                        run.pause();
                    }
                    self.benchmark_campaign.message =
                        Some(format!("Could not resume campaign execution: {error}"));
                }
            },
            Err(rejection) => {
                self.benchmark_campaign.message =
                    Some(format!("{} ({})", rejection.message(), rejection.code()));
            }
        }
    }

    fn refresh_campaign_live_activity(&mut self) {
        let now_unix_ms = unix_ms();
        let last_refresh = self.benchmark_campaign.live_activity.last_refresh_unix_ms;
        if last_refresh != 0
            && now_unix_ms.saturating_sub(last_refresh) < CAMPAIGN_LIVE_LOG_REFRESH_MS
        {
            return;
        }
        let Some((experiment_root, ordinal, backend_id, environment)) = (|| {
            let running = self.benchmark_campaign.running.as_ref()?;
            let run = self.benchmark_campaign.run.as_ref()?;
            let next = run.next_scheduled()?;
            let backend_id = run
                .plan()
                .candidates
                .iter()
                .find(|candidate| candidate.representation.model_id == next.model_id)
                .map(|candidate| candidate.representation.backend_id.clone())?;
            Some((
                running.experiment_root.clone(),
                next.ordinal,
                backend_id,
                run.plan().execution_environment.managed_environment(),
            ))
        })() else {
            return;
        };

        let child_root = experiment_root.join("child");
        let mut child_lines = Vec::new();
        let mut errors = Vec::new();
        for (stream, path) in [
            (
                "stdout",
                child_root.join(format!("run-{ordinal:04}-stdout.log")),
            ),
            (
                "stderr",
                child_root.join(format!("run-{ordinal:04}-stderr.log")),
            ),
        ] {
            match read_recent_campaign_log_lines(&path, CAMPAIGN_LIVE_LOG_LINES_PER_SOURCE) {
                Ok(lines) => {
                    child_lines.extend(lines.into_iter().map(|line| format!("{stream} | {line}")))
                }
                Err(error) => errors.push(error),
            }
        }

        let mut backend_lines = self.benchmark_campaign.live_activity.backend_lines.clone();
        if backend_id == MANAGED_BACKEND_ID {
            match environment {
                Some(environment) => {
                    let completed = self
                        .benchmark_campaign
                        .live_activity
                        .backend_task
                        .as_ref()
                        .and_then(CampaignBackendLogTask::poll);
                    if let Some(result) = completed {
                        self.benchmark_campaign.live_activity.backend_task = None;
                        match result {
                            Ok(lines) => backend_lines = lines,
                            Err(error) => errors.push(error),
                        }
                    }
                    if self.benchmark_campaign.live_activity.backend_task.is_none() {
                        self.benchmark_campaign.live_activity.backend_task =
                            Some(CampaignBackendLogTask::spawn(
                                self.managed_local_runtime.clone(),
                                environment,
                            ));
                    }
                }
                None => {
                    self.benchmark_campaign.live_activity.backend_task = None;
                    backend_lines.clear();
                    errors.push(
                        "managed campaign has no frozen execution environment for live logs"
                            .to_owned(),
                    );
                }
            }
        } else {
            self.benchmark_campaign.live_activity.backend_task = None;
            backend_lines.clear();
        }
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        self.benchmark_campaign.live_activity.update(
            now_unix_ms,
            child_lines,
            backend_lines,
            error,
        );
    }

    /// Admits newly written run results into the campaign state machine.
    pub(super) fn poll_benchmark_campaign(&mut self) {
        self.refresh_campaign_live_activity();
        let Some(running) = self.benchmark_campaign.running.as_mut() else {
            return;
        };
        if running.exit_status.is_none() {
            match running.process.try_wait() {
                Ok(Some(status)) => running.exit_status = Some(status.to_string()),
                Ok(None) => {}
                Err(error) => {
                    running.exit_status = Some(format!("status unavailable: {error}"));
                }
            }
        }
        let runs_root = running.experiment_root.join("runs");
        let execution_started_unix_ms = running.started_unix_ms;
        let mut results = Vec::new();
        let mut newest_error = None;
        if let Ok(entries) = fs::read_dir(&runs_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    continue;
                }
                match read_campaign_result(&path) {
                    Ok(result)
                        if result_belongs_to_execution(&result, execution_started_unix_ms) =>
                    {
                        results.push(result);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if running.result_errors.insert(path.clone()) {
                            newest_error = Some(format!(
                                "Campaign result `{}` is invalid and was not admitted: {error}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }
        results.sort_by_key(|result| result.run.ordinal);
        let Some(run) = self.benchmark_campaign.run.as_mut() else {
            return;
        };
        let mut checkpoint_changed = false;
        for result in results {
            if running.admitted.contains(&result.run.ordinal) {
                continue;
            }
            let scheduled = CampaignScheduledRun {
                ordinal: result.run.ordinal,
                model_id: result.run.model_id,
                task_id: result.run.task_id,
                repetition: result.run.repetition,
            };
            let ordinal = scheduled.ordinal;
            let evidence = CampaignEvidence {
                scheduled,
                outcome: result.outcome,
                failure_kind: result.failure_kind,
                attempt: 0,
                record: result.record,
            };
            match run.record(evidence) {
                Ok(()) => {
                    running.admitted.insert(ordinal);
                    checkpoint_changed = true;
                }
                Err(rejection) => {
                    newest_error = Some(format!(
                        "Run {ordinal} was not admitted: {} ({})",
                        rejection.message(),
                        rejection.code()
                    ));
                    break;
                }
            }
        }
        if let Some(error) = newest_error {
            self.benchmark_campaign.message = Some(error);
        }

        let exit = running.exit_status.clone();
        let pause_requested = running.pause_requested;
        let stdout_path = running.stdout_path.clone();
        let stderr_path = running.stderr_path.clone();
        if let Some(exit) = exit {
            let completed = run.state() == CampaignState::Completed;
            if !completed {
                run.pause();
                checkpoint_changed = true;
                self.benchmark_campaign.message = Some(if pause_requested {
                    "Campaign paused. The active inference process stopped; completed evidence is retained and the interrupted ordinal will be rerun on Resume."
                        .to_owned()
                } else {
                    format!(
                        "Campaign parent exited ({exit}) before completion. The campaign is paused and may be resumed. Diagnostics: `{}` and `{}`.",
                        stdout_path.display(),
                        stderr_path.display()
                    )
                });
            }
            self.benchmark_campaign.running = None;
        }
        if checkpoint_changed {
            self.save_campaign_checkpoint();
        }
    }

    fn save_campaign_checkpoint(&mut self) {
        let (Some(plan), Some(run)) = (
            self.benchmark_campaign.plan.as_ref(),
            self.benchmark_campaign.run.as_ref(),
        ) else {
            return;
        };
        let checkpoint = CampaignCheckpoint {
            plan: plan.clone(),
            run: run.clone(),
        };
        let path = campaign_checkpoint_path(&self.benchmark_experiment_root);
        let temporary = path.with_extension("tmp");
        let result = serde_json::to_vec_pretty(&checkpoint)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
                if path.exists() {
                    fs::remove_file(&path).map_err(|error| error.to_string())?;
                }
                fs::rename(&temporary, &path).map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            self.benchmark_campaign.message =
                Some(format!("Could not persist campaign checkpoint: {error}"));
        }
    }
}

fn engine_commit_changed(frozen_engine_commit: &str, current_engine_commit: &str) -> bool {
    !current_engine_commit.is_empty() && frozen_engine_commit != current_engine_commit
}

fn managed_model_is_campaign_candidate(model: &ManagedModelRegistration) -> bool {
    model.has_exact_representation_identity()
}

fn frozen_harness_identity(runtime: Option<&BenchmarkRuntimeIdentity>) -> (String, String, String) {
    let Some(runtime) = runtime else {
        return (
            "Legacy Native".to_owned(),
            "Legacy Native Harness".to_owned(),
            "GameEngine Native Agent Runtime".to_owned(),
        );
    };
    let lane = match runtime.lane {
        BenchmarkLane::RawModel => "Raw Model",
        BenchmarkLane::AgentHarness => "Agent Harness",
        BenchmarkLane::CodingAgent => "Coding Agent",
    }
    .to_owned();
    let harness = if runtime.lane == BenchmarkLane::AgentHarness {
        "Goose ACP Agent Harness".to_owned()
    } else {
        runtime.harness.harness_id.clone()
    };
    let agent = runtime
        .agent_runtime
        .as_ref()
        .map(|agent| match &agent.runtime_version {
            TelemetryValue::Measured(version) => {
                format!("{} {}", agent.runtime_id, version)
            }
            TelemetryValue::ConservativeEstimate(version) => {
                format!("{} {} (estimated)", agent.runtime_id, version)
            }
            TelemetryValue::Unavailable => {
                format!("{} · version unavailable", agent.runtime_id)
            }
        })
        .unwrap_or_else(|| "none".to_owned());
    (lane, harness, agent)
}

/// Reports whether one on-disk run result was produced by this execution.
///
/// A frozen plan always resolves to the same experiment directory, so starting
/// the same campaign again finds the previous execution's results still on disk
/// until each run is overwritten. Admitting those would report an earlier
/// attempt's outcome as this campaign's evidence.
fn result_belongs_to_execution(
    result: &BenchmarkExperimentResult,
    execution_started_unix_ms: u64,
) -> bool {
    result.finished_unix_ms >= execution_started_unix_ms
}

fn read_recent_campaign_log_lines(path: &Path, max_lines: usize) -> Result<Vec<String>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!("could not open `{}`: {error}", path.display()));
        }
    };
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect `{}`: {error}", path.display()))?
        .len();
    let start = length.saturating_sub(CAMPAIGN_LIVE_LOG_TAIL_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start))
            .map_err(|error| format!("could not seek `{}`: {error}", path.display()))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
    if start > 0
        && let Some(newline) = bytes.iter().position(|byte| *byte == b'\n')
    {
        bytes.drain(..=newline);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.reverse();
    Ok(lines)
}

fn read_campaign_result(path: &Path) -> Result<BenchmarkExperimentResult, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark_experiment::{BenchmarkPlannedRun, BenchmarkRoutingMode};

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
            capability: Default::default(),
            projector: None,
            source: None,
            license: None,
        }
    }

    #[test]
    fn rebuilt_editor_commit_is_detected_before_campaign_resume() {
        let frozen = "a".repeat(40);
        let current = "b".repeat(40);

        assert!(engine_commit_changed(&frozen, &current));
        assert!(!engine_commit_changed(&frozen, &frozen));
        assert!(!engine_commit_changed(&frozen, ""));
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

    #[test]
    fn managed_local_environment_recommends_goose_acp_and_removes_unsupported_tasks() {
        let mut panel = BenchmarkCampaignPanel::default();
        assert_eq!(
            panel.harness_selection,
            CampaignHarnessSelection::LegacyNativeHarness
        );
        panel.set_execution_environment(CampaignExecutionEnvironment::WindowsNative);
        assert_eq!(
            panel.harness_selection,
            CampaignHarnessSelection::GooseAcpAgentHarness
        );
        assert!(!panel.selected_tasks.contains("read_question_v1"));
        assert!(!panel.selected_tasks.contains("visual_evaluation_v1"));
        assert!(panel.selected_tasks.contains("validation_repair_v1"));
    }

    #[test]
    fn explicit_legacy_native_selection_remains_available_for_managed_local() {
        let mut panel = BenchmarkCampaignPanel::default();
        panel.set_execution_environment(CampaignExecutionEnvironment::WindowsNative);
        panel.set_harness_selection(CampaignHarnessSelection::LegacyNativeHarness);
        assert!(!panel.uses_goose_acp());
        panel.selected_tasks.insert("read_question_v1".to_owned());
        assert!(panel.selected_tasks.contains("read_question_v1"));
    }

    #[test]
    fn live_activity_tracks_new_run_output_without_reclassifying_stale_backend_tail() {
        let mut activity = CampaignLiveActivity::default();
        activity.update(
            100,
            Vec::new(),
            vec!["previous server line".to_owned()],
            None,
        );
        assert_eq!(activity.last_activity_unix_ms, None);

        let child = vec!["stderr | goose ACP progress".to_owned()];
        activity.update(
            200,
            child.clone(),
            vec!["previous server line".to_owned()],
            None,
        );
        assert_eq!(activity.last_activity_unix_ms, Some(200));

        activity.update(300, child.clone(), vec!["new server line".to_owned()], None);
        assert_eq!(activity.last_activity_unix_ms, Some(300));
        activity.update(400, child, vec!["new server line".to_owned()], None);
        assert_eq!(activity.last_activity_unix_ms, Some(300));
    }

    #[test]
    fn live_log_tail_reads_only_the_newest_lines() {
        let root = campaign_reset_test_root("live-log-tail");
        fs::create_dir_all(&root).expect("log root should be created");
        let path = root.join("run-0000-stderr.log");
        fs::write(&path, b"one\ntwo\nthree\nfour\n").expect("log should be written");
        assert_eq!(
            read_recent_campaign_log_lines(&path, 2).expect("log tail should be readable"),
            vec!["three".to_owned(), "four".to_owned()]
        );
        let _ = fs::remove_dir_all(root);
    }

    fn campaign_result(finished_unix_ms: u64) -> BenchmarkExperimentResult {
        BenchmarkExperimentResult {
            experiment_id: "local-model-campaign-test".to_owned(),
            engine_commit_head: "a".repeat(40),
            fixture_version: "gameengine-agent-fixture-v1".to_owned(),
            routing_mode: BenchmarkRoutingMode::SingleModel,
            run: BenchmarkPlannedRun {
                ordinal: 0,
                model_id: "gguf:test".to_owned(),
                task_id: "project_inspection_v1".to_owned(),
                repetition: 0,
            },
            started_unix_ms: finished_unix_ms.saturating_sub(1_000),
            finished_unix_ms,
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: None,
            routed_to_another_model: false,
            harness_message: None,
            record: None,
        }
    }

    #[test]
    fn a_restarted_campaign_ignores_the_previous_executions_results() {
        let execution_started = 2_000;
        assert!(!result_belongs_to_execution(
            &campaign_result(execution_started - 1),
            execution_started
        ));
        assert!(result_belongs_to_execution(
            &campaign_result(execution_started),
            execution_started
        ));
        assert!(result_belongs_to_execution(
            &campaign_result(execution_started + 1),
            execution_started
        ));
    }

    fn test_campaign_plan() -> CampaignPlan {
        CampaignPolicy {
            campaign_id: "campaign-reset-test".to_owned(),
            engine_commit_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            comparison_class: CampaignComparisonClass::ModelComparison,
            execution_profile: CampaignExecutionProfile::Warm,
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            backend_runtime_version: "runtime-v1".to_owned(),
            hardware: BenchmarkHardwareIdentity::default(),
            quality: QualityPreference::Balanced,
            run_timeout_seconds: 600,
            candidates: vec![CampaignCandidate {
                representation: CampaignRepresentation {
                    backend_id: "ollama-compatible".to_owned(),
                    model_id: "test-model".to_owned(),
                    model_version: "test-model-digest".to_owned(),
                    quantization: "q4".to_owned(),
                    representation_size_bytes: 1_024,
                },
                source: CampaignCandidateSource::installed(),
            }],
            task_ids: vec!["project_inspection_v1".to_owned()],
            benchmark_runtime: None,
            repetitions: 1,
            host_seed: 7,
        }
        .freeze()
        .expect("test campaign should freeze")
    }

    fn completed_campaign_run(plan: &CampaignPlan) -> CampaignRun {
        let mut run = CampaignRun::prepare(plan.clone());
        run.start(None, &BTreeSet::from(["test-model".to_owned()]))
            .expect("installed candidate should start");
        let scheduled = run
            .next_scheduled()
            .expect("test campaign should schedule one run");
        run.record(CampaignEvidence {
            scheduled,
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: None,
            attempt: 0,
            record: None,
        })
        .expect("failed evidence should complete the only scheduled run");
        assert_eq!(run.state(), CampaignState::Completed);
        run
    }

    fn campaign_reset_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-campaign-reset-test-{name}-{}-{}",
            std::process::id(),
            unix_ms()
        ))
    }

    fn write_test_checkpoint(root: &Path, plan: &CampaignPlan, run: &CampaignRun) {
        fs::create_dir_all(root).expect("checkpoint root should be created");
        let checkpoint = CampaignCheckpoint {
            plan: plan.clone(),
            run: run.clone(),
        };
        fs::write(
            campaign_checkpoint_path(root),
            serde_json::to_vec_pretty(&checkpoint).expect("checkpoint should serialize"),
        )
        .expect("checkpoint should be written");
    }

    #[test]
    fn campaign_control_state_preserves_existing_state_machine_behavior() {
        let plan = test_campaign_plan();
        let mut panel = BenchmarkCampaignPanel::default();
        assert_eq!(panel.control_state(), None);
        assert!(panel.settings_editable());

        panel.plan = Some(plan.clone());
        panel.run = Some(CampaignRun::prepare(plan.clone()));
        assert_eq!(panel.control_state(), Some(CampaignState::Planned));
        assert!(!panel.settings_editable());

        panel
            .run
            .as_mut()
            .expect("planned run")
            .start(None, &BTreeSet::from(["test-model".to_owned()]))
            .expect("installed candidate should start");
        assert_eq!(panel.control_state(), Some(CampaignState::Running));

        panel.run.as_mut().expect("running run").pause();
        assert_eq!(panel.control_state(), Some(CampaignState::Paused));

        panel.run = Some(completed_campaign_run(&plan));
        assert_eq!(panel.control_state(), Some(CampaignState::Completed));
    }

    #[test]
    fn completed_campaign_reset_clears_freeze_and_machine_checkpoint() {
        let root = campaign_reset_test_root("completed");
        let plan = test_campaign_plan();
        let run = completed_campaign_run(&plan);
        write_test_checkpoint(&root, &plan, &run);

        let mut panel = BenchmarkCampaignPanel {
            plan: Some(plan),
            run: Some(run),
            acquisition_approved: true,
            message: Some("Campaign complete.".to_owned()),
            ..BenchmarkCampaignPanel::default()
        };
        assert_eq!(panel.control_state(), Some(CampaignState::Completed));
        assert!(!panel.settings_editable());
        assert!(campaign_checkpoint_path(&root).exists());

        panel.reset_for_new_campaign(&root);

        assert!(panel.plan.is_none());
        assert!(panel.run.is_none());
        assert!(panel.running.is_none());
        assert!(panel.acquisition.is_none());
        assert!(!panel.acquisition_approved);
        assert!(panel.message.is_none());
        assert!(panel.prior_plan.is_some());
        assert!(panel.settings_editable());
        assert!(!campaign_checkpoint_path(&root).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restored_completed_checkpoint_can_reset_to_editable_campaign() {
        let root = campaign_reset_test_root("restored");
        let plan = test_campaign_plan();
        let run = completed_campaign_run(&plan);
        write_test_checkpoint(&root, &plan, &run);

        let mut restored = BenchmarkCampaignPanel::load_checkpoint(&root);
        assert_eq!(restored.control_state(), Some(CampaignState::Completed));
        assert!(!restored.settings_editable());
        assert!(restored.message.as_deref().is_some_and(|message| {
            message.contains("Restored a machine-local campaign checkpoint")
        }));

        restored.reset_for_new_campaign(&root);

        assert_eq!(restored.control_state(), None);
        assert!(restored.plan.is_none());
        assert!(restored.run.is_none());
        assert!(restored.settings_editable());
        assert!(!campaign_checkpoint_path(&root).exists());
        let _ = fs::remove_dir_all(root);
    }
}
