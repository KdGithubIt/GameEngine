//! ADR 0156 benchmark campaign identity primitives.
//!
//! Physical model bytes are frozen separately from execution-runtime identity so
//! Windows-native versus WSL2 evidence cannot be mistaken for model-only evidence.

use crate::agent_benchmark::{
    benchmark_task, BenchmarkModelIdentity, BenchmarkTaskKind, BenchmarkToolBudget,
};
use crate::native_agent::BASELINE_HARNESS_VERSION;
use crate::native_agent_runtime::{HarnessPolicy, NATIVE_WRITE_HARNESS_VERSION};
use crate::resource_arbitration::{InferenceWorkload, TelemetryValue};
use serde::{Deserialize, Serialize};

pub(crate) const CAMPAIGN_SCHEMA_VERSION: u32 = 1;
pub(crate) const CAMPAIGN_HARNESS_VERSION: &str = "gameengine-agent-benchmark-campaign-v1";
pub(crate) const CAMPAIGN_SCHEDULE_VERSION: &str = "task-repetition-candidate-interleave-v1";
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

fn task_tool_budget(task_id: &str) -> Result<BenchmarkToolBudget, String> {
    let task = benchmark_task(task_id)
        .ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
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
    let permission_budget = match task.kind {
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
    };
    let work_claims = match task.kind {
        BenchmarkTaskKind::CodeImplementation | BenchmarkTaskKind::ValidationRepair => {
            vec!["code_path".to_owned()]
        }
        BenchmarkTaskKind::TypedAuthoringMutation => vec!["authoring_document".to_owned()],
        _ => Vec::new(),
    };
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
pub(crate) struct CampaignTaskPlan {
    pub(crate) task_id: String,
    pub(crate) runtime_harness_version: String,
    pub(crate) workload: InferenceWorkload,
    pub(crate) tool_budget: BenchmarkToolBudget,
}

impl CampaignTaskPlan {
    pub(crate) fn for_task(task_id: &str) -> Result<Self, String> {
        let runtime_harness_version = match campaign_task_harness(task_id)? {
            CampaignTaskHarness::NativeReadQuestion => BASELINE_HARNESS_VERSION,
            CampaignTaskHarness::GovernedAgentHost
            | CampaignTaskHarness::ProductionRuntimeDebug => NATIVE_WRITE_HARNESS_VERSION,
        };
        Ok(Self {
            task_id: task_id.to_owned(),
            runtime_harness_version: runtime_harness_version.to_owned(),
            workload: campaign_task_workload(task_id)?,
            tool_budget: task_tool_budget(task_id)?,
        })
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
        assert!(!serde_json::to_string(&representation)
            .expect("representation JSON")
            .contains("windows-runtime"));
    }
}
