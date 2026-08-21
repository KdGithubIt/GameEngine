//! Process-isolated execution for reproducible GameEngine Agent Benchmark runs.
//!
//! Every planned run receives a fresh repository-owned fixture copy and a fresh
//! Editor process. This prevents authoring, generated files, validation, Play,
//! proposal, permission, and model-run state from leaking between models.

#![allow(dead_code)]

use crate::agent_benchmark::BenchmarkExecutionIdentity;
use crate::agent_benchmark_campaign::CandidateTaskContract;
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkExperimentSpec, BenchmarkExperimentStore,
    BenchmarkFixtureSandbox, BenchmarkPlannedRun, BenchmarkRoutingMode, BenchmarkRunFailureKind,
    BenchmarkRunOutcome,
};
use crate::managed_local_runtime::{MANAGED_BACKEND_ID, ManagedExecutionEnvironment};
use crate::resource_arbitration::QualityPreference;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub(crate) const BENCHMARK_CHILD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkChildRunSpec {
    pub(crate) schema_version: u32,
    pub(crate) experiment_id: String,
    pub(crate) engine_commit_head: String,
    pub(crate) fixture_version: String,
    pub(crate) backend_id: String,
    #[serde(default)]
    pub(crate) managed_execution_environment: Option<ManagedExecutionEnvironment>,
    pub(crate) endpoint: String,
    pub(crate) model_id: String,
    pub(crate) task_id: String,
    pub(crate) repetition: u32,
    pub(crate) ordinal: u64,
    pub(crate) quality: QualityPreference,
    pub(crate) routing_mode: BenchmarkRoutingMode,
    pub(crate) result_path: PathBuf,
    /// ADR 0156 execution identity this child stamps onto its record.
    #[serde(default)]
    pub(crate) execution_identity: Option<BenchmarkExecutionIdentity>,
    /// ADR 0156 candidate-visible contract for this task, when a campaign owns it.
    #[serde(default)]
    pub(crate) candidate_contract: Option<CandidateTaskContract>,
}

