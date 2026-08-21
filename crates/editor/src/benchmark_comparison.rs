//! Model-only comparison over a reproducible ADR 0142 benchmark experiment.
//!
//! [`crate::benchmark_experiment`] owns experiment identity and run isolation;
//! this module turns the resulting per-run records into the comparison the
//! product actually needs. Three rules shape every type here.
//!
//! Raw throughput is never the ranking. Task and completion-gate success, the
//! governed-loop cost of reaching that success, and resource pressure are all
//! reported side by side so a recommendation cannot be justified by tokens per
//! second alone.
//!
//! Unavailable telemetry stays unavailable. A metric that no run measured is
//! reported with a zero measured count rather than a zero value, and a
//! conservative backend estimate is counted apart from a real measurement so an
//! estimate can never be presented as measured evidence.
//!
//! Model-only ranking is permitted only when every non-model dimension matched.
//! Otherwise the comparison is returned as explicitly non-equivalent, and no
//! catalog recommendation may be derived from it.

#![allow(dead_code)]

use crate::agent_benchmark::{BenchmarkRecord, ComparisonEquivalence, comparison_equivalence};
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkExperimentSpec, BenchmarkRoutingMode,
    BenchmarkRunFailureKind, BenchmarkRunOutcome,
};
use crate::resource_arbitration::TelemetryValue;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const RUNTIME_INTERACTION_TASK: &str = "runtime_interaction_v1";
const VISUAL_EVALUATION_TASK: &str = "visual_evaluation_v1";

/// One numeric metric aggregated over the runs of a single model.
///
/// Measured, conservatively estimated, and unavailable observations are counted
/// separately, and only measured observations contribute to the total and to
/// the reported range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkMetricAggregate {
    pub(crate) measured_runs: usize,
    pub(crate) estimated_runs: usize,
    pub(crate) unavailable_runs: usize,
    pub(crate) measured_total: u64,
    pub(crate) measured_minimum: Option<u64>,
    pub(crate) measured_maximum: Option<u64>,
}

impl BenchmarkMetricAggregate {
    fn observe(&mut self, value: &TelemetryValue<u64>) {
        match value {
            TelemetryValue::Measured(measured) => {
                self.measured_runs += 1;
                self.measured_total = self.measured_total.saturating_add(*measured);
                self.measured_minimum = Some(
                    self.measured_minimum
                        .map_or(*measured, |current| current.min(*measured)),
                );
                self.measured_maximum = Some(
                    self.measured_maximum
                        .map_or(*measured, |current| current.max(*measured)),
                );
            }
            TelemetryValue::ConservativeEstimate(_) => self.estimated_runs += 1,
            TelemetryValue::Unavailable => self.unavailable_runs += 1,
        }
    }

    /// Mean over measured runs only, scaled by 1000 to stay integral.
    ///
    /// Returns `None` when nothing was measured, so a caller cannot mistake an
    /// unavailable metric for a measured zero.
    pub(crate) fn measured_mean_milli(&self) -> Option<u64> {
        if self.measured_runs == 0 {
            return None;
        }
        Some(self.measured_total.saturating_mul(1000) / self.measured_runs as u64)
    }

    /// Whether this metric was never measured across the compared runs.
    pub(crate) fn is_unavailable(&self) -> bool {
        self.measured_runs == 0
    }
}

/// A success count paired with the number of runs that could report it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkSuccessRate {
    pub(crate) successes: usize,
    pub(crate) observations: usize,
}

impl BenchmarkSuccessRate {
    fn observe(&mut self, success: bool) {
        self.observations += 1;
        if success {
            self.successes += 1;
        }
    }

    /// Success share in parts per thousand, or `None` without observations.
    pub(crate) fn permille(&self) -> Option<u64> {
        if self.observations == 0 {
            return None;
        }
        Some((self.successes as u64).saturating_mul(1000) / self.observations as u64)
    }
}

/// Whether the compared runs differ only by model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkComparisonEquivalence {
    /// Every non-model dimension matched; a model-only ranking is meaningful.
    EquivalentModelComparison,
    /// At least one non-model dimension changed, naming each changed dimension.
    NonEquivalent { differences: Vec<String> },
    /// Too few complete records exist to decide equivalence at all.
    InsufficientEvidence { reason: String },
}

