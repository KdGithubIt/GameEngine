//! Codex ACP adapter layered on the provider-neutral ACP process runtime.
//!
//! This module owns Codex-specific discovery, setup, launch placement, safe
//! session-mode mapping, and model preferences. ACP wire I/O remains in the
//! shared transport and Agent Host remains authoritative for permissions,
//! authoring, validation, and completion.

use crate::acp_agent_runtime::{
    AcpAgentDescriptor, AcpAgentRuntime, AcpAgentSession, AcpCapabilities, AcpMcpAccessLevel,
    AcpProcessRuntime, AcpRuntimeError, AcpRuntimeIdentity, AcpSessionBinding,
    AcpSessionOpenRequest,
};
use crate::external_agent_provider::{
    ExternalAgentExecutionEnvironment, ExternalAgentExecutionPlacement, command_output,
    installer_is_available, placed_launch_command, probe_wsl_loopback_reachability,
    wsl_environment_forwarding,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;

/// Logical AI target used by AI Studio routing.
pub(crate) const CODEX_ACP_LOGICAL_TARGET: &str = "codex";
/// Descriptor ID registered in the provider-neutral ACP registry.
pub(crate) const CODEX_ACP_DESCRIPTOR_ID: &str = "codex.acp";
/// Executable published by the Codex ACP package.
pub(crate) const CODEX_ACP_EXECUTABLE: &str = "codex-acp";
/// ACP adapter package whose behavior this adapter is pinned to.
pub(crate) const CODEX_ACP_PACKAGE_NAME: &str = "@agentclientprotocol/codex-acp";
/// Exact validated Codex ACP package version.
pub(crate) const CODEX_ACP_PACKAGE_VERSION: &str = "1.6.2";
/// Exact package installed by the setup action.
pub(crate) const CODEX_ACP_INSTALL_PACKAGE: &str = "@agentclientprotocol/codex-acp@1.6.2";

const INITIAL_AGENT_MODE_ENV: &str = "INITIAL_AGENT_MODE";
const MODEL_CONFIG_ID: &str = "model";
const REASONING_EFFORT_CONFIG_ID: &str = "reasoning_effort";
const MODE_CONFIG_ID: &str = "mode";
const FAST_MODE_CONFIG_ID: &str = "fast-mode";
const READ_ONLY_MODE_ID: &str = "read-only";
const AGENT_MODE_ID: &str = "agent";
const FAST_MODE_ON: &str = "on";
const FAST_MODE_OFF: &str = "off";

/// Machine-level readiness of the pinned Codex ACP package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexAcpAvailability {
    Available,
    Missing,
    VersionMismatch {
        expected: String,
        observed: Option<String>,
    },
    ProbeFailed(String),
}

impl CodexAcpAvailability {
    fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    fn diagnostic(&self) -> Option<String> {
        match self {
            Self::Available => None,
            Self::Missing => Some(format!(
                "Codex ACP is not installed in the selected execution environment. Install {CODEX_ACP_INSTALL_PACKAGE} or select an environment where `{CODEX_ACP_EXECUTABLE}` is available."
            )),
            Self::VersionMismatch { expected, observed } => Some(match observed {
                Some(observed) => format!(
                    "Codex ACP {observed} is installed, but GameEngine requires exactly {expected}. Run the pinned Codex ACP setup action before using this adapter."
                ),
                None => format!(
                    "Codex ACP is installed but did not report a parseable version. GameEngine requires exactly {expected}."
                ),
            }),
            Self::ProbeFailed(message) => Some(format!(
                "Codex ACP could not be probed in the selected execution environment: {message}"
            )),
        }
    }
}

/// Result of the machine-level Codex ACP setup probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexAcpProbe {
    pub(crate) availability: CodexAcpAvailability,
    pub(crate) installer_available: bool,
    pub(crate) expected_identity: AcpRuntimeIdentity,
}

impl CodexAcpProbe {
    fn setup_diagnostic(&self) -> Option<String> {
        if matches!(self.availability, CodexAcpAvailability::Missing) && !self.installer_available {
            return Some(format!(
                "Codex ACP is not installed, and npm is not available in the selected execution environment. Install npm there first, then install {CODEX_ACP_INSTALL_PACKAGE}."
            ));
        }
        self.availability.diagnostic()
    }
}

/// User-selected Codex settings that have a formal ACP config mapping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CodexAcpSessionPreferences {
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) fast_mode: Option<bool>,
}

/// Safe provider policy derived from authoritative GameEngine ACP access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexAcpPolicy {
    pub(crate) mode_id: &'static str,
}

/// One process command owned by the Codex adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexAcpCommand {
    pub(crate) program: OsString,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

/// Codex runtime adapter delegating ACP protocol mechanics to [`AcpProcessRuntime`].
pub(crate) struct CodexAcpRuntime {
    descriptor: AcpAgentDescriptor,
    placement: ExternalAgentExecutionPlacement,
    preferences: CodexAcpSessionPreferences,
}

