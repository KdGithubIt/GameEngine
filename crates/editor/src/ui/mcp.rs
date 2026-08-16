//! MCP tool routing into the live Editor authoring host.

use super::EditorApp;
use engine_authoring::{
    AuthoringPermission, AuthoringPermissions, AuthoringSession, ComponentSchemaRegistry,
};
use engine_mcp::{
    BehaviorTreeApplyInput, BehaviorTreeGraphInput, BehaviorTreeMcpTools, EntityFindInput,
    EntityInspectInput, McpToolError, SceneMcpTools, SceneMutationInput,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::fmt;

/// Structured failure returned when the Editor cannot execute one MCP tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorMcpCallFailure {
    code: String,
    message: String,
}

impl EditorMcpCallFailure {
    /// Stable diagnostic-style code for the failed tool invocation.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Human-readable description of the failure.
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn invalid_arguments(error: serde_json::Error) -> Self {
        Self::new("mcp.invalid_arguments", error.to_string())
    }

    fn tool(error: McpToolError) -> Self {
        let code = error.code().to_owned();
        Self::new(code, error.to_string())
    }

    fn no_scene() -> Self {
        Self::new(
            "editor.no_scene_document",
            "the active Editor tab is not a Scene document",
        )
    }
}

impl fmt::Display for EditorMcpCallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EditorMcpCallFailure {}

impl EditorApp {
    /// Executes one structured MCP tool against the live project-scoped Editor.
    ///
    /// Scene writes use the active tab's existing authoring session, so MCP
    /// mutations share dirty state, stale-generation checks, and undo history
    /// with equivalent human edits. This method never persists a document;
    /// normal Editor save policy remains authoritative.
    ///
    /// # Errors
    ///
    /// Returns [`EditorMcpCallFailure`] for invalid tool arguments, missing
    /// Scene context, permission/authoring rejection, or unknown tools.
    pub fn handle_mcp_tool_call(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, EditorMcpCallFailure> {
        let permissions = mcp_authoring_permissions();
        let scene_tools = SceneMcpTools::new();
        let behavior_tools = BehaviorTreeMcpTools::new();

        match name {
            "project.describe" => {
                require_empty_arguments(arguments)?;
                to_value(scene_tools.project_describe(self.project_root(), &permissions)?)
            }
            "scene.inspect" => {
                require_empty_arguments(arguments)?;
                let session = self.scene_authoring_session()?;
                to_value(scene_tools.scene_inspect(session, &permissions)?)
            }
            "scene.validate" => {
                require_empty_arguments(arguments)?;
                let session = self.scene_authoring_session()?;
                to_value(scene_tools.scene_validate(session, &permissions)?)
            }
            "scene.preview" => {
                let input: SceneMutationInput = decode(arguments)?;
                let session = self.scene_authoring_session()?;
                to_value(scene_tools.scene_preview(session, &permissions, input)?)
            }
            "scene.apply" => {
                let input: SceneMutationInput = decode(arguments)?;
                let mutation = {
                    let session = self.scene_authoring_session_mut()?;
                    scene_tools.scene_apply(session, &permissions, input)?
                };
                self.session
                    .extend_diagnostics(mutation.diagnostics.iter().cloned());
                if mutation.success {
                    self.session.finish_external_scene_mutation();
                }
                to_value(mutation)
            }
            "entity.find" => {
                let input: EntityFindInput = decode(arguments)?;
                let session = self.scene_authoring_session()?;
                to_value(scene_tools.entity_find(session, &permissions, input)?)
            }
            "entity.inspect" => {
                let input: EntityInspectInput = decode(arguments)?;
                let session = self.scene_authoring_session()?;
                to_value(scene_tools.entity_inspect(session, &permissions, input)?)
            }
            "component.schemas" => {
                require_empty_arguments(arguments)?;
                let registry = self.mcp_component_schema_registry();
                to_value(scene_tools.component_schemas(&registry, &permissions)?)
            }
            "behavior_tree.schemas" => {
                require_empty_arguments(arguments)?;
                to_value(behavior_tools.behavior_tree_schemas())
            }
            "behavior_tree.validate" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_validate(input)?)
            }
            "behavior_tree.compile" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_compile(input)?)
            }
            "behavior_tree.layout" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_layout(input)?)
            }
            "behavior_tree.nodes" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_nodes(input)?)
            }
            "behavior_tree.edges" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_edges(input)?)
            }
            "behavior_tree.apply" => {
                let input: BehaviorTreeApplyInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_apply(input)?)
            }
            _ => Err(EditorMcpCallFailure::new(
                "mcp.unknown_tool",
                format!("unknown MCP tool `{name}`"),
            )),
        }
    }

    fn scene_authoring_session(&self) -> Result<&AuthoringSession, EditorMcpCallFailure> {
        self.session
            .scene_authoring_session()
            .ok_or_else(EditorMcpCallFailure::no_scene)
    }

    fn scene_authoring_session_mut(
        &mut self,
    ) -> Result<&mut AuthoringSession, EditorMcpCallFailure> {
        self.session
            .scene_authoring_session_mut()
            .ok_or_else(EditorMcpCallFailure::no_scene)
    }

    fn mcp_component_schema_registry(&self) -> ComponentSchemaRegistry {
        let mut registry = ComponentSchemaRegistry::builtin();
        let builtins = engine::builtin_registry();
        for definition in builtins.definitions() {
            registry.register(definition.schema.clone());
        }
        if let Some(module) = &self.game_module {
            for schema in module.component_schemas() {
                registry.register(schema.clone());
            }
        }
        registry
    }
}

