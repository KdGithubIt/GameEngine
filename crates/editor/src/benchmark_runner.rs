//! Headless entry point that executes one reproducible ADR 0142 experiment.
//!
//! The Editor binary is both the parent and the child of a benchmark suite. As
//! a parent it owns no project lease and opens no window: it loads a frozen
//! experiment spec, drives [`crate::benchmark_process`] until every planned run
//! has reported, and writes the comparison report. As a child it is started
//! once per run with `--benchmark-run` and exits after writing one result.
//!
//! Keeping the parent headless is what makes an 84-run suite usable. It can be
//! scripted, it survives a model that never answers, and it never mixes a
//! human-driven Editor session into measured evidence.

use crate::benchmark_comparison::{compare_experiment, BenchmarkExperimentComparison};
use crate::benchmark_experiment::{
    BenchmarkExperimentSpec, BenchmarkExperimentStore, BENCHMARK_FIXTURE_REPOSITORY_PATH,
    ENGINE_COMMIT_HEAD,
};
use crate::benchmark_process::{BenchmarkCoordinatorState, BenchmarkExperimentCoordinator};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default loopback endpoint of the Ollama-compatible local backend.
pub const DEFAULT_BENCHMARK_ENDPOINT: &str = "http://127.0.0.1:11434";

/// How often the parent checks whether the active child has exited.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Machine-local configuration for one experiment execution.
///
/// Nothing here belongs to the comparison identity. The endpoint, the fixture
/// checkout, and the Editor executable describe *this machine*, while the model
/// list, task list, repeat count, and completion criteria are frozen in the
/// spec file so a rerun on other hardware stays comparable or is reported as
/// non-equivalent.
#[derive(Debug, Clone)]
pub struct BenchmarkExperimentOptions {
    /// Frozen experiment spec to execute.
    pub spec_path: PathBuf,
    /// Local backend endpoint handed to every child run.
    pub endpoint: String,
    /// Repository-owned fixture template, resolved from the working directory
    /// when absent.
    pub fixture_template_root: Option<PathBuf>,
    /// Editor executable started per run, defaulting to the running binary.
    pub editor_executable: Option<PathBuf>,
    /// Wall-clock budget for a single run before it is treated as hung.
    pub run_timeout: Option<Duration>,
}

impl BenchmarkExperimentOptions {
    /// Creates options for `spec_path` using the local loopback defaults.
    pub fn new(spec_path: PathBuf) -> Self {
        Self {
            spec_path,
            endpoint: DEFAULT_BENCHMARK_ENDPOINT.to_owned(),
            fixture_template_root: None,
            editor_executable: None,
            run_timeout: None,
        }
    }
}

/// What one executed experiment produced.
#[derive(Debug, Clone)]
pub struct BenchmarkExperimentOutcome {
    /// Identifier of the executed experiment.
    pub experiment_id: String,
    /// Runs the frozen spec planned.
    pub planned_runs: usize,
    /// Runs that reported a result.
    pub completed_runs: usize,
    /// Runs whose outcome was `Passed`.
    pub passed_runs: usize,
    /// Written comparison report.
    pub comparison_path: PathBuf,
    /// Whether the evidence is complete and comparable enough to recommend.
    pub supports_recommendation: bool,
    /// Reason the suite stopped early, when it did.
    pub stopped_early: Option<String>,
    /// Human-readable comparison report for the executed experiment.
    pub report: String,
}

