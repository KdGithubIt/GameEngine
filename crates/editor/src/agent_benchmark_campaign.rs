//! ADR 0156 automated multi-model benchmark campaign orchestration.
//!
//! Campaign state is machine-local application data. The immutable plan freezes every comparison
//! dimension before measured work starts; candidate-visible fixture contracts are kept separate
//! from host-only evaluation state.

use crate::agent_benchmark::{
    benchmark_task, comparison_equivalence, BenchmarkExecutionIdentity, BenchmarkHardwareIdentity,
    BenchmarkModelIdentity, BenchmarkRecord, BenchmarkTaskKind, BenchmarkToolBudget,
    ComparisonEquivalence, BENCHMARK_CORPUS_VERSION, BENCHMARK_HARNESS_VERSION,
    BENCHMARK_SCHEMA_VERSION, WORKLOAD_POLICY_VERSION,
};
use crate::native_agent::BASELINE_HARNESS_VERSION;
use crate::native_agent_runtime::{HarnessPolicy, NATIVE_WRITE_HARNESS_VERSION};
use crate::resource_arbitration::{InferenceWorkload, QualityPreference, TelemetryValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const CAMPAIGN_SCHEMA_VERSION: u32 = 1;
pub(crate) const CAMPAIGN_HARNESS_VERSION: &str = "gameengine-agent-benchmark-campaign-v1";
pub(crate) const CAMPAIGN_SCHEDULE_VERSION: &str = "task-repetition-candidate-interleave-v1";
pub(crate) const CAMPAIGN_FIXTURE_VERSION: &str = "gameengine-agent-fixture-v1";
pub(crate) const DEFAULT_CAMPAIGN_REPETITIONS: u32 = 3;
const MAX_REPETITIONS: u32 = 20;
const MAX_PRE_MEASUREMENT_RETRIES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignComparisonClass {
    ModelComparison,
    RuntimeCharacterization,
}

impl CampaignComparisonClass {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ModelComparison => "Model comparison",
            Self::RuntimeCharacterization => "Runtime / platform characterization",
        }
    }

    const fn identity(self) -> &'static str {
        match self {
            Self::ModelComparison => "model_comparison",
            Self::RuntimeCharacterization => "runtime_characterization",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignExecutionProfile {
    Warm,
    Cold,
}

impl CampaignExecutionProfile {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Warm => "Warm",
            Self::Cold => "Cold",
        }
    }

    const fn identity(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignExecutionEnvironment {
    CompatibleBackend,
    WindowsNative,
    Wsl2Linux,
}

impl CampaignExecutionEnvironment {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CompatibleBackend => "Frozen compatible backend",
            Self::WindowsNative => "Windows native",
            Self::Wsl2Linux => "WSL2 Linux",
        }
    }

    const fn identity(self) -> &'static str {
        match self {
            Self::CompatibleBackend => "compatible_backend",
            Self::WindowsNative => "windows_native",
            Self::Wsl2Linux => "wsl2_linux",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CampaignTaskHarness {
    NativeReadQuestion,
    GovernedAgentHost,
    ProductionRuntimeDebug,
}

pub(crate) fn campaign_task_harness(task_id: &str) -> Result<CampaignTaskHarness, String> {
    let task = benchmark_task(task_id)
        .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    Ok(match task.kind {
        BenchmarkTaskKind::ReadQuestion => CampaignTaskHarness::NativeReadQuestion,
        BenchmarkTaskKind::ProjectInspection
        | BenchmarkTaskKind::CodeImplementation
        | BenchmarkTaskKind::TypedAuthoringMutation
        | BenchmarkTaskKind::ValidationRepair => CampaignTaskHarness::GovernedAgentHost,
        BenchmarkTaskKind::RuntimeInteraction | BenchmarkTaskKind::VisualEvaluation => {
            CampaignTaskHarness::ProductionRuntimeDebug
        }
    })
}

pub(crate) fn campaign_task_workload(task_id: &str) -> Result<InferenceWorkload, String> {
    let task = benchmark_task(task_id)
        .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    Ok(match task.kind {
        BenchmarkTaskKind::ReadQuestion | BenchmarkTaskKind::ProjectInspection => {
            InferenceWorkload::InteractiveReasoning
        }
        BenchmarkTaskKind::CodeImplementation
        | BenchmarkTaskKind::TypedAuthoringMutation
        | BenchmarkTaskKind::ValidationRepair => InferenceWorkload::StrongReasoning,
        BenchmarkTaskKind::RuntimeInteraction | BenchmarkTaskKind::VisualEvaluation => {
            InferenceWorkload::RuntimeObservation
        }
    })
}

fn expected_runtime_harness(harness: CampaignTaskHarness) -> &'static str {
    match harness {
        CampaignTaskHarness::NativeReadQuestion => BASELINE_HARNESS_VERSION,
        CampaignTaskHarness::GovernedAgentHost | CampaignTaskHarness::ProductionRuntimeDebug => {
            NATIVE_WRITE_HARNESS_VERSION
        }
    }
}

fn task_permission_budget(kind: BenchmarkTaskKind) -> Vec<String> {
    match kind {
        BenchmarkTaskKind::CodeImplementation | BenchmarkTaskKind::ValidationRepair => {
            vec!["code_workspace_apply".to_owned()]
        }
        BenchmarkTaskKind::RuntimeInteraction => {
            vec!["runtime_launch".to_owned(), "runtime_input_control".to_owned()]
        }
        BenchmarkTaskKind::VisualEvaluation => vec![
            "runtime_launch".to_owned(),
            "runtime_input_control".to_owned(),
            "frame_capture".to_owned(),
        ],
        _ => Vec::new(),
    }
}

fn task_work_claim_budget(kind: BenchmarkTaskKind) -> Vec<String> {
    match kind {
        BenchmarkTaskKind::CodeImplementation | BenchmarkTaskKind::ValidationRepair => {
            vec!["code_path".to_owned()]
        }
        BenchmarkTaskKind::TypedAuthoringMutation => vec!["authoring_document".to_owned()],
        _ => Vec::new(),
    }
}

