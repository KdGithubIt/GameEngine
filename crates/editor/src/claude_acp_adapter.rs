//! Claude-specific discovery and launch metadata for the common ACP runtime.
//!
//! This module owns provider installation/authentication probes and immutable
//! launch metadata only. ACP wire I/O, permissions, MCP authority, validation,
//! and completion remain in the common runtime and Agent Host layers.

use crate::acp_agent_runtime::{AcpAgentDescriptor, AcpCapabilities, AcpRuntimeIdentity};
use crate::external_agent_provider::{
    ExternalAgentExecutionEnvironment, ExternalAgentExecutionPlacement,
    claude_credential_present, command_output_with_environment, placed_launch_command,
    wsl_environment_forwarding,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;

/// Descriptor ID registered for Claude ACP.
pub(crate) const CLAUDE_ACP_AGENT_ID: &str = "claude.acp";
/// npm package that publishes the Claude ACP adapter.
pub(crate) const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
/// Executable published by [`CLAUDE_ACP_PACKAGE`].
pub(crate) const CLAUDE_ACP_EXECUTABLE: &str = "claude-agent-acp";
/// Environment override understood by the adapter for its wrapped Claude Code CLI.
pub(crate) const CLAUDE_CODE_EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";

/// Machine-local configuration used to discover one Claude ACP adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpConfig {
    pub(crate) executable: OsString,
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

/// Provider-owned setup action offered for an unavailable Claude ACP runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeAcpSetupAction {
    Install,
    SignIn,
}

/// Stable diagnostic category for Claude ACP discovery/setup failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeAcpDiagnosticCode {
    AdapterUnavailable,
    AdapterVersionUnavailable,
    ClaudeCodeUnavailable,
    ClaudeCodeVersionUnavailable,
    AuthenticationRequired,
    AuthenticationProbeFailed,
}

/// Actionable Claude ACP discovery failure with no credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpDiagnostic {
    pub(crate) code: ClaudeAcpDiagnosticCode,
    pub(crate) message: String,
    pub(crate) setup_action: Option<ClaudeAcpSetupAction>,
}

impl std::fmt::Display for ClaudeAcpDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClaudeAcpDiagnostic {}

/// One provider setup command with placement-specific environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpSetupPlan {
    pub(crate) program: OsString,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

/// Ready-to-register Claude descriptor and wrapped CLI identity evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeAcpRegistration {
    pub(crate) descriptor: AcpAgentDescriptor,
    pub(crate) claude_code_version: String,
}

/// Discovers a ready Claude ACP runtime using the existing provider placement rules.
pub(crate) fn discover_claude_acp(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
) -> Result<ClaudeAcpRegistration, ClaudeAcpDiagnostic> {
    discover_claude_acp_with(config, placement, |program, arguments, environment| {
        command_output_with_environment(placement, program, arguments.iter(), environment)
    })
}

/// Builds the provider-owned install/sign-in command for a Claude ACP diagnostic.
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

fn discover_claude_acp_with<F>(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
    mut run: F,
) -> Result<ClaudeAcpRegistration, ClaudeAcpDiagnostic>
where
    F: FnMut(&OsStr, &[OsString], &[(OsString, OsString)]) -> io::Result<(bool, String)>,
{
    let environment = launch_environment(config, placement);
    let adapter_version_output = run(
        &config.executable,
        &[OsString::from("--version")],
        &environment,
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
        &environment,
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
        &environment,
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
    let environment = environment.into_iter().collect::<BTreeMap<_, _>>();
    Ok(ClaudeAcpRegistration {
        descriptor: AcpAgentDescriptor {
            id: CLAUDE_ACP_AGENT_ID.to_owned(),
            executable,
            arguments,
            environment,
            // Descriptor capabilities are minimum requirements. Optional
            // load/resume/list/config support is retained only after live ACP
            // negotiation in the common transport.
            capabilities: AcpCapabilities {
                mcp_http: true,
                ..AcpCapabilities::default()
            },
            runtime_identity: AcpRuntimeIdentity::stable(
                CLAUDE_ACP_PACKAGE,
                Some(adapter_version),
            ),
        },
        claude_code_version,
    })
}

fn launch_environment(
    config: &ClaudeAcpConfig,
    placement: &ExternalAgentExecutionPlacement,
) -> Vec<(OsString, OsString)> {
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

fn adapter_unavailable(detail: String) -> ClaudeAcpDiagnostic {
    diagnostic(
        ClaudeAcpDiagnosticCode::AdapterUnavailable,
        format!(
            "Claude ACP is unavailable ({detail}). Install {CLAUDE_ACP_PACKAGE} so `{CLAUDE_ACP_EXECUTABLE}` is available in the selected execution environment."
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
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+' | '_'))
        })
        .find(|token| {
            token.matches('.').count() >= 2
                && token
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_digit())
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_runner(
        _program: &OsStr,
        arguments: &[OsString],
        _environment: &[(OsString, OsString)],
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
                Ok((true, r#"{"loggedIn":true}"#.to_owned()))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unexpected Claude ACP probe",
            )),
        }
    }

    #[test]
    fn discovery_keeps_optional_capabilities_negotiated() {
        let registration = discover_claude_acp_with(
            &ClaudeAcpConfig::default(),
            &ExternalAgentExecutionPlacement::windows_native(),
            ready_runner,
        )
        .expect("ready Claude ACP should be discoverable");

        assert_eq!(registration.descriptor.id, CLAUDE_ACP_AGENT_ID);
        assert!(registration.descriptor.capabilities.mcp_http);
        assert!(!registration.descriptor.capabilities.session_resume);
        assert_eq!(registration.claude_code_version, "2.1.237");
    }

    #[test]
    fn launch_environment_is_descriptor_authority() {
        let placement = ExternalAgentExecutionPlacement {
            environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            distribution: "Ubuntu-24.04".to_owned(),
        };
        let config = ClaudeAcpConfig {
            executable: OsString::from(CLAUDE_ACP_EXECUTABLE),
            claude_code_executable: Some(OsString::from("/opt/claude/claude")),
        };
        let registration =
            discover_claude_acp_with(&config, &placement, ready_runner).expect("ready Claude ACP");

        assert_eq!(
            registration
                .descriptor
                .environment
                .get(OsStr::new(CLAUDE_CODE_EXECUTABLE_ENV)),
            Some(&OsString::from("/opt/claude/claude"))
        );
        assert!(registration.descriptor.environment.contains_key(OsStr::new("WSLENV")));
    }

    #[test]
    fn signed_out_claude_fails_closed() {
        let result = discover_claude_acp_with(
            &ClaudeAcpConfig::default(),
            &ExternalAgentExecutionPlacement::windows_native(),
            |_program, arguments, environment| {
                if arguments
                    == [
                        OsString::from("--cli"),
                        OsString::from("auth"),
                        OsString::from("status"),
                    ]
                {
                    return Ok((true, r#"{"loggedIn":false}"#.to_owned()));
                }
                ready_runner(OsStr::new(CLAUDE_ACP_EXECUTABLE), arguments, environment)
            },
        );
        let diagnostic = result.expect_err("signed-out Claude must fail closed");
        assert_eq!(
            diagnostic.code,
            ClaudeAcpDiagnosticCode::AuthenticationRequired
        );
    }
}
