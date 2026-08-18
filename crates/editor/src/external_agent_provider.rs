//! Provider-specific adapters for external coding-agent runtimes.
//!
//! Adapters translate provider lifecycle and wire-protocol details into the
//! existing Agent Host boundary. They do not own authoring semantics,
//! permissions, code apply, validation, or completion gates.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Stdio};

pub(crate) const GAMEENGINE_AGENT_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const GAMEENGINE_MCP_SERVER_NAME: &str = "gameengine_editor";
const GAMEENGINE_MCP_TOKEN_ENV: &str = "GAMEENGINE_MCP_AUTH_TOKEN";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentProviderKind {
    ClaudeCode,
    Codex,
    #[default]
    Generic,
}

impl ExternalAgentProviderKind {
    pub(crate) const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Generic];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Generic => "Generic command",
        }
    }

    pub(crate) fn run_label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Generic => "generic-external",
        }
    }

    fn program(self) -> Option<&'static OsStr> {
        match self {
            Self::ClaudeCode => Some(OsStr::new("claude")),
            Self::Codex => Some(OsStr::new("codex")),
            Self::Generic => None,
        }
    }

    pub(crate) fn capabilities(self) -> ExternalAgentProviderCapabilities {
        match self {
            Self::ClaudeCode | Self::Codex => ExternalAgentProviderCapabilities {
                provider_managed_auth: true,
                mcp_injection: true,
                structured_events: true,
                host_cancellation: true,
            },
            Self::Generic => ExternalAgentProviderCapabilities {
                provider_managed_auth: false,
                mcp_injection: true,
                structured_events: true,
                host_cancellation: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderCapabilities {
    pub(crate) provider_managed_auth: bool,
    pub(crate) mcp_injection: bool,
    pub(crate) structured_events: bool,
    pub(crate) host_cancellation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentDiscoveryStatus {
    Unchecked,
    Available,
    Unavailable,
}

impl ExternalAgentDiscoveryStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "not checked",
            Self::Available => "available",
            Self::Unavailable => "not found",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalAgentAuthStatus {
    Unchecked,
    Authenticated,
    SignInRequired,
    NotApplicable,
    Unavailable,
}

impl ExternalAgentAuthStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "not checked",
            Self::Authenticated => "authenticated",
            Self::SignInRequired => "sign-in required",
            Self::NotApplicable => "provider-managed status unavailable",
            Self::Unavailable => "provider unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderStatus {
    pub(crate) kind: ExternalAgentProviderKind,
    pub(crate) discovery: ExternalAgentDiscoveryStatus,
    pub(crate) auth: ExternalAgentAuthStatus,
}

impl ExternalAgentProviderStatus {
    pub(crate) fn unchecked(kind: ExternalAgentProviderKind) -> Self {
        let auth = if kind == ExternalAgentProviderKind::Generic {
            ExternalAgentAuthStatus::NotApplicable
        } else {
            ExternalAgentAuthStatus::Unchecked
        };
        Self {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Unchecked,
            auth,
        }
    }

    pub(crate) fn generic(configured: bool) -> Self {
        Self {
            kind: ExternalAgentProviderKind::Generic,
            discovery: if configured {
                ExternalAgentDiscoveryStatus::Available
            } else {
                ExternalAgentDiscoveryStatus::Unavailable
            },
            auth: ExternalAgentAuthStatus::NotApplicable,
        }
    }

    #[cfg(feature = "visual-validation")]
    pub(crate) fn visual_fixture(kind: ExternalAgentProviderKind) -> Self {
        Self {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: if kind == ExternalAgentProviderKind::Generic {
                ExternalAgentAuthStatus::NotApplicable
            } else {
                ExternalAgentAuthStatus::Authenticated
            },
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.discovery == ExternalAgentDiscoveryStatus::Available
            && matches!(
                self.auth,
                ExternalAgentAuthStatus::Authenticated | ExternalAgentAuthStatus::NotApplicable
            )
    }

    pub(crate) fn remote_json(&self) -> Value {
        let capabilities = self.kind.capabilities();
        serde_json::json!({
            "kind": self.kind.run_label(),
            "discovery": self.discovery.label(),
            "authentication": self.auth.label(),
            "capabilities": {
                "provider_managed_auth": capabilities.provider_managed_auth,
                "mcp_injection": capabilities.mcp_injection,
                "structured_events": capabilities.structured_events,
                "host_cancellation": capabilities.host_cancellation,
            }
        })
    }
}

pub(crate) fn probe_provider(
    kind: ExternalAgentProviderKind,
    generic_program: &str,
) -> ExternalAgentProviderStatus {
    if kind == ExternalAgentProviderKind::Generic {
        return ExternalAgentProviderStatus::generic(!generic_program.trim().is_empty());
    }
    let Some(program) = kind.program() else {
        return ExternalAgentProviderStatus::unchecked(kind);
    };
    let available = command_success(program, ["--version"]).unwrap_or(false);
    if !available {
        return ExternalAgentProviderStatus {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Unavailable,
            auth: ExternalAgentAuthStatus::Unavailable,
        };
    }
    let auth = match kind {
        ExternalAgentProviderKind::ClaudeCode => command_success(program, ["auth", "status"]),
        ExternalAgentProviderKind::Codex => command_success(program, ["login", "status"]),
        ExternalAgentProviderKind::Generic => Ok(true),
    };
    ExternalAgentProviderStatus {
        kind,
        discovery: ExternalAgentDiscoveryStatus::Available,
        auth: match auth {
            Ok(true) => ExternalAgentAuthStatus::Authenticated,
            Ok(false) => ExternalAgentAuthStatus::SignInRequired,
            Err(_) => ExternalAgentAuthStatus::SignInRequired,
        },
    }
}

fn command_success<I, S>(program: &OsStr, args: I) -> io::Result<bool>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
}

#[derive(Debug, Clone)]
pub(crate) struct ExternalAgentLaunchPlan {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
}

pub(crate) fn build_launch_plan(
    kind: ExternalAgentProviderKind,
    generic_program: &str,
    generic_args: &[String],
    prompt: &str,
    mcp_endpoint: &str,
) -> Result<ExternalAgentLaunchPlan, String> {
    match kind {
        ExternalAgentProviderKind::ClaudeCode => {
            let server = serde_json::json!({
                "type": "http",
                "url": mcp_endpoint,
                "headers": {
                    "Authorization": format!("Bearer ${{{GAMEENGINE_MCP_TOKEN_ENV}}}"),
                },
            });
            let mut servers = serde_json::Map::new();
            servers.insert(GAMEENGINE_MCP_SERVER_NAME.to_owned(), server);
            let mut root = serde_json::Map::new();
            root.insert("mcpServers".to_owned(), Value::Object(servers));
            let mcp_config = Value::Object(root).to_string();
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from("claude"),
                args: vec![
                    OsString::from("-p"),
                    OsString::from(prompt),
                    OsString::from("--output-format"),
                    OsString::from("stream-json"),
                    OsString::from("--verbose"),
                    OsString::from("--mcp-config"),
                    OsString::from(mcp_config),
                    OsString::from("--strict-mcp-config"),
                    OsString::from("--allowedTools"),
                    OsString::from("Edit"),
                    OsString::from("Write"),
                    OsString::from("mcp__gameengine_editor__*"),
                ],
            })
        }
        ExternalAgentProviderKind::Codex => {
            let mcp_url = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.url={}",
                toml_basic_string(mcp_endpoint)
            );
            let bearer_env = format!(
                "mcp_servers.{GAMEENGINE_MCP_SERVER_NAME}.bearer_token_env_var={}",
                toml_basic_string(GAMEENGINE_MCP_TOKEN_ENV)
            );
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from("codex"),
                args: vec![
                    OsString::from("exec"),
                    OsString::from("--json"),
                    OsString::from("--skip-git-repo-check"),
                    OsString::from("--sandbox"),
                    OsString::from("workspace-write"),
                    OsString::from("-c"),
                    OsString::from(mcp_url),
                    OsString::from("-c"),
                    OsString::from(bearer_env),
                    OsString::from(prompt),
                ],
            })
        }
        ExternalAgentProviderKind::Generic => {
            if generic_program.trim().is_empty() {
                return Err("Configure a generic external agent command before Go.".to_owned());
            }
            Ok(ExternalAgentLaunchPlan {
                program: OsString::from(generic_program.trim()),
                args: generic_args
                    .iter()
                    .map(|argument| OsString::from(argument.as_str()))
                    .collect(),
            })
        }
    }
}

