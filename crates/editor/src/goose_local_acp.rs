//! Goose ACP adapter for GameEngine-managed local inference.
//!
//! Goose-specific discovery and process-local Managed Local configuration end
//! here. ACP wire I/O is delegated to the common process runtime; Managed Local
//! retains model/server ownership and Agent Host retains execution authority.

use crate::acp_agent_runtime::{
    AcpAgentDescriptor, AcpAgentRuntime, AcpAgentSession, AcpCapabilities, AcpProcessRuntime,
    AcpRuntimeError, AcpRuntimeIdentity, AcpSessionOpenRequest,
};
use crate::managed_local_runtime::{
    ManagedExecutionEnvironment, ManagedLocalEndpointLease, ManagedLocalModelConfig,
    ManagedLocalRuntime, managed_context_tokens,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Descriptor ID registered for the Managed Local Goose adapter.
pub(crate) const GOOSE_LOCAL_ACP_DESCRIPTOR_ID: &str = "goose.managed-local";
/// Stable ACP agent name reported by Goose initialization.
pub(crate) const GOOSE_ACP_AGENT_NAME: &str = "goose";
const GOOSE_EXECUTABLE_ENV: &str = "GAMEENGINE_GOOSE_EXECUTABLE";
const GOOSE_PROVIDER_ID: &str = "custom_gameengine_managed_local";
const GOOSE_PROVIDER_FILE: &str = "custom_gameengine_managed_local.json";
const GOOSE_PATH_ROOT_ENV: &str = "GOOSE_PATH_ROOT";
const GOOSE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
static GOOSE_CONFIG_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Identity linking the ACP Goose process to the exact Managed Local runtime/model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GooseLocalRuntimeIdentity {
    pub(crate) acp: AcpRuntimeIdentity,
    pub(crate) managed_runtime: String,
    pub(crate) model_id: String,
    pub(crate) model_content_sha256: String,
    pub(crate) model_representation: Option<String>,
    pub(crate) execution_environment: ManagedExecutionEnvironment,
}

/// Frozen machine-local configuration needed to open Goose on Managed Local.
#[derive(Debug, Clone)]
pub(crate) struct GooseLocalAcpConfig {
    pub(crate) managed_model: ManagedLocalModelConfig,
}

impl GooseLocalAcpConfig {
    /// Validates the frozen Managed Local identity used by this adapter.
    ///
    /// Session working-directory authority belongs to Agent Host/Bridge: Ask
    /// uses the project root while Build uses the isolated Agent Code Workspace.
    pub(crate) fn new(managed_model: ManagedLocalModelConfig) -> Result<Self, AcpRuntimeError> {
        if !managed_model.state_root.is_absolute() {
            return Err(AcpRuntimeError::InvalidDescriptor(
                "Managed Local state root must be absolute before Goose ACP can isolate its machine-local configuration"
                    .to_owned(),
            ));
        }
        Ok(Self { managed_model })
    }
}

/// Runtime wrapper coupling one common ACP process to a Managed Local endpoint lease.
pub(crate) struct GooseLocalAcpRuntime {
    descriptor: AcpAgentDescriptor,
    identity: GooseLocalRuntimeIdentity,
    config: GooseLocalAcpConfig,
}

impl GooseLocalAcpRuntime {
    /// Discovers the machine-local Goose executable and exact ACP identity.
    pub(crate) fn discover(config: GooseLocalAcpConfig) -> Result<Self, AcpRuntimeError> {
        let executable = discover_goose_executable()?;
        let version_output = command_output_with_timeout(&executable, &["--version"])?;
        let version_text = first_nonempty_output_line(&version_output).ok_or_else(|| {
            AcpRuntimeError::Transport(
                "Goose executable did not report a version; reinstall or set GAMEENGINE_GOOSE_EXECUTABLE to a working goose binary"
                    .to_owned(),
            )
        })?;
        let version = extract_semver(&version_text).ok_or_else(|| {
            AcpRuntimeError::Transport(format!(
                "Goose executable reported an unparseable version: {version_text}"
            ))
        })?;
        command_output_with_timeout(&executable, &["acp", "--help"])?;

        let acp_identity = AcpRuntimeIdentity::stable(GOOSE_ACP_AGENT_NAME, Some(version));
        let identity = GooseLocalRuntimeIdentity {
            acp: acp_identity.clone(),
            managed_runtime: config.managed_model.benchmark_runtime_identity(),
            model_id: config.managed_model.model_id.clone(),
            model_content_sha256: config.managed_model.model_content_sha256.clone(),
            model_representation: config.managed_model.model_representation.clone(),
            execution_environment: config.managed_model.environment,
        };
        let descriptor = AcpAgentDescriptor {
            id: GOOSE_LOCAL_ACP_DESCRIPTOR_ID.to_owned(),
            executable: executable.into_os_string(),
            arguments: vec![OsString::from("acp")],
            environment: BTreeMap::new(),
            capabilities: AcpCapabilities {
                mcp_http: true,
                ..AcpCapabilities::default()
            },
            runtime_identity: acp_identity,
        };
        Ok(Self {
            descriptor,
            identity,
            config,
        })
    }

    /// Returns the combined ACP + Managed Local identity used by benchmark evidence.
    pub(crate) fn runtime_identity(&self) -> &GooseLocalRuntimeIdentity {
        &self.identity
    }
}

impl AcpAgentRuntime for GooseLocalAcpRuntime {
    fn descriptor(&self) -> &AcpAgentDescriptor {
        &self.descriptor
    }

    fn open_session(
        &mut self,
        request: AcpSessionOpenRequest,
    ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
        if !request.working_directory.is_absolute() || !request.working_directory.is_dir() {
            return Err(AcpRuntimeError::InvalidSessionBinding(
                "Goose ACP working directory must be an existing absolute directory supplied by Agent Host"
                    .to_owned(),
            ));
        }

        let lease = ManagedLocalRuntime::lease_endpoint(&self.config.managed_model)
            .map_err(|error| AcpRuntimeError::Transport(error.to_string()))?;
        validate_managed_lease_identity(&lease, &self.config.managed_model)?;
        let ephemeral =
            GooseEphemeralConfig::create(&self.config.managed_model, &lease).map_err(|error| {
                AcpRuntimeError::Transport(format!(
                    "could not create isolated Goose config: {error}"
                ))
            })?;

        let mut descriptor = self.descriptor.clone();
        descriptor.environment = goose_environment(&lease, &ephemeral);
        let mut runtime = AcpProcessRuntime::new(descriptor)?;
        match runtime.open_session(request) {
            Ok(inner) => Ok(Box::new(GooseLocalAcpSession {
                inner,
                lease: Some(lease),
                ephemeral: Some(ephemeral),
                closed: false,
            })),
            Err(error) => {
                drop(ephemeral);
                drop(lease);
                Err(error)
            }
        }
    }
}

struct GooseLocalAcpSession {
    inner: Box<dyn AcpAgentSession>,
    lease: Option<ManagedLocalEndpointLease>,
    ephemeral: Option<GooseEphemeralConfig>,
    closed: bool,
}

impl GooseLocalAcpSession {
    fn release_resources(&mut self) -> Result<(), AcpRuntimeError> {
        if let Some(lease) = self.lease.take() {
            lease
                .release()
                .map_err(|error| AcpRuntimeError::Transport(error.to_string()))?;
        }
        self.ephemeral.take();
        Ok(())
    }
}

impl AcpAgentSession for GooseLocalAcpSession {
    fn acp_session_id(&self) -> &str {
        self.inner.acp_session_id()
    }

    fn binding(&self) -> &crate::acp_agent_runtime::AcpSessionBinding {
        self.inner.binding()
    }

    fn capabilities(&self) -> &AcpCapabilities {
        self.inner.capabilities()
    }

    fn runtime_identity(&self) -> &AcpRuntimeIdentity {
        self.inner.runtime_identity()
    }

    fn set_session_config_option(
        &mut self,
        option_id: &str,
        value: &str,
    ) -> Result<(), AcpRuntimeError> {
        self.inner.set_session_config_option(option_id, value)
    }

    fn send_prompt(&mut self, prompt: &str) -> Result<(), AcpRuntimeError> {
        self.inner.send_prompt(prompt)
    }

    fn try_next_event(
        &mut self,
    ) -> Result<Option<crate::acp_agent_runtime::AcpNormalizedEvent>, AcpRuntimeError> {
        self.inner.try_next_event()
    }

    fn resolve_permission(
        &mut self,
        resolution: crate::acp_agent_runtime::AcpPermissionResolution,
    ) -> Result<(), AcpRuntimeError> {
        self.inner.resolve_permission(resolution)
    }

    fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
        self.inner.cancel()
    }

    fn close(&mut self) -> Result<(), AcpRuntimeError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let close_result = self.inner.close();
        let release_result = self.release_resources();
        close_result?;
        release_result
    }
}

