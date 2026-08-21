//! GameEngine-managed local inference and agent-runtime lifecycle for ADR 0155/0166.
//!
//! This module owns machine-local llama.cpp installation, model registration,
//! pinned Goose ACP runtime installation, Windows/WSL execution-environment setup,
//! and demand-driven loopback process lifecycle. It deliberately has no authoring
//! or egui dependency.

mod gguf;

pub(crate) use gguf::GgufModelCapability;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const MANAGED_BACKEND_ID: &str = "gameengine-managed-llama-cpp";
pub(crate) const PINNED_LLAMA_CPP_TAG: &str = "b10336";
pub(crate) const PINNED_LLAMA_CPP_REVISION: &str = "f401bb1";
/// Goose version pinned for the GameEngine-managed ACP agent runtime.
pub(crate) const PINNED_GOOSE_VERSION: &str = "1.45.0+gameengine.ge-midturn-2";
const PINNED_GOOSE_WINDOWS_ASSET: &str =
    "gameengine-managed-goose-v1.45.0-ge-midturn-2-x86_64-pc-windows-msvc.zip";
const PINNED_GOOSE_WINDOWS_SHA256: &str =
    "b9ab2de08972b3cee38b3262a78702726fc1d5d87ffbccb9c166f208cbc0444a";
const PINNED_GOOSE_EXECUTABLE_RELATIVE_PATH: &str = "goose.exe";
const PINNED_GOOSE_WINDOWS_URL: &str = concat!(
    "https://github.com/KdGithubIt/GameEngine/releases/download/",
    "managed-goose-v1.45.0-ge-midturn-2/",
    "gameengine-managed-goose-v1.45.0-ge-midturn-2-x86_64-pc-windows-msvc.zip"
);
const GOOSE_RUNTIME_STATE_SCHEMA_VERSION: u32 = 1;
pub(crate) const MANAGED_WSL_DISTRIBUTION: &str = "GameEngine-LocalAI";
const MANAGED_WSL_BASE_DISTRIBUTION: &str = "Ubuntu-22.04";
const MANAGED_WSL_EXPECTED_VERSION_ID: &str = "22.04";
const MANAGED_RUNTIME_COMPATIBILITY_VERSION: &str = "llama-server-openai-v1";
const RELEASE_METADATA_URL: &str =
    "https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/b10336";
const WINDOWS_CUDA_RUNTIME_ASSET: &str = "llama-b10336-bin-win-cuda-12.4-x64.zip";
const WINDOWS_CUDA_SUPPORT_ASSET: &str = "cudart-llama-bin-win-cuda-12.4-x64.zip";
const WINDOWS_CUDA_MANIFEST: &str = "llama-b10336-win-cuda-12.4-manifest.txt";
const WSL_CUDA_MANIFEST: &str = "llama-b10336-wsl-cuda-12.4-source-manifest.txt";
const WSL_CUDA_REPOSITORY_URL: &str =
    "https://developer.download.nvidia.com/compute/cuda/repos/wsl-ubuntu/x86_64";
const WSL_CUDA_COMPILER_PACKAGE: &str = "cuda-compiler-12-4";
const WSL_CUDA_LIBRARIES_DEV_PACKAGE: &str = "cuda-libraries-dev-12-4";
const WSL_CUDA_KEYRING_ASSET: &str = "cuda-keyring_1.1-1_all.deb";
const LLAMA_CPP_REPOSITORY_URL: &str = "https://github.com/ggml-org/llama.cpp.git";
const STATE_SCHEMA_VERSION: u32 = 2;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const WSL_SERVER_LOG_ROOT: &str = "/var/lib/gameengine/local-ai/logs";
// Keep the WSL launch shell alive briefly so WSL does not reap the detached
// llama-server before its dynamic loader and redirected log are established.
const WSL_SERVER_LAUNCH_GRACE_SECONDS: u64 = 1;
/// Leave one GiB of device memory for KV cache, compute buffers, and the rest of the GPU workload.
///
/// The pinned llama.cpp revision supports automatic layer selection plus `--fit`; keeping
/// `--n-gpu-layers` unset lets that fitter account for the exact model and currently available VRAM.
const MANAGED_GPU_FIT_TARGET_MIB: u32 = 1024;
/// Serve one request at a time so the context window below is not multiplied by idle slots.
///
/// llama-server allocates `--ctx-size` per slot, so the KV cache it reserves is the context window
/// times the slot count. Managed local inference is issued sequentially, so extra slots only
/// multiply KV residency and scatter prompt-prefix reuse across cold slots.
const MANAGED_SERVER_PARALLEL_SLOTS: u32 = 1;
/// Smallest context window retained by the Managed Local physical launch policy.
///
/// This is a backend floor for existing Managed Local consumers, not an Agent Harness guarantee.
/// Consumers such as Goose ACP may declare a larger admission requirement without increasing the
/// physical context beyond the model and device resource plan.
const MANAGED_CONTEXT_FLOOR_TOKENS: u32 = 8_192;
/// Largest context window managed inference requests, whatever the model declares.
///
/// Long-context models declare windows far beyond what an agent turn uses, and llama-server
/// reserves KV cache for the whole window at load time, so following a declared 128K window
/// would spend device memory on context the harness never fills.
const MANAGED_CONTEXT_CEILING_TOKENS: u32 = 32_768;
/// Window used when the registered GGUF declares no usable shape.
///
/// This is the value GameEngine measured as sufficient for managed-agent prompts before model
/// metadata was read. It applies only when the file itself says nothing, never in preference to
/// what a file declares.
const MANAGED_CONTEXT_UNMEASURED_TOKENS: u32 = 12_288;
/// Share of measured device memory the KV cache may occupy.
///
/// The remainder stays available to model weights, compute buffers, and the Editor renderer that
/// shares the device.
const MANAGED_KV_CACHE_MEMORY_PERCENT: u64 = 25;
/// Context windows are aligned down to this multiple so a launch stays reproducible.
const MANAGED_CONTEXT_ALIGNMENT_TOKENS: u32 = 512;

/// Minimum physical context required by one Managed Local consumer.
///
/// The requirement is an admission predicate only. It never raises the physical context selected
/// from GGUF metadata and device-memory planning, so a consumer that needs more context fails
/// closed rather than forcing an unsupported or unsafe `--ctx-size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedContextRequirement {
    minimum_tokens: u32,
}

impl ManagedContextRequirement {
    pub(crate) const fn new(minimum_tokens: u32) -> Self {
        Self { minimum_tokens }
    }

    pub(crate) const fn minimum_tokens(self) -> u32 {
        self.minimum_tokens
    }

    pub(crate) const fn admits(self, physical_context_tokens: u32) -> bool {
        physical_context_tokens >= self.minimum_tokens
    }
}
/// Allow a released llama-server to finish exiting before another one measures device memory.
///
/// A forced kill closes the loopback socket long before the process frees its device allocations,
/// so treating an unreachable endpoint as a completed release lets the next launch run `--fit`
/// while the previous weights and KV cache are still resident. The fitter then moves blocks onto
/// the CPU for the whole session, which costs far more than waiting out the teardown.
const MANAGED_SERVER_RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
/// Allow a force-killed llama-server to exit after the graceful release timeout expires.
///
/// A WSL2 llama-server can hang mid-shutdown while releasing the GPU paravirtualization
/// device, so it never reacts to the plain `kill` sent at the start of release. This second,
/// shorter wait follows a `kill -9` sent once the graceful timeout is exhausted.
const MANAGED_SERVER_FORCE_KILL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedExecutionEnvironment {
    #[default]
    WindowsNative,
    Wsl2Linux,
}

impl ManagedExecutionEnvironment {
    pub(crate) const ALL: [Self; 2] = [Self::WindowsNative, Self::Wsl2Linux];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::WindowsNative => "Windows native",
            Self::Wsl2Linux => "WSL2 Linux",
        }
    }

    pub(crate) const fn benchmark_id(self) -> &'static str {
        match self {
            Self::WindowsNative => "windows_native",
            Self::Wsl2Linux => "wsl2_linux",
        }
    }

    fn storage_key(self) -> &'static str {
        self.benchmark_id()
    }
}

// ADR 0155 defines the complete diagnostic vocabulary even when a specific
// first-release machine never exercises every platform/resource failure layer.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedDiagnosticLayer {
    OperatingSystemPrerequisite,
    ElevationOrRestart,
    WslDistributionProvisioning,
    GpuOrBackendCapability,
    RuntimeArtifactIntegrity,
    ModelTransferOrIntegrity,
    ManagedProcessStartup,
    ModelResource,
    InferenceProtocol,
}

impl ManagedDiagnosticLayer {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::OperatingSystemPrerequisite => "operating-system prerequisite",
            Self::ElevationOrRestart => "elevation or restart",
            Self::WslDistributionProvisioning => "WSL distribution provisioning",
            Self::GpuOrBackendCapability => "GPU/backend capability",
            Self::RuntimeArtifactIntegrity => "runtime artifact integrity/update",
            Self::ModelTransferOrIntegrity => "model transfer/content verification",
            Self::ManagedProcessStartup => "managed process/server startup",
            Self::ModelResource => "model load/OOM/resource",
            Self::InferenceProtocol => "inference protocol/model turn",
        }
    }
}

#[derive(Debug)]
pub(crate) struct ManagedLocalRuntimeError {
    layer: ManagedDiagnosticLayer,
    message: String,
}

impl ManagedLocalRuntimeError {
    fn new(layer: ManagedDiagnosticLayer, message: impl Into<String>) -> Self {
        Self {
            layer,
            message: message.into(),
        }
    }

    pub(crate) const fn layer(&self) -> ManagedDiagnosticLayer {
        self.layer
    }
}

impl fmt::Display for ManagedLocalRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.layer.label(), self.message)
    }
}

impl std::error::Error for ManagedLocalRuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedRuntimeInstallation {
    schema_version: u32,
    pub(crate) runtime_family: String,
    pub(crate) runtime_tag: String,
    pub(crate) runtime_revision: String,
    pub(crate) environment: ManagedExecutionEnvironment,
    pub(crate) artifact_name: String,
    pub(crate) artifact_sha256: String,
    pub(crate) installed_unix_ms: u64,
    pub(crate) compatibility_version: String,
    pub(crate) server_path: String,
    pub(crate) retained_artifact_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedGooseInstallation {
    schema_version: u32,
    pub(crate) version: String,
    pub(crate) asset_name: String,
    pub(crate) asset_sha256: String,
    pub(crate) executable_sha256: String,
    pub(crate) installed_unix_ms: u64,
    pub(crate) executable_path: PathBuf,
    pub(crate) retained_artifact_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedGooseOverride {
    schema_version: u32,
    executable_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedGooseSetupStatus {
    Ready,
    NotInstalled,
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedModelRegistration {
    pub(crate) model_id: String,
    pub(crate) display_name: String,
    pub(crate) content_sha256: String,
    pub(crate) source_path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) modified_unix_ms: Option<u64>,
    /// Canonical llama.cpp file-type label when GGUF metadata safely provides one.
    ///
    /// Older registries may contain a filename-derived value here. Benchmark
    /// identity never treats this legacy field as authoritative.
    pub(crate) quantization: Option<String>,
    /// Stable descriptor measured from GGUF metadata and every tensor descriptor.
    #[serde(default)]
    pub(crate) representation: Option<String>,
    /// Optional multimodal projector paired with this model.
    ///
    /// A projector is what makes a local model able to read an image at all.
    /// It is registered per model rather than inferred from a model name, so a
    /// new model family works the moment its projector is registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) projector: Option<ManagedProjectorRegistration>,
    /// Launch-relevant model shape measured from the same GGUF metadata block.
    ///
    /// Registrations written before GameEngine measured model shape carry the
    /// default, which reports every value as unmeasured rather than guessing.
    #[serde(default)]
    pub(crate) capability: GgufModelCapability,
    pub(crate) source: Option<String>,
    pub(crate) license: Option<String>,
}

/// A registered multimodal projector file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedProjectorRegistration {
    /// Projector file on the Windows filesystem.
    pub(crate) source_path: PathBuf,
    /// Content digest of the projector file.
    pub(crate) content_sha256: String,
    /// Size in bytes, used for the WSL2 duplicate-storage decision.
    pub(crate) size_bytes: u64,
    /// Modification time recorded at registration.
    pub(crate) modified_unix_ms: Option<u64>,
}

impl ManagedModelRegistration {
    pub(crate) fn exact_representation(&self) -> Option<&str> {
        self.representation
            .as_deref()
            .filter(|descriptor| gguf::is_representation_descriptor(descriptor))
    }

    pub(crate) fn has_exact_representation_identity(&self) -> bool {
        is_sha256_hex(&self.content_sha256)
            && self.size_bytes > 0
            && self.exact_representation().is_some()
    }
}

/// Restores measured representations for models registered by an older build.
///
/// Registration has always recorded the digest and byte size, but the GGUF
/// representation descriptor was added later, so registries written before that
/// hold records that can never become campaign candidates. Returns whether any
/// record changed, so a reader only rewrites the registry when a record was
/// actually restored.
fn remeasure_unmeasured_representations(registry: &mut ManagedModelRegistry) -> bool {
    let mut remeasured = false;
    for model in &mut registry.models {
        if model.exact_representation().is_some()
            && model.capability != GgufModelCapability::default()
        {
            continue;
        }
        let Some(representation) = remeasure_registered_representation(model) else {
            continue;
        };
        // Replace the legacy label from the same measurement that produced the
        // descriptor, so both fields keep describing one inspection of one file.
        model.quantization = representation.canonical_quantization;
        model.representation = Some(representation.descriptor);
        model.capability = representation.capability;
        remeasured = true;
    }
    remeasured
}

/// Re-reads the GGUF header of an already registered model file.
///
/// ADR 0155 keeps presentation paths free of whole-file hashing, so this accepts
/// a file only while its byte size and modification time still match the record.
/// A file that passes that check is the one whose digest was recorded, under the
/// same assumption [`ManagedLocalRuntime::verify_registered_model`] already makes
/// before inference. Anything else stays unmeasured and keeps asking the operator
/// to register the exact GGUF again.
fn remeasure_registered_representation(
    model: &ManagedModelRegistration,
) -> Option<gguf::GgufRepresentation> {
    let recorded_modified = model.modified_unix_ms?;
    if !is_sha256_hex(&model.content_sha256) || model.size_bytes == 0 {
        return None;
    }
    let metadata = fs::metadata(&model.source_path).ok()?;
    if metadata.len() != model.size_bytes {
        return None;
    }
    if metadata.modified().ok().and_then(system_time_unix_ms)? != recorded_modified {
        return None;
    }
    let representation = gguf::inspect_representation(&model.source_path).ok()?;
    gguf::is_representation_descriptor(&representation.descriptor).then_some(representation)
}

/// Whether a resolved managed configuration also runs content verification.
///
/// ADR 0155 requires integrity verification before inference, not for every
/// rendered label. Verifying a WSL2 model means hashing the whole GGUF, so
/// presentation paths MUST pass [`ManagedIntegrityCheck::Skipped`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedIntegrityCheck {
    /// Resolve paths and identity only. Safe to call for presentation.
    Skipped,
    /// Resolve and verify the retained runtime artifact and model content.
    Enforced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedLocalModelConfig {
    pub(crate) state_root: PathBuf,
    pub(crate) environment: ManagedExecutionEnvironment,
    pub(crate) model_id: String,
    pub(crate) model_content_sha256: String,
    pub(crate) model_path: PathBuf,
    pub(crate) model_size_bytes: u64,
    pub(crate) quantization: Option<String>,
    pub(crate) model_representation: Option<String>,
    /// Model shape measured from the registered GGUF, used for launch policy.
    pub(crate) capability: GgufModelCapability,
    /// Prepared multimodal projector path, when one is registered.
    pub(crate) projector_path: Option<PathBuf>,
    pub(crate) runtime_tag: String,
    pub(crate) runtime_revision: String,
    pub(crate) runtime_artifact_sha256: String,
    pub(crate) runtime_compatibility_version: String,
}

impl ManagedRuntimeInstallation {
    pub(crate) fn benchmark_runtime_identity(&self) -> String {
        format!(
            "llama.cpp:{}@{};env={};artifact_sha256={};compat={}",
            self.runtime_tag,
            self.runtime_revision,
            self.environment.benchmark_id(),
            self.artifact_sha256,
            self.compatibility_version,
        )
    }
}

impl ManagedLocalModelConfig {
    pub(crate) fn benchmark_runtime_identity(&self) -> String {
        format!(
            "llama.cpp:{}@{};env={};artifact_sha256={};compat={}",
            self.runtime_tag,
            self.runtime_revision,
            self.environment.benchmark_id(),
            self.runtime_artifact_sha256,
            self.runtime_compatibility_version,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedEndpoint {
    pub(crate) url: String,
    pub(crate) process_id: u32,
    pub(crate) reused_process: bool,
}

/// Read-only identity exposed to external local-agent adapters while a managed
/// llama-server endpoint is leased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedLocalEndpointIdentity {
    pub(crate) endpoint_url: String,
    pub(crate) model_id: String,
    pub(crate) model_content_sha256: String,
    pub(crate) model_representation: Option<String>,
    pub(crate) runtime_identity: String,
    pub(crate) execution_environment: ManagedExecutionEnvironment,
}

/// Process-local lifecycle lease for one exact managed endpoint.
///
/// The lease never owns or serializes Managed Local state. It prevents another
/// runtime request or setup operation from replacing/stopping this exact
/// llama-server while an external adapter is using it. Dropping the lease only
/// releases that protection; ManagedLocalRuntime retains process ownership.
#[derive(Debug)]
pub(crate) struct ManagedLocalEndpointLease {
    key: String,
    identity: ManagedLocalEndpointIdentity,
    released: bool,
}

#[derive(Debug, Clone)]
struct ManagedEndpointLeaseState {
    holders: usize,
    state_root: PathBuf,
    environment: ManagedExecutionEnvironment,
}

static MANAGED_ENDPOINT_LEASES: OnceLock<Mutex<BTreeMap<String, ManagedEndpointLeaseState>>> =
    OnceLock::new();

impl ManagedLocalEndpointLease {
    pub(crate) fn identity(&self) -> &ManagedLocalEndpointIdentity {
        &self.identity
    }

    pub(crate) fn release(mut self) -> Result<(), ManagedLocalRuntimeError> {
        self.release_inner()?;
        self.released = true;
        Ok(())
    }

    fn release_inner(&self) -> Result<(), ManagedLocalRuntimeError> {
        if self.released {
            return Ok(());
        }
        release_managed_endpoint_lease(&self.key)
    }
}

impl Drop for ManagedLocalEndpointLease {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.release_inner();
            self.released = true;
        }
    }
}

fn managed_endpoint_lease_key(config: &ManagedLocalModelConfig) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        config.state_root.display(),
        config.environment.storage_key(),
        config.model_content_sha256,
        config.runtime_revision,
        config.runtime_artifact_sha256
    )
}

