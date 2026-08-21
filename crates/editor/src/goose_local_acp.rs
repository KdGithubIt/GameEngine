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
const GOOSE_CONTEXT_LIMIT_ENV: &str = "GOOSE_CONTEXT_LIMIT";
const GOOSE_MAX_TOKENS_ENV: &str = "GOOSE_MAX_TOKENS";
/// Reserve one eighth of the physical context for a single model response.
///
/// Goose auto-compacts at 80% by default. A 12.5% response cap therefore leaves
/// additional headroom for a normal threshold-triggered provider call while
/// scaling from 1,024 output tokens at the 8,192-token managed floor to 4,096
/// at the 32,768-token managed ceiling. Long same-turn tool loops remain Goose
/// state-machine responsibility; if upstream still overruns this contract, the
/// ACP prompt failure is terminalized rather than retried by GameEngine.
const GOOSE_OUTPUT_BUDGET_DIVISOR: u32 = 8;
const GOOSE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
static GOOSE_CONFIG_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GooseManagedTokenBudget {
    context_tokens: u32,
    max_output_tokens: u32,
}

impl GooseManagedTokenBudget {
    fn from_config(config: &ManagedLocalModelConfig) -> Self {
        Self::from_context_tokens(managed_context_tokens(config))
    }

    fn from_context_tokens(context_tokens: u32) -> Self {
        debug_assert!(context_tokens >= GOOSE_OUTPUT_BUDGET_DIVISOR);
        Self {
            context_tokens,
            max_output_tokens: context_tokens / GOOSE_OUTPUT_BUDGET_DIVISOR,
        }
    }
}

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
        let resolved = discover_goose_executable(&config.managed_model)?;
        let executable = resolved.executable;
        let acp_identity = AcpRuntimeIdentity::stable(GOOSE_ACP_AGENT_NAME, Some(resolved.version));
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

    /// Returns whether Managed Local has no discoverable Goose candidate and needs UI setup.
    ///
    /// This is intentionally a filesystem-only presentation check. The full resolver still
    /// probes `--version` and `acp --help` immediately before a session is registered.
    pub(crate) fn setup_required(config: &ManagedLocalModelConfig) -> bool {
        let Ok(manager) = ManagedLocalRuntime::open(config.state_root.clone()) else {
            return true;
        };
        if manager.managed_goose_candidate_available() {
            return false;
        }
        if manager
            .goose_executable_override()
            .ok()
            .flatten()
            .is_some_and(|path| path.is_file())
        {
            return false;
        }
        if std::env::var_os(GOOSE_EXECUTABLE_ENV)
            .map(PathBuf::from)
            .is_some_and(|path| path.is_file())
        {
            return false;
        }
        !goose_fallback_candidates()
            .into_iter()
            .any(|candidate| candidate.executable.is_file())
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
        let token_budget = GooseManagedTokenBudget::from_config(&self.config.managed_model);
        let ephemeral =
            GooseEphemeralConfig::create(&self.config.managed_model, &lease, token_budget).map_err(
                |error| {
                    AcpRuntimeError::Transport(format!(
                        "could not create isolated Goose config: {error}"
                    ))
                },
            )?;

        let mut descriptor = self.descriptor.clone();
        descriptor.environment = goose_environment(&lease, &ephemeral, token_budget);
        let mut runtime = AcpProcessRuntime::new_with_tool_name_metadata_path(
            descriptor,
            &["goose", "toolCall", "toolName"],
        )?;
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
    token_budget: GooseManagedTokenBudget,
) -> BTreeMap<OsString, OsString> {
    let mut environment = BTreeMap::from([
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
    ]);
    environment.extend(goose_token_budget_environment(token_budget));
    environment
}