impl CodexAcpRuntime {
    /// Discovers the pinned adapter and constructs one registry-ready runtime.
    pub(crate) fn discover(
        placement: ExternalAgentExecutionPlacement,
        preferences: CodexAcpSessionPreferences,
    ) -> Result<Self, AcpRuntimeError> {
        let probe = probe_codex_acp(&placement);
        if !probe.availability.is_available() {
            return Err(AcpRuntimeError::Transport(
                probe
                    .setup_diagnostic()
                    .unwrap_or_else(|| "Codex ACP setup is unavailable".to_owned()),
            ));
        }
        let (executable, arguments) =
            placed_launch_command(&placement, OsString::from(CODEX_ACP_EXECUTABLE), Vec::new());
        Ok(Self {
            descriptor: AcpAgentDescriptor {
                id: CODEX_ACP_DESCRIPTOR_ID.to_owned(),
                executable,
                arguments,
                environment: BTreeMap::new(),
                capabilities: AcpCapabilities {
                    session_config_options: true,
                    mcp_http: true,
                    ..AcpCapabilities::default()
                },
                runtime_identity: probe.expected_identity,
            },
            placement,
            preferences,
        })
    }
}

impl AcpAgentRuntime for CodexAcpRuntime {
    fn descriptor(&self) -> &AcpAgentDescriptor {
        &self.descriptor
    }

    fn open_session(
        &mut self,
        request: AcpSessionOpenRequest,
    ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
        preflight_codex_acp_session_environment(&self.placement, &request.binding)
            .map_err(AcpRuntimeError::Transport)?;

        let command = codex_acp_server_command(&self.placement, &request.binding);
        let mut descriptor = self.descriptor.clone();
        descriptor.executable = command.program;
        descriptor.arguments = command.arguments;
        descriptor.environment = command.environment.into_iter().collect();

        let mut runtime = AcpProcessRuntime::new(descriptor)?;
        let mut session = runtime.open_session(request.clone())?;
        for (option_id, value) in codex_acp_session_config(&request.binding, &self.preferences)? {
            session.set_session_config_option(&option_id, &value)?;
        }
        Ok(session)
    }
}

/// Maps authoritative GameEngine access to the narrowest Codex ACP mode.
///
/// `agent-full-access` is intentionally never selected. Provider sandboxing is
/// defense in depth and never replaces Agent Host permission or MCP authority.
pub(crate) fn codex_acp_policy(binding: &AcpSessionBinding) -> CodexAcpPolicy {
    match binding.mcp.access {
        AcpMcpAccessLevel::ReadOnly => CodexAcpPolicy {
            mode_id: READ_ONLY_MODE_ID,
        },
        AcpMcpAccessLevel::AgentRunBoundReadWrite => CodexAcpPolicy {
            mode_id: AGENT_MODE_ID,
        },
    }
}