impl Drop for GooseLocalAcpSession {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.inner.close();
            self.closed = true;
        }
        let _ = self.release_resources();
    }
}

fn validate_managed_lease_identity(
    lease: &ManagedLocalEndpointLease,
    config: &ManagedLocalModelConfig,
) -> Result<(), AcpRuntimeError> {
    let identity = lease.identity();
    if identity.model_id != config.model_id
        || identity.model_content_sha256 != config.model_content_sha256
        || identity.model_representation != config.model_representation
        || identity.runtime_identity != config.benchmark_runtime_identity()
        || identity.execution_environment != config.environment
        || !identity.endpoint_url.starts_with("http://127.0.0.1:")
    {
        return Err(AcpRuntimeError::Transport(
            "Managed Local endpoint lease identity does not match the frozen Goose configuration"
                .to_owned(),
        ));
    }
    Ok(())
}

fn goose_environment(
    lease: &ManagedLocalEndpointLease,
    ephemeral: &GooseEphemeralConfig,
) -> BTreeMap<OsString, OsString> {
    BTreeMap::from([
        (
            OsString::from("GOOSE_PROVIDER"),
            OsString::from(GOOSE_PROVIDER_ID),
        ),
        (
            OsString::from("GOOSE_MODEL"),
            OsString::from(&lease.identity().model_id),
        ),
        (OsString::from("GOOSE_MODE"), OsString::from("approve")),
        (
            OsString::from(GOOSE_PATH_ROOT_ENV),
            ephemeral.root.as_os_str().to_os_string(),
        ),
        (
            OsString::from("GOOSE_TELEMETRY_ENABLED"),
            OsString::from("false"),
        ),
        (
            OsString::from("GOOSE_PROJECT_TRACKER_ENABLED"),
            OsString::from("false"),
        ),
    ])
}