fn managed_endpoint_lease_registry() -> &'static Mutex<BTreeMap<String, ManagedEndpointLeaseState>>
{
    MANAGED_ENDPOINT_LEASES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock_managed_endpoint_leases() -> Result<
    std::sync::MutexGuard<'static, BTreeMap<String, ManagedEndpointLeaseState>>,
    ManagedLocalRuntimeError,
> {
    managed_endpoint_lease_registry().lock().map_err(|_| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ManagedProcessStartup,
            "managed Local AI endpoint lease registry was poisoned",
        )
    })
}

fn release_managed_endpoint_lease(key: &str) -> Result<(), ManagedLocalRuntimeError> {
    let mut leases = lock_managed_endpoint_leases()?;
    let Some(state) = leases.get_mut(key) else {
        return Ok(());
    };
    state.holders = state.holders.saturating_sub(1);
    if state.holders == 0 {
        leases.remove(key);
    }
    Ok(())
}

fn managed_conflicting_active_lease(
    config: &ManagedLocalModelConfig,
) -> Result<bool, ManagedLocalRuntimeError> {
    let desired_key = managed_endpoint_lease_key(config);
    let leases = lock_managed_endpoint_leases()?;
    Ok(leases.iter().any(|(key, state)| {
        key != &desired_key && state.holders > 0 && state.state_root == config.state_root
    }))
}

fn managed_endpoint_has_active_lease(
    config: &ManagedLocalModelConfig,
) -> Result<bool, ManagedLocalRuntimeError> {
    let leases = lock_managed_endpoint_leases()?;
    Ok(leases
        .values()
        .any(|state| state.holders > 0 && state.state_root == config.state_root))
}

fn managed_environment_has_active_lease(
    state_root: &Path,
    environment: ManagedExecutionEnvironment,
) -> Result<bool, ManagedLocalRuntimeError> {
    let leases = lock_managed_endpoint_leases()?;
    Ok(leases.values().any(|state| {
        state.holders > 0 && state.state_root == state_root && state.environment == environment
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedSetupStatus {
    Ready,
    RuntimeNotInstalled,
    WslDistributionMissing,
    RestartRequired,
    OperatingSystemPrerequisiteUnavailable(String),
}

// ADR 0155 freezes exact Download & Run approval semantics for ADR 0156 campaign
// integration; the standalone setup UI intentionally does not auto-download weights.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAcquisitionCandidate {
    pub(crate) candidate_id: String,
    pub(crate) source: String,
    pub(crate) representation: String,
    pub(crate) license: Option<String>,
    pub(crate) expected_sha256: String,
    pub(crate) transfer_bytes: u64,
    pub(crate) storage_bytes: u64,
}

// Retained for the ADR 0156 frozen-campaign handoff; normal Local AI setup has no
// implicit model-acquisition path.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAcquisitionPlan {
    pub(crate) plan_id: String,
    pub(crate) candidates: Vec<ManagedAcquisitionCandidate>,
}

// Produced by ADR 0156 campaign review; retained here so transfer/storage approval
// cannot be redefined by the later campaign UI.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAcquisitionReview {
    pub(crate) candidate_count: usize,
    pub(crate) total_transfer_bytes: u64,
    pub(crate) total_storage_bytes: u64,
}

// Approval tokens are consumed by ADR 0156 campaign execution, not by ordinary
// single-model Local AI setup.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedAcquisitionApproval {
    plan_id: String,
    candidate_ids: BTreeSet<String>,
}

// ADR 0156 consumes these exact frozen-campaign review/approval helpers.
#[allow(dead_code)]
impl ManagedAcquisitionPlan {
    pub(crate) fn review(&self) -> ManagedAcquisitionReview {
        ManagedAcquisitionReview {
            candidate_count: self.candidates.len(),
            total_transfer_bytes: self.candidates.iter().fold(0_u64, |total, candidate| {
                total.saturating_add(candidate.transfer_bytes)
            }),
            total_storage_bytes: self.candidates.iter().fold(0_u64, |total, candidate| {
                total.saturating_add(candidate.storage_bytes)
            }),
        }
    }

    pub(crate) fn approve_exact(&self) -> ManagedAcquisitionApproval {
        ManagedAcquisitionApproval {
            plan_id: self.plan_id.clone(),
            candidate_ids: self
                .candidates
                .iter()
                .map(|candidate| candidate.candidate_id.clone())
                .collect(),
        }
    }
}

// ADR 0156 consumes this exact-approval check when executing a frozen campaign.
#[allow(dead_code)]
impl ManagedAcquisitionApproval {
    pub(crate) fn authorizes(
        &self,
        plan_id: &str,
        candidate_id: &str,
    ) -> Result<(), ManagedLocalRuntimeError> {
        if self.plan_id != plan_id || !self.candidate_ids.contains(candidate_id) {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "Download & Run approval does not cover this model representation",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedLocalRuntime {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) enum ManagedSetupOperation {
    InstallRuntime(ManagedExecutionEnvironment),
    InstallGoose,
    ProvisionWsl,
    RegisterModel(PathBuf),
    /// Pairs a multimodal projector with one registered model.
    RegisterProjector {
        model_id: String,
        path: PathBuf,
    },
    /// Drops the projector registration, returning the model to text only.
    RemoveProjector {
        model_id: String,
    },
    PrepareModel {
        model_id: String,
        environment: ManagedExecutionEnvironment,
        duplicate_storage_approved: bool,
    },
    RemoveEnvironment(ManagedExecutionEnvironment),
}

#[derive(Debug, Clone)]
pub(crate) enum ManagedSetupResult {
    RuntimeInstalled(ManagedRuntimeInstallation),
    GooseInstalled(ManagedGooseInstallation),
    WslProvisioned,
    ModelRegistered(ManagedModelRegistration),
    ModelPrepared(PathBuf),
    EnvironmentRemoved(ManagedExecutionEnvironment),
}

pub(crate) struct ManagedSetupTask {
    result: Receiver<Result<ManagedSetupResult, ManagedLocalRuntimeError>>,
}

impl ManagedSetupTask {
    pub(crate) fn spawn(
        manager: ManagedLocalRuntime,
        operation: ManagedSetupOperation,
    ) -> Result<Self, ManagedLocalRuntimeError> {
        let (sender, result) = mpsc::channel();
        thread::Builder::new()
            .name("managed-local-ai-setup".to_owned())
            .spawn(move || {
                let outcome = match operation {
                    ManagedSetupOperation::InstallRuntime(environment) => manager
                        .install_pinned_runtime(environment)
                        .map(ManagedSetupResult::RuntimeInstalled),
                    ManagedSetupOperation::InstallGoose => manager
                        .install_pinned_goose()
                        .map(ManagedSetupResult::GooseInstalled)
                        .map_err(|error| {
                            ManagedLocalRuntimeError::new(
                                error.layer(),
                                format!("Goose setup failed: {error}"),
                            )
                        }),
                    ManagedSetupOperation::ProvisionWsl => manager
                        .provision_managed_wsl_distribution()
                        .map(|()| ManagedSetupResult::WslProvisioned),
                    ManagedSetupOperation::RegisterModel(path) => manager
                        .register_existing_gguf(&path, None)
                        .map(ManagedSetupResult::ModelRegistered),
                    ManagedSetupOperation::RegisterProjector { model_id, path } => manager
                        .register_projector(&model_id, &path)
                        .map(ManagedSetupResult::ModelRegistered),
                    ManagedSetupOperation::RemoveProjector { model_id } => manager
                        .remove_projector(&model_id)
                        .map(ManagedSetupResult::ModelRegistered),
                    ManagedSetupOperation::PrepareModel {
                        model_id,
                        environment,
                        duplicate_storage_approved,
                    } => manager
                        .prepare_model_for_environment(
                            &model_id,
                            environment,
                            duplicate_storage_approved,
                        )
                        .and_then(|path| {
                            // A projector is part of the same preparation: an
                            // image-capable model that reached WSL without it
                            // would start and then fail on the first image.
                            manager
                                .prepare_projector_for_environment(
                                    &model_id,
                                    environment,
                                    duplicate_storage_approved,
                                )
                                .map(|_| path)
                        })
                        .map(ManagedSetupResult::ModelPrepared),
                    ManagedSetupOperation::RemoveEnvironment(environment) => manager
                        .remove_environment(environment)
                        .map(|()| ManagedSetupResult::EnvironmentRemoved(environment)),
                };
                let _ = sender.send(outcome);
            })
            .map_err(|error| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                    format!("could not start managed Local AI setup worker: {error}"),
                )
            })?;
        Ok(Self { result })
    }

    pub(crate) fn poll(&self) -> Option<Result<ManagedSetupResult, ManagedLocalRuntimeError>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                "managed Local AI setup worker disconnected unexpectedly",
            ))),
        }
    }
}

/// Managed-environment facts whose collection requires Windows or WSL2 process probes.
///
/// Every WSL2 answer costs at least one `wsl.exe` process, so the Editor collects
/// these on a worker thread and renders the last completed snapshot. Probing from
/// the frame loop stalls the UI thread for as long as the dedicated distribution
/// is busy, which spans the whole duration of a managed model transfer.
#[derive(Debug, Clone)]
pub(crate) struct ManagedEnvironmentProbe {
    pub(crate) environment: ManagedExecutionEnvironment,
    pub(crate) model_id: String,
    pub(crate) setup_status: ManagedSetupStatus,
    pub(crate) additional_storage_bytes: Result<u64, String>,
    pub(crate) described_config: Result<ManagedLocalModelConfig, String>,
}

pub(crate) struct ManagedEnvironmentProbeTask {
    result: Receiver<ManagedEnvironmentProbe>,
}

