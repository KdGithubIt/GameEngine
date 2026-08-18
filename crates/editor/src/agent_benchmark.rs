//! Versioned GameEngine Agent Benchmark records and evidence-derived local model catalog.
//!
//! Benchmark data is machine-local application data. Records intentionally contain no
//! conversation transcript, retrieved source text, project path, credentials, or model prompt.

use crate::agent_host::{
    AgentEventEvidence, AgentEventKind, AgentRun, AgentRunState, CompletionStatus,
};
use crate::native_agent::{InstalledModelInventory, NativeMetrics};
use crate::native_agent_runtime::{HarnessPolicy, NATIVE_WRITE_HARNESS_VERSION};
use crate::resource_arbitration::{InferenceWorkload, QualityPreference, TelemetryValue};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const BENCHMARK_SCHEMA_VERSION: u32 = 1;
pub(crate) const BENCHMARK_CORPUS_VERSION: &str = "gameengine-agent-v1";
pub(crate) const BENCHMARK_HARNESS_VERSION: &str = "gameengine-agent-benchmark-harness-v1";
const WORKLOAD_POLICY_VERSION: &str = "adr0135-workload-policy-v1";
const CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BenchmarkTaskKind {
    ReadQuestion,
    ProjectInspection,
    CodeImplementation,
    TypedAuthoringMutation,
    ValidationRepair,
    RuntimeInteraction,
    VisualEvaluation,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BenchmarkTaskDescriptor {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: BenchmarkTaskKind,
    pub(crate) completion_criteria: &'static [&'static str],
}

