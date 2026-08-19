use super::*;
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkRoutingMode, BenchmarkRunFailureKind, BenchmarkRunOutcome,
};
use crate::benchmark_process::BenchmarkChildRunSpec;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct BenchmarkChildState {
    spec: BenchmarkChildRunSpec,
    started: bool,
    started_unix_ms: u64,
    result_written: bool,
}

impl AiStudioPanel {
    /// Configures this Editor process as one isolated benchmark child.
    pub fn configure_benchmark_child(&mut self, path: &Path) -> Result<(), String> {
        let spec = BenchmarkChildRunSpec::read(path)?;
        match spec.backend_id.as_str() {
            "ollama-compatible" => {
                self.model_backend = ModelBackendPreference::Local;
                self.local_model_endpoint = spec.endpoint.clone();
                self.local_model_name = spec.model_id.clone();
            }
            MANAGED_BACKEND_ID => {
                let environment = spec.managed_execution_environment.ok_or_else(|| {
                    "managed benchmark child is missing its frozen execution environment".to_owned()
                })?;
                self.model_backend = ModelBackendPreference::ManagedLocal;
                self.managed_execution_environment = environment;
                self.managed_model_id = spec.model_id.clone();
                self.managed_probe = None;
                self.managed_probe_completed_at = None;
                self.managed_probe_requested = true;
            }
            backend => {
                return Err(format!(
                    "first-release benchmark child does not support backend `{backend}`"
                ));
            }
        }
        self.quality_preference = spec.quality;
        self.benchmark_task_id = spec.task_id.clone();
        let compatible_backend = spec.backend_id == "ollama-compatible";
        self.benchmark_child = Some(BenchmarkChildState {
            spec,
            started: false,
            started_unix_ms: unix_ms(),
            result_written: false,
        });
        self.presentation.close();
        // Comparable evidence must freeze the exact backend/model/runtime representation
        // before task start. Compatible backends discover their inventory; managed runs
        // wait for the asynchronous managed-environment probe instead.
        if compatible_backend {
            self.start_model_discovery();
        }
        Ok(())
    }

    pub(super) fn benchmark_child_active(&self) -> bool {
        self.benchmark_child.is_some()
    }

    pub(super) fn benchmark_child_requires_initial_validation_failure(&self) -> bool {
        self.benchmark_child
            .as_ref()
            .is_some_and(|child| child.spec.task_id == "validation_repair_v1")
    }

    pub(super) fn benchmark_child_allows(&self, capability: AgentCapability) -> bool {
        let Some(child) = self.benchmark_child.as_ref() else {
            return false;
        };
        match child.spec.task_id.as_str() {
            "runtime_interaction_v1" | "visual_evaluation_v1" => matches!(
                capability,
                AgentCapability::RuntimeLaunch
                    | AgentCapability::RuntimeInputControl
                    | AgentCapability::FrameCapture
            ),
            _ => false,
        }
    }