impl ManagedEnvironmentProbeTask {
    pub(crate) fn spawn(
        manager: ManagedLocalRuntime,
        environment: ManagedExecutionEnvironment,
        model_id: String,
    ) -> Result<Self, ManagedLocalRuntimeError> {
        let (sender, result) = mpsc::channel();
        thread::Builder::new()
            .name("managed-local-ai-probe".to_owned())
            .spawn(move || {
                let _ = sender.send(manager.probe_environment(environment, model_id));
            })
            .map_err(|error| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                    format!("could not start managed Local AI probe worker: {error}"),
                )
            })?;
        Ok(Self { result })
    }

    /// Returns `None` while the probe is still running, `Some(None)` when the
    /// worker ended without a snapshot so the caller retires the task instead of
    /// waiting on a closed channel forever.
    pub(crate) fn poll(&self) -> Option<Option<ManagedEnvironmentProbe>> {
        match self.result.try_recv() {
            Ok(probe) => Some(Some(probe)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

impl ManagedLocalRuntime {
    pub(crate) fn open(root: PathBuf) -> Result<Self, ManagedLocalRuntimeError> {
        fs::create_dir_all(&root).map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!("could not create managed Local AI state: {error}"),
            )
        })?;
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the lightweight setup state shown by AI Studio.
    ///
    /// Full executable SHA-256 and ACP capability verification happens in the shared resolver
    /// before launch; the frame loop never hashes the Goose binary repeatedly.
    pub(crate) fn managed_goose_setup_status(&self) -> ManagedGooseSetupStatus {
        match self.managed_goose_installation() {
            Ok(None) => ManagedGooseSetupStatus::NotInstalled,
            Ok(Some(_)) if self.managed_goose_candidate_available() => {
                ManagedGooseSetupStatus::Ready
            }
            Ok(Some(_)) => ManagedGooseSetupStatus::Invalid(
                "the managed Goose installation is stale or incomplete".to_owned(),
            ),
            Err(error) => ManagedGooseSetupStatus::Invalid(error.to_string()),
        }
    }

    /// Returns whether the managed installation metadata names a present pinned executable.
    ///
    /// This lightweight check is safe for the composer frame loop. Full SHA-256 verification
    /// remains mandatory in [`ManagedLocalRuntime::managed_goose_executable`] before launch.
    pub(crate) fn managed_goose_candidate_available(&self) -> bool {
        matches!(
            self.managed_goose_installation(),
            Ok(Some(installation))
                if installation.schema_version == GOOSE_RUNTIME_STATE_SCHEMA_VERSION
                    && installation.version == PINNED_GOOSE_VERSION
                    && installation.asset_name == PINNED_GOOSE_WINDOWS_ASSET
                    && installation.asset_sha256 == PINNED_GOOSE_WINDOWS_SHA256
                    && installation.executable_path.is_file()
        )
    }

    /// Returns the verified GameEngine-managed Goose executable, when installed.
    pub(crate) fn managed_goose_executable(
        &self,
    ) -> Result<Option<PathBuf>, ManagedLocalRuntimeError> {
        let Some(installation) = self.managed_goose_installation()? else {
            return Ok(None);
        };
        if installation.schema_version != GOOSE_RUNTIME_STATE_SCHEMA_VERSION
            || installation.version != PINNED_GOOSE_VERSION
            || installation.asset_name != PINNED_GOOSE_WINDOWS_ASSET
            || installation.asset_sha256 != PINNED_GOOSE_WINDOWS_SHA256
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "managed Goose installation does not match the pinned GameEngine runtime",
            ));
        }
        if !installation.executable_path.is_file() {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!(
                    "managed Goose executable is missing: {}",
                    installation.executable_path.display()
                ),
            ));
        }
        if !installation.retained_artifact_path.is_file() {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!(
                    "managed Goose retained artifact is missing: {}",
                    installation.retained_artifact_path.display()
                ),
            ));
        }
        verify_file_sha256(
            &installation.executable_path,
            &installation.executable_sha256,
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
        )?;
        Ok(Some(installation.executable_path))
    }

    /// Returns the persisted machine-local Goose executable override.
    pub(crate) fn goose_executable_override(
        &self,
    ) -> Result<Option<PathBuf>, ManagedLocalRuntimeError> {
        let override_state: Option<ManagedGooseOverride> =
            read_optional_json(&self.goose_override_path()).map_err(runtime_io)?;
        let Some(override_state) = override_state else {
            return Ok(None);
        };
        if override_state.schema_version != GOOSE_RUNTIME_STATE_SCHEMA_VERSION {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "machine-local Goose override uses an unsupported schema version",
            ));
        }
        Ok(Some(override_state.executable_path))
    }

    /// Sets or clears the machine-local Goose executable override.
    pub(crate) fn set_goose_executable_override(
        &self,
        executable: Option<PathBuf>,
    ) -> Result<(), ManagedLocalRuntimeError> {
        let path = self.goose_override_path();
        let Some(executable) = executable else {
            return match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(runtime_io(error)),
            };
        };
        if !executable.is_absolute() || !executable.is_file() {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "machine-local Goose override must name an existing absolute executable path",
            ));
        }
        write_json(
            &path,
            &ManagedGooseOverride {
                schema_version: GOOSE_RUNTIME_STATE_SCHEMA_VERSION,
                executable_path: executable,
            },
        )
        .map_err(runtime_io)
    }

    /// Downloads, verifies, stages, and activates the pinned Goose ACP runtime.
    pub(crate) fn install_pinned_goose(
        &self,
    ) -> Result<ManagedGooseInstallation, ManagedLocalRuntimeError> {
        if !cfg!(target_os = "windows") {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                "managed Goose provisioning is currently available only from the Windows-hosted Editor",
            ));
        }
        if let Some(installation) = self.managed_goose_installation()?
            && installation.schema_version == GOOSE_RUNTIME_STATE_SCHEMA_VERSION
            && installation.version == PINNED_GOOSE_VERSION
            && installation.asset_sha256 == PINNED_GOOSE_WINDOWS_SHA256
            && self.managed_goose_executable()?.is_some()
        {
            return Ok(installation);
        }

        let runtime_root = self.goose_runtime_root();
        let final_root = runtime_root.join(format!("v{PINNED_GOOSE_VERSION}"));
        let staging_root = runtime_root.join(format!("v{PINNED_GOOSE_VERSION}.staging"));
        let rollback_root = runtime_root.join(format!("v{PINNED_GOOSE_VERSION}.rollback"));
        let download = self.root.join("downloads").join(PINNED_GOOSE_WINDOWS_ASSET);
        if download.is_file()
            && verify_file_sha256(
                &download,
                PINNED_GOOSE_WINDOWS_SHA256,
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            )
            .is_err()
        {
            fs::remove_file(&download).map_err(runtime_io)?;
        }
        if !download.is_file() {
            download_https_file(PINNED_GOOSE_WINDOWS_URL, &download)?;
        }
        verify_file_sha256(
            &download,
            PINNED_GOOSE_WINDOWS_SHA256,
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
        )?;

        if staging_root.exists() {
            fs::remove_dir_all(&staging_root).map_err(runtime_io)?;
        }
        fs::create_dir_all(&staging_root).map_err(runtime_io)?;
        expand_zip(&download, &staging_root)?;
        let staged_executable = staging_root.join(PINNED_GOOSE_EXECUTABLE_RELATIVE_PATH);
        if !staged_executable.is_file() {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "verified Goose archive does not contain goose.exe",
            ));
        }
        let executable_sha256 = sha256_via_platform(&staged_executable)?;
        let artifact_dir = staging_root.join("artifacts");
        fs::create_dir_all(&artifact_dir).map_err(runtime_io)?;
        let staged_artifact = artifact_dir.join(PINNED_GOOSE_WINDOWS_ASSET);
        fs::copy(&download, &staged_artifact).map_err(runtime_io)?;
        verify_file_sha256(
            &staged_artifact,
            PINNED_GOOSE_WINDOWS_SHA256,
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
        )?;

        let installation = ManagedGooseInstallation {
            schema_version: GOOSE_RUNTIME_STATE_SCHEMA_VERSION,
            version: PINNED_GOOSE_VERSION.to_owned(),
            asset_name: PINNED_GOOSE_WINDOWS_ASSET.to_owned(),
            asset_sha256: PINNED_GOOSE_WINDOWS_SHA256.to_owned(),
            executable_sha256,
            installed_unix_ms: now_unix_ms(),
            executable_path: final_root.join(PINNED_GOOSE_EXECUTABLE_RELATIVE_PATH),
            retained_artifact_path: final_root
                .join("artifacts")
                .join(PINNED_GOOSE_WINDOWS_ASSET),
        };
        write_json(&staging_root.join("installation.json"), &installation).map_err(runtime_io)?;

        if rollback_root.exists() {
            fs::remove_dir_all(&rollback_root).map_err(runtime_io)?;
        }
        if final_root.exists() {
            fs::rename(&final_root, &rollback_root).map_err(runtime_io)?;
        }
        if let Err(error) = fs::rename(&staging_root, &final_root) {
            if rollback_root.exists() {
                let _ = fs::rename(&rollback_root, &final_root);
            }
            return Err(runtime_io(error));
        }
        if let Err(error) = write_json(&self.goose_active_path(), &installation) {
            let _ = fs::remove_dir_all(&final_root);
            if rollback_root.exists() {
                let _ = fs::rename(&rollback_root, &final_root);
            }
            return Err(runtime_io(error));
        }
        if rollback_root.exists() {
            fs::remove_dir_all(&rollback_root).map_err(runtime_io)?;
        }
        let _verified_executable = self.managed_goose_executable()?;
        Ok(installation)
    }

    fn managed_goose_installation(
        &self,
    ) -> Result<Option<ManagedGooseInstallation>, ManagedLocalRuntimeError> {
        read_optional_json(&self.goose_active_path()).map_err(runtime_io)
    }

    fn goose_runtime_root(&self) -> PathBuf {
        self.root.join("agent-runtime").join("goose")
    }

    fn goose_active_path(&self) -> PathBuf {
        self.goose_runtime_root().join("active.json")
    }

    fn goose_override_path(&self) -> PathBuf {
        self.goose_runtime_root().join("override.json")
    }

    pub(crate) fn setup_status(
        &self,
        environment: ManagedExecutionEnvironment,
    ) -> ManagedSetupStatus {
        self.setup_status_with_continuation(environment, || {
            cfg!(target_os = "windows")
                && matches!(
                    wsl_status(),
                    Ok(WslStatus::Available {
                        managed_distribution: true
                    })
                )
        })
    }

    /// Setup status with the restart continuation check supplied by the caller,
    /// so the marker lifecycle can be exercised without depending on the host
    /// WSL state.
    fn setup_status_with_continuation(
        &self,
        environment: ManagedExecutionEnvironment,
        continuation_ready: impl FnOnce() -> bool,
    ) -> ManagedSetupStatus {
        if self.restart_marker_path().is_file() {
            if continuation_ready() {
                let _ = self.clear_restart_required();
            } else {
                return ManagedSetupStatus::RestartRequired;
            }
        }
        if !cfg!(target_os = "windows") {
            return ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(
                "the ADR 0155 first release is Windows-hosted".to_owned(),
            );
        }
        if environment == ManagedExecutionEnvironment::Wsl2Linux {
            match wsl_status() {
                Ok(WslStatus::Unavailable(message)) => {
                    return ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(message);
                }
                Ok(WslStatus::Available {
                    managed_distribution: false,
                }) => return ManagedSetupStatus::WslDistributionMissing,
                Ok(WslStatus::Available {
                    managed_distribution: true,
                }) => {}
                Err(error) => {
                    return ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(
                        error.to_string(),
                    );
                }
            }
        }
        match self.active_installation(environment) {
            Ok(Some(installation))
                if installation.schema_version == STATE_SCHEMA_VERSION
                    && installation.runtime_tag == PINNED_LLAMA_CPP_TAG
                    && installation.runtime_revision == PINNED_LLAMA_CPP_REVISION
                    && installation.artifact_name
                        == match environment {
                            ManagedExecutionEnvironment::WindowsNative => WINDOWS_CUDA_MANIFEST,
                            ManagedExecutionEnvironment::Wsl2Linux => WSL_CUDA_MANIFEST,
                        } =>
            {
                ManagedSetupStatus::Ready
            }
            Ok(Some(_)) | Ok(None) | Err(_) => ManagedSetupStatus::RuntimeNotInstalled,
        }
    }

    pub(crate) fn mark_restart_required(&self) -> Result<(), ManagedLocalRuntimeError> {
        write_atomic(&self.restart_marker_path(), b"restart_required\n").map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ElevationOrRestart,
                format!("could not persist setup continuation marker: {error}"),
            )
        })
    }

    pub(crate) fn clear_restart_required(&self) -> Result<(), ManagedLocalRuntimeError> {
        match fs::remove_file(self.restart_marker_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ElevationOrRestart,
                format!("could not clear setup continuation marker: {error}"),
            )),
        }
    }

    pub(crate) fn provision_managed_wsl_distribution(
        &self,
    ) -> Result<(), ManagedLocalRuntimeError> {
        if !cfg!(target_os = "windows") {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                "WSL2 provisioning is available only from the Windows-hosted Editor",
            ));
        }
        match wsl_status()? {
            WslStatus::Unavailable(message) => {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ElevationOrRestart,
                    format!(
                        "WSL2 is not ready. GameEngine will not bypass UAC or reboot automatically: {message}"
                    ),
                ));
            }
            WslStatus::Available {
                managed_distribution: true,
            } => return Ok(()),
            WslStatus::Available {
                managed_distribution: false,
            } => {}
        }
        let output = Command::new("wsl.exe")
            .args([
                "--install",
                MANAGED_WSL_BASE_DISTRIBUTION,
                "--name",
                MANAGED_WSL_DISTRIBUTION,
                "--no-launch",
                "--web-download",
            ])
            .output()
            .map_err(|error| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::WslDistributionProvisioning,
                    format!("could not start managed WSL distribution provisioning: {error}"),
                )
            })?;
        let combined = command_output_text(&output);
        if !output.status.success() {
            if output_mentions_restart(&combined) {
                self.mark_restart_required()?;
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ElevationOrRestart,
                    "Windows reports that WSL setup requires a restart; setup remains incomplete until the application is reopened after restart",
                ));
            }
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::WslDistributionProvisioning,
                format!("managed WSL distribution provisioning failed: {combined}"),
            ));
        }
        match wsl_status()? {
            WslStatus::Available {
                managed_distribution: true,
            } => Ok(()),
            _ => Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::WslDistributionProvisioning,
                "WSL provisioning returned success but the dedicated GameEngine distribution is not registered",
            )),
        }
    }

    pub(crate) fn install_pinned_runtime(
        &self,
        environment: ManagedExecutionEnvironment,
    ) -> Result<ManagedRuntimeInstallation, ManagedLocalRuntimeError> {
        if let Some(existing) = self.active_installation(environment)?
            && existing.runtime_tag == PINNED_LLAMA_CPP_TAG
            && existing.runtime_revision == PINNED_LLAMA_CPP_REVISION
            && self.verify_retained_runtime_artifact(&existing).is_ok()
        {
            return Ok(existing);
        }
        if !cfg!(target_os = "windows") {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                "managed llama.cpp installation is available only from the Windows first-release Editor",
            ));
        }
        if environment == ManagedExecutionEnvironment::Wsl2Linux {
            match wsl_status()? {
                WslStatus::Available {
                    managed_distribution: true,
                } => {}
                WslStatus::Available {
                    managed_distribution: false,
                } => {
                    return Err(ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::WslDistributionProvisioning,
                        "the dedicated GameEngine-LocalAI WSL distribution is not provisioned",
                    ));
                }
                WslStatus::Unavailable(message) => {
                    return Err(ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                        message,
                    ));
                }
            }
        }

        let environment_root = self.root.join("runtime").join(environment.storage_key());
        fs::create_dir_all(&environment_root).map_err(runtime_io)?;
        let final_root = environment_root.join(PINNED_LLAMA_CPP_TAG);
        let staging_root = environment_root.join(format!("{}.staging", PINNED_LLAMA_CPP_TAG));
        if staging_root.exists() {
            fs::remove_dir_all(&staging_root).map_err(runtime_io)?;
        }
        fs::create_dir_all(&staging_root).map_err(runtime_io)?;

        let (artifact_name, artifact_sha256, server_path) = match environment {
            ManagedExecutionEnvironment::WindowsNative => {
                let downloads = self.root.join("downloads");
                fs::create_dir_all(&downloads).map_err(runtime_io)?;
                let mut verified_assets = Vec::new();
                for asset_name in windows_cuda_asset_names() {
                    let release_asset = fetch_release_asset(asset_name)?;
                    let expected_sha256 = release_asset
                        .digest
                        .strip_prefix("sha256:")
                        .filter(|digest| is_sha256_hex(digest))
                        .ok_or_else(|| {
                            ManagedLocalRuntimeError::new(
                                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                                format!(
                                    "official llama.cpp release metadata did not provide a SHA-256 digest for {asset_name}"
                                ),
                            )
                        })?
                        .to_ascii_lowercase();
                    let archive_path = downloads.join(asset_name);
                    download_https_file(&release_asset.browser_download_url, &archive_path)?;
                    verify_file_sha256(
                        &archive_path,
                        &expected_sha256,
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                    )?;
                    expand_zip(&archive_path, &staging_root)?;
                    verified_assets.push((asset_name.to_owned(), expected_sha256));
                }
                verify_windows_cuda_runtime(&staging_root)?;
                let server_path = find_file_named(&staging_root, "llama-server.exe")
                    .ok_or_else(|| {
                        ManagedLocalRuntimeError::new(
                            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                            "verified CUDA llama.cpp runtime does not contain llama-server.exe",
                        )
                    })?
                    .to_string_lossy()
                    .into_owned();
                for (asset_name, expected_sha256) in &verified_assets {
                    let source = downloads.join(asset_name);
                    let retained = staging_root.join(asset_name);
                    fs::copy(&source, &retained).map_err(runtime_io)?;
                    verify_file_sha256(
                        &retained,
                        expected_sha256,
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                    )?;
                }
                let manifest_path = staging_root.join(WINDOWS_CUDA_MANIFEST);
                write_atomic(
                    &manifest_path,
                    windows_cuda_runtime_manifest(&verified_assets).as_bytes(),
                )
                .map_err(runtime_io)?;
                let manifest_sha256 = runtime_manifest_sha256(&manifest_path)?;
                (
                    WINDOWS_CUDA_MANIFEST.to_owned(),
                    manifest_sha256,
                    server_path,
                )
            }
            ManagedExecutionEnvironment::Wsl2Linux => {
                let provenance = build_pinned_wsl_cuda_runtime()?;
                let manifest_path = staging_root.join(WSL_CUDA_MANIFEST);
                write_atomic(
                    &manifest_path,
                    wsl_cuda_runtime_manifest(&provenance).as_bytes(),
                )
                .map_err(runtime_io)?;
                let manifest_sha256 = runtime_manifest_sha256(&manifest_path)?;
                (
                    WSL_CUDA_MANIFEST.to_owned(),
                    manifest_sha256,
                    format!(
                        "/var/lib/gameengine/local-ai/runtime/{PINNED_LLAMA_CPP_TAG}/llama-server"
                    ),
                )
            }
        };

        let installation = ManagedRuntimeInstallation {
            schema_version: STATE_SCHEMA_VERSION,
            runtime_family: "llama.cpp".to_owned(),
            runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
            runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
            environment,
            artifact_name: artifact_name.clone(),
            artifact_sha256,
            installed_unix_ms: now_unix_ms(),
            compatibility_version: MANAGED_RUNTIME_COMPATIBILITY_VERSION.to_owned(),
            server_path,
            retained_artifact_path: final_root.join(&artifact_name),
        };
        write_json(&staging_root.join("installation.json"), &installation).map_err(runtime_io)?;

        if final_root.exists() {
            fs::remove_dir_all(&final_root).map_err(runtime_io)?;
        }
        fs::rename(&staging_root, &final_root).map_err(runtime_io)?;
        let mut activated = installation;
        if activated.environment == ManagedExecutionEnvironment::WindowsNative {
            activated.server_path = final_root
                .join(
                    Path::new(&activated.server_path)
                        .strip_prefix(&staging_root)
                        .unwrap_or(Path::new("llama-server.exe")),
                )
                .to_string_lossy()
                .into_owned();
        }
        activated.retained_artifact_path = final_root.join(&artifact_name);
        write_json(&environment_root.join("active.json"), &activated).map_err(runtime_io)?;
        Ok(activated)
    }

    pub(crate) fn active_installation(
        &self,
        environment: ManagedExecutionEnvironment,
    ) -> Result<Option<ManagedRuntimeInstallation>, ManagedLocalRuntimeError> {
        let path = self
            .root
            .join("runtime")
            .join(environment.storage_key())
            .join("active.json");
        read_optional_json(&path).map_err(runtime_io)
    }

    /// Returns every registered managed model.
    ///
    /// Records written before GameEngine measured GGUF representations are
    /// re-measured here from the registered file itself, so an operator does not
    /// have to re-register a multi-gigabyte GGUF only to recover a descriptor
    /// that the file still carries.
    pub(crate) fn registered_models(
        &self,
    ) -> Result<Vec<ManagedModelRegistration>, ManagedLocalRuntimeError> {
        let mut registry: ManagedModelRegistry = read_optional_json(&self.model_registry_path())
            .map_err(model_io)?
            .unwrap_or_default();
        if remeasure_unmeasured_representations(&mut registry) {
            // A registry that cannot be rewritten only costs another measurement
            // on the next read. These descriptors were measured from the
            // registered bytes either way, so a failed rewrite must not hide them.
            let _ = write_json(&self.model_registry_path(), &registry);
        }
        Ok(registry.models)
    }

    pub(crate) fn register_existing_gguf(
        &self,
        path: &Path,
        display_name: Option<&str>,
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("gguf") {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "only an explicit .gguf file can be registered as a managed model",
            ));
        }
        let canonical = fs::canonicalize(path).map_err(model_io)?;
        let representation = gguf::inspect_representation(&canonical).map_err(model_io)?;
        let content_sha256 = sha256_via_platform(&canonical)?;
        let metadata = fs::metadata(&canonical).map_err(model_io)?;
        let model_id = format!("gguf:{}", &content_sha256[..16]);
        let registration = ManagedModelRegistration {
            model_id: model_id.clone(),
            display_name: display_name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    canonical
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| model_id.clone()),
            content_sha256,
            source_path: canonical.clone(),
            size_bytes: metadata.len(),
            modified_unix_ms: metadata.modified().ok().and_then(system_time_unix_ms),
            quantization: representation.canonical_quantization,
            representation: Some(representation.descriptor),
            capability: representation.capability,
            projector: None,
            source: None,
            license: None,
        };
        let mut registry: ManagedModelRegistry = read_optional_json(&self.model_registry_path())
            .map_err(model_io)?
            .unwrap_or_default();
        if let Some(existing) = registry
            .models
            .iter_mut()
            .find(|model| model.content_sha256 == registration.content_sha256)
        {
            *existing = registration.clone();
        } else {
            registry.models.push(registration.clone());
            registry
                .models
                .sort_by(|left, right| left.model_id.cmp(&right.model_id));
        }
        write_json(&self.model_registry_path(), &registry).map_err(model_io)?;
        Ok(registration)
    }

    /// Registers a multimodal projector for one already registered model.
    ///
    /// The projector is what gives a local model image input. Nothing here
    /// depends on which model family it belongs to: any GGUF projector paired
    /// with any registered model makes that configuration image-capable.
    pub(crate) fn register_projector(
        &self,
        model_id: &str,
        path: &Path,
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("gguf") {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "only an explicit .gguf projector file can be registered",
            ));
        }
        let canonical = fs::canonicalize(path).map_err(model_io)?;
        let content_sha256 = sha256_via_platform(&canonical)?;
        let metadata = fs::metadata(&canonical).map_err(model_io)?;
        let projector = ManagedProjectorRegistration {
            source_path: canonical,
            content_sha256,
            size_bytes: metadata.len(),
            modified_unix_ms: metadata.modified().ok().and_then(system_time_unix_ms),
        };
        self.update_registration(model_id, |model| model.projector = Some(projector.clone()))
    }

    /// Removes the projector registration from one model.
    pub(crate) fn remove_projector(
        &self,
        model_id: &str,
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        self.update_registration(model_id, |model| model.projector = None)
    }

    fn update_registration(
        &self,
        model_id: &str,
        mutate: impl FnOnce(&mut ManagedModelRegistration),
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        let mut registry: ManagedModelRegistry = read_optional_json(&self.model_registry_path())
            .map_err(model_io)?
            .unwrap_or_default();
        let record = registry
            .models
            .iter_mut()
            .find(|model| model.model_id == model_id)
            .ok_or_else(|| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ModelResource,
                    format!("managed model `{model_id}` is not registered"),
                )
            })?;
        mutate(record);
        let updated = record.clone();
        write_json(&self.model_registry_path(), &registry).map_err(model_io)?;
        Ok(updated)
    }

    #[cfg(feature = "visual-validation")]
    pub(crate) fn register_visual_validation_model(
        &self,
        path: &Path,
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        gguf::write_visual_validation_gguf(path).map_err(model_io)?;
        self.register_existing_gguf(path, None)
    }

    pub(crate) fn additional_storage_for_environment(
        &self,
        model_id: &str,
        environment: ManagedExecutionEnvironment,
    ) -> Result<u64, ManagedLocalRuntimeError> {
        if environment == ManagedExecutionEnvironment::WindowsNative {
            return Ok(0);
        }
        let model = self.require_model(model_id)?;
        let target = wsl_model_path(&model.content_sha256);
        if wsl_file_exists(&target).unwrap_or(false) {
            Ok(0)
        } else {
            Ok(model.size_bytes)
        }
    }

    /// Collects every managed-environment fact the Local AI panel renders.
    ///
    /// Callers must run this off the UI thread: the WSL2 paths of
    /// [`Self::setup_status`] and [`Self::additional_storage_for_environment`]
    /// both block on `wsl.exe`.
    pub(crate) fn probe_environment(
        &self,
        environment: ManagedExecutionEnvironment,
        model_id: String,
    ) -> ManagedEnvironmentProbe {
        let setup_status = self.setup_status(environment);
        let (additional_storage_bytes, described_config) = if model_id.trim().is_empty() {
            (
                Ok(0),
                Err(
                    "Register or select a managed GGUF model before starting inference.".to_owned(),
                ),
            )
        } else {
            (
                self.additional_storage_for_environment(&model_id, environment)
                    .map_err(|error| error.to_string()),
                self.configuration_for(&model_id, environment, ManagedIntegrityCheck::Skipped)
                    .map_err(|error| error.to_string()),
            )
        };
        ManagedEnvironmentProbe {
            environment,
            model_id,
            setup_status,
            additional_storage_bytes,
            described_config,
        }
    }

    pub(crate) fn prepare_model_for_environment(
        &self,
        model_id: &str,
        environment: ManagedExecutionEnvironment,
        duplicate_storage_approved: bool,
    ) -> Result<PathBuf, ManagedLocalRuntimeError> {
        let model = self.require_model(model_id)?;
        if environment == ManagedExecutionEnvironment::WindowsNative {
            self.verify_registered_model(&model)?;
            return Ok(model.source_path);
        }
        match wsl_status()? {
            WslStatus::Available {
                managed_distribution: true,
            } => {}
            WslStatus::Available {
                managed_distribution: false,
            } => {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::WslDistributionProvisioning,
                    "the dedicated GameEngine-LocalAI WSL distribution is not provisioned",
                ));
            }
            WslStatus::Unavailable(message) => {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                    message,
                ));
            }
        }
        self.verify_registered_model(&model)?;
        prepare_wsl_copy(
            &model.source_path,
            &model.content_sha256,
            model.size_bytes,
            duplicate_storage_approved,
        )
    }

    /// Prepares the multimodal projector for one environment, when registered.
    ///
    /// Returns `Ok(None)` for a model with no projector, which is the ordinary
    /// text-only case rather than a failure.
    pub(crate) fn prepare_projector_for_environment(
        &self,
        model_id: &str,
        environment: ManagedExecutionEnvironment,
        duplicate_storage_approved: bool,
    ) -> Result<Option<PathBuf>, ManagedLocalRuntimeError> {
        let model = self.require_model(model_id)?;
        let Some(projector) = model.projector else {
            return Ok(None);
        };
        if environment == ManagedExecutionEnvironment::WindowsNative {
            return Ok(Some(projector.source_path));
        }
        prepare_wsl_copy(
            &projector.source_path,
            &projector.content_sha256,
            projector.size_bytes,
            duplicate_storage_approved,
        )
        .map(Some)
    }

    /// Resolves the launch configuration of a registered managed model.
    ///
    /// With [`ManagedIntegrityCheck::Enforced`] this also runs the ADR 0155
    /// pre-inference verification, which hashes the retained runtime artifact
    /// and, on WSL2, the whole model file. Callers that only render the
    /// resulting identity MUST pass [`ManagedIntegrityCheck::Skipped`] and MUST
    /// NOT call the enforced form from a frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the pinned runtime is not installed, the model is
    /// not registered, the selected environment has no prepared model copy, or
    /// an enforced integrity check fails.
    pub(crate) fn configuration_for(
        &self,
        model_id: &str,
        environment: ManagedExecutionEnvironment,
        integrity: ManagedIntegrityCheck,
    ) -> Result<ManagedLocalModelConfig, ManagedLocalRuntimeError> {
        let enforced = integrity == ManagedIntegrityCheck::Enforced;
        let installation = self.active_installation(environment)?.ok_or_else(|| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!(
                    "{} managed llama.cpp runtime is not installed",
                    environment.label()
                ),
            )
        })?;
        if enforced {
            self.verify_retained_runtime_artifact(&installation)?;
        }
        let model = self.require_model(model_id)?;
        let model_path = if environment == ManagedExecutionEnvironment::WindowsNative {
            if enforced {
                self.verify_registered_model(&model)?;
            }
            model.source_path.clone()
        } else {
            let path = wsl_model_path(&model.content_sha256);
            if !wsl_file_exists(&path)? {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                    format!(
                        "model {} is registered but no verified Linux-native WSL2 copy is prepared",
                        model.display_name
                    ),
                ));
            }
            if enforced {
                verify_wsl_sha256(&path, &model.content_sha256)?;
            }
            PathBuf::from(path)
        };
        let projector_path = match model.projector.as_ref() {
            Some(projector) if environment == ManagedExecutionEnvironment::WindowsNative => {
                Some(projector.source_path.clone())
            }
            Some(projector) => {
                let path = wsl_model_path(&projector.content_sha256);
                if !wsl_file_exists(&path)? {
                    return Err(ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                        format!(
                            "model {} has a registered projector with no verified Linux-native WSL2 copy",
                            model.display_name
                        ),
                    ));
                }
                if enforced {
                    verify_wsl_sha256(&path, &projector.content_sha256)?;
                }
                Some(PathBuf::from(path))
            }
            None => None,
        };
        Ok(ManagedLocalModelConfig {
            state_root: self.root.clone(),
            environment,
            model_id: model.model_id,
            model_content_sha256: model.content_sha256,
            model_path,
            model_size_bytes: model.size_bytes,
            quantization: model.quantization,
            model_representation: model.representation,
            capability: model.capability,
            projector_path,
            runtime_tag: installation.runtime_tag,
            runtime_revision: installation.runtime_revision,
            runtime_artifact_sha256: installation.artifact_sha256,
            runtime_compatibility_version: installation.compatibility_version,
        })
    }

    /// Runs the ADR 0155 pre-inference integrity gate against an identity frozen earlier by the UI.
    ///
    /// This function hashes managed model bytes when required and therefore belongs on an
    /// inference worker, never on the Editor frame thread. It also rejects machine-local state
    /// drift instead of silently substituting a different model or runtime after Send.
    pub(crate) fn verify_frozen_configuration(
        config: &ManagedLocalModelConfig,
    ) -> Result<(), ManagedLocalRuntimeError> {
        let manager = Self::open(config.state_root.clone())?;
        let verified = manager.configuration_for(
            &config.model_id,
            config.environment,
            ManagedIntegrityCheck::Enforced,
        )?;
        if verified.model_content_sha256 != config.model_content_sha256
            || verified.model_path != config.model_path
            || verified.model_size_bytes != config.model_size_bytes
            || verified.quantization != config.quantization
            || verified.model_representation != config.model_representation
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "managed model identity changed after the inference request was frozen",
            ));
        }
        if verified.runtime_tag != config.runtime_tag
            || verified.runtime_revision != config.runtime_revision
            || verified.runtime_artifact_sha256 != config.runtime_artifact_sha256
            || verified.runtime_compatibility_version != config.runtime_compatibility_version
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "managed runtime identity changed after the inference request was frozen",
            ));
        }
        Ok(())
    }

    /// Acquires a process-local lease for the exact frozen managed model/runtime
    /// and returns the endpoint plus read-only identity needed by an external
    /// local-agent adapter.
    pub(crate) fn lease_endpoint(
        config: &ManagedLocalModelConfig,
    ) -> Result<ManagedLocalEndpointLease, ManagedLocalRuntimeError> {
        Self::verify_frozen_configuration(config)?;
        let key = managed_endpoint_lease_key(config);
        {
            let mut leases = lock_managed_endpoint_leases()?;
            if leases.iter().any(|(existing_key, state)| {
                existing_key != &key && state.holders > 0 && state.state_root == config.state_root
            }) {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    "managed llama.cpp is leased by another model/runtime identity",
                ));
            }
            let state = leases
                .entry(key.clone())
                .or_insert_with(|| ManagedEndpointLeaseState {
                    holders: 0,
                    state_root: config.state_root.clone(),
                    environment: config.environment,
                });
            state.holders = state.holders.saturating_add(1);
        }

        let endpoint = match Self::ensure_endpoint(config) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = release_managed_endpoint_lease(&key);
                return Err(error);
            }
        };
        let identity = ManagedLocalEndpointIdentity {
            endpoint_url: endpoint.url.clone(),
            model_id: config.model_id.clone(),
            model_content_sha256: config.model_content_sha256.clone(),
            model_representation: config.model_representation.clone(),
            runtime_identity: config.benchmark_runtime_identity(),
            execution_environment: config.environment,
        };
        Ok(ManagedLocalEndpointLease {
            key,
            identity,
            released: false,
        })
    }

    pub(crate) fn ensure_endpoint(
        config: &ManagedLocalModelConfig,
    ) -> Result<ManagedEndpoint, ManagedLocalRuntimeError> {
        if managed_conflicting_active_lease(config)? {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                "managed llama.cpp endpoint cannot switch model/runtime while another endpoint lease is active",
            ));
        }
        let manager = Self::open(config.state_root.clone())?;
        let installation = manager
            .active_installation(config.environment)?
            .ok_or_else(|| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                    "managed llama.cpp runtime is no longer installed",
                )
            })?;
        if installation.runtime_tag != config.runtime_tag
            || installation.runtime_revision != config.runtime_revision
            || installation.artifact_sha256 != config.runtime_artifact_sha256
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "managed runtime identity changed after this model configuration was frozen",
            ));
        }
        manager.verify_retained_runtime_artifact(&installation)?;
        if let Some(process) = manager.read_process_state()? {
            if process.environment == config.environment
                && process.model_content_sha256 == config.model_content_sha256
                && process.runtime_revision == config.runtime_revision
                && endpoint_is_healthy(&process.endpoint)
            {
                return Ok(ManagedEndpoint {
                    url: process.endpoint,
                    process_id: process.process_id,
                    reused_process: true,
                });
            }
            manager.stop_process_state(&process)?;
        }

        let port = reserve_loopback_port()?;
        let endpoint = format!("http://127.0.0.1:{port}");
        let process_id = match config.environment {
            ManagedExecutionEnvironment::WindowsNative => {
                manager.spawn_windows_server(&installation, config, port)?
            }
            ManagedExecutionEnvironment::Wsl2Linux => {
                manager.spawn_wsl_server(&installation, config, port)?
            }
        };
        let state = ManagedProcessState {
            schema_version: STATE_SCHEMA_VERSION,
            environment: config.environment,
            process_id,
            endpoint: endpoint.clone(),
            runtime_revision: config.runtime_revision.clone(),
            model_content_sha256: config.model_content_sha256.clone(),
            started_unix_ms: now_unix_ms(),
        };
        manager.write_process_state(&state)?;
        let started = Instant::now();
        while started.elapsed() < STARTUP_TIMEOUT {
            let endpoint_healthy = endpoint_is_healthy(&endpoint);
            if endpoint_healthy {
                return Ok(ManagedEndpoint {
                    url: endpoint,
                    process_id,
                    reused_process: false,
                });
            }
            let process_alive = process_is_alive(&state)?;
            if managed_process_exited_before_health(endpoint_healthy, process_alive) {
                let _ = manager.clear_process_state();
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    format!(
                        "managed llama.cpp exited before its loopback health endpoint became ready; see {}",
                        manager.log_path(config.environment).display()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = manager.stop_process_state(&state);
        Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ManagedProcessStartup,
            "managed llama.cpp did not become healthy within the bounded startup interval",
        ))
    }

    pub(crate) fn remove_environment(
        &self,
        environment: ManagedExecutionEnvironment,
    ) -> Result<(), ManagedLocalRuntimeError> {
        if managed_environment_has_active_lease(&self.root, environment)? {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                "cannot remove a Managed Local runtime while an endpoint lease is active",
            ));
        }
        if let Some(process) = self.read_process_state()?
            && process.environment == environment
        {
            self.stop_process_state(&process)?;
        }
        if environment == ManagedExecutionEnvironment::Wsl2Linux
            && matches!(
                wsl_status(),
                Ok(WslStatus::Available {
                    managed_distribution: true
                })
            )
        {
            let output = Command::new("wsl.exe")
                .args(["--unregister", MANAGED_WSL_DISTRIBUTION])
                .output()
                .map_err(|error| {
                    ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::WslDistributionProvisioning,
                        format!("could not remove the dedicated managed WSL distribution: {error}"),
                    )
                })?;
            if !output.status.success() {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::WslDistributionProvisioning,
                    format!(
                        "could not remove the dedicated managed WSL distribution: {}",
                        command_output_text(&output)
                    ),
                ));
            }
        }
        let runtime_root = self.root.join("runtime").join(environment.storage_key());
        match fs::remove_dir_all(runtime_root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(runtime_io(error)),
        }
        if environment == ManagedExecutionEnvironment::Wsl2Linux {
            self.clear_restart_required()?;
        }
        Ok(())
    }

    pub(crate) fn stop_for_config(
        config: &ManagedLocalModelConfig,
    ) -> Result<(), ManagedLocalRuntimeError> {
        if managed_endpoint_has_active_lease(config)? {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                "cannot stop a Managed Local endpoint while its lifecycle lease is active",
            ));
        }
        let manager = Self::open(config.state_root.clone())?;
        let Some(process) = manager.read_process_state()? else {
            return Ok(());
        };
        if process.environment == config.environment
            && process.model_content_sha256 == config.model_content_sha256
        {
            manager.stop_process_state(&process)?;
        }
        Ok(())
    }

    pub(crate) fn is_resident(
        config: &ManagedLocalModelConfig,
    ) -> Result<bool, ManagedLocalRuntimeError> {
        let manager = Self::open(config.state_root.clone())?;
        Ok(manager.read_process_state()?.is_some_and(|process| {
            process.environment == config.environment
                && process.model_content_sha256 == config.model_content_sha256
                && endpoint_is_healthy(&process.endpoint)
        }))
    }

    fn spawn_windows_server(
        &self,
        installation: &ManagedRuntimeInstallation,
        config: &ManagedLocalModelConfig,
        port: u16,
    ) -> Result<u32, ManagedLocalRuntimeError> {
        let log_path = self.log_path(ManagedExecutionEnvironment::WindowsNative);
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent).map_err(runtime_io)?;
        }
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(runtime_io)?;
        let stderr = stdout.try_clone().map_err(runtime_io)?;
        let child = Command::new(&installation.server_path)
            .args(windows_server_arguments(
                &config.model_path,
                config.projector_path.as_deref(),
                port,
                managed_context_tokens(config),
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    format!("could not launch pinned Windows llama-server: {error}"),
                )
            })?;
        Ok(child.id())
    }

    fn spawn_wsl_server(
        &self,
        installation: &ManagedRuntimeInstallation,
        config: &ManagedLocalModelConfig,
        port: u16,
    ) -> Result<u32, ManagedLocalRuntimeError> {
        match wsl_status()? {
            WslStatus::Available {
                managed_distribution: true,
            } => {}
            WslStatus::Available {
                managed_distribution: false,
            } => {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::WslDistributionProvisioning,
                    "the dedicated managed WSL distribution disappeared before runtime launch",
                ));
            }
            WslStatus::Unavailable(message) => {
                return Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                    message,
                ));
            }
        }
        let command = wsl_server_launch_command(
            &installation.server_path,
            &config.model_path,
            config.projector_path.as_deref(),
            port,
            config.environment,
            managed_context_tokens(config),
        );
        let output = wsl_shell(&command, ManagedDiagnosticLayer::ManagedProcessStartup)?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<u32>().ok())
            .ok_or_else(|| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    "WSL2 launch did not report the managed llama-server process ID",
                )
            })
    }

    fn verify_retained_runtime_artifact(
        &self,
        installation: &ManagedRuntimeInstallation,
    ) -> Result<(), ManagedLocalRuntimeError> {
        let expected_manifest = match installation.environment {
            ManagedExecutionEnvironment::WindowsNative => WINDOWS_CUDA_MANIFEST,
            ManagedExecutionEnvironment::Wsl2Linux => WSL_CUDA_MANIFEST,
        };
        if installation.schema_version != STATE_SCHEMA_VERSION
            || installation.runtime_family != "llama.cpp"
            || installation.runtime_tag != PINNED_LLAMA_CPP_TAG
            || installation.runtime_revision != PINNED_LLAMA_CPP_REVISION
            || installation.artifact_name != expected_manifest
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "managed runtime metadata does not match the pinned first-release CUDA runtime identity",
            ));
        }
        verify_file_sha256(
            &installation.retained_artifact_path,
            &installation.artifact_sha256,
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
        )?;
        let manifest =
            fs::read_to_string(&installation.retained_artifact_path).map_err(runtime_io)?;
        let environment_marker = format!("environment={}", installation.environment.benchmark_id());
        if !manifest
            .lines()
            .any(|line| line == "format=gameengine-managed-runtime-v2")
            || !manifest
                .lines()
                .any(|line| line == format!("tag={PINNED_LLAMA_CPP_TAG}"))
            || !manifest
                .lines()
                .any(|line| line == format!("revision={PINNED_LLAMA_CPP_REVISION}"))
            || !manifest.lines().any(|line| line == environment_marker)
            || !manifest.lines().any(|line| line == "backend=cuda")
        {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                "managed runtime provenance manifest does not match the pinned CUDA runtime identity",
            ));
        }

        match installation.environment {
            ManagedExecutionEnvironment::WindowsNative => {
                let retained_root =
                    installation
                        .retained_artifact_path
                        .parent()
                        .ok_or_else(|| {
                            ManagedLocalRuntimeError::new(
                                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                                "managed Windows CUDA manifest has no runtime directory",
                            )
                        })?;
                for asset_name in windows_cuda_asset_names() {
                    let marker = format!("asset={asset_name} sha256=");
                    let expected_sha256 = manifest
                        .lines()
                        .find_map(|line| line.strip_prefix(&marker))
                        .filter(|digest| is_sha256_hex(digest))
                        .ok_or_else(|| {
                            ManagedLocalRuntimeError::new(
                                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                                format!("managed Windows CUDA manifest is missing {asset_name}"),
                            )
                        })?;
                    verify_file_sha256(
                        &retained_root.join(asset_name),
                        expected_sha256,
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                    )?;
                }
                if !Path::new(&installation.server_path).is_file() {
                    return Err(ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                        "managed Windows CUDA llama-server executable is missing",
                    ));
                }
            }
            ManagedExecutionEnvironment::Wsl2Linux => {
                let server_sha256 = manifest
                    .lines()
                    .find_map(|line| line.strip_prefix("server_sha256="))
                    .filter(|digest| is_sha256_hex(digest))
                    .ok_or_else(|| {
                        ManagedLocalRuntimeError::new(
                            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                            "managed WSL CUDA manifest is missing llama-server SHA-256",
                        )
                    })?;
                let bench_sha256 = manifest
                    .lines()
                    .find_map(|line| line.strip_prefix("bench_sha256="))
                    .filter(|digest| is_sha256_hex(digest))
                    .ok_or_else(|| {
                        ManagedLocalRuntimeError::new(
                            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                            "managed WSL CUDA manifest is missing llama-bench SHA-256",
                        )
                    })?;
                verify_wsl_sha256(&installation.server_path, server_sha256).map_err(|error| {
                    ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                        error.to_string(),
                    )
                })?;
                let bench_path = format!(
                    "/var/lib/gameengine/local-ai/runtime/{PINNED_LLAMA_CPP_TAG}/llama-bench"
                );
                verify_wsl_sha256(&bench_path, bench_sha256).map_err(|error| {
                    ManagedLocalRuntimeError::new(
                        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                        error.to_string(),
                    )
                })?;
            }
        }
        Ok(())
    }

    fn verify_registered_model(
        &self,
        model: &ManagedModelRegistration,
    ) -> Result<(), ManagedLocalRuntimeError> {
        let metadata = fs::metadata(&model.source_path).map_err(model_io)?;
        if metadata.len() != model.size_bytes {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "registered GGUF size changed; re-register the exact model representation",
            ));
        }
        let modified = metadata.modified().ok().and_then(system_time_unix_ms);
        if modified != model.modified_unix_ms {
            verify_file_sha256(
                &model.source_path,
                &model.content_sha256,
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            )?;
        }
        Ok(())
    }

    fn require_model(
        &self,
        model_id: &str,
    ) -> Result<ManagedModelRegistration, ManagedLocalRuntimeError> {
        self.registered_models()?
            .into_iter()
            .find(|model| model.model_id == model_id)
            .ok_or_else(|| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                    format!("managed model `{model_id}` is not registered"),
                )
            })
    }

    fn model_registry_path(&self) -> PathBuf {
        self.root.join("models").join("registry.json")
    }

    fn restart_marker_path(&self) -> PathBuf {
        self.root.join("state").join("restart-required.txt")
    }

    /// Returns the newest managed llama-server log lines without exposing
    /// execution-environment-specific paths to AI Studio.
    pub(crate) fn recent_server_log_lines(
        &self,
        environment: ManagedExecutionEnvironment,
        max_lines: usize,
    ) -> Result<Vec<String>, ManagedLocalRuntimeError> {
        if max_lines == 0 {
            return Ok(Vec::new());
        }
        match environment {
            ManagedExecutionEnvironment::WindowsNative => {
                let path = self.log_path(environment);
                match read_recent_local_log_lines(&path, max_lines) {
                    Ok(lines) => Ok(lines),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(error) => Err(runtime_io(error)),
                }
            }
            ManagedExecutionEnvironment::Wsl2Linux => {
                let line_count = max_lines.to_string();
                let log = wsl_server_log_path(environment);
                let output = managed_wsl_command()
                    .args(["tail", "-n", line_count.as_str(), log.as_str()])
                    .output()
                    .map_err(|error| {
                        ManagedLocalRuntimeError::new(
                            ManagedDiagnosticLayer::ManagedProcessStartup,
                            format!("could not read managed WSL llama-server log: {error}"),
                        )
                    })?;
                if output.status.success() {
                    return Ok(recent_log_lines(&output.stdout, max_lines));
                }
                let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                if message.contains("No such file or directory") {
                    return Ok(Vec::new());
                }
                Err(ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    if message.is_empty() {
                        format!(
                            "managed WSL llama-server log tail exited as {}",
                            output.status
                        )
                    } else {
                        format!("managed WSL llama-server log tail failed: {message}")
                    },
                ))
            }
        }
    }

    fn process_state_path(&self) -> PathBuf {
        self.root.join("state").join("process.json")
    }

    fn log_path(&self, environment: ManagedExecutionEnvironment) -> PathBuf {
        match environment {
            ManagedExecutionEnvironment::WindowsNative => self
                .root
                .join("logs")
                .join(format!("llama-server-{}.log", environment.storage_key())),
            ManagedExecutionEnvironment::Wsl2Linux => {
                PathBuf::from(wsl_server_log_path(environment))
            }
        }
    }

    fn read_process_state(&self) -> Result<Option<ManagedProcessState>, ManagedLocalRuntimeError> {
        read_optional_json(&self.process_state_path()).map_err(runtime_io)
    }

    fn write_process_state(
        &self,
        state: &ManagedProcessState,
    ) -> Result<(), ManagedLocalRuntimeError> {
        write_json(&self.process_state_path(), state).map_err(runtime_io)
    }

    fn clear_process_state(&self) -> Result<(), ManagedLocalRuntimeError> {
        match fs::remove_file(self.process_state_path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(runtime_io(error)),
        }
    }

    fn stop_process_state(
        &self,
        state: &ManagedProcessState,
    ) -> Result<(), ManagedLocalRuntimeError> {
        let status = match state.environment {
            ManagedExecutionEnvironment::WindowsNative => Command::new("taskkill.exe")
                .args(["/PID", &state.process_id.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
            ManagedExecutionEnvironment::Wsl2Linux => managed_wsl_command()
                .args(["kill", &state.process_id.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        };
        if let Err(error) = status {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelResource,
                format!("could not stop managed llama-server: {error}"),
            ));
        }
        wait_while_process_alive(state, MANAGED_SERVER_RELEASE_TIMEOUT)?;
        if managed_release_should_force_kill(state.environment, process_is_alive(state)?) {
            // The graceful `kill` above never lands while llama-server is stuck releasing the
            // WSL2 GPU paravirtualization device, so escalate to SIGKILL before giving up.
            let _ = managed_wsl_command()
                .args(["kill", "-9", &state.process_id.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            wait_while_process_alive(state, MANAGED_SERVER_FORCE_KILL_TIMEOUT)?;
        }
        let still_alive = process_is_alive(state)?;
        self.clear_process_state()?;
        if still_alive {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelResource,
                "managed llama-server did not exit after the requested release, so its device memory is still reserved",
            ));
        }
        if endpoint_is_healthy(&state.endpoint) {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelResource,
                "managed llama-server remained reachable after the requested release",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedProcessState {
    schema_version: u32,
    environment: ManagedExecutionEnvironment,
    process_id: u32,
    endpoint: String,
    runtime_revision: String,
    model_content_sha256: String,
    started_unix_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ManagedModelRegistry {
    #[serde(default)]
    models: Vec<ManagedModelRegistration>,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseMetadata {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WslStatus {
    Unavailable(String),
    Available { managed_distribution: bool },
}

fn classify_wsl_status(
    status_success: bool,
    status_message: &str,
    distributions_success: bool,
    distributions_text: &str,
) -> WslStatus {
    if !status_success {
        return WslStatus::Unavailable(status_message.to_owned());
    }
    if !distributions_success {
        return WslStatus::Unavailable(distributions_text.to_owned());
    }
    let managed_distribution = distributions_text
        .lines()
        .map(str::trim)
        .any(|line| line == MANAGED_WSL_DISTRIBUTION);
    WslStatus::Available {
        managed_distribution,
    }
}

/// Resolves the managed context window from measured model shape and device memory.
///
/// The policy is deliberately model-family independent: it reads what the GGUF declares,
/// bounds it by what the measured device can hold, and clamps the result to the range the
/// managed agent protocol needs. A model that declares nothing keeps the previously measured
/// default rather than receiving a value invented from its name.
fn resolve_managed_context_tokens(
    capability: &GgufModelCapability,
    device_memory_bytes: Option<u64>,
) -> u32 {
    let memory_budget_tokens = match (capability.kv_cache_bytes_per_token, device_memory_bytes) {
        (Some(bytes_per_token), Some(device_memory)) if bytes_per_token > 0 => {
            let budget = device_memory / 100 * MANAGED_KV_CACHE_MEMORY_PERCENT;
            Some(u32::try_from(budget / bytes_per_token).unwrap_or(u32::MAX))
        }
        _ => None,
    };
    let requested = match (capability.train_context_tokens, memory_budget_tokens) {
        (Some(declared), Some(budget)) => declared.min(budget),
        (Some(declared), None) => declared,
        (None, Some(budget)) => budget,
        (None, None) => return MANAGED_CONTEXT_UNMEASURED_TOKENS,
    };
    let aligned = requested / MANAGED_CONTEXT_ALIGNMENT_TOKENS * MANAGED_CONTEXT_ALIGNMENT_TOKENS;
    aligned.clamp(MANAGED_CONTEXT_FLOOR_TOKENS, MANAGED_CONTEXT_CEILING_TOKENS)
}

/// Context window a managed configuration launches with on this machine.
pub(crate) fn managed_context_tokens(config: &ManagedLocalModelConfig) -> u32 {
    resolve_managed_context_tokens(
        &config.capability,
        crate::agent_benchmark::largest_device_memory_bytes(),
    )
}

fn windows_server_arguments(
    model_path: &Path,
    projector_path: Option<&Path>,
    port: u16,
    context_tokens: u32,
) -> Vec<String> {
    let mut arguments = vec![
        "--model".to_owned(),
        model_path.to_string_lossy().into_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--fit".to_owned(),
        "on".to_owned(),
        "--fit-target".to_owned(),
        MANAGED_GPU_FIT_TARGET_MIB.to_string(),
        "--parallel".to_owned(),
        MANAGED_SERVER_PARALLEL_SLOTS.to_string(),
        "--ctx-size".to_owned(),
        context_tokens.to_string(),
    ];
    if let Some(projector) = projector_path {
        arguments.push("--mmproj".to_owned());
        arguments.push(projector.to_string_lossy().into_owned());
    }
    arguments
}

fn managed_process_exited_before_health(endpoint_healthy: bool, process_alive: bool) -> bool {
    !endpoint_healthy && !process_alive
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslCudaBuildProvenance {
    revision: String,
    compiler_version: String,
    libraries_dev_version: String,
    server_sha256: String,
    bench_sha256: String,
}

fn windows_cuda_asset_names() -> [&'static str; 2] {
    [WINDOWS_CUDA_RUNTIME_ASSET, WINDOWS_CUDA_SUPPORT_ASSET]
}

fn runtime_manifest_sha256(path: &Path) -> Result<String, ManagedLocalRuntimeError> {
    sha256_via_platform(path).map_err(|error| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            error.to_string(),
        )
    })
}

fn verify_windows_cuda_runtime(root: &Path) -> Result<(), ManagedLocalRuntimeError> {
    let bench = find_file_named(root, "llama-bench.exe").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "verified CUDA llama.cpp archive does not contain llama-bench.exe",
        )
    })?;
    let output = Command::new(&bench)
        .arg("--list-devices")
        .output()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::GpuOrBackendCapability,
                format!("could not inspect the pinned Windows CUDA runtime: {error}"),
            )
        })?;
    let devices = command_output_text(&output);
    if !output.status.success() || !devices.to_ascii_lowercase().contains("cuda") {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::GpuOrBackendCapability,
            format!("pinned Windows CUDA runtime did not report a CUDA device: {devices}"),
        ));
    }
    Ok(())
}