impl BenchmarkChildRunSpec {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != BENCHMARK_CHILD_SCHEMA_VERSION {
            return Err(format!(
                "unsupported benchmark child schema {}",
                self.schema_version
            ));
        }
        if self.experiment_id.trim().is_empty()
            || self.backend_id.trim().is_empty()
            || self.model_id.trim().is_empty()
            || self.task_id.trim().is_empty()
        {
            return Err("benchmark child identity fields must be non-empty".to_owned());
        }
        if self.backend_id == MANAGED_BACKEND_ID {
            if self.managed_execution_environment.is_none() {
                return Err(
                    "managed benchmark child requires an exact execution environment".to_owned(),
                );
            }
        } else {
            if self.endpoint.trim().is_empty() {
                return Err(
                    "compatible-backend benchmark child requires a local endpoint".to_owned(),
                );
            }
            if self.managed_execution_environment.is_some() {
                return Err(
                    "compatible-backend benchmark child cannot carry managed runtime identity"
                        .to_owned(),
                );
            }
        }
        if self.engine_commit_head.len() != 40
            || !self
                .engine_commit_head
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("benchmark child requires an exact 40-character engine SHA".to_owned());
        }
        if self.routing_mode != BenchmarkRoutingMode::SingleModel {
            return Err(
                "first-release benchmark child executes only strict single-model baselines"
                    .to_owned(),
            );
        }
        if self.result_path.as_os_str().is_empty() {
            return Err("benchmark child requires an explicit result destination".to_owned());
        }
        Ok(())
    }

    pub(crate) fn read(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        let spec = serde_json::from_slice::<Self>(&bytes).map_err(|error| error.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        fs::write(path, bytes).map_err(|error| error.to_string())
    }

    pub(crate) fn planned_run(&self) -> BenchmarkPlannedRun {
        BenchmarkPlannedRun {
            ordinal: self.ordinal,
            model_id: self.model_id.clone(),
            task_id: self.task_id.clone(),
            repetition: self.repetition,
        }
    }

    /// Returns the benchmark lane/runtime frozen by the owning campaign.
    pub(crate) fn benchmark_runtime(
        &self,
    ) -> Option<&crate::agent_benchmark::BenchmarkRuntimeIdentity> {
        self.execution_identity
            .as_ref()
            .and_then(|identity| identity.benchmark_runtime.as_ref())
    }

    /// First-release benchmark runtime identities are ACP agent-harness identities.
    pub(crate) fn uses_acp_runtime(&self) -> bool {
        self.benchmark_runtime().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BenchmarkCoordinatorState {
    Idle,
    Running {
        completed: usize,
        total: usize,
        current: BenchmarkPlannedRun,
    },
    Complete {
        completed: usize,
        total: usize,
    },
    Failed {
        completed: usize,
        total: usize,
        message: String,
    },
}

/// One running benchmark child, viewed only through what the coordinator needs.
///
/// The coordinator never inspects a child's output. A run reports itself by
/// writing its result file and exiting, so the only questions worth asking a
/// child are whether it has exited and how to stop it.
pub(crate) trait BenchmarkChildProcess {
    /// Returns the exit description once the child has exited.
    fn finished(&mut self) -> Result<Option<String>, String>;
    /// Stops a child that is still running.
    fn terminate(&mut self) -> Result<(), String>;
}

/// Starts one isolated benchmark child.
///
/// This is the seam that lets the whole coordinator, including run isolation,
/// repeat handling, and failure recording, be tested without a local model or
/// a real Editor process.
pub(crate) trait BenchmarkChildLauncher {
    /// Starts `executable` against one reset fixture and one child spec.
    fn launch(
        &self,
        executable: &Path,
        project_root: &Path,
        child_spec_path: &Path,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<Box<dyn BenchmarkChildProcess>, String>;
}

/// Launches each benchmark run as a real isolated Editor process.
pub(crate) struct EditorProcessLauncher;

impl BenchmarkChildLauncher for EditorProcessLauncher {
    fn launch(
        &self,
        executable: &Path,
        project_root: &Path,
        child_spec_path: &Path,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<Box<dyn BenchmarkChildProcess>, String> {
        if !executable.is_file() {
            return Err(format!(
                "Editor executable `{}` is unavailable",
                executable.display()
            ));
        }
        let stdout = File::create(stdout_path).map_err(|error| {
            format!(
                "could not create benchmark child log `{}`: {error}",
                stdout_path.display()
            )
        })?;
        let stderr = File::create(stderr_path).map_err(|error| {
            format!(
                "could not create benchmark child log `{}`: {error}",
                stderr_path.display()
            )
        })?;
        let child = Command::new(executable)
            .arg("--project")
            .arg(project_root)
            .arg("--benchmark-run")
            .arg(child_spec_path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| format!("could not start benchmark Editor child: {error}"))?;
        Ok(Box::new(EditorChildProcess { child }))
    }
}

struct EditorChildProcess {
    child: Child,
}

impl BenchmarkChildProcess for EditorChildProcess {
    fn finished(&mut self) -> Result<Option<String>, String> {
        Ok(self
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .map(|status| status.to_string()))
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.child.kill().map_err(|error| error.to_string())?;
        let _ = self.child.wait();
        Ok(())
    }
}

struct ActiveChild {
    process: Box<dyn BenchmarkChildProcess>,
    run: BenchmarkPlannedRun,
    result_path: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    started_unix_ms: u64,
}

pub(crate) struct BenchmarkExperimentCoordinator {
    spec: BenchmarkExperimentSpec,
    endpoint: String,
    editor_executable: PathBuf,
    launcher: Box<dyn BenchmarkChildLauncher>,
    sandbox: BenchmarkFixtureSandbox,
    store: BenchmarkExperimentStore,
    queue: VecDeque<BenchmarkPlannedRun>,
    active: Option<ActiveChild>,
    results: Vec<BenchmarkExperimentResult>,
    stopped: bool,
}

impl BenchmarkExperimentCoordinator {
    pub(crate) fn new(
        spec: BenchmarkExperimentSpec,
        endpoint: String,
        editor_executable: PathBuf,
        fixture_template_root: PathBuf,
        run_root: PathBuf,
    ) -> Result<Self, String> {
        Self::with_launcher(
            spec,
            endpoint,
            editor_executable,
            fixture_template_root,
            run_root,
            Box::new(EditorProcessLauncher),
        )
    }

    /// Creates a coordinator that starts each run through `launcher`.
    pub(crate) fn with_launcher(
        spec: BenchmarkExperimentSpec,
        endpoint: String,
        editor_executable: PathBuf,
        fixture_template_root: PathBuf,
        run_root: PathBuf,
        launcher: Box<dyn BenchmarkChildLauncher>,
    ) -> Result<Self, String> {
        spec.validate()?;
        if spec.routing_mode != BenchmarkRoutingMode::SingleModel {
            return Err(
                "single-model benchmark coordinator cannot execute a routed experiment".to_owned(),
            );
        }
        if spec.backend_id != MANAGED_BACKEND_ID && endpoint.trim().is_empty() {
            return Err("benchmark compatible-backend endpoint must be non-empty".to_owned());
        }
        let store = BenchmarkExperimentStore::open(spec.output_destination.clone())?;
        store.write_spec(&spec)?;
        let queue = VecDeque::from(spec.planned_runs()?);
        Ok(Self {
            spec,
            endpoint,
            editor_executable,
            launcher,
            sandbox: BenchmarkFixtureSandbox::new(fixture_template_root, run_root),
            store,
            queue,
            active: None,
            results: Vec::new(),
            stopped: false,
        })
    }

    pub(crate) fn state(&self) -> BenchmarkCoordinatorState {
        let total = self.results.len() + self.queue.len() + usize::from(self.active.is_some());
        if let Some(active) = self.active.as_ref() {
            return BenchmarkCoordinatorState::Running {
                completed: self.results.len(),
                total,
                current: active.run.clone(),
            };
        }
        if self.stopped && !self.queue.is_empty() {
            return BenchmarkCoordinatorState::Failed {
                completed: self.results.len(),
                total,
                message: "benchmark stopped after a failed run".to_owned(),
            };
        }
        if self.queue.is_empty() {
            BenchmarkCoordinatorState::Complete {
                completed: self.results.len(),
                total,
            }
        } else {
            BenchmarkCoordinatorState::Idle
        }
    }

    pub(crate) fn results(&self) -> &[BenchmarkExperimentResult] {
        &self.results
    }

    /// Restores the completed prefix of a paused experiment.
    ///
    /// Every preceding result must exist and match the frozen spec. Missing or
    /// corrupt evidence stops resume instead of silently rerunning a different
    /// schedule under the same campaign identity.
    pub(crate) fn resume_from(&mut self, next_ordinal: u64) -> Result<(), String> {
        if self.active.is_some() || !self.results.is_empty() {
            return Err("benchmark resume must be configured before polling".to_owned());
        }
        let planned = self.spec.planned_runs()?;
        if next_ordinal as usize > planned.len() {
            return Err(format!(
                "benchmark resume ordinal {next_ordinal} exceeds {} planned runs",
                planned.len()
            ));
        }
        let mut restored = Vec::with_capacity(next_ordinal as usize);
        for ordinal in 0..next_ordinal {
            let result = self.store.read_result(&self.spec, ordinal)?;
            let expected = planned
                .get(ordinal as usize)
                .ok_or_else(|| format!("benchmark run {ordinal} is not planned"))?;
            if &result.run != expected {
                return Err(format!(
                    "resumed run {ordinal} does not match the frozen schedule"
                ));
            }
            restored.push(result);
        }
        self.results = restored;
        self.queue = planned
            .into_iter()
            .filter(|run| run.ordinal >= next_ordinal)
            .collect();
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> Result<(), String> {
        if let Some(active) = self.active.as_mut() {
            let Some(status) = active.process.finished()? else {
                return Ok(());
            };
            let active = self.active.take().expect("active child exists");
            // A child that dies without writing a result is evidence about that
            // model, not a reason to abandon the remaining runs. An 84-run suite
            // that aborts on the first crashing model measures nothing, so the
            // dead run is recorded as a harness failure and the queue continues.
            let result = match read_child_result(&active.result_path) {
                Ok(result) => result,
                Err(error) => self.failure_result(
                    &active,
                    BenchmarkRunFailureKind::Harness,
                    format!(
                        "child exited as {status} without a valid result: {error}; stdout `{}`, stderr `{}`",
                        active.stdout_path.display(),
                        active.stderr_path.display()
                    ),
                ),
            };
            result.validate_against(&self.spec)?;
            self.store.write_result(&self.spec, &result)?;
            let failed = !matches!(
                result.outcome,
                crate::benchmark_experiment::BenchmarkRunOutcome::Passed
            );
            self.results.push(result);
            if failed && self.spec.stop_on_failure {
                self.stopped = true;
                return Ok(());
            }
        }

        if self.active.is_none()
            && !self.stopped
            && let Some(run) = self.queue.pop_front()
        {
            self.active = Some(self.spawn_run(run)?);
        }
        Ok(())
    }

    pub(crate) fn interrupt(&mut self) -> Result<(), String> {
        self.stopped = true;
        self.queue.clear();
        if let Some(active) = self.active.as_mut() {
            active.process.terminate()?;
        }
        self.active = None;
        Ok(())
    }

    /// Stops only the active run for a user-requested campaign pause.
    ///
    /// No evidence is fabricated for the interrupted run. A resumed parent
    /// starts again at this ordinal after restoring the completed prefix.
    pub(crate) fn pause_active(&mut self) -> Result<(), String> {
        if let Some(active) = self.active.as_mut() {
            active.process.terminate()?;
        }
        self.active = None;
        Ok(())
    }

    /// Converts one hung child into timeout evidence and keeps the queue alive.
    pub(crate) fn timeout_active(&mut self, timeout: std::time::Duration) -> Result<(), String> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        active.process.terminate()?;
        let result = self.failure_result(
            &active,
            BenchmarkRunFailureKind::Timeout,
            format!(
                "benchmark run {} on model `{}` exceeded its {} second budget; stdout `{}`, stderr `{}`",
                active.run.ordinal,
                active.run.model_id,
                timeout.as_secs(),
                active.stdout_path.display(),
                active.stderr_path.display()
            ),
        );
        self.store.write_result(&self.spec, &result)?;
        self.results.push(result);
        if self.spec.stop_on_failure {
            self.stopped = true;
        }
        Ok(())
    }

    /// Records a run whose child never reported, without inventing metrics.
    ///
    /// The outcome is a failure with no record at all, so a crashed run can
    /// never contribute measured evidence or qualify a recommendation.
    fn failure_result(
        &self,
        active: &ActiveChild,
        failure_kind: BenchmarkRunFailureKind,
        message: String,
    ) -> BenchmarkExperimentResult {
        BenchmarkExperimentResult {
            experiment_id: self.spec.experiment_id.clone(),
            engine_commit_head: self.spec.engine_commit_head.clone(),
            fixture_version: self.spec.fixture_version.clone(),
            routing_mode: self.spec.routing_mode,
            run: active.run.clone(),
            started_unix_ms: active.started_unix_ms,
            finished_unix_ms: unix_ms().max(active.started_unix_ms),
            outcome: BenchmarkRunOutcome::Failed,
            failure_kind: Some(failure_kind),
            routed_to_another_model: false,
            harness_message: Some(message),
            record: None,
        }
    }

    fn spawn_run(&self, run: BenchmarkPlannedRun) -> Result<ActiveChild, String> {
        let project_root = self
            .sandbox
            .reset_run(&self.spec.experiment_id, run.ordinal)?;
        if run.task_id == "validation_repair_v1" {
            inject_validation_repair_fault(&project_root)?;
        }
        let experiment_root = self
            .spec
            .output_destination
            .join(safe_component(&self.spec.experiment_id));
        let child_root = experiment_root.join("child");
        fs::create_dir_all(&child_root).map_err(|error| error.to_string())?;
        let child_spec_path = child_root.join(format!("run-{:04}.json", run.ordinal));
        let result_path = child_root.join(format!("run-{:04}-result.json", run.ordinal));
        let stdout_path = child_root.join(format!("run-{:04}-stdout.log", run.ordinal));
        let stderr_path = child_root.join(format!("run-{:04}-stderr.log", run.ordinal));
        // Result paths are derived from the ordinal, so re-running an experiment
        // finds the previous execution's file already there. A child that dies
        // before reporting would otherwise inherit that stale result and record
        // an earlier attempt's outcome as this run's measured evidence.
        match fs::remove_file(&result_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let child_spec = BenchmarkChildRunSpec {
            schema_version: BENCHMARK_CHILD_SCHEMA_VERSION,
            experiment_id: self.spec.experiment_id.clone(),
            engine_commit_head: self.spec.engine_commit_head.clone(),
            fixture_version: self.spec.fixture_version.clone(),
            backend_id: self.spec.backend_id.clone(),
            managed_execution_environment: self.spec.managed_execution_environment,
            endpoint: if self.spec.backend_id == MANAGED_BACKEND_ID {
                String::new()
            } else {
                self.endpoint.clone()
            },
            model_id: run.model_id.clone(),
            task_id: run.task_id.clone(),
            repetition: run.repetition,
            ordinal: run.ordinal,
            quality: self.spec.quality,
            routing_mode: self.spec.routing_mode,
            result_path: result_path.clone(),
            execution_identity: self
                .spec
                .execution_identity_by_task
                .get(&run.task_id)
                .cloned(),
            candidate_contract: self
                .spec
                .candidate_contract_by_task
                .get(&run.task_id)
                .cloned(),
        };
        child_spec.write(&child_spec_path)?;
        let process = self.launcher.launch(
            &self.editor_executable,
            &project_root,
            &child_spec_path,
            &stdout_path,
            &stderr_path,
        )?;
        Ok(ActiveChild {
            process,
            run,
            result_path,
            stdout_path,
            stderr_path,
            started_unix_ms: unix_ms(),
        })
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn read_child_result(path: &Path) -> Result<BenchmarkExperimentResult, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn inject_validation_repair_fault(project_root: &Path) -> Result<(), String> {
    let path = project_root.join("game/src/benchmark_target.rs");
    let source = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let baseline = "assert_eq!(fixture_score(4), 5);";
    if source.matches(baseline).count() != 1 {
        return Err("validation-repair fixture baseline is not the expected version".to_owned());
    }
    fs::write(
        &path,
        source.replace(baseline, "assert_eq!(fixture_score(4), 999);"),
    )
    .map_err(|error| error.to_string())
}

fn safe_component(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        "experiment".to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark_experiment::BENCHMARK_FIXTURE_VERSION;

    #[test]
    fn child_spec_round_trip_preserves_exact_model_and_result_destination() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("child.json");
        let spec = BenchmarkChildRunSpec {
            schema_version: BENCHMARK_CHILD_SCHEMA_VERSION,
            experiment_id: "experiment".to_owned(),
            engine_commit_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
            backend_id: "ollama-compatible".to_owned(),
            managed_execution_environment: None,
            endpoint: "http://127.0.0.1:11434".to_owned(),
            model_id: "model:q4".to_owned(),
            task_id: "visual_evaluation_v1".to_owned(),
            repetition: 2,
            ordinal: 9,
            quality: QualityPreference::Balanced,
            routing_mode: BenchmarkRoutingMode::SingleModel,
            result_path: root.path().join("result.json"),
            execution_identity: None,
            candidate_contract: None,
        };
        spec.write(&path).expect("write");
        let loaded = BenchmarkChildRunSpec::read(&path).expect("read");
        assert_eq!(loaded, spec);
        assert_eq!(loaded.planned_run().model_id, "model:q4");
    }

    #[test]
    fn child_spec_preserves_the_frozen_acp_runtime_identity() {
        let root = tempfile::tempdir().expect("root");
        let runtime = crate::agent_benchmark::BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                "goose",
                Some("1.2.3".to_owned()),
            ),
        );
        let spec = BenchmarkChildRunSpec {
            schema_version: BENCHMARK_CHILD_SCHEMA_VERSION,
            experiment_id: "experiment".to_owned(),
            engine_commit_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
            backend_id: MANAGED_BACKEND_ID.to_owned(),
            managed_execution_environment: Some(ManagedExecutionEnvironment::WindowsNative),
            endpoint: String::new(),
            model_id: "gguf:model".to_owned(),
            task_id: "code_implementation_v1".to_owned(),
            repetition: 0,
            ordinal: 0,
            quality: QualityPreference::Balanced,
            routing_mode: BenchmarkRoutingMode::SingleModel,
            result_path: root.path().join("result.json"),
            execution_identity: Some(BenchmarkExecutionIdentity {
                campaign_harness_version: "campaign-harness-v1".to_owned(),
                schedule_policy_version: "schedule-v1".to_owned(),
                comparison_class: "model_comparison".to_owned(),
                execution_profile: "warm".to_owned(),
                execution_environment: "windows_native".to_owned(),
                fixture_id: "fixture".to_owned(),
                fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
                fixture_instance_id: "fixture-instance".to_owned(),
                sampling_profile: "sampling".to_owned(),
                seed_policy: "seed".to_owned(),
                benchmark_runtime: Some(runtime.clone()),
            }),
            candidate_contract: None,
        };
        assert!(spec.uses_acp_runtime());
        assert_eq!(spec.benchmark_runtime(), Some(&runtime));

        let path = root.path().join("acp-child.json");
        spec.write(&path).expect("write");
        let loaded = BenchmarkChildRunSpec::read(&path).expect("read");
        assert!(loaded.uses_acp_runtime());
        assert_eq!(loaded.benchmark_runtime(), Some(&runtime));
    }
}

#[cfg(test)]
mod coordinator_tests {
    use super::*;
    use crate::benchmark_experiment::{
        BENCHMARK_FIXTURE_VERSION, BenchmarkExperimentResult, BenchmarkRunFailureKind,
        BenchmarkRunOutcome,
    };
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const BASELINE: &str = "assert_eq!(fixture_score(4), 5);";
    const TARGET: &str = "game/src/benchmark_target.rs";

    /// What one fake child saw in its fixture before it touched anything.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ObservedLaunch {
        ordinal: u64,
        model_id: String,
        task_id: String,
        repetition: u32,
        target_source: String,
        saw_previous_contamination: bool,
    }

    /// A benchmark child that never starts an Editor or a model.
    ///
    /// Each launch records what the freshly reset fixture contained, then
    /// deliberately contaminates that fixture the way a real write-capable run
    /// would. A later run seeing that contamination is exactly the isolation
    /// failure these tests exist to catch.
    struct FakeChildLauncher {
        observed: Rc<RefCell<Vec<ObservedLaunch>>>,
        outcomes: Rc<RefCell<Vec<Option<BenchmarkRunOutcome>>>>,
    }

    struct FinishedChild;

    impl BenchmarkChildProcess for FinishedChild {
        fn finished(&mut self) -> Result<Option<String>, String> {
            Ok(Some("exit code: 0".to_owned()))
        }

        fn terminate(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    struct HangingChild {
        terminations: Rc<Cell<usize>>,
    }

    impl BenchmarkChildProcess for HangingChild {
        fn finished(&mut self) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn terminate(&mut self) -> Result<(), String> {
            self.terminations.set(self.terminations.get() + 1);
            Ok(())
        }
    }

    struct HangingChildLauncher {
        launches: Rc<Cell<usize>>,
        terminations: Rc<Cell<usize>>,
    }

    impl BenchmarkChildLauncher for HangingChildLauncher {
        fn launch(
            &self,
            _executable: &Path,
            _project_root: &Path,
            _child_spec_path: &Path,
            _stdout_path: &Path,
            _stderr_path: &Path,
        ) -> Result<Box<dyn BenchmarkChildProcess>, String> {
            self.launches.set(self.launches.get() + 1);
            Ok(Box::new(HangingChild {
                terminations: Rc::clone(&self.terminations),
            }))
        }
    }

    impl BenchmarkChildLauncher for FakeChildLauncher {
        fn launch(
            &self,
            _executable: &Path,
            project_root: &Path,
            child_spec_path: &Path,
            _stdout_path: &Path,
            _stderr_path: &Path,
        ) -> Result<Box<dyn BenchmarkChildProcess>, String> {
            let spec = BenchmarkChildRunSpec::read(child_spec_path)?;
            let target = project_root.join(TARGET);
            self.observed.borrow_mut().push(ObservedLaunch {
                ordinal: spec.ordinal,
                model_id: spec.model_id.clone(),
                task_id: spec.task_id.clone(),
                repetition: spec.repetition,
                target_source: fs::read_to_string(&target).map_err(|error| error.to_string())?,
                saw_previous_contamination: project_root.join("contamination.txt").exists(),
            });
            fs::write(&target, "contaminated by the previous run")
                .map_err(|error| error.to_string())?;
            fs::write(project_root.join("contamination.txt"), "leftover")
                .map_err(|error| error.to_string())?;

            let outcome = if self.outcomes.borrow().is_empty() {
                Some(BenchmarkRunOutcome::Passed)
            } else {
                self.outcomes.borrow_mut().remove(0)
            };
            if let Some(outcome) = outcome {
                let result = BenchmarkExperimentResult {
                    experiment_id: spec.experiment_id.clone(),
                    engine_commit_head: spec.engine_commit_head.clone(),
                    fixture_version: spec.fixture_version.clone(),
                    routing_mode: spec.routing_mode,
                    run: spec.planned_run(),
                    started_unix_ms: 1,
                    finished_unix_ms: 2,
                    outcome,
                    failure_kind: (outcome != BenchmarkRunOutcome::Passed)
                        .then_some(BenchmarkRunFailureKind::CompletionGate),
                    routed_to_another_model: false,
                    harness_message: None,
                    record: None,
                };
                let bytes =
                    serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
                if let Some(parent) = spec.result_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&spec.result_path, bytes).map_err(|error| error.to_string())?;
            }
            Ok(Box::new(FinishedChild))
        }
    }

    fn fixture_template(root: &Path) -> PathBuf {
        let template = root.join("fixture");
        fs::create_dir_all(template.join("game/src")).expect("fixture directories");
        fs::write(
            template.join(TARGET),
            format!(
                "pub fn fixture_score(value: i32) -> i32 {{ value + 1 }}\n#[test]\nfn baseline() {{ {BASELINE} }}\n"
            ),
        )
        .expect("fixture target");
        fs::write(template.join("project.json"), "{}").expect("fixture project");
        template
    }

    fn experiment(
        root: &Path,
        models: &[&str],
        tasks: &[&str],
        repeat: u32,
    ) -> BenchmarkExperimentSpec {
        BenchmarkExperimentSpec::local_single_model_comparison(
            "isolation",
            HEAD,
            models.iter().map(|model| (*model).to_owned()).collect(),
            tasks.iter().map(|task| (*task).to_owned()).collect(),
            repeat,
            QualityPreference::Balanced,
            root.join("results"),
        )
    }

    fn drive(
        spec: BenchmarkExperimentSpec,
        root: &Path,
        outcomes: Vec<Option<BenchmarkRunOutcome>>,
    ) -> (
        Result<(), String>,
        Vec<ObservedLaunch>,
        Vec<BenchmarkExperimentResult>,
        BenchmarkCoordinatorState,
    ) {
        let observed = Rc::new(RefCell::new(Vec::new()));
        let launcher = FakeChildLauncher {
            observed: Rc::clone(&observed),
            outcomes: Rc::new(RefCell::new(outcomes)),
        };
        let mut coordinator = BenchmarkExperimentCoordinator::with_launcher(
            spec,
            "http://127.0.0.1:11434".to_owned(),
            root.join("editor-does-not-run.exe"),
            fixture_template(root),
            root.join("runs"),
            Box::new(launcher),
        )
        .expect("coordinator");
        let mut outcome = Ok(());
        for _ in 0..512 {
            if let Err(error) = coordinator.poll() {
                outcome = Err(error);
                break;
            }
            if matches!(
                coordinator.state(),
                BenchmarkCoordinatorState::Complete { .. }
                    | BenchmarkCoordinatorState::Failed { .. }
            ) {
                break;
            }
        }
        let state = coordinator.state();
        let results = coordinator.results().to_vec();
        let launches = observed.borrow().clone();
        (outcome, launches, results, state)
    }

    #[test]
    fn every_run_starts_from_the_same_freshly_reset_fixture() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(
            root.path(),
            &["model-a", "model-b"],
            &["read_question_v1"],
            3,
        );
        let (outcome, launches, results, state) = drive(spec, root.path(), Vec::new());
        outcome.expect("suite ran");
        assert_eq!(launches.len(), 6);
        assert_eq!(results.len(), 6);
        assert!(matches!(state, BenchmarkCoordinatorState::Complete { .. }));
        for launch in &launches {
            assert!(
                launch.target_source.contains(BASELINE),
                "run {} saw a mutated fixture",
                launch.ordinal
            );
            assert!(
                !launch.saw_previous_contamination,
                "run {} inherited generated files from an earlier run",
                launch.ordinal
            );
        }
    }

    #[test]
    fn one_model_never_inherits_the_workspace_of_another() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(
            root.path(),
            &["model-a", "model-b"],
            &["code_implementation_v1"],
            1,
        );
        let (outcome, launches, _, _) = drive(spec, root.path(), Vec::new());
        outcome.expect("suite ran");
        let second = launches
            .iter()
            .find(|launch| launch.model_id == "model-b")
            .expect("second model ran");
        assert!(second.target_source.contains(BASELINE));
        assert!(!second.saw_previous_contamination);
    }

    #[test]
    fn every_planned_repetition_executes_exactly_once() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 4);
        let (outcome, launches, results, _) = drive(spec, root.path(), Vec::new());
        outcome.expect("suite ran");
        let mut repetitions = launches
            .iter()
            .map(|launch| launch.repetition)
            .collect::<Vec<_>>();
        repetitions.sort_unstable();
        assert_eq!(repetitions, vec![0, 1, 2, 3]);
        assert_eq!(results.len(), 4);
    }

    #[test]
    fn a_failing_run_is_recorded_and_the_suite_continues_by_default() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 3);
        let (outcome, launches, results, state) = drive(
            spec,
            root.path(),
            vec![
                Some(BenchmarkRunOutcome::Failed),
                Some(BenchmarkRunOutcome::Passed),
                Some(BenchmarkRunOutcome::Passed),
            ],
        );
        outcome.expect("suite ran");
        assert_eq!(launches.len(), 3);
        assert_eq!(results.len(), 3);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.outcome == BenchmarkRunOutcome::Failed)
                .count(),
            1
        );
        assert!(matches!(state, BenchmarkCoordinatorState::Complete { .. }));
    }

    #[test]
    fn a_timed_out_child_is_one_failed_run_and_the_next_run_still_starts() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 2);
        let launches = Rc::new(Cell::new(0));
        let terminations = Rc::new(Cell::new(0));
        let launcher = HangingChildLauncher {
            launches: Rc::clone(&launches),
            terminations: Rc::clone(&terminations),
        };
        let mut coordinator = BenchmarkExperimentCoordinator::with_launcher(
            spec,
            "http://127.0.0.1:11434".to_owned(),
            root.path().join("editor.exe"),
            fixture_template(root.path()),
            root.path().join("runs"),
            Box::new(launcher),
        )
        .expect("coordinator");
        coordinator.poll().expect("first run starts");
        coordinator
            .timeout_active(std::time::Duration::from_secs(1))
            .expect("timeout becomes evidence");
        assert_eq!(terminations.get(), 1);
        assert_eq!(coordinator.results().len(), 1);
        assert_eq!(
            coordinator.results()[0].failure_kind,
            Some(BenchmarkRunFailureKind::Timeout)
        );
        coordinator.poll().expect("next run starts");
        assert_eq!(launches.get(), 2);
        assert!(matches!(
            coordinator.state(),
            BenchmarkCoordinatorState::Running { current, .. } if current.ordinal == 1
        ));
    }

    #[test]
    fn stop_on_failure_halts_the_remaining_runs() {
        let root = tempfile::tempdir().expect("root");
        let mut spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 4);
        spec.stop_on_failure = true;
        let (outcome, launches, results, state) = drive(
            spec,
            root.path(),
            vec![
                Some(BenchmarkRunOutcome::Passed),
                Some(BenchmarkRunOutcome::Failed),
            ],
        );
        outcome.expect("suite ran");
        assert_eq!(launches.len(), 2);
        assert_eq!(results.len(), 2);
        assert!(matches!(state, BenchmarkCoordinatorState::Failed { .. }));
    }

    #[test]
    fn a_child_that_writes_no_result_is_recorded_as_a_harness_failure() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 1);
        let (outcome, launches, results, state) = drive(spec, root.path(), vec![None]);
        outcome.expect("a dead child is evidence, not an abort");
        assert_eq!(launches.len(), 1);
        let result = results.first().expect("the dead run was still recorded");
        assert_eq!(result.outcome, BenchmarkRunOutcome::Failed);
        assert_eq!(result.failure_kind, Some(BenchmarkRunFailureKind::Harness));
        assert!(
            result.record.is_none(),
            "a dead run carries no measurements"
        );
        assert!(
            result
                .harness_message
                .as_ref()
                .is_some_and(|message| message.contains("without a valid result"))
        );
        assert!(matches!(state, BenchmarkCoordinatorState::Complete { .. }));
    }

    #[test]
    fn a_rerun_never_inherits_the_previous_executions_child_result() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 1);
        let child_root = root.path().join("results").join("isolation").join("child");
        fs::create_dir_all(&child_root).expect("child directory");
        let stale = BenchmarkExperimentResult {
            experiment_id: "isolation".to_owned(),
            engine_commit_head: HEAD.to_owned(),
            fixture_version: BENCHMARK_FIXTURE_VERSION.to_owned(),
            routing_mode: BenchmarkRoutingMode::SingleModel,
            run: BenchmarkPlannedRun {
                ordinal: 0,
                model_id: "model-a".to_owned(),
                task_id: "read_question_v1".to_owned(),
                repetition: 0,
            },
            started_unix_ms: 1,
            finished_unix_ms: 2,
            outcome: BenchmarkRunOutcome::Passed,
            failure_kind: None,
            routed_to_another_model: false,
            harness_message: None,
            record: None,
        };
        fs::write(
            child_root.join("run-0000-result.json"),
            serde_json::to_vec_pretty(&stale).expect("stale result"),
        )
        .expect("write stale result");

        let (outcome, _, results, _) = drive(spec, root.path(), vec![None]);
        outcome.expect("suite ran");
        let result = results.first().expect("the dead run was still recorded");
        assert_eq!(
            result.outcome,
            BenchmarkRunOutcome::Failed,
            "a re-run reported the previous execution's outcome"
        );
        assert_eq!(result.failure_kind, Some(BenchmarkRunFailureKind::Harness));
    }

    #[test]
    fn one_crashing_model_does_not_abandon_the_remaining_runs() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 4);
        let (outcome, launches, results, state) = drive(
            spec,
            root.path(),
            vec![
                Some(BenchmarkRunOutcome::Passed),
                None,
                Some(BenchmarkRunOutcome::Passed),
                Some(BenchmarkRunOutcome::Passed),
            ],
        );
        outcome.expect("suite ran");
        assert_eq!(launches.len(), 4);
        assert_eq!(results.len(), 4);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.outcome == BenchmarkRunOutcome::Passed)
                .count(),
            3
        );
        assert!(matches!(state, BenchmarkCoordinatorState::Complete { .. }));
    }

    #[test]
    fn the_validation_repair_run_starts_from_an_injected_failing_baseline() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(root.path(), &["model-a"], &["validation_repair_v1"], 1);
        let (outcome, launches, _, _) = drive(spec, root.path(), Vec::new());
        outcome.expect("suite ran");
        let launch = launches.first().expect("repair run");
        assert!(
            !launch.target_source.contains(BASELINE),
            "the repair task must not start from an already passing fixture"
        );
        assert!(
            launch
                .target_source
                .contains("assert_eq!(fixture_score(4), 999);")
        );
    }

    #[test]
    fn a_routed_experiment_is_refused_before_any_run_starts() {
        let root = tempfile::tempdir().expect("root");
        let mut spec = experiment(root.path(), &["model-a"], &["read_question_v1"], 1);
        spec.routing_mode = BenchmarkRoutingMode::Routed;
        let observed = Rc::new(RefCell::new(Vec::new()));
        let launcher = FakeChildLauncher {
            observed: Rc::clone(&observed),
            outcomes: Rc::new(RefCell::new(Vec::new())),
        };
        let coordinator = BenchmarkExperimentCoordinator::with_launcher(
            spec,
            "http://127.0.0.1:11434".to_owned(),
            root.path().join("editor.exe"),
            fixture_template(root.path()),
            root.path().join("runs"),
            Box::new(launcher),
        );
        assert!(coordinator.is_err());
        assert!(observed.borrow().is_empty());
    }

    #[test]
    fn each_child_receives_its_own_exact_model_and_fixture_identity() {
        let root = tempfile::tempdir().expect("root");
        let spec = experiment(
            root.path(),
            &["qwen-a:q4_k_m", "qwen-a:q8_0"],
            &["read_question_v1"],
            1,
        );
        let (outcome, launches, results, _) = drive(spec, root.path(), Vec::new());
        outcome.expect("suite ran");
        let models = launches
            .iter()
            .map(|launch| launch.model_id.clone())
            .collect::<Vec<_>>();
        assert!(models.contains(&"qwen-a:q4_k_m".to_owned()));
        assert!(models.contains(&"qwen-a:q8_0".to_owned()));
        for result in &results {
            assert_eq!(result.engine_commit_head, HEAD);
            assert_eq!(result.fixture_version, BENCHMARK_FIXTURE_VERSION);
            assert_eq!(result.routing_mode, BenchmarkRoutingMode::SingleModel);
            assert!(!result.routed_to_another_model);
        }
    }
}
