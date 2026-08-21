//! ADR 0156 benchmark campaign identity primitives.
//!
//! Physical model bytes are frozen separately from execution-runtime identity so
//! Windows-native versus WSL2 evidence cannot be mistaken for model-only evidence.

use crate::agent_benchmark::{
    BenchmarkModelIdentity, BenchmarkRecord, BenchmarkTaskKind, BenchmarkToolBudget, benchmark_task,
};
use crate::agent_host::{AgentCapability, AgentWorkClaim};
use crate::managed_local_runtime::ManagedExecutionEnvironment;
use crate::native_agent::BASELINE_HARNESS_VERSION;
use crate::native_agent_runtime::{HarnessPolicy, NATIVE_WRITE_HARNESS_VERSION};
use crate::resource_arbitration::{InferenceWorkload, TelemetryValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(crate) const CAMPAIGN_SCHEMA_VERSION: u32 = 3;
pub(crate) const CAMPAIGN_HARNESS_VERSION: &str = "gameengine-agent-benchmark-campaign-v2";
pub(crate) const CAMPAIGN_SCHEDULE_VERSION: &str = "task-repetition-candidate-interleave-v2";
pub(crate) const CAMPAIGN_FIXTURE_VERSION: &str = "gameengine-agent-fixture-v1";
pub(crate) const DEFAULT_CAMPAIGN_REPETITIONS: u32 = 3;

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

    pub(crate) const fn managed_environment(self) -> Option<ManagedExecutionEnvironment> {
        match self {
            Self::CompatibleBackend => None,
            Self::WindowsNative => Some(ManagedExecutionEnvironment::WindowsNative),
            Self::Wsl2Linux => Some(ManagedExecutionEnvironment::Wsl2Linux),
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
    let task =
        benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
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
    let task =
        benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
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

/// Frozen AgentHost authorization and ownership declared by one campaign task.
///
/// Both campaign metadata and the actual immutable proposal consume this type,
/// preventing displayed budgets from drifting away from runtime authorization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CampaignTaskAgentPolicy {
    pub(crate) requested_capabilities: BTreeSet<AgentCapability>,
    pub(crate) work_claims: BTreeSet<AgentWorkClaim>,
}

pub(crate) fn campaign_task_agent_policy(task_id: &str) -> Result<CampaignTaskAgentPolicy, String> {
    let task =
        benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    let mut policy = CampaignTaskAgentPolicy::default();
    match task.kind {
        BenchmarkTaskKind::CodeImplementation | BenchmarkTaskKind::ValidationRepair => {
            policy
                .requested_capabilities
                .insert(AgentCapability::CodeWorkspaceApply);
            policy
                .work_claims
                .insert(AgentWorkClaim::code_path("game/src/benchmark_target.rs"));
        }
        BenchmarkTaskKind::TypedAuthoringMutation => {
            policy
                .work_claims
                .insert(AgentWorkClaim::authoring_document(
                    "assets/scenes/main.scene.json",
                ));
        }
        BenchmarkTaskKind::RuntimeInteraction => {
            policy
                .requested_capabilities
                .insert(AgentCapability::RuntimeLaunch);
            policy
                .requested_capabilities
                .insert(AgentCapability::RuntimeInputControl);
        }
        BenchmarkTaskKind::VisualEvaluation => {
            policy
                .requested_capabilities
                .insert(AgentCapability::RuntimeLaunch);
            policy
                .requested_capabilities
                .insert(AgentCapability::RuntimeInputControl);
            policy
                .requested_capabilities
                .insert(AgentCapability::FrameCapture);
        }
        BenchmarkTaskKind::ReadQuestion | BenchmarkTaskKind::ProjectInspection => {}
    }
    Ok(policy)
}

fn task_tool_budget(task_id: &str) -> Result<BenchmarkToolBudget, String> {
    if campaign_task_harness(task_id)? == CampaignTaskHarness::NativeReadQuestion {
        return Ok(BenchmarkToolBudget {
            max_model_turns: 1,
            max_tool_failures: 0,
            repair_budget: 0,
            permission_budget: vec!["read_only".to_owned()],
            work_claims: Vec::new(),
        });
    }
    let policy = HarnessPolicy::default();
    let agent_policy = campaign_task_agent_policy(task_id)?;
    let permission_budget = agent_policy
        .requested_capabilities
        .into_iter()
        .map(|capability| {
            serde_json::to_value(capability)
                .map_err(|error| error.to_string())?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "capability did not serialize as a string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let work_claims = agent_policy
        .work_claims
        .into_iter()
        .map(|claim| {
            serde_json::to_value(claim.kind)
                .map_err(|error| error.to_string())?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "work claim kind did not serialize as a string".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BenchmarkToolBudget {
        max_model_turns: policy.max_model_turns,
        max_tool_failures: policy.max_tool_failures,
        repair_budget: policy.repair_budget,
        permission_budget,
        work_claims,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CampaignRepresentation {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: String,
    /// Provider quantization label or an equivalent exact representation descriptor.
    ///
    /// The serialized field name is retained for campaign-schema compatibility.
    pub(crate) quantization: String,
    pub(crate) representation_size_bytes: u64,
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
        }
    }

    pub(crate) fn exact(&self) -> bool {
        !self.backend_id.trim().is_empty()
            && !self.model_id.trim().is_empty()
            && !self.model_version.trim().is_empty()
            && !self.quantization.trim().is_empty()
            && self.representation_size_bytes > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignRuntimeIdentity {
    pub(crate) execution_environment: CampaignExecutionEnvironment,
    pub(crate) backend_runtime_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignCandidateSource {
    pub(crate) source_reference: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) expected_sha256: Option<String>,
    pub(crate) transfer_size_bytes: Option<u64>,
    pub(crate) storage_size_bytes: Option<u64>,
}

impl CampaignCandidateSource {
    pub(crate) fn installed() -> Self {
        Self {
            source_reference: None,
            license: None,
            expected_sha256: None,
            transfer_size_bytes: None,
            storage_size_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CampaignCandidate {
    pub(crate) representation: CampaignRepresentation,
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
        let hash = stable_hash(format!("{task_id}:{host_seed}").as_bytes());
        Self {
            fixture_id: format!("gameengine-agent-{task_id}"),
            fixture_version: CAMPAIGN_FIXTURE_VERSION.to_owned(),
            instance_id: format!("{hash:016x}"),
            host_seed,
        }
    }

    /// Derives the host-owned evaluation material for this frozen instance.
    ///
    /// The host seed stays private to this module: only the derived assertions
    /// leave, and they leave in a type the candidate context cannot serialize.
    ///
    /// # Errors
    ///
    /// Returns an error when `task_id` is not one of the seven ADR 0142 tasks.
    pub(crate) fn host_only_evaluation(&self, task_id: &str) -> Result<HostOnlyEvaluation, String> {
        let task =
            benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
        let expected_marker = self.host_seed.rotate_right(29) ^ stable_hash(task.id.as_bytes());
        Ok(HostOnlyEvaluation {
            task_id: task.id.to_owned(),
            hidden_assertions: task
                .completion_criteria
                .iter()
                .map(|criterion| format!("host-verified: {criterion}"))
                .collect(),
            scoring_threshold: task.completion_criteria.len() as u32,
            expected_marker,
        })
    }

    pub(crate) fn candidate_contract(
        &self,
        task_id: &str,
    ) -> Result<CandidateTaskContract, String> {
        let task =
            benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
        let marker_hash = self.host_seed.rotate_left(17) ^ stable_hash(task_id.as_bytes());
        Ok(CandidateTaskContract {
            task_id: task.id.to_owned(),
            prompt: format!(
                "Execute candidate-visible ADR0156 fixture fixture-{:08x} using only normal production tools.",
                marker_hash as u32
            ),
            completion_criteria: task
                .completion_criteria
                .iter()
                .map(|criterion| (*criterion).to_owned())
                .collect(),
        })
    }
}

/// Host-owned evaluation material for one frozen fixture instance.
///
/// This deliberately does not implement `Serialize`. Candidate-visible context
/// is assembled only from serializable contract types, so hidden assertions and
/// scoring thresholds cannot reach a prompt, a tool result, or an on-disk
/// candidate document by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostOnlyEvaluation {
    pub(crate) task_id: String,
    pub(crate) hidden_assertions: Vec<String>,
    pub(crate) scoring_threshold: u32,
    pub(crate) expected_marker: u64,
}

impl HostOnlyEvaluation {
    /// Evaluates task-specific host evidence without exposing the oracle.
    ///
    /// The model's claimed gate names are insufficient by themselves. Each
    /// task must also carry the production-harness measurements that prove the
    /// relevant operation actually occurred.
    pub(crate) fn passes(&self, record: &BenchmarkRecord) -> bool {
        if record.identity.task_id != self.task_id
            || record.metrics.acceptance_success != TelemetryValue::Measured(true)
            || record.metrics.completion_success != TelemetryValue::Measured(true)
        {
            return false;
        }
        let at_least = |value: &TelemetryValue<u64>, minimum| matches!(value, TelemetryValue::Measured(actual) if *actual >= minimum);
        match self.task_id.as_str() {
            "read_question_v1" => at_least(&record.metrics.model_turns, 1),
            "project_inspection_v1" => {
                at_least(&record.metrics.tool_calls, 2)
                    && record.metrics.invalid_or_failed_tool_calls == TelemetryValue::Measured(0)
            }
            "code_implementation_v1" => {
                at_least(&record.metrics.code_edits, 1)
                    && at_least(&record.metrics.validation_attempts, 1)
            }
            "typed_authoring_mutation_v1" => at_least(&record.metrics.tool_calls, 1),
            "validation_repair_v1" => {
                at_least(&record.metrics.code_edits, 1)
                    && at_least(&record.metrics.validation_attempts, 2)
                    && at_least(&record.metrics.repair_loops, 1)
            }
            "runtime_interaction_v1" => at_least(&record.metrics.play_attempts, 1),
            "visual_evaluation_v1" => {
                at_least(&record.metrics.play_attempts, 1)
                    && at_least(&record.metrics.frame_capture_attempts, 1)
                    && at_least(&record.metrics.visual_evaluation_attempts, 1)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CandidateTaskContract {
    pub(crate) task_id: String,
    pub(crate) prompt: String,
    pub(crate) completion_criteria: Vec<String>,
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
        let runtime_harness_version = match campaign_task_harness(task_id)? {
            CampaignTaskHarness::NativeReadQuestion => BASELINE_HARNESS_VERSION,
            CampaignTaskHarness::GovernedAgentHost
            | CampaignTaskHarness::ProductionRuntimeDebug => NATIVE_WRITE_HARNESS_VERSION,
        };
        Ok(Self {
            task_id: task_id.to_owned(),
            fixture: CampaignFixtureIdentity::for_task(task_id, host_seed),
            runtime_harness_version: runtime_harness_version.to_owned(),
            workload: campaign_task_workload(task_id)?,
            tool_budget: task_tool_budget(task_id)?,
        })
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_benchmark::BENCHMARK_TASKS;

    #[test]
    fn all_seven_tasks_map_to_authoritative_production_harnesses() {
        for task in BENCHMARK_TASKS {
            let harness = campaign_task_harness(task.id).expect("harness");
            if matches!(
                task.kind,
                BenchmarkTaskKind::RuntimeInteraction | BenchmarkTaskKind::VisualEvaluation
            ) {
                assert_eq!(harness, CampaignTaskHarness::ProductionRuntimeDebug);
            }
        }
    }

    #[test]
    fn physical_representation_excludes_runtime_platform_identity() {
        let model = BenchmarkModelIdentity {
            backend_id: "gameengine-managed-llama-cpp".to_owned(),
            model_id: "gguf:test".to_owned(),
            model_version: TelemetryValue::Measured("sha256-test".to_owned()),
            quantization: TelemetryValue::Measured("Q4_K_M".to_owned()),
            representation_size_bytes: TelemetryValue::Measured(1_000),
            backend_runtime_version: TelemetryValue::Measured("windows-runtime".to_owned()),
        };
        let representation = CampaignRepresentation::from_model(&model);
        assert!(representation.exact());
        assert!(
            !serde_json::to_string(&representation)
                .expect("representation JSON")
                .contains("windows-runtime")
        );
    }

    #[test]
    fn task_budget_and_proposal_policy_share_one_authorization_source() {
        let code_policy =
            campaign_task_agent_policy("code_implementation_v1").expect("code policy");
        assert_eq!(
            code_policy.requested_capabilities,
            BTreeSet::from([AgentCapability::CodeWorkspaceApply])
        );
        assert_eq!(
            code_policy.work_claims,
            BTreeSet::from([AgentWorkClaim::code_path("game/src/benchmark_target.rs")])
        );
        let code_budget = task_tool_budget("code_implementation_v1").expect("code budget");
        assert_eq!(
            code_budget.permission_budget,
            vec!["code_workspace_apply".to_owned()]
        );
        assert_eq!(code_budget.work_claims, vec!["code_path".to_owned()]);

        let inspection =
            campaign_task_agent_policy("project_inspection_v1").expect("inspection policy");
        assert!(inspection.requested_capabilities.is_empty());
        assert!(inspection.work_claims.is_empty());
    }
}