fn goose_token_budget_environment(
    token_budget: GooseManagedTokenBudget,
) -> [(OsString, OsString); 2] {
    [
        (
            OsString::from(GOOSE_CONTEXT_LIMIT_ENV),
            OsString::from(token_budget.context_tokens.to_string()),
        ),
        (
            OsString::from(GOOSE_MAX_TOKENS_ENV),
            OsString::from(token_budget.max_output_tokens.to_string()),
        ),
    ]
}

fn goose_provider_document(
    lease: &ManagedLocalEndpointLease,
    token_budget: GooseManagedTokenBudget,
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
            "context_limit": token_budget.context_tokens as usize,
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
        token_budget: GooseManagedTokenBudget,
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
        let provider = serde_json::to_vec_pretty(&goose_provider_document(lease, token_budget))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GooseExecutableSource {
    Managed,
    MachineOverride,
    EnvironmentOverride,
    Path,
    LegacyHome,
}

impl GooseExecutableSource {
    const fn label(self) -> &'static str {
        match self {
            Self::Managed => "GameEngine-managed Goose",
            Self::MachineOverride => "machine-local Goose override",
            Self::EnvironmentOverride => GOOSE_EXECUTABLE_ENV,
            Self::Path => "PATH",
            Self::LegacyHome => "legacy home installation",
        }
    }
}

