//! AI Agent Bridge MCP tools (Phase 40, ADR 0035).
//!
//! Provides tool descriptors and pure data-layer functions for the AI agent
//! bridge.  No IPC or file I/O is performed here; the transport layer is
//! implemented in the editor session (see ADR 0035).

use crate::McpToolDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// An input payload from an external AI agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAgentInput {
    /// Action to perform.
    ///
    /// Known values: `"key_press"`, `"key_release"`, `"mouse_move"`,
    /// `"mouse_click"`, `"capture_frame"`.
    pub action: String,
    /// Action-specific parameters.
    pub payload: serde_json::Value,
}

/// Result returned to an external AI agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAgentOutput {
    /// `true` when the action was accepted or the query succeeded.
    pub success: bool,
    /// Human-readable status message.
    pub message: String,
    /// Optional action-specific result data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl AiAgentOutput {
    /// Creates a successful output with `message`.
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data: None,
        }
    }

    /// Creates a failure output with `message`.
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            data: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

const KNOWN_ACTIONS: &[&str] = &[
    "key_press",
    "key_release",
    "mouse_move",
    "mouse_click",
    "capture_frame",
];

/// Validates an [`AiAgentInput`] document.
///
/// Returns a list of human-readable error strings.  An empty list means the
/// input is valid.
pub fn validate_ai_agent_input(input: &AiAgentInput) -> Vec<String> {
    let mut errors = Vec::new();
    if input.action.is_empty() {
        errors.push("action must not be empty".into());
    } else if !KNOWN_ACTIONS.contains(&input.action.as_str()) {
        errors.push(format!(
            "unknown action '{}'; known actions: {}",
            input.action,
            KNOWN_ACTIONS.join(", ")
        ));
    }
    errors
}

/// Describes the current session capabilities returned by
/// `ai_agent.describe_session`.
pub fn describe_session() -> AiAgentOutput {
    AiAgentOutput {
        success: true,
        message: "session capabilities".into(),
        data: Some(json!({
            "known_actions": KNOWN_ACTIONS,
            "ipc_transport": "filesystem_inbox_outbox",
            "version": 1,
        })),
    }
}

// ---------------------------------------------------------------------------
// Tool descriptors
// ---------------------------------------------------------------------------

/// Returns the MCP tool descriptors for the AI Agent Bridge tools.
pub fn ai_agent_tool_descriptors() -> Vec<McpToolDescriptor> {
    vec![
        McpToolDescriptor {
            name: "ai_agent.describe_session".into(),
            description:
                "Returns the session capabilities and available actions for the AI agent bridge."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        McpToolDescriptor {
            name: "ai_agent.validate_input".into(),
            description:
                "Validates an AiAgentInput document. Returns errors if the input is malformed."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "The action to validate."
                    },
                    "payload": {
                        "description": "Action-specific parameters."
                    }
                },
                "required": ["action", "payload"]
            }),
        },
        McpToolDescriptor {
            name: "ai_agent.inject_input".into(),
            description: "Queues an AiAgentInput to the filesystem inbox for the running editor \
                          session (filesystem_inbox_outbox transport, ADR 0035)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "inbox_path": {
                        "type": "string",
                        "description": "Filesystem path to write the inbox request JSON."
                    },
                    "action": {
                        "type": "string",
                        "description": "The action to perform."
                    },
                    "payload": {
                        "description": "Action-specific parameters."
                    }
                },
                "required": ["inbox_path", "action", "payload"]
            }),
        },
        McpToolDescriptor {
            name: "ai_agent.capture_frame".into(),
            description: "Queues a capture_frame request to the filesystem inbox. The running \
                          editor writes the captured frame PNG to its outbox."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "inbox_path": {
                        "type": "string",
                        "description": "Filesystem path to write the capture request."
                    }
                },
                "required": ["inbox_path"]
            }),
        },
    ]
}

/// Handles the `ai_agent.describe_session` MCP tool call.
pub fn handle_describe_session(_input: serde_json::Value) -> serde_json::Value {
    serde_json::to_value(describe_session())
        .unwrap_or_else(|_| json!({"success": false, "message": "serialization error"}))
}

/// Handles the `ai_agent.validate_input` MCP tool call.
pub fn handle_validate_input(input: serde_json::Value) -> serde_json::Value {
    match serde_json::from_value::<AiAgentInput>(input) {
        Ok(agent_input) => {
            let errors = validate_ai_agent_input(&agent_input);
            if errors.is_empty() {
                serde_json::to_value(AiAgentOutput::ok("input is valid"))
                    .unwrap_or_else(|_| json!({"success": true, "message": "input is valid"}))
            } else {
                serde_json::to_value(AiAgentOutput {
                    success: false,
                    message: errors.join("; "),
                    data: None,
                })
                .unwrap_or_else(|_| json!({"success": false, "message": "validation failed"}))
            }
        }
        Err(e) => serde_json::to_value(AiAgentOutput::err(format!("invalid input JSON: {e}")))
            .unwrap_or_else(|_| json!({"success": false, "message": "parse error"})),
    }
}