fn toml_basic_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                quoted.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExternalAgentSemanticEvent {
    Progress {
        step: &'static str,
        detail: &'static str,
    },
    ToolAction {
        tool: String,
        action: &'static str,
        success: Option<bool>,
    },
    GameEngineProtocolPayload(String),
}

pub(crate) fn translate_provider_line(
    kind: ExternalAgentProviderKind,
    line: &str,
) -> Vec<ExternalAgentSemanticEvent> {
    match kind {
        ExternalAgentProviderKind::ClaudeCode => translate_claude_line(line),
        ExternalAgentProviderKind::Codex => translate_codex_line(line),
        ExternalAgentProviderKind::Generic => Vec::new(),
    }
}

fn translate_claude_line(line: &str) -> Vec<ExternalAgentSemanticEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            events.push(ExternalAgentSemanticEvent::Progress {
                step: "provider_connected",
                detail: "Claude Code initialized its external agent session.",
            });
        }
        Some("assistant") => {
            if let Some(content) = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            {
                for item in content {
                    match item.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let tool = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("provider_tool")
                                .to_owned();
                            events.push(ExternalAgentSemanticEvent::ToolAction {
                                tool,
                                action: "provider tool requested",
                                success: None,
                            });
                        }
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                collect_protocol_payloads(text, &mut events);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("result") => {
            events.push(ExternalAgentSemanticEvent::Progress {
                step: "provider_turn_finished",
                detail: "Claude Code returned control to the GameEngine host.",
            });
        }
        _ => {}
    }
    events
}