fn os_release_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| value.trim().trim_matches('\"'))
    })
}

fn verify_managed_wsl_userland() -> Result<(), ManagedLocalRuntimeError> {
    let output = wsl_shell(
        "cat /etc/os-release",
        ManagedDiagnosticLayer::GpuOrBackendCapability,
    )?;
    let text = String::from_utf8_lossy(&output.stdout);
    let id = os_release_value(&text, "ID");
    let version = os_release_value(&text, "VERSION_ID");
    if id != Some("ubuntu") || version != Some(MANAGED_WSL_EXPECTED_VERSION_ID) {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::GpuOrBackendCapability,
            format!(
                "managed GameEngine-LocalAI WSL userland must be Ubuntu {MANAGED_WSL_EXPECTED_VERSION_ID}; observed ID={} VERSION_ID={}",
                id.unwrap_or("<missing>"),
                version.unwrap_or("<missing>")
            ),
        ));
    }
    Ok(())
}

fn wsl_cuda_bootstrap_command() -> String {
    format!(
        concat!(
            "set -eu; export DEBIAN_FRONTEND=noninteractive; ",
            "apt-get update; ",
            "apt-get install -y --no-install-recommends ca-certificates curl gnupg git build-essential cmake ninja-build pkg-config libcurl4-openssl-dev; ",
            "if ! dpkg-query -W -f='${{Status}}' {compiler_package} 2>/dev/null | grep -qx 'install ok installed' || ! dpkg-query -W -f='${{Status}}' {libraries_dev_package} 2>/dev/null | grep -qx 'install ok installed'; then ",
            "curl -fsSL {repository}/{keyring} -o /tmp/{keyring}; dpkg -i /tmp/{keyring}; rm -f /tmp/{keyring}; apt-get update; apt-get install -y --no-install-recommends {compiler_package} {libraries_dev_package}; ",
            "fi; ",
            "dpkg-query -W -f='${{Status}}' {compiler_package} | grep -qx 'install ok installed'; ",
            "dpkg-query -W -f='${{Status}}' {libraries_dev_package} | grep -qx 'install ok installed'; ",
            "test -x /usr/local/cuda-12.4/bin/nvcc; test -e /dev/dxg"
        ),
        compiler_package = WSL_CUDA_COMPILER_PACKAGE,
        libraries_dev_package = WSL_CUDA_LIBRARIES_DEV_PACKAGE,
        repository = WSL_CUDA_REPOSITORY_URL,
        keyring = WSL_CUDA_KEYRING_ASSET,
    )
}