pub(crate) const BENCHMARK_TASKS: [BenchmarkTaskDescriptor; 7] = [
    BenchmarkTaskDescriptor {
        id: "read_question_v1",
        label: "Read question",
        kind: BenchmarkTaskKind::ReadQuestion,
        completion_criteria: &["answer_returned", "provenance_reported"],
    },
    BenchmarkTaskDescriptor {
        id: "project_inspection_v1",
        label: "Project inspection",
        kind: BenchmarkTaskKind::ProjectInspection,
        completion_criteria: &["acceptance_criteria", "authoring_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "code_implementation_v1",
        label: "Code implementation",
        kind: BenchmarkTaskKind::CodeImplementation,
        completion_criteria: &["acceptance_criteria", "source_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "typed_authoring_mutation_v1",
        label: "Typed authoring mutation",
        kind: BenchmarkTaskKind::TypedAuthoringMutation,
        completion_criteria: &["acceptance_criteria", "authoring_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "validation_repair_v1",
        label: "Validation and repair",
        kind: BenchmarkTaskKind::ValidationRepair,
        completion_criteria: &["acceptance_criteria", "source_validation"],
    },
    BenchmarkTaskDescriptor {
        id: "runtime_interaction_v1",
        label: "Runtime interaction",
        kind: BenchmarkTaskKind::RuntimeInteraction,
        completion_criteria: &["play_launch", "interaction_scenarios"],
    },
    BenchmarkTaskDescriptor {
        id: "visual_evaluation_v1",
        label: "Visual evaluation",
        kind: BenchmarkTaskKind::VisualEvaluation,
        completion_criteria: &["frame_capture", "visual_evaluation"],
    },
];

pub(crate) fn benchmark_task(id: &str) -> Option<&'static BenchmarkTaskDescriptor> {
    BENCHMARK_TASKS.iter().find(|task| task.id == id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkModelIdentity {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: TelemetryValue<String>,
    pub(crate) quantization: TelemetryValue<String>,
    pub(crate) representation_size_bytes: TelemetryValue<u64>,
    pub(crate) backend_runtime_version: TelemetryValue<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkHardwareIdentity {
    pub(crate) platform: String,
    pub(crate) gpu: TelemetryValue<String>,
    pub(crate) total_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) total_system_memory_bytes: TelemetryValue<u64>,
}

impl Default for BenchmarkHardwareIdentity {
    fn default() -> Self {
        Self {
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            gpu: TelemetryValue::Unavailable,
            total_gpu_memory_bytes: TelemetryValue::Unavailable,
            total_system_memory_bytes: TelemetryValue::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkToolBudget {
    pub(crate) max_model_turns: u32,
    pub(crate) max_tool_failures: u32,
    pub(crate) repair_budget: u32,
    pub(crate) permission_budget: Vec<String>,
    #[serde(default)]
    pub(crate) work_claims: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkIdentity {
    pub(crate) corpus_version: String,
    pub(crate) task_id: String,
    pub(crate) harness_version: String,
    pub(crate) runtime_harness_version: String,
    pub(crate) model: BenchmarkModelIdentity,
    pub(crate) hardware: BenchmarkHardwareIdentity,
    pub(crate) quality: QualityPreference,
    pub(crate) workload_policy_version: String,
    pub(crate) observed_workload: TelemetryValue<InferenceWorkload>,
    pub(crate) tool_budget: BenchmarkToolBudget,
    pub(crate) completion_criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkMetrics {
    pub(crate) acceptance_success: TelemetryValue<bool>,
    pub(crate) completion_success: TelemetryValue<bool>,
    pub(crate) model_turns: TelemetryValue<u64>,
    pub(crate) tool_calls: TelemetryValue<u64>,
    pub(crate) invalid_or_failed_tool_calls: TelemetryValue<u64>,
    pub(crate) code_edits: TelemetryValue<u64>,
    pub(crate) validation_attempts: TelemetryValue<u64>,
    pub(crate) repair_loops: TelemetryValue<u64>,
    pub(crate) play_attempts: TelemetryValue<u64>,
    pub(crate) frame_capture_attempts: TelemetryValue<u64>,
    pub(crate) visual_evaluation_attempts: TelemetryValue<u64>,
    pub(crate) human_interventions: TelemetryValue<u64>,
    pub(crate) elapsed_ms: TelemetryValue<u64>,
    pub(crate) prompt_tokens: TelemetryValue<u64>,
    pub(crate) response_tokens: TelemetryValue<u64>,
    pub(crate) load_latency_ms: TelemetryValue<u64>,
    pub(crate) ttft_ms: TelemetryValue<u64>,
    pub(crate) generation_tokens_per_second_milli: TelemetryValue<u64>,
    pub(crate) peak_backend_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) peak_editor_gpu_memory_bytes: TelemetryValue<u64>,
    pub(crate) model_unload_reload_ms: TelemetryValue<u64>,
    pub(crate) renderer_reclaim_resume_ms: TelemetryValue<u64>,
    pub(crate) oom_failures: TelemetryValue<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BenchmarkRecord {
    pub(crate) schema_version: u32,
    pub(crate) recorded_unix_ms: u64,
    pub(crate) identity: BenchmarkIdentity,
    pub(crate) metrics: BenchmarkMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComparisonEquivalence {
    EquivalentModelComparison,
    NonEquivalent(Vec<&'static str>),
}

fn model_identity_is_measured(identity: &BenchmarkModelIdentity) -> bool {
    matches!(&identity.model_version, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(&identity.quantization, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(identity.representation_size_bytes, TelemetryValue::Measured(value) if value > 0)
        && matches!(&identity.backend_runtime_version, TelemetryValue::Measured(value) if !value.trim().is_empty())
}

fn hardware_identity_is_measured(identity: &BenchmarkHardwareIdentity) -> bool {
    !identity.platform.trim().is_empty()
        && matches!(&identity.gpu, TelemetryValue::Measured(value) if !value.trim().is_empty())
        && matches!(identity.total_gpu_memory_bytes, TelemetryValue::Measured(value) if value > 0)
        && matches!(identity.total_system_memory_bytes, TelemetryValue::Measured(value) if value > 0)
}

fn benchmark_identity_is_measured(identity: &BenchmarkIdentity) -> bool {
    model_identity_is_measured(&identity.model)
        && hardware_identity_is_measured(&identity.hardware)
        && matches!(identity.observed_workload, TelemetryValue::Measured(_))
}

pub(crate) fn comparison_equivalence(
    left: &BenchmarkRecord,
    right: &BenchmarkRecord,
) -> ComparisonEquivalence {
    let mut differences = Vec::new();
    if left.identity.corpus_version != right.identity.corpus_version {
        differences.push("corpus_version");
    }
    if left.identity.task_id != right.identity.task_id {
        differences.push("task_id");
    }
    if left.identity.harness_version != right.identity.harness_version
        || left.identity.runtime_harness_version != right.identity.runtime_harness_version
    {
        differences.push("harness_version");
    }
    if !model_identity_is_measured(&left.identity.model)
        || !model_identity_is_measured(&right.identity.model)
    {
        differences.push("model_representation");
    }
    if left.identity.model.backend_id != right.identity.model.backend_id
        || left.identity.model.backend_runtime_version != right.identity.model.backend_runtime_version
    {
        differences.push("backend_runtime");
    }
    if !hardware_identity_is_measured(&left.identity.hardware)
        || !hardware_identity_is_measured(&right.identity.hardware)
        || left.identity.hardware != right.identity.hardware
    {
        differences.push("hardware");
    }
    if left.identity.quality != right.identity.quality
        || left.identity.workload_policy_version != right.identity.workload_policy_version
        || !matches!(left.identity.observed_workload, TelemetryValue::Measured(_))
        || !matches!(right.identity.observed_workload, TelemetryValue::Measured(_))
        || left.identity.observed_workload != right.identity.observed_workload
    {
        differences.push("quality_or_workload");
    }
    if left.identity.tool_budget != right.identity.tool_budget {
        differences.push("tool_or_permission_budget");
    }
    if left.identity.completion_criteria != right.identity.completion_criteria {
        differences.push("completion_criteria");
    }
    if differences.is_empty() {
        ComparisonEquivalence::EquivalentModelComparison
    } else {
        ComparisonEquivalence::NonEquivalent(differences)
    }
}

pub(crate) struct BenchmarkStore {
    root: PathBuf,
}

impl BenchmarkStore {
    pub(crate) fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    pub(crate) fn load(&self) -> Result<Vec<BenchmarkRecord>, String> {
        let mut paths = fs::read_dir(&self.root)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == std::ffi::OsStr::new("json")))
            .collect::<Vec<_>>();
        paths.sort();
        let mut records = Vec::new();
        for path in paths {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            let record = serde_json::from_slice::<BenchmarkRecord>(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            validate_record(&record)?;
            records.push(record);
        }
        Ok(records)
    }

    pub(crate) fn record(&self, record: &BenchmarkRecord) -> Result<PathBuf, String> {
        validate_record(record)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
        let stem = format!(
            "{}-{}-{}",
            record.recorded_unix_ms,
            safe_file_component(&record.identity.task_id),
            safe_file_component(&record.identity.model.model_id),
        );
        for suffix in 0..1_000_u32 {
            let file_name = if suffix == 0 {
                format!("{stem}.json")
            } else {
                format!("{stem}-{suffix}.json")
            };
            let path = self.root.join(file_name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(&bytes).map_err(|error| error.to_string())?;
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("could not allocate a unique benchmark record file".to_owned())
    }
}

fn validate_record(record: &BenchmarkRecord) -> Result<(), String> {
    if record.schema_version != BENCHMARK_SCHEMA_VERSION {
        return Err(format!("unsupported benchmark schema version {}", record.schema_version));
    }
    if record.identity.corpus_version != BENCHMARK_CORPUS_VERSION {
        return Err(format!("unsupported benchmark corpus `{}`", record.identity.corpus_version));
    }
    let Some(task) = benchmark_task(&record.identity.task_id) else {
        return Err(format!("unknown benchmark task `{}`", record.identity.task_id));
    };
    let expected = task.completion_criteria.iter().map(|criterion| (*criterion).to_owned()).collect::<Vec<_>>();
    if record.identity.completion_criteria != expected {
        return Err("benchmark completion criteria do not match the versioned corpus task".to_owned());
    }
    if record.identity.model.backend_id.trim().is_empty() || record.identity.model.model_id.trim().is_empty() {
        return Err("benchmark backend and model identity must be non-empty".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogManifest {
    schema_version: u32,
    catalog_version: String,
    #[serde(default)]
    entries: Vec<CatalogCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct CatalogCandidate {
    pub(crate) backend_id: String,
    pub(crate) model_id: String,
    pub(crate) model_version: String,
    pub(crate) quantization: String,
    pub(crate) source: String,
    pub(crate) license: String,
    pub(crate) transfer_size_bytes: u64,
    pub(crate) storage_size_bytes: u64,
    pub(crate) memory_guidance: String,
    pub(crate) context_limit: Option<u64>,
    #[serde(default)]
    pub(crate) modalities: Vec<String>,
    #[serde(default)]
    pub(crate) tool_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogProfile {
    Lightweight,
    Balanced,
    HighQuality,
}

impl CatalogProfile {
    pub(crate) const ALL: [Self; 3] = [Self::Lightweight, Self::Balanced, Self::HighQuality];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Lightweight => "Lightweight",
            Self::Balanced => "Balanced",
            Self::HighQuality => "High Quality",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogRecommendation {
    pub(crate) profile: CatalogProfile,
    pub(crate) candidate: CatalogCandidate,
    pub(crate) benchmark_version: String,
    pub(crate) evidence_runs: usize,
    pub(crate) aggregate_elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CuratedModelCatalog {
    pub(crate) catalog_version: String,
    recommendations: Vec<CatalogRecommendation>,
}

impl CuratedModelCatalog {
    pub(crate) fn from_bundled_manifest(records: &[BenchmarkRecord]) -> Result<Self, String> {
        let manifest = serde_json::from_str::<CatalogManifest>(include_str!(
            "../resources/local_model_catalog_v1.json"
        ))
        .map_err(|error| error.to_string())?;
        Self::derive(manifest, records)
    }

    fn derive(manifest: CatalogManifest, records: &[BenchmarkRecord]) -> Result<Self, String> {
        if manifest.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported local model catalog schema {}",
                manifest.schema_version
            ));
        }
        let mut qualified = Vec::new();
        for candidate in &manifest.entries {
            qualified.extend(qualify_candidate(candidate, records));
        }
        let Some(reference) = qualified.first().cloned() else {
            return Ok(Self {
                catalog_version: manifest.catalog_version,
                recommendations: Vec::new(),
            });
        };
        let comparable = qualified
            .into_iter()
            .filter(|candidate| candidate.context == reference.context)
            .filter(|candidate| candidate.is_equivalent_to(&reference))
            .collect::<Vec<_>>();
        let mut recommendations = Vec::new();
        if let Some(fastest) = comparable
            .iter()
            .min_by_key(|evidence| evidence.aggregate_elapsed_ms)
            .cloned()
        {
            recommendations.push(fastest.recommendation(CatalogProfile::Lightweight));
        }
        if let Some(balanced) = comparable
            .iter()
            .min_by_key(|evidence| {
                evidence
                    .aggregate_elapsed_ms
                    .saturating_add(evidence.repair_penalty_ms)
            })
            .cloned()
        {
            recommendations.push(balanced.recommendation(CatalogProfile::Balanced));
        }
        if let Some(high_quality) = comparable
            .iter()
            .min_by_key(|evidence| (evidence.repair_penalty_ms, evidence.aggregate_elapsed_ms))
            .cloned()
        {
            recommendations.push(high_quality.recommendation(CatalogProfile::HighQuality));
        }
        Ok(Self {
            catalog_version: manifest.catalog_version,
            recommendations,
        })
    }

    pub(crate) fn recommendation(&self, profile: CatalogProfile) -> Option<&CatalogRecommendation> {
        self.recommendations
            .iter()
            .find(|recommendation| recommendation.profile == profile)
    }

    pub(crate) fn profiles_for_model(
        &self,
        backend_id: &str,
        model_id: &str,
    ) -> Vec<CatalogProfile> {
        self.recommendations
            .iter()
            .filter(|recommendation| {
                recommendation.candidate.backend_id == backend_id
                    && recommendation.candidate.model_id == model_id
            })
            .map(|recommendation| recommendation.profile)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchmarkSuiteContext {
    corpus_version: String,
    harness_version: String,
    backend_id: String,
    backend_runtime_version: TelemetryValue<String>,
    hardware: BenchmarkHardwareIdentity,
    quality: QualityPreference,
    workload_policy_version: String,
}

impl BenchmarkSuiteContext {
    fn from_record(record: &BenchmarkRecord) -> Self {
        Self {
            corpus_version: record.identity.corpus_version.clone(),
            harness_version: record.identity.harness_version.clone(),
            backend_id: record.identity.model.backend_id.clone(),
            backend_runtime_version: record.identity.model.backend_runtime_version.clone(),
            hardware: record.identity.hardware.clone(),
            quality: record.identity.quality,
            workload_policy_version: record.identity.workload_policy_version.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct QualifiedCandidate {
    candidate: CatalogCandidate,
    context: BenchmarkSuiteContext,
    task_records: Vec<BenchmarkRecord>,
    aggregate_elapsed_ms: u64,
    repair_penalty_ms: u64,
}

impl QualifiedCandidate {
    fn recommendation(self, profile: CatalogProfile) -> CatalogRecommendation {
        CatalogRecommendation {
            profile,
            candidate: self.candidate,
            benchmark_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            evidence_runs: self.task_records.len(),
            aggregate_elapsed_ms: self.aggregate_elapsed_ms,
        }
    }

    fn is_equivalent_to(&self, other: &Self) -> bool {
        BENCHMARK_TASKS.iter().all(|task| {
            let left = self
                .task_records
                .iter()
                .find(|record| record.identity.task_id == task.id);
            let right = other
                .task_records
                .iter()
                .find(|record| record.identity.task_id == task.id);
            matches!(
                (left, right),
                (Some(left), Some(right))
                    if comparison_equivalence(left, right)
                        == ComparisonEquivalence::EquivalentModelComparison
            )
        })
    }
}

fn qualify_candidate(
    candidate: &CatalogCandidate,
    records: &[BenchmarkRecord],
) -> Vec<QualifiedCandidate> {
    if candidate.source.trim().is_empty() || candidate.license.trim().is_empty() {
        return Vec::new();
    }
    let matching = records
        .iter()
        .filter(|record| {
            benchmark_identity_is_measured(&record.identity)
                && candidate_matches_record(candidate, record)
        })
        .collect::<Vec<_>>();
    let mut qualified = Vec::new();
    for seed in &matching {
        let context = BenchmarkSuiteContext::from_record(seed);
        if qualified
            .iter()
            .any(|candidate: &QualifiedCandidate| candidate.context == context)
        {
            continue;
        }
        let cohort = matching
            .iter()
            .copied()
            .filter(|record| BenchmarkSuiteContext::from_record(record) == context)
            .collect::<Vec<_>>();
        let mut task_records = Vec::new();
        let mut elapsed = 0_u64;
        let mut repair_penalty = 0_u64;
        let mut complete = true;
        for task in BENCHMARK_TASKS {
            let record = cohort.iter().find(|record| {
                record.identity.task_id == task.id
                    && matches!(
                        record.metrics.completion_success,
                        TelemetryValue::Measured(true)
                    )
            });
            let Some(record) = record.copied() else {
                complete = false;
                break;
            };
            let TelemetryValue::Measured(task_elapsed) = record.metrics.elapsed_ms else {
                complete = false;
                break;
            };
            elapsed = elapsed.saturating_add(task_elapsed);
            if let TelemetryValue::Measured(repairs) = record.metrics.repair_loops {
                repair_penalty = repair_penalty.saturating_add(repairs.saturating_mul(30_000));
            }
            if let TelemetryValue::Measured(interventions) = record.metrics.human_interventions {
                repair_penalty =
                    repair_penalty.saturating_add(interventions.saturating_mul(60_000));
            }
            task_records.push((*record).clone());
        }
        if complete {
            qualified.push(QualifiedCandidate {
                candidate: candidate.clone(),
                context,
                task_records,
                aggregate_elapsed_ms: elapsed,
                repair_penalty_ms: repair_penalty,
            });
        }
    }
    qualified
}

fn candidate_matches_record(candidate: &CatalogCandidate, record: &BenchmarkRecord) -> bool {
    record.identity.model.backend_id == candidate.backend_id
        && record.identity.model.model_id == candidate.model_id
        && matches!(
            &record.identity.model.model_version,
            TelemetryValue::Measured(version) if version == &candidate.model_version
        )
        && matches!(
            &record.identity.model.quantization,
            TelemetryValue::Measured(quantization) if quantization == &candidate.quantization
        )
}

pub(crate) fn model_identity(
    backend_id: &str,
    model_id: &str,
    inventory: Option<&InstalledModelInventory>,
) -> BenchmarkModelIdentity {
    let installed = inventory.and_then(|inventory| inventory.models.iter().find(|model| model.name == model_id));
    BenchmarkModelIdentity {
        backend_id: backend_id.to_owned(),
        model_id: model_id.to_owned(),
        model_version: installed
            .and_then(|model| model.digest.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        quantization: installed
            .and_then(|model| model.quantization_level.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        representation_size_bytes: installed
            .and_then(|model| model.size_bytes)
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
        backend_runtime_version: inventory
            .and_then(|inventory| inventory.backend_version.clone())
            .map(TelemetryValue::Measured)
            .unwrap_or_default(),
    }
}

pub(crate) fn read_question_record(
    task_id: &str,
    metrics: &NativeMetrics,
    inventory: Option<&InstalledModelInventory>,
    quality: QualityPreference,
    workload: InferenceWorkload,
) -> Result<BenchmarkRecord, String> {
    let task = benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    if task.kind != BenchmarkTaskKind::ReadQuestion {
        return Err("the last read-oriented result can only record a read-question benchmark task".to_owned());
    }
    Ok(BenchmarkRecord {
        schema_version: BENCHMARK_SCHEMA_VERSION,
        recorded_unix_ms: unix_ms(),
        identity: BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: task.id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: metrics.harness_version.to_owned(),
            model: model_identity(metrics.backend_id, &metrics.model_id, inventory),
            hardware: BenchmarkHardwareIdentity::default(),
            quality,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(workload),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 1,
                max_tool_failures: 0,
                repair_budget: 0,
                permission_budget: vec!["read_only".to_owned()],
                work_claims: Vec::new(),
            },
            completion_criteria: task.completion_criteria.iter().map(|criterion| (*criterion).to_owned()).collect(),
        },
        metrics: BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(true),
            completion_success: TelemetryValue::Measured(true),
            model_turns: TelemetryValue::Measured(u64::from(metrics.model_turns)),
            tool_calls: TelemetryValue::Measured(0),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(0),
            code_edits: TelemetryValue::Measured(0),
            validation_attempts: TelemetryValue::Measured(0),
            repair_loops: TelemetryValue::Measured(0),
            play_attempts: TelemetryValue::Measured(0),
            frame_capture_attempts: TelemetryValue::Measured(0),
            visual_evaluation_attempts: TelemetryValue::Measured(0),
            human_interventions: TelemetryValue::Measured(0),
            elapsed_ms: TelemetryValue::Measured(metrics.elapsed_ms),
            prompt_tokens: optional_measured(metrics.prompt_eval_tokens),
            response_tokens: optional_measured(metrics.response_tokens),
            load_latency_ms: optional_measured(metrics.load_latency_ms),
            ttft_ms: optional_measured(metrics.ttft_ms),
            generation_tokens_per_second_milli: optional_measured(metrics.generation_tokens_per_second_milli),
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Unavailable,
        },
    })
}

pub(crate) fn agent_run_record(
    task_id: &str,
    run: &AgentRun,
    backend_id: &str,
    model_id: &str,
    inventory: Option<&InstalledModelInventory>,
    quality: QualityPreference,
) -> Result<BenchmarkRecord, String> {
    let task = benchmark_task(task_id).ok_or_else(|| format!("unknown benchmark task `{task_id}`"))?;
    if task.kind == BenchmarkTaskKind::ReadQuestion {
        return Err("write-capable AgentRun evidence cannot be recorded as a read-question task".to_owned());
    }
    if !matches!(run.state, AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled) {
        return Err("benchmark evidence requires a terminal AgentRun".to_owned());
    }
    let policy = HarnessPolicy::default();
    let permission_budget = run
        .proposal_snapshot
        .requested_capabilities
        .iter()
        .filter_map(|capability| serde_json::to_value(capability).ok())
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let tool_calls = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::ToolAction { .. })))
        .count() as u64;
    let failed_tool_calls = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::ToolAction { success: Some(false), .. })))
        .count() as u64;
    let play_attempts = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::Playtest { .. })))
        .count() as u64;
    let frame_attempts = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::CapturedFrame { .. })))
        .count() as u64;
    let visual_attempts = run
        .events
        .iter()
        .filter(|event| matches!(&event.evidence, Some(AgentEventEvidence::CompletionGate { gate, .. }) if gate == "visual_evaluation"))
        .count() as u64;
    let human_interventions = run
        .events
        .iter()
        .filter(|event| event.kind == AgentEventKind::UserMessage)
        .count() as u64;
    let elapsed_ms = run.finished_unix_ms.map(|finished| finished.saturating_sub(run.started_unix_ms));
    let completion_success = completion_success(run);
    Ok(BenchmarkRecord {
        schema_version: BENCHMARK_SCHEMA_VERSION,
        recorded_unix_ms: unix_ms(),
        identity: BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: task.id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: NATIVE_WRITE_HARNESS_VERSION.to_owned(),
            model: model_identity(backend_id, model_id, inventory),
            hardware: BenchmarkHardwareIdentity::default(),
            quality,
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Unavailable,
            tool_budget: BenchmarkToolBudget {
                max_model_turns: policy.max_model_turns,
                max_tool_failures: policy.max_tool_failures,
                repair_budget: policy.repair_budget,
                permission_budget,
                work_claims: run
                    .proposal_snapshot
                    .work_claims
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            },
            completion_criteria: task.completion_criteria.iter().map(|criterion| (*criterion).to_owned()).collect(),
        },
        metrics: BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(run.completion.acceptance_criteria == CompletionStatus::Passed),
            completion_success: TelemetryValue::Measured(completion_success),
            model_turns: TelemetryValue::Unavailable,
            tool_calls: TelemetryValue::Measured(tool_calls),
            invalid_or_failed_tool_calls: TelemetryValue::Measured(failed_tool_calls),
            code_edits: TelemetryValue::Measured(run.audit.code_changes),
            validation_attempts: TelemetryValue::Measured(run.validation_attempts.len() as u64),
            repair_loops: TelemetryValue::Unavailable,
            play_attempts: TelemetryValue::Measured(play_attempts),
            frame_capture_attempts: TelemetryValue::Measured(frame_attempts),
            visual_evaluation_attempts: TelemetryValue::Measured(visual_attempts),
            human_interventions: TelemetryValue::Measured(human_interventions),
            elapsed_ms: elapsed_ms.map(TelemetryValue::Measured).unwrap_or_default(),
            prompt_tokens: TelemetryValue::Unavailable,
            response_tokens: TelemetryValue::Unavailable,
            load_latency_ms: TelemetryValue::Unavailable,
            ttft_ms: TelemetryValue::Unavailable,
            generation_tokens_per_second_milli: TelemetryValue::Unavailable,
            peak_backend_gpu_memory_bytes: TelemetryValue::Unavailable,
            peak_editor_gpu_memory_bytes: TelemetryValue::Unavailable,
            model_unload_reload_ms: TelemetryValue::Unavailable,
            renderer_reclaim_resume_ms: TelemetryValue::Unavailable,
            oom_failures: TelemetryValue::Unavailable,
        },
    })
}

fn completion_success(run: &AgentRun) -> bool {
    [
        run.completion.acceptance_criteria,
        run.completion.authoring_validation,
        run.completion.source_validation,
        run.completion.play_launch,
        run.completion.frame_capture,
        run.completion.visual_evaluation,
        run.completion.interaction_scenarios,
    ]
    .into_iter()
    .all(|status| matches!(status, CompletionStatus::Passed | CompletionStatus::NotApplicable))
}

fn optional_measured(value: Option<u64>) -> TelemetryValue<u64> {
    value.map(TelemetryValue::Measured).unwrap_or_default()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn safe_file_component(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') { character } else { '-' })
        .collect::<String>();
    if output.len() > 80 {
        output.truncate(80);
    }
    if output.is_empty() { "unknown".to_owned() } else { output }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured_identity(model: &str) -> BenchmarkIdentity {
        BenchmarkIdentity {
            corpus_version: BENCHMARK_CORPUS_VERSION.to_owned(),
            task_id: BENCHMARK_TASKS[0].id.to_owned(),
            harness_version: BENCHMARK_HARNESS_VERSION.to_owned(),
            runtime_harness_version: "runtime-harness-v1".to_owned(),
            model: BenchmarkModelIdentity {
                backend_id: "test-backend".to_owned(),
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
            workload_policy_version: WORKLOAD_POLICY_VERSION.to_owned(),
            observed_workload: TelemetryValue::Measured(InferenceWorkload::InteractiveReasoning),
            tool_budget: BenchmarkToolBudget {
                max_model_turns: 24,
                max_tool_failures: 4,
                repair_budget: 2,
                permission_budget: vec!["managed".to_owned()],
                work_claims: vec!["code_path:game".to_owned()],
            },
            completion_criteria: BENCHMARK_TASKS[0].completion_criteria.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    fn measured_metrics(elapsed_ms: u64) -> BenchmarkMetrics {
        BenchmarkMetrics {
            acceptance_success: TelemetryValue::Measured(true),
            completion_success: TelemetryValue::Measured(true),
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
        }
    }

    fn record(model: &str, task: BenchmarkTaskDescriptor, elapsed_ms: u64) -> BenchmarkRecord {
        let mut identity = measured_identity(model);
        identity.task_id = task.id.to_owned();
        identity.completion_criteria = task.completion_criteria.iter().map(|value| (*value).to_owned()).collect();
        BenchmarkRecord {
            schema_version: BENCHMARK_SCHEMA_VERSION,
            recorded_unix_ms: 1,
            identity,
            metrics: measured_metrics(elapsed_ms),
        }
    }

    #[test]
    fn corpus_covers_required_first_release_workloads() {
        assert_eq!(BENCHMARK_TASKS.len(), 7);
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::ReadQuestion));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::ProjectInspection));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::CodeImplementation));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::TypedAuthoringMutation));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::ValidationRepair));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::RuntimeInteraction));
        assert!(BENCHMARK_TASKS.iter().any(|task| task.kind == BenchmarkTaskKind::VisualEvaluation));
    }

    #[test]
    fn comparison_is_model_only_when_every_harness_dimension_matches() {
        let left = record("model-a", BENCHMARK_TASKS[0], 10);
        let right = record("model-b", BENCHMARK_TASKS[0], 10);
        assert_eq!(comparison_equivalence(&left, &right), ComparisonEquivalence::EquivalentModelComparison);
        let mut changed = right.clone();
        changed.identity.hardware.platform = "different".to_owned();
        assert!(matches!(comparison_equivalence(&left, &changed), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"hardware")));

        let mut incomplete_hardware = right.clone();
        incomplete_hardware.identity.hardware.gpu = TelemetryValue::Unavailable;
        assert!(matches!(comparison_equivalence(&left, &incomplete_hardware), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"hardware")));

        let mut incomplete_model = right.clone();
        incomplete_model.identity.model.representation_size_bytes = TelemetryValue::Unavailable;
        assert!(matches!(comparison_equivalence(&left, &incomplete_model), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"model_representation")));

        let mut different_claims = right.clone();
        different_claims.identity.tool_budget.work_claims = vec!["code_path:assets".to_owned()];
        assert!(matches!(comparison_equivalence(&left, &different_claims), ComparisonEquivalence::NonEquivalent(fields) if fields.contains(&"tool_or_permission_budget")));
    }

    #[test]
    fn unavailable_telemetry_serializes_explicitly() {
        let value = serde_json::to_string(&TelemetryValue::<u64>::Unavailable).expect("telemetry JSON");
        assert!(value.contains("unavailable"));
    }

    #[test]
    fn persisted_record_contains_no_prompt_project_path_or_credentials() {
        let root = tempfile::tempdir().expect("benchmark tempdir");
        let store = BenchmarkStore::open(root.path().to_path_buf()).expect("benchmark store");
        let benchmark = record("safe-model", BENCHMARK_TASKS[0], 10);
        let path = store.record(&benchmark).expect("record benchmark");
        let text = fs::read_to_string(path).expect("record text");
        assert!(!text.contains("secret-token"));
        assert!(!text.contains("C:\\private\\project"));
        assert!(!text.contains("conversation"));
        assert!(!text.contains("\"prompt_text\""));
        assert!(text.contains("\"prompt_tokens\""));
    }

    #[test]
    fn catalog_requires_complete_same_context_success_and_provenance() {
        let candidate = CatalogCandidate {
            backend_id: "test-backend".to_owned(),
            model_id: "model-a".to_owned(),
            model_version: "model-a-digest".to_owned(),
            quantization: "q4".to_owned(),
            source: "https://example.invalid/model".to_owned(),
            license: "test-license".to_owned(),
            transfer_size_bytes: 1,
            storage_size_bytes: 1,
            memory_guidance: "test".to_owned(),
            context_limit: Some(1),
            modalities: vec!["text".to_owned()],
            tool_capabilities: vec!["structured".to_owned()],
        };
        let partial = vec![record("model-a", BENCHMARK_TASKS[0], 10)];
        let manifest = CatalogManifest { schema_version: CATALOG_SCHEMA_VERSION, catalog_version: "test".to_owned(), entries: vec![candidate.clone()] };
        let catalog = CuratedModelCatalog::derive(manifest.clone(), &partial).expect("partial catalog");
        assert!(catalog.recommendation(CatalogProfile::Balanced).is_none());

        let complete = BENCHMARK_TASKS.iter().copied().map(|task| record("model-a", task, 10)).collect::<Vec<_>>();
        let mut incomplete_identity = complete.clone();
        for record in &mut incomplete_identity {
            record.identity.hardware.gpu = TelemetryValue::Unavailable;
        }
        let incomplete_catalog = CuratedModelCatalog::derive(manifest.clone(), &incomplete_identity)
            .expect("incomplete identity catalog");
        assert!(incomplete_catalog.recommendation(CatalogProfile::Balanced).is_none());

        let catalog = CuratedModelCatalog::derive(manifest, &complete).expect("complete catalog");
        let recommendation = catalog.recommendation(CatalogProfile::Balanced).expect("balanced recommendation");
        assert_eq!(recommendation.candidate.model_id, candidate.model_id);
        assert_eq!(recommendation.evidence_runs, BENCHMARK_TASKS.len());
    }
}