fn task_tool_budget(task_id: &str) -> Result<BenchmarkToolBudget, String> {
    let task = benchmark_task(task_id)
        .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    let harness = campaign_task_harness(task_id)?;
    Ok(match harness {
        CampaignTaskHarness::NativeReadQuestion => BenchmarkToolBudget {
            max_model_turns: 1,
            max_tool_failures: 0,
            repair_budget: 0,
            permission_budget: vec!["read_only".to_owned()],
            work_claims: Vec::new(),
        },
        CampaignTaskHarness::GovernedAgentHost | CampaignTaskHarness::ProductionRuntimeDebug => {
            let policy = HarnessPolicy::default();
            BenchmarkToolBudget {
                max_model_turns: policy.max_model_turns,
                max_tool_failures: policy.max_tool_failures,
                repair_budget: policy.repair_budget,
                permission_budget: task_permission_budget(task.kind),
                work_claims: task_work_claim_budget(task.kind),
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CampaignRepresentation {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: String,
    pub(crate) quantization: String,
    pub(crate) representation_size_bytes: u64,
    pub(crate) backend_runtime_version: String,
}

impl CampaignRepresentation {
    pub(crate) fn from_model(model: &BenchmarkModelIdentity) -> Self {
        Self {
            backend_id: model.backend_id.clone(),
            model_id: model.model_id.clone(),
            model_version: measured_text(&model.model_version).unwrap_or_default(),
            quantization: measured_text(&model.quantization).unwrap_or_default(),
            representation_size_bytes: measured_u64(&model.representation_size_bytes)
                .unwrap_or_default(),
            backend_runtime_version: measured_text(&model.backend_runtime_version)
                .unwrap_or_default(),
        }
    }

    fn exact(&self) -> bool {
        !self.backend_id.trim().is_empty()
            && !self.model_id.trim().is_empty()
            && !self.model_version.trim().is_empty()
            && !self.quantization.trim().is_empty()
            && self.representation_size_bytes > 0
            && !self.backend_runtime_version.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignCandidateSource {
    pub(crate) source_reference: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) transfer_size_bytes: Option<u64>,
    pub(crate) storage_size_bytes: Option<u64>,
}

impl CampaignCandidateSource {
    pub(crate) fn installed() -> Self {
        Self {
            source_reference: None,
            license: None,
            transfer_size_bytes: None,
            storage_size_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignCandidate {
    pub(crate) model: BenchmarkModelIdentity,
    pub(crate) source: CampaignCandidateSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignFixtureIdentity {
    pub(crate) fixture_id: String,
    pub(crate) fixture_version: String,
    pub(crate) instance_id: String,
    host_seed: u64,
}

impl CampaignFixtureIdentity {
    pub(crate) fn for_task(task_id: &str, host_seed: u64) -> Self {
        let instance = stable_hash(format!("{task_id}:{host_seed}").as_bytes());
        Self {
            fixture_id: format!("gameengine-agent-{task_id}"),
            fixture_version: CAMPAIGN_FIXTURE_VERSION.to_owned(),
            instance_id: format!("{instance:016x}"),
            host_seed,
        }
    }

    pub(crate) fn candidate_contract(
        &self,
        task_id: &str,
    ) -> Result<CandidateTaskContract, String> {
        let task = benchmark_task(task_id)
            .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
        let marker_hash = self.host_seed.rotate_left(17) ^ stable_hash(task_id.as_bytes());
        let visible_marker = format!("fixture-{:08x}", marker_hash as u32);
        Ok(CandidateTaskContract {
            task_id: task.id.to_owned(),
            prompt: candidate_prompt(task.kind, &visible_marker),
            completion_criteria: task
                .completion_criteria
                .iter()
                .map(|criterion| (*criterion).to_owned())
                .collect(),
        })
    }

    fn host_evaluator(&self, task_id: &str) -> Result<HostOnlyEvaluator, String> {
        let task = benchmark_task(task_id)
            .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
        let hidden = stable_hash(
            format!("{}:{}", task.id, self.host_seed.rotate_right(9)).as_bytes(),
        );
        Ok(HostOnlyEvaluator {
            task_id: task.id.to_owned(),
            hidden_token: format!("host-{hidden:016x}"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CandidateTaskContract {
    pub(crate) task_id: String,
    pub(crate) prompt: String,
    pub(crate) completion_criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostOnlyEvaluator {
    task_id: String,
    hidden_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignTaskPlan {
    pub(crate) task_id: String,
    pub(crate) fixture: CampaignFixtureIdentity,
    pub(crate) runtime_harness_version: String,
    pub(crate) workload: InferenceWorkload,
    pub(crate) tool_budget: BenchmarkToolBudget,
}

impl CampaignTaskPlan {
    pub(crate) fn for_task(task_id: &str, host_seed: u64) -> Result<Self, String> {
        let harness = campaign_task_harness(task_id)?;
        Ok(Self {
            task_id: task_id.to_owned(),
            fixture: CampaignFixtureIdentity::for_task(task_id, host_seed),
            runtime_harness_version: expected_runtime_harness(harness).to_owned(),
            workload: campaign_task_workload(task_id)?,
            tool_budget: task_tool_budget(task_id)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub(crate) enum CampaignSeedPolicy {
    Fixed(u64),
    ProviderUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkCampaignPlan {
    pub(crate) schema_version: u32,
    pub(crate) corpus_version: String,
    pub(crate) benchmark_harness_version: String,
    pub(crate) campaign_harness_version: String,
    pub(crate) schedule_version: String,
    pub(crate) candidates: Vec<CampaignCandidate>,
    pub(crate) tasks: Vec<CampaignTaskPlan>,
    pub(crate) repetitions: u32,
    pub(crate) comparison_class: CampaignComparisonClass,
    pub(crate) execution_profile: CampaignExecutionProfile,
    pub(crate) execution_environments: Vec<CampaignExecutionEnvironment>,
    pub(crate) quality: QualityPreference,
    pub(crate) hardware: BenchmarkHardwareIdentity,
    pub(crate) workload_policy_version: String,
    pub(crate) sampling_profile: String,
    pub(crate) seed_policy: CampaignSeedPolicy,
}

impl BenchmarkCampaignPlan {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != CAMPAIGN_SCHEMA_VERSION
            || self.corpus_version != BENCHMARK_CORPUS_VERSION
            || self.benchmark_harness_version != BENCHMARK_HARNESS_VERSION
            || self.campaign_harness_version != CAMPAIGN_HARNESS_VERSION
            || self.schedule_version != CAMPAIGN_SCHEDULE_VERSION
            || self.workload_policy_version != WORKLOAD_POLICY_VERSION
        {
            return Err("campaign schema or harness identity is incompatible".to_owned());
        }
        if self.candidates.is_empty()
            || self.tasks.is_empty()
            || self.repetitions == 0
            || self.repetitions > MAX_REPETITIONS
            || self.sampling_profile.trim().is_empty()
        {
            return Err("campaign selection or sampling policy is invalid".to_owned());
        }
        if !hardware_exact(&self.hardware) {
            return Err("campaign hardware identity must be measured before execution".to_owned());
        }

        let mut representations = BTreeSet::new();
        for candidate in &self.candidates {
            let representation = CampaignRepresentation::from_model(&candidate.model);
            if !representation.exact() || !representations.insert(representation) {
                return Err("campaign candidates require unique exact representations".to_owned());
            }
        }

        let mut task_ids = BTreeSet::new();
        for task in &self.tasks {
            let expected = CampaignTaskPlan::for_task(&task.task_id, task.fixture.host_seed)?;
            if !task_ids.insert(task.task_id.clone())
                || task.fixture.fixture_id.trim().is_empty()
                || task.fixture.fixture_version.trim().is_empty()
                || task.fixture.instance_id.trim().is_empty()
                || task.runtime_harness_version != expected.runtime_harness_version
                || task.workload != expected.workload
                || task.tool_budget != expected.tool_budget
            {
                return Err("campaign task/fixture execution identity is invalid or duplicated"
                    .to_owned());
            }
        }

        let environments = self
            .execution_environments
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if environments.len() != self.execution_environments.len() {
            return Err("campaign execution environments must be unique".to_owned());
        }
        match self.comparison_class {
            CampaignComparisonClass::ModelComparison if environments.len() == 1 => Ok(()),
            CampaignComparisonClass::RuntimeCharacterization
                if self.candidates.len() == 1
                    && environments
                        == BTreeSet::from([
                            CampaignExecutionEnvironment::WindowsNative,
                            CampaignExecutionEnvironment::Wsl2Linux,
                        ]) =>
            {
                Ok(())
            }
            CampaignComparisonClass::ModelComparison => {
                Err("model comparison must freeze one execution environment".to_owned())
            }
            CampaignComparisonClass::RuntimeCharacterization => Err(
                "runtime characterization requires one model and Windows native + WSL2 Linux"
                    .to_owned(),
            ),
        }
    }

    pub(crate) fn fingerprint(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Ok(format!(
            "{:016x}{:016x}",
            stable_hash(&bytes),
            stable_hash_seed(&bytes, 0x8422_2325_cbf2_9ce4)
        ))
    }

    fn schedule(&self) -> Result<Vec<CampaignScheduleEntry>, String> {
        self.validate()?;
        let mut schedule = Vec::new();
        let mut ordinal = 0_u32;
        for (task_index, task) in self.tasks.iter().enumerate() {
            for repetition in 1..=self.repetitions {
                match self.comparison_class {
                    CampaignComparisonClass::ModelComparison => {
                        let environment = self.execution_environments[0];
                        for candidate_index in 0..self.candidates.len() {
                            schedule.push(CampaignScheduleEntry {
                                ordinal,
                                task_index,
                                candidate_index,
                                task_id: task.task_id.clone(),
                                repetition,
                                execution_environment: environment,
                            });
                            ordinal = ordinal.saturating_add(1);
                        }
                    }
                    CampaignComparisonClass::RuntimeCharacterization => {
                        for environment in &self.execution_environments {
                            schedule.push(CampaignScheduleEntry {
                                ordinal,
                                task_index,
                                candidate_index: 0,
                                task_id: task.task_id.clone(),
                                repetition,
                                execution_environment: *environment,
                            });
                            ordinal = ordinal.saturating_add(1);
                        }
                    }
                }
            }
        }
        Ok(schedule)
    }

    pub(crate) fn resume_identity(&self) -> CampaignResumeIdentity {
        CampaignResumeIdentity {
            corpus_version: self.corpus_version.clone(),
            benchmark_harness_version: self.benchmark_harness_version.clone(),
            campaign_harness_version: self.campaign_harness_version.clone(),
            schedule_version: self.schedule_version.clone(),
            workload_policy_version: self.workload_policy_version.clone(),
            candidates: self
                .candidates
                .iter()
                .map(|candidate| CampaignRepresentation::from_model(&candidate.model))
                .collect(),
            fixtures: self.tasks.iter().map(|task| task.fixture.clone()).collect(),
            task_runtime_harness_versions: self
                .tasks
                .iter()
                .map(|task| task.runtime_harness_version.clone())
                .collect(),
            task_workloads: self.tasks.iter().map(|task| task.workload).collect(),
            task_tool_budgets: self
                .tasks
                .iter()
                .map(|task| task.tool_budget.clone())
                .collect(),
            hardware: self.hardware.clone(),
            comparison_class: self.comparison_class,
            execution_profile: self.execution_profile,
            execution_environments: self.execution_environments.clone(),
            sampling_profile: self.sampling_profile.clone(),
            seed_policy: self.seed_policy.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignResumeIdentity {
    pub(crate) corpus_version: String,
    pub(crate) benchmark_harness_version: String,
    pub(crate) campaign_harness_version: String,
    pub(crate) schedule_version: String,
    pub(crate) workload_policy_version: String,
    pub(crate) candidates: Vec<CampaignRepresentation>,
    pub(crate) fixtures: Vec<CampaignFixtureIdentity>,
    pub(crate) task_runtime_harness_versions: Vec<String>,
    pub(crate) task_workloads: Vec<InferenceWorkload>,
    pub(crate) task_tool_budgets: Vec<BenchmarkToolBudget>,
    pub(crate) hardware: BenchmarkHardwareIdentity,
    pub(crate) comparison_class: CampaignComparisonClass,
    pub(crate) execution_profile: CampaignExecutionProfile,
    pub(crate) execution_environments: Vec<CampaignExecutionEnvironment>,
    pub(crate) sampling_profile: String,
    pub(crate) seed_policy: CampaignSeedPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignMissingCandidate {
    pub(crate) representation: CampaignRepresentation,
    pub(crate) source_reference: String,
    pub(crate) license: String,
    pub(crate) transfer_size_bytes: u64,
    pub(crate) storage_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignPreflight {
    pub(crate) plan_fingerprint: String,
    pub(crate) missing: Vec<CampaignMissingCandidate>,
    pub(crate) total_transfer_size_bytes: u64,
    pub(crate) total_storage_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignDownloadApproval {
    pub(crate) plan_fingerprint: String,
    pub(crate) approved_missing: Vec<CampaignRepresentation>,
}

impl CampaignDownloadApproval {
    pub(crate) fn exact(preflight: &CampaignPreflight) -> Self {
        Self {
            plan_fingerprint: preflight.plan_fingerprint.clone(),
            approved_missing: preflight
                .missing
                .iter()
                .map(|missing| missing.representation.clone())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkCampaignState {
    Draft,
    Running,
    Paused,
    Stopped,
    Completed,
    Incompatible,
}

impl BenchmarkCampaignState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
            Self::Completed => "Completed",
            Self::Incompatible => "Incompatible",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignRunStatus {
    Pending,
    Preparing,
    Running,
    Completed,
    Failed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignFailureKind {
    OutOfMemory,
    BackendCrash,
    InvalidToolBehavior,
    ValidationFailure,
    RuntimeFailure,
    VisualFailure,
    TaskTimeout,
    IdentityMismatch,
    InfrastructurePreMeasurement,
    Cancelled,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignScheduleEntry {
    pub(crate) ordinal: u32,
    pub(crate) task_index: usize,
    pub(crate) candidate_index: usize,
    pub(crate) task_id: String,
    pub(crate) repetition: u32,
    pub(crate) execution_environment: CampaignExecutionEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignRunProgress {
    pub(crate) schedule: CampaignScheduleEntry,
    pub(crate) status: CampaignRunStatus,
    pub(crate) pre_measurement_retries: u32,
    pub(crate) record: Option<BenchmarkRecord>,
    pub(crate) failure: Option<CampaignFailureKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignRunRequest {
    pub(crate) campaign_fingerprint: String,
    pub(crate) schedule: CampaignScheduleEntry,
    pub(crate) model: BenchmarkModelIdentity,
    pub(crate) fixture: CampaignFixtureIdentity,
    pub(crate) candidate_contract: CandidateTaskContract,
    pub(crate) harness: CampaignTaskHarness,
    pub(crate) reset_instance_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreMeasurementFailureDisposition {
    Retry,
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchmarkCampaign {
    pub(crate) schema_version: u32,
    pub(crate) plan: BenchmarkCampaignPlan,
    pub(crate) plan_fingerprint: String,
    pub(crate) state: BenchmarkCampaignState,
    pub(crate) runs: Vec<CampaignRunProgress>,
    pub(crate) created_unix_ms: u64,
    pub(crate) updated_unix_ms: u64,
}

impl BenchmarkCampaign {
    pub(crate) fn new(plan: BenchmarkCampaignPlan) -> Result<Self, String> {
        let plan_fingerprint = plan.fingerprint()?;
        let runs = plan
            .schedule()?
            .into_iter()
            .map(|schedule| CampaignRunProgress {
                schedule,
                status: CampaignRunStatus::Pending,
                pre_measurement_retries: 0,
                record: None,
                failure: None,
            })
            .collect();
        let now = unix_ms();
        Ok(Self {
            schema_version: CAMPAIGN_SCHEMA_VERSION,
            plan,
            plan_fingerprint,
            state: BenchmarkCampaignState::Draft,
            runs,
            created_unix_ms: now,
            updated_unix_ms: now,
        })
    }

    pub(crate) fn preflight(
        &self,
        installed: &[BenchmarkModelIdentity],
    ) -> Result<CampaignPreflight, String> {
        self.plan.validate()?;
        let mut missing = Vec::new();
        let mut transfer = 0_u64;
        let mut storage = 0_u64;
        for candidate in &self.plan.candidates {
            let representation = CampaignRepresentation::from_model(&candidate.model);
            if installed
                .iter()
                .any(|model| CampaignRepresentation::from_model(model) == representation)
            {
                continue;
            }
            let source_reference = candidate
                .source
                .source_reference
                .clone()
                .ok_or_else(|| format!("missing source for `{}`", representation.model_id))?;
            let license = candidate
                .source
                .license
                .clone()
                .ok_or_else(|| format!("missing license for `{}`", representation.model_id))?;
            let transfer_size_bytes = candidate.source.transfer_size_bytes.ok_or_else(|| {
                format!("missing transfer size for `{}`", representation.model_id)
            })?;
            let storage_size_bytes = candidate.source.storage_size_bytes.ok_or_else(|| {
                format!("missing storage size for `{}`", representation.model_id)
            })?;
            transfer = transfer.saturating_add(transfer_size_bytes);
            storage = storage.saturating_add(storage_size_bytes);
            missing.push(CampaignMissingCandidate {
                representation,
                source_reference,
                license,
                transfer_size_bytes,
                storage_size_bytes,
            });
        }
        Ok(CampaignPreflight {
            plan_fingerprint: self.plan_fingerprint.clone(),
            missing,
            total_transfer_size_bytes: transfer,
            total_storage_size_bytes: storage,
        })
    }

    pub(crate) fn validate_download_approval(
        &self,
        preflight: &CampaignPreflight,
        approval: &CampaignDownloadApproval,
    ) -> Result<(), String> {
        if preflight.plan_fingerprint != self.plan_fingerprint
            || approval.plan_fingerprint != self.plan_fingerprint
        {
            return Err("download approval does not belong to the frozen campaign".to_owned());
        }
        let expected = preflight
            .missing
            .iter()
            .map(|missing| missing.representation.clone())
            .collect::<Vec<_>>();
        if approval.approved_missing != expected {
            return Err("download approval must match the exact missing candidate set".to_owned());
        }
        Ok(())
    }

    pub(crate) fn start(&mut self, verified: &[BenchmarkModelIdentity]) -> Result<(), String> {
        if self.state != BenchmarkCampaignState::Draft {
            return Err("campaign can only start from Draft".to_owned());
        }
        self.plan.validate()?;
        for candidate in &self.plan.candidates {
            let expected = CampaignRepresentation::from_model(&candidate.model);
            if !verified
                .iter()
                .any(|model| CampaignRepresentation::from_model(model) == expected)
            {
                return Err(format!(
                    "candidate `{}` was not content-verified before campaign start",
                    expected.model_id
                ));
            }
        }
        self.state = BenchmarkCampaignState::Running;
        self.touch();
        Ok(())
    }

    pub(crate) fn begin_next_run(&mut self) -> Result<Option<CampaignRunRequest>, String> {
        if self.state != BenchmarkCampaignState::Running {
            return Ok(None);
        }
        if self.has_active_run() {
            return Err("measured local campaign execution is sequential".to_owned());
        }
        let Some(index) = self
            .runs
            .iter()
            .position(|run| run.status == CampaignRunStatus::Pending)
        else {
            self.state = BenchmarkCampaignState::Completed;
            self.touch();
            return Ok(None);
        };
        self.runs[index].status = CampaignRunStatus::Preparing;
        let schedule = self.runs[index].schedule.clone();
        let request = self.run_request(schedule)?;
        self.touch();
        Ok(Some(request))
    }

    pub(crate) fn preparing_run_request(&self) -> Result<Option<CampaignRunRequest>, String> {
        self.request_for_status(
            CampaignRunStatus::Preparing,
            "campaign persistence contains multiple preparing runs",
        )
    }

    pub(crate) fn running_run_request(&self) -> Result<Option<CampaignRunRequest>, String> {
        self.request_for_status(
            CampaignRunStatus::Running,
            "campaign persistence contains multiple measured runs",
        )
    }

    fn request_for_status(
        &self,
        status: CampaignRunStatus,
        duplicate_message: &str,
    ) -> Result<Option<CampaignRunRequest>, String> {
        let matches = self
            .runs
            .iter()
            .filter(|run| run.status == status)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(duplicate_message.to_owned());
        }
        matches
            .first()
            .map(|run| self.run_request(run.schedule.clone()))
            .transpose()
    }

    fn run_request(
        &self,
        schedule: CampaignScheduleEntry,
    ) -> Result<CampaignRunRequest, String> {
        let model = self
            .plan
            .candidates
            .get(schedule.candidate_index)
            .ok_or_else(|| "campaign candidate index is invalid".to_owned())?
            .model
            .clone();
        let fixture = self
            .plan
            .tasks
            .get(schedule.task_index)
            .ok_or_else(|| "campaign task index is invalid".to_owned())?
            .fixture
            .clone();
        let candidate_contract = fixture.candidate_contract(&schedule.task_id)?;
        let harness = campaign_task_harness(&schedule.task_id)?;
        let reset_hash = stable_hash(
            format!(
                "{}:{}:{}:{}:{}",
                self.plan_fingerprint,
                schedule.task_id,
                schedule.repetition,
                schedule.candidate_index,
                schedule.execution_environment.identity()
            )
            .as_bytes(),
        );
        Ok(CampaignRunRequest {
            campaign_fingerprint: self.plan_fingerprint.clone(),
            schedule,
            model,
            fixture,
            candidate_contract,
            harness,
            reset_instance_id: format!("run-{reset_hash:016x}"),
        })
    }

    pub(crate) fn mark_measurement_started(
        &mut self,
        request: &CampaignRunRequest,
    ) -> Result<(), String> {
        self.validate_request(request, CampaignRunStatus::Preparing)?;
        self.runs[request.schedule.ordinal as usize].status = CampaignRunStatus::Running;
        self.touch();
        Ok(())
    }

    pub(crate) fn record_pre_measurement_failure(
        &mut self,
        request: &CampaignRunRequest,
    ) -> Result<PreMeasurementFailureDisposition, String> {
        self.validate_request(request, CampaignRunStatus::Preparing)?;
        let run = &mut self.runs[request.schedule.ordinal as usize];
        run.pre_measurement_retries = run.pre_measurement_retries.saturating_add(1);
        let disposition = if run.pre_measurement_retries <= MAX_PRE_MEASUREMENT_RETRIES {
            run.status = CampaignRunStatus::Pending;
            PreMeasurementFailureDisposition::Retry
        } else {
            run.status = CampaignRunStatus::Failed;
            run.failure = Some(CampaignFailureKind::InfrastructurePreMeasurement);
            PreMeasurementFailureDisposition::Exhausted
        };
        self.touch();
        Ok(disposition)
    }

    pub(crate) fn complete_run(
        &mut self,
        request: &CampaignRunRequest,
        record: BenchmarkRecord,
    ) -> Result<(), String> {
        let record = self.validate_and_attach_record(request, record)?;
        let run = &mut self.runs[request.schedule.ordinal as usize];
        run.status = CampaignRunStatus::Completed;
        run.record = Some(record);
        self.touch();
        Ok(())
    }

    pub(crate) fn fail_measured_run(
        &mut self,
        request: &CampaignRunRequest,
        failure: CampaignFailureKind,
    ) -> Result<(), String> {
        self.fail_measured_run_with_record(request, failure, None)
    }

    pub(crate) fn fail_measured_run_with_record(
        &mut self,
        request: &CampaignRunRequest,
        failure: CampaignFailureKind,
        record: Option<BenchmarkRecord>,
    ) -> Result<(), String> {
        if failure == CampaignFailureKind::InfrastructurePreMeasurement {
            return Err(
                "pre-measurement infrastructure failures use the bounded retry path".to_owned(),
            );
        }
        self.validate_request(request, CampaignRunStatus::Running)?;
        let record = record
            .map(|record| self.validate_and_attach_record(request, record))
            .transpose()?;
        let run = &mut self.runs[request.schedule.ordinal as usize];
        run.status = CampaignRunStatus::Failed;
        run.failure = Some(failure);
        run.record = record;
        self.touch();
        Ok(())
    }

    fn validate_and_attach_record(
        &mut self,
        request: &CampaignRunRequest,
        mut record: BenchmarkRecord,
    ) -> Result<BenchmarkRecord, String> {
        self.validate_request(request, CampaignRunStatus::Running)?;
        let task_plan = &self.plan.tasks[request.schedule.task_index];
        let descriptor = benchmark_task(&request.schedule.task_id)
            .ok_or_else(|| "frozen campaign task descriptor is unavailable".to_owned())?;
        let expected_completion_criteria = descriptor
            .completion_criteria
            .iter()
            .map(|criterion| (*criterion).to_owned())
            .collect::<Vec<_>>();
        if record.schema_version != BENCHMARK_SCHEMA_VERSION
            || record.identity.corpus_version != self.plan.corpus_version
            || record.identity.harness_version != self.plan.benchmark_harness_version
            || record.identity.runtime_harness_version != task_plan.runtime_harness_version
            || CampaignRepresentation::from_model(&record.identity.model)
                != CampaignRepresentation::from_model(&request.model)
            || record.identity.task_id != request.schedule.task_id
            || record.identity.hardware != self.plan.hardware
            || record.identity.quality != self.plan.quality
            || record.identity.workload_policy_version != self.plan.workload_policy_version
            || record.identity.observed_workload != TelemetryValue::Measured(task_plan.workload)
            || record.identity.tool_budget != task_plan.tool_budget
            || record.identity.completion_criteria != expected_completion_criteria
            || record.identity.execution.is_some()
        {
            self.reject_run(request, CampaignFailureKind::IdentityMismatch)?;
            return Err("automatic campaign evidence identity mismatch".to_owned());
        }
        apply_execution_identity(&mut record, self, request)?;
        Ok(record)
    }

    pub(crate) fn pause(&mut self) -> Result<(), String> {
        if self.state != BenchmarkCampaignState::Running || self.has_active_run() {
            return Err("campaign pause is allowed only at a run boundary".to_owned());
        }
        self.state = BenchmarkCampaignState::Paused;
        self.touch();
        Ok(())
    }

    pub(crate) fn stop(&mut self) -> Result<(), String> {
        if self.has_active_run() {
            return Err("campaign stop is allowed only after active work is cleaned up".to_owned());
        }
        if matches!(
            self.state,
            BenchmarkCampaignState::Completed | BenchmarkCampaignState::Incompatible
        ) {
            return Err("terminal campaign cannot be stopped".to_owned());
        }
        self.state = BenchmarkCampaignState::Stopped;
        self.touch();
        Ok(())
    }

    pub(crate) fn resume_identity_for_environment(
        &self,
        hardware: BenchmarkHardwareIdentity,
        verified: &[BenchmarkModelIdentity],
    ) -> CampaignResumeIdentity {
        let mut identity = self.plan.resume_identity();
        identity.hardware = hardware;
        identity.candidates = self
            .plan
            .candidates
            .iter()
            .map(|candidate| {
                verified
                    .iter()
                    .find(|model| {
                        model.backend_id == candidate.model.backend_id
                            && model.model_id == candidate.model.model_id
                    })
                    .map(CampaignRepresentation::from_model)
                    .unwrap_or_else(|| CampaignRepresentation::from_model(&BenchmarkModelIdentity {
                        backend_id: candidate.model.backend_id.clone(),
                        model_id: candidate.model.model_id.clone(),
                        model_version: TelemetryValue::Unavailable,
                        quantization: TelemetryValue::Unavailable,
                        representation_size_bytes: TelemetryValue::Unavailable,
                        backend_runtime_version: TelemetryValue::Unavailable,
                    }))
            })
            .collect();
        identity
    }

    pub(crate) fn resume(&mut self, current: CampaignResumeIdentity) -> Result<(), String> {
        if !matches!(
            self.state,
            BenchmarkCampaignState::Paused | BenchmarkCampaignState::Stopped
        ) {
            return Err("campaign is not paused or stopped".to_owned());
        }
        if current != self.plan.resume_identity() {
            self.state = BenchmarkCampaignState::Incompatible;
            self.touch();
            return Err("campaign environment drift requires a new derived campaign".to_owned());
        }
        self.state = BenchmarkCampaignState::Running;
        self.touch();
        Ok(())
    }

    pub(crate) fn completed_records(&self) -> Vec<&BenchmarkRecord> {
        self.runs
            .iter()
            .filter_map(|run| run.record.as_ref())
            .collect()
    }

    pub(crate) fn report(&self) -> CampaignReport {
        CampaignReport::from_campaign(self)
    }

    fn validate_request(
        &self,
        request: &CampaignRunRequest,
        expected_status: CampaignRunStatus,
    ) -> Result<(), String> {
        if request.campaign_fingerprint != self.plan_fingerprint {
            return Err("run request belongs to another campaign".to_owned());
        }
        let Some(run) = self.runs.get(request.schedule.ordinal as usize) else {
            return Err("run request ordinal is outside the frozen schedule".to_owned());
        };
        if run.schedule != request.schedule || run.status != expected_status {
            return Err("run request is stale for the current campaign state".to_owned());
        }
        Ok(())
    }

    fn reject_run(
        &mut self,
        request: &CampaignRunRequest,
        failure: CampaignFailureKind,
    ) -> Result<(), String> {
        self.validate_request(request, CampaignRunStatus::Running)?;
        let run = &mut self.runs[request.schedule.ordinal as usize];
        run.status = CampaignRunStatus::Rejected;
        run.failure = Some(failure);
        self.touch();
        Ok(())
    }

    fn has_active_run(&self) -> bool {
        self.runs.iter().any(|run| {
            matches!(
                run.status,
                CampaignRunStatus::Preparing | CampaignRunStatus::Running
            )
        })
    }

    fn touch(&mut self) {
        self.updated_unix_ms = unix_ms();
    }
}

pub(crate) struct BenchmarkCampaignStore {
    root: PathBuf,
}

impl BenchmarkCampaignStore {
    pub(crate) fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    pub(crate) fn save(&self, campaign: &BenchmarkCampaign) -> Result<PathBuf, String> {
        let bytes = serde_json::to_vec_pretty(campaign).map_err(|error| error.to_string())?;
        let path = self.root.join(format!("{}.json", campaign.plan_fingerprint));
        let temporary = self
            .root
            .join(format!("{}.json.tmp", campaign.plan_fingerprint));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        replace_campaign_file(&temporary, &path)?;
        Ok(path)
    }

    pub(crate) fn load(&self, fingerprint: &str) -> Result<BenchmarkCampaign, String> {
        let path = self.root.join(format!("{fingerprint}.json"));
        self.load_path(&path)
    }

    pub(crate) fn load_latest(&self) -> Result<Option<BenchmarkCampaign>, String> {
        let mut latest: Option<BenchmarkCampaign> = None;
        for entry in fs::read_dir(&self.root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let campaign = self.load_path(&path)?;
            if latest
                .as_ref()
                .is_none_or(|current| campaign.updated_unix_ms > current.updated_unix_ms)
            {
                latest = Some(campaign);
            }
        }
        Ok(latest)
    }

    fn load_path(&self, path: &Path) -> Result<BenchmarkCampaign, String> {
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        let campaign = serde_json::from_slice::<BenchmarkCampaign>(&bytes)
            .map_err(|error| error.to_string())?;
        if campaign.schema_version != CAMPAIGN_SCHEMA_VERSION
            || campaign.plan_fingerprint != campaign.plan.fingerprint()?
        {
            return Err("persisted campaign identity is incompatible or corrupted".to_owned());
        }
        Ok(campaign)
    }
}

fn replace_campaign_file(temporary: &Path, path: &Path) -> Result<(), String> {
    let backup = path.with_extension("json.bak");
    if backup.exists() {
        fs::remove_file(&backup).map_err(|error| error.to_string())?;
    }
    let had_existing = path.exists();
    if had_existing {
        fs::rename(path, &backup).map_err(|error| error.to_string())?;
    }
    match fs::rename(temporary, path) {
        Ok(()) => {
            if had_existing {
                let _ = fs::remove_file(backup);
            }
            Ok(())
        }
        Err(error) => {
            if had_existing {
                let _ = fs::rename(&backup, path);
            }
            Err(error.to_string())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignTaskReport {
    pub(crate) attempted: u64,
    pub(crate) successful: u64,
    pub(crate) failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignCandidateReport {
    pub(crate) model_id: String,
    pub(crate) attempted: u64,
    pub(crate) successful: u64,
    pub(crate) failed: u64,
    pub(crate) rejected: u64,
    pub(crate) human_interventions: u64,
    pub(crate) validation_attempts: u64,
    pub(crate) repair_loops: u64,
    pub(crate) oom_count: u64,
    pub(crate) median_success_elapsed_ms: TelemetryValue<u64>,
    pub(crate) median_generation_tokens_per_second_milli: TelemetryValue<u64>,
    pub(crate) median_load_latency_ms: TelemetryValue<u64>,
    pub(crate) peak_backend_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) per_task: BTreeMap<String, CampaignTaskReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignReport {
    pub(crate) campaign_fingerprint: String,
    pub(crate) comparison_class: CampaignComparisonClass,
    pub(crate) execution_profile: CampaignExecutionProfile,
    pub(crate) candidates: Vec<CampaignCandidateReport>,
}

impl CampaignReport {
    fn from_campaign(campaign: &BenchmarkCampaign) -> Self {
        let mut candidates = Vec::new();
        for (candidate_index, candidate) in campaign.plan.candidates.iter().enumerate() {
            let runs = campaign
                .runs
                .iter()
                .filter(|run| run.schedule.candidate_index == candidate_index)
                .collect::<Vec<_>>();
            let mut elapsed = Vec::new();
            let mut throughput = Vec::new();
            let mut load_latency = Vec::new();
            let mut peak_gpu = Vec::new();
            let mut per_task = BTreeMap::new();
            let mut successful = 0_u64;
            let mut failed = 0_u64;
            let mut rejected = 0_u64;
            let mut interventions = 0_u64;
            let mut validations = 0_u64;
            let mut repairs = 0_u64;
            let mut oom_count = 0_u64;
            for run in &runs {
                let task = per_task
                    .entry(run.schedule.task_id.clone())
                    .or_insert(CampaignTaskReport {
                        attempted: 0,
                        successful: 0,
                        failed: 0,
                    });
                if !matches!(
                    run.status,
                    CampaignRunStatus::Pending | CampaignRunStatus::Preparing
                ) {
                    task.attempted = task.attempted.saturating_add(1);
                }
                match run.status {
                    CampaignRunStatus::Completed => {
                        successful = successful.saturating_add(1);
                        task.successful = task.successful.saturating_add(1);
                    }
                    CampaignRunStatus::Failed => {
                        failed = failed.saturating_add(1);
                        task.failed = task.failed.saturating_add(1);
                    }
                    CampaignRunStatus::Rejected => {
                        rejected = rejected.saturating_add(1);
                        task.failed = task.failed.saturating_add(1);
                    }
                    CampaignRunStatus::Pending
                    | CampaignRunStatus::Preparing
                    | CampaignRunStatus::Running => {}
                }
                if run.failure == Some(CampaignFailureKind::OutOfMemory) {
                    oom_count = oom_count.saturating_add(1);
                }
                let Some(record) = run.record.as_ref() else {
                    continue;
                };
                if measured_bool(&record.metrics.completion_success) == Some(true) {
                    push_measured(&mut elapsed, &record.metrics.elapsed_ms);
                }
                push_measured(
                    &mut throughput,
                    &record.metrics.generation_tokens_per_second_milli,
                );
                push_measured(&mut load_latency, &record.metrics.load_latency_ms);
                push_measured(
                    &mut peak_gpu,
                    &record.metrics.peak_backend_gpu_memory_bytes,
                );
                interventions = interventions.saturating_add(
                    measured_u64(&record.metrics.human_interventions).unwrap_or_default(),
                );
                validations = validations.saturating_add(
                    measured_u64(&record.metrics.validation_attempts).unwrap_or_default(),
                );
                repairs = repairs.saturating_add(
                    measured_u64(&record.metrics.repair_loops).unwrap_or_default(),
                );
                oom_count = oom_count.saturating_add(
                    measured_u64(&record.metrics.oom_failures).unwrap_or_default(),
                );
            }
            candidates.push(CampaignCandidateReport {
                model_id: candidate.model.model_id.clone(),
                attempted: successful.saturating_add(failed).saturating_add(rejected),
                successful,
                failed,
                rejected,
                human_interventions: interventions,
                validation_attempts: validations,
                repair_loops: repairs,
                oom_count,
                median_success_elapsed_ms: median(&mut elapsed),
                median_generation_tokens_per_second_milli: median(&mut throughput),
                median_load_latency_ms: median(&mut load_latency),
                peak_backend_gpu_memory_bytes: maximum(&peak_gpu),
                per_task,
            });
        }
        Self {
            campaign_fingerprint: campaign.plan_fingerprint.clone(),
            comparison_class: campaign.plan.comparison_class,
            execution_profile: campaign.plan.execution_profile,
            candidates,
        }
    }
}

pub(crate) fn reusable_baseline(
    plan: &BenchmarkCampaignPlan,
    records: &[BenchmarkRecord],
    baseline_model: &BenchmarkModelIdentity,
) -> bool {
    if plan.validate().is_err()
        || plan.comparison_class != CampaignComparisonClass::ModelComparison
    {
        return false;
    }
    let Some(environment) = plan.execution_environments.first() else {
        return false;
    };
    let expected = CampaignRepresentation::from_model(baseline_model);
    plan.tasks.iter().all(|task| {
        records
            .iter()
            .filter(|record| {
                CampaignRepresentation::from_model(&record.identity.model) == expected
                    && record.identity.task_id == task.task_id
                    && record.identity.execution.as_ref().is_some_and(|execution| {
                        execution.fixture_id == task.fixture.fixture_id
                            && execution.fixture_version == task.fixture.fixture_version
                            && execution.fixture_instance_id == task.fixture.instance_id
                            && execution.comparison_class == plan.comparison_class.identity()
                            && execution.execution_profile == plan.execution_profile.identity()
                            && execution.execution_environment == environment.identity()
                    })
            })
            .count()
            >= plan.repetitions as usize
    })
}

fn apply_execution_identity(
    record: &mut BenchmarkRecord,
    campaign: &BenchmarkCampaign,
    request: &CampaignRunRequest,
) -> Result<(), String> {
    let task = campaign
        .plan
        .tasks
        .get(request.schedule.task_index)
        .ok_or_else(|| "campaign task index is invalid".to_owned())?;
    record.identity.execution = Some(BenchmarkExecutionIdentity {
        campaign_harness_version: CAMPAIGN_HARNESS_VERSION.to_owned(),
        schedule_policy_version: CAMPAIGN_SCHEDULE_VERSION.to_owned(),
        comparison_class: campaign.plan.comparison_class.identity().to_owned(),
        execution_profile: campaign.plan.execution_profile.identity().to_owned(),
        execution_environment: request.schedule.execution_environment.identity().to_owned(),
        fixture_id: task.fixture.fixture_id.clone(),
        fixture_version: task.fixture.fixture_version.clone(),
        fixture_instance_id: task.fixture.instance_id.clone(),
        sampling_profile: campaign.plan.sampling_profile.clone(),
        seed_policy: serde_json::to_string(&campaign.plan.seed_policy)
            .map_err(|error| error.to_string())?,
    });
    Ok(())
}

pub(crate) fn records_are_comparable(left: &BenchmarkRecord, right: &BenchmarkRecord) -> bool {
    comparison_equivalence(left, right) == ComparisonEquivalence::EquivalentModelComparison
}

fn hardware_exact(hardware: &BenchmarkHardwareIdentity) -> bool {
    !hardware.platform.trim().is_empty()
        && measured_text(&hardware.gpu).is_some()
        && measured_u64(&hardware.total_gpu_memory_bytes).is_some()
        && measured_u64(&hardware.total_system_memory_bytes).is_some()
}

fn candidate_prompt(kind: BenchmarkTaskKind, marker: &str) -> String {
    match kind {
        BenchmarkTaskKind::ReadQuestion => format!(
            "Inspect the candidate-visible fixture {marker} and answer the bounded project question with provenance."
        ),
        BenchmarkTaskKind::ProjectInspection => format!(
            "Inspect fixture {marker}. Report the requested project state using governed read-only evidence."
        ),
        BenchmarkTaskKind::CodeImplementation => format!(
            "Implement the visible acceptance criteria in isolated fixture {marker}; use only governed tools."
        ),
        BenchmarkTaskKind::TypedAuthoringMutation => format!(
            "Apply the visible typed authoring change in fixture {marker} through the production authoring API."
        ),
        BenchmarkTaskKind::ValidationRepair => format!(
            "Repair the visible validation failure in fixture {marker} and prove it with managed validation."
        ),
        BenchmarkTaskKind::RuntimeInteraction => format!(
            "Use the production runtime-debug surface for fixture {marker}; satisfy the visible interaction criteria."
        ),
        BenchmarkTaskKind::VisualEvaluation => format!(
            "Use production runtime observation for fixture {marker}; evaluate the captured frame only if image input is available."
        ),
    }
}

fn measured_text(value: &TelemetryValue<String>) -> Option<String> {
    match value {
        TelemetryValue::Measured(value) if !value.trim().is_empty() => Some(value.clone()),
        TelemetryValue::Measured(_)
        | TelemetryValue::ConservativeEstimate(_)
        | TelemetryValue::Unavailable => None,
    }
}

fn measured_u64(value: &TelemetryValue<u64>) -> Option<u64> {
    match value {
        TelemetryValue::Measured(value) => Some(*value),
        TelemetryValue::ConservativeEstimate(_) | TelemetryValue::Unavailable => None,
    }
}

fn measured_bool(value: &TelemetryValue<bool>) -> Option<bool> {
    match value {
        TelemetryValue::Measured(value) => Some(*value),
        TelemetryValue::ConservativeEstimate(_) | TelemetryValue::Unavailable => None,
    }
}

fn push_measured(values: &mut Vec<u64>, value: &TelemetryValue<u64>) {
    if let Some(value) = measured_u64(value) {
        values.push(value);
    }
}

fn median(values: &mut [u64]) -> TelemetryValue<u64> {
    if values.is_empty() {
        return TelemetryValue::Unavailable;
    }
    values.sort_unstable();
    TelemetryValue::Measured(values[values.len() / 2])
}

fn maximum(values: &[u64]) -> TelemetryValue<u64> {
    values
        .iter()
        .copied()
        .max()
        .map(TelemetryValue::Measured)
        .unwrap_or_default()
}

fn stable_hash(bytes: &[u8]) -> u64 {
    stable_hash_seed(bytes, 0xcbf2_9ce4_8422_2325)
}

fn stable_hash_seed(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_benchmark::{BenchmarkIdentity, BenchmarkMetrics, BENCHMARK_TASKS};

    fn model(name: &str) -> BenchmarkModelIdentity {
        BenchmarkModelIdentity {
            backend_id: "ollama-compatible".to_owned(),
            model_id: name.to_owned(),
            model_version: TelemetryValue::Measured(format!("{name}-digest")),
            quantization: TelemetryValue::Measured("q4".to_owned()),
            representation_size_bytes: TelemetryValue::Measured(1_000),
            backend_runtime_version: TelemetryValue::Measured("runtime-v1".to_owned()),
        }
    }

    fn hardware() -> BenchmarkHardwareIdentity {
        BenchmarkHardwareIdentity {
            platform: "windows-x86_64".to_owned(),
            gpu: TelemetryValue::Measured("gpu".to_owned()),
            total_gpu_memory_bytes: TelemetryValue::Measured(12_000),
            total_system_memory_bytes: TelemetryValue::Measured(32_000),
        }
    }

    fn plan() -> BenchmarkCampaignPlan {
        BenchmarkCampaignPlan {
            schema_version: CAMPAIGN_SCHEMA_VERSION,
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            benchmark_harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            campaign_harness_version: CAMPAIGN_HARNESS_VERSION.to_owned(),
            schedule_version: CAMPAIGN_SCHEDULE_VERSION.to_owned(),
            candidates: vec![
                CampaignCandidate {
                    model: model("model-a"),
                    source: CampaignCandidateSource::installed(),
                },
                CampaignCandidate {
                    model: model("model-b"),
                    source: CampaignCandidateSource::installed(),
                },
            ],
            tasks: BENCHMARK_TASKS
                .iter()
                .take(2)
                .enumerate()
                .map(|(index, task)| {
                    CampaignTaskPlan::for_task(task.id, 100 + index as u64).expect("task plan")
                })
                .collect(),
            repetitions: 2,
            comparison_class: CampaignComparisonClass::ModelComparison,
            execution_profile: CampaignExecutionProfile::Warm,
            execution_environments: vec![CampaignExecutionEnvironment::WindowsNative],
            quality: QualityPreference::Balanced,
            hardware: hardware(),
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            sampling_profile: "temperature-0".to_owned(),
            seed_policy: CampaignSeedPolicy::Fixed(7),
        }
    }

    fn record(model_name: &str, task_id: &str) -> BenchmarkRecord {
        let task = benchmark_task(task_id).expect("task");
        let campaign_plan = plan();
        let task_plan = campaign_plan
            .tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .expect("campaign task plan");
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity: BenchmarkIdentity {
                corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
                task_id: task_id.to_owned(),
                harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
                runtime_harness_version: task_plan.runtime_harness_version.clone(),
                model: model(model_name),
                hardware: hardware(),
                quality: QualityPreference::Balanced,
                workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
                observed_workload: TelemetryValue::Measured(task_plan.workload),
                tool_budget: task_plan.tool_budget.clone(),
                completion_criteria: task
                    .completion_criteria
                    .iter()
                    .map(|criterion| (*criterion).to_owned())
                    .collect(),
                execution: None,
            },
            metrics: BenchmarkMetrics {
                acceptance_success: TelemetryValue::Measured(true),
                completion_success: TelemetryValue::Measured(true),
                model_turns: TelemetryValue::Measured(1),
                tool_calls: TelemetryValue::Measured(1),
                invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
                code_edits: TelemetryValue::Measured(0),
                validation_attempts: TelemetryValue::Measured(1),
                repair_loops: TelemetryValue::Measured(0),
                play_attempts: TelemetryValue::Measured(0),
                frame_capture_attempts: TelemetryValue::Measured(0),
                visual_evaluation_attempts: TelemetryValue::Measured(0),
                human_interventions: TelemetryValue::Measured(0),
                elapsed_ms: TelemetryValue::Measured(10),
                prompt_tokens: TelemetryValue::Measured(10),
                response_tokens: TelemetryValue::Measured(10),
                load_latency_ms: TelemetryValue::Measured(2),
                ttft_ms: TelemetryValue::Unavailable,
                generation_tokens_per_second_milli: TelemetryValue::Measured(2_000),
                peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
                peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
                model_unload_reload_ms: TelemetryValue::Unavailable,
                renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
                oom_failures: TelemetryValue::Measured(0),
            },
        }
    }

    #[test]
    fn all_seven_tasks_map_to_production_harnesses() {
        for task in BENCHMARK_TASKS {
            let harness = campaign_task_harness(task.id).expect("campaign task harness");
            if matches!(
                task.kind,
                BenchmarkTaskKind::RuntimeInteraction | BenchmarkTaskKind::VisualEvaluation
            ) {
                assert_eq!(harness, CampaignTaskHarness::ProductionRuntimeDebug);
                assert_eq!(
                    campaign_task_workload(task.id).expect("workload"),
                    InferenceWorkload::RuntimeObservation
                );
            }
        }
    }

    #[test]
    fn schedule_is_deterministic_interleaved_and_sequential() {
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        let schedule = campaign.plan.schedule().expect("schedule");
        assert_eq!(schedule[0].candidate_index, 0);
        assert_eq!(schedule[1].candidate_index, 1);
        assert_eq!(schedule, campaign.plan.schedule().expect("same schedule"));
        campaign
            .start(&[model("model-a"), model("model-b")])
            .expect("start");
        let request = campaign.begin_next_run().expect("next").expect("request");
        assert!(campaign.begin_next_run().is_err());
        campaign.mark_measurement_started(&request).expect("measure");
        assert!(campaign.begin_next_run().is_err());
    }

    #[test]
    fn pre_measurement_retry_is_bounded_and_measured_failure_is_retained() {
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        let verified = vec![model("model-a"), model("model-b")];
        campaign.start(&verified).expect("start");
        for attempt in 0..=MAX_PRE_MEASUREMENT_RETRIES {
            let request = campaign.begin_next_run().expect("next").expect("request");
            let disposition = campaign
                .record_pre_measurement_failure(&request)
                .expect("pre-measurement failure");
            let expected = if attempt < MAX_PRE_MEASUREMENT_RETRIES {
                PreMeasurementFailureDisposition::Retry
            } else {
                PreMeasurementFailureDisposition::Exhausted
            };
            assert_eq!(disposition, expected);
        }
        let request = campaign.begin_next_run().expect("next").expect("request");
        campaign.mark_measurement_started(&request).expect("started");
        assert!(campaign.record_pre_measurement_failure(&request).is_err());
        campaign
            .fail_measured_run(&request, CampaignFailureKind::ValidationFailure)
            .expect("measured failure retained");
    }

    #[test]
    fn candidate_contract_never_serializes_host_oracle() {
        let fixture = CampaignFixtureIdentity::for_task(BENCHMARK_TASKS[0].id, 42);
        let candidate = fixture
            .candidate_contract(BENCHMARK_TASKS[0].id)
            .expect("candidate contract");
        let oracle = fixture
            .host_evaluator(BENCHMARK_TASKS[0].id)
            .expect("host evaluator");
        let json = serde_json::to_string(&candidate).expect("candidate JSON");
        assert!(!json.contains(&oracle.hidden_token));
        assert_eq!(oracle.task_id, BENCHMARK_TASKS[0].id);
    }

    #[test]
    fn automatic_recording_rejects_identity_mismatch() {
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        campaign
            .start(&[model("model-a"), model("model-b")])
            .expect("start");
        let request = campaign.begin_next_run().expect("next").expect("request");
        campaign.mark_measurement_started(&request).expect("started");
        let mismatched = record("model-b", &request.schedule.task_id);
        assert!(campaign.complete_run(&request, mismatched).is_err());
        assert_eq!(
            campaign.runs[request.schedule.ordinal as usize].status,
            CampaignRunStatus::Rejected
        );
    }

    #[test]
    fn pause_resume_rejects_environment_drift() {
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        campaign
            .start(&[model("model-a"), model("model-b")])
            .expect("start");
        campaign.pause().expect("pause");
        let original = campaign.plan.resume_identity();
        campaign.resume(original.clone()).expect("resume");
        campaign.pause().expect("pause again");
        let mut drift = original;
        drift.hardware.platform = "different".to_owned();
        assert!(campaign.resume(drift).is_err());
        assert_eq!(campaign.state, BenchmarkCampaignState::Incompatible);
    }

    #[test]
    fn campaign_store_replaces_existing_progress_file() {
        let root = tempfile::tempdir().expect("campaign store tempdir");
        let store = BenchmarkCampaignStore::open(root.path().to_path_buf())
            .expect("campaign store");
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        store.save(&campaign).expect("initial save");
        campaign
            .start(&[model("model-a"), model("model-b")])
            .expect("start");
        campaign.pause().expect("pause");
        store.save(&campaign).expect("replace saved progress");
        assert_eq!(
            store
                .load(&campaign.plan_fingerprint)
                .expect("reload")
                .state,
            BenchmarkCampaignState::Paused
        );
    }

    #[test]
    fn campaign_execution_identity_keeps_fixture_exact() {
        let mut campaign = BenchmarkCampaign::new(plan()).expect("campaign");
        campaign
            .start(&[model("model-a"), model("model-b")])
            .expect("start");
        let request = campaign.begin_next_run().expect("next").expect("request");
        campaign.mark_measurement_started(&request).expect("started");
        let evidence = record("model-a", &request.schedule.task_id);
        campaign.complete_run(&request, evidence).expect("complete");
        let execution = campaign.runs[request.schedule.ordinal as usize]
            .record
            .as_ref()
            .and_then(|record| record.identity.execution.as_ref())
            .expect("execution identity");
        assert_eq!(execution.fixture_instance_id, request.fixture.instance_id);
    }
}