/// Everything the product compares for one exact model representation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkModelComparison {
    pub(crate) model_id: String,
    pub(crate) planned_runs: usize,
    pub(crate) recorded_runs: usize,
    pub(crate) runs_with_record: usize,
    pub(crate) missing_runs: usize,
    pub(crate) passed_runs: usize,
    pub(crate) failed_runs: usize,
    pub(crate) unavailable_runs: usize,
    pub(crate) interrupted_runs: usize,
    pub(crate) backend_failures: usize,
    pub(crate) capability_unavailable_runs: usize,
    pub(crate) out_of_memory_failures: usize,
    pub(crate) timeout_failures: usize,
    pub(crate) task_success: BTreeMap<String, BenchmarkSuccessRate>,
    pub(crate) acceptance_success: BenchmarkSuccessRate,
    pub(crate) completion_gate_success: BenchmarkSuccessRate,
    pub(crate) runtime_interaction_success: BenchmarkSuccessRate,
    pub(crate) visual_evaluation_success: BenchmarkSuccessRate,
    pub(crate) model_turns: BenchmarkMetricAggregate,
    pub(crate) tool_calls: BenchmarkMetricAggregate,
    pub(crate) invalid_or_failed_tool_calls: BenchmarkMetricAggregate,
    pub(crate) code_edits: BenchmarkMetricAggregate,
    pub(crate) validation_attempts: BenchmarkMetricAggregate,
    pub(crate) repair_loops: BenchmarkMetricAggregate,
    pub(crate) human_interventions: BenchmarkMetricAggregate,
    pub(crate) elapsed_ms: BenchmarkMetricAggregate,
    pub(crate) prompt_tokens: BenchmarkMetricAggregate,
    pub(crate) response_tokens: BenchmarkMetricAggregate,
    pub(crate) load_latency_ms: BenchmarkMetricAggregate,
    pub(crate) ttft_ms: BenchmarkMetricAggregate,
    pub(crate) generation_tokens_per_second_milli: BenchmarkMetricAggregate,
    pub(crate) peak_backend_gpu_memory_bytes: BenchmarkMetricAggregate,
    pub(crate) peak_editor_gpu_memory_bytes: BenchmarkMetricAggregate,
    pub(crate) oom_failures: BenchmarkMetricAggregate,
}

impl BenchmarkModelComparison {
    /// Whether every planned run produced usable measured evidence.
    ///
    /// A run that reported only a backend failure is a recorded outcome but not
    /// evidence: it carries no measured metrics and no representation identity.
    /// Counting it as complete would let a model that never answered qualify a
    /// catalog recommendation, so a missing record fails this check.
    pub(crate) fn evidence_is_complete(&self) -> bool {
        self.planned_runs > 0
            && self.recorded_runs == self.planned_runs
            && self.runs_with_record == self.planned_runs
            && self.interrupted_runs == 0
    }

    fn observe_outcome(&mut self, result: &BenchmarkExperimentResult) {
        self.recorded_runs += 1;
        match result.outcome {
            BenchmarkRunOutcome::Passed => self.passed_runs += 1,
            BenchmarkRunOutcome::Failed => self.failed_runs += 1,
            BenchmarkRunOutcome::Unavailable => self.unavailable_runs += 1,
            BenchmarkRunOutcome::Interrupted => self.interrupted_runs += 1,
        }
        match result.failure_kind {
            Some(BenchmarkRunFailureKind::Backend) => self.backend_failures += 1,
            Some(BenchmarkRunFailureKind::CapabilityUnavailable) => {
                self.capability_unavailable_runs += 1
            }
            Some(BenchmarkRunFailureKind::OutOfMemory) => self.out_of_memory_failures += 1,
            Some(BenchmarkRunFailureKind::Timeout) => self.timeout_failures += 1,
            _ => {}
        }
        let passed = result.outcome == BenchmarkRunOutcome::Passed;
        self.task_success
            .entry(result.run.task_id.clone())
            .or_default()
            .observe(passed);
        match result.run.task_id.as_str() {
            RUNTIME_INTERACTION_TASK => self.runtime_interaction_success.observe(passed),
            VISUAL_EVALUATION_TASK => self.visual_evaluation_success.observe(passed),
            _ => {}
        }
    }

