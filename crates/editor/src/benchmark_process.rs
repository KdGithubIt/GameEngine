//! Process-isolated execution for reproducible GameEngine Agent Benchmark runs.
//!
//! Every planned run receives a fresh repository-owned fixture copy and a fresh
//! Editor process. This prevents authoring, generated files, validation, Play,
//! proposal, permission, and model-run state from leaking between models.

#![allow(dead_code)]

use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkExperimentSpec, BenchmarkExperimentStore,
    BenchmarkFixtureSandbox, BenchmarkPlannedRun, BenchmarkRoutingMode,
};
use crate::resource_arbitration::QualityPreference;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
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
    pub(crate) endpoint: String,
    pub(crate) model_id: String,
    pub(crate) task_id: String,
    pub(crate) repetition: u32,
    pub(crate) ordinal: u64,
    pub(crate) quality: QualityPreference,
    pub(crate) routing_mode: BenchmarkRoutingMode,
    pub(crate) result_path: PathBuf,
}

impl BenchmarkChildRunSpec {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema_version != BENCHMARK_CHILD_SCHEMA_VERSION {
            return Err(format!("unsupported benchmark child schema {}", self.schema_version));
        }
        if self.experiment_id.trim().is_empty()
            || self.backend_id.trim().is_empty()
            || self.endpoint.trim().is_empty()
            || self.model_id.trim().is_empty()
            || self.task_id.trim().is_empty()
        {
            return Err("benchmark child identity fields must be non-empty".to_owned());
        }
        if self.engine_commit_head.len() != 40
            || !self.engine_commit_head.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("benchmark child requires an exact 40-character engine SHA".to_owned());
        }
        if self.routing_mode != BenchmarkRoutingMode::SingleModel {
            return Err("first-release benchmark child executes only strict single-model baselines".to_owned());
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BenchmarkCoordinatorState {
    Idle,
    Running { completed: usize, total: usize, current: BenchmarkPlannedRun },
    Complete { completed: usize, total: usize },
    Failed { completed: usize, total: usize, message: String },
}

struct ActiveChild {
    child: Child,
    run: BenchmarkPlannedRun,
    result_path: PathBuf,
}

pub(crate) struct BenchmarkExperimentCoordinator {
    spec: BenchmarkExperimentSpec,
    endpoint: String,
    editor_executable: PathBuf,
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
        spec.validate()?;
        if spec.routing_mode != BenchmarkRoutingMode::SingleModel {
            return Err("single-model benchmark coordinator cannot execute a routed experiment".to_owned());
        }
        if endpoint.trim().is_empty() {
            return Err("benchmark local-model endpoint must be non-empty".to_owned());
        }
        if !editor_executable.is_file() {
            return Err(format!("Editor executable `{}` is unavailable", editor_executable.display()));
        }
        let store = BenchmarkExperimentStore::open(spec.output_destination.clone())?;
        store.write_spec(&spec)?;
        let queue = VecDeque::from(spec.planned_runs()?);
        Ok(Self {
            spec,
            endpoint,
            editor_executable,
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
            BenchmarkCoordinatorState::Complete { completed: self.results.len(), total }
        } else {
            BenchmarkCoordinatorState::Idle
        }
    }

    pub(crate) fn results(&self) -> &[BenchmarkExperimentResult] {
        &self.results
    }

    pub(crate) fn poll(&mut self) -> Result<(), String> {
        if let Some(active) = self.active.as_mut() {
            let Some(status) = active.child.try_wait().map_err(|error| error.to_string())? else {
                return Ok(());
            };
            let active = self.active.take().expect("active child exists");
            let result = read_child_result(&active.result_path).map_err(|error| {
                format!(
                    "benchmark child {} for {} exited as {status} without a valid result: {error}",
                    active.run.ordinal, active.run.model_id
                )
            })?;
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

        if self.active.is_none() && !self.stopped {
            if let Some(run) = self.queue.pop_front() {
                self.active = Some(self.spawn_run(run)?);
            }
        }
        Ok(())
    }

    pub(crate) fn interrupt(&mut self) -> Result<(), String> {
        self.stopped = true;
        self.queue.clear();
        if let Some(active) = self.active.as_mut() {
            active.child.kill().map_err(|error| error.to_string())?;
            let _ = active.child.wait();
        }
        self.active = None;
        Ok(())
    }

    fn spawn_run(&self, run: BenchmarkPlannedRun) -> Result<ActiveChild, String> {
        let project_root = self.sandbox.reset_run(&self.spec.experiment_id, run.ordinal)?;
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
        let child_spec = BenchmarkChildRunSpec {
            schema_version: BENCHMARK_CHILD_SCHEMA_VERSION,
            experiment_id: self.spec.experiment_id.clone(),
            engine_commit_head: self.spec.engine_commit_head.clone(),
            fixture_version: self.spec.fixture_version.clone(),
            backend_id: self.spec.backend_id.clone(),
            endpoint: self.endpoint.clone(),
            model_id: run.model_id.clone(),
            task_id: run.task_id.clone(),
            repetition: run.repetition,
            ordinal: run.ordinal,
            quality: self.spec.quality,
            routing_mode: self.spec.routing_mode,
            result_path: result_path.clone(),
        };
        child_spec.write(&child_spec_path)?;
        let child = Command::new(&self.editor_executable)
            .arg("--project")
            .arg(&project_root)
            .arg("--benchmark-run")
            .arg(&child_spec_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start benchmark Editor child: {error}"))?;
        Ok(ActiveChild { child, run, result_path })
    }
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
    fs::write(&path, source.replace(baseline, "assert_eq!(fixture_score(4), 999);"))
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
    if value.is_empty() { "experiment".to_owned() } else { value }
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
            endpoint: "http://127.0.0.1:11434".to_owned(),
            model_id: "model:q4".to_owned(),
            task_id: "visual_evaluation_v1".to_owned(),
            repetition: 2,
            ordinal: 9,
            quality: QualityPreference::Balanced,
            routing_mode: BenchmarkRoutingMode::SingleModel,
            result_path: root.path().join("result.json"),
        };
        spec.write(&path).expect("write");
        let loaded = BenchmarkChildRunSpec::read(&path).expect("read");
        assert_eq!(loaded, spec);
        assert_eq!(loaded.planned_run().model_id, "model:q4");
    }
}