    pub(super) fn poll_benchmark_child(&mut self) {
        let Some(child) = self.benchmark_child.as_ref() else {
            return;
        };
        let result_written = child.result_written;
        let backend_id = child.spec.backend_id.clone();
        let started = child.started;
        let task_id = child.spec.task_id.clone();
        if result_written {
            return;
        }
        if backend_id == MANAGED_BACKEND_ID {
            self.managed_probe_requested = true;
            if self.managed_probe_task.is_some() {
                return;
            }
            let Some(probe) = self.managed_probe.as_ref() else {
                return;
            };
            if probe.environment != self.managed_execution_environment
                || probe.model_id != self.managed_model_id
            {
                return;
            }
            if let Err(error) = probe.described_config.as_ref() {
                let message = format!("managed benchmark preflight failed: {error}");
                self.write_benchmark_child_failure(
                    BenchmarkRunFailureKind::CapabilityUnavailable,
                    message,
                );
                return;
            }
        } else if self.model_discovery.is_some() {
            // Representation discovery is still in flight; starting now would
            // freeze an unmeasured model identity for the whole run.
            return;
        }
        if !started {
            if let Err(error) = self.start_benchmark_child_task() {
                self.write_benchmark_child_failure(BenchmarkRunFailureKind::Harness, error);
            }
            return;
        }
        if task_id == "read_question_v1" {
            if self.native_question.is_none() && self.pending_native_question_start.is_none() {
                if let Some(snapshot) = self.last_native_question_benchmark.clone()
                    && snapshot.policy.task_id == task_id
                {
                    match read_question_record(
                        &task_id,
                        &snapshot.metrics,
                        snapshot.policy.inventory.as_ref(),
                        snapshot.policy.quality,
                        snapshot.policy.workload,
                        &snapshot.policy.hardware,
                    ) {
                        Ok(record) => self.write_benchmark_child_record(record, false),
                        Err(error) => self.write_benchmark_child_failure(
                            BenchmarkRunFailureKind::CompletionGate,
                            error,
                        ),
                    }
                } else if let Some(status) = self.status.clone()
                    && (status.contains("unavailable")
                        || status.contains("failed")
                        || status.contains("error"))
                {
                    self.write_benchmark_child_failure(BenchmarkRunFailureKind::Backend, status);
                }
            }
            return;
        }

        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let state = self.host.run(&run_id).map(|run| run.state).ok();
        if state == Some(AgentRunState::Evaluating) {
            match self.host.complete_run(&run_id) {
                Ok(()) | Err(AgentHostError::CompletionPending) => {}
                Err(error) => {
                    self.write_benchmark_child_failure(
                        BenchmarkRunFailureKind::Harness,
                        error.to_string(),
                    );
                    return;
                }
            }
        }
        let Ok(run) = self.host.run(&run_id).cloned() else {
            return;
        };
        if !matches!(
            run.state,
            AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
        ) {
            return;
        }
        let Some(identity) = self.native_run_benchmark_context.clone() else {
            self.write_benchmark_child_failure(
                BenchmarkRunFailureKind::Harness,
                "terminal benchmark run lost its frozen model identity".to_owned(),
            );
            return;
        };
        match agent_run_record(
            &task_id,
            &run,
            AgentRunBenchmarkIdentity {
                backend_id: &identity.backend_id,
                model_id: &identity.model_id,
                inventory: identity.inventory.as_ref(),
                quality: identity.quality,
                workload: identity.workload,
                hardware: &identity.hardware,
            },
        ) {
            Ok(record) => self.write_benchmark_child_record(record, identity.routed),
            Err(error) => {
                self.write_benchmark_child_failure(BenchmarkRunFailureKind::CompletionGate, error)
            }
        }
    }

    /// Reports whether this benchmark child has written its result and may exit.
    ///
    /// Taking the request also clears the child state, so the shell closes the
    /// viewport exactly once per run.
    pub fn take_benchmark_child_exit_request(&mut self) -> bool {
        if !self
            .benchmark_child
            .as_ref()
            .is_some_and(|child| child.result_written)
        {
            return false;
        }
        self.benchmark_child.take();
        true
    }

    fn start_benchmark_child_task(&mut self) -> Result<(), String> {
        let (task_id, model_id) = self
            .benchmark_child
            .as_ref()
            .map(|child| (child.spec.task_id.clone(), child.spec.model_id.clone()))
            .ok_or_else(|| "benchmark child is unavailable".to_owned())?;
        if let Some(child) = self.benchmark_child.as_mut() {
            child.started = true;
            child.started_unix_ms = unix_ms();
        }
        self.status = Some(format!(
            "Benchmark child starting {task_id} on exact model {model_id}."
        ));
        // A campaign supplies the candidate-visible contract. Only that side of
        // the fixture exists in this process, so no prompt path can reach the
        // host-owned evaluation material even by mistake.
        let campaign_prompt = self
            .benchmark_child
            .as_ref()
            .and_then(|child| child.spec.candidate_contract.as_ref())
            .map(|contract| contract.prompt.clone());
        if let Some(prompt) = campaign_prompt
            && task_id == "read_question_v1"
        {
            self.host
                .append_message(&self.selected_session, ConversationRole::User, prompt)
                .map_err(|error| error.to_string())?;
            self.start_native_question();
            return Ok(());
        }
        if task_id == "read_question_v1" {
            self.host
                .append_message(
                    &self.selected_session,
                    ConversationRole::User,
                    "In this repository-owned benchmark fixture, which Rust function defines the fixture score? Report the defining project/repository provenance, not only the answer.",
                )
                .map_err(|error| error.to_string())?;
            self.start_native_question();
            return Ok(());
        }
        self.proposal_draft = benchmark_proposal(&task_id)?;
        self.begin_run();
        Ok(())
    }

    fn write_benchmark_child_record(&mut self, mut record: BenchmarkRecord, routed: bool) {
        // Stamped here, at measurement time, from the frozen spec the parent
        // handed this child. A campaign that stamped its own identity onto a
        // returned record afterwards could never detect a mismatch.
        if let Some(child) = self.benchmark_child.as_ref() {
            record.identity.execution = child.spec.execution_identity.clone();
        }
        let outcome = if record.metrics.completion_success == TelemetryValue::Measured(true) {
            BenchmarkRunOutcome::Passed
        } else {
            BenchmarkRunOutcome::Failed
        };
        let failure_kind = (outcome != BenchmarkRunOutcome::Passed)
            .then_some(BenchmarkRunFailureKind::CompletionGate);
        self.write_benchmark_child_result(outcome, failure_kind, routed, None, Some(record));
    }

