//! Claude-specific discovery and launch metadata for the ACP runtime.
//!
//! This adapter owns only Claude ACP installation, authentication, executable
//! placement, runtime identity, and provider metadata. ACP transport/session
//! behavior remains provider-neutral in [`crate::acp_agent_runtime`].

use crate::acp_agent_runtime::{AcpAgentDescriptor, AcpCapabilities, AcpRuntimeIdentity};
use crate::external_agent_provider::{
    claude_credential_present, command_output, placed_launch_command, wsl_environment_forwarding,
    ExternalAgentExecutionEnvironment, ExternalAgentExecutionPlacement,
};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;

/// Descriptor ID used by the Claude ACP adapter.
pub(crate) const CLAUDE_ACP_AGENT_ID: &str = "claude.acp";
/// npm package that publishes the supported Claude ACP executable.
pub(crate) const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
/// Executable published by [`CLAUDE_ACP_PACKAGE`].
pub(crate) const CLAUDE_ACP_EXECUTABLE: &str = "claude-agent-acp";
/// Environment variable understood by the Claude ACP adapter for overriding Claude Code.
pub(crate) const CLAUDE_CODE_EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";

const ACP_PERMISSION_REQUEST_METHOD: &str = "session/request_permission";
const ACP_SESSION_UPDATE_METHOD: &str = "session/update";
const ACP_TOOL_CALL_UPDATE_KIND: &str = "tool_call_update";
const ACP_TOOL_CALL_KIND: &str = "tool_call";

/// Machine-local configuration for locating the Claude ACP runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpConfig {
    /// ACP executable or absolute executable path.
    pub(crate) executable: OsString,
    /// Optional Claude Code executable forwarded through the adapter.
    pub(crate) claude_code_executable: Option<OsString>,
}

impl Default for ClaudeAcpConfig {
    fn default() -> Self {
        Self {
            executable: OsString::from(CLAUDE_ACP_EXECUTABLE),
            claude_code_executable: None,
        }
    }
}

/// Environment variables required when launching one Claude ACP process.
pub(crate) type ClaudeAcpEnvironment = Vec<(OsString, OsString)>;

/// Claude ACP provider metadata used by the provider-neutral event mapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpEventMappingMetadata {
    /// ACP request method that carries permission options and the related tool call.
    pub(crate) permission_request_method: &'static str,
    /// ACP notification method that carries semantic session updates.
    pub(crate) session_update_method: &'static str,
    /// Session update discriminator for a newly created tool call.
    pub(crate) tool_call_kind: &'static str,
    /// Session update discriminator for later tool call state changes.
    pub(crate) tool_call_update_kind: &'static str,
}

/// Ready-to-register Claude ACP descriptor plus launch-only metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpRegistration {
    /// Provider-neutral descriptor consumed by the ACP runtime registry.
    pub(crate) descriptor: AcpAgentDescriptor,
    /// Environment applied to the child process in addition to the descriptor command.
    pub(crate) environment: ClaudeAcpEnvironment,
    /// Exact wrapped Claude Code version reported by the ACP package.
    pub(crate) claude_code_version: String,
    /// Standard ACP event shapes emitted by this adapter.
    pub(crate) event_mapping: ClaudeAcpEventMappingMetadata,
}

/// Setup action that can resolve a Claude ACP diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeAcpSetupAction {
    /// Install the ACP adapter package.
    Install,
    /// Start Claude Code's provider-owned authentication flow through the ACP adapter.
    SignIn,
}

/// Stable diagnostic category for Claude ACP setup failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeAcpDiagnosticCode {
    /// The ACP adapter executable could not be started successfully.
    AdapterUnavailable,
    /// The ACP adapter did not report a usable exact version.
    AdapterVersionUnavailable,
    /// The wrapped Claude Code executable could not be started successfully.
    ClaudeCodeUnavailable,
    /// The wrapped Claude Code executable did not report a usable exact version.
    ClaudeCodeVersionUnavailable,
    /// Claude Code is installed but has no provider credential.
    AuthenticationRequired,
    /// Authentication state could not be determined safely.
    AuthenticationProbeFailed,
}

/// Actionable diagnostic returned instead of an unusable descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpDiagnostic {
    /// Stable machine-readable diagnostic category.
    pub(crate) code: ClaudeAcpDiagnosticCode,
    /// Developer-facing explanation with no credential material.
    pub(crate) message: String,
    /// Provider-owned setup action that can resolve the diagnostic when known.
    pub(crate) setup_action: Option<ClaudeAcpSetupAction>,
}