/// Executes one experiment to completion and writes its comparison report.
///
/// Returns an error only when the experiment could not be executed at all. A
/// failing model is normal benchmark evidence: it is recorded per run and
/// summarized in the report rather than aborting the suite, unless the spec
/// itself asked to stop on the first failure.
pub fn run_benchmark_experiment(
    options: BenchmarkExperimentOptions,
) -> Result<BenchmarkExperimentOutcome, String> {
    let spec = read_spec(&options.spec_path)?;
    // A spec file states which engine it is measuring, and nothing stopped it
    // from naming a commit this binary was not built from. Every recorded run
    // would then carry a false engine identity, which is precisely the
    // provenance ADR 0142 exists to guarantee.
    if !ENGINE_COMMIT_HEAD.is_empty() && spec.engine_commit_head != ENGINE_COMMIT_HEAD {
        return Err(format!(
            "experiment declares GameEngine {} but this Editor was built from {ENGINE_COMMIT_HEAD}",
            spec.engine_commit_head
        ));
    }
    let planned_runs = spec.planned_runs()?.len();
    let fixture_template_root = resolve_fixture_template(options.fixture_template_root.as_deref())?;
    let editor_executable = match options.editor_executable {
        Some(path) => path,
        None => std::env::current_exe().map_err(|error| {
            format!("benchmark parent could not resolve its own executable: {error}")
        })?,
    };
    let run_root = spec.output_destination.join("fixture-runs");
    let mut coordinator = BenchmarkExperimentCoordinator::new(
        spec.clone(),
        options.endpoint,
        editor_executable,
        fixture_template_root,
        run_root,
    )?;

    let mut stopped_early = None;
    let mut current_ordinal = None;
    let mut current_started = Instant::now();
    loop {
        coordinator.poll()?;
        match coordinator.state() {
            BenchmarkCoordinatorState::Complete { .. } => break,
            BenchmarkCoordinatorState::Failed { message, .. } => {
                stopped_early = Some(message);
                break;
            }
            BenchmarkCoordinatorState::Running { current, .. } => {
                if current_ordinal != Some(current.ordinal) {
                    current_ordinal = Some(current.ordinal);
                    current_started = Instant::now();
                }
                if let Some(timeout) = options.run_timeout
                    && current_started.elapsed() >= timeout
                {
                    coordinator.interrupt()?;
                    stopped_early = Some(format!(
                        "benchmark run {} on model `{}` exceeded its {} second budget",
                        current.ordinal,
                        current.model_id,
                        timeout.as_secs()
                    ));
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            BenchmarkCoordinatorState::Idle => std::thread::sleep(POLL_INTERVAL),
        }
    }

    let results = coordinator.results();
    let comparison = compare_experiment(&spec, results)?;
    let store = BenchmarkExperimentStore::open(spec.output_destination.clone())?;
    let comparison_path = store.write_report(&spec.experiment_id, "comparison", &comparison)?;
    Ok(BenchmarkExperimentOutcome {
        experiment_id: spec.experiment_id.clone(),
        planned_runs,
        completed_runs: results.len(),
        passed_runs: comparison
            .models
            .iter()
            .map(|model| model.passed_runs)
            .sum(),
        comparison_path,
        supports_recommendation: comparison.supports_recommendation(),
        stopped_early,
        report: format_comparison(&comparison),
    })
}

/// Renders a comparison report as the plain text the runner prints on exit.
///
/// Success and governed-loop cost lead; throughput follows. A metric no run
/// measured is printed as unavailable rather than as zero.
fn format_comparison(comparison: &BenchmarkExperimentComparison) -> String {
    let mut text = format!(
        "experiment {} on GameEngine {}\ncorpus {} / harness {} / fixture {}\n",
        comparison.experiment_id,
        comparison.engine_commit_head,
        comparison.corpus_version,
        comparison.harness_version,
        comparison.fixture_version,
    );
    text.push_str(&format!("equivalence: {:?}\n", comparison.equivalence));
    for model in &comparison.models {
        text.push_str(&format!(
            "\n  {}\n    runs {}/{} recorded, {} passed, {} failed, {} unavailable, {} missing\n",
            model.model_id,
            model.recorded_runs,
            model.planned_runs,
            model.passed_runs,
            model.failed_runs,
            model.unavailable_runs,
            model.missing_runs,
        ));
        text.push_str(&format!(
            "    completion gate {}, backend failures {}, OOM {}, capability unavailable {}\n",
            format_rate(model.completion_gate_success.permille()),
            model.backend_failures,
            model.out_of_memory_failures,
            model.capability_unavailable_runs,
        ));
        text.push_str(&format!(
            "    turns {}, tool calls {}, invalid tool calls {}, repair loops {}, validation attempts {}\n",
            format_mean(model.model_turns.measured_mean_milli()),
            format_mean(model.tool_calls.measured_mean_milli()),
            format_mean(model.invalid_or_failed_tool_calls.measured_mean_milli()),
            format_mean(model.repair_loops.measured_mean_milli()),
            format_mean(model.validation_attempts.measured_mean_milli()),
        ));
        text.push_str(&format!(
            "    elapsed ms {}, load ms {}, TTFT ms {}, tokens/s {}\n",
            format_mean(model.elapsed_ms.measured_mean_milli()),
            format_mean(model.load_latency_ms.measured_mean_milli()),
            format_mean(model.ttft_ms.measured_mean_milli()),
            format_mean(
                model
                    .generation_tokens_per_second_milli
                    .measured_mean_milli()
                    .map(|value| value / 1000)
            ),
        ));
        text.push_str(&format!(
            "    runtime interaction {}, visual evaluation {}\n",
            format_rate(model.runtime_interaction_success.permille()),
            format_rate(model.visual_evaluation_success.permille()),
        ));
    }
    if !comparison.supports_recommendation() {
        text.push_str(
            "\nevidence is incomplete or non-equivalent; no catalog recommendation is derived\n",
        );
    }
    text
}

fn format_rate(permille: Option<u64>) -> String {
    match permille {
        Some(value) => format!("{}.{}%", value / 10, value % 10),
        None => "unavailable".to_owned(),
    }
}

fn format_mean(mean_milli: Option<u64>) -> String {
    match mean_milli {
        Some(value) => format!("{}.{:03}", value / 1000, value % 1000),
        None => "unavailable".to_owned(),
    }
}

fn read_spec(path: &Path) -> Result<BenchmarkExperimentSpec, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
    let spec = serde_json::from_slice::<BenchmarkExperimentSpec>(&bytes)
        .map_err(|error| format!("could not parse `{}`: {error}", path.display()))?;
    spec.validate()?;
    Ok(spec)
}

/// Resolves the repository-owned fixture template.
///
/// The benchmark deliberately refuses to fall back to any project the user
/// happens to have open: a private project must never become benchmark corpus.
fn resolve_fixture_template(override_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = override_root {
        if !root.is_dir() {
            return Err(format!(
                "benchmark fixture template `{}` is unavailable",
                root.display()
            ));
        }
        return Ok(root.to_path_buf());
    }
    let working_directory = std::env::current_dir()
        .map_err(|error| format!("benchmark parent could not read its directory: {error}"))?;
    for candidate in working_directory.ancestors() {
        let fixture = candidate.join(BENCHMARK_FIXTURE_REPOSITORY_PATH);
        if fixture.is_dir() {
            return Ok(fixture);
        }
    }
    Err(format!(
        "benchmark fixture `{BENCHMARK_FIXTURE_REPOSITORY_PATH}` was not found above `{}`; pass an explicit fixture root",
        working_directory.display()
    ))
}
