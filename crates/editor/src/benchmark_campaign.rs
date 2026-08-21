//! ADR 0156 benchmark campaign orchestration.
//!
//! A campaign freezes the exact candidate set, per-task fixture plans,
//! repetition policy, and execution environment before any measured run, then
//! drives the ADR 0142 experiment harness through that frozen schedule.
//!
//! Campaign identity is deliberately separate from record identity. Changing a
//! model, runtime, task, or repetition policy produces a new campaign rather
//! than silently reinterpreting evidence that was already recorded under the
//! previous policy.

use crate::agent_benchmark::{
    BENCHMARK_CORPUS_VERSION, BENCHMARK_HARNESS_VERSION, BenchmarkExecutionIdentity,
    BenchmarkHardwareIdentity, BenchmarkLane, BenchmarkRecord, BenchmarkRuntimeIdentity,
    benchmark_task,
};
use crate::agent_benchmark_campaign::{
    CAMPAIGN_HARNESS_VERSION, CAMPAIGN_SCHEDULE_VERSION, CAMPAIGN_SCHEMA_VERSION,
    CampaignCandidate, CampaignComparisonClass, CampaignExecutionEnvironment,
    CampaignExecutionProfile, CampaignRepresentation, CampaignRuntimeIdentity, CampaignTaskPlan,
    CandidateTaskContract, HostOnlyEvaluation,
};
use crate::benchmark_experiment::{
    BENCHMARK_FIXTURE_VERSION, BenchmarkExecutionOrder, BenchmarkExperimentSpec,
    BenchmarkRoutingMode, BenchmarkRunFailureKind, BenchmarkRunOutcome,
};
use crate::managed_local_runtime::{
    MANAGED_BACKEND_ID, ManagedAcquisitionApproval, ManagedAcquisitionCandidate,
    ManagedAcquisitionPlan,
};
use crate::resource_arbitration::{QualityPreference, TelemetryValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Sampling policy frozen into every campaign record.
///
/// Benchmarks compare capability, not sampler luck, so a campaign pins a
/// deterministic sampler instead of inheriting interactive defaults.
pub(crate) const CAMPAIGN_SAMPLING_PROFILE: &str = "temperature-zero-seeded-v1";

/// Seed policy frozen into every campaign record.
pub(crate) const CAMPAIGN_SEED_POLICY: &str = "fixed-model-seed-zero-v1";
const MIN_SUPPORTED_CAMPAIGN_SCHEMA_VERSION: u32 = 2;

/// Why a campaign refused an operation.
///
/// These are the stable codes tests and UI surfaces assert on, so refusal
/// reasons do not depend on prose that may be reworded later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CampaignRejection {
    /// Recorded evidence does not belong to this frozen campaign.
    IdentityMismatch,
    /// Evidence arrived while the campaign was not running.
    NotRunning,
    /// The environment changed while the campaign was paused.
    EnvironmentDrift,
    /// Download & Run approval does not cover this representation.
    UnapprovedCandidate,
    /// The run does not match the next scheduled position.
    OutOfOrder,
    /// The measured model bytes differ from the frozen candidate representation.
    RepresentationDrift,
    /// A passing run did not satisfy the host-owned completion threshold.
    UnmetHostCriteria,
}

impl CampaignRejection {
    /// Returns the stable diagnostic code for this rejection.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::IdentityMismatch => "campaign_identity_mismatch",
            Self::NotRunning => "campaign_not_running",
            Self::EnvironmentDrift => "campaign_environment_drift",
            Self::UnapprovedCandidate => "campaign_unapproved_candidate",
            Self::OutOfOrder => "campaign_out_of_order",
            Self::RepresentationDrift => "campaign_representation_drift",
            Self::UnmetHostCriteria => "campaign_unmet_host_criteria",
        }
    }

    /// Returns a user-facing explanation for this rejection.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::IdentityMismatch => "evidence identity does not match the frozen campaign",
            Self::NotRunning => "campaign is not running",
            Self::EnvironmentDrift => "execution environment changed while the campaign was paused",
            Self::UnapprovedCandidate => {
                "Download & Run approval does not cover this model representation"
            }
            Self::OutOfOrder => "run does not match the next scheduled campaign position",
            Self::RepresentationDrift => {
                "measured model bytes differ from the frozen candidate representation"
            }
            Self::UnmetHostCriteria => {
                "run passed the candidate contract but not the host-owned completion threshold"
            }
        }
    }
}

/// Draft campaign policy, editable until [`CampaignPolicy::freeze`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignPolicy {
    /// Machine-local campaign identifier.
    pub(crate) campaign_id: String,
    /// Exact GameEngine commit the campaign measures.
    pub(crate) engine_commit_head: String,
    /// Whether this compares models or characterizes runtimes.
    pub(crate) comparison_class: CampaignComparisonClass,
    /// Whether startup cost is included in the measured window.
    pub(crate) execution_profile: CampaignExecutionProfile,
    /// The single frozen execution environment for this campaign.
    pub(crate) execution_environment: CampaignExecutionEnvironment,
    /// Backend runtime version string frozen into every record.
    pub(crate) backend_runtime_version: String,
    /// Hardware identity captured before the campaign freezes.
    pub(crate) hardware: BenchmarkHardwareIdentity,
    /// Quality policy applied by the real inference harness.
    pub(crate) quality: QualityPreference,
    /// Finite wall-clock budget applied to every scheduled run.
    pub(crate) run_timeout_seconds: u64,
    /// Exact candidate set.
    pub(crate) candidates: Vec<CampaignCandidate>,
    /// ADR 0142 task identifiers to run.
    pub(crate) task_ids: Vec<String>,
    /// Repetitions per candidate and task.
    pub(crate) repetitions: u32,
    /// Host-owned fixture seed. Never exposed to a candidate.
    pub(crate) host_seed: u64,
}

impl CampaignPolicy {
    /// Freezes this policy into an immutable [`CampaignPlan`].
    ///
    /// Freezing consumes the policy, which is how the ADR's "immutable after
    /// measured execution starts" rule is enforced by the type system rather
    /// than by a runtime flag a caller could forget to check.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy is incomplete, when a candidate's
    /// physical representation is not exactly measured, or when a task
    /// identifier is unknown or duplicated.
    pub(crate) fn freeze(self) -> Result<CampaignPlan, String> {
        if self.campaign_id.trim().is_empty() {
            return Err("campaign id must be non-empty".to_owned());
        }
        if !is_full_git_sha(&self.engine_commit_head) {
            return Err("campaign requires an exact 40-character GameEngine commit SHA".to_owned());
        }
        if self.backend_runtime_version.trim().is_empty() {
            return Err("campaign requires an exact backend runtime version".to_owned());
        }
        if self.candidates.is_empty() {
            return Err("campaign requires at least one candidate".to_owned());
        }
        if self.repetitions == 0 {
            return Err("campaign repetition count must be at least one".to_owned());
        }
        if self.run_timeout_seconds == 0 {
            return Err("campaign run timeout must be greater than zero".to_owned());
        }
        if self.quality == QualityPreference::Auto {
            return Err(
                "campaign quality must be an explicit Fast, Balanced, or Deep policy".to_owned(),
            );
        }
        if self.task_ids.is_empty() {
            return Err("campaign requires at least one task".to_owned());
        }

        let mut seen_models = BTreeSet::new();
        for candidate in &self.candidates {
            if !candidate.representation.exact() {
                return Err(
                    "campaign candidates require an exactly measured physical representation"
                        .to_owned(),
                );
            }
            if !seen_models.insert(candidate.representation.model_id.clone()) {
                return Err("campaign candidate model ids must be unique".to_owned());
            }
        }

        let mut seen_tasks = BTreeSet::new();
        let mut task_plans = Vec::with_capacity(self.task_ids.len());
        for task_id in &self.task_ids {
            if benchmark_task(task_id).is_none() {
                return Err(format!("unknown benchmark task `{task_id}`"));
            }
            if !seen_tasks.insert(task_id.clone()) {
                return Err(format!("benchmark task `{task_id}` is duplicated"));
            }
            task_plans.push(CampaignTaskPlan::for_task(task_id, self.host_seed)?);
        }

        let mut plan = CampaignPlan {
            schema_version: CAMPAIGN_SCHEMA_VERSION,
            campaign_id: self.campaign_id,
            engine_commit_head: self.engine_commit_head,
            comparison_class: self.comparison_class,
            execution_profile: self.execution_profile,
            execution_environment: self.execution_environment,
            backend_runtime_version: self.backend_runtime_version,
            hardware: self.hardware,
            quality: self.quality,
            run_timeout_seconds: self.run_timeout_seconds,
            candidates: self.candidates,
            task_plans,
            benchmark_runtime: None,
            repetitions: self.repetitions,
            harness_version: CAMPAIGN_HARNESS_VERSION.to_owned(),
            schedule_version: CAMPAIGN_SCHEDULE_VERSION.to_owned(),
            sampling_profile: CAMPAIGN_SAMPLING_PROFILE.to_owned(),
            seed_policy: CAMPAIGN_SEED_POLICY.to_owned(),
            host_seed: self.host_seed,
            plan_digest: String::new(),
        };
        plan.plan_digest = plan.compute_digest();
        Ok(plan)
    }
}