fn mcp_authoring_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
}

fn require_empty_arguments(arguments: Value) -> Result<(), EditorMcpCallFailure> {
    match arguments {
        Value::Object(arguments) if arguments.is_empty() => Ok(()),
        other => Err(EditorMcpCallFailure::new(
            "mcp.invalid_arguments",
            format!("tool expects an empty argument object, received {other}"),
        )),
    }
}

fn decode<T: DeserializeOwned>(arguments: Value) -> Result<T, EditorMcpCallFailure> {
    serde_json::from_value(arguments).map_err(EditorMcpCallFailure::invalid_arguments)
}

fn to_value<T: Serialize>(value: T) -> Result<Value, EditorMcpCallFailure> {
    serde_json::to_value(value).map_err(|error| {
        EditorMcpCallFailure::new("mcp.result_serialization_failed", error.to_string())
    })
}

impl From<McpToolError> for EditorMcpCallFailure {
    fn from(error: McpToolError) -> Self {
        Self::tool(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AuthoringCommand, EntityId};
    use serde_json::json;

    fn editor_app() -> (tempfile::TempDir, EditorApp) {
        let parent = tempfile::tempdir().expect("temporary project parent");
        let path = parent.path().join("McpGame");
        let root = engine_project_lifecycle::create_standard_project(&path, "McpGame")
            .expect("project scaffold");
        (parent, EditorApp::from_project(root))
    }

    #[test]
    fn scene_apply_uses_live_editor_history_and_dirty_state() {
        let (_directory, mut app) = editor_app();
        let before_document_revision = app.session.document_revision();
        let snapshot = app
            .handle_mcp_tool_call("scene.inspect", json!({}))
            .expect("scene inspection");
        let revision = snapshot["revision"].as_u64().expect("revision");
        let generation = snapshot["generation"].as_u64().expect("generation");
        let entity = EntityId::generate();

        let result = app
            .handle_mcp_tool_call(
                "scene.apply",
                json!({
                    "expected_revision": revision,
                    "expected_generation": generation,
                    "commands": [AuthoringCommand::CreateEntity {
                        id: entity.clone(),
                        name: "created_by_mcp".into(),
                        parent: None,
                    }]
                }),
            )
            .expect("scene apply");

        assert_eq!(result["success"], Value::Bool(true));
        assert!(app.session.scene_entity(&entity).is_some());
        assert!(app.session.is_dirty());
        assert!(app.session.can_undo());
        assert_ne!(before_document_revision, app.session.document_revision());
        assert!(app.session.undo());
        assert!(app.session.scene_entity(&entity).is_none());
    }

    #[test]
    fn stale_scene_apply_returns_structured_authoring_error() {
        let (_directory, mut app) = editor_app();
        let snapshot = app
            .handle_mcp_tool_call("scene.inspect", json!({}))
            .expect("scene inspection");
        let revision = snapshot["revision"].as_u64().expect("revision");
        let generation = snapshot["generation"].as_u64().expect("generation");

        app.handle_mcp_tool_call(
            "scene.apply",
            json!({
                "expected_revision": revision,
                "expected_generation": generation,
                "commands": [AuthoringCommand::CreateEntity {
                    id: EntityId::generate(),
                    name: "first".into(),
                    parent: None,
                }]
            }),
        )
        .expect("first apply");

        let error = app
            .handle_mcp_tool_call(
                "scene.apply",
                json!({
                    "expected_revision": revision,
                    "expected_generation": generation,
                    "commands": []
                }),
            )
            .expect_err("stale base must reject");
        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn component_schema_query_uses_editor_engine_registry() {
        let (_directory, mut app) = editor_app();
        let output = app
            .handle_mcp_tool_call("component.schemas", json!({}))
            .expect("schema query");
        let schemas = output["schemas"].as_array().expect("schema array");

        assert!(schemas.iter().any(|schema| {
            schema["type_id"] == Value::String("engine.camera".into())
        }));
    }
}