fn wsl_cuda_build_command() -> String {
    format!(
        concat!(
            "set -eu; root=/var/lib/gameengine/local-ai; src=\"$root/src/llama.cpp-{tag}\"; stage=\"$root/runtime/{tag}.staging\"; final=\"$root/runtime/{tag}\"; log=\"$root/logs/llama-build-{tag}.log\"; ",
            "mkdir -p \"$root/src\" \"$root/runtime\" \"$root/logs\"; rm -rf \"$src\" \"$stage\"; ",
            "trap 'status=$?; trap - 0; if [ \"$status\" = 0 ]; then :; else tail -200 \"$log\" >&2 || true; fi; exit \"$status\"' 0; ",
            "{{ git clone --filter=blob:none --depth 1 --branch {tag} {repository} \"$src\"; ",
            "revision=$(git -C \"$src\" rev-parse HEAD); case \"$revision\" in {revision}*) ;; *) echo \"unexpected llama.cpp revision: $revision\" >&2; exit 1;; esac; ",
            "export PATH=/usr/local/cuda-12.4/bin:$PATH; ",
            "cmake -S \"$src\" -B \"$src/build\" -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=ON -DCMAKE_CUDA_COMPILER=/usr/local/cuda-12.4/bin/nvcc -DCMAKE_INSTALL_RPATH='/usr/local/cuda-12.4/lib64;$ORIGIN' -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON; ",
            "cmake --build \"$src/build\" --target llama-server llama-bench --parallel; ",
            "mkdir -p \"$stage\"; cp -a \"$src/build/bin/.\" \"$stage/\"; test -x \"$stage/llama-server\"; test -x \"$stage/llama-bench\"; ",
            "rm -rf \"$src\"; \"$stage/llama-server\" --version; devices=$(\"$stage/llama-bench\" --list-devices 2>&1); printf '%s\n' \"$devices\"; printf '%s\n' \"$devices\" | grep -qi cuda; ",
            "server_sha=$(sha256sum \"$stage/llama-server\" | awk '{{print $1}}'); bench_sha=$(sha256sum \"$stage/llama-bench\" | awk '{{print $1}}'); ",
            "compiler_version=$(dpkg-query -W -f='${{Version}}' {compiler_package}); libraries_dev_version=$(dpkg-query -W -f='${{Version}}' {libraries_dev_package}); ",
            "rm -rf \"$final\"; mv \"$stage\" \"$final\"; }} >\"$log\" 2>&1; ",
            "trap - 0; printf 'GAMEENGINE_REVISION=%s\nGAMEENGINE_COMPILER_VERSION=%s\nGAMEENGINE_LIBRARIES_DEV_VERSION=%s\nGAMEENGINE_SERVER_SHA256=%s\nGAMEENGINE_BENCH_SHA256=%s\n' \"$revision\" \"$compiler_version\" \"$libraries_dev_version\" \"$server_sha\" \"$bench_sha\""
        ),
        tag = PINNED_LLAMA_CPP_TAG,
        revision = PINNED_LLAMA_CPP_REVISION,
        repository = LLAMA_CPP_REPOSITORY_URL,
        compiler_package = WSL_CUDA_COMPILER_PACKAGE,
        libraries_dev_package = WSL_CUDA_LIBRARIES_DEV_PACKAGE,
    )
}

fn build_pinned_wsl_cuda_runtime() -> Result<WslCudaBuildProvenance, ManagedLocalRuntimeError> {
    verify_managed_wsl_userland()?;
    let bootstrap = wsl_cuda_bootstrap_command();
    wsl_shell(&bootstrap, ManagedDiagnosticLayer::GpuOrBackendCapability)?;

    let build = wsl_cuda_build_command();
    let output = wsl_shell(&build, ManagedDiagnosticLayer::RuntimeArtifactIntegrity)?;
    let text = String::from_utf8_lossy(&output.stdout);
    let marker = |prefix: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .map(str::to_owned)
            .filter(|value| !value.is_empty())
    };
    let revision = marker("GAMEENGINE_REVISION=").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build did not report the exact llama.cpp revision",
        )
    })?;
    if !revision.starts_with(PINNED_LLAMA_CPP_REVISION) {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            format!("WSL CUDA source build produced unexpected revision {revision}"),
        ));
    }
    let compiler_version = marker("GAMEENGINE_COMPILER_VERSION=").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build did not report the installed CUDA compiler version",
        )
    })?;
    let libraries_dev_version = marker("GAMEENGINE_LIBRARIES_DEV_VERSION=").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build did not report the installed CUDA development libraries version",
        )
    })?;
    let server_sha256 = marker("GAMEENGINE_SERVER_SHA256=").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build did not report llama-server SHA-256",
        )
    })?;
    let bench_sha256 = marker("GAMEENGINE_BENCH_SHA256=").ok_or_else(|| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build did not report llama-bench SHA-256",
        )
    })?;
    if !is_sha256_hex(&server_sha256) || !is_sha256_hex(&bench_sha256) {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "WSL CUDA source build reported malformed binary SHA-256 provenance",
        ));
    }
    Ok(WslCudaBuildProvenance {
        revision,
        compiler_version,
        libraries_dev_version,
        server_sha256,
        bench_sha256,
    })
}