/// One frozen campaign plan.
///
/// Every equivalence-relevant field participates in
/// [`CampaignPlan::plan_digest`], so any policy change yields a different
/// campaign identity instead of extending the previous one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignPlan {
    schema_version: u32,
    /// Machine-local campaign identifier.
    pub(crate) campaign_id: String,
    /// Exact GameEngine commit the campaign measures.
    pub(crate) engine_commit_head: String,
    /// Whether this compares models or characterizes runtimes.
    pub(crate) comparison_class: CampaignComparisonClass,
    /// Whether startup cost is included in the measured window.
    pub(crate) execution_profile: CampaignExecutionProfile,
    /// The single frozen execution environment.
    pub(crate) execution_environment: CampaignExecutionEnvironment,
    /// Backend runtime version frozen into every record.
    pub(crate) backend_runtime_version: String,
    /// Hardware identity frozen for resume and comparison checks.
    pub(crate) hardware: BenchmarkHardwareIdentity,
    /// Quality policy frozen into every child run.
    pub(crate) quality: QualityPreference,
    /// Finite wall-clock budget for one scheduled run.
    pub(crate) run_timeout_seconds: u64,
    /// Exact candidate set in frozen order.
    pub(crate) candidates: Vec<CampaignCandidate>,
    /// Per-task plans in frozen order.
    pub(crate) task_plans: Vec<CampaignTaskPlan>,
    /// Optional schema-v4 benchmark lane/runtime identity frozen for this campaign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) benchmark_runtime: Option<BenchmarkRuntimeIdentity>,
    /// Repetitions per candidate and task.
    pub(crate) repetitions: u32,
    /// Campaign harness version.
    pub(crate) harness_version: String,
    /// Schedule policy version.
    pub(crate) schedule_version: String,
    /// Sampling profile frozen into every record.
    pub(crate) sampling_profile: String,
    /// Seed policy frozen into every record.
    pub(crate) seed_policy: String,
    host_seed: u64,
    plan_digest: String,
}

impl CampaignPlan {
    /// Returns the identity digest for this frozen plan.
    pub(crate) fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    /// Derives a new frozen plan with one explicit benchmark lane/runtime identity.
    ///
    /// This must be called before [`CampaignRun::prepare`]. The runtime identity
    /// becomes part of the plan digest and every derived execution identity.
    #[allow(dead_code)]
    pub(crate) fn with_benchmark_runtime(
        mut self,
        runtime: BenchmarkRuntimeIdentity,
    ) -> Result<Self, String> {
        if runtime.lane == BenchmarkLane::RawModel {
            return Err(
                "ADR0156 task campaigns execute through agent/production harnesses and cannot be registered as raw_model"
                    .to_owned(),
            );
        }
        self.schema_version = CAMPAIGN_SCHEMA_VERSION;
        let runtime_harness_version = runtime.harness.harness_version.clone();
        for task_plan in &mut self.task_plans {
            task_plan.runtime_harness_version = runtime_harness_version.clone();
        }
        self.benchmark_runtime = Some(runtime);
        self.plan_digest = self.compute_digest();
        Ok(self)
    }

    /// Returns the benchmark lane/runtime identity frozen into this campaign.
    #[allow(dead_code)]
    pub(crate) fn benchmark_runtime(&self) -> Option<&BenchmarkRuntimeIdentity> {
        self.benchmark_runtime.as_ref()
    }

    fn compute_digest(&self) -> String {
        let mut hash = FNV_OFFSET;
        hash = mix(hash, self.campaign_id.as_bytes());
        hash = mix(hash, self.engine_commit_head.as_bytes());
        hash = mix(hash, self.comparison_class.label().as_bytes());
        hash = mix(hash, self.execution_profile.label().as_bytes());
        hash = mix(hash, self.execution_environment.label().as_bytes());
        hash = mix(hash, self.backend_runtime_version.as_bytes());
        hash = mix(
            hash,
            serde_json::to_string(&self.hardware)
                .unwrap_or_default()
                .as_bytes(),
        );
        hash = mix(hash, self.quality.label().as_bytes());
        hash = mix(hash, &self.run_timeout_seconds.to_le_bytes());
        hash = mix(hash, self.harness_version.as_bytes());
        hash = mix(hash, self.schedule_version.as_bytes());
        hash = mix(hash, self.sampling_profile.as_bytes());
        hash = mix(hash, self.seed_policy.as_bytes());
        if let Some(runtime) = self.benchmark_runtime.as_ref() {
            hash = mix(hash, serde_json::to_string(runtime).unwrap_or_default().as_bytes());
        }
        for candidate in &self.candidates {
            hash = mix(hash, candidate.representation.backend_id.as_bytes());
            hash = mix(hash, candidate.representation.model_id.as_bytes());
            hash = mix(hash, candidate.representation.model_version.as_bytes());
            hash = mix(hash, candidate.representation.quantization.as_bytes());
            hash = mix(
                hash,
                &candidate
                    .representation
                    .representation_size_bytes
                    .to_le_bytes(),
            );
        }
        for plan in &self.task_plans {
            hash = mix(hash, plan.task_id.as_bytes());
            hash = mix(hash, plan.fixture.instance_id.as_bytes());
            hash = mix(hash, plan.fixture.fixture_version.as_bytes());
            hash = mix(hash, plan.runtime_harness_version.as_bytes());
        }
        hash = mix(hash, &self.repetitions.to_le_bytes());
        hash = mix(hash, &self.host_seed.to_le_bytes());
        format!("{hash:016x}")
    }

    /// Returns the runtime identity this campaign froze.
    ///
    /// Runtime identity is reported separately from model identity so a
    /// Windows-native versus WSL2 difference is never read as a model result.
    pub(crate) fn runtime_identity(&self) -> CampaignRuntimeIdentity {
        CampaignRuntimeIdentity {
            execution_environment: self.execution_environment,
            backend_runtime_version: self.backend_runtime_version.clone(),
        }
    }

    /// Returns the deterministic execution schedule for this plan.
    ///
    /// The order is stable for the same plan: tasks in frozen order, then
    /// repetitions, then candidates in frozen order for thermal interleaving.
    pub(crate) fn schedule(&self) -> Vec<CampaignScheduledRun> {
        let mut runs = Vec::new();
        for task_plan in &self.task_plans {
            for repetition in 0..self.repetitions {
                for candidate in &self.candidates {
                    runs.push(CampaignScheduledRun {
                        ordinal: runs.len() as u64,
                        model_id: candidate.representation.model_id.clone(),
                        task_id: task_plan.task_id.clone(),
                        repetition,
                    });
                }
            }
        }
        runs
    }

    /// Returns the execution identity stamped into records from this campaign.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is not part of this frozen plan.
    pub(crate) fn execution_identity(
        &self,
        task_id: &str,
    ) -> Result<BenchmarkExecutionIdentity, String> {
        let task_plan = self.task_plan(task_id)?;
        Ok(BenchmarkExecutionIdentity {
            campaign_harness_version: self.harness_version.clone(),
            schedule_policy_version: self.schedule_version.clone(),
            comparison_class: self.comparison_class.label().to_owned(),
            execution_profile: self.execution_profile.label().to_owned(),
            execution_environment: self.execution_environment.label().to_owned(),
            fixture_id: task_plan.fixture.fixture_id.clone(),
            fixture_version: task_plan.fixture.fixture_version.clone(),
            fixture_instance_id: task_plan.fixture.instance_id.clone(),
            sampling_profile: self.sampling_profile.clone(),
            seed_policy: self.seed_policy.clone(),
            benchmark_runtime: self.benchmark_runtime.clone(),
        })
    }

