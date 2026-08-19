//! GameEngine-managed local inference runtime lifecycle for ADR 0155.
//!
//! This module owns machine-local llama.cpp installation, model registration,
//! Windows/WSL execution-environment setup, and demand-driven loopback process
//! lifecycle. It deliberately has no authoring or egui dependency.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const MANAGED_BACKEND_ID: &str = "gameengine-managed-llama-cpp";
pub(crate) const PINNED_LLAMA_CPP_TAG: &str = "b10336";
pub(crate) const PINNED_LLAMA_CPP_REVISION: &str = "f401bb1";
pub(crate) const MANAGED_WSL_DISTRIBUTION: &str = "GameEngine-LocalAI";
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
pub(crate) struct ManagedModelRegistration {
    pub(crate) model_id: String,
    pub(crate) display_name: String,
    pub(crate) content_sha256: String,
    pub(crate) source_path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) modified_unix_ms: Option<u64>,
    pub(crate) quantization: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) license: Option<String>,
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
    pub(crate) runtime_tag: String,
    pub(crate) runtime_revision: String,
    pub(crate) runtime_artifact_sha256: String,
    pub(crate) runtime_compatibility_version: String,
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
            total_transfer_bytes: self
                .candidates
                .iter()
                .fold(0_u64, |total, candidate| total.saturating_add(candidate.transfer_bytes)),
            total_storage_bytes: self
                .candidates
                .iter()
                .fold(0_u64, |total, candidate| total.saturating_add(candidate.storage_bytes)),
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
    ProvisionWsl,
    RegisterModel(PathBuf),
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
                    ManagedSetupOperation::ProvisionWsl => manager
                        .provision_managed_wsl_distribution()
                        .map(|()| ManagedSetupResult::WslProvisioned),
                    ManagedSetupOperation::RegisterModel(path) => manager
                        .register_existing_gguf(&path, None)
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

    pub(crate) fn poll(
        &self,
    ) -> Option<Result<ManagedSetupResult, ManagedLocalRuntimeError>> {
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

    pub(crate) fn setup_status(
        &self,
        environment: ManagedExecutionEnvironment,
    ) -> ManagedSetupStatus {
        if self.restart_marker_path().is_file() {
            let continuation_ready = cfg!(target_os = "windows")
                && matches!(
                    wsl_status(),
                    Ok(WslStatus::Available {
                        managed_distribution: true
                    })
                );
            if continuation_ready {
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
                    return ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(message)
                }
                Ok(WslStatus::Available { managed_distribution: false }) => {
                    return ManagedSetupStatus::WslDistributionMissing
                }
                Ok(WslStatus::Available { managed_distribution: true }) => {}
                Err(error) => {
                    return ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(
                        error.to_string(),
                    )
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
            WslStatus::Available { managed_distribution: true } => return Ok(()),
            WslStatus::Available { managed_distribution: false } => {}
        }
        let output = Command::new("wsl.exe")
            .args([
                "--install",
                "Ubuntu-24.04",
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
            WslStatus::Available { managed_distribution: true } => Ok(()),
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
                WslStatus::Available { managed_distribution: true } => {}
                WslStatus::Available { managed_distribution: false } => {
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

        let environment_root = self
            .root
            .join("runtime")
            .join(environment.storage_key());
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

    pub(crate) fn registered_models(
        &self,
    ) -> Result<Vec<ManagedModelRegistration>, ManagedLocalRuntimeError> {
        let registry: ManagedModelRegistry = read_optional_json(&self.model_registry_path())
            .map_err(model_io)?
            .unwrap_or_default();
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
            quantization: infer_quantization_from_name(&canonical),
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
            registry.models.sort_by(|left, right| left.model_id.cmp(&right.model_id));
        }
        write_json(&self.model_registry_path(), &registry).map_err(model_io)?;
        Ok(registration)
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
            WslStatus::Available { managed_distribution: true } => {}
            WslStatus::Available { managed_distribution: false } => {
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
        let target = wsl_model_path(&model.content_sha256);
        if wsl_file_exists(&target)? {
            verify_wsl_sha256(&target, &model.content_sha256)?;
            return Ok(PathBuf::from(target));
        }
        if !duplicate_storage_approved {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ModelTransferOrIntegrity,
                format!(
                    "WSL2 execution requires an additional {} bytes for a Linux-native copy; explicit storage approval is required",
                    model.size_bytes
                ),
            ));
        }
        stream_file_into_wsl(&model.source_path, &target)?;
        verify_wsl_sha256(&target, &model.content_sha256)?;
        Ok(PathBuf::from(target))
    }

    pub(crate) fn configuration_for(
        &self,
        model_id: &str,
        environment: ManagedExecutionEnvironment,
    ) -> Result<ManagedLocalModelConfig, ManagedLocalRuntimeError> {
        let installation = self
            .active_installation(environment)?
            .ok_or_else(|| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::RuntimeArtifactIntegrity,
                    format!("{} managed llama.cpp runtime is not installed", environment.label()),
                )
            })?;
        self.verify_retained_runtime_artifact(&installation)?;
        let model = self.require_model(model_id)?;
        let model_path = if environment == ManagedExecutionEnvironment::WindowsNative {
            self.verify_registered_model(&model)?;
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
            verify_wsl_sha256(&path, &model.content_sha256)?;
            PathBuf::from(path)
        };
        Ok(ManagedLocalModelConfig {
            state_root: self.root.clone(),
            environment,
            model_id: model.model_id,
            model_content_sha256: model.content_sha256,
            model_path,
            model_size_bytes: model.size_bytes,
            quantization: model.quantization,
            runtime_tag: installation.runtime_tag,
            runtime_revision: installation.runtime_revision,
            runtime_artifact_sha256: installation.artifact_sha256,
            runtime_compatibility_version: installation.compatibility_version,
        })
    }

    pub(crate) fn ensure_endpoint(
        config: &ManagedLocalModelConfig,
    ) -> Result<ManagedEndpoint, ManagedLocalRuntimeError> {
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
            let _ = manager.stop_process_state(&process);
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
        let runtime_root = self
            .root
            .join("runtime")
            .join(environment.storage_key());
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
            .args(windows_server_arguments(&config.model_path, port))
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
            WslStatus::Available { managed_distribution: true } => {}
            WslStatus::Available { managed_distribution: false } => {
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
        let server = shell_quote(&installation.server_path);
        let model = shell_quote(&config.model_path.to_string_lossy());
        let log = format!(
            "/var/lib/gameengine/local-ai/logs/llama-server-{}.log",
            config.environment.storage_key()
        );
        let command = format!(
            "mkdir -p /var/lib/gameengine/local-ai/logs; nohup {server} --model {model} --host 127.0.0.1 --port {port} --n-gpu-layers 999 > {} 2>&1 < /dev/null & echo $!",
            shell_quote(&log)
        );
        let output = managed_wsl_command()
            .args(["sh", "-lc", &command])
            .output()
            .map_err(|error| {
                ManagedLocalRuntimeError::new(
                    ManagedDiagnosticLayer::ManagedProcessStartup,
                    format!("could not launch pinned WSL2 llama-server: {error}"),
                )
            })?;
        if !output.status.success() {
            return Err(ManagedLocalRuntimeError::new(
                ManagedDiagnosticLayer::ManagedProcessStartup,
                format!("WSL2 llama-server launch failed: {}", command_output_text(&output)),
            ));
        }
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
        let manifest = fs::read_to_string(&installation.retained_artifact_path)
            .map_err(runtime_io)?;
        let environment_marker = format!(
            "environment={}",
            installation.environment.benchmark_id()
        );
        if !manifest.lines().any(|line| line == "format=gameengine-managed-runtime-v2")
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
                let retained_root = installation
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

    fn process_state_path(&self) -> PathBuf {
        self.root.join("state").join("process.json")
    }

    fn log_path(&self, environment: ManagedExecutionEnvironment) -> PathBuf {
        self.root
            .join("logs")
            .join(format!("llama-server-{}.log", environment.storage_key()))
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
        let started = Instant::now();
        while endpoint_is_healthy(&state.endpoint) && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(50));
        }
        self.clear_process_state()?;
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

fn windows_server_arguments(model_path: &Path, port: u16) -> Vec<String> {
    vec![
        "--model".to_owned(),
        model_path.to_string_lossy().into_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port".to_owned(),
        port.to_string(),
        "--n-gpu-layers".to_owned(),
        "999".to_owned(),
    ]
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

fn build_pinned_wsl_cuda_runtime() -> Result<WslCudaBuildProvenance, ManagedLocalRuntimeError> {
    let bootstrap = format!(
        "set -eu; export DEBIAN_FRONTEND=noninteractive; apt-get update; apt-get install -y --no-install-recommends ca-certificates curl gnupg git build-essential cmake ninja-build pkg-config libcurl4-openssl-dev; installed=$(dpkg-query -W -f='${{Status}}\n' {compiler_package} {libraries_dev_package} 2>/dev/null | grep -c 'ok installed' || true); if [ \"$installed\" -ne 2 ]; then curl -fsSL {repository}/{keyring} -o /tmp/{keyring}; dpkg -i /tmp/{keyring}; rm -f /tmp/{keyring}; apt-get update; apt-get install -y --no-install-recommends {compiler_package} {libraries_dev_package}; fi; test -x /usr/local/cuda-12.4/bin/nvcc; test -e /dev/dxg",
        compiler_package = WSL_CUDA_COMPILER_PACKAGE,
        libraries_dev_package = WSL_CUDA_LIBRARIES_DEV_PACKAGE,
        repository = WSL_CUDA_REPOSITORY_URL,
        keyring = WSL_CUDA_KEYRING_ASSET,
    );
    wsl_shell(&bootstrap, ManagedDiagnosticLayer::GpuOrBackendCapability)?;

    let build = format!(
        "set -eu; root=/var/lib/gameengine/local-ai; src=\"$root/src/llama.cpp-{tag}\"; stage=\"$root/runtime/{tag}.staging\"; final=\"$root/runtime/{tag}\"; log=\"$root/logs/llama-build-{tag}.log\"; mkdir -p \"$root/src\" \"$root/runtime\" \"$root/logs\"; rm -rf \"$src\" \"$stage\"; {{ git clone --filter=blob:none --depth 1 --branch {tag} {repository} \"$src\"; revision=$(git -C \"$src\" rev-parse HEAD); case \"$revision\" in {revision}*) ;; *) echo \"unexpected llama.cpp revision: $revision\" >&2; exit 1;; esac; export PATH=/usr/local/cuda-12.4/bin:$PATH; cmake -S \"$src\" -B \"$src/build\" -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=ON -DCMAKE_CUDA_COMPILER=/usr/local/cuda-12.4/bin/nvcc; cmake --build \"$src/build\" --target llama-server llama-bench --parallel; mkdir -p \"$stage\"; cp -a \"$src/build/bin/.\" \"$stage/\"; test -x \"$stage/llama-server\"; test -x \"$stage/llama-bench\"; \"$stage/llama-server\" --version; devices=$(\"$stage/llama-bench\" --list-devices 2>&1); printf '%s\n' \"$devices\"; printf '%s\n' \"$devices\" | grep -qi cuda; server_sha=$(sha256sum \"$stage/llama-server\" | awk '{{print $1}}'); bench_sha=$(sha256sum \"$stage/llama-bench\" | awk '{{print $1}}'); compiler_version=$(dpkg-query -W -f='${{Version}}' {compiler_package}); libraries_dev_version=$(dpkg-query -W -f='${{Version}}' {libraries_dev_package}); rm -rf \"$final\"; mv \"$stage\" \"$final\"; rm -rf \"$src\"; }} >\"$log\" 2>&1 || {{ tail -200 \"$log\" >&2; exit 1; }}; printf 'GAMEENGINE_REVISION=%s\nGAMEENGINE_COMPILER_VERSION=%s\nGAMEENGINE_LIBRARIES_DEV_VERSION=%s\nGAMEENGINE_SERVER_SHA256=%s\nGAMEENGINE_BENCH_SHA256=%s\n' \"$revision\" \"$compiler_version\" \"$libraries_dev_version\" \"$server_sha\" \"$bench_sha\"",
        tag = PINNED_LLAMA_CPP_TAG,
        revision = PINNED_LLAMA_CPP_REVISION,
        repository = LLAMA_CPP_REPOSITORY_URL,
        compiler_package = WSL_CUDA_COMPILER_PACKAGE,
        libraries_dev_package = WSL_CUDA_LIBRARIES_DEV_PACKAGE,
    );
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
    let metadata: GithubReleaseMetadata = serde_json::from_slice(&output.stdout).map_err(|error| {
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
    argument_prelude.push_str("); " );
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
            format!("managed setup command failed: {}", command_output_text(&output)),
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
            format!("SHA-256 calculation failed: {}", command_output_text(&output)),
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
    let actual = sha256_via_platform(path).map_err(|error| {
        ManagedLocalRuntimeError::new(layer, error.to_string())
    })?;
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
        .args([
            "sh",
            "-lc",
            &format!("cat > {}", shell_quote(remote)),
        ])
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
            format!("managed WSL transfer failed: {}", command_output_text(&output)),
        ));
    }
    Ok(())
}

fn wsl_model_path(content_sha256: &str) -> String {
    format!("/var/lib/gameengine/local-ai/models/{content_sha256}.gguf")
}

fn wsl_file_exists(path: &str) -> Result<bool, ManagedLocalRuntimeError> {
    let output = managed_wsl_command()
        .args([
            "sh",
            "-lc",
            &format!("test -f {}", shell_quote(path)),
        ])
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
    let output = managed_wsl_command()
        .args(["sh", "-lc", command])
        .output()
        .map_err(|error| {
            ManagedLocalRuntimeError::new(layer, format!("could not invoke managed WSL: {error}"))
        })?;
    if !output.status.success() {
        return Err(ManagedLocalRuntimeError::new(
            layer,
            format!("managed WSL command failed: {}", command_output_text(&output)),
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
    let status = Command::new("wsl.exe").arg("--status").output().map_err(|error| {
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
            .args(["/FI", &format!("PID eq {}", state.process_id), "/FO", "CSV", "/NH"])
            .output(),
        ManagedExecutionEnvironment::Wsl2Linux => managed_wsl_command()
            .args([
                "sh",
                "-lc",
                &format!("kill -0 {}", state.process_id),
            ])
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn infer_quantization_from_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_ascii_uppercase();
    for marker in [
        "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_K_S", "Q4_K_M", "Q5_0",
        "Q5_K_S", "Q5_K_M", "Q6_K", "Q8_0", "IQ2", "IQ3", "IQ4",
    ] {
        if name.contains(marker) {
            return Some(marker.to_owned());
        }
    }
    None
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
        if candidate.chars().any(|character| character.is_ascii_alphanumeric()) {
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
                expected_sha256:
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
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
        fs::write(&model, b"GGUF-test-bytes").expect("model fixture");
        let before = fs::read(&model).expect("before bytes");
        let manager = ManagedLocalRuntime::open(root.join("state")).expect("manager");
        let registration = manager
            .register_existing_gguf(&model, None)
            .expect("register existing GGUF");
        let after = fs::read(&model).expect("after bytes");
        assert_eq!(before, after);
        assert_eq!(registration.quantization.as_deref(), Some("Q4_K_M"));
        assert!(registration.model_id.starts_with("gguf:"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn restart_continuation_is_machine_local_state() {
        let root = temp_root("restart");
        let manager = ManagedLocalRuntime::open(root.clone()).expect("manager");
        manager.mark_restart_required().expect("mark restart");
        assert_eq!(manager.setup_status(ManagedExecutionEnvironment::WindowsNative), ManagedSetupStatus::RestartRequired);
        manager.clear_restart_required().expect("clear restart");
        assert!(!manager.restart_marker_path().exists());
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
        assert_eq!(WINDOWS_CUDA_MANIFEST, "llama-b10336-win-cuda-12.4-manifest.txt");
        assert_eq!(
            WSL_CUDA_MANIFEST,
            "llama-b10336-wsl-cuda-12.4-source-manifest.txt"
        );
        assert_eq!(WSL_CUDA_COMPILER_PACKAGE, "cuda-compiler-12-4");
        assert_eq!(
            WSL_CUDA_LIBRARIES_DEV_PACKAGE,
            "cuda-libraries-dev-12-4"
        );
        assert_eq!(STATE_SCHEMA_VERSION, 2);
        assert_eq!(MANAGED_WSL_DISTRIBUTION, "GameEngine-LocalAI");

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
        assert!(wsl_manifest.contains(
            "cuda_libraries_dev_package=cuda-libraries-dev-12-4"
        ));
        assert!(wsl_manifest.contains("source_revision=f401bb1deadbeef"));
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
    }

    #[test]
    fn windows_launch_contract_uses_exact_model_loopback_and_gpu_arguments() {
        let model = Path::new(r"C:\\models\\sample-Q4_K_M.gguf");
        assert_eq!(
            windows_server_arguments(model, 18443),
            vec![
                "--model".to_owned(),
                model.to_string_lossy().into_owned(),
                "--host".to_owned(),
                "127.0.0.1".to_owned(),
                "--port".to_owned(),
                "18443".to_owned(),
                "--n-gpu-layers".to_owned(),
                "999".to_owned(),
            ]
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
    fn startup_contract_never_treats_a_crashed_process_as_healthy() {
        assert!(!managed_process_exited_before_health(true, false));
        assert!(!managed_process_exited_before_health(false, true));
        assert!(managed_process_exited_before_health(false, false));
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
        assert!(manager.verify_retained_runtime_artifact(&installation).is_err());
        let after = fs::read(environment_root.join("active.json")).expect("after active");
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(root);
    }
}