/// Probes the selected execution environment for the exact pinned adapter.
pub(crate) fn probe_codex_acp(placement: &ExternalAgentExecutionPlacement) -> CodexAcpProbe {
    let availability =
        match command_output(placement, OsStr::new(CODEX_ACP_EXECUTABLE), ["--version"]) {
            Ok((true, output)) if output_reports_exact_version(&output) => {
                CodexAcpAvailability::Available
            }
            Ok((true, output)) => CodexAcpAvailability::VersionMismatch {
                expected: CODEX_ACP_PACKAGE_VERSION.to_owned(),
                observed: extract_version(&output),
            },
            Ok((false, output)) => {
                let detail = output.trim();
                CodexAcpAvailability::ProbeFailed(if detail.is_empty() {
                    "`codex-acp --version` exited unsuccessfully".to_owned()
                } else {
                    detail.to_owned()
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => CodexAcpAvailability::Missing,
            Err(error) => CodexAcpAvailability::ProbeFailed(error.to_string()),
        };
    CodexAcpProbe {
        availability,
        installer_available: installer_is_available(placement),
        expected_identity: AcpRuntimeIdentity::stable(
            CODEX_ACP_PACKAGE_NAME,
            Some(CODEX_ACP_PACKAGE_VERSION.to_owned()),
        ),
    }
}

/// Builds the pinned npm installation command for the selected environment.
pub(crate) fn codex_acp_install_command(
    placement: &ExternalAgentExecutionPlacement,
) -> CodexAcpCommand {
    let npm = match placement.environment {
        ExternalAgentExecutionEnvironment::WindowsNative => "npm.cmd",
        ExternalAgentExecutionEnvironment::Wsl2Linux => "npm",
    };
    let (program, arguments) = placed_launch_command(
        placement,
        OsString::from(npm),
        vec![
            OsString::from("install"),
            OsString::from("-g"),
            OsString::from(CODEX_ACP_INSTALL_PACKAGE),
        ],
    );
    CodexAcpCommand {
        program,
        arguments,
        environment: Vec::new(),
    }
}

/// Builds the long-lived Codex ACP stdio server command for one binding.
pub(crate) fn codex_acp_server_command(
    placement: &ExternalAgentExecutionPlacement,
    binding: &AcpSessionBinding,
) -> CodexAcpCommand {
    let (program, arguments) =
        placed_launch_command(placement, OsString::from(CODEX_ACP_EXECUTABLE), Vec::new());
    let mut environment = vec![(
        OsString::from(INITIAL_AGENT_MODE_ENV),
        OsString::from(codex_acp_policy(binding).mode_id),
    )];
    if placement.environment == ExternalAgentExecutionEnvironment::Wsl2Linux {
        environment.push(wsl_environment_forwarding(&environment, &[]));
    }
    CodexAcpCommand {
        program,
        arguments,
        environment,
    }
}

/// Proves WSL can reach the same loopback Editor MCP endpoint before launch.
pub(crate) fn preflight_codex_acp_session_environment(
    placement: &ExternalAgentExecutionPlacement,
    binding: &AcpSessionBinding,
) -> Result<(), String> {
    probe_wsl_loopback_reachability(
        placement,
        binding.mcp.endpoint(),
        binding.mcp.authorization_token(),
    )
}

/// Maps binding policy and user preferences to formal ACP config-option writes.
fn codex_acp_session_config(
    binding: &AcpSessionBinding,
    preferences: &CodexAcpSessionPreferences,
) -> Result<Vec<(String, String)>, AcpRuntimeError> {
    let mut selections = vec![(
        MODE_CONFIG_ID.to_owned(),
        codex_acp_policy(binding).mode_id.to_owned(),
    )];
    if let Some(model) = non_empty_preference("model", preferences.model.as_deref())? {
        selections.push((MODEL_CONFIG_ID.to_owned(), model.to_owned()));
    }
    if let Some(effort) =
        non_empty_preference("reasoning effort", preferences.reasoning_effort.as_deref())?
    {
        selections.push((REASONING_EFFORT_CONFIG_ID.to_owned(), effort.to_owned()));
    }
    if let Some(enabled) = preferences.fast_mode {
        selections.push((
            FAST_MODE_CONFIG_ID.to_owned(),
            if enabled { FAST_MODE_ON } else { FAST_MODE_OFF }.to_owned(),
        ));
    }
    Ok(selections)
}

fn non_empty_preference<'a>(
    name: &str,
    value: Option<&'a str>,
) -> Result<Option<&'a str>, AcpRuntimeError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AcpRuntimeError::Unsupported(format!(
            "Codex ACP {name} preference must not be empty"
        )));
    }
    Ok(Some(trimmed))
}

fn output_reports_exact_version(output: &str) -> bool {
    extract_version(output).as_deref() == Some(CODEX_ACP_PACKAGE_VERSION)
}

fn extract_version(output: &str) -> Option<String> {
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

    fn read_only_binding() -> AcpSessionBinding {
        AcpSessionBinding::read_only("session-1", "http://127.0.0.1:4000/mcp", "secret")
            .expect("read-only binding")
    }

    fn run_bound_binding() -> AcpSessionBinding {
        AcpSessionBinding::run_bound("session-1", "run-1", "http://127.0.0.1:4000/mcp", "secret")
            .expect("run-bound binding")
    }

    #[test]
    fn exact_adapter_version_is_required() {
        assert_eq!(
            extract_version("@agentclientprotocol/codex-acp 1.6.2\n"),
            Some("1.6.2".to_owned())
        );
        assert!(output_reports_exact_version("codex-acp 1.6.2"));
        assert!(!output_reports_exact_version("codex-acp 1.6.3"));
    }

    #[test]
    fn gameengine_access_never_maps_to_full_access() {
        assert_eq!(
            codex_acp_policy(&read_only_binding()).mode_id,
            READ_ONLY_MODE_ID
        );
        assert_eq!(
            codex_acp_policy(&run_bound_binding()).mode_id,
            AGENT_MODE_ID
        );
    }

    #[test]
    fn session_preferences_are_formal_value_id_writes() {
        let selections = codex_acp_session_config(
            &run_bound_binding(),
            &CodexAcpSessionPreferences {
                model: Some("gpt-5.3-codex".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                fast_mode: Some(true),
            },
        )
        .expect("config selections");
        assert_eq!(
            selections,
            vec![
                (MODE_CONFIG_ID.to_owned(), AGENT_MODE_ID.to_owned()),
                (MODEL_CONFIG_ID.to_owned(), "gpt-5.3-codex".to_owned()),
                (REASONING_EFFORT_CONFIG_ID.to_owned(), "high".to_owned()),
                (FAST_MODE_CONFIG_ID.to_owned(), FAST_MODE_ON.to_owned()),
            ]
        );
    }

    #[test]
    fn install_command_pins_only_the_codex_acp_package() {
        let plan = codex_acp_install_command(&ExternalAgentExecutionPlacement::windows_native());
        assert!(
            plan.arguments
                .iter()
                .any(|argument| { argument.to_string_lossy() == CODEX_ACP_INSTALL_PACKAGE })
        );
        assert!(
            !plan
                .arguments
                .iter()
                .any(|argument| { argument.to_string_lossy() == "@openai/codex@0.148.0" })
        );
    }
}