fn goose_provider_document(
    lease: &ManagedLocalEndpointLease,
    config: &ManagedLocalModelConfig,
) -> Value {
    json!({
        "name": GOOSE_PROVIDER_ID,
        "engine": "openai",
        "display_name": "GameEngine Managed Local",
        "description": "Ephemeral GameEngine-managed local inference endpoint",
        "api_key_env": "",
        "base_url": lease.identity().endpoint_url.clone(),
        "models": [{
            "name": lease.identity().model_id.clone(),
            "context_limit": managed_context_tokens(config) as usize,
            "input_token_cost": null,
            "output_token_cost": null,
            "currency": null,
            "supports_cache_control": null,
            "reasoning": false
        }],
        "headers": null,
        "timeout_seconds": 900,
        "supports_streaming": false,
        "requires_auth": false,
        "catalog_provider_id": null,
        "base_path": "v1/chat/completions",
        "env_vars": null,
        "dynamic_models": false,
        "skip_canonical_filtering": true,
        "model_doc_link": null,
        "setup_steps": [],
        "fast_model": null,
        "preserves_thinking": false,
        "emit_clear_thinking": false,
        "setup": null
    })
}

struct GooseEphemeralConfig {
    root: PathBuf,
}

impl GooseEphemeralConfig {
    fn create(
        config: &ManagedLocalModelConfig,
        lease: &ManagedLocalEndpointLease,
    ) -> std::io::Result<Self> {
        let id = GOOSE_CONFIG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = config
            .state_root
            .join("goose-acp")
            .join(format!("{}-{id}", std::process::id()));
        if !root.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "GOOSE_PATH_ROOT must be an absolute machine-local path",
            ));
        }
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        let provider_dir = root.join("config").join("custom_providers");
        fs::create_dir_all(&provider_dir)?;
        let provider = serde_json::to_vec_pretty(&goose_provider_document(lease, config))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        fs::write(provider_dir.join(GOOSE_PROVIDER_FILE), provider)?;
        Ok(Self { root })
    }
}