impl std::fmt::Display for ClaudeAcpDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClaudeAcpDiagnostic {}

/// Discovers a ready Claude ACP runtime using the existing external-agent placement rules.
///
/// The returned descriptor identifies the exact ACP package version reported by
/// `claude-agent-acp --version`. Authentication is verified through the wrapped
/// Claude Code CLI before the descriptor is offered for registration.
///
/// # Errors
///
/// Returns an actionable setup diagnostic when the adapter, wrapped Claude Code,
/// exact version, or provider authentication is unavailable.
pub(crate) fn discover_claude_acp(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
) -> Result<ClaudeAcpRegistration, ClaudeAcpDiagnostic> {
    discover_claude_acp_with(config, placement, |program, arguments| {
        command_output(placement, program, arguments.iter())
    })
}

/// Builds the command used for an installation or provider sign-in action.
pub(crate) fn build_claude_acp_setup_plan(
    action: ClaudeAcpSetupAction,
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
) -> ClaudeAcpSetupPlan {
    let (program, arguments) = match action {
        ClaudeAcpSetupAction::Install => (
            OsString::from("npm"),
            vec![
                OsString::from("install"),
                OsString::from("--global"),
                OsString::from(CLAUDE_ACP_PACKAGE),
            ],
        ),
        ClaudeAcpSetupAction::SignIn => (
            config.executable.clone(),
            vec![
                OsString::from("--cli"),
                OsString::from("auth"),
                OsString::from("login"),
            ],
        ),
    };
    let (program, arguments) = placed_launch_command(placement, program, arguments);
    ClaudeAcpSetupPlan {
        program,
        arguments,
        environment: launch_environment(config, placement),
    }
}

/// One provider setup command with the environment needed by its placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpSetupPlan {
    /// Program to start on the host OS.
    pub(crate) program: OsString,
    /// Exact ordered arguments for the setup process.
    pub(crate) arguments: Vec<OsString>,
    /// Environment applied to the setup process.
    pub(crate) environment: ClaudeAcpEnvironment,
}

fn discover_claude_acp_with<F>(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
    mut run: F,
) -> Result<ClaudeAcpRegistration, ClaudeAcpDiagnostic>
where
    F: FnMut(&OsStr, &[OsString]) -> io::Result<(bool, String)>,
{
    let adapter_version_output = run(
        &config.executable,
        &[OsString::from("--version")],
    )
    .map_err(|error| adapter_unavailable(error.to_string()))?;
    if !adapter_version_output.0 {
        return Err(adapter_unavailable(
            "the version probe exited unsuccessfully".to_owned(),
        ));
    }
    let adapter_version = extract_version(&adapter_version_output.1).ok_or_else(|| {
        diagnostic(
            ClaudeAcpDiagnosticCode::AdapterVersionUnavailable,
            "Claude ACP is installed, but `claude-agent-acp --version` did not report a usable exact version.",
            Some(ClaudeAcpSetupAction::Install),
        )
    })?;

    let claude_version_output = run(
        &config.executable,
        &[OsString::from("--cli"), OsString::from("--version")],
    )
    .map_err(|error| {
        diagnostic(
            ClaudeAcpDiagnosticCode::ClaudeCodeUnavailable,
            format!("Claude ACP could not start its wrapped Claude Code executable: {error}"),
            Some(ClaudeAcpSetupAction::Install),
        )
    })?;
    if !claude_version_output.0 {
        return Err(diagnostic(
            ClaudeAcpDiagnosticCode::ClaudeCodeUnavailable,
            "Claude ACP is installed, but its wrapped Claude Code executable did not start successfully.",
            Some(ClaudeAcpSetupAction::Install),
        ));
    }
    let claude_code_version = extract_version(&claude_version_output.1).ok_or_else(|| {
        diagnostic(
            ClaudeAcpDiagnosticCode::ClaudeCodeVersionUnavailable,
            "Claude ACP started Claude Code, but the CLI did not report a usable exact version.",
            Some(ClaudeAcpSetupAction::Install),
        )
    })?;

    let auth_output = run(
        &config.executable,
        &[
            OsString::from("--cli"),
            OsString::from("auth"),
            OsString::from("status"),
        ],
    )
    .map_err(|error| {
        diagnostic(
            ClaudeAcpDiagnosticCode::AuthenticationProbeFailed,
            format!("Claude ACP could not inspect Claude Code authentication: {error}"),
            Some(ClaudeAcpSetupAction::SignIn),
        )
    })?;
    if !claude_credential_present(&auth_output.1, auth_output.0) {
        return Err(diagnostic(
            ClaudeAcpDiagnosticCode::AuthenticationRequired,
            "Claude ACP and Claude Code are available, but Claude Code authentication is not ready.",
            Some(ClaudeAcpSetupAction::SignIn),
        ));
    }

    let (executable, arguments) =
        placed_launch_command(placement, config.executable.clone(), Vec::new());
    Ok(ClaudeAcpRegistration {
        descriptor: AcpAgentDescriptor {
            id: CLAUDE_ACP_AGENT_ID.to_owned(),
            executable,
            arguments,
            capabilities: claude_acp_capabilities(),
            runtime_identity: AcpRuntimeIdentity::stable(CLAUDE_ACP_PACKAGE, Some(adapter_version)),
        },
        environment: launch_environment(config, placement),
        claude_code_version,
        event_mapping: ClaudeAcpEventMappingMetadata {
            permission_request_method: ACP_PERMISSION_REQUEST_METHOD,
            session_update_method: ACP_SESSION_UPDATE_METHOD,
            tool_call_kind: ACP_TOOL_CALL_KIND,
            tool_call_update_kind: ACP_TOOL_CALL_UPDATE_KIND,
        },
    })
}