    fn write_benchmark_child_failure(&mut self, kind: BenchmarkRunFailureKind, message: String) {
        self.status = Some(message.clone());
        let outcome = if matches!(kind, BenchmarkRunFailureKind::CapabilityUnavailable) {
            BenchmarkRunOutcome::Unavailable
        } else {
            BenchmarkRunOutcome::Failed
        };
        // A failed run is only useful if the next person can tell a refused
        // capability from an exhausted backend, so the reason travels with the
        // result. It describes the harness, never the model's output.
        self.write_benchmark_child_result(outcome, Some(kind), false, Some(message), None);
    }

    fn write_benchmark_child_result(
        &mut self,
        outcome: BenchmarkRunOutcome,
        failure_kind: Option<BenchmarkRunFailureKind>,
        routed: bool,
        harness_message: Option<String>,
        record: Option<BenchmarkRecord>,
    ) {
        let Some(child) = self.benchmark_child.as_mut() else {
            return;
        };
        if child.result_written {
            return;
        }
        let result = BenchmarkExperimentResult {
            experiment_id: child.spec.experiment_id.clone(),
            engine_commit_head: child.spec.engine_commit_head.clone(),
            fixture_version: child.spec.fixture_version.clone(),
            routing_mode: BenchmarkRoutingMode::SingleModel,
            run: child.spec.planned_run(),
            started_unix_ms: child.started_unix_ms,
            finished_unix_ms: unix_ms(),
            outcome,
            failure_kind,
            routed_to_another_model: routed,
            harness_message,
            record,
        };
        let write_result = serde_json::to_vec_pretty(&result)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                if let Some(parent) = child.spec.result_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&child.spec.result_path, bytes).map_err(|error| error.to_string())
            });
        match write_result {
            Ok(()) => child.result_written = true,
            Err(error) => self.status = Some(format!("Benchmark result write failed: {error}")),
        }
    }
}

fn benchmark_proposal(task_id: &str) -> Result<AgentProposal, String> {
    let mut proposal = AgentProposal::default();
    proposal.goal = match task_id {
        "project_inspection_v1" => "Inspect the repository-owned benchmark project using actual project/authoring evidence and identify its main scene plus Rust fixture source.",
        "code_implementation_v1" => "Change game/src/benchmark_target.rs so fixture_score(4) returns 8, update the test accordingly, and pass managed source validation.",
        "typed_authoring_mutation_v1" => "Create one entity named BenchmarkAgentEntity through GameEngine typed authoring/MCP only, then validate the authoritative authoring result.",
        "validation_repair_v1" => "Repair the host-proven failing benchmark_target test so the intended baseline fixture_score(4) == 5 passes, then revalidate.",
        "runtime_interaction_v1" => "Use normal Editor Play and the Agent virtual-input action to send a Space key press and verify the managed runtime interaction completes.",
        "visual_evaluation_v1" => "Launch normal Editor Play, capture the actual Game View, inspect the attached image, and resolve visual_evaluation only from that image.",
        other => return Err(format!("unsupported benchmark child task `{other}`")),
    }.to_owned();
    proposal.requirements = vec![
        "Use only governed GameEngine Agent Host paths; do not bypass typed authoring, permissions, work claims, managed validation, or normal Play.".to_owned(),
        "Return completion gates only from evidence actually observed in this run.".to_owned(),
    ];
    proposal.acceptance_criteria = vec![proposal.goal.clone()];
    match task_id {
        "code_implementation_v1" | "validation_repair_v1" => {
            proposal.planned_code_changes = vec!["game/src/benchmark_target.rs".to_owned()];
            proposal.validation_plan = vec!["all".to_owned()];
        }
        "typed_authoring_mutation_v1" => {
            proposal.planned_project_changes = vec![
                "Create BenchmarkAgentEntity in the authoritative scene through typed authoring."
                    .to_owned(),
            ];
        }
        "runtime_interaction_v1" => {
            proposal.playtest_plan = vec![
                "Send Space pressed through runtime_input and verify the managed interaction."
                    .to_owned(),
            ];
        }
        "visual_evaluation_v1" => {
            proposal.playtest_plan = vec![
                "Capture and visually evaluate the actual managed Game View frame.".to_owned(),
            ];
        }
        _ => {}
    }
    Ok(proposal)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
