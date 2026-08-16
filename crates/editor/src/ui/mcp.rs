//! MCP tool routing into the live Editor authoring host.

use super::EditorApp;
use crate::session::StructuredAuthoringError;
use engine_authoring::{
    AuthoringPermission, AuthoringPermissions, AuthoringSession, ComponentSchemaRegistry,
};
use engine_mcp::{
    AssetInspectInput, AssetMcpTools, AssetSearchInput, BehaviorTreeApplyInput,
    BehaviorTreeGraphInput, BehaviorTreeMcpTools, EntityFindInput, EntityInspectInput,
    GraphMutationInput, GraphViewMutationInput, McpToolError, PrefabCreateInput,
    PrefabInstantiateInput, PrefabMcpTools, SceneMcpTools, SceneMutationInput, UiMutationInput,
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

    fn structured(error: StructuredAuthoringError) -> Self {
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
        let asset_tools = AssetMcpTools::new();
        let prefab_tools = PrefabMcpTools::new();
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
            "asset.search" => {
                let input: AssetSearchInput = decode(arguments)?;
                to_value(asset_tools.asset_search(
                    self.project_root(),
                    &self.asset_manifest,
                    &permissions,
                    input,
                )?)
            }
            "asset.inspect" => {
                let input: AssetInspectInput = decode(arguments)?;
                to_value(asset_tools.asset_inspect(
                    self.project_root(),
                    &self.asset_manifest,
                    &permissions,
                    input,
                )?)
            }
            "prefab.create" => {
                let input: PrefabCreateInput = decode(arguments)?;
                let project = self.project_root().clone();
                let scene = self.scene_authoring_session()?.scene().clone();
                let creation = prefab_tools.prefab_create(
                    &project,
                    &mut self.asset_manifest,
                    &permissions,
                    &scene,
                    input,
                )?;
                self.asset_browser.refresh(&project.assets_root());
                to_value(creation)
            }
            "prefab.preview" => {
                let input: PrefabInstantiateInput = decode(arguments)?;
                let project = self.project_root().clone();
                let session = self.scene_authoring_session()?;
                to_value(prefab_tools.prefab_preview(
                    &project,
                    session,
                    &permissions,
                    input,
                )?)
            }
            "prefab.instantiate" => {
                let input: PrefabInstantiateInput = decode(arguments)?;
                let project = self.project_root().clone();
                let mutation = {
                    let session = self.scene_authoring_session_mut()?;
                    prefab_tools.prefab_instantiate(
                        &project,
                        session,
                        &permissions,
                        input,
                    )?
                };
                self.session
                    .extend_diagnostics(mutation.mutation.diagnostics.iter().cloned());
                if mutation.mutation.success {
                    self.session.finish_external_scene_mutation();
                }
                to_value(mutation)
            }
            "graph.inspect" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_graph_inspect(&permissions)?)
            }
            "graph.validate" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_graph_validate(&permissions)?)
            }
            "graph.preview" => {
                let input: GraphMutationInput = decode(arguments)?;
                to_value(self.session.structured_graph_preview(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
            }
            "graph.apply" => {
                let input: GraphMutationInput = decode(arguments)?;
                to_value(self.session.structured_graph_apply(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
            }
            "graph.layout.inspect" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_graph_view_inspect(&permissions)?)
            }
            "graph.layout.validate" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_graph_view_validate(&permissions)?)
            }
            "graph.layout.preview" => {
                let input: GraphViewMutationInput = decode(arguments)?;
                to_value(self.session.structured_graph_view_preview(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
            }
            "graph.layout.apply" => {
                let input: GraphViewMutationInput = decode(arguments)?;
                to_value(self.session.structured_graph_view_apply(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
            }
            "ui.inspect" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_ui_inspect(&permissions)?)
            }
            "ui.validate" => {
                require_empty_arguments(arguments)?;
                to_value(self.session.structured_ui_validate(&permissions)?)
            }
            "ui.preview" => {
                let input: UiMutationInput = decode(arguments)?;
                to_value(self.session.structured_ui_preview(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
            }
            "ui.apply" => {
                let input: UiMutationInput = decode(arguments)?;
                to_value(self.session.structured_ui_apply(
                    &permissions,
                    input.expected_revision,
                    input.expected_generation,
                    input.commands,
                )?)
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
        .with(AuthoringPermission::AssetWrite)
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

impl From<StructuredAuthoringError> for EditorMcpCallFailure {
    fn from(error: StructuredAuthoringError) -> Self {
        Self::structured(error)
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

    #[test]
    fn asset_search_reads_the_live_editor_manifest() {
        let (_directory, mut app) = editor_app();
        let asset_id = engine_authoring::AssetId::generate();
        app.asset_manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: "textures/hero.png".into(),
                name: Some("hero_texture".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );

        let output = app
            .handle_mcp_tool_call("asset.search", json!({"query": "hero"}))
            .expect("asset search");
        let assets = output["assets"].as_array().expect("asset array");

        assert_eq!(assets.len(), 1);
        assert_eq!(
            assets[0]["id"],
            Value::String(asset_id.as_str().to_owned())
        );
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::session::EditorSession;
    use engine_authoring::{
        GraphCommand, GraphViewCommand, UiDocumentCommand, Value as AuthoringValue, Vec2, Viewport,
    };
    use serde_json::{json, Value};

    fn editor_app() -> (tempfile::TempDir, EditorApp) {
        let parent = tempfile::tempdir().expect("temporary project parent");
        let path = parent.path().join("McpParityGame");
        let root = engine_project_lifecycle::create_standard_project(&path, "McpParityGame")
            .expect("project scaffold");
        (parent, EditorApp::from_project(root))
    }

    #[test]
    fn generic_graph_and_layout_adapters_produce_equivalent_results() {
        let seed = EditorSession::behavior_tree_example().expect("behavior tree example");
        let graph_json = serde_json::to_string_pretty(seed.graph()).expect("graph JSON");
        let view_json = serde_json::to_string_pretty(
            seed.graph_view().expect("behavior tree example view"),
        )
        .expect("view JSON");
        let graph_command = GraphCommand::SetGraphAnnotation {
            key: "parity.marker".into(),
            value: AuthoringValue::String("same".into()),
        };
        let permissions = mcp_authoring_permissions();

        let mut direct = EditorSession::new(
            serde_json::from_str(&graph_json).expect("direct graph"),
            Some(serde_json::from_str(&view_json).expect("direct view")),
        );
        let direct_base = direct
            .structured_graph_inspect(&permissions)
            .expect("direct inspect");
        let direct_preview = direct
            .structured_graph_preview(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                vec![graph_command.clone()],
            )
            .expect("direct preview");
        let direct_apply = direct
            .structured_graph_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                vec![graph_command.clone()],
            )
            .expect("direct apply");
        assert!(direct_apply.success);
        let stale = direct
            .structured_graph_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                Vec::new(),
            )
            .expect_err("direct stale graph base");
        assert_eq!(stale.code(), "authoring.stale_revision");

        let (_project, mut app) = editor_app();
        app.session = EditorSession::new(
            serde_json::from_str(&graph_json).expect("MCP graph"),
            Some(serde_json::from_str(&view_json).expect("MCP view")),
        );
        let mcp_base = app
            .handle_mcp_tool_call("graph.inspect", json!({}))
            .expect("MCP inspect");
        let mcp_revision = mcp_base["revision"].as_u64().expect("MCP revision");
        let mcp_generation = mcp_base["generation"].as_u64().expect("MCP generation");
        let mcp_preview = app
            .handle_mcp_tool_call(
                "graph.preview",
                json!({
                    "expected_revision": mcp_revision,
                    "expected_generation": mcp_generation,
                    "commands": [graph_command.clone()]
                }),
            )
            .expect("MCP preview");
        assert_eq!(
            mcp_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("direct preview diff")
        );
        assert_eq!(
            mcp_preview["diagnostics"],
            serde_json::to_value(&direct_preview.diagnostics).expect("direct diagnostics")
        );
        app.handle_mcp_tool_call(
            "graph.apply",
            json!({
                "expected_revision": mcp_revision,
                "expected_generation": mcp_generation,
                "commands": [graph_command.clone()]
            }),
        )
        .expect("MCP apply");
        let mcp_stale = app
            .handle_mcp_tool_call(
                "graph.apply",
                json!({
                    "expected_revision": mcp_revision,
                    "expected_generation": mcp_generation,
                    "commands": []
                }),
            )
            .expect_err("MCP stale graph base");
        assert_eq!(mcp_stale.code(), "authoring.stale_revision");
        assert_eq!(
            serde_json::to_value(app.session.graph()).expect("MCP graph value"),
            serde_json::to_value(direct.graph()).expect("direct graph value")
        );

        let cli_dir = tempfile::tempdir().expect("CLI parity directory");
        let graph_path = cli_dir.path().join("parity.graph.json");
        let view_path = cli_dir.path().join("parity.graph.view.json");
        let graph_commands_path = cli_dir.path().join("graph-commands.json");
        std::fs::write(&graph_path, &graph_json).expect("write CLI graph");
        std::fs::write(&view_path, &view_json).expect("write CLI view");
        std::fs::write(
            &graph_commands_path,
            serde_json::to_string_pretty(&vec![graph_command]).expect("graph command JSON"),
        )
        .expect("write graph commands");
        let cli_graph = engine_cli::run_cli_with_status([
            "graph".to_owned(),
            "apply".to_owned(),
            graph_path.to_string_lossy().into_owned(),
            graph_commands_path.to_string_lossy().into_owned(),
        ])
        .expect("CLI graph apply");
        assert_eq!(cli_graph.exit_code, 0, "{}", cli_graph.output);
        let cli_graph_value: Value = serde_json::from_str(
            &std::fs::read_to_string(&graph_path).expect("read CLI graph"),
        )
        .expect("CLI graph value");
        assert_eq!(
            cli_graph_value,
            serde_json::to_value(direct.graph()).expect("direct graph value")
        );

        let viewport_command = GraphViewCommand::SetViewport {
            viewport: Viewport::new(Vec2::new(48.0, -12.0), 1.25),
        };
        let direct_layout_base = direct
            .structured_graph_view_inspect(&permissions)
            .expect("direct layout inspect");
        let direct_layout_preview = direct
            .structured_graph_view_preview(
                &permissions,
                direct_layout_base.revision,
                direct_layout_base.generation,
                vec![viewport_command.clone()],
            )
            .expect("direct layout preview");
        direct
            .structured_graph_view_apply(
                &permissions,
                direct_layout_base.revision,
                direct_layout_base.generation,
                vec![viewport_command.clone()],
            )
            .expect("direct layout apply");

        let mcp_layout_base = app
            .handle_mcp_tool_call("graph.layout.inspect", json!({}))
            .expect("MCP layout inspect");
        let layout_revision = mcp_layout_base["revision"].as_u64().expect("layout revision");
        let layout_generation = mcp_layout_base["generation"].as_u64().expect("layout generation");
        let mcp_layout_preview = app
            .handle_mcp_tool_call(
                "graph.layout.preview",
                json!({
                    "expected_revision": layout_revision,
                    "expected_generation": layout_generation,
                    "commands": [viewport_command.clone()]
                }),
            )
            .expect("MCP layout preview");
        assert_eq!(
            mcp_layout_preview["diff"],
            serde_json::to_value(&direct_layout_preview.diff).expect("layout preview diff")
        );
        app.handle_mcp_tool_call(
            "graph.layout.apply",
            json!({
                "expected_revision": layout_revision,
                "expected_generation": layout_generation,
                "commands": [viewport_command.clone()]
            }),
        )
        .expect("MCP layout apply");
        assert_eq!(
            serde_json::to_value(app.session.graph_view()).expect("MCP layout value"),
            serde_json::to_value(direct.graph_view()).expect("direct layout value")
        );

        let layout_commands_path = cli_dir.path().join("layout-commands.json");
        std::fs::write(
            &layout_commands_path,
            serde_json::to_string_pretty(&vec![viewport_command]).expect("layout command JSON"),
        )
        .expect("write layout commands");
        let cli_layout = engine_cli::run_cli_with_status([
            "graph".to_owned(),
            "layout".to_owned(),
            "apply".to_owned(),
            graph_path.to_string_lossy().into_owned(),
            view_path.to_string_lossy().into_owned(),
            layout_commands_path.to_string_lossy().into_owned(),
        ])
        .expect("CLI layout apply");
        assert_eq!(cli_layout.exit_code, 0, "{}", cli_layout.output);
        let cli_view_value: Value = serde_json::from_str(
            &std::fs::read_to_string(&view_path).expect("read CLI view"),
        )
        .expect("CLI view value");
        assert_eq!(
            cli_view_value,
            serde_json::to_value(direct.graph_view().expect("direct view")).expect("view value")
        );
    }

    #[test]
    fn generic_ui_adapters_produce_equivalent_results_and_stale_rejection() {
        let permissions = mcp_authoring_permissions();
        let cli_dir = tempfile::tempdir().expect("UI parity directory");
        let ui_path = cli_dir.path().join("parity.ui.json");
        let commands_path = cli_dir.path().join("ui-commands.json");
        let baseline = engine_authoring::UiDocument::default();
        std::fs::write(
            &ui_path,
            baseline.to_json_string().expect("baseline UI JSON"),
        )
        .expect("write baseline UI");
        let command = UiDocumentCommand::RenameNode {
            node: "root".into(),
            new_id: "root_parity".into(),
        };
        std::fs::write(
            &commands_path,
            serde_json::to_string_pretty(&vec![command.clone()]).expect("UI command JSON"),
        )
        .expect("write UI commands");

        let mut direct = EditorSession::empty_behavior_tree();
        direct
            .open_ui_discarding_changes(ui_path.clone())
            .expect("open direct UI");
        let direct_base = direct
            .structured_ui_inspect(&permissions)
            .expect("direct UI inspect");
        let direct_preview = direct
            .structured_ui_preview(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                vec![command.clone()],
            )
            .expect("direct UI preview");
        direct
            .structured_ui_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                vec![command.clone()],
            )
            .expect("direct UI apply");
        let direct_stale = direct
            .structured_ui_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                Vec::new(),
            )
            .expect_err("direct stale UI base");
        assert_eq!(direct_stale.code(), "authoring.stale_revision");

        let (_project, mut app) = editor_app();
        app.session
            .open_ui_discarding_changes(ui_path.clone())
            .expect("open MCP UI");
        let mcp_base = app
            .handle_mcp_tool_call("ui.inspect", json!({}))
            .expect("MCP UI inspect");
        let revision = mcp_base["revision"].as_u64().expect("UI revision");
        let generation = mcp_base["generation"].as_u64().expect("UI generation");
        let mcp_preview = app
            .handle_mcp_tool_call(
                "ui.preview",
                json!({
                    "expected_revision": revision,
                    "expected_generation": generation,
                    "commands": [command.clone()]
                }),
            )
            .expect("MCP UI preview");
        assert_eq!(
            mcp_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("direct UI diff")
        );
        app.handle_mcp_tool_call(
            "ui.apply",
            json!({
                "expected_revision": revision,
                "expected_generation": generation,
                "commands": [command]
            }),
        )
        .expect("MCP UI apply");
        let mcp_stale = app
            .handle_mcp_tool_call(
                "ui.apply",
                json!({
                    "expected_revision": revision,
                    "expected_generation": generation,
                    "commands": []
                }),
            )
            .expect_err("MCP stale UI base");
        assert_eq!(mcp_stale.code(), "authoring.stale_revision");
        assert_eq!(
            serde_json::to_value(app.session.ui_document()).expect("MCP UI value"),
            serde_json::to_value(direct.ui_document()).expect("direct UI value")
        );

        let cli_ui = engine_cli::run_cli_with_status([
            "ui".to_owned(),
            "apply".to_owned(),
            ui_path.to_string_lossy().into_owned(),
            commands_path.to_string_lossy().into_owned(),
        ])
        .expect("CLI UI apply");
        assert_eq!(cli_ui.exit_code, 0, "{}", cli_ui.output);
        let cli_ui_value: Value = serde_json::from_str(
            &std::fs::read_to_string(&ui_path).expect("read CLI UI"),
        )
        .expect("CLI UI value");
        assert_eq!(
            cli_ui_value,
            serde_json::to_value(direct.ui_document().expect("direct UI")).expect("UI value")
        );
    }
}