    /// Returns the candidate-visible contract for one task.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is not part of this frozen plan.
    pub(crate) fn candidate_contract(
        &self,
        task_id: &str,
    ) -> Result<CandidateTaskContract, String> {
        self.task_plan(task_id)?.fixture.candidate_contract(task_id)
    }

    /// Returns the host-owned evaluation material for one task.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is not part of this frozen plan.
    pub(crate) fn host_only_evaluation(&self, task_id: &str) -> Result<HostOnlyEvaluation, String> {
        self.task_plan(task_id)?
            .fixture
            .host_only_evaluation(task_id)
    }

    fn task_plan(&self, task_id: &str) -> Result<&CampaignTaskPlan, String> {
        self.task_plans
            .iter()
            .find(|plan| plan.task_id == task_id)
            .ok_or_else(|| format!("task `{task_id}` is not part of this campaign"))
    }

    /// Builds the Download & Run plan for exactly the candidates that are missing.
    ///
    /// Returns `None` when every candidate is already installed, so preflight
    /// never asks for an approval it does not need.
    pub(crate) fn acquisition_plan(
        &self,
        installed_model_ids: &BTreeSet<String>,
    ) -> Option<ManagedAcquisitionPlan> {
        let candidates: Vec<ManagedAcquisitionCandidate> = self
            .candidates
            .iter()
            .filter(|candidate| !installed_model_ids.contains(&candidate.representation.model_id))
            .filter_map(|candidate| {
                let source = candidate.source.source_reference.clone()?;
                let expected_sha256 = candidate.source.expected_sha256.clone()?;
                Some(ManagedAcquisitionCandidate {
                    candidate_id: candidate.representation.model_id.clone(),
                    source,
                    representation: format!(
                        "{} {}",
                        candidate.representation.model_version,
                        candidate.representation.quantization
                    ),
                    license: candidate.source.license.clone(),
                    expected_sha256,
                    transfer_bytes: candidate.source.transfer_size_bytes.unwrap_or_default(),
                    storage_bytes: candidate.source.storage_size_bytes.unwrap_or_default(),
                })
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some(ManagedAcquisitionPlan {
            plan_id: format!("campaign-{}-{}", self.campaign_id, self.plan_digest),
            candidates,
        })
    }

    /// Bridges this frozen plan onto the ADR 0142 experiment harness.
    ///
    /// # Errors
    ///
    /// Returns an error when the derived specification is not internally valid.
    pub(crate) fn experiment_spec(
        &self,
        output_destination: PathBuf,
    ) -> Result<BenchmarkExperimentSpec, String> {
        let backend_id = self
            .candidates
            .first()
            .map(|candidate| candidate.representation.backend_id.clone())
            .ok_or_else(|| "campaign requires at least one candidate".to_owned())?;
        let managed_execution_environment = if backend_id == MANAGED_BACKEND_ID {
            Some(self.execution_environment.managed_environment().ok_or_else(|| {
                "GameEngine-managed campaigns require Windows native or WSL2 Linux execution"
                    .to_owned()
            })?)
        } else {
            if self.execution_environment != CampaignExecutionEnvironment::CompatibleBackend {
                return Err(
                    "Windows native / WSL2 campaign identity requires the GameEngine-managed backend"
                        .to_owned(),
                );
            }
            None
        };
        let spec = BenchmarkExperimentSpec {
            schema_version: 1,
            experiment_id: format!("{}-{}", self.campaign_id, self.plan_digest),
            engine_commit_head: self.engine_commit_head.clone(),
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
            backend_id,
            managed_execution_environment,
            model_ids: self
                .candidates
                .iter()
                .map(|candidate| candidate.representation.model_id.clone())
                .collect(),
            task_ids: self
                .task_plans
                .iter()
                .map(|plan| plan.task_id.clone())
                .collect(),
            repeat_count: self.repetitions,
            // Pinned rather than `Auto`: an adaptive policy could pick a
            // different posture per run, so a model would be compared against
            // whatever the harness happened to choose that time.
            quality: self.quality,
            routing_mode: BenchmarkRoutingMode::SingleModel,
            execution_order: BenchmarkExecutionOrder::TaskRepeatCandidateInterleaved,
            stop_on_failure: false,
            output_destination,
            execution_identity_by_task: self
                .task_plans
                .iter()
                .map(|plan| {
                    self.execution_identity(&plan.task_id)
                        .map(|identity| (plan.task_id.clone(), identity))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?,
            candidate_contract_by_task: self
                .task_plans
                .iter()
                .map(|plan| {
                    self.candidate_contract(&plan.task_id)
                        .map(|contract| (plan.task_id.clone(), contract))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Compares this plan against a prior campaign for baseline reuse.
    ///
    /// Adding a model must not force every historical model to run again, but a
    /// changed equivalence dimension must not be papered over either.
    pub(crate) fn baseline_reuse(&self, prior: &CampaignPlan) -> CampaignBaselineReuse {
        let mut changed = Vec::new();
        if self.engine_commit_head != prior.engine_commit_head {
            changed.push("engine_commit_head");
        }
        if self.comparison_class != prior.comparison_class {
            changed.push("comparison_class");
        }
        if self.execution_profile != prior.execution_profile {
            changed.push("execution_profile");
        }
        if self.execution_environment != prior.execution_environment {
            changed.push("execution_environment");
        }
        if self.backend_runtime_version != prior.backend_runtime_version {
            changed.push("backend_runtime_version");
        }
        if self.benchmark_runtime != prior.benchmark_runtime {
            changed.push("benchmark_runtime");
        }
        if self.hardware != prior.hardware {
            changed.push("hardware");
        }
        if self.quality != prior.quality {
            changed.push("quality");
        }
        if self.run_timeout_seconds != prior.run_timeout_seconds {
            changed.push("run_timeout");
        }
        if self.harness_version != prior.harness_version
            || self.schedule_version != prior.schedule_version
            || self.sampling_profile != prior.sampling_profile
            || self.seed_policy != prior.seed_policy
        {
            changed.push("harness_policy");
        }
        if self.repetitions != prior.repetitions {
            changed.push("repetitions");
        }
        let prior_fixtures: BTreeMap<&str, &str> = prior
            .task_plans
            .iter()
            .map(|plan| (plan.task_id.as_str(), plan.fixture.instance_id.as_str()))
            .collect();
        if self.task_plans.iter().any(|plan| {
            prior_fixtures
                .get(plan.task_id.as_str())
                .is_some_and(|instance_id| *instance_id != plan.fixture.instance_id)
        }) {
            changed.push("fixture_instance");
        }
        if changed.is_empty() {
            let model_ids = prior
                .candidates
                .iter()
                .map(|candidate| candidate.representation.model_id.clone())
                .filter(|model_id| {
                    self.candidates
                        .iter()
                        .any(|candidate| &candidate.representation.model_id == model_id)
                })
                .collect();
            CampaignBaselineReuse::Reusable { model_ids }
        } else {
            CampaignBaselineReuse::RequiresBaselineRerun { changed }
        }
    }
}

/// Whether a new campaign can reuse a prior campaign's baseline evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CampaignBaselineReuse {
    /// Comparable: the listed models keep their prior evidence.
    Reusable {
        /// Models whose prior evidence stays comparable.
        model_ids: Vec<String>,
    },
    /// Not comparable: the listed equivalence dimensions changed.
    RequiresBaselineRerun {
        /// Equivalence dimensions that changed.
        changed: Vec<&'static str>,
    },
}

/// One scheduled position in a frozen campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignScheduledRun {
    /// Zero-based position in the deterministic schedule.
    pub(crate) ordinal: u64,
    /// Candidate model for this position.
    pub(crate) model_id: String,
    /// ADR 0142 task for this position.
    pub(crate) task_id: String,
    /// Zero-based repetition index.
    pub(crate) repetition: u32,
}

/// Lifecycle state of a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CampaignState {
    /// Frozen but not started. No evidence is recorded in this state.
    Planned,
    /// Started and recording evidence.
    Running,
    /// Suspended; prior evidence is retained.
    Paused,
    /// Every scheduled position produced evidence.
    Completed,
}

/// Evidence for one executed schedule position.
///
/// Repetitions are stored individually: collapsing them into an average would
/// destroy the distribution the ADR requires before a ranking decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignEvidence {
    /// The schedule position this evidence belongs to.
    pub(crate) scheduled: CampaignScheduledRun,
    /// Measured outcome.
    pub(crate) outcome: BenchmarkRunOutcome,
    /// Failure classification when the run did not pass.
    pub(crate) failure_kind: Option<BenchmarkRunFailureKind>,
    /// Attempt index reserved for a future process-connected retry protocol.
    ///
    /// The current coordinator never retries: every emitted result advances the
    /// same schedule in both the process layer and this state machine.
    pub(crate) attempt: u32,
    /// The ADR 0142 record, when the run produced measured evidence.
    pub(crate) record: Option<BenchmarkRecord>,
}

/// An observation of the live execution environment, used to detect drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CampaignEnvironmentProbe {
    /// Execution environment currently available.
    pub(crate) execution_environment: CampaignExecutionEnvironment,
    /// Backend runtime version currently available.
    pub(crate) backend_runtime_version: String,
    /// GameEngine commit currently loaded.
    pub(crate) engine_commit_head: String,
    /// Current hardware identity, including truthful unavailable telemetry.
    pub(crate) hardware: BenchmarkHardwareIdentity,
    /// Exact model representations currently available for the frozen set.
    pub(crate) representations: Vec<CampaignRepresentation>,
}

/// A started campaign that owns the frozen plan and its accumulated evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CampaignRun {
    plan: CampaignPlan,
    state: CampaignState,
    #[serde(skip)]
    approval: Option<ManagedAcquisitionApproval>,
    evidence: Vec<CampaignEvidence>,
    next_ordinal: u64,
}

impl CampaignRun {
    /// Prepares a frozen plan for execution without recording anything.
    ///
    /// Campaign start is the explicit recording action, so a prepared campaign
    /// stays in [`CampaignState::Planned`] until [`CampaignRun::start`].
    pub(crate) fn prepare(plan: CampaignPlan) -> Self {
        Self {
            plan,
            state: CampaignState::Planned,
            approval: None,
            evidence: Vec::new(),
            next_ordinal: 0,
        }
    }

    /// Returns the frozen plan.
    pub(crate) fn plan(&self) -> &CampaignPlan {
        &self.plan
    }

    /// Returns the current lifecycle state.
    pub(crate) fn state(&self) -> CampaignState {
        self.state
    }

    /// Returns every recorded evidence entry in schedule order.
    pub(crate) fn evidence(&self) -> &[CampaignEvidence] {
        &self.evidence
    }

    /// Returns the first schedule ordinal not yet represented by evidence.
    pub(crate) fn next_ordinal(&self) -> u64 {
        self.next_ordinal
    }

    /// Returns whether an approved acquisition is held for this campaign.
    pub(crate) fn has_acquisition_approval(&self) -> bool {
        self.approval.is_some()
    }

    /// Starts the campaign, consuming the Download & Run approval.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignRejection::UnapprovedCandidate`] when the approval does
    /// not cover every candidate that preflight reported as missing.
    pub(crate) fn start(
        &mut self,
        approval: Option<ManagedAcquisitionApproval>,
        installed_model_ids: &BTreeSet<String>,
    ) -> Result<(), CampaignRejection> {
        if let Some(acquisition) = self.plan.acquisition_plan(installed_model_ids) {
            let approval = approval
                .as_ref()
                .ok_or(CampaignRejection::UnapprovedCandidate)?;
            for candidate in &acquisition.candidates {
                approval
                    .authorizes(&acquisition.plan_id, &candidate.candidate_id)
                    .map_err(|_| CampaignRejection::UnapprovedCandidate)?;
            }
        }
        self.approval = approval;
        self.state = CampaignState::Running;
        Ok(())
    }

    /// Returns the next schedule position, or `None` when nothing is pending.
    ///
    /// Only one position is ever outstanding, which is how the ADR default of
    /// one measured local run at a time is enforced structurally rather than by
    /// convention in the caller.
    pub(crate) fn next_scheduled(&self) -> Option<CampaignScheduledRun> {
        if self.state != CampaignState::Running {
            return None;
        }
        self.plan
            .schedule()
            .into_iter()
            .find(|run| run.ordinal == self.next_ordinal)
    }

    /// Records evidence for the next scheduled position.
    ///
    /// Every emitted result advances the schedule because the process
    /// coordinator also advances after emitting it. Measured failures remain
    /// evidence, while pre-measurement failures remain explicit unavailable
    /// evidence instead of entering an unconnected UI-only retry state.
    ///
    /// # Errors
    ///
    /// Returns a rejection when the campaign is not running, when the evidence
    /// does not match the next scheduled position, or when a carried record does
    /// not have this campaign's execution identity.
    pub(crate) fn record(&mut self, evidence: CampaignEvidence) -> Result<(), CampaignRejection> {
        if self.state != CampaignState::Running {
            return Err(CampaignRejection::NotRunning);
        }
        let expected = self.next_scheduled().ok_or(CampaignRejection::OutOfOrder)?;
        if evidence.scheduled != expected {
            return Err(CampaignRejection::OutOfOrder);
        }
        if evidence.outcome == BenchmarkRunOutcome::Passed && evidence.record.is_none() {
            return Err(CampaignRejection::UnmetHostCriteria);
        }
        if let Some(record) = evidence.record.as_ref() {
            let expected_identity = self
                .plan
                .execution_identity(&evidence.scheduled.task_id)
                .map_err(|_| CampaignRejection::IdentityMismatch)?;
            if record.identity.execution.as_ref() != Some(&expected_identity) {
                return Err(CampaignRejection::IdentityMismatch);
            }
            if record.identity.runtime != self.plan.benchmark_runtime {
                return Err(CampaignRejection::IdentityMismatch);
            }
            let expected_task_plan = self
                .plan
                .task_plan(&evidence.scheduled.task_id)
                .map_err(|_| CampaignRejection::IdentityMismatch)?;
            if record.identity.runtime_harness_version
                != expected_task_plan.runtime_harness_version
            {
                return Err(CampaignRejection::IdentityMismatch);
            }
            if record.identity.model.model_id != evidence.scheduled.model_id
                || record.identity.task_id != evidence.scheduled.task_id
            {
                return Err(CampaignRejection::IdentityMismatch);
            }
            // The record must describe the same physical bytes the campaign
            // froze. A model re-pulled at a new digest mid-campaign would
            // otherwise contribute evidence under the old candidate's name.
            let frozen = self
                .plan
                .candidates
                .iter()
                .find(|candidate| candidate.representation.model_id == evidence.scheduled.model_id)
                .ok_or(CampaignRejection::IdentityMismatch)?;
            if CampaignRepresentation::from_model(&record.identity.model) != frozen.representation {
                return Err(CampaignRejection::RepresentationDrift);
            }
            // Scoring is host-owned. The candidate sees neither these checks nor
            // their thresholds; admission derives them from host-recorded
            // metrics rather than trusting the model's completion statement.
            let evaluation = self
                .plan
                .host_only_evaluation(&evidence.scheduled.task_id)
                .map_err(|_| CampaignRejection::IdentityMismatch)?;
            if evidence.outcome == BenchmarkRunOutcome::Passed && !evaluation.passes(record) {
                return Err(CampaignRejection::UnmetHostCriteria);
            }
        }

        self.evidence.push(evidence);
        self.next_ordinal += 1;
        if self.next_ordinal as usize >= self.plan.schedule().len() {
            self.state = CampaignState::Completed;
        }
        Ok(())
    }

    /// Suspends the campaign, retaining all prior evidence.
    pub(crate) fn pause(&mut self) {
        if self.state == CampaignState::Running {
            self.state = CampaignState::Paused;
        }
    }

    /// Resumes a paused campaign after confirming the environment did not drift.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignRejection::EnvironmentDrift`] when the probe does not
    /// match the frozen plan, and [`CampaignRejection::NotRunning`] when the
    /// campaign was not paused.
    pub(crate) fn resume(
        &mut self,
        probe: &CampaignEnvironmentProbe,
    ) -> Result<(), CampaignRejection> {
        if self.state != CampaignState::Paused {
            return Err(CampaignRejection::NotRunning);
        }
        let frozen_representations = self
            .plan
            .candidates
            .iter()
            .map(|candidate| candidate.representation.clone())
            .collect::<BTreeSet<_>>();
        let current_representations = probe
            .representations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let current_contract = (MIN_SUPPORTED_CAMPAIGN_SCHEMA_VERSION
            ..=CAMPAIGN_SCHEMA_VERSION)
            .contains(&self.plan.schema_version)
            && (self.plan.schema_version >= 3 || self.plan.benchmark_runtime.is_none())
            && self.plan.plan_digest == self.plan.compute_digest()
            && self.plan.harness_version == CAMPAIGN_HARNESS_VERSION
            && self.plan.schedule_version == CAMPAIGN_SCHEDULE_VERSION
            && self.plan.sampling_profile == CAMPAIGN_SAMPLING_PROFILE
            && self.plan.seed_policy == CAMPAIGN_SEED_POLICY
            && self.plan.task_plans.iter().all(|task| {
                task.fixture.fixture_version
                    == crate::agent_benchmark_campaign::CAMPAIGN_FIXTURE_VERSION
            });
        if !current_contract
            || probe.execution_environment != self.plan.execution_environment
            || probe.backend_runtime_version != self.plan.backend_runtime_version
            || probe.engine_commit_head != self.plan.engine_commit_head
            || probe.hardware != self.plan.hardware
            || current_representations != frozen_representations
        {
            return Err(CampaignRejection::EnvironmentDrift);
        }
        self.state = CampaignState::Running;
        Ok(())
    }

    /// Builds the aggregate report for the evidence recorded so far.
    pub(crate) fn report(&self) -> CampaignReport {
        let mut models: BTreeMap<String, CampaignModelReport> = self
            .plan
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.representation.model_id.clone(),
                    CampaignModelReport {
                        model_id: candidate.representation.model_id.clone(),
                        planned_runs: 0,
                        recorded_runs: 0,
                        task_successes: 0,
                        measured_failures: 0,
                        unavailable_runs: 0,
                        aggregate_elapsed_ms: None,
                        evidence_complete: false,
                        measured_elapsed_ms: 0,
                        elapsed_fully_measured: true,
                    },
                )
            })
            .collect();
        for run in self.plan.schedule() {
            if let Some(model) = models.get_mut(&run.model_id) {
                model.planned_runs += 1;
            }
        }
        for evidence in &self.evidence {
            let Some(model) = models.get_mut(&evidence.scheduled.model_id) else {
                continue;
            };
            model.recorded_runs += 1;
            match evidence.outcome {
                BenchmarkRunOutcome::Passed => model.task_successes += 1,
                BenchmarkRunOutcome::Failed => model.measured_failures += 1,
                BenchmarkRunOutcome::Unavailable | BenchmarkRunOutcome::Interrupted => {
                    model.unavailable_runs += 1;
                }
            }
            match evidence
                .record
                .as_ref()
                .map(|record| &record.metrics.elapsed_ms)
            {
                Some(TelemetryValue::Measured(elapsed)) => {
                    model.measured_elapsed_ms = model.measured_elapsed_ms.saturating_add(*elapsed);
                }
                // One unmeasured run makes the model's total unknowable. Summing
                // only the runs that happened to report would understate the
                // aggregate while looking like a complete measurement.
                Some(TelemetryValue::ConservativeEstimate(_))
                | Some(TelemetryValue::Unavailable)
                | None => {
                    model.elapsed_fully_measured = false;
                }
            }
        }
        for model in models.values_mut() {
            model.evidence_complete =
                model.planned_runs > 0 && model.recorded_runs == model.planned_runs;
            model.aggregate_elapsed_ms = (model.recorded_runs > 0 && model.elapsed_fully_measured)
                .then_some(model.measured_elapsed_ms);
        }
        let models: Vec<CampaignModelReport> = models.into_values().collect();
        let evidence_complete =
            !models.is_empty() && models.iter().all(|model| model.evidence_complete);
        CampaignReport {
            campaign_id: self.plan.campaign_id.clone(),
            plan_digest: self.plan.plan_digest.clone(),
            comparison_class: self.plan.comparison_class,
            execution_profile: self.plan.execution_profile,
            execution_environment: self.plan.execution_environment,
            quality: self.plan.quality,
            run_timeout_seconds: self.plan.run_timeout_seconds,
            state: self.state,
            models,
            evidence_complete,
        }
    }

    /// Returns records that may be consumed by ADR 0150 model routing.
    ///
    /// Only a model comparison with complete evidence qualifies. Runtime
    /// characterization measures the platform, not the model, so feeding it to
    /// routing would let a WSL2 result recommend a model.
    pub(crate) fn qualified_records(&self) -> Vec<&BenchmarkRecord> {
        if self.plan.comparison_class != CampaignComparisonClass::ModelComparison {
            return Vec::new();
        }
        if matches!(
            self.plan.benchmark_runtime.as_ref().map(|runtime| runtime.lane),
            Some(BenchmarkLane::AgentHarness) | Some(BenchmarkLane::CodingAgent)
        ) {
            return Vec::new();
        }
        if !self.report().evidence_complete {
            return Vec::new();
        }
        self.evidence
            .iter()
            .filter(|evidence| evidence.outcome == BenchmarkRunOutcome::Passed)
            .filter_map(|evidence| evidence.record.as_ref())
            .collect()
    }
}

