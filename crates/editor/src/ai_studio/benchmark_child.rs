use super::*;
use crate::agent_benchmark::{
    BenchmarkLane, BenchmarkModelTelemetry, BenchmarkRuntimeIdentity,
    agent_run_record_with_runtime, benchmark_project_inspection_ready,
};
use crate::agent_benchmark_campaign::{
    CampaignExecutionProfile, campaign_task_agent_policy_for_runtime,
};
use crate::agent_host::AgentRun;
use crate::benchmark_experiment::{
    BenchmarkExperimentResult, BenchmarkRoutingMode, BenchmarkRunFailureKind, BenchmarkRunOutcome,
};
use crate::benchmark_process::BenchmarkChildRunSpec;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const BENCHMARK_PREFLIGHT_TIMEOUT_MS: u64 = 120_000;
const BENCHMARK_RUN_INITIALIZATION_TIMEOUT_MS: u64 = 30_000;

pub(super) struct BenchmarkChildState {
    spec: BenchmarkChildRunSpec,
    started: bool,
    configured_unix_ms: u64,
    started_unix_ms: u64,
    profile_requested: bool,
    pub(super) profile_prepared: bool,
    result_written: bool,
}

impl AiStudioPanel {
    /// Configures this Editor process as one isolated benchmark child.
    pub fn configure_benchmark_child(&mut self, path: &Path) -> Result<(), String> {
        let spec = BenchmarkChildRunSpec::read(path)?;
        validate_benchmark_runtime_spec(&spec)?;
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
        // Both supported benchmark backends are model backends, and the campaign
        // plan froze which one produces this run's evidence. The persisted Editor
        // preferences this child loaded may still select an external agent
        // provider, which would route the run to an executor the campaign never
        // froze and stall it on a permission no campaign child can answer.
        self.selected_ai_family = SelectedAiFamily::Model;
        self.quality_preference = spec.quality;
        self.benchmark_task_id = spec.task_id.clone();
        let compatible_backend = spec.backend_id == "ollama-compatible";
        self.benchmark_child = Some(BenchmarkChildState {
            spec,
            started: false,
            configured_unix_ms: unix_ms(),
            started_unix_ms: 0,
            profile_requested: false,
            profile_prepared: false,
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

    pub(super) fn benchmark_child_uses_acp_runtime(&self) -> bool {
        self.benchmark_child
            .as_ref()
            .is_some_and(|child| child.spec.uses_acp_runtime())
    }

    /// Mirrors only credential-safe ACP activity into the run-scoped child log.
    ///
    /// The ACP transport owns raw protocol I/O, which may contain ephemeral MCP
    /// credentials and is therefore never copied into benchmark diagnostics.
    /// The campaign parent already captures this child process's stderr per run,
    /// so normalized event categories provide enough liveness evidence without
    /// creating another persistence or authorization surface.
    pub(super) fn report_benchmark_acp_live_event(&self, event: &AcpNormalizedEvent) {
        if !self.benchmark_child_uses_acp_runtime() {
            return;
        }
        eprintln!("[benchmark.acp] {}", benchmark_acp_live_summary(event));
    }

    pub(super) fn validate_benchmark_acp_runtime_identity(
        &self,
        identity: &crate::acp_agent_runtime::AcpRuntimeIdentity,
    ) -> Result<(), String> {
        let Some(child) = self.benchmark_child.as_ref() else {
            return Ok(());
        };
        let runtime = benchmark_runtime_from_spec(&child.spec).ok_or_else(|| {
            "benchmark child reached ACP without a frozen benchmark runtime identity".to_owned()
        })?;
        if runtime.matches_acp_runtime(identity) {
            Ok(())
        } else {
            Err(format!(
                "negotiated ACP runtime identity `{}` {:?} protocol v{} does not match the frozen benchmark runtime",
                identity.agent_name, identity.agent_version, identity.protocol_version
            ))
        }
    }

    pub(super) fn benchmark_child_requires_initial_validation_failure(&self) -> bool {
        self.benchmark_child
            .as_ref()
            .is_some_and(|child| child.spec.task_id == "validation_repair_v1")
    }

    /// Checks whether a benchmark child may hand control to managed validation.
    ///
    /// The provider-facing action protocol permits a generic
    /// `ready_for_validation` action, but benchmark tasks also have
    /// task-specific host evidence requirements. Keeping this check at the
    /// child boundary prevents a model from ending an inspection after a
    /// planning-only response while leaving non-benchmark interactive runs on
    /// their existing path.
    pub(super) fn validate_benchmark_ready_for_validation(
        &self,
        run: &AgentRun,
    ) -> Result<(), String> {
        let Some(child) = self.benchmark_child.as_ref() else {
            return Ok(());
        };
        if child.spec.task_id == "project_inspection_v1" {
            benchmark_project_inspection_ready(run)?;
        }
        Ok(())
    }

    pub(super) fn benchmark_child_allows(&self, capability: AgentCapability) -> bool {
        let Some(child) = self.benchmark_child.as_ref() else {
            return false;
        };
        benchmark_task_allows_capability(
            &child.spec.task_id,
            benchmark_runtime_from_spec(&child.spec),
            capability,
        )
    }

    /// Ends this run because it asked for a capability its campaign never froze.
    ///
    /// A campaign child runs without an operator, so an approval prompt it did
    /// not pre-authorize can never be answered. Recording the refusal as an
    /// unavailable capability lets the parent continue the schedule instead of
    /// leaving the whole campaign waiting on a dialog nobody can see.
    pub(super) fn refuse_unbudgeted_benchmark_child_permission(
        &mut self,
        capability: AgentCapability,
    ) {
        let message = format!(
            "Benchmark child requested `{}`, which this task's frozen capability budget does not authorize.",
            capability.label()
        );
        self.write_benchmark_child_failure(BenchmarkRunFailureKind::CapabilityUnavailable, message);
    }

    pub(super) fn poll_benchmark_child(&mut self) {
        let Some(child) = self.benchmark_child.as_ref() else {
            return;
        };
        let result_written = child.result_written;
        let backend_id = child.spec.backend_id.clone();
        let started = child.started;
        let task_id = child.spec.task_id.clone();
        let configured_unix_ms = child.configured_unix_ms;
        let started_unix_ms = child.started_unix_ms;
        if result_written {
            return;
        }
        if !started
            && unix_ms().saturating_sub(configured_unix_ms) >= BENCHMARK_PREFLIGHT_TIMEOUT_MS
        {
            self.write_benchmark_child_failure(
                BenchmarkRunFailureKind::Timeout,
                "benchmark preflight exceeded its managed-probe/model-discovery deadline"
                    .to_owned(),
            );
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
            let profile_prepared = self
                .benchmark_child
                .as_ref()
                .is_some_and(|child| child.profile_prepared);
            if !profile_prepared {
                if let Err(error) = self.prepare_benchmark_execution_profile() {
                    self.write_benchmark_child_failure(
                        BenchmarkRunFailureKind::CapabilityUnavailable,
                        error,
                    );
                }
                return;
            }
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
            if unix_ms().saturating_sub(started_unix_ms) >= BENCHMARK_RUN_INITIALIZATION_TIMEOUT_MS
            {
                self.write_benchmark_child_failure(
                    BenchmarkRunFailureKind::Harness,
                    "benchmark task started but no AgentRun identity was created before the initialization deadline"
                        .to_owned(),
                );
            }
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
        let benchmark_identity = AgentRunBenchmarkIdentity {
            backend_id: &identity.backend_id,
            model_id: &identity.model_id,
            inventory: identity.inventory.as_ref(),
            quality: identity.quality,
            workload: identity.workload,
            hardware: &identity.hardware,
        };
        let runtime = self
            .benchmark_child
            .as_ref()
            .and_then(|child| benchmark_runtime_from_spec(&child.spec))
            .cloned();
        let record = match runtime {
            Some(runtime) => agent_run_record_with_runtime(
                &task_id,
                &run,
                benchmark_identity,
                runtime,
                BenchmarkModelTelemetry::unavailable(),
            ),
            None => agent_run_record(&task_id, &run, benchmark_identity),
        };
        match record {
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
        let benchmark_runtime = self
            .benchmark_child
            .as_ref()
            .and_then(|child| benchmark_runtime_from_spec(&child.spec))
            .cloned();
        self.proposal_draft = benchmark_proposal(&task_id, benchmark_runtime.as_ref())?;
        self.begin_run();
        if self.active_run_id.is_none() {
            return Err(self.status.clone().unwrap_or_else(|| {
                "benchmark AgentRun initialization produced no run id".to_owned()
            }));
        }
        Ok(())
    }

    fn prepare_benchmark_execution_profile(&mut self) -> Result<(), String> {
        let (requested, profile) = self
            .benchmark_child
            .as_ref()
            .map(|child| {
                let profile = child
                    .spec
                    .execution_identity
                    .as_ref()
                    .map(|identity| identity.execution_profile.clone());
                (child.profile_requested, profile)
            })
            .ok_or_else(|| "benchmark child is unavailable".to_owned())?;
        let Some(profile) = profile else {
            if let Some(child) = self.benchmark_child.as_mut() {
                child.profile_prepared = true;
            }
            return Ok(());
        };
        if requested || self.model_resource_task.is_some() {
            return Ok(());
        }
        let operation = match profile.as_str() {
            value if value == CampaignExecutionProfile::Warm.label() => {
                ModelResourceOperation::Reload
            }
            value if value == CampaignExecutionProfile::Cold.label() => {
                ModelResourceOperation::Release
            }
            other => return Err(format!("unsupported benchmark execution profile `{other}`")),
        };
        let config = self.selected_local_resource_config().ok_or_else(|| {
            "benchmark execution profile requires a verifiable local model resource adapter"
                .to_owned()
        })?;
        let task =
            ModelResourceTask::spawn(config, operation).map_err(|error| error.to_string())?;
        if let Some(child) = self.benchmark_child.as_mut() {
            child.profile_requested = true;
        }
        self.model_resource_task = Some(task);
        self.model_resource_continuation =
            Some(ModelResourceContinuation::BenchmarkProfilePrepared);
        self.status = Some(format!(
            "Preparing benchmark {} boundary through verified model resource controls.",
            profile
        ));
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
        // ADR 0159 keeps one completion-gate failure kind and separates, inside
        // it, a run that produced no model output from one whose output the
        // runtime rejected. The distinction is derived from recorded exchanges
        // and states the shape of the failure, never the model's text.
        let harness_message = failure_kind.map(|_| match record.metrics.model_turns {
            TelemetryValue::Measured(0) => {
                "completion gate not satisfied and no model output was recorded".to_owned()
            }
            TelemetryValue::Measured(turns) => format!(
                "completion gate not satisfied after {turns} recorded model turn(s) the runtime accepted or rejected"
            ),
            _ => "completion gate not satisfied and model turns were not recorded".to_owned(),
        });
        self.write_benchmark_child_result(
            outcome,
            failure_kind,
            routed,
            harness_message,
            Some(record),
        );
    }

    pub(super) fn write_benchmark_child_failure(
        &mut self,
        kind: BenchmarkRunFailureKind,
        message: String,
    ) {
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
            Err(error) => {
                // The parent can classify a child that exits without a valid
                // result as a harness failure. Remaining alive here would turn
                // a recoverable disk/path error into an infinite wait.
                child.result_written = true;
                let message = format!("Benchmark result write failed: {error}");
                eprintln!("{message}");
                self.status = Some(message);
            }
        }
    }
}

fn benchmark_acp_live_summary(event: &AcpNormalizedEvent) -> String {
    match event {
        AcpNormalizedEvent::AgentMessage { .. } => "agent message received".to_owned(),
        AcpNormalizedEvent::Progress { .. } => "progress update".to_owned(),
        AcpNormalizedEvent::Plan { entries } => {
            format!("plan updated ({} entries)", entries.len())
        }
        AcpNormalizedEvent::ToolCall { status, .. } => {
            format!("tool call is {status:?}")
        }
        AcpNormalizedEvent::SessionInfo { .. } => "session metadata updated".to_owned(),
        AcpNormalizedEvent::ProtocolDiagnostic { .. } => {
            "protocol diagnostic reported".to_owned()
        }
        AcpNormalizedEvent::TurnFinished { stop_reason } => {
            format!("turn finished: {stop_reason:?}")
        }
        AcpNormalizedEvent::PermissionRequest(_) => "permission requested".to_owned(),
    }
}

fn benchmark_runtime_from_spec(spec: &BenchmarkChildRunSpec) -> Option<&BenchmarkRuntimeIdentity> {
    spec.benchmark_runtime()
}

fn validate_benchmark_runtime_spec(spec: &BenchmarkChildRunSpec) -> Result<(), String> {
    let Some(runtime) = benchmark_runtime_from_spec(spec) else {
        return Ok(());
    };
    if runtime.lane == BenchmarkLane::RawModel {
        return Err(
            "ADR0156 Agent Benchmark tasks cannot execute under the raw_model lane".to_owned(),
        );
    }
    if runtime.lane != BenchmarkLane::AgentHarness {
        return Err(
            "first-release Managed Local ACP campaigns support only the agent_harness lane"
                .to_owned(),
        );
    }
    if spec.backend_id != MANAGED_BACKEND_ID {
        return Err(
            "first-release ACP agent_harness campaigns require GameEngine Managed Local".to_owned(),
        );
    }
    if spec.task_id == "read_question_v1" {
        return Err(
            "read_question_v1 is owned by the Native provenance harness and cannot be relabelled as ACP agent_harness evidence"
                .to_owned(),
        );
    }
    if spec.task_id == "visual_evaluation_v1" {
        return Err(
            "visual_evaluation_v1 requires host-captured image content that the current common ACP session boundary does not carry"
                .to_owned(),
        );
    }
    let agent = runtime.agent_runtime.as_ref().ok_or_else(|| {
        "ACP agent_harness benchmark identity is missing its agent runtime".to_owned()
    })?;
    if agent.runtime_id != GOOSE_ACP_AGENT_NAME {
        return Err(format!(
            "first-release Managed Local ACP benchmark expected Goose runtime `{GOOSE_ACP_AGENT_NAME}`, got `{}`",
            agent.runtime_id
        ));
    }
    Ok(())
}

fn benchmark_task_allows_capability(
    task_id: &str,
    runtime: Option<&BenchmarkRuntimeIdentity>,
    capability: AgentCapability,
) -> bool {
    campaign_task_agent_policy_for_runtime(task_id, runtime)
        .is_ok_and(|policy| policy.requested_capabilities.contains(&capability))
}

fn benchmark_proposal(
    task_id: &str,
    runtime: Option<&BenchmarkRuntimeIdentity>,
) -> Result<AgentProposal, String> {
    let mut proposal = AgentProposal::default();
    let agent_policy = campaign_task_agent_policy_for_runtime(task_id, runtime)?;
    proposal.requested_capabilities = agent_policy.requested_capabilities;
    proposal.work_claims = agent_policy.work_claims;
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
        "project_inspection_v1" => {
            proposal.validation_plan = vec!["authoring validation".to_owned()];
        }
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

/// Current wall-clock time in milliseconds since the Unix epoch.
pub(super) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn benchmark_proposals_include_the_frozen_task_authorization() {
        let code = benchmark_proposal("code_implementation_v1", None).expect("code proposal");
        assert_eq!(
            code.requested_capabilities,
            BTreeSet::from([AgentCapability::CodeWorkspaceApply])
        );
        assert_eq!(
            code.work_claims,
            BTreeSet::from([AgentWorkClaim::code_path("game/src/benchmark_target.rs")])
        );

        let inspection =
            benchmark_proposal("project_inspection_v1", None).expect("inspection proposal");
        assert!(inspection.requested_capabilities.is_empty());
        assert!(inspection.work_claims.is_empty());
        assert_eq!(
            inspection.validation_plan,
            vec!["authoring validation".to_owned()]
        );
    }

    #[test]
    fn no_benchmark_task_authorizes_launching_an_external_agent_runtime() {
        // A legacy child that reaches the external-agent route measures a provider the
        // campaign never froze, so the legacy policy must continue to exclude process
        // authority. Agent-inclusive plans opt in through their explicit runtime identity.
        for task_id in [
            "read_question_v1",
            "project_inspection_v1",
            "code_implementation_v1",
            "typed_authoring_mutation_v1",
            "validation_repair_v1",
            "runtime_interaction_v1",
            "visual_evaluation_v1",
        ] {
            let policy =
                campaign_task_agent_policy_for_runtime(task_id, None).expect("task agent policy");
            assert!(
                !policy
                    .requested_capabilities
                    .contains(&AgentCapability::ExternalAgentProcess),
                "task `{task_id}` must not authorize an external agent runtime"
            );
        }
    }

    fn runtime_validation_spec(
        task_id: &str,
        runtime: Option<BenchmarkRuntimeIdentity>,
    ) -> BenchmarkChildRunSpec {
        BenchmarkChildRunSpec {
            schema_version: crate::benchmark_process::BENCHMARK_CHILD_SCHEMA_VERSION,
            experiment_id: "experiment".to_owned(),
            engine_commit_head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            fixture_version: crate::benchmark_experiment::BENCHMARK_FIXTURE_VERSION.to_owned(),
            backend_id: MANAGED_BACKEND_ID.to_owned(),
            managed_execution_environment: Some(ManagedExecutionEnvironment::WindowsNative),
            endpoint: String::new(),
            model_id: "gguf:model".to_owned(),
            task_id: task_id.to_owned(),
            repetition: 0,
            ordinal: 0,
            quality: QualityPreference::Balanced,
            routing_mode: BenchmarkRoutingMode::SingleModel,
            result_path: PathBuf::from("result.json"),
            execution_identity: runtime.map(|benchmark_runtime| {
                crate::agent_benchmark::BenchmarkExecutionIdentity {
                    campaign_harness_version: "campaign-harness-v1".to_owned(),
                    schedule_policy_version: "schedule-v1".to_owned(),
                    comparison_class: "model_comparison".to_owned(),
                    execution_profile: "warm".to_owned(),
                    execution_environment: "windows_native".to_owned(),
                    fixture_id: "fixture".to_owned(),
                    fixture_version: crate::benchmark_experiment::BENCHMARK_FIXTURE_VERSION
                        .to_owned(),
                    fixture_instance_id: "fixture-instance".to_owned(),
                    sampling_profile: "sampling".to_owned(),
                    seed_policy: "seed".to_owned(),
                    benchmark_runtime: Some(benchmark_runtime),
                }
            }),
            candidate_contract: None,
        }
    }

    #[test]
    fn managed_acp_child_accepts_validation_repair_and_rejects_unsupported_evidence() {
        let runtime = BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                GOOSE_ACP_AGENT_NAME,
                Some("1.0.0".to_owned()),
            ),
        );
        assert!(
            validate_benchmark_runtime_spec(&runtime_validation_spec(
                "validation_repair_v1",
                Some(runtime.clone()),
            ))
            .is_ok()
        );
        for task_id in ["read_question_v1", "visual_evaluation_v1"] {
            assert!(
                validate_benchmark_runtime_spec(&runtime_validation_spec(
                    task_id,
                    Some(runtime.clone()),
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn explicit_legacy_managed_child_remains_valid_without_acp_identity() {
        assert!(
            validate_benchmark_runtime_spec(&runtime_validation_spec("read_question_v1", None,))
                .is_ok()
        );
    }

    #[test]
    fn headless_acp_auto_approval_refuses_capabilities_outside_the_frozen_policy() {
        let runtime = BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                GOOSE_ACP_AGENT_NAME,
                Some("1.0.0".to_owned()),
            ),
        );
        assert!(benchmark_task_allows_capability(
            "code_implementation_v1",
            Some(&runtime),
            AgentCapability::ExternalAgentProcess,
        ));
        assert!(benchmark_task_allows_capability(
            "code_implementation_v1",
            Some(&runtime),
            AgentCapability::CodeWorkspaceApply,
        ));
        assert!(!benchmark_task_allows_capability(
            "code_implementation_v1",
            Some(&runtime),
            AgentCapability::RuntimeLaunch,
        ));
    }

    #[test]
    fn acp_benchmark_proposal_uses_the_frozen_agent_inclusive_authority() {
        let runtime = BenchmarkRuntimeIdentity::gameengine_acp_agent_harness(
            &crate::acp_agent_runtime::AcpRuntimeIdentity::stable(
                GOOSE_ACP_AGENT_NAME,
                Some("1.0.0".to_owned()),
            ),
        );
        let proposal = benchmark_proposal("code_implementation_v1", Some(&runtime))
            .expect("ACP code proposal");
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::ExternalAgentProcess)
        );
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::CodeWorkspaceApply)
        );
    }
}