#[derive(Debug, Clone)]
struct GooseExecutableCandidate {
    source: GooseExecutableSource,
    executable: PathBuf,
    strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedGooseExecutable {
    executable: PathBuf,
    version: String,
}

fn discover_goose_executable(
    config: &ManagedLocalModelConfig,
) -> Result<ResolvedGooseExecutable, AcpRuntimeError> {
    let manager = ManagedLocalRuntime::open(config.state_root.clone())
        .map_err(|error| AcpRuntimeError::Transport(error.to_string()))?;
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();

    match manager.managed_goose_executable() {
        Ok(Some(executable)) => candidates.push(GooseExecutableCandidate {
            source: GooseExecutableSource::Managed,
            executable,
            strict: false,
        }),
        Ok(None) => {}
        Err(error) => diagnostics.push(format!("managed Goose is invalid: {error}")),
    }
    match manager.goose_executable_override() {
        Ok(Some(executable)) => candidates.push(GooseExecutableCandidate {
            source: GooseExecutableSource::MachineOverride,
            executable,
            strict: true,
        }),
        Ok(None) => {}
        Err(error) => {
            return Err(AcpRuntimeError::Transport(format!(
                "machine-local Goose override is invalid: {error}"
            )));
        }
    }
    if let Some(configured) = std::env::var_os(GOOSE_EXECUTABLE_ENV) {
        candidates.push(GooseExecutableCandidate {
            source: GooseExecutableSource::EnvironmentOverride,
            executable: PathBuf::from(configured),
            strict: true,
        });
    }
    candidates.extend(goose_fallback_candidates());

    resolve_goose_candidates(candidates, diagnostics, probe_goose_executable)
}

fn resolve_goose_candidates(
    candidates: Vec<GooseExecutableCandidate>,
    mut diagnostics: Vec<String>,
    mut probe: impl FnMut(&Path) -> Result<String, AcpRuntimeError>,
) -> Result<ResolvedGooseExecutable, AcpRuntimeError> {
    for candidate in candidates {
        if !candidate.executable.is_file() {
            let message = format!(
                "{} points to `{}`, but that file does not exist",
                candidate.source.label(),
                candidate.executable.display()
            );
            if candidate.strict {
                return Err(AcpRuntimeError::Transport(message));
            }
            diagnostics.push(message);
            continue;
        }
        match probe(&candidate.executable) {
            Ok(version) => {
                return Ok(ResolvedGooseExecutable {
                    executable: candidate.executable,
                    version,
                });
            }
            Err(error) if candidate.strict => {
                return Err(AcpRuntimeError::Transport(format!(
                    "{} is not a usable Goose ACP executable: {error}",
                    candidate.source.label()
                )));
            }
            Err(error) => diagnostics.push(format!(
                "{} candidate `{}` is not usable: {error}",
                candidate.source.label(),
                candidate.executable.display()
            )),
        }
    }
    let detail = if diagnostics.is_empty() {
        String::new()
    } else {
        format!(" Details: {}", diagnostics.join("; "))
    };
    Err(AcpRuntimeError::Transport(format!(
        "Goose ACP runtime is not ready. Open AI Studio Settings > Models > Managed Local AI and choose Install Goose; GameEngine can provision the pinned runtime without PATH or environment-variable setup.{detail}"
    )))
}

fn probe_goose_executable(executable: &Path) -> Result<String, AcpRuntimeError> {
    let version_output = command_output_with_timeout(executable, &["--version"])?;
    let version_text = first_nonempty_output_line(&version_output).ok_or_else(|| {
        AcpRuntimeError::Transport("Goose executable did not report a version".to_owned())
    })?;
    let version = extract_semver(&version_text).ok_or_else(|| {
        AcpRuntimeError::Transport(format!(
            "Goose executable reported an unparseable version: {version_text}"
        ))
    })?;
    command_output_with_timeout(executable, &["acp", "--help"])?;
    Ok(version)
}

fn goose_fallback_candidates() -> Vec<GooseExecutableCandidate> {
    let executable_name = if cfg!(windows) { "goose.exe" } else { "goose" };
    let mut candidates = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| GooseExecutableCandidate {
                    source: GooseExecutableSource::Path,
                    executable: directory.join(executable_name),
                    strict: false,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        if cfg!(windows) {
            candidates.push(GooseExecutableCandidate {
                source: GooseExecutableSource::LegacyHome,
                executable: home.join("goose").join("goose.exe"),
                strict: false,
            });
            candidates.push(GooseExecutableCandidate {
                source: GooseExecutableSource::LegacyHome,
                executable: home.join(".local").join("bin").join("goose.exe"),
                strict: false,
            });
        } else {
            candidates.push(GooseExecutableCandidate {
                source: GooseExecutableSource::LegacyHome,
                executable: home.join(".local").join("bin").join("goose"),
                strict: false,
            });
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
    fn managed_context_contract_reserves_output_budget_at_the_physical_limit() {
        let budget = GooseManagedTokenBudget::from_context_tokens(8_192);
        assert_eq!(budget.context_tokens, 8_192);
        assert_eq!(budget.max_output_tokens, 1_024);

        let environment = BTreeMap::from(goose_token_budget_environment(budget));
        assert_eq!(
            environment.get(&OsString::from(GOOSE_CONTEXT_LIMIT_ENV)),
            Some(&OsString::from("8192"))
        );
        assert_eq!(
            environment.get(&OsString::from(GOOSE_MAX_TOKENS_ENV)),
            Some(&OsString::from("1024"))
        );
    }

    #[test]
    fn managed_context_override_is_independent_of_canonical_model_names() {
        let budget = GooseManagedTokenBudget::from_context_tokens(8_192);
        let environment = BTreeMap::from(goose_token_budget_environment(budget));

        for canonical_shaped_model in ["gpt-4.1", "deepseek-ai/deepseek-v4-pro"] {
            let advertised = environment
                .get(&OsString::from(GOOSE_CONTEXT_LIMIT_ENV))
                .expect("managed context override");
            assert_eq!(
                advertised,
                &OsString::from("8192"),
                "{canonical_shaped_model} must not change the managed physical context"
            );
        }
    }

    fn candidate_fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "gameengine-goose-resolver-{}-{name}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("resolver fixture root");
        let executable = root.join("goose.exe");
        fs::write(&executable, b"fixture").expect("resolver fixture executable");
        executable
    }

    #[test]
    fn managed_goose_precedes_machine_env_and_path_candidates() {
        let managed = candidate_fixture("managed");
        let machine = candidate_fixture("machine");
        let environment = candidate_fixture("environment");
        let path = candidate_fixture("path");
        let resolved = resolve_goose_candidates(
            vec![
                GooseExecutableCandidate {
                    source: GooseExecutableSource::Managed,
                    executable: managed.clone(),
                    strict: false,
                },
                GooseExecutableCandidate {
                    source: GooseExecutableSource::MachineOverride,
                    executable: machine,
                    strict: true,
                },
                GooseExecutableCandidate {
                    source: GooseExecutableSource::EnvironmentOverride,
                    executable: environment,
                    strict: true,
                },
                GooseExecutableCandidate {
                    source: GooseExecutableSource::Path,
                    executable: path,
                    strict: false,
                },
            ],
            Vec::new(),
            |_| Ok("1.44.0".to_owned()),
        )
        .expect("managed candidate");
        assert_eq!(resolved.executable, managed);
    }

    #[test]
    fn invalid_managed_candidate_can_fall_back_to_machine_override() {
        let managed = candidate_fixture("invalid-managed");
        let machine = candidate_fixture("valid-machine");
        let resolved = resolve_goose_candidates(
            vec![
                GooseExecutableCandidate {
                    source: GooseExecutableSource::Managed,
                    executable: managed.clone(),
                    strict: false,
                },
                GooseExecutableCandidate {
                    source: GooseExecutableSource::MachineOverride,
                    executable: machine.clone(),
                    strict: true,
                },
            ],
            Vec::new(),
            |candidate| {
                if candidate == managed {
                    Err(AcpRuntimeError::Transport("not Goose".to_owned()))
                } else {
                    Ok("1.44.0".to_owned())
                }
            },
        )
        .expect("machine override fallback");
        assert_eq!(resolved.executable, machine);
    }

    #[test]
    fn invalid_environment_override_is_not_silently_ignored() {
        let environment = candidate_fixture("invalid-env");
        let path = candidate_fixture("path-after-env");
        let error = resolve_goose_candidates(
            vec![
                GooseExecutableCandidate {
                    source: GooseExecutableSource::EnvironmentOverride,
                    executable: environment.clone(),
                    strict: true,
                },
                GooseExecutableCandidate {
                    source: GooseExecutableSource::Path,
                    executable: path,
                    strict: false,
                },
            ],
            Vec::new(),
            |candidate| {
                if candidate == environment {
                    Err(AcpRuntimeError::Transport("not Goose".to_owned()))
                } else {
                    Ok("1.44.0".to_owned())
                }
            },
        )
        .expect_err("explicit environment override must fail closed");
        assert!(error.to_string().contains(GOOSE_EXECUTABLE_ENV));
    }

    #[test]
    fn environment_override_resolves_when_no_managed_candidate_exists() {
        let environment = candidate_fixture("valid-env");
        let resolved = resolve_goose_candidates(
            vec![GooseExecutableCandidate {
                source: GooseExecutableSource::EnvironmentOverride,
                executable: environment.clone(),
                strict: true,
            }],
            Vec::new(),
            |_| Ok("1.44.0".to_owned()),
        )
        .expect("environment override");
        assert_eq!(resolved.executable, environment);
    }

    #[test]
    fn path_candidate_remains_the_non_override_fallback() {
        let path = candidate_fixture("valid-path");
        let resolved = resolve_goose_candidates(
            vec![GooseExecutableCandidate {
                source: GooseExecutableSource::Path,
                executable: path.clone(),
                strict: false,
            }],
            Vec::new(),
            |_| Ok("1.44.0".to_owned()),
        )
        .expect("PATH fallback");
        assert_eq!(resolved.executable, path);
    }

    #[test]
    fn no_goose_candidate_reports_in_product_setup_action() {
        let error = resolve_goose_candidates(Vec::new(), Vec::new(), |_| {
            unreachable!("there is no candidate to probe")
        })
        .expect_err("missing Goose must be reported");
        let message = error.to_string();
        assert!(message.contains("Install Goose"));
        assert!(!message.contains("add it to PATH"));
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