fn translate_codex_line(line: &str) -> Vec<ExternalAgentSemanticEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_connected",
            detail: "Codex initialized its external agent thread.",
        }),
        Some("turn.started") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_turn_started",
            detail: "Codex started a provider turn.",
        }),
        Some("turn.completed") => events.push(ExternalAgentSemanticEvent::Progress {
            step: "provider_turn_finished",
            detail: "Codex returned control to the GameEngine host.",
        }),
        Some("item.started") | Some("item.completed") => {
            if let Some(item) = value.get("item") {
                match item.get("type").and_then(Value::as_str) {
                    Some("mcp_tool_call") => {
                        let tool = item
                            .get("tool")
                            .or_else(|| item.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("mcp_tool")
                            .to_owned();
                        events.push(ExternalAgentSemanticEvent::ToolAction {
                            tool,
                            action: "provider MCP tool activity",
                            success: None,
                        });
                    }
                    Some("command_execution") => events.push(
                        ExternalAgentSemanticEvent::ToolAction {
                            tool: "provider.command".to_owned(),
                            action: "provider command activity",
                            success: None,
                        },
                    ),
                    Some("file_change") => events.push(ExternalAgentSemanticEvent::ToolAction {
                        tool: "workspace.file_change".to_owned(),
                        action: "provider workspace edit",
                        success: None,
                    }),
                    Some("agent_message") => {
                        if let Some(text) = item
                            .get("text")
                            .or_else(|| item.get("message"))
                            .and_then(Value::as_str)
                        {
                            collect_protocol_payloads(text, &mut events);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    events
}

fn collect_protocol_payloads(text: &str, events: &mut Vec<ExternalAgentSemanticEvent>) {
    for line in text.lines() {
        if let Some(payload) = line
            .trim()
            .strip_prefix(GAMEENGINE_AGENT_EVENT_PREFIX)
        {
            events.push(ExternalAgentSemanticEvent::GameEngineProtocolPayload(
                payload.to_owned(),
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ExternalAgentDiagnostics {
    authentication: bool,
    rate_limited: bool,
    mcp: bool,
    configuration: bool,
}

impl ExternalAgentDiagnostics {
    pub(crate) fn observe(&mut self, kind: ExternalAgentProviderKind, line: &str) {
        if kind == ExternalAgentProviderKind::Generic {
            return;
        }
        let lower = line.to_ascii_lowercase();
        self.authentication |= lower.contains("not logged in")
            || lower.contains("authentication failed")
            || lower.contains("unauthorized")
            || lower.contains("sign in required");
        self.rate_limited |= lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("\"status\":429")
            || lower.contains("status 429");
        self.mcp |= lower.contains("mcp")
            && (lower.contains("failed")
                || lower.contains("error")
                || lower.contains("unavailable"));
        self.configuration |= lower.contains("configuration error")
            || lower.contains("invalid config")
            || lower.contains("invalid configuration");
    }

    pub(crate) fn classify_exit(
        self,
        kind: ExternalAgentProviderKind,
        exit_code: Option<i32>,
    ) -> ExternalAgentFailureClassification {
        let provider = kind.label();
        if self.authentication {
            return ExternalAgentFailureClassification {
                message: format!("{provider} authentication is unavailable or expired."),
                retryable: false,
            };
        }
        if self.rate_limited {
            return ExternalAgentFailureClassification {
                message: format!("{provider} reported provider-side rate limiting."),
                retryable: true,
            };
        }
        if self.mcp {
            return ExternalAgentFailureClassification {
                message: format!("{provider} could not use the injected GameEngine MCP connection."),
                retryable: true,
            };
        }
        if self.configuration {
            return ExternalAgentFailureClassification {
                message: format!("{provider} rejected its provider configuration."),
                retryable: false,
            };
        }
        ExternalAgentFailureClassification {
            message: format!("{provider} exited unsuccessfully with {exit_code:?}."),
            retryable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalAgentFailureClassification {
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_launch_plan_preserves_direct_argument_semantics() {
        let args = vec![
            "--flag".to_owned(),
            "value".to_owned(),
            ";".to_owned(),
            "echo".to_owned(),
            "nope".to_owned(),
        ];
        let plan = build_launch_plan(
            ExternalAgentProviderKind::Generic,
            "custom-agent",
            &args,
            "ignored",
            "http://127.0.0.1:1/mcp",
        )
        .expect("generic plan");
        assert_eq!(plan.program, OsString::from("custom-agent"));
        assert_eq!(
            plan.args,
            vec![
                OsString::from("--flag"),
                OsString::from("value"),
                OsString::from(";"),
                OsString::from("echo"),
                OsString::from("nope"),
            ]
        );
    }

    #[test]
    fn claude_mcp_config_is_valid_json_and_uses_ephemeral_environment() {
        let plan = build_launch_plan(
            ExternalAgentProviderKind::ClaudeCode,
            "",
            &[],
            "task",
            "http://127.0.0.1:1234/mcp",
        )
        .expect("claude plan");
        let config_index = plan
            .args
            .iter()
            .position(|value| value == OsStr::new("--mcp-config"))
            .expect("mcp config flag");
        let config = plan.args[config_index + 1]
            .to_str()
            .expect("UTF-8 MCP config");
        let parsed: Value = serde_json::from_str(config).expect("valid MCP config JSON");
        let server = &parsed["mcpServers"][GAMEENGINE_MCP_SERVER_NAME];
        assert_eq!(server["url"], "http://127.0.0.1:1234/mcp");
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer ${GAMEENGINE_MCP_AUTH_TOKEN}"
        );
        let allowed_index = plan
            .args
            .iter()
            .position(|value| value == OsStr::new("--allowedTools"))
            .expect("allowed tools flag");
        assert_eq!(
            &plan.args[allowed_index + 1..],
            &[
                OsString::from("Edit"),
                OsString::from("Write"),
                OsString::from("mcp__gameengine_editor__*"),
            ]
        );
    }

    #[test]
    fn codex_mcp_config_uses_bearer_environment_and_workspace_sandbox() {
        let plan = build_launch_plan(
            ExternalAgentProviderKind::Codex,
            "",
            &[],
            "task",
            "http://127.0.0.1:4321/mcp",
        )
        .expect("codex plan");
        let args = plan
            .args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(args.contains("--sandbox\nworkspace-write"));
        assert!(args.contains("http://127.0.0.1:4321/mcp"));
        assert!(args.contains("GAMEENGINE_MCP_AUTH_TOKEN"));
    }

    #[test]
    fn claude_stream_translation_keeps_host_protocol_explicit() {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": r#"GAMEENGINE_AGENT_EVENT {"type":"progress","step":"inspect","detail":"scene"}"#,
                    },
                    {
                        "type": "tool_use",
                        "name": "mcp__gameengine_editor__scene_get",
                    },
                ]
            }
        })
        .to_string();
        let events = translate_provider_line(ExternalAgentProviderKind::ClaudeCode, &line);
        assert!(events.iter().any(|event| matches!(
            event,
            ExternalAgentSemanticEvent::GameEngineProtocolPayload(payload)
                if payload.contains("\"type\":\"progress\"")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ExternalAgentSemanticEvent::ToolAction { tool, .. }
                if tool == "mcp__gameengine_editor__scene_get"
        )));
    }

    #[test]
    fn provider_failure_mapping_is_sanitized_and_classified() {
        let mut diagnostics = ExternalAgentDiagnostics::default();
        diagnostics.observe(ExternalAgentProviderKind::Codex, "rate limit exceeded");
        let failure = diagnostics.classify_exit(ExternalAgentProviderKind::Codex, Some(1));
        assert!(failure.retryable);
        assert!(failure.message.contains("rate limiting"));
        assert!(!failure.message.contains("turn.failed"));
    }

    #[test]
    fn remote_status_contains_only_sanitized_adapter_state() {
        let status = ExternalAgentProviderStatus {
            kind: ExternalAgentProviderKind::ClaudeCode,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: ExternalAgentAuthStatus::Authenticated,
        };
        let json = status.remote_json().to_string();
        assert!(json.contains("claude-code"));
        assert!(json.contains("authenticated"));
        assert!(!json.contains("GAMEENGINE_MCP"));
        assert!(!json.contains("program"));
    }
}