/// Handles the `ai_agent.inject_input` MCP tool call.
///
/// Validates the action and writes the serialized [`AiAgentInput`] to the
/// path specified by `inbox_path` in the input object.
pub fn handle_inject_input(input: serde_json::Value) -> serde_json::Value {
    let inbox_path = match input.get("inbox_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_owned(),
        None => {
            return serde_json::to_value(AiAgentOutput::err("inbox_path is required"))
                .unwrap_or_else(|_| json!({"success": false, "message": "inbox_path missing"}));
        }
    };

    let agent_input = AiAgentInput {
        action: input
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        payload: input
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    };

    let errors = validate_ai_agent_input(&agent_input);
    if !errors.is_empty() {
        return serde_json::to_value(AiAgentOutput::err(errors.join("; ")))
            .unwrap_or_else(|_| json!({"success": false, "message": "validation failed"}));
    }

    match serde_json::to_string_pretty(&agent_input) {
        Ok(json) => match std::fs::write(&inbox_path, &json) {
            Ok(()) => {
                serde_json::to_value(AiAgentOutput::ok(format!("input queued to {inbox_path}")))
                    .unwrap_or_else(|_| json!({"success": true, "message": "queued"}))
            }
            Err(e) => {
                serde_json::to_value(AiAgentOutput::err(format!("failed to write inbox: {e}")))
                    .unwrap_or_else(|_| json!({"success": false, "message": "io error"}))
            }
        },
        Err(e) => serde_json::to_value(AiAgentOutput::err(format!("serialization error: {e}")))
            .unwrap_or_else(|_| json!({"success": false, "message": "serialize error"})),
    }
}

/// Handles the `ai_agent.capture_frame` MCP tool call.
///
/// Writes a `capture_frame` request to `inbox_path`.  The running editor
/// session reads the inbox and writes the captured frame PNG to its outbox.
pub fn handle_capture_frame(input: serde_json::Value) -> serde_json::Value {
    let inbox_path = match input.get("inbox_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_owned(),
        None => {
            return serde_json::to_value(AiAgentOutput::err("inbox_path is required"))
                .unwrap_or_else(|_| json!({"success": false, "message": "inbox_path missing"}));
        }
    };

    let request = AiAgentInput {
        action: "capture_frame".into(),
        payload: serde_json::json!({}),
    };

    match serde_json::to_string_pretty(&request) {
        Ok(json) => match std::fs::write(&inbox_path, &json) {
            Ok(()) => serde_json::to_value(AiAgentOutput::ok(format!(
                "capture request queued to {inbox_path}"
            )))
            .unwrap_or_else(|_| json!({"success": true, "message": "queued"})),
            Err(e) => {
                serde_json::to_value(AiAgentOutput::err(format!("failed to write inbox: {e}")))
                    .unwrap_or_else(|_| json!({"success": false, "message": "io error"}))
            }
        },
        Err(e) => serde_json::to_value(AiAgentOutput::err(format!("serialization error: {e}")))
            .unwrap_or_else(|_| json!({"success": false, "message": "serialize error"})),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_known_action_returns_no_errors() {
        let input = AiAgentInput {
            action: "key_press".into(),
            payload: serde_json::json!({"key": "W"}),
        };
        assert!(validate_ai_agent_input(&input).is_empty());
    }

    #[test]
    fn validate_unknown_action_returns_error() {
        let input = AiAgentInput {
            action: "explode".into(),
            payload: serde_json::Value::Null,
        };
        let errors = validate_ai_agent_input(&input);
        assert!(!errors.is_empty(), "unknown action must produce an error");
        assert!(errors[0].contains("unknown action"));
    }

    #[test]
    fn validate_empty_action_returns_error() {
        let input = AiAgentInput {
            action: String::new(),
            payload: serde_json::Value::Null,
        };
        let errors = validate_ai_agent_input(&input);
        assert!(errors.iter().any(|e| e.contains("must not be empty")));
    }

    #[test]
    fn describe_session_returns_known_actions() {
        let out = describe_session();
        assert!(out.success);
        let data = out.data.unwrap();
        let actions = data["known_actions"].as_array().unwrap();
        assert!(actions.iter().any(|v| v == "key_press"));
        assert!(actions.iter().any(|v| v == "capture_frame"));
    }

    #[test]
    fn ai_agent_tool_descriptors_contains_expected_names() {
        let descs = ai_agent_tool_descriptors();
        let names: Vec<&str> = descs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"ai_agent.describe_session"));
        assert!(names.contains(&"ai_agent.validate_input"));
    }

    #[test]
    fn handle_validate_input_accepts_valid_payload() {
        let input = serde_json::json!({"action": "mouse_click", "payload": {"x": 100, "y": 200}});
        let result = handle_validate_input(input);
        assert_eq!(result["success"], true);
    }
}
