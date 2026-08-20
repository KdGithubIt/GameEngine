//! Benchmark-gated multi-model routing for the native AgentRuntime (ADR 0150).
//!
//! Routing remains an optimization over one provider-independent AgentRun. A
//! specialist is eligible only when comparable ADR 0142 evidence proves that it
//! preserves successful completion and improves the configured objective.

use crate::agent_benchmark::{
    BENCHMARK_TASKS, BenchmarkRecord, BenchmarkTaskKind, ComparisonEquivalence,
    comparison_equivalence,
};
use crate::native_agent::NativeModelConfig;
use crate::resource_arbitration::TelemetryValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub(crate) const MODEL_ROUTER_POLICY_VERSION: &str = "adr0150-measured-routing-v1";
const MIN_LATENCY_IMPROVEMENT_PERCENT: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RoutingWorkload {
    ReadQuestion,
    ProjectInspection,
    CodeImplementation,
    TypedAuthoringMutation,
    ValidationRepair,
    RuntimeInteraction,
    VisualEvaluation,
}

impl RoutingWorkload {
    fn from_task_kind(kind: BenchmarkTaskKind) -> Self {
        match kind {
            BenchmarkTaskKind::ReadQuestion => Self::ReadQuestion,
            BenchmarkTaskKind::ProjectInspection => Self::ProjectInspection,
            BenchmarkTaskKind::CodeImplementation => Self::CodeImplementation,
            BenchmarkTaskKind::TypedAuthoringMutation => Self::TypedAuthoringMutation,
            BenchmarkTaskKind::ValidationRepair => Self::ValidationRepair,
            BenchmarkTaskKind::RuntimeInteraction => Self::RuntimeInteraction,
            BenchmarkTaskKind::VisualEvaluation => Self::VisualEvaluation,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ReadQuestion => "read question",
            Self::ProjectInspection => "project inspection",
            Self::CodeImplementation => "code implementation",
            Self::TypedAuthoringMutation => "typed authoring mutation",
            Self::ValidationRepair => "validation repair",
            Self::RuntimeInteraction => "runtime interaction",
            Self::VisualEvaluation => "visual evaluation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelKey {
    backend_id: String,
    model_id: String,
}

impl ModelKey {
    fn from_config(config: &NativeModelConfig) -> Self {
        Self {
            backend_id: config.backend_id().to_owned(),
            model_id: config.model_id(),
        }
    }

    fn matches_record(&self, record: &BenchmarkRecord) -> bool {
        record.identity.model.backend_id == self.backend_id
            && record.identity.model.model_id == self.model_id
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RoutingSpecialist {
    pub(crate) config: NativeModelConfig,
    pub(crate) task_id: String,
    pub(crate) baseline_elapsed_ms: Option<u64>,
    pub(crate) specialist_elapsed_ms: Option<u64>,
    pub(crate) improves_task_success: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelRoutingPolicy {
    primary: NativeModelConfig,
    specialists: BTreeMap<RoutingWorkload, RoutingSpecialist>,
    image_verified: BTreeSet<ModelKey>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelRouteDecision {
    pub(crate) workload: RoutingWorkload,
    pub(crate) config: NativeModelConfig,
    pub(crate) reason: String,
    pub(crate) context_handoff: bool,
    pub(crate) fallback: bool,
}

impl ModelRouteDecision {
    pub(crate) fn audit_summary(&self) -> String {
        format!(
            "policy={} workload={} backend={} model={} context_handoff={} fallback={} reason={}",
            MODEL_ROUTER_POLICY_VERSION,
            self.workload.label(),
            self.config.backend_id(),
            self.config.model_id(),
            self.context_handoff,
            self.fallback,
            self.reason
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelRoutingError {
    CapabilityUnavailable {
        workload: RoutingWorkload,
        capability: &'static str,
    },
    UserDecisionRequired {
        workload: RoutingWorkload,
        backend_id: String,
        model_id: String,
    },
}

impl fmt::Display for ModelRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapabilityUnavailable {
                workload,
                capability,
            } => write!(
                formatter,
                "no benchmark-qualified model declares required {capability} capability for {}",
                workload.label()
            ),
            Self::UserDecisionRequired {
                workload,
                backend_id,
                model_id,
            } => write!(
                formatter,
                "routing {} to remote backend `{backend_id}` model `{model_id}` requires an explicit user decision because processing posture/cost class would change",
                workload.label()
            ),
        }
    }
}

impl std::error::Error for ModelRoutingError {}

impl ModelRoutingPolicy {
    pub(crate) fn derive(
        primary: NativeModelConfig,
        candidates: Vec<NativeModelConfig>,
        records: &[BenchmarkRecord],
    ) -> Self {
        let primary_key = ModelKey::from_config(&primary);
        let mut specialists = BTreeMap::new();
        let mut image_verified = BTreeSet::new();

        for record in records {
            if record.identity.task_id == "visual_evaluation_v1" && record_success(record) {
                image_verified.insert(ModelKey {
                    backend_id: record.identity.model.backend_id.clone(),
                    model_id: record.identity.model.model_id.clone(),
                });
            }
        }

        for task in BENCHMARK_TASKS {
            let workload = RoutingWorkload::from_task_kind(task.kind);
            let mut best: Option<(RoutingSpecialist, ImprovementRank)> = None;
            for candidate in &candidates {
                let candidate_key = ModelKey::from_config(candidate);
                if candidate_key == primary_key {
                    continue;
                }
                let Some((specialist, rank)) = best_comparable_improvement(
                    task.id,
                    &primary_key,
                    &candidate_key,
                    candidate.clone(),
                    records,
                ) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(_, current)| rank > *current) {
                    best = Some((specialist, rank));
                }
            }
            if let Some((specialist, _)) = best {
                specialists.insert(workload, specialist);
            }
        }

        Self {
            primary,
            specialists,
            image_verified,
        }
    }

    pub(crate) fn adopted_specialist_count(&self) -> usize {
        self.specialists.len()
    }

    pub(crate) fn select(
        &self,
        workload: RoutingWorkload,
        requires_image: bool,
    ) -> Result<ModelRouteDecision, ModelRoutingError> {
        if let Some(specialist) = self.specialists.get(&workload)
            && self.supports_requirements(&specialist.config, requires_image)
        {
            if !self.primary.requires_network() && specialist.config.requires_network() {
                return Err(ModelRoutingError::UserDecisionRequired {
                    workload,
                    backend_id: specialist.config.backend_id().to_owned(),
                    model_id: specialist.config.model_id(),
                });
            }
            let reason = match (
                specialist.improves_task_success,
                specialist.baseline_elapsed_ms,
                specialist.specialist_elapsed_ms,
            ) {
                (true, _, _) => format!("{} improved measured task success", specialist.task_id),
                (false, Some(baseline), Some(selected)) => format!(
                    "{} preserved success and improved measured latency from {baseline} ms to {selected} ms",
                    specialist.task_id
                ),
                _ => format!("{} satisfied measured routing policy", specialist.task_id),
            };
            return Ok(ModelRouteDecision {
                workload,
                config: specialist.config.clone(),
                reason,
                context_handoff: ModelKey::from_config(&specialist.config)
                    != ModelKey::from_config(&self.primary),
                fallback: false,
            });
        }

        if self.supports_requirements(&self.primary, requires_image) {
            return Ok(ModelRouteDecision {
                workload,
                config: self.primary.clone(),
                reason: "single-model baseline retained because no qualified specialist improves this workload".to_owned(),
                context_handoff: false,
                fallback: false,
            });
        }

        Err(ModelRoutingError::CapabilityUnavailable {
            workload,
            capability: if requires_image {
                "image input"
            } else {
                "requested"
            },
        })
    }

    pub(crate) fn fallback(
        &self,
        failed: &NativeModelConfig,
        workload: RoutingWorkload,
        requires_image: bool,
    ) -> Option<ModelRouteDecision> {
        if ModelKey::from_config(failed) == ModelKey::from_config(&self.primary)
            || !self.supports_requirements(&self.primary, requires_image)
        {
            return None;
        }
        if failed.requires_network() != self.primary.requires_network()
            && self.primary.requires_network()
        {
            return None;
        }
        Some(ModelRouteDecision {
            workload,
            config: self.primary.clone(),
            reason: "compatible specialist failed before the turn started; reverted to the measured single-model baseline".to_owned(),
            context_handoff: true,
            fallback: true,
        })
    }

    fn supports_requirements(&self, config: &NativeModelConfig, requires_image: bool) -> bool {
        if !requires_image {
            return true;
        }
        config.capability_profile().image_input == Some(true)
            || self.image_verified.contains(&ModelKey::from_config(config))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ImprovementRank {
    success_gain: bool,
    latency_gain_ms: u64,
}

fn best_comparable_improvement(
    task_id: &str,
    primary: &ModelKey,
    candidate: &ModelKey,
    config: NativeModelConfig,
    records: &[BenchmarkRecord],
) -> Option<(RoutingSpecialist, ImprovementRank)> {
    let mut best = None;
    for baseline in records
        .iter()
        .filter(|record| record.identity.task_id == task_id && primary.matches_record(record))
    {
        for specialist in records
            .iter()
            .filter(|record| record.identity.task_id == task_id && candidate.matches_record(record))
        {
            if comparison_equivalence(baseline, specialist)
                != ComparisonEquivalence::EquivalentModelComparison
                || !record_success(specialist)
                || has_oom_regression(baseline, specialist)
            {
                continue;
            }
            let baseline_success = record_success(baseline);
            let baseline_elapsed = measured_u64(&baseline.metrics.elapsed_ms);
            let specialist_elapsed = measured_u64(&specialist.metrics.elapsed_ms);
            let success_gain = !baseline_success;
            let latency_gain_ms = match (baseline_elapsed, specialist_elapsed) {
                (Some(left), Some(right)) if left > right => left - right,
                _ => 0,
            };
            let latency_improves = match (baseline_elapsed, specialist_elapsed) {
                (Some(left), Some(right)) if left > 0 => {
                    right.saturating_mul(100)
                        <= left.saturating_mul(100 - MIN_LATENCY_IMPROVEMENT_PERCENT)
                }
                _ => false,
            };
            if !success_gain && !latency_improves {
                continue;
            }
            let rank = ImprovementRank {
                success_gain,
                latency_gain_ms,
            };
            if best.as_ref().is_none_or(|(_, current)| rank > *current) {
                best = Some((
                    RoutingSpecialist {
                        config: config.clone(),
                        task_id: task_id.to_owned(),
                        baseline_elapsed_ms: baseline_elapsed,
                        specialist_elapsed_ms: specialist_elapsed,
                        improves_task_success: success_gain,
                    },
                    rank,
                ));
            }
        }
    }
    best
}

fn record_success(record: &BenchmarkRecord) -> bool {
    matches!(
        record.metrics.completion_success,
        TelemetryValue::Measured(true)
    ) && !matches!(
        record.metrics.acceptance_success,
        TelemetryValue::Measured(false)
    )
}

fn has_oom_regression(baseline: &BenchmarkRecord, candidate: &BenchmarkRecord) -> bool {
    match (
        measured_u64(&baseline.metrics.oom_failures),
        measured_u64(&candidate.metrics.oom_failures),
    ) {
        (Some(left), Some(right)) => right > left,
        (None, Some(right)) => right > 0,
        (_, None) => false,
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
    use crate::agent_benchmark::{
        BENCHMARK_CORPUS_VERSION, BENCHMARK_HARNESS_VERSION, BENCHMARK_SCHEMA_VERSION,
        BenchmarkHardwareIdentity, BenchmarkIdentity, BenchmarkMetrics, BenchmarkModelIdentity,
        BenchmarkToolBudget,
    };
    use crate::native_agent::{DEFAULT_LOCAL_MODEL_ENDPOINT, LocalModelConfig};
    use crate::resource_arbitration::{InferenceWorkload, QualityPreference};

    fn local(model: &str) -> NativeModelConfig {
        NativeModelConfig::Local(LocalModelConfig {
            endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            model: model.to_owned(),
        })
    }

    fn measured_record(
        model: &str,
        task_index: usize,
        elapsed_ms: u64,
        successful: bool,
    ) -> BenchmarkRecord {
        let task = BENCHMARK_TASKS[task_index];
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity: BenchmarkIdentity {
                corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
                task_id: task.id.to_owned(),
                harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
                runtime_harness_version: "runtime-harness-v1".to_owned(),
                runtime: None,
                model: BenchmarkModelIdentity {
                    backend_id: "ollama-compatible".to_owned(),
                    model_id: model.to_owned(),
                    model_version: TelemetryValue::Measured(format!("{model}-digest")),
                    quantization: TelemetryValue::Measured("q4".to_owned()),
                    representation_size_bytes: TelemetryValue::Measured(1_000),
                    backend_runtime_version: TelemetryValue::Measured("runtime-v1".to_owned()),
                },
                hardware: BenchmarkHardwareIdentity {
                    platform: "test".to_owned(),
                    gpu: TelemetryValue::Measured("gpu".to_owned()),
                    total_gpu_memory_bytes: TelemetryValue::Measured(12_000),
                    total_system_memory_bytes: TelemetryValue::Measured(32_000),
                },
                quality: QualityPreference::Balanced,
                workload_policy_version: "adr0135-workload-policy-v1".to_owned(),
                observed_workload: TelemetryValue::Measured(
                    InferenceWorkload::InteractiveReasoning,
                ),
                tool_budget: BenchmarkToolBudget {
                    max_model_turns: 24,
                    max_tool_failures: 4,
                    repair_budget: 2,
                    permission_budget: vec!["managed".to_owned()],
                    work_claims: vec!["code_path".to_owned()],
                },
                completion_criteria: task
                    .completion_criteria
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                execution: None,
            },
            metrics: BenchmarkMetrics {
                acceptance_success: TelemetryValue::Measured(successful),
                completion_success: TelemetryValue::Measured(successful),
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
                elapsed_ms: TelemetryValue::Measured(elapsed_ms),
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
            },
        }
    }

    #[test]
    fn workload_labels_are_stable_audit_values() {
        assert_eq!(
            RoutingWorkload::ValidationRepair.label(),
            "validation repair"
        );
        assert_eq!(
            RoutingWorkload::VisualEvaluation.label(),
            "visual evaluation"
        );
    }

    #[test]
    fn latency_policy_requires_at_least_five_percent_improvement() {
        let improves = |baseline: u64, specialist: u64| {
            specialist.saturating_mul(100)
                <= baseline.saturating_mul(100 - MIN_LATENCY_IMPROVEMENT_PERCENT)
        };
        assert!(improves(1_000, 950));
        assert!(improves(1_000, 900));
        assert!(!improves(1_000, 960));
        assert!(!improves(1_000, 1_000));
    }

    #[test]
    fn benchmark_qualified_specialist_is_selected_with_explicit_handoff() {
        let primary = local("primary");
        let specialist = local("specialist");
        let records = vec![
            measured_record("primary", 2, 1_000, true),
            measured_record("specialist", 2, 900, true),
        ];
        let policy = ModelRoutingPolicy::derive(primary, vec![specialist], &records);

        let decision = policy
            .select(RoutingWorkload::CodeImplementation, false)
            .expect("qualified specialist route");
        assert_eq!(decision.config.model_id(), "specialist");
        assert!(decision.context_handoff);
        assert!(!decision.fallback);
        assert!(
            decision
                .audit_summary()
                .contains(MODEL_ROUTER_POLICY_VERSION)
        );
    }

    #[test]
    fn unmeasured_advantage_keeps_the_single_model_baseline() {
        let primary = local("primary");
        let specialist = local("specialist");
        let records = vec![
            measured_record("primary", 2, 1_000, true),
            measured_record("specialist", 2, 960, true),
        ];
        let policy = ModelRoutingPolicy::derive(primary, vec![specialist], &records);

        let decision = policy
            .select(RoutingWorkload::CodeImplementation, false)
            .expect("baseline route");
        assert_eq!(decision.config.model_id(), "primary");
        assert!(!decision.context_handoff);
        assert_eq!(policy.adopted_specialist_count(), 0);
    }

    #[test]
    fn oom_regression_disqualifies_an_otherwise_faster_specialist() {
        let primary = local("primary");
        let specialist = local("specialist");
        let baseline = measured_record("primary", 2, 1_000, true);
        let mut regressed = measured_record("specialist", 2, 800, true);
        regressed.metrics.oom_failures = TelemetryValue::Measured(1);
        let policy = ModelRoutingPolicy::derive(primary, vec![specialist], &[baseline, regressed]);

        let decision = policy
            .select(RoutingWorkload::CodeImplementation, false)
            .expect("baseline route");
        assert_eq!(decision.config.model_id(), "primary");
    }

    #[test]
    fn visual_workload_fails_closed_without_image_capability_or_evidence() {
        let policy = ModelRoutingPolicy::derive(local("primary"), Vec::new(), &[]);
        assert!(matches!(
            policy.select(RoutingWorkload::VisualEvaluation, true),
            Err(ModelRoutingError::CapabilityUnavailable {
                workload: RoutingWorkload::VisualEvaluation,
                capability: "image input"
            })
        ));
    }

    #[test]
    fn successful_visual_benchmark_can_verify_image_capability() {
        let records = vec![measured_record("primary", 6, 1_000, true)];
        let policy = ModelRoutingPolicy::derive(local("primary"), Vec::new(), &records);
        let decision = policy
            .select(RoutingWorkload::VisualEvaluation, true)
            .expect("benchmark-verified visual route");
        assert_eq!(decision.config.model_id(), "primary");
    }

    #[test]
    fn fallback_returns_only_to_the_compatible_selected_baseline() {
        let primary = local("primary");
        let specialist = local("specialist");
        let records = vec![
            measured_record("primary", 2, 1_000, true),
            measured_record("specialist", 2, 900, true),
        ];
        let policy =
            ModelRoutingPolicy::derive(primary.clone(), vec![specialist.clone()], &records);
        let fallback = policy
            .fallback(&specialist, RoutingWorkload::CodeImplementation, false)
            .expect("specialist fallback");
        assert_eq!(fallback.config.model_id(), "primary");
        assert!(fallback.context_handoff);
        assert!(fallback.fallback);
        assert!(
            policy
                .fallback(&primary, RoutingWorkload::CodeImplementation, false)
                .is_none()
        );
    }
}