fn adapter_unavailable(detail: String) -> ClaudeAcpDiagnostic {
    diagnostic(
        ClaudeAcpDiagnosticCode::AdapterUnavailable,
        format!(
            "Claude ACP is unavailable ({detail}). Install {CLAUDE_ACP_PACKAGE} with Node.js 22 or newer so `{CLAUDE_ACP_EXECUTABLE}` is on PATH."
        ),
        Some(ClaudeAcpSetupAction::Install),
    )
}

fn diagnostic(
    code: ClaudeAcpDiagnosticCode,
    message: impl Into<String>,
    setup_action: Option<ClaudeAcpSetupAction>,
) -> ClaudeAcpDiagnostic {
    ClaudeAcpDiagnostic {
        code,
        message: message.into(),
        setup_action,
    }
}

fn extract_version(output: &str) -> Option<String> {
    output
        .split(|character: char| {
            !(character.is_ascii_alphanumeric()
                || matches!(character, '.' | '-' | '+' | '_'))
        })
        .find(|token| {
            token
                .split('.')
                .take(3)
                .all(|segment| !segment.is_empty())
                && token.matches('.').count() >= 2
                && token.chars().next().is_some_and(|character| character.is_ascii_digit())
        })
        .map(str::to_owned)
}

fn claude_acp_capabilities() -> AcpCapabilities {
    AcpCapabilities {
        session_load: true,
        session_resume: true,
        session_list: true,
        session_config_options: true,
        mcp_http: true,
        mcp_sse: true,
        mcp_over_acp: false,
        extensions: BTreeSet::from([
            "prompt.embedded_context".to_owned(),
            "prompt.image".to_owned(),
            "session.close".to_owned(),
            "session.fork".to_owned(),
        ]),
    }
}