    fn observe_record(&mut self, record: &BenchmarkRecord) {
        self.runs_with_record += 1;
        let metrics = &record.metrics;
        if let TelemetryValue::Measured(success) = metrics.acceptance_success {
            self.acceptance_success.observe(success);
        }
        if let TelemetryValue::Measured(success) = metrics.completion_success {
            self.completion_gate_success.observe(success);
        }
        self.model_turns.observe(&metrics.model_turns);
        self.tool_calls.observe(&metrics.tool_calls);
        self.invalid_or_failed_tool_calls
            .observe(&metrics.invalid_or_failed_tool_calls);
        self.code_edits.observe(&metrics.code_edits);
        self.validation_attempts
            .observe(&metrics.validation_attempts);
        self.repair_loops.observe(&metrics.repair_loops);
        self.human_interventions
            .observe(&metrics.human_interventions);
        self.elapsed_ms.observe(&metrics.elapsed_ms);
        self.prompt_tokens.observe(&metrics.prompt_tokens);
        self.response_tokens.observe(&metrics.response_tokens);
        self.load_latency_ms.observe(&metrics.load_latency_ms);
        self.ttft_ms.observe(&metrics.ttft_ms);
        self.generation_tokens_per_second_milli
            .observe(&metrics.generation_tokens_per_second_milli);
        self.peak_backend_gpu_memory_bytes
            .observe(&metrics.peak_backend_gpu_memory_bytes);
        self.peak_editor_gpu_memory_bytes
            .observe(&metrics.peak_editor_gpu_memory_bytes);
        self.oom_failures.observe(&metrics.oom_failures);
    }
}

/// A whole experiment reduced to a comparable, provenance-carrying report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkExperimentComparison {
    pub(crate) experiment_id: String,
    pub(crate) engine_commit_head: String,
    pub(crate) corpus_version: String,
    pub(crate) harness_version: String,
    pub(crate) fixture_version: String,
    pub(crate) routing_mode: BenchmarkRoutingMode,
    pub(crate) equivalence: BenchmarkComparisonEquivalence,
    pub(crate) models: Vec<BenchmarkModelComparison>,
}

impl BenchmarkExperimentComparison {
    /// Whether this experiment may justify a curated catalog recommendation.
    ///
    /// ADR 0142 derives Lightweight, Balanced, and High Quality slots only from
    /// complete comparable evidence. An incomplete or non-equivalent experiment
    /// therefore produces no recommendation instead of a guessed default.
    pub(crate) fn supports_recommendation(&self) -> bool {
        self.equivalence == BenchmarkComparisonEquivalence::EquivalentModelComparison
            && self.models.len() > 1
            && self
                .models
                .iter()
                .all(BenchmarkModelComparison::evidence_is_complete)
    }
}