fn windows_cuda_runtime_manifest(assets: &[(String, String)]) -> String {
    let mut manifest = format!(
        "format=gameengine-managed-runtime-v2\nruntime=llama.cpp\ntag={PINNED_LLAMA_CPP_TAG}\nrevision={PINNED_LLAMA_CPP_REVISION}\nenvironment=windows_native\nbackend=cuda\ncuda_toolkit=12.4\n"
    );
    for (name, sha256) in assets {
        manifest.push_str(&format!("asset={name} sha256={sha256}\n"));
    }
    manifest
}

fn wsl_cuda_runtime_manifest(provenance: &WslCudaBuildProvenance) -> String {
    format!(
        "format=gameengine-managed-runtime-v2\nruntime=llama.cpp\ntag={PINNED_LLAMA_CPP_TAG}\nrevision={PINNED_LLAMA_CPP_REVISION}\nenvironment=wsl2_linux\nbackend=cuda\ncuda_toolkit=12.4\ncuda_compiler_package={WSL_CUDA_COMPILER_PACKAGE}\ncuda_compiler_version={}\ncuda_libraries_dev_package={WSL_CUDA_LIBRARIES_DEV_PACKAGE}\ncuda_libraries_dev_version={}\nsource={}\nsource_revision={}\nserver_sha256={}\nbench_sha256={}\n",
        provenance.compiler_version,
        provenance.libraries_dev_version,
        LLAMA_CPP_REPOSITORY_URL,
        provenance.revision,
        provenance.server_sha256,
        provenance.bench_sha256,
    )
}

fn fetch_release_asset(name: &str) -> Result<GithubReleaseAsset, ManagedLocalRuntimeError> {
    let script = concat!(
        "$ProgressPreference='SilentlyContinue'; ",
        "$headers=@{'User-Agent'='GameEngine-Managed-LocalAI'}; ",
        "$r=Invoke-RestMethod -Headers $headers -Uri $args[0]; ",
        "$r | ConvertTo-Json -Depth 8 -Compress"
    );
    let output = powershell_output(script, &[RELEASE_METADATA_URL])?;
    let metadata: GithubReleaseMetadata =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!("could not parse official llama.cpp release metadata: {error}"),
            )
        })?;
    if metadata.tag_name != PINNED_LLAMA_CPP_TAG {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            format!(
                "official release metadata resolved tag `{}` instead of pinned `{PINNED_LLAMA_CPP_TAG}`",
                metadata.tag_name
            ),
        ));
    }
    metadata
        .assets
        .into_iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                format!("pinned llama.cpp release does not contain `{name}`"),
            )
        })
}

fn download_https_file(url: &str, path: &Path) -> Result<(), ManagedLocalRuntimeError> {
    if !url.starts_with("https://github.com/") {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            "managed runtime download URL is not an HTTPS GitHub release asset",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(runtime_io)?;
    }
    let path_text = path.to_string_lossy();
    let script = concat!(
        "$ProgressPreference='SilentlyContinue'; ",
        "Invoke-WebRequest -UseBasicParsing -Uri $args[0] -OutFile $args[1]"
    );
    powershell_output(script, &[url, &path_text]).map(|_| ())
}

fn expand_zip(archive: &Path, destination: &Path) -> Result<(), ManagedLocalRuntimeError> {
    let archive = archive.to_string_lossy();
    let destination = destination.to_string_lossy();
    powershell_output(
        "Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force",
        &[&archive, &destination],
    )
    .map(|_| ())
}

fn powershell_output(
    script: &str,
    arguments: &[&str],
) -> Result<std::process::Output, ManagedLocalRuntimeError> {
    let mut argument_prelude = String::from("$args=@(");
    for index in 0..arguments.len() {
        if index > 0 {
            argument_prelude.push(',');
        }
        argument_prelude.push_str(&format!("$env:GAMEENGINE_MANAGED_ARG_{index}"));
    }
    argument_prelude.push_str("); ");
    argument_prelude.push_str(script);
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &argument_prelude,
    ]);
    for (index, argument) in arguments.iter().enumerate() {
        command.env(format!("GAMEENGINE_MANAGED_ARG_{index}"), argument);
    }
    let output = command.output().map_err(|error| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::OperatingSystemPrerequisite,
            format!("could not invoke Windows PowerShell for managed setup: {error}"),
        )
    })?;
    if !output.status.success() {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
            format!(
                "managed setup command failed: {}",
                command_output_text(&output)
            ),
        ));
    }
    Ok(output)
}

fn sha256_via_platform(path: &Path) -> Result<String, ManagedLocalRuntimeError> {
    #[cfg(target_os = "windows")]
    let output = Command::new("certutil.exe")
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .map_err(model_io)?;
    #[cfg(not(target_os = "windows"))]
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(model_io)?;
    if !output.status.success() {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            format!(
                "SHA-256 calculation failed: {}",
                command_output_text(&output)
            ),
        ));
    }
    let output_text = decode_windows_command_text(&output.stdout);
    let digest = output_text
        .split_whitespace()
        .map(|token| token.trim().to_ascii_lowercase())
        .find(|token| is_sha256_hex(token))
        .ok_or_else(|| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "SHA-256 calculation returned no valid digest",
            )
        })?;
    Ok(digest)
}

fn verify_file_sha256(
    path: &Path,
    expected: &str,
    layer: ManagedDiagnosticLayer,
) -> Result<(), ManagedLocalRuntimeError> {
    if !is_sha256_hex(expected) {
        return Err(ManagedLocalRuntimeError::new(
            layer,
            "expected SHA-256 digest is malformed",
        ));
    }
    let actual = sha256_via_platform(path)
        .map_err(|error| ManagedLocalRuntimeError::new(layer, error.to_string()))?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(ManagedLocalRuntimeError::new(
            layer,
            format!("SHA-256 mismatch: expected {expected}, observed {actual}"),
        ));
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn find_file_named(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|value| value.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file_named(&path, name)
        {
            return Some(found);
        }
    }
    None
}

fn managed_wsl_command() -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", MANAGED_WSL_DISTRIBUTION, "-u", "root", "--"]);
    command
}

fn managed_wsl_script_command() -> Command {
    let mut command = managed_wsl_command();
    command.args(["sh", "-s"]);
    command
}

/// Copies one verified file into the managed WSL distribution.
///
/// The copy lands only after its digest matches in the distribution, so an
/// interrupted transfer can never be mistaken for a prepared file.
fn prepare_wsl_copy(
    source: &Path,
    content_sha256: &str,
    size_bytes: u64,
    duplicate_storage_approved: bool,
) -> Result<PathBuf, ManagedLocalRuntimeError> {
    let target = wsl_model_path(content_sha256);
    if wsl_file_exists(&target)? {
        verify_wsl_sha256(&target, content_sha256)?;
        return Ok(PathBuf::from(target));
    }
    if !duplicate_storage_approved {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            format!(
                "WSL2 execution requires an additional {size_bytes} bytes for a Linux-native copy; explicit storage approval is required"
            ),
        ));
    }
    let staging = format!("{target}.staging");
    if let Err(error) = stream_file_into_wsl(source, &staging) {
        let _ = wsl_shell(
            &format!("rm -f {}", shell_quote(&staging)),
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
        );
        return Err(error);
    }
    if let Err(error) = verify_wsl_sha256(&staging, content_sha256) {
        let _ = wsl_shell(
            &format!("rm -f {}", shell_quote(&staging)),
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
        );
        return Err(error);
    }
    wsl_shell(
        &format!("mv -f {} {}", shell_quote(&staging), shell_quote(&target)),
        ManagedDiagnosticLayer::ModelTransferOrIntegrity,
    )?;
    verify_wsl_sha256(&target, content_sha256)?;
    Ok(PathBuf::from(target))
}

fn stream_file_into_wsl(local: &Path, remote: &str) -> Result<(), ManagedLocalRuntimeError> {
    let parent = Path::new(remote)
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                "managed WSL destination has no parent directory",
            )
        })?;
    wsl_shell(
        &format!("mkdir -p {}", shell_quote(&parent)),
        ManagedDiagnosticLayer::ModelTransferOrIntegrity,
    )?;
    let mut child = managed_wsl_command()
        .args(["tee", remote])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                format!("could not open managed WSL transfer: {error}"),
            )
        })?;
    let mut source = File::open(local).map_err(model_io)?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            "managed WSL transfer did not expose stdin",
        ));
    };
    io::copy(&mut source, &mut stdin).map_err(model_io)?;
    drop(stdin);
    let output = child.wait_with_output().map_err(model_io)?;
    if !output.status.success() {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            format!(
                "managed WSL transfer failed: {}",
                command_output_text(&output)
            ),
        ));
    }
    Ok(())
}

fn wsl_model_path(content_sha256: &str) -> String {
    format!("/var/lib/gameengine/local-ai/models/{content_sha256}.gguf")
}

fn wsl_file_exists(path: &str) -> Result<bool, ManagedLocalRuntimeError> {
    let output = managed_wsl_command()
        .args(["test", "-f", path])
        .output()
        .map_err(model_io)?;
    Ok(output.status.success())
}

fn verify_wsl_sha256(path: &str, expected: &str) -> Result<(), ManagedLocalRuntimeError> {
    let output = wsl_shell(
        &format!("sha256sum {}", shell_quote(path)),
        ManagedDiagnosticLayer::ModelTransferOrIntegrity,
    )?;
    let actual = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ModelTransferOrIntegrity,
            format!("WSL copy SHA-256 mismatch: expected {expected}, observed {actual}"),
        ));
    }
    Ok(())
}

fn wsl_shell(
    command: &str,
    layer: ManagedDiagnosticLayer,
) -> Result<std::process::Output, ManagedLocalRuntimeError> {
    let mut child = managed_wsl_script_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(layer, format!("could not invoke managed WSL: {error}"))
        })?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(ManagedLocalRuntimeError::new(
            layer,
            "managed WSL shell did not expose stdin",
        ));
    };
    stdin.write_all(command.as_bytes()).map_err(|error| {
        ManagedLocalRuntimeError::new(
            layer,
            format!("could not stream managed WSL script: {error}"),
        )
    })?;
    stdin.write_all(b"\n").map_err(|error| {
        ManagedLocalRuntimeError::new(
            layer,
            format!("could not terminate managed WSL script: {error}"),
        )
    })?;
    drop(stdin);
    let output = child.wait_with_output().map_err(|error| {
        ManagedLocalRuntimeError::new(layer, format!("could not wait for managed WSL: {error}"))
    })?;
    if !output.status.success() {
        return Err(ManagedLocalRuntimeError::new(
            layer,
            format!(
                "managed WSL command failed: {}",
                command_output_text(&output)
            ),
        ));
    }
    Ok(output)
}

fn wsl_status() -> Result<WslStatus, ManagedLocalRuntimeError> {
    if !cfg!(target_os = "windows") {
        return Ok(WslStatus::Unavailable(
            "WSL2 is available only on Windows".to_owned(),
        ));
    }
    let status = Command::new("wsl.exe")
        .arg("--status")
        .output()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                format!("could not query WSL2 status: {error}"),
            )
        })?;
    if !status.status.success() {
        return Ok(classify_wsl_status(
            false,
            &command_output_text(&status),
            false,
            "",
        ));
    }
    let distributions = Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .output()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::OperatingSystemPrerequisite,
                format!("could not list WSL distributions: {error}"),
            )
        })?;
    if !distributions.status.success() {
        return Ok(classify_wsl_status(
            true,
            "",
            false,
            &command_output_text(&distributions),
        ));
    }
    let text = decode_windows_command_text(&distributions.stdout);
    Ok(classify_wsl_status(true, "", true, &text))
}

fn process_is_alive(state: &ManagedProcessState) -> Result<bool, ManagedLocalRuntimeError> {
    let status = match state.environment {
        ManagedExecutionEnvironment::WindowsNative => Command::new("tasklist.exe")
            .args([
                "/FI",
                &format!("PID eq {}", state.process_id),
                "/FO",
                "CSV",
                "/NH",
            ])
            .output(),
        ManagedExecutionEnvironment::Wsl2Linux => managed_wsl_command()
            .arg("kill")
            .arg("-0")
            .arg(state.process_id.to_string())
            .output(),
    }
    .map_err(|error| {
        ManagedLocalRuntimeError::new(
            ManagedDiagnosticLayer::ManagedProcessStartup,
            format!("could not inspect managed process state: {error}"),
        )
    })?;
    if state.environment == ManagedExecutionEnvironment::Wsl2Linux {
        return Ok(status.status.success());
    }
    let text = command_output_text(&status);
    Ok(status.status.success() && text.contains(&state.process_id.to_string()))
}

/// Decides whether a stalled release should escalate from a graceful `kill` to `kill -9`.
///
/// Only WSL2 escalates: Windows Native release already uses `taskkill.exe /F`, which is a
/// forced termination, so a second forceful attempt would be redundant.
fn managed_release_should_force_kill(
    environment: ManagedExecutionEnvironment,
    still_alive_after_graceful_kill: bool,
) -> bool {
    environment == ManagedExecutionEnvironment::Wsl2Linux && still_alive_after_graceful_kill
}

/// Polls `process_is_alive` until it reports exit or `timeout` elapses.
fn wait_while_process_alive(
    state: &ManagedProcessState,
    timeout: Duration,
) -> Result<(), ManagedLocalRuntimeError> {
    let started = Instant::now();
    while process_is_alive(state)? && started.elapsed() < timeout {
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn reserve_loopback_port() -> Result<u16, ManagedLocalRuntimeError> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).map_err(
        |error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                format!("could not reserve a loopback port for managed inference: {error}"),
            )
        },
    )?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| {
            ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                format!("could not inspect the reserved loopback port: {error}"),
            )
        })
}

fn endpoint_is_healthy(endpoint: &str) -> bool {
    let Some(authority) = endpoint.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    let Ok(port) = authority.parse::<u16>() else {
        return false;
    };
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, HEALTH_CONNECT_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(HEALTH_CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEALTH_CONNECT_TIMEOUT));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 128];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    let text = String::from_utf8_lossy(&response[..read]);
    text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200")
}

fn wsl_server_log_path(environment: ManagedExecutionEnvironment) -> String {
    format!(
        "{WSL_SERVER_LOG_ROOT}/llama-server-{}.log",
        environment.storage_key()
    )
}

const RECENT_LOG_TAIL_BYTES: u64 = 64 * 1024;

fn read_recent_local_log_lines(path: &Path, max_lines: usize) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(RECENT_LOG_TAIL_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if start > 0
        && let Some(newline) = bytes.iter().position(|byte| *byte == b'\n')
    {
        bytes.drain(..=newline);
    }
    Ok(recent_log_lines(&bytes, max_lines))
}

fn recent_log_lines(bytes: &[u8], max_lines: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.reverse();
    lines
}

fn wsl_server_launch_command(
    server_path: &str,
    model_path: &Path,
    projector_path: Option<&Path>,
    port: u16,
    environment: ManagedExecutionEnvironment,
    context_tokens: u32,
) -> String {
    let server = shell_quote(server_path);
    let model = shell_quote(&model_path.to_string_lossy());
    let projector = projector_path
        .map(|path| format!(" --mmproj {}", shell_quote(&path.to_string_lossy())))
        .unwrap_or_default();
    let log = wsl_server_log_path(environment);
    let quoted_log = shell_quote(&log);
    format!(
        "mkdir -p {}; nohup {server} --model {model} --host 127.0.0.1 --port {port} --fit on --fit-target {MANAGED_GPU_FIT_TARGET_MIB} --parallel {MANAGED_SERVER_PARALLEL_SLOTS} --ctx-size {context_tokens}{projector} > {quoted_log} 2>&1 < /dev/null & pid=$!; sleep {WSL_SERVER_LAUNCH_GRACE_SECONDS}; if ! kill -0 \"$pid\" 2>/dev/null; then printf 'managed llama-server exited during WSL startup; log: %s\\n' {quoted_log} >&2; tail -n 40 {quoted_log} >&2 2>/dev/null || true; exit 1; fi; echo \"$pid\"",
        shell_quote(WSL_SERVER_LOG_ROOT)
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)
}

fn runtime_io(error: io::Error) -> ManagedLocalRuntimeError {
    ManagedLocalRuntimeError::new(
        ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
        error.to_string(),
    )
}

fn model_io(error: io::Error) -> ManagedLocalRuntimeError {
    ManagedLocalRuntimeError::new(
        ManagedDiagnosticLayer::ModelTransferOrIntegrity,
        error.to_string(),
    )
}

fn command_output_text(output: &std::process::Output) -> String {
    let stdout = decode_windows_command_text(&output.stdout);
    let stderr = decode_windows_command_text(&output.stderr);
    let combined = format!("{} {}", stdout.trim(), stderr.trim());
    if combined.trim().is_empty() {
        format!("exit status {}", output.status)
    } else {
        combined.trim().to_owned()
    }
}