fn launch_environment(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
) -> ClaudeAcpEnvironment {
    let mut environment = config
        .claude_code_executable
        .as_ref()
        .map(|executable| {
            vec![(
                OsString::from(CLAUDE_CODE_EXECUTABLE_ENV),
                executable.clone(),
            )]
        })
        .unwrap_or_default();
    if placement.environment == ExternalAgentExecutionEnvironment::Wsl2Linux
        && !environment.is_empty()
    {
        environment.push(wsl_environment_forwarding(&environment, &[]));
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_runner(
        _program: &OsStr,
        arguments: &[OsString],
    ) -> io::Result<(bool, String)> {
        let args = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        match args.as_slice() {
            [version] if version == "--version" => Ok((true, "0.70.0\n".to_owned())),
            [cli, version] if cli == "--cli" && version == "--version" => {
                Ok((true, "2.1.237 (Claude Code)\n".to_owned()))
            }
            [cli, auth, status]
                if cli == "--cli" && auth == "auth" && status == "status" =>
            {
                Ok((true, r#"{"loggedIn":true,"authMethod":"claude.ai"}"#.to_owned()))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unexpected test probe",
            )),
        }
    }

    #[test]
    fn discovery_builds_exact_descriptor_without_provider_enum() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let registration =
            discover_claude_acp_with(&ClaudeAcpConfig::default(), &placement, ready_runner)
                .expect("ready Claude ACP should be discoverable");

        assert_eq!(registration.descriptor.id, CLAUDE_ACP_AGENT_ID);
        assert_eq!(
            registration.descriptor.runtime_identity.agent_name,
            CLAUDE_ACP_PACKAGE
        );
        assert_eq!(
            registration.descriptor.runtime_identity.agent_version.as_deref(),
            Some("0.70.0")
        );
        assert_eq!(registration.claude_code_version, "2.1.237");
        assert!(registration.descriptor.capabilities.session_resume);
        assert!(registration.descriptor.capabilities.mcp_http);
        assert!(!registration.descriptor.capabilities.mcp_over_acp);
    }

    #[test]
    fn authentication_failure_returns_sign_in_diagnostic() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let result = discover_claude_acp_with(
            &ClaudeAcpConfig::default(),
            &placement,
            |_program, arguments| {
                if arguments
                    == [
                        OsString::from("--cli"),
                        OsString::from("auth"),
                        OsString::from("status"),
                    ]
                {
                    return Ok((false, r#"{"loggedIn":false}"#.to_owned()));
                }
                ready_runner(OsStr::new(CLAUDE_ACP_EXECUTABLE), arguments)
            },
        );

        let diagnostic = result.expect_err("signed-out Claude must not register as ready");
        assert_eq!(
            diagnostic.code,
            ClaudeAcpDiagnosticCode::AuthenticationRequired
        );
        assert_eq!(
            diagnostic.setup_action,
            Some(ClaudeAcpSetupAction::SignIn)
        );
    }

    #[test]
    fn event_metadata_uses_standard_acp_permission_and_tool_updates() {
        let registration = discover_claude_acp_with(
            &ClaudeAcpConfig::default(),
            &ExternalAgentExecutionPlacement::windows_native(),
            ready_runner,
        )
        .expect("ready Claude ACP should be discoverable");

        assert_eq!(
            registration.event_mapping.permission_request_method,
            "session/request_permission"
        );
        assert_eq!(
            registration.event_mapping.session_update_method,
            "session/update"
        );
        assert_eq!(registration.event_mapping.tool_call_kind, "tool_call");
        assert_eq!(
            registration.event_mapping.tool_call_update_kind,
            "tool_call_update"
        );
    }

    #[test]
    fn wsl_override_is_forwarded_without_becoming_descriptor_data() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: "Ubuntu-24.04".to_owned(),
        };
        let config = ClaudeAcpConfig {
            executable: OsString::from(CLAUDE_ACP_EXECUTABLE),
            claude_code_executable: Some(OsString::from("/opt/claude/claude")),
        };
        let registration = discover_claude_acp_with(&config, &placement, ready_runner)
            .expect("ready WSL Claude ACP should be discoverable");

        assert_eq!(registration.descriptor.executable, OsString::from("wsl.exe"));
        assert!(registration
            .descriptor
            .arguments
            .contains(&OsString::from(CLAUDE_ACP_EXECUTABLE)));
        assert!(registration.environment.iter().any(|(name, value)| {
            name.as_os_str() == OsStr::new(CLAUDE_CODE_EXECUTABLE_ENV)
                && value.as_os_str() == OsStr::new("/opt/claude/claude")
        }));
        assert!(registration.environment.iter().any(|(name, value)| {
            name.as_os_str() == OsStr::new("WSLENV")
                && value
                    .to_string_lossy()
                    .split(':')
                    .any(|entry| entry == CLAUDE_CODE_EXECUTABLE_ENV)
        }));
    }

    #[test]
    fn setup_plans_use_the_acp_package_and_wrapped_cli() {
        let placement = ExternalAgentExecutionPlacement::windows_native();
        let config = ClaudeAcpConfig::default();

        let install =
            build_claude_acp_setup_plan(ClaudeAcpSetupAction::Install, &config, &placement);
        assert!(install.arguments.contains(&OsString::from(CLAUDE_ACP_PACKAGE)));

        let sign_in =
            build_claude_acp_setup_plan(ClaudeAcpSetupAction::SignIn, &config, &placement);
        assert!(sign_in.arguments.windows(3).any(|arguments| {
            arguments
                == [
                    OsString::from("--cli"),
                    OsString::from("auth"),
                    OsString::from("login"),
                ]
        }));
    }
}