/// Per-model aggregate for one campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignModelReport {
    /// Candidate model.
    pub(crate) model_id: String,
    /// Scheduled positions for this model.
    pub(crate) planned_runs: usize,
    /// Positions that produced evidence.
    pub(crate) recorded_runs: usize,
    /// Positions where the model completed the task.
    pub(crate) task_successes: usize,
    /// Positions where the model measurably failed the task.
    pub(crate) measured_failures: usize,
    /// Positions where no measurement was possible.
    pub(crate) unavailable_runs: usize,
    /// Aggregate elapsed time, or `None` when the telemetry was not measured.
    ///
    /// This stays `None` rather than zero: reporting unmeasured throughput as a
    /// number would make an unavailable metric look like a fast one.
    pub(crate) aggregate_elapsed_ms: Option<u64>,
    /// Whether every scheduled position for this model produced evidence.
    pub(crate) evidence_complete: bool,
    #[serde(skip)]
    measured_elapsed_ms: u64,
    #[serde(skip)]
    elapsed_fully_measured: bool,
}

/// Aggregate campaign report.
///
/// Task success is reported before throughput: a model that fails the task
/// quickly is not a better candidate than one that succeeds slowly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignReport {
    /// Campaign this report summarizes.
    pub(crate) campaign_id: String,
    /// Frozen plan digest the evidence was recorded under.
    pub(crate) plan_digest: String,
    /// Whether this compares models or characterizes runtimes.
    pub(crate) comparison_class: CampaignComparisonClass,
    /// Whether startup cost is inside the measured window.
    pub(crate) execution_profile: CampaignExecutionProfile,
    /// The single frozen execution environment.
    pub(crate) execution_environment: CampaignExecutionEnvironment,
    /// Frozen inference quality used for every model turn.
    pub(crate) quality: QualityPreference,
    /// Frozen timeout applied to each scheduled run.
    pub(crate) run_timeout_seconds: u64,
    /// Lifecycle state at the time the report was produced.
    pub(crate) state: CampaignState,
    /// Per-model aggregates in stable model order.
    pub(crate) models: Vec<CampaignModelReport>,
    /// Whether every scheduled position produced evidence.
    pub(crate) evidence_complete: bool,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn mix(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_benchmark::{
        BENCHMARK_SCHEMA_VERSION, BENCHMARK_TASKS, BenchmarkAgentRuntimeIdentity,
        BenchmarkHardwareIdentity, BenchmarkHarnessIdentity, BenchmarkIdentity, BenchmarkLane,
        BenchmarkMetrics, BenchmarkModelIdentity, BenchmarkRuntimeIdentity, BenchmarkToolBudget,
    };
    use crate::agent_benchmark_campaign::{
        CampaignCandidateSource, CampaignTaskHarness, campaign_task_harness,
    };
    use crate::resource_arbitration::InferenceWorkload;

    const ENGINE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn representation(model_id: &str) -> CampaignRepresentation {
        CampaignRepresentation {
            backend_id: "ollama-compatible".to_owned(),
            model_id: model_id.to_owned(),
            model_version: format!("{model_id}-digest"),
            quantization: "q4".to_owned(),
            representation_size_bytes: 1_000,
        }
    }

    fn installed_candidate(model_id: &str) -> CampaignCandidate {
        CampaignCandidate {
            representation: representation(model_id),
            source: CampaignCandidateSource::installed(),
        }
    }

    fn acquirable_candidate(model_id: &str) -> CampaignCandidate {
        CampaignCandidate {
            representation: representation(model_id),
            source: CampaignCandidateSource {
                source_reference: Some(format!("https://example.invalid/{model_id}.gguf")),
                license: Some("test-license".to_owned()),
                expected_sha256: Some("a".repeat(64)),
                transfer_size_bytes: Some(2_000),
                storage_size_bytes: Some(2_048),
            },
        }
    }

    fn policy(models: &[&str], tasks: &[&str]) -> CampaignPolicy {
        CampaignPolicy {
            campaign_id: "campaign".to_owned(),
            engine_commit_head: ENGINE_SHA.to_owned(),
            comparison_class: CampaignComparisonClass::ModelComparison,
            execution_profile: CampaignExecutionProfile::Warm,
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            backend_runtime_version: "runtime-v1".to_owned(),
            hardware: BenchmarkHardwareIdentity::default(),
            quality: QualityPreference::Balanced,
            run_timeout_seconds: 1_800,
            candidates: models
                .iter()
                .map(|model| installed_candidate(model))
                .collect(),
            task_ids: tasks.iter().map(|task| (*task).to_owned()).collect(),
            repetitions: 2,
            host_seed: 7,
        }
    }

    fn frozen(models: &[&str], tasks: &[&str]) -> CampaignPlan {
        policy(models, tasks).freeze().expect("policy must freeze")
    }

    fn installed(models: &[&str]) -> BTreeSet<String> {
        models.iter().map(|model| (*model).to_owned()).collect()
    }

    fn metrics(elapsed_ms: TelemetryValue<u64>) -> BenchmarkMetrics {
        BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(true),
            completion_success: TelemetryValue::Measured(true),
            model_turns: TelemetryValue::Measured(1),
            tool_calls: TelemetryValue::Measured(1),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
            code_edits: TelemetryValue::Measured(0),
            validation_attempts: TelemetryValue::Measured(0),
            repair_loops: TelemetryValue::Measured(0),
            play_attempts: TelemetryValue::Measured(0),
            frame_capture_attempts: TelemetryValue::Measured(0),
            visual_evaluation_attempts: TelemetryValue::Measured(0),
            human_interventions: TelemetryValue::Measured(0),
            elapsed_ms,
            prompt_tokens: TelemetryValue::Measured(1),
            response_tokens: TelemetryValue::Measured(1),
            load_latency_ms: TelemetryValue::Measured(1),
            ttft_ms: TelemetryValue::Unavailable,
            generation_tokens_per_second_milli: TelemetryValue::Measured(1),
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Measured(0),
        }
    }

    fn record_for(plan: &CampaignPlan, model_id: &str, task_id: &str) -> BenchmarkRecord {
        let task = benchmark_task(task_id).expect("known task");
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity: BenchmarkIdentity {
                corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
                task_id: task_id.to_owned(),
                harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
                runtime_harness_version: plan
                    .task_plan(task_id)
                    .expect("task plan")
                    .runtime_harness_version
                    .clone(),
                runtime: plan.benchmark_runtime.clone(),
                model: BenchmarkModelIdentity {
                    backend_id: "ollama-compatible".to_owned(),
                    model_id: model_id.to_owned(),
                    model_version: TelemetryValue::Measured(format!("{model_id}-digest")),
                    quantization: TelemetryValue::Measured("q4".to_owned()),
                    representation_size_bytes: TelemetryValue::Measured(1_000),
                    backend_runtime_version: TelemetryValue::Measured("runtime-v1".to_owned()),
                },
                hardware: BenchmarkHardwareIdentity {
                    platform: "test".to_owned(),
                    gpu: TelemetryValue::Measured("gpu".to_owned()),
                    total_gpu_memory_bytes: TelemetryValue::Measured(1),
                    total_system_memory_bytes: TelemetryValue::Measured(1),
                },
                quality: QualityPreference::Balanced,
                workload_policy_version: "adr0135-workload-policy-v1".to_owned(),
                observed_workload: TelemetryValue::Measured(
                    InferenceWorkload::InteractiveReasoning,
                ),
                tool_budget: BenchmarkToolBudget {
                    max_model_turns: 1,
                    max_tool_failures: 0,
                    repair_budget: 0,
                    permission_budget: vec!["read_only".to_owned()],
                    work_claims: Vec::new(),
                },
                completion_criteria: task
                    .completion_criteria
                    .iter()
                    .map(|criterion| (*criterion).to_owned())
                    .collect(),
                execution: plan.execution_identity(task_id).ok(),
            },
            metrics: metrics(TelemetryValue::Measured(5)),
        }
    }

    fn started(plan: CampaignPlan, models: &[&str]) -> CampaignRun {
        let mut run = CampaignRun::prepare(plan);
        run.start(None, &installed(models))
            .expect("installed candidates need no approval");
        run
    }

    fn pass(run: &mut CampaignRun, plan: &CampaignPlan) {
        let scheduled = run.next_scheduled().expect("a pending position");
        let record = record_for(plan, &scheduled.model_id, &scheduled.task_id);
        run.record(CampaignEvidence {
            scheduled,
            outcome: BenchmarkRunOutcome::Passed,
            failure_kind: None,
            attempt: 0,
            record: Some(record),
        })
        .expect("evidence must be admitted");
    }

    #[test]
    fn changing_repetition_policy_produces_a_different_campaign_identity() {
        let baseline = frozen(&["model-a"], &["read_question_v1"]);
        let mut changed = policy(&["model-a"], &["read_question_v1"]);
        changed.repetitions = 3;
        let changed = changed.freeze().expect("policy must freeze");
        assert_ne!(baseline.plan_digest(), changed.plan_digest());
    }

    #[test]
    fn changing_the_execution_environment_produces_a_different_campaign_identity() {
        let baseline = frozen(&["model-a"], &["read_question_v1"]);
        let mut changed = policy(&["model-a"], &["read_question_v1"]);
        changed.execution_environment = CampaignExecutionEnvironment::Wsl2Linux;
        let changed = changed.freeze().expect("policy must freeze");
        assert_ne!(baseline.plan_digest(), changed.plan_digest());
    }

    #[test]
    fn a_candidate_without_exactly_measured_bytes_cannot_be_frozen() {
        let mut inexact = policy(&["model-a"], &["read_question_v1"]);
        inexact.candidates[0]
            .representation
            .representation_size_bytes = 0;
        assert!(inexact.freeze().is_err());
    }

    #[test]
    fn the_schedule_is_stable_for_the_same_frozen_plan() {
        let plan = frozen(
            &["model-a", "model-b"],
            &["read_question_v1", "project_inspection_v1"],
        );
        let schedule = plan.schedule();
        assert_eq!(schedule, plan.schedule());
        assert_eq!(schedule.len(), 2 * 2 * 2);
        assert_eq!(schedule[0].model_id, "model-a");
        assert_eq!(schedule[1].model_id, "model-b");
        assert_eq!(schedule[0].task_id, "read_question_v1");
        assert_eq!(schedule[1].task_id, "read_question_v1");
        assert_eq!(schedule[0].repetition, 0);
        assert_eq!(schedule[1].repetition, 0);
    }

    #[test]
    fn every_task_descriptor_maps_to_its_intended_production_harness() {
        for task in BENCHMARK_TASKS.iter() {
            let harness = campaign_task_harness(task.id).expect("known task");
            let expected = match task.id {
                "read_question_v1" => CampaignTaskHarness::NativeReadQuestion,
                "runtime_interaction_v1" | "visual_evaluation_v1" => {
                    CampaignTaskHarness::ProductionRuntimeDebug
                }
                _ => CampaignTaskHarness::GovernedAgentHost,
            };
            assert_eq!(
                harness, expected,
                "task {} routed to the wrong harness",
                task.id
            );
        }
    }

    fn coding_runtime(runtime_id: &str) -> BenchmarkRuntimeIdentity {
        let mut harness = BenchmarkHarnessIdentity::new("acp-coding-agent", "harness-v1");
        harness.adapter_version = TelemetryValue::Measured("adapter-v1".to_owned());
        harness.acp_protocol_version = TelemetryValue::Measured(1);
        harness.mcp_tool_contract = TelemetryValue::Measured("editor-mcp-contract-v1".to_owned());
        harness.permission_profile =
            TelemetryValue::Measured("benchmark-agent-readwrite-v1".to_owned());
        BenchmarkRuntimeIdentity::coding_agent(
            harness,
            BenchmarkAgentRuntimeIdentity {
                runtime_id: runtime_id.to_owned(),
                runtime_version: TelemetryValue::Measured("1.0".to_owned()),
            },
        )
    }

    #[test]
    fn benchmark_runtime_registration_changes_campaign_identity_and_execution_identity() {
        let legacy = frozen(&["model-a"], &["read_question_v1"]);
        let classified = legacy
            .clone()
            .with_benchmark_runtime(coding_runtime("goose"))
            .expect("coding runtime registration");
        assert_ne!(legacy.plan_digest(), classified.plan_digest());
        assert_eq!(
            classified.benchmark_runtime().map(|runtime| runtime.lane),
            Some(BenchmarkLane::CodingAgent)
        );
        let execution = classified
            .execution_identity("read_question_v1")
            .expect("execution identity");
        assert_eq!(execution.benchmark_runtime, classified.benchmark_runtime);
    }

    #[test]
    fn agent_inclusive_campaign_evidence_never_qualifies_as_model_only_routing_evidence() {
        let plan = frozen(&["model-a"], &["read_question_v1"])
            .with_benchmark_runtime(coding_runtime("goose"))
            .expect("coding runtime registration");
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        assert!(run.qualified_records().is_empty());
    }

    #[test]
    fn campaign_rejects_a_record_with_a_different_benchmark_runtime() {
        let plan = frozen(&["model-a"], &["read_question_v1"])
            .with_benchmark_runtime(coding_runtime("goose"))
            .expect("coding runtime registration");
        let mut run = started(plan.clone(), &["model-a"]);
        let scheduled = run.next_scheduled().expect("scheduled run");
        let mut record = record_for(&plan, "model-a", "read_question_v1");
        record.identity.runtime = None;
        assert_eq!(
            run.record(CampaignEvidence {
                scheduled,
                outcome: BenchmarkRunOutcome::Passed,
                failure_kind: None,
                attempt: 0,
                record: Some(record),
            }),
            Err(CampaignRejection::IdentityMismatch)
        );
    }

    #[test]
    fn the_candidate_contract_never_carries_host_only_evaluation_state() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let contract = plan
            .candidate_contract("read_question_v1")
            .expect("contract must exist");
        let evaluation = plan
            .host_only_evaluation("read_question_v1")
            .expect("evaluation must exist");
        let serialized = serde_json::to_string(&contract).expect("contract serializes");
        assert!(!serialized.contains(&evaluation.expected_marker.to_string()));
        for assertion in &evaluation.hidden_assertions {
            assert!(!serialized.contains(assertion));
        }
    }

    #[test]
    fn host_only_inspection_evaluator_requires_measured_operations_not_gate_names_alone() {
        let plan = frozen(&["model-a"], &["project_inspection_v1"]);
        let evaluation = plan
            .host_only_evaluation("project_inspection_v1")
            .expect("evaluation");
        let mut record = record_for(&plan, "model-a", "project_inspection_v1");
        record.metrics.tool_calls = TelemetryValue::Measured(1);
        assert!(!evaluation.passes(&record));
        record.metrics.tool_calls = TelemetryValue::Measured(2);
        assert!(evaluation.passes(&record));
    }

    #[test]
    fn download_and_run_covers_only_the_missing_candidates() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        assert!(plan.acquisition_plan(&installed(&["model-a"])).is_none());

        let mut acquiring = policy(&["model-a"], &["read_question_v1"]);
        acquiring.candidates = vec![
            acquirable_candidate("model-a"),
            installed_candidate("model-b"),
        ];
        let plan = acquiring.freeze().expect("policy must freeze");
        let acquisition = plan
            .acquisition_plan(&installed(&["model-b"]))
            .expect("one candidate is missing");
        assert_eq!(acquisition.candidates.len(), 1);
        assert_eq!(acquisition.candidates[0].candidate_id, "model-a");
        assert_eq!(acquisition.review().total_transfer_bytes, 2_000);
    }

    #[test]
    fn a_campaign_with_missing_candidates_refuses_to_start_without_approval() {
        let mut acquiring = policy(&["model-a"], &["read_question_v1"]);
        acquiring.candidates = vec![acquirable_candidate("model-a")];
        let plan = acquiring.freeze().expect("policy must freeze");
        let mut run = CampaignRun::prepare(plan.clone());
        assert_eq!(
            run.start(None, &BTreeSet::new()),
            Err(CampaignRejection::UnapprovedCandidate)
        );

        let approval = plan
            .acquisition_plan(&BTreeSet::new())
            .expect("a missing candidate")
            .approve_exact();
        assert!(run.start(Some(approval), &BTreeSet::new()).is_ok());
        assert!(run.has_acquisition_approval());
    }

    #[test]
    fn a_prepared_campaign_records_nothing_until_it_is_started() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let run = CampaignRun::prepare(plan);
        assert_eq!(run.state(), CampaignState::Planned);
        assert!(run.next_scheduled().is_none());
    }

    #[test]
    fn only_one_scheduled_position_is_outstanding_at_a_time() {
        let plan = frozen(&["model-a", "model-b"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a", "model-b"]);
        let first = run.next_scheduled().expect("a pending position");
        assert_eq!(run.next_scheduled(), Some(first.clone()));
        pass(&mut run, &plan);
        assert_ne!(run.next_scheduled(), Some(first));
    }

    #[test]
    fn a_record_from_another_campaign_is_rejected() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let other = frozen(&["model-a"], &["project_inspection_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        let scheduled = run.next_scheduled().expect("a pending position");
        let mut record = record_for(&plan, "model-a", "read_question_v1");
        record.identity.execution = other.execution_identity("project_inspection_v1").ok();
        assert_eq!(
            run.record(CampaignEvidence {
                scheduled,
                outcome: BenchmarkRunOutcome::Passed,
                failure_kind: None,
                attempt: 0,
                record: Some(record),
            }),
            Err(CampaignRejection::IdentityMismatch)
        );
    }

    #[test]
    fn a_model_re_pulled_at_new_bytes_is_rejected_as_representation_drift() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        let scheduled = run.next_scheduled().expect("a pending position");
        let mut record = record_for(&plan, "model-a", "read_question_v1");
        record.identity.model.representation_size_bytes = TelemetryValue::Measured(2_000);
        assert_eq!(
            run.record(CampaignEvidence {
                scheduled,
                outcome: BenchmarkRunOutcome::Passed,
                failure_kind: None,
                attempt: 0,
                record: Some(record),
            }),
            Err(CampaignRejection::RepresentationDrift)
        );
    }

    #[test]
    fn a_measured_failure_is_retained_rather_than_retried() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        let scheduled = run.next_scheduled().expect("a pending position");
        let mut record = record_for(&plan, "model-a", "read_question_v1");
        record.metrics.completion_success = TelemetryValue::Measured(false);
        run.record(CampaignEvidence {
            scheduled: scheduled.clone(),
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: Some(BenchmarkRunFailureKind::CompletionGate),
            attempt: 0,
            record: Some(record),
        })
        .expect("a measured failure is evidence");
        assert_eq!(run.evidence().len(), 1);
        assert_ne!(run.next_scheduled(), Some(scheduled));
    }

    #[test]
    fn a_pre_measurement_harness_failure_is_recorded_without_an_unexecuted_retry() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan, &["model-a"]);
        let scheduled = run.next_scheduled().expect("a pending position");
        let infrastructure = CampaignEvidence {
            scheduled,
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: Some(BenchmarkRunFailureKind::Harness),
            attempt: 0,
            record: None,
        };
        run.record(infrastructure).expect("failure is evidence");
        assert_eq!(run.evidence().len(), 1);
    }

    #[test]
    fn pause_retains_prior_evidence_and_resume_rejects_environment_drift() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        run.pause();
        assert_eq!(run.state(), CampaignState::Paused);
        assert_eq!(run.evidence().len(), 1);

        let drifted = CampaignEnvironmentProbe {
            execution_environment: CampaignExecutionEnvironment::Wsl2Linux,
            backend_runtime_version: "runtime-v1".to_owned(),
            engine_commit_head: ENGINE_SHA.to_owned(),
            hardware: BenchmarkHardwareIdentity::default(),
            representations: plan
                .candidates
                .iter()
                .map(|candidate| candidate.representation.clone())
                .collect(),
        };
        assert_eq!(
            run.resume(&drifted),
            Err(CampaignRejection::EnvironmentDrift)
        );
        assert_eq!(run.evidence().len(), 1);

        let unchanged = CampaignEnvironmentProbe {
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            backend_runtime_version: "runtime-v1".to_owned(),
            engine_commit_head: ENGINE_SHA.to_owned(),
            hardware: BenchmarkHardwareIdentity::default(),
            representations: plan
                .candidates
                .iter()
                .map(|candidate| candidate.representation.clone())
                .collect(),
        };
        assert!(run.resume(&unchanged).is_ok());
    }

    #[test]
    fn campaign_checkpoint_round_trip_preserves_the_next_pending_ordinal() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        let bytes = serde_json::to_vec(&run).expect("checkpoint serializes");
        let mut restored = serde_json::from_slice::<CampaignRun>(&bytes).expect("checkpoint reads");
        assert_eq!(restored.next_ordinal(), 1);
        assert_eq!(restored.evidence(), run.evidence());
        restored.pause();
        assert_eq!(restored.state(), CampaignState::Paused);
    }

    #[test]
    fn resume_rejects_a_checkpoint_whose_frozen_plan_digest_was_tampered_with() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        run.pause();
        run.plan.plan_digest.push_str("-tampered");
        let unchanged = CampaignEnvironmentProbe {
            execution_environment: CampaignExecutionEnvironment::CompatibleBackend,
            backend_runtime_version: "runtime-v1".to_owned(),
            engine_commit_head: ENGINE_SHA.to_owned(),
            hardware: BenchmarkHardwareIdentity::default(),
            representations: plan
                .candidates
                .iter()
                .map(|candidate| candidate.representation.clone())
                .collect(),
        };
        assert_eq!(
            run.resume(&unchanged),
            Err(CampaignRejection::EnvironmentDrift)
        );
    }

    #[test]
    fn evidence_arriving_while_paused_is_refused() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        let scheduled = run.next_scheduled().expect("a pending position");
        run.pause();
        assert_eq!(
            run.record(CampaignEvidence {
                scheduled,
                outcome: BenchmarkRunOutcome::Passed,
                failure_kind: None,
                attempt: 0,
                record: Some(record_for(&plan, "model-a", "read_question_v1")),
            }),
            Err(CampaignRejection::NotRunning)
        );
    }

    #[test]
    fn each_repetition_is_preserved_individually() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        pass(&mut run, &plan);
        assert_eq!(run.evidence().len(), 2);
        assert_eq!(run.evidence()[0].scheduled.repetition, 0);
        assert_eq!(run.evidence()[1].scheduled.repetition, 1);
        assert_eq!(run.state(), CampaignState::Completed);
    }

    #[test]
    fn an_unmeasured_elapsed_time_is_reported_as_unavailable_rather_than_zero() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        let scheduled = run.next_scheduled().expect("a pending position");
        let mut record = record_for(&plan, "model-a", "read_question_v1");
        record.metrics.elapsed_ms = TelemetryValue::Unavailable;
        run.record(CampaignEvidence {
            scheduled,
            outcome: BenchmarkRunOutcome::Passed,
            failure_kind: None,
            attempt: 0,
            record: Some(record),
        })
        .expect("evidence must be admitted");
        let report = run.report();
        assert_eq!(report.models[0].aggregate_elapsed_ms, None);
        assert_eq!(report.models[0].task_successes, 2);
    }

    #[test]
    fn an_incomplete_campaign_qualifies_no_records_for_routing() {
        let plan = frozen(&["model-a"], &["read_question_v1"]);
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        assert!(!run.report().evidence_complete);
        assert!(run.qualified_records().is_empty());
        pass(&mut run, &plan);
        assert!(run.report().evidence_complete);
        assert_eq!(run.qualified_records().len(), 2);
    }

    #[test]
    fn runtime_characterization_evidence_never_qualifies_for_model_routing() {
        let mut characterization = policy(&["model-a"], &["read_question_v1"]);
        characterization.comparison_class = CampaignComparisonClass::RuntimeCharacterization;
        characterization.execution_environment = CampaignExecutionEnvironment::Wsl2Linux;
        let plan = characterization.freeze().expect("policy must freeze");
        let mut run = started(plan.clone(), &["model-a"]);
        pass(&mut run, &plan);
        pass(&mut run, &plan);
        assert!(run.report().evidence_complete);
        assert!(run.qualified_records().is_empty());
    }

    #[test]
    fn adding_a_model_reuses_a_strictly_comparable_baseline() {
        let baseline = frozen(&["model-a"], &["read_question_v1"]);
        let extended = frozen(&["model-a", "model-b"], &["read_question_v1"]);
        assert_eq!(
            extended.baseline_reuse(&baseline),
            CampaignBaselineReuse::Reusable {
                model_ids: vec!["model-a".to_owned()],
            }
        );
    }

    #[test]
    fn a_changed_equivalence_dimension_forces_a_baseline_rerun() {
        let baseline = frozen(&["model-a"], &["read_question_v1"]);
        let mut drifted = policy(&["model-a", "model-b"], &["read_question_v1"]);
        drifted.backend_runtime_version = "runtime-v2".to_owned();
        let drifted = drifted.freeze().expect("policy must freeze");
        assert_eq!(
            drifted.baseline_reuse(&baseline),
            CampaignBaselineReuse::RequiresBaselineRerun {
                changed: vec!["backend_runtime_version"],
            }
        );
    }

    #[test]
    fn the_experiment_specification_carries_campaign_identity_for_every_task() {
        let plan = frozen(&["model-a"], &["read_question_v1", "project_inspection_v1"]);
        let spec = plan
            .experiment_spec(PathBuf::from("benchmarks"))
            .expect("plan must lower to a valid experiment");
        assert_eq!(spec.execution_identity_by_task.len(), 2);
        assert_eq!(spec.candidate_contract_by_task.len(), 2);
        for task_id in ["read_question_v1", "project_inspection_v1"] {
            assert_eq!(
                spec.execution_identity_by_task.get(task_id),
                plan.execution_identity(task_id).ok().as_ref()
            );
        }
    }

    #[test]
    fn a_cold_profile_campaign_is_a_different_campaign_than_a_warm_one() {
        let warm = frozen(&["model-a"], &["read_question_v1"]);
        let mut cold = policy(&["model-a"], &["read_question_v1"]);
        cold.execution_profile = CampaignExecutionProfile::Cold;
        let cold = cold.freeze().expect("policy must freeze");
        assert_ne!(warm.plan_digest(), cold.plan_digest());
        assert_eq!(
            warm.execution_identity("read_question_v1")
                .expect("identity")
                .execution_profile,
            CampaignExecutionProfile::Warm.label()
        );
        assert_eq!(
            cold.execution_identity("read_question_v1")
                .expect("identity")
                .execution_profile,
            CampaignExecutionProfile::Cold.label()
        );
    }
}