impl Drop for GooseEphemeralConfig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn discover_goose_executable() -> Result<PathBuf, AcpRuntimeError> {
    if let Some(configured) = std::env::var_os(GOOSE_EXECUTABLE_ENV) {
        let configured = PathBuf::from(configured);
        if configured.is_file() {
            return Ok(configured);
        }
        return Err(AcpRuntimeError::Transport(format!(
            "{GOOSE_EXECUTABLE_ENV} points to `{}` but that file does not exist",
            configured.display()
        )));
    }
    for candidate in goose_executable_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(AcpRuntimeError::Transport(
        "Goose executable was not found. Install Goose, add it to PATH, or set GAMEENGINE_GOOSE_EXECUTABLE to the machine-local goose executable"
            .to_owned(),
    ))
}

fn goose_executable_candidates() -> Vec<PathBuf> {
    let executable_name = if cfg!(windows) { "goose.exe" } else { "goose" };
    let mut candidates = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(executable_name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        if cfg!(windows) {
            candidates.push(home.join("goose").join("goose.exe"));
            candidates.push(home.join(".local").join("bin").join("goose.exe"));
        } else {
            candidates.push(home.join(".local").join("bin").join("goose"));
        }
    }
    candidates
}

fn command_output_with_timeout(
    executable: &Path,
    arguments: &[&str],
) -> Result<Output, AcpRuntimeError> {
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            AcpRuntimeError::Transport(format!(
                "could not execute Goose `{}`: {error}",
                executable.display()
            ))
        })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|error| {
                    AcpRuntimeError::Transport(format!("could not collect Goose output: {error}"))
                })?;
                if !status.success() {
                    return Err(AcpRuntimeError::Transport(format!(
                        "Goose command `{}` failed: {}",
                        arguments.join(" "),
                        command_output_text(&output)
                    )));
                }
                return Ok(output);
            }
            Ok(None) if started.elapsed() < GOOSE_PROBE_TIMEOUT => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AcpRuntimeError::Transport(format!(
                    "Goose command `{}` timed out",
                    arguments.join(" ")
                )));
            }
            Err(error) => {
                return Err(AcpRuntimeError::Transport(format!(
                    "could not inspect Goose command status: {error}"
                )));
            }
        }
    }
}

fn command_output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{} {}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned()
}

fn first_nonempty_output_line(output: &Output) -> Option<String> {
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .chain(output.stderr.split(|byte| *byte == b'\n'))
        .map(|line| String::from_utf8_lossy(line).trim().to_owned())
        .find(|line| !line.is_empty())
}

fn extract_semver(output: &str) -> Option<String> {
    output
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '.' || character == '-')
        })
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            let mut parts = token.split('.');
            let valid = (0..3).all(|_| {
                parts.next().is_some_and(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())
                })
            });
            valid && parts.next().is_none()
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_version_parser_uses_semantic_version_only() {
        assert_eq!(extract_semver("goose 1.9.3"), Some("1.9.3".to_owned()));
        assert_eq!(extract_semver("goose unknown"), None);
    }

    #[test]
    fn managed_identity_stays_attached_to_goose_identity() {
        let identity = GooseLocalRuntimeIdentity {
            acp: AcpRuntimeIdentity::stable("goose", Some("1.0.0".to_owned())),
            managed_runtime: "llama.cpp:test".to_owned(),
            model_id: "gguf:test".to_owned(),
            model_content_sha256: "a".repeat(64),
            model_representation: Some("gguf".to_owned()),
            execution_environment: ManagedExecutionEnvironment::WindowsNative,
        };
        assert_eq!(identity.model_id, "gguf:test");
        assert_eq!(identity.model_content_sha256, "a".repeat(64));
    }
}