fn decode_windows_command_text(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.len().is_multiple_of(2) {
        let utf16 = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let candidate = String::from_utf16_lossy(&utf16);
        if candidate
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
        {
            return candidate;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn output_mentions_restart(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("restart") || text.contains("reboot")
}

fn now_unix_ms() -> u64 {
    system_time_unix_ms(SystemTime::now()).unwrap_or(0)
}

fn system_time_unix_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-managed-local-{name}-{}-{}",
            std::process::id(),
            now_unix_ms()
        ))
    }

    #[test]
    fn runtime_and_environment_identity_are_distinct_from_ollama() {
        assert_eq!(MANAGED_BACKEND_ID, "gameengine-managed-llama-cpp");
        assert_ne!(MANAGED_BACKEND_ID, "ollama-compatible");
        assert_ne!(
            ManagedExecutionEnvironment::WindowsNative.benchmark_id(),
            ManagedExecutionEnvironment::Wsl2Linux.benchmark_id()
        );
        assert_eq!(ManagedExecutionEnvironment::ALL.len(), 2);
    }

    #[test]
    fn managed_goose_installation_is_discoverable_and_integrity_checked() {
        let root = temp_root("goose-installation");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let final_root = manager
            .goose_runtime_root()
            .join(format!("v{PINNED_GOOSE_VERSION}"));
        let executable = final_root.join(PINNED_GOOSE_EXECUTABLE_RELATIVE_PATH);
        fs::create_dir_all(executable.parent().expect("executable parent")).expect("goose parent");
        fs::write(&executable, b"managed-goose-fixture").expect("goose fixture");
        let executable_sha256 = sha256_via_platform(&executable).expect("fixture sha256");
        let retained_artifact = final_root
            .join("artifacts")
            .join(PINNED_GOOSE_WINDOWS_ASSET);
        fs::create_dir_all(retained_artifact.parent().expect("artifact parent"))
            .expect("artifact directory");
        fs::write(&retained_artifact, b"managed-goose-archive-fixture")
            .expect("retained Goose fixture");
        let installation = ManagedGooseInstallation {
            schema_version: GOOSE_RUNTIME_STATE_SCHEMA_VERSION,
            version: PINNED_GOOSE_VERSION.to_owned(),
            asset_name: PINNED_GOOSE_WINDOWS_ASSET.to_owned(),
            asset_sha256: PINNED_GOOSE_WINDOWS_SHA256.to_owned(),
            executable_sha256,
            installed_unix_ms: now_unix_ms(),
            executable_path: executable.clone(),
            retained_artifact_path: retained_artifact.clone(),
        };
        write_json(&manager.goose_active_path(), &installation).expect("active Goose state");
        assert_eq!(
            manager.managed_goose_setup_status(),
            ManagedGooseSetupStatus::Ready
        );
        assert_eq!(
            manager
                .managed_goose_executable()
                .expect("managed Goose lookup"),
            Some(executable.clone())
        );
        fs::write(&executable, b"corrupt").expect("corrupt fixture");
        assert!(manager.managed_goose_executable().is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_goose_pin_names_the_immutable_patched_release() {
        assert_eq!(PINNED_GOOSE_VERSION, "1.45.0+gameengine.ge-midturn-2");
        assert_eq!(
            PINNED_GOOSE_WINDOWS_ASSET,
            "gameengine-managed-goose-v1.45.0-ge-midturn-2-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            PINNED_GOOSE_WINDOWS_SHA256,
            "b9ab2de08972b3cee38b3262a78702726fc1d5d87ffbccb9c166f208cbc0444a"
        );
        assert_eq!(
            PINNED_GOOSE_WINDOWS_URL,
            "https://github.com/KdGithubIt/GameEngine/releases/download/managed-goose-v1.45.0-ge-midturn-2/gameengine-managed-goose-v1.45.0-ge-midturn-2-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(PINNED_GOOSE_EXECUTABLE_RELATIVE_PATH, "goose.exe");
    }

    #[test]
    fn machine_local_goose_override_round_trips_without_environment_variables() {
        let root = temp_root("goose-override");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let executable = root.join("custom-goose.exe");
        fs::create_dir_all(&root).expect("fixture root");
        fs::write(&executable, b"custom-goose-fixture").expect("override fixture");
        manager
            .set_goose_executable_override(Some(executable.clone()))
            .expect("save override");
        assert_eq!(
            manager.goose_executable_override().expect("read override"),
            Some(executable)
        );
        manager
            .set_goose_executable_override(None)
            .expect("clear override");
        assert_eq!(
            manager
                .goose_executable_override()
                .expect("read cleared override"),
            None
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_digest_is_rejected_before_comparison() {
        assert!(!is_sha256_hex("abc"));
        assert!(is_sha256_hex(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn exact_download_and_run_approval_cannot_expand_to_future_candidate() {
        let plan = ManagedAcquisitionPlan {
            plan_id: "campaign-1".to_owned(),
            candidates: vec![ManagedAcquisitionCandidate {
                candidate_id: "model-a-q4".to_owned(),
                source: "https://example.invalid/model-a.gguf".to_owned(),
                representation: "Q4_K_M".to_owned(),
                license: Some("test-license".to_owned()),
                expected_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_owned(),
                transfer_bytes: 10,
                storage_bytes: 12,
            }],
        };
        let approval = plan.approve_exact();
        assert!(approval.authorizes("campaign-1", "model-a-q4").is_ok());
        assert!(approval.authorizes("campaign-1", "model-b-q4").is_err());
        assert!(approval.authorizes("campaign-2", "model-a-q4").is_err());
    }

    #[test]
    fn acquisition_review_reports_aggregate_transfer_and_storage() {
        let plan = ManagedAcquisitionPlan {
            plan_id: "campaign".to_owned(),
            candidates: vec![
                ManagedAcquisitionCandidate {
                    candidate_id: "a".to_owned(),
                    source: "a".to_owned(),
                    representation: "q4".to_owned(),
                    license: None,
                    expected_sha256: "0".repeat(64),
                    transfer_bytes: 10,
                    storage_bytes: 12,
                },
                ManagedAcquisitionCandidate {
                    candidate_id: "b".to_owned(),
                    source: "b".to_owned(),
                    representation: "q5".to_owned(),
                    license: None,
                    expected_sha256: "1".repeat(64),
                    transfer_bytes: 20,
                    storage_bytes: 25,
                },
            ],
        };
        assert_eq!(
            plan.review(),
            ManagedAcquisitionReview {
                candidate_count: 2,
                total_transfer_bytes: 30,
                total_storage_bytes: 37,
            }
        );
    }

    #[test]
    fn existing_gguf_registration_does_not_modify_bytes() {
        let root = temp_root("register");
        let model = root.join("sample-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model, Some(15), &[12, 12, 14]).expect("model fixture");
        let before = fs::read(&model).expect("before bytes");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model, None)
            .expect("register existing GGUF");
        let after = fs::read(&model).expect("after bytes");
        assert_eq!(before, after);
        assert_eq!(registration.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(
            registration.exact_representation(),
            Some("gguf-repr-v1;gguf=3;file_type=15;quantization_version=2;types=Q4_K:2,Q6_K:1")
        );
        assert!(registration.model_id.starts_with("gguf:"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn registration_representation_is_filename_independent() {
        let root = temp_root("representation-name");
        fs::create_dir_all(&root).expect("temp root");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let names = [
            "named-Q4_K_M.gguf",
            "no-marker.gguf",
            "qwen3.8-27b-abliterated-3.69bpw.gguf",
        ];
        let mut registrations = Vec::new();
        for name in names {
            let path = root.join(name);
            gguf::write_test_gguf(&path, Some(15), &[12, 12, 14]).expect("model fixture");
            registrations.push(
                manager
                    .register_existing_gguf(&path, None)
                    .expect("register GGUF"),
            );
        }
        for registration in registrations.iter().skip(1) {
            assert_eq!(registration.content_sha256, registrations[0].content_sha256);
            assert_eq!(registration.representation, registrations[0].representation);
        }
        assert!(registrations[2].has_exact_representation_identity());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn filename_quantization_marker_cannot_override_gguf_metadata() {
        let root = temp_root("representation-lie");
        let model = root.join("actually-q6-fake-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model, Some(18), &[14, 14, 0]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model, None)
            .expect("register GGUF");
        assert_eq!(registration.quantization.as_deref(), Some("Q6_K"));
        let representation = registration
            .exact_representation()
            .expect("exact representation");
        assert!(representation.contains("file_type=18"));
        assert!(representation.contains("types=F32:1,Q6_K:2"));
        assert!(!representation.contains("Q4_K_M"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_registry_deserializes_without_claiming_exact_representation() {
        let registry: ManagedModelRegistry = serde_json::from_value(serde_json::json!({
            "models": [{
                "model_id": "gguf:legacy",
                "display_name": "legacy-Q4_K_M.gguf",
                "content_sha256": "a".repeat(64),
                "source_path": "C:/models/legacy-Q4_K_M.gguf",
                "size_bytes": 1024,
                "modified_unix_ms": 1,
                "quantization": "Q4_K_M",
                "source": null,
                "license": null
            }]
        }))
        .expect("legacy registry");
        let model = &registry.models[0];
        assert_eq!(model.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(model.representation, None);
        assert!(!model.has_exact_representation_identity());
    }

    /// Rewrites a registry into the shape an older GameEngine build persisted.
    fn strip_measured_representation(path: &Path) {
        let bytes = fs::read(path).expect("registry bytes");
        let mut registry: serde_json::Value =
            serde_json::from_slice(&bytes).expect("registry document");
        for model in registry["models"].as_array_mut().expect("registry models") {
            let model = model.as_object_mut().expect("registry model");
            model.remove("representation");
            model["quantization"] = serde_json::Value::Null;
        }
        fs::write(path, serde_json::to_vec(&registry).expect("registry bytes"))
            .expect("write registry");
    }

    #[test]
    fn legacy_registration_is_remeasured_from_its_registered_gguf() {
        let root = temp_root("legacy-remeasure");
        let model = root.join("legacy-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model, Some(15), &[12, 12, 14]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model, None)
            .expect("register GGUF");
        strip_measured_representation(&manager.model_registry_path());

        let models = manager.registered_models().expect("registered models");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].representation, registration.representation);
        assert_eq!(models[0].quantization, registration.quantization);
        assert!(models[0].has_exact_representation_identity());
        let persisted: ManagedModelRegistry = read_optional_json(&manager.model_registry_path())
            .expect("registry")
            .expect("registry document");
        assert_eq!(
            persisted.models[0].representation, registration.representation,
            "a remeasured registry must be persisted instead of measured on every read"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_registration_whose_file_no_longer_matches_stays_unmeasured() {
        let root = temp_root("legacy-drift");
        let model = root.join("drifted-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model, Some(15), &[12, 12, 14]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        manager
            .register_existing_gguf(&model, None)
            .expect("register GGUF");
        let registry_path = manager.model_registry_path();
        strip_measured_representation(&registry_path);
        let mut registry: serde_json::Value =
            serde_json::from_slice(&fs::read(&registry_path).expect("registry bytes"))
                .expect("registry document");
        registry["models"][0]["size_bytes"] = serde_json::json!(1);
        fs::write(
            &registry_path,
            serde_json::to_vec(&registry).expect("registry bytes"),
        )
        .expect("write registry");
        let before = fs::read(&registry_path).expect("registry bytes");

        let models = manager.registered_models().expect("registered models");

        assert_eq!(models[0].representation, None);
        assert!(!models[0].has_exact_representation_identity());
        assert_eq!(
            fs::read(&registry_path).expect("registry bytes"),
            before,
            "a record that cannot be remeasured must not be rewritten"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_registration_without_a_recorded_modification_time_stays_unmeasured() {
        let root = temp_root("legacy-untimed");
        let model = root.join("untimed-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model, Some(15), &[12, 12, 14]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        manager
            .register_existing_gguf(&model, None)
            .expect("register GGUF");
        let registry_path = manager.model_registry_path();
        strip_measured_representation(&registry_path);
        let mut registry: serde_json::Value =
            serde_json::from_slice(&fs::read(&registry_path).expect("registry bytes"))
                .expect("registry document");
        registry["models"][0]["modified_unix_ms"] = serde_json::Value::Null;
        fs::write(
            &registry_path,
            serde_json::to_vec(&registry).expect("registry bytes"),
        )
        .expect("write registry");

        let models = manager.registered_models().expect("registered models");

        assert_eq!(models[0].representation, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restart_continuation_is_machine_local_state() {
        let root = temp_root("restart");
        let manager = ManagedLocalRuntime::open(root.clone()).expect("manager");
        let marker = manager.restart_marker_path();

        manager.mark_restart_required().expect("mark restart");
        assert!(marker.is_file(), "continuation marker must be persisted");
        assert!(
            marker.starts_with(&root),
            "continuation marker must live under machine-local runtime state"
        );

        // While the continuation is unsatisfied, setup stays blocked and the
        // marker survives, independently of the host WSL state.
        assert_eq!(
            manager
                .setup_status_with_continuation(ManagedExecutionEnvironment::WindowsNative, || {
                    false
                }),
            ManagedSetupStatus::RestartRequired
        );
        assert!(marker.is_file());

        // Once the continuation is satisfied the marker is consumed and setup
        // resumes the ordinary installation probe.
        assert_ne!(
            manager
                .setup_status_with_continuation(ManagedExecutionEnvironment::WindowsNative, || {
                    true
                }),
            ManagedSetupStatus::RestartRequired
        );
        assert!(!marker.exists());

        manager.mark_restart_required().expect("mark restart again");
        manager.clear_restart_required().expect("clear restart");
        assert!(!marker.exists());
        manager
            .clear_restart_required()
            .expect("clear is idempotent");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loopback_port_reservation_never_selects_wildcard_address() {
        let port = reserve_loopback_port().expect("loopback port");
        assert_ne!(port, 0);
        let endpoint = format!("http://127.0.0.1:{port}");
        assert!(endpoint.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn pinned_cuda_runtime_plan_uses_two_windows_assets_and_wsl_source_build() {
        assert_eq!(
            windows_cuda_asset_names(),
            [
                "llama-b10336-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-12.4-x64.zip",
            ]
        );
        assert_eq!(
            WINDOWS_CUDA_MANIFEST,
            "llama-b10336-win-cuda-12.4-manifest.txt"
        );
        assert_eq!(
            WSL_CUDA_MANIFEST,
            "llama-b10336-wsl-cuda-12.4-source-manifest.txt"
        );
        assert_eq!(WSL_CUDA_COMPILER_PACKAGE, "cuda-compiler-12-4");
        assert_eq!(WSL_CUDA_LIBRARIES_DEV_PACKAGE, "cuda-libraries-dev-12-4");
        assert_eq!(STATE_SCHEMA_VERSION, 2);
        assert_eq!(MANAGED_WSL_DISTRIBUTION, "GameEngine-LocalAI");
        assert_eq!(MANAGED_WSL_BASE_DISTRIBUTION, "Ubuntu-22.04");
        assert_eq!(MANAGED_WSL_EXPECTED_VERSION_ID, "22.04");

        let windows_manifest = windows_cuda_runtime_manifest(&[
            (WINDOWS_CUDA_RUNTIME_ASSET.to_owned(), "a".repeat(64)),
            (WINDOWS_CUDA_SUPPORT_ASSET.to_owned(), "b".repeat(64)),
        ]);
        assert!(windows_manifest.contains("environment=windows_native"));
        assert!(windows_manifest.contains("backend=cuda"));
        assert!(windows_manifest.contains(WINDOWS_CUDA_RUNTIME_ASSET));
        assert!(windows_manifest.contains(WINDOWS_CUDA_SUPPORT_ASSET));

        let wsl_manifest = wsl_cuda_runtime_manifest(&WslCudaBuildProvenance {
            revision: format!("{PINNED_LLAMA_CPP_REVISION}deadbeef"),
            compiler_version: "12.4.131-1".to_owned(),
            libraries_dev_version: "12.4.1-1".to_owned(),
            server_sha256: "c".repeat(64),
            bench_sha256: "d".repeat(64),
        });
        assert!(wsl_manifest.contains("environment=wsl2_linux"));
        assert!(wsl_manifest.contains("backend=cuda"));
        assert!(wsl_manifest.contains("cuda_compiler_package=cuda-compiler-12-4"));
        assert!(wsl_manifest.contains("cuda_libraries_dev_package=cuda-libraries-dev-12-4"));
        assert!(wsl_manifest.contains("source_revision=f401bb1deadbeef"));
    }

    #[test]
    fn os_release_parser_accepts_managed_ubuntu_22_04_point_release() {
        let os_release = "PRETTY_NAME=\"Ubuntu 22.04.5 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\nID=ubuntu\n";
        assert_eq!(os_release_value(os_release, "ID"), Some("ubuntu"));
        assert_eq!(os_release_value(os_release, "VERSION_ID"), Some("22.04"));
    }

    #[test]
    fn wsl_cuda_shell_contract_is_posix_safe_and_relocatable() {
        let bootstrap = wsl_cuda_bootstrap_command();
        assert!(!bootstrap.contains("/etc/os-release"));
        assert!(!bootstrap.contains("VERSION_ID"));
        assert!(!bootstrap.contains("Ubuntu 24.04"));
        assert!(bootstrap.contains("cuda-compiler-12-4"));
        assert!(bootstrap.contains("cuda-libraries-dev-12-4"));
        assert!(bootstrap.contains("grep -qx 'install ok installed'"));
        assert!(!bootstrap.contains("grep -c"));
        assert!(!bootstrap.contains(" -ne "));
        assert!(!bootstrap.contains("cuda-toolkit-12-4"));

        let build = wsl_cuda_build_command();
        assert!(build.contains("-DCMAKE_INSTALL_RPATH='/usr/local/cuda-12.4/lib64;$ORIGIN'"));
        assert!(build.contains("-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON"));
        assert!(build.contains("trap 'status=$?"));
        assert!(!build.contains("} >\"$log\" 2>&1 ||"));
        let source_removed = build.find("rm -rf \"$src\";").expect("source removal");
        let staged_runtime_probe = build
            .find("\"$stage/llama-server\" --version")
            .expect("relocated server probe");
        assert!(source_removed < staged_runtime_probe);
    }

    #[test]
    fn managed_wsl_commands_bypass_first_launch_user_setup_as_root() {
        let command = managed_wsl_command();
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-d".to_owned(),
                MANAGED_WSL_DISTRIBUTION.to_owned(),
                "-u".to_owned(),
                "root".to_owned(),
                "--".to_owned(),
            ]
        );

        let script_command = managed_wsl_script_command();
        let script_args = script_command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            script_args,
            vec![
                "-d".to_owned(),
                MANAGED_WSL_DISTRIBUTION.to_owned(),
                "-u".to_owned(),
                "root".to_owned(),
                "--".to_owned(),
                "sh".to_owned(),
                "-s".to_owned(),
            ]
        );
        assert!(!script_args.iter().any(|argument| argument == "-lc"));
    }

    #[test]
    fn context_window_follows_declared_model_shape_within_device_memory() {
        let capability = GgufModelCapability {
            architecture: Some("any-architecture".to_owned()),
            train_context_tokens: Some(131_072),
            kv_cache_bytes_per_token: Some(16_384),
            sliding_window_tokens: None,
            chat_template: true,
        };
        // A 12 GiB device budgets 3 GiB of KV cache, which holds 196_608 tokens at this cost,
        // so the declared window is the binding constraint and the ceiling clamps it.
        assert_eq!(
            resolve_managed_context_tokens(&capability, Some(12 * 1024 * 1024 * 1024)),
            MANAGED_CONTEXT_CEILING_TOKENS
        );
    }

    #[test]
    fn a_costly_model_is_bounded_by_measured_device_memory_not_by_its_declared_window() {
        let capability = GgufModelCapability {
            architecture: Some("dense".to_owned()),
            train_context_tokens: Some(32_768),
            kv_cache_bytes_per_token: Some(163_840),
            sliding_window_tokens: None,
            chat_template: true,
        };
        // 3 GiB of KV budget divided by 160 KiB per token leaves 19_660 tokens, aligned down.
        assert_eq!(
            resolve_managed_context_tokens(&capability, Some(12 * 1024 * 1024 * 1024)),
            19_456
        );
    }

    #[test]
    fn consumer_context_requirement_rejects_without_expanding_the_physical_plan() {
        let requirement = ManagedContextRequirement::new(32_768);
        assert_eq!(requirement.minimum_tokens(), 32_768);
        assert!(!requirement.admits(19_456));
        assert!(requirement.admits(32_768));
    }

    #[test]
    fn a_small_declared_window_never_drops_below_the_protocol_floor() {
        let capability = GgufModelCapability {
            architecture: Some("small".to_owned()),
            train_context_tokens: Some(2_048),
            kv_cache_bytes_per_token: Some(1_024),
            sliding_window_tokens: None,
            chat_template: false,
        };
        assert_eq!(
            resolve_managed_context_tokens(&capability, Some(12 * 1024 * 1024 * 1024)),
            MANAGED_CONTEXT_FLOOR_TOKENS
        );
    }

    #[test]
    fn an_unmeasured_model_keeps_the_previously_measured_default() {
        assert_eq!(
            resolve_managed_context_tokens(&GgufModelCapability::default(), None),
            MANAGED_CONTEXT_UNMEASURED_TOKENS
        );
        assert_eq!(
            resolve_managed_context_tokens(
                &GgufModelCapability::default(),
                Some(12 * 1024 * 1024 * 1024)
            ),
            MANAGED_CONTEXT_UNMEASURED_TOKENS
        );
    }

    #[test]
    fn an_unmeasured_device_still_follows_what_the_model_declares() {
        let capability = GgufModelCapability {
            architecture: Some("any-architecture".to_owned()),
            train_context_tokens: Some(16_384),
            kv_cache_bytes_per_token: Some(16_384),
            sliding_window_tokens: None,
            chat_template: true,
        };
        assert_eq!(resolve_managed_context_tokens(&capability, None), 16_384);
    }

    #[test]
    fn registering_a_gguf_measures_the_launch_shape_it_declares() {
        let state_root = temp_root("capability-registration");
        let _ = fs::remove_dir_all(&state_root);
        let manager = ManagedLocalRuntime::open(state_root.clone()).expect("runtime state");
        let model = state_root.join("declared.gguf");
        fs::create_dir_all(&state_root).expect("state root");
        gguf::write_test_gguf_with_architecture(
            &model,
            "any-architecture",
            &[
                (".context_length", 16_384),
                (".block_count", 4),
                (".embedding_length", 512),
                (".attention.head_count", 8),
                (".attention.head_count_kv", 2),
            ],
        )
        .expect("model fixture");
        let registration = manager
            .register_existing_gguf(&model, None)
            .expect("registration");
        assert_eq!(registration.capability.train_context_tokens, Some(16_384));
        assert_eq!(
            registration.capability.kv_cache_bytes_per_token,
            Some(2_048)
        );
        let reloaded = manager.registered_models().expect("registry");
        assert_eq!(reloaded[0].capability, registration.capability);
        let _ = fs::remove_dir_all(&state_root);
    }

    #[test]
    fn a_registered_projector_reaches_the_launch_command_for_both_environments() {
        let model = Path::new(r"C:\\models\\sample-Q4_K_M.gguf");
        let projector = Path::new(r"C:\\models\\sample-mmproj.gguf");
        let arguments = windows_server_arguments(model, Some(projector), 18443, 12_288);
        let mmproj = arguments
            .iter()
            .position(|argument| argument == "--mmproj")
            .expect("projector flag");
        assert_eq!(
            arguments[mmproj + 1],
            projector.to_string_lossy().into_owned()
        );

        let command = wsl_server_launch_command(
            "/var/lib/gameengine/local-ai/runtime/b10336/llama-server",
            Path::new("/var/lib/gameengine/local-ai/models/model.gguf"),
            Some(Path::new(
                "/var/lib/gameengine/local-ai/models/projector.gguf",
            )),
            18443,
            ManagedExecutionEnvironment::Wsl2Linux,
            12_288,
        );
        assert!(
            command.contains("--mmproj '/var/lib/gameengine/local-ai/models/projector.gguf'"),
            "{command}"
        );
    }

    #[test]
    fn a_text_only_model_launches_without_a_projector_flag() {
        let model = Path::new(r"C:\\models\\sample-Q4_K_M.gguf");
        let arguments = windows_server_arguments(model, None, 18443, 12_288);
        assert!(!arguments.iter().any(|argument| argument == "--mmproj"));
        let command = wsl_server_launch_command(
            "/server",
            Path::new("/model.gguf"),
            None,
            18443,
            ManagedExecutionEnvironment::Wsl2Linux,
            12_288,
        );
        assert!(!command.contains("--mmproj"));
    }

    #[test]
    fn registering_and_removing_a_projector_changes_only_that_model() {
        let state_root = temp_root("projector-registration");
        let _ = fs::remove_dir_all(&state_root);
        let manager = ManagedLocalRuntime::open(state_root.clone()).expect("runtime state");
        fs::create_dir_all(&state_root).expect("state root");
        let model_path = state_root.join("model.gguf");
        gguf::write_test_gguf(&model_path, Some(15), &[12, 12, 14]).expect("model fixture");
        let projector_path = state_root.join("projector.gguf");
        gguf::write_test_gguf(&projector_path, Some(15), &[12]).expect("projector fixture");
        let registration = manager
            .register_existing_gguf(&model_path, None)
            .expect("registration");
        assert!(registration.projector.is_none());

        let with_projector = manager
            .register_projector(&registration.model_id, &projector_path)
            .expect("projector registration");
        let recorded = with_projector.projector.expect("projector record");
        assert!(is_sha256_hex(&recorded.content_sha256));
        assert_eq!(
            manager.registered_models().expect("registry")[0]
                .projector
                .as_ref()
                .map(|projector| projector.content_sha256.clone()),
            Some(recorded.content_sha256)
        );

        let removed = manager
            .remove_projector(&registration.model_id)
            .expect("projector removal");
        assert!(removed.projector.is_none());
        let _ = fs::remove_dir_all(&state_root);
    }

    #[test]
    fn registering_a_projector_for_an_unknown_model_is_refused() {
        let state_root = temp_root("projector-unknown-model");
        let _ = fs::remove_dir_all(&state_root);
        let manager = ManagedLocalRuntime::open(state_root.clone()).expect("runtime state");
        fs::create_dir_all(&state_root).expect("state root");
        let projector_path = state_root.join("projector.gguf");
        gguf::write_test_gguf(&projector_path, Some(15), &[12]).expect("projector fixture");
        assert!(
            manager
                .register_projector("gguf:missing", &projector_path)
                .is_err()
        );
        let _ = fs::remove_dir_all(&state_root);
    }

    #[test]
    fn windows_launch_uses_the_exact_managed_physical_context() {
        let model = Path::new(r"C:\\models\\sample-Q4_K_M.gguf");
        let arguments = windows_server_arguments(model, None, 18443, 8_192);
        let context_flag = arguments
            .iter()
            .position(|argument| argument == "--ctx-size")
            .expect("managed context flag");
        assert_eq!(arguments.get(context_flag + 1), Some(&"8192".to_owned()));
    }

    #[test]
    fn windows_launch_contract_leaves_gpu_layers_to_memory_fitter() {
        let model = Path::new(r"C:\\models\\sample-Q4_K_M.gguf");
        let arguments = windows_server_arguments(model, None, 18443, 12_288);
        assert_eq!(
            arguments,
            vec![
                "--model".to_owned(),
                model.to_string_lossy().into_owned(),
                "--host".to_owned(),
                "127.0.0.1".to_owned(),
                "--port".to_owned(),
                "18443".to_owned(),
                "--fit".to_owned(),
                "on".to_owned(),
                "--fit-target".to_owned(),
                "1024".to_owned(),
                "--parallel".to_owned(),
                "1".to_owned(),
                "--ctx-size".to_owned(),
                "12288".to_owned(),
            ]
        );
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "--n-gpu-layers")
        );
    }

    #[test]
    fn wsl_status_classification_fails_closed_and_requires_exact_managed_distribution() {
        assert_eq!(
            classify_wsl_status(false, "WSL unavailable", false, ""),
            WslStatus::Unavailable("WSL unavailable".to_owned())
        );
        assert_eq!(
            classify_wsl_status(true, "", false, "distribution list failed"),
            WslStatus::Unavailable("distribution list failed".to_owned())
        );
        assert_eq!(
            classify_wsl_status(true, "", true, "Ubuntu-24.04\nGameEngine-LocalAI\n"),
            WslStatus::Available {
                managed_distribution: true,
            }
        );
        assert_eq!(
            classify_wsl_status(true, "", true, "GameEngine-LocalAI-copy\n"),
            WslStatus::Available {
                managed_distribution: false,
            }
        );
    }

    #[test]
    fn recent_log_lines_keep_only_the_newest_non_empty_entries() {
        assert_eq!(
            recent_log_lines(b"one\n\ntwo\nthree\n", 2),
            vec!["two".to_owned(), "three".to_owned()]
        );
    }

    #[test]
    fn managed_log_path_matches_execution_environment() {
        let manager = ManagedLocalRuntime {
            root: PathBuf::from("state-root"),
        };
        assert_eq!(
            manager.log_path(ManagedExecutionEnvironment::WindowsNative),
            PathBuf::from("state-root")
                .join("logs")
                .join("llama-server-windows_native.log")
        );
        assert_eq!(
            manager.log_path(ManagedExecutionEnvironment::Wsl2Linux),
            PathBuf::from("/var/lib/gameengine/local-ai/logs/llama-server-wsl2_linux.log")
        );
    }

    #[test]
    fn wsl_launch_contract_holds_session_until_process_survives_grace() {
        let command = wsl_server_launch_command(
            "/var/lib/gameengine/local-ai/runtime/b10336/llama-server",
            Path::new("/var/lib/gameengine/local-ai/models/model.gguf"),
            None,
            18443,
            ManagedExecutionEnvironment::Wsl2Linux,
            12_288,
        );
        let pid_capture = command.find("pid=$!").expect("PID capture");
        let grace = command.find("sleep 1").expect("WSL launch grace");
        let alive_check = command
            .find("kill -0 \"$pid\"")
            .expect("post-grace liveness check");
        let pid_report = command.rfind("echo \"$pid\"").expect("PID report");
        assert!(pid_capture < grace);
        assert!(grace < alive_check);
        assert!(alive_check < pid_report);
        assert!(command.contains("nohup"));
        assert!(command.contains(
            "tail -n 40 '/var/lib/gameengine/local-ai/logs/llama-server-wsl2_linux.log'"
        ));
    }

    #[test]
    fn startup_contract_never_treats_a_crashed_process_as_healthy() {
        assert!(!managed_process_exited_before_health(true, false));
        assert!(!managed_process_exited_before_health(false, true));
        assert!(managed_process_exited_before_health(false, false));
    }

    #[test]
    fn release_only_escalates_to_force_kill_on_wsl2_after_a_stalled_graceful_kill() {
        assert!(managed_release_should_force_kill(
            ManagedExecutionEnvironment::Wsl2Linux,
            true
        ));
        assert!(!managed_release_should_force_kill(
            ManagedExecutionEnvironment::Wsl2Linux,
            false
        ));
        assert!(!managed_release_should_force_kill(
            ManagedExecutionEnvironment::WindowsNative,
            true
        ));
        assert!(!managed_release_should_force_kill(
            ManagedExecutionEnvironment::WindowsNative,
            false
        ));
    }

    #[test]
    fn benchmark_runtime_identity_includes_revision_environment_and_artifact_digest() {
        let config = ManagedLocalModelConfig {
            state_root: PathBuf::from("state"),
            environment: ManagedExecutionEnvironment::WindowsNative,
            model_id: "gguf:test".to_owned(),
            model_content_sha256: "a".repeat(64),
            model_path: PathBuf::from("model.gguf"),
            model_size_bytes: 1,
            quantization: Some("Q4_K_M".to_owned()),
            capability: GgufModelCapability::default(),
            model_representation: Some(
                "gguf-repr-v1;gguf=3;file_type=15;quantization_version=2;types=Q4_K:1".to_owned(),
            ),
            projector_path: None,
            runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
            runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
            runtime_artifact_sha256: "b".repeat(64),
            runtime_compatibility_version: MANAGED_RUNTIME_COMPATIBILITY_VERSION.to_owned(),
        };
        let identity = config.benchmark_runtime_identity();
        assert!(identity.contains(PINNED_LLAMA_CPP_REVISION));
        assert!(identity.contains("env=windows_native"));
        assert!(identity.contains(&"b".repeat(64)));
    }

    fn write_active_installation(root: &Path, artifact: &Path) -> ManagedRuntimeInstallation {
        let installation = ManagedRuntimeInstallation {
            schema_version: STATE_SCHEMA_VERSION,
            runtime_family: "llama.cpp".to_owned(),
            runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
            runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
            environment: ManagedExecutionEnvironment::WindowsNative,
            artifact_name: WINDOWS_CUDA_MANIFEST.to_owned(),
            artifact_sha256: "f".repeat(64),
            installed_unix_ms: 1,
            compatibility_version: MANAGED_RUNTIME_COMPATIBILITY_VERSION.to_owned(),
            server_path: "llama-server.exe".to_owned(),
            retained_artifact_path: artifact.to_path_buf(),
        };
        let environment_root = root.join("runtime").join("windows_native");
        fs::create_dir_all(&environment_root).expect("environment root");
        write_json(&environment_root.join("active.json"), &installation).expect("active pointer");
        installation
    }

    #[test]
    fn presentation_configuration_skips_runtime_artifact_verification() {
        let root = temp_root("describe-runtime");
        let model_source = root.join("sample-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model_source, Some(15), &[12, 14]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model_source, None)
            .expect("register existing GGUF");
        let artifact = root.join("state").join("runtime-manifest.txt");
        fs::create_dir_all(root.join("state")).expect("state root");
        fs::write(&artifact, b"not-the-pinned-manifest").expect("artifact fixture");
        write_active_installation(&root.join("state"), &artifact);

        // The enforced form hashes the retained artifact, so the deliberately
        // mismatched digest must fail it.
        assert!(
            manager
                .configuration_for(
                    &registration.model_id,
                    ManagedExecutionEnvironment::WindowsNative,
                    ManagedIntegrityCheck::Enforced,
                )
                .is_err()
        );
        let described = manager
            .configuration_for(
                &registration.model_id,
                ManagedExecutionEnvironment::WindowsNative,
                ManagedIntegrityCheck::Skipped,
            )
            .expect("presentation configuration resolves without verification");
        assert_eq!(described.model_content_sha256, registration.content_sha256);
        assert_eq!(described.model_path, registration.source_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn presentation_configuration_skips_registered_model_verification() {
        let root = temp_root("describe-model");
        let model_source = root.join("sample-Q4_K_M.gguf");
        fs::create_dir_all(&root).expect("temp root");
        gguf::write_test_gguf(&model_source, Some(15), &[12, 14]).expect("model fixture");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model_source, None)
            .expect("register existing GGUF");
        let artifact = root.join("state").join("runtime-manifest.txt");
        fs::create_dir_all(root.join("state")).expect("state root");
        fs::write(&artifact, b"not-the-pinned-manifest").expect("artifact fixture");
        write_active_installation(&root.join("state"), &artifact);
        fs::write(&model_source, b"tampered").expect("tamper the registered source");

        assert!(manager.verify_registered_model(&registration).is_err());
        assert!(
            manager
                .configuration_for(
                    &registration.model_id,
                    ManagedExecutionEnvironment::WindowsNative,
                    ManagedIntegrityCheck::Skipped,
                )
                .is_ok()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rollback_pointer_is_not_rewritten_by_failed_digest_verification() {
        let root = temp_root("rollback");
        let manager = ManagedLocalRuntime::open(root.clone()).expect("manager");
        let environment_root = root.join("runtime").join("windows_native");
        fs::create_dir_all(&environment_root).expect("environment root");
        let artifact = environment_root.join("old.zip");
        fs::write(&artifact, b"old-good-runtime").expect("old artifact");
        let installation = ManagedRuntimeInstallation {
            schema_version: STATE_SCHEMA_VERSION,
            runtime_family: "llama.cpp".to_owned(),
            runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
            runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
            environment: ManagedExecutionEnvironment::WindowsNative,
            artifact_name: "old.zip".to_owned(),
            artifact_sha256: "f".repeat(64),
            installed_unix_ms: 1,
            compatibility_version: MANAGED_RUNTIME_COMPATIBILITY_VERSION.to_owned(),
            server_path: "llama-server.exe".to_owned(),
            retained_artifact_path: artifact,
        };
        write_json(&environment_root.join("active.json"), &installation).expect("active pointer");
        let before = fs::read(environment_root.join("active.json")).expect("before active");
        assert!(
            manager
                .verify_retained_runtime_artifact(&installation)
                .is_err()
        );
        let after = fs::read(environment_root.join("active.json")).expect("after active");
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(root);
    }
}
