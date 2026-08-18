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

use crate::agent_benchmark::{comparison_equivalence, BenchmarkRecord, ComparisonEquivalence};
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkExperimentSpec, BenchmarkRoutingMode,
    BenchmarkRunFailureKind, BenchmarkRunOutcome,
};
use crate::resource_arbitration::TelemetryValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub(crate) missing_runs: usize,
    pub(crate) passed_runs: usize,
    pub(crate) failed_runs: usize,
    pub(crate) unavailable_runs: usize,
    pub(crate) interrupted_runs: usize,
    pub(crate) backend_failures: usize,
    pub(crate) capability_unavailable_runs: usize,
    pub(crate) out_of_memory_failures: usize,
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
    /// Whether this model reported every planned run without an interruption.
    pub(crate) fn evidence_is_complete(&self) -> bool {
        self.planned_runs > 0
            && self.recorded_runs == self.planned_runs
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
                "planned benchmark run references a model outside the experiment".to_owned()
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
    let mut differences: Vec<String> = Vec::new();
    for records in by_task.values() {
        let Some((reference, rest)) = records.split_first() else {
            continue;
        };
        for record in rest {
            if let ComparisonEquivalence::NonEquivalent(changed) =
                comparison_equivalence(reference, record)
            {
                for dimension in changed {
                    let dimension = dimension.to_owned();
                    if !differences.contains(&dimension) {
                        differences.push(dimension);
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