/// Reduces one experiment's results to a model-only comparison report.
///
/// Every result is revalidated against the frozen spec first, so a routed or
/// mismatched run cannot silently enter a single-model comparison.
pub(crate) fn compare_experiment(
    spec: &BenchmarkExperimentSpec,
    results: &[BenchmarkExperimentResult],
) -> Result<BenchmarkExperimentComparison, String> {
    spec.validate()?;
    let mut models = spec
        .model_ids
        .iter()
        .map(|model_id| {
            (
                model_id.clone(),
                BenchmarkModelComparison {
                    model_id: model_id.clone(),
                    ..BenchmarkModelComparison::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for planned in spec.planned_runs()? {
        let Some(model) = models.get_mut(&planned.model_id) else {
            return Err(
                "planned benchmark run references a model outside the experiment".to_owned(),
            );
        };
        model.planned_runs += 1;
    }
    for result in results {
        result.validate_against(spec)?;
        let Some(model) = models.get_mut(&result.run.model_id) else {
            return Err("benchmark result references a model outside the experiment".to_owned());
        };
        model.observe_outcome(result);
        if let Some(record) = result.record.as_ref() {
            model.observe_record(record);
        }
    }
    for model in models.values_mut() {
        model.missing_runs = model.planned_runs.saturating_sub(model.recorded_runs);
    }
    Ok(BenchmarkExperimentComparison {
        experiment_id: spec.experiment_id.clone(),
        engine_commit_head: spec.engine_commit_head.clone(),
        corpus_version: spec.corpus_version.clone(),
        harness_version: spec.harness_version.clone(),
        fixture_version: spec.fixture_version.clone(),
        routing_mode: spec.routing_mode,
        equivalence: experiment_equivalence(spec, results),
        models: models.into_values().collect(),
    })
}

/// Decides whether the recorded runs differ only by model.
///
/// Records of the same task are compared pairwise through the ADR 0142
/// equivalence rule. Because that rule also rejects unmeasured model, hardware,
/// and workload identity, a run that could not describe its own representation
/// makes the whole experiment non-equivalent rather than silently comparable.
fn experiment_equivalence(
    spec: &BenchmarkExperimentSpec,
    results: &[BenchmarkExperimentResult],
) -> BenchmarkComparisonEquivalence {
    let mut by_task: BTreeMap<&str, Vec<&BenchmarkRecord>> = BTreeMap::new();
    for result in results {
        if let Some(record) = result.record.as_ref() {
            by_task
                .entry(result.run.task_id.as_str())
                .or_default()
                .push(record);
        }
    }
    if by_task.is_empty() {
        return BenchmarkComparisonEquivalence::InsufficientEvidence {
            reason: "no benchmark run produced a record".to_owned(),
        };
    }
    if spec.model_ids.len() < 2 {
        return BenchmarkComparisonEquivalence::InsufficientEvidence {
            reason: "a model-only comparison needs at least two model representations".to_owned(),
        };
    }
    // Two planned models are not two compared models. When only one of them
    // produced a record there is nothing to compare it against, and reporting
    // that as an equivalent model comparison would dress a single measured
    // model up as a ranking.
    let recorded_models = results
        .iter()
        .filter(|result| result.record.is_some())
        .map(|result| result.run.model_id.as_str())
        .collect::<BTreeSet<_>>();
    if recorded_models.len() < 2 {
        return BenchmarkComparisonEquivalence::InsufficientEvidence {
            reason: format!(
                "only {} model representation(s) produced a record; the others reported no measured evidence",
                recorded_models.len()
            ),
        };
    }
    let mut differences: Vec<String> = Vec::new();
    for records in by_task.values() {
        let Some((reference, rest)) = records.split_first() else {
            continue;
        };
        for record in rest {
            match comparison_equivalence(reference, record) {
                ComparisonEquivalence::EquivalentModelComparison => {}
                ComparisonEquivalence::EquivalentAgentHarnessComparison => {
                    let dimension = "agent_harness_comparison".to_owned();
                    if !differences.contains(&dimension) {
                        differences.push(dimension);
                    }
                }
                ComparisonEquivalence::EquivalentCodingAgentComparison => {
                    let dimension = "coding_agent_comparison".to_owned();
                    if !differences.contains(&dimension) {
                        differences.push(dimension);
                    }
                }
                ComparisonEquivalence::NonEquivalent(changed) => {
                    for dimension in changed {
                        let dimension = dimension.to_owned();
                        if !differences.contains(&dimension) {
                            differences.push(dimension);
                        }
                    }
                }
            }
        }
    }
    if differences.is_empty() {
        BenchmarkComparisonEquivalence::EquivalentModelComparison
    } else {
        differences.sort();
        BenchmarkComparisonEquivalence::NonEquivalent { differences }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_benchmark::{
        BENCHMARK_CORPUS_VERSION, BENCHMARK_HARNESS_VERSION, BENCHMARK_SCHEMA_VERSION,
        BenchmarkHardwareIdentity, BenchmarkIdentity, BenchmarkMetrics, BenchmarkModelIdentity,
        BenchmarkToolBudget, WORKLOAD_POLICY_VERSION,
    };
    use crate::benchmark_experiment::{
        BENCHMARK_FIXTURE_VERSION, BenchmarkExperimentSpec, BenchmarkPlannedRun,
    };
    use crate::resource_arbitration::{InferenceWorkload, QualityPreference};
    use std::path::PathBuf;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const READ_TASK: &str = "read_question_v1";

    fn spec(models: &[&str], tasks: &[&str], repeat: u32) -> BenchmarkExperimentSpec {
        BenchmarkExperimentSpec::local_single_model_comparison(
            "comparison",
            HEAD,
            models.iter().map(|model| (*model).to_owned()).collect(),
            tasks.iter().map(|task| (*task).to_owned()).collect(),
            repeat,
            QualityPreference::Balanced,
            PathBuf::from("results"),
        )
    }

    fn identity(model: &str, task: &str) -> BenchmarkIdentity {
        BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: task.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: "runtime-harness-v1".to_owned(),
            runtime: None,
            model: BenchmarkModelIdentity {
                backend_id: "ollama-compatible".to_owned(),
                model_id: model.to_owned(),
                model_version: TelemetryValue::Measured(format!("{model}-digest")),
                quantization: TelemetryValue::Measured("q4_k_m".to_owned()),
                representation_size_bytes: TelemetryValue::Measured(16_000_000_000),
                backend_runtime_version: TelemetryValue::Measured("ollama-0.1.0".to_owned()),
            },
            hardware: BenchmarkHardwareIdentity {
                platform: "windows".to_owned(),
                gpu: TelemetryValue::Measured("RTX 4070 Ti".to_owned()),
                total_gpu_memory_bytes: TelemetryValue::Measured(12_884_901_888),
                total_system_memory_bytes: TelemetryValue::Measured(68_719_476_736),
            },
            quality: QualityPreference::Balanced,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(InferenceWorkload::InteractiveReasoning),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 24,
                max_tool_failures: 4,
                repair_budget: 2,
                permission_budget: vec!["managed".to_owned()],
                work_claims: vec!["code_path".to_owned()],
            },
            completion_criteria: vec![
                "answer_returned".to_owned(),
                "provenance_reported".to_owned(),
            ],
            execution: None,
        }
    }

    fn metrics() -> BenchmarkMetrics {
        BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(true),
            completion_success: TelemetryValue::Measured(true),
            model_turns: TelemetryValue::Measured(2),
            tool_calls: TelemetryValue::Measured(3),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
            code_edits: TelemetryValue::Measured(0),
            validation_attempts: TelemetryValue::Measured(0),
            repair_loops: TelemetryValue::Measured(0),
            play_attempts: TelemetryValue::Measured(0),
            frame_capture_attempts: TelemetryValue::Measured(0),
            visual_evaluation_attempts: TelemetryValue::Measured(0),
            human_interventions: TelemetryValue::Measured(0),
            elapsed_ms: TelemetryValue::Measured(1_000),
            prompt_tokens: TelemetryValue::Measured(100),
            response_tokens: TelemetryValue::Measured(50),
            load_latency_ms: TelemetryValue::Measured(400),
            ttft_ms: TelemetryValue::Unavailable,
            generation_tokens_per_second_milli: TelemetryValue::Measured(25_000),
            peak_backend_gpu_memory_bytes: TelemetryValue::ConservativeEstimate(9_000_000_000),
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Measured(0),
        }
    }

    fn record(model: &str, task: &str) -> BenchmarkRecord {
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity: identity(model, task),
            metrics: metrics(),
        }
    }

    fn passed(model: &str, task: &str, repetition: u32, ordinal: u64) -> BenchmarkExperimentResult {
        BenchmarkExperimentResult {
            experiment_id: "comparison".to_owned(),
            engine_commit_head: HEAD.to_owned(),
            fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
            routing_mode: BenchmarkRoutingMode::SingleModel,
            run: BenchmarkPlannedRun {
                ordinal,
                model_id: model.to_owned(),
                task_id: task.to_owned(),
                repetition,
            },
            started_unix_ms: 1,
            finished_unix_ms: 2,
            outcome: BenchmarkRunOutcome::Passed,
            failure_kind: None,
            routed_to_another_model: false,
            harness_message: None,
            record: Some(record(model, task)),
        }
    }

    fn every_run(spec: &BenchmarkExperimentSpec) -> Vec<BenchmarkExperimentResult> {
        spec.planned_runs()
            .expect("plan")
            .into_iter()
            .map(|run| passed(&run.model_id, &run.task_id, run.repetition, run.ordinal))
            .collect()
    }

    #[test]
    fn unmeasured_telemetry_stays_unavailable_instead_of_becoming_zero() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        let model = comparison.models.first().expect("model");
        assert!(model.ttft_ms.is_unavailable());
        assert_eq!(model.ttft_ms.measured_mean_milli(), None);
        assert_eq!(model.ttft_ms.measured_total, 0);
        assert_eq!(model.ttft_ms.unavailable_runs, 1);
    }

    #[test]
    fn conservative_estimates_are_never_counted_as_measurements() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        let model = comparison.models.first().expect("model");
        assert_eq!(model.peak_backend_gpu_memory_bytes.estimated_runs, 1);
        assert_eq!(model.peak_backend_gpu_memory_bytes.measured_runs, 0);
        assert!(model.peak_backend_gpu_memory_bytes.is_unavailable());
    }

    #[test]
    fn repeated_runs_of_one_model_aggregate_into_a_single_row() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 3);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        assert_eq!(comparison.models.len(), 2);
        let model = comparison.models.first().expect("model");
        assert_eq!(model.planned_runs, 3);
        assert_eq!(model.recorded_runs, 3);
        assert_eq!(model.passed_runs, 3);
        assert_eq!(model.missing_runs, 0);
        assert_eq!(model.elapsed_ms.measured_runs, 3);
        assert_eq!(model.elapsed_ms.measured_mean_milli(), Some(1_000_000));
    }

    #[test]
    fn one_model_result_never_enters_another_model_row() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        for model in &comparison.models {
            assert_eq!(model.recorded_runs, 1);
            assert_eq!(
                model
                    .task_success
                    .get(READ_TASK)
                    .expect("task")
                    .observations,
                1
            );
        }
    }

    #[test]
    fn equivalent_records_permit_a_model_only_comparison() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 2);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        assert_eq!(
            comparison.equivalence,
            BenchmarkComparisonEquivalence::EquivalentModelComparison
        );
        assert!(comparison.supports_recommendation());
    }

    #[test]
    fn a_changed_backend_runtime_makes_the_experiment_non_equivalent() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let mut results = every_run(&spec);
        if let Some(record) = results[1].record.as_mut() {
            record.identity.model.backend_runtime_version =
                TelemetryValue::Measured("ollama-0.2.0".to_owned());
        }
        let comparison = compare_experiment(&spec, &results).expect("compare");
        assert_eq!(
            comparison.equivalence,
            BenchmarkComparisonEquivalence::NonEquivalent {
                differences: vec!["backend_runtime".to_owned()],
            }
        );
        assert!(!comparison.supports_recommendation());
    }

    #[test]
    fn incomplete_evidence_blocks_a_catalog_recommendation() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 2);
        let mut results = every_run(&spec);
        results.pop();
        let comparison = compare_experiment(&spec, &results).expect("compare");
        assert!(comparison.models.iter().any(|model| model.missing_runs > 0));
        assert!(!comparison.supports_recommendation());
    }

    #[test]
    fn a_single_model_experiment_is_never_presented_as_a_comparison() {
        let spec = spec(&["model-a"], &[READ_TASK], 2);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        assert!(matches!(
            comparison.equivalence,
            BenchmarkComparisonEquivalence::InsufficientEvidence { .. }
        ));
        assert!(!comparison.supports_recommendation());
    }

    #[test]
    fn backend_and_out_of_memory_failures_are_attributed_to_their_own_model() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 2);
        let mut results = every_run(&spec);
        results[0].outcome = BenchmarkRunOutcome::Failed;
        results[0].failure_kind = Some(BenchmarkRunFailureKind::Backend);
        results[0].record = None;
        results[1].outcome = BenchmarkRunOutcome::Failed;
        results[1].failure_kind = Some(BenchmarkRunFailureKind::OutOfMemory);
        results[1].record = None;
        let comparison = compare_experiment(&spec, &results).expect("compare");
        let failing = comparison
            .models
            .iter()
            .find(|model| model.model_id == "model-a")
            .expect("model-a");
        assert_eq!(failing.backend_failures, 1);
        assert_eq!(failing.out_of_memory_failures, 1);
        assert_eq!(failing.failed_runs, 2);
        assert_eq!(failing.passed_runs, 0);
        let healthy = comparison
            .models
            .iter()
            .find(|model| model.model_id == "model-b")
            .expect("model-b");
        assert_eq!(healthy.backend_failures, 0);
        assert_eq!(healthy.passed_runs, 2);
    }

    #[test]
    fn a_missing_visual_capability_is_unavailable_rather_than_a_fabricated_success() {
        let visual = "visual_evaluation_v1";
        let spec = spec(&["model-a", "model-b"], &[visual], 1);
        let mut results = every_run(&spec);
        results[0].outcome = BenchmarkRunOutcome::Unavailable;
        results[0].failure_kind = Some(BenchmarkRunFailureKind::CapabilityUnavailable);
        results[0].record = None;
        let comparison = compare_experiment(&spec, &results).expect("compare");
        let text_only = comparison
            .models
            .iter()
            .find(|model| model.model_id == "model-a")
            .expect("model-a");
        assert_eq!(text_only.capability_unavailable_runs, 1);
        assert_eq!(text_only.unavailable_runs, 1);
        assert_eq!(text_only.passed_runs, 0);
        assert_eq!(text_only.visual_evaluation_success.successes, 0);
        assert_eq!(text_only.visual_evaluation_success.observations, 1);
        assert_eq!(text_only.visual_evaluation_success.permille(), Some(0));
    }

    #[test]
    fn a_completed_repair_cycle_is_visible_in_the_comparison() {
        let repair = "validation_repair_v1";
        let spec = spec(&["model-a", "model-b"], &[repair], 1);
        let mut results = every_run(&spec);
        for result in &mut results {
            if let Some(record) = result.record.as_mut() {
                record.metrics.validation_attempts = TelemetryValue::Measured(2);
                record.metrics.repair_loops = TelemetryValue::Measured(1);
                record.metrics.code_edits = TelemetryValue::Measured(1);
            }
        }
        let comparison = compare_experiment(&spec, &results).expect("compare");
        let model = comparison.models.first().expect("model");
        assert_eq!(model.validation_attempts.measured_minimum, Some(2));
        assert_eq!(model.repair_loops.measured_total, 1);
        assert_eq!(model.completion_gate_success.permille(), Some(1_000));
        assert_eq!(model.task_success.get(repair).expect("task").successes, 1);
    }

    #[test]
    fn a_routed_result_cannot_enter_a_single_model_comparison() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let mut results = every_run(&spec);
        results[0].routed_to_another_model = true;
        assert!(compare_experiment(&spec, &results).is_err());
    }

    /// Regression: a real two-model run where one model never answered.
    ///
    /// The failing model still wrote a result file, so both models looked
    /// "fully recorded" and the surviving model had nothing to be compared
    /// against. The experiment was reported as an equivalent model comparison
    /// that supported a recommendation, on the evidence of one model.
    #[test]
    fn one_model_answering_is_not_an_equivalent_two_model_comparison() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 1);
        let mut results = every_run(&spec);
        let failing = results
            .iter_mut()
            .find(|result| result.run.model_id == "model-b")
            .expect("model-b run");
        failing.outcome = BenchmarkRunOutcome::Failed;
        failing.failure_kind = Some(BenchmarkRunFailureKind::Backend);
        failing.record = None;

        let comparison = compare_experiment(&spec, &results).expect("compare");
        assert!(
            matches!(
                comparison.equivalence,
                BenchmarkComparisonEquivalence::InsufficientEvidence { .. }
            ),
            "one recorded model cannot be an equivalent comparison, got {:?}",
            comparison.equivalence
        );
        assert!(!comparison.supports_recommendation());
    }

    #[test]
    fn a_run_that_produced_no_record_is_not_complete_evidence() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 2);
        let mut results = every_run(&spec);
        let failing = results
            .iter_mut()
            .find(|result| result.run.model_id == "model-b")
            .expect("model-b run");
        failing.outcome = BenchmarkRunOutcome::Failed;
        failing.failure_kind = Some(BenchmarkRunFailureKind::Backend);
        failing.record = None;

        let comparison = compare_experiment(&spec, &results).expect("compare");
        let failed = comparison
            .models
            .iter()
            .find(|model| model.model_id == "model-b")
            .expect("model-b");
        assert_eq!(failed.recorded_runs, 2);
        assert_eq!(failed.runs_with_record, 1);
        assert_eq!(failed.missing_runs, 0);
        assert!(
            !failed.evidence_is_complete(),
            "a backend failure carries no measured evidence"
        );
        assert!(!comparison.supports_recommendation());
    }

    #[test]
    fn complete_evidence_still_qualifies_when_every_run_recorded_metrics() {
        let spec = spec(&["model-a", "model-b"], &[READ_TASK], 2);
        let comparison = compare_experiment(&spec, &every_run(&spec)).expect("compare");
        for model in &comparison.models {
            assert_eq!(model.runs_with_record, model.planned_runs);
            assert!(model.evidence_is_complete());
        }
        assert!(comparison.supports_recommendation());
    }
}
