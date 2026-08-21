//! MCP tool routing into the live Editor authoring host.

use super::EditorApp;
use crate::session::StructuredAuthoringError;
use engine_authoring::{
    AnimationSet, AuthoringPermission, AuthoringPermissions, AuthoringSession,
    ComponentSchemaRegistry, MaterialAsset, ProjectSettings, TypedDocumentAuthoringError,
};
use engine_mcp::{
    AUTHORING_APPLY_TOOL, AUTHORING_CAPABILITIES_TOOL, AUTHORING_DESCRIBE_TOOL,
    AUTHORING_INSPECT_TOOL, AUTHORING_LIST_TOOL, AUTHORING_PREVIEW_TOOL, AUTHORING_VALIDATE_TOOL,
    AssetInspectInput, AssetMcpTools, AssetSearchInput, AuthoringCapabilityMcpTools, AuthoringVerb,
    BehaviorTreeApplyInput, BehaviorTreeGraphInput, BehaviorTreeMcpTools, CapabilityDescribeInput,
    CapabilityInvokeInput, EntityFindInput, EntityInspectInput, GraphMutationInput,
    GraphViewMutationInput, McpToolError, PrefabCreateInput, PrefabInstantiateInput,
    PrefabMcpTools, SceneMcpTools, SceneMutationInput, TypedDocumentMutationInput, UiMutationInput,
    VfxEffectInput, VfxMcpTools, VfxMutationInput, VfxTemplateInput,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
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

    fn typed(error: TypedDocumentAuthoringError) -> Self {
        let code = error.code().to_owned();
        Self::new(code, error.to_string())
    }

    fn no_typed_document(domain: &str) -> Self {
        Self::new(
            "editor.no_typed_document",
            format!("no active {domain} document is open in the Editor"),
        )
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
        let vfx_tools = VfxMcpTools::new();
        let capability_tools = AuthoringCapabilityMcpTools::new();

        match name {
            AUTHORING_LIST_TOOL => {
                require_empty_arguments(arguments)?;
                to_value(
                    capability_tools
                        .list(&permissions)
                        .map_err(McpToolError::from)?,
                )
            }
            AUTHORING_CAPABILITIES_TOOL => {
                require_empty_arguments(arguments)?;
                to_value(
                    capability_tools
                        .capabilities(&permissions)
                        .map_err(McpToolError::from)?,
                )
            }
            AUTHORING_DESCRIBE_TOOL => {
                let input: CapabilityDescribeInput = decode(arguments)?;
                to_value(
                    capability_tools
                        .describe(&permissions, input)
                        .map_err(McpToolError::from)?,
                )
            }
            AUTHORING_INSPECT_TOOL
            | AUTHORING_VALIDATE_TOOL
            | AUTHORING_PREVIEW_TOOL
            | AUTHORING_APPLY_TOOL => {
                let verb = match name {
                    AUTHORING_VALIDATE_TOOL => AuthoringVerb::Validate,
                    AUTHORING_PREVIEW_TOOL => AuthoringVerb::Preview,
                    AUTHORING_APPLY_TOOL => AuthoringVerb::Apply,
                    _ => AuthoringVerb::Inspect,
                };
                let input: CapabilityInvokeInput = decode(arguments)?;
                let plan = capability_tools
                    .plan(verb, &permissions, input)
                    .map_err(McpToolError::from)?;
                // The registry reserves the `authoring` namespace, so the
                // resolved tool is always a domain tool and this dispatch
                // cannot re-enter the generic surface.
                self.handle_mcp_tool_call(&plan.tool, plan.arguments)
            }
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
                to_value(prefab_tools.prefab_preview(&project, session, &permissions, input)?)
            }
            "prefab.instantiate" => {
                let input: PrefabInstantiateInput = decode(arguments)?;
                let project = self.project_root().clone();
                let mutation = {
                    let session = self.scene_authoring_session_mut()?;
                    prefab_tools.prefab_instantiate(&project, session, &permissions, input)?
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
            "material.inspect" => {
                require_empty_arguments(arguments)?;
                let output = self
                    .material_editor
                    .structured_inspect(&permissions)
                    .map_err(EditorMcpCallFailure::typed)?
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Material"))?;
                to_value(output)
            }
            "material.validate" => {
                require_empty_arguments(arguments)?;
                let output = self
                    .material_editor
                    .structured_validate(&permissions)
                    .map_err(EditorMcpCallFailure::typed)?
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Material"))?;
                to_value(output)
            }
            "material.preview" => {
                let input: TypedDocumentMutationInput<MaterialAsset> = decode(arguments)?;
                let output = self
                    .material_editor
                    .structured_preview(
                        &permissions,
                        input.expected_revision,
                        input.expected_generation,
                        input.replacement,
                    )
                    .map_err(EditorMcpCallFailure::typed)?
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Material"))?;
                to_value(output)
            }
            "material.apply" => {
                let input: TypedDocumentMutationInput<MaterialAsset> = decode(arguments)?;
                let output = self
                    .material_editor
                    .structured_apply(
                        &permissions,
                        input.expected_revision,
                        input.expected_generation,
                        input.replacement,
                    )
                    .map_err(EditorMcpCallFailure::typed)?
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Material"))?;
                to_value(output)
            }
            "project_settings.inspect" => {
                require_empty_arguments(arguments)?;
                let panel = self
                    .project_settings_panel
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Project Settings"))?;
                to_value(
                    panel
                        .structured_inspect(&permissions)
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "project_settings.validate" => {
                require_empty_arguments(arguments)?;
                let panel = self
                    .project_settings_panel
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Project Settings"))?;
                to_value(
                    panel
                        .structured_validate(&permissions)
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "project_settings.preview" => {
                let input: TypedDocumentMutationInput<ProjectSettings> = decode(arguments)?;
                let panel = self
                    .project_settings_panel
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Project Settings"))?;
                to_value(
                    panel
                        .structured_preview(
                            &permissions,
                            input.expected_revision,
                            input.expected_generation,
                            input.replacement,
                        )
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "project_settings.apply" => {
                let input: TypedDocumentMutationInput<ProjectSettings> = decode(arguments)?;
                let panel = self
                    .project_settings_panel
                    .as_mut()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Project Settings"))?;
                to_value(
                    panel
                        .structured_apply(
                            &permissions,
                            input.expected_revision,
                            input.expected_generation,
                            input.replacement,
                        )
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "animation_set.inspect" => {
                require_empty_arguments(arguments)?;
                let editor = self
                    .animation_set_editor
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Animation Set"))?;
                to_value(
                    editor
                        .structured_inspect(&permissions)
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "animation_set.validate" => {
                require_empty_arguments(arguments)?;
                let editor = self
                    .animation_set_editor
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Animation Set"))?;
                to_value(
                    editor
                        .structured_validate(&permissions)
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "animation_set.preview" => {
                let input: TypedDocumentMutationInput<AnimationSet> = decode(arguments)?;
                let editor = self
                    .animation_set_editor
                    .as_ref()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Animation Set"))?;
                to_value(
                    editor
                        .structured_preview(
                            &permissions,
                            input.expected_revision,
                            input.expected_generation,
                            input.replacement,
                        )
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "animation_set.apply" => {
                let input: TypedDocumentMutationInput<AnimationSet> = decode(arguments)?;
                let editor = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| EditorMcpCallFailure::no_typed_document("Animation Set"))?;
                to_value(
                    editor
                        .structured_apply(
                            &permissions,
                            input.expected_revision,
                            input.expected_generation,
                            input.replacement,
                        )
                        .map_err(EditorMcpCallFailure::typed)?,
                )
            }
            "vfx.schemas" => {
                require_empty_arguments(arguments)?;
                to_value(vfx_tools.schemas(&permissions)?)
            }
            "vfx.inspect" => {
                let input: VfxEffectInput = decode(arguments)?;
                to_value(vfx_tools.inspect(&permissions, input)?)
            }
            "vfx.validate" => {
                let input: VfxEffectInput = decode(arguments)?;
                to_value(vfx_tools.validate(&permissions, input)?)
            }
            "vfx.preview" => {
                let input: VfxMutationInput = decode(arguments)?;
                to_value(vfx_tools.preview(&permissions, input)?)
            }
            "vfx.apply" => {
                let input: VfxMutationInput = decode(arguments)?;
                to_value(vfx_tools.apply(&permissions, input)?)
            }
            "vfx.template" => {
                let input: VfxTemplateInput = decode(arguments)?;
                to_value(vfx_tools.template(&permissions, input)?)
            }
            "behavior_tree.schemas" => {
                require_empty_arguments(arguments)?;
                to_value(behavior_tools.behavior_tree_schemas(&permissions)?)
            }
            "behavior_tree.validate" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_validate(&permissions, input)?)
            }
            "behavior_tree.compile" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_compile(&permissions, input)?)
            }
            "behavior_tree.layout" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_layout(&permissions, input)?)
            }
            "behavior_tree.nodes" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_nodes(&permissions, input)?)
            }
            "behavior_tree.edges" => {
                let input: BehaviorTreeGraphInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_edges(&permissions, input)?)
            }
            "behavior_tree.apply" => {
                let input: BehaviorTreeApplyInput = decode(arguments)?;
                to_value(behavior_tools.behavior_tree_apply(&permissions, input)?)
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

        assert!(
            schemas
                .iter()
                .any(|schema| { schema["type_id"] == Value::String("engine.camera".into()) })
        );
    }

    #[test]
    fn generic_apply_routes_through_the_same_live_scene_transaction() {
        let (_directory, mut app) = editor_app();
        let snapshot = app
            .handle_mcp_tool_call("authoring.inspect", json!({"capability": "scene.inspect"}))
            .expect("generic scene inspection");
        let revision = snapshot["revision"].as_u64().expect("revision");
        let generation = snapshot["generation"].as_u64().expect("generation");
        let entity = EntityId::generate();

        let result = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({
                    "capability": "scene.apply",
                    "arguments": {
                        "expected_revision": revision,
                        "expected_generation": generation,
                        "commands": [AuthoringCommand::CreateEntity {
                            id: entity.clone(),
                            name: "created_generically".into(),
                            parent: None,
                        }]
                    }
                }),
            )
            .expect("generic scene apply");

        assert_eq!(result["success"], Value::Bool(true));
        assert!(app.session.scene_entity(&entity).is_some());
        assert!(app.session.is_dirty());
        assert!(app.session.can_undo());
        assert!(app.session.undo());
        assert!(app.session.scene_entity(&entity).is_none());
    }

    #[test]
    fn capability_discovery_reports_the_registry_and_its_bound_tools() {
        let (_directory, mut app) = editor_app();

        let listed = app
            .handle_mcp_tool_call("authoring.capabilities", json!({}))
            .expect("capability discovery");
        let described = app
            .handle_mcp_tool_call("authoring.describe", json!({"capability": "scene.apply"}))
            .expect("capability description");

        let ids = listed["capabilities"]
            .as_array()
            .expect("capability array")
            .iter()
            .map(|capability| capability["id"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"scene.apply".to_owned()));
        assert!(ids.contains(&"behavior_tree.compile".to_owned()));
        assert_eq!(described["tool"], Value::String("scene.apply".into()));
        assert_eq!(
            described["capability"]["exposure"],
            Value::String("generic".into())
        );
    }

    #[test]
    fn specialized_capabilities_are_rejected_by_the_generic_surface() {
        let (_directory, mut app) = editor_app();

        let error = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({"capability": "behavior_tree.apply", "arguments": {}}),
            )
            .expect_err("specialized capabilities keep their declared tool");

        assert_eq!(error.code(), "mcp.capability_not_generic");
    }

    #[test]
    fn project_describe_reports_the_canonical_capability_registry() {
        let (_directory, mut app) = editor_app();

        let output = app
            .handle_mcp_tool_call("project.describe", json!({}))
            .expect("project description");
        let reported = output["authoring_capabilities"]
            .as_array()
            .expect("authoring capability array")
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        let expected = engine_authoring::AuthoringCapabilityRegistry::builtin()
            .capabilities()
            .map(|capability| capability.id.as_str().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(reported, expected);
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
        assert_eq!(assets[0]["id"], Value::String(asset_id.as_str().to_owned()));
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::animation_set_editor::AnimationSetEditorState;
    use crate::material_editor::MaterialEditorPanel;
    use crate::project_settings_panel::ProjectSettingsPanel;
    use crate::session::EditorSession;
    use engine_authoring::{
        AnimationSet, AssetId, GraphCommand, GraphViewCommand, MaterialAsset, ProjectSettings,
        UiDocumentCommand, Value as AuthoringValue, Vec2, Viewport,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn editor_app() -> (tempfile::TempDir, EditorApp) {
        let parent = tempfile::tempdir().expect("temporary project parent");
        let path = parent.path().join("McpParityGame");
        let root = engine_project_lifecycle::create_standard_project(&path, "McpParityGame")
            .expect("project scaffold");
        (parent, EditorApp::from_project(root))
    }

    fn cli_json(args: Vec<String>) -> Value {
        let result = engine_cli::run_cli_with_status(args).expect("CLI invocation");
        assert_eq!(result.exit_code, 0, "{}", result.output);
        serde_json::from_str(&result.output).expect("CLI output JSON")
    }

    fn material_diff_documents(value: &Value) -> Vec<(MaterialAsset, MaterialAsset)> {
        value
            .as_array()
            .expect("material diff array")
            .iter()
            .map(|change| {
                let before =
                    serde_json::from_value(change["before"].clone()).expect("material diff before");
                let after =
                    serde_json::from_value(change["after"].clone()).expect("material diff after");
                (before, after)
            })
            .collect()
    }

    #[test]
    fn generic_graph_and_layout_adapters_produce_equivalent_results() {
        let seed = EditorSession::behavior_tree_example().expect("behavior tree example");
        let graph_json = serde_json::to_string_pretty(seed.graph()).expect("graph JSON");
        let view_json =
            serde_json::to_string_pretty(seed.graph_view().expect("behavior tree example view"))
                .expect("view JSON");
        let graph_command = GraphCommand::SetGraphAnnotation {
            key: "parity.marker".into(),
            value: Some(AuthoringValue::String("same".into())),
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
        app.session.reset(EditorSession::new(
            serde_json::from_str(&graph_json).expect("MCP graph"),
            Some(serde_json::from_str(&view_json).expect("MCP view")),
        ));
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
        let cli_graph_value: Value =
            serde_json::from_str(&std::fs::read_to_string(&graph_path).expect("read CLI graph"))
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
        let layout_revision = mcp_layout_base["revision"]
            .as_u64()
            .expect("layout revision");
        let layout_generation = mcp_layout_base["generation"]
            .as_u64()
            .expect("layout generation");
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
        let cli_view_value: Value =
            serde_json::from_str(&std::fs::read_to_string(&view_path).expect("read CLI view"))
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
        let cli_ui_value: Value =
            serde_json::from_str(&std::fs::read_to_string(&ui_path).expect("read CLI UI"))
                .expect("CLI UI value");
        assert_eq!(
            cli_ui_value,
            serde_json::to_value(direct.ui_document().expect("direct UI")).expect("UI value")
        );
    }

    #[test]
    fn typed_material_adapters_are_equivalent_and_editor_apply_is_undoable() {
        let (directory, mut app) = editor_app();
        let project = app.project_root().clone();
        let relative = PathBuf::from("materials/adr0121-parity.material.json");
        let absolute = project.assets_root().join(&relative);
        std::fs::create_dir_all(absolute.parent().expect("material parent"))
            .expect("material directory");

        let baseline_json = MaterialAsset::default().to_json().expect("material JSON");
        std::fs::write(&absolute, &baseline_json).expect("material fixture");
        let baseline = MaterialAsset::from_json(&baseline_json).expect("canonical material");
        let mut replacement_source = baseline.clone();
        replacement_source.cast_shadow = !baseline.cast_shadow;
        let replacement_json = replacement_source
            .to_json()
            .expect("replacement material JSON");
        let replacement =
            MaterialAsset::from_json(&replacement_json).expect("canonical replacement material");
        let replacement_path = directory.path().join("material-replacement.json");
        std::fs::write(&replacement_path, &replacement_json).expect("material replacement fixture");

        let permissions = mcp_authoring_permissions();
        let mut direct = MaterialEditorPanel::new();
        direct.open_material(relative.clone(), baseline.clone());
        let direct_base = direct
            .structured_inspect(&permissions)
            .expect("direct material inspect")
            .expect("direct active material");
        let direct_validation = direct
            .structured_validate(&permissions)
            .expect("direct material validate")
            .expect("direct active material");
        let direct_preview = direct
            .structured_preview(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct material preview")
            .expect("direct active material");
        let direct_apply = direct
            .structured_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct material apply")
            .expect("direct active material");
        assert!(direct_apply.success);
        assert!(direct.can_undo());

        app.material_editor
            .open_material(relative.clone(), baseline.clone());
        let mcp_base = app
            .handle_mcp_tool_call(
                "authoring.inspect",
                json!({"capability": "material.inspect"}),
            )
            .expect("generic material inspect");
        let revision = mcp_base["revision"].as_u64().expect("material revision");
        let generation = mcp_base["generation"]
            .as_u64()
            .expect("material generation");
        let mcp_validation = app
            .handle_mcp_tool_call(
                "authoring.validate",
                json!({"capability": "material.validate"}),
            )
            .expect("generic material validate");
        assert_eq!(
            mcp_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            mcp_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics).expect("material diagnostics")
        );
        let arguments = json!({
            "expected_revision": revision,
            "expected_generation": generation,
            "replacement": replacement.clone()
        });
        let domain_preview = app
            .handle_mcp_tool_call("material.preview", arguments.clone())
            .expect("domain material preview");
        let generic_preview = app
            .handle_mcp_tool_call(
                "authoring.preview",
                json!({"capability": "material.preview", "arguments": arguments.clone()}),
            )
            .expect("generic material preview");
        assert_eq!(domain_preview, generic_preview);
        assert_eq!(
            generic_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("material preview diff")
        );
        let generic_apply = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({"capability": "material.apply", "arguments": arguments}),
            )
            .expect("generic material apply");
        assert_eq!(
            generic_apply["diff"],
            serde_json::to_value(&direct_apply.diff).expect("material apply diff")
        );
        assert!(app.material_editor.can_undo());
        assert!(app.material_editor.undo());
        assert_eq!(app.material_editor.active_material(), Some(&baseline));
        assert!(app.material_editor.can_redo());
        assert!(app.material_editor.redo());
        assert_eq!(app.material_editor.active_material(), Some(&replacement));
        let stale = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({
                    "capability": "material.apply",
                    "arguments": {
                        "expected_revision": revision,
                        "expected_generation": generation,
                        "replacement": replacement.clone()
                    }
                }),
            )
            .expect_err("stale material apply");
        assert_eq!(stale.code(), "authoring.stale_revision");

        let cli_validation = cli_json(vec![
            "material".into(),
            "validate".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            cli_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics).expect("CLI material diagnostics")
        );
        let cli_preview = cli_json(vec![
            "material".into(),
            "preview".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        let direct_preview_documents: Vec<_> = direct_preview
            .diff
            .iter()
            .map(|change| (change.before.clone(), change.after.clone()))
            .collect();
        assert_eq!(
            material_diff_documents(&cli_preview["diff"]),
            direct_preview_documents
        );
        let cli_apply = cli_json(vec![
            "material".into(),
            "apply".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        let direct_apply_documents: Vec<_> = direct_apply
            .diff
            .iter()
            .map(|change| (change.before.clone(), change.after.clone()))
            .collect();
        assert_eq!(
            material_diff_documents(&cli_apply["diff"]),
            direct_apply_documents
        );
        let persisted = MaterialAsset::from_json(
            &std::fs::read_to_string(&absolute).expect("persisted material JSON"),
        )
        .expect("persisted material");
        assert_eq!(persisted, replacement);
    }

    #[test]
    fn typed_project_settings_adapters_are_equivalent_and_editor_apply_is_undoable() {
        let (directory, mut app) = editor_app();
        let project = app.project_root().clone();
        let baseline = app
            .project_settings_panel
            .as_ref()
            .expect("project settings panel")
            .settings
            .clone();
        let mut replacement = baseline.clone();
        replacement.tags.push("adr0121_parity".into());
        let replacement_path = directory.path().join("project-settings-replacement.json");
        std::fs::write(
            &replacement_path,
            serde_json::to_string_pretty(&replacement).expect("settings replacement JSON"),
        )
        .expect("settings replacement fixture");

        let permissions = mcp_authoring_permissions();
        let mut direct = ProjectSettingsPanel::new(baseline.clone());
        let direct_base = direct
            .structured_inspect(&permissions)
            .expect("direct settings inspect");
        let direct_validation = direct
            .structured_validate(&permissions)
            .expect("direct settings validate");
        let direct_preview = direct
            .structured_preview(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct settings preview");
        let direct_apply = direct
            .structured_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct settings apply");
        assert!(direct_apply.success);
        assert!(direct.can_undo());

        let mcp_base = app
            .handle_mcp_tool_call(
                "authoring.inspect",
                json!({"capability": "project_settings.inspect"}),
            )
            .expect("generic settings inspect");
        let revision = mcp_base["revision"].as_u64().expect("settings revision");
        let generation = mcp_base["generation"]
            .as_u64()
            .expect("settings generation");
        let mcp_validation = app
            .handle_mcp_tool_call(
                "authoring.validate",
                json!({"capability": "project_settings.validate"}),
            )
            .expect("generic settings validate");
        assert_eq!(
            mcp_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            mcp_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics).expect("settings diagnostics")
        );
        let arguments = json!({
            "expected_revision": revision,
            "expected_generation": generation,
            "replacement": replacement.clone()
        });
        let domain_preview = app
            .handle_mcp_tool_call("project_settings.preview", arguments.clone())
            .expect("domain settings preview");
        let generic_preview = app
            .handle_mcp_tool_call(
                "authoring.preview",
                json!({
                    "capability": "project_settings.preview",
                    "arguments": arguments.clone()
                }),
            )
            .expect("generic settings preview");
        assert_eq!(domain_preview, generic_preview);
        assert_eq!(
            generic_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("settings preview diff")
        );
        let generic_apply = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({"capability": "project_settings.apply", "arguments": arguments}),
            )
            .expect("generic settings apply");
        assert_eq!(
            generic_apply["diff"],
            serde_json::to_value(&direct_apply.diff).expect("settings apply diff")
        );
        let panel = app
            .project_settings_panel
            .as_mut()
            .expect("live project settings panel");
        assert!(panel.can_undo());
        assert!(panel.undo());
        assert_eq!(panel.settings, baseline);
        assert!(panel.can_redo());
        assert!(panel.redo());
        assert_eq!(panel.settings, replacement);
        let stale = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({
                    "capability": "project_settings.apply",
                    "arguments": {
                        "expected_revision": revision,
                        "expected_generation": generation,
                        "replacement": replacement.clone()
                    }
                }),
            )
            .expect_err("stale settings apply");
        assert_eq!(stale.code(), "authoring.stale_revision");

        let cli_validation = cli_json(vec![
            "project_settings".into(),
            "validate".into(),
            project.path().to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            cli_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics).expect("CLI settings diagnostics")
        );
        let cli_preview = cli_json(vec![
            "project_settings".into(),
            "preview".into(),
            project.path().to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("CLI settings preview diff")
        );
        let cli_apply = cli_json(vec![
            "project_settings".into(),
            "apply".into(),
            project.path().to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_apply["diff"],
            serde_json::to_value(&direct_apply.diff).expect("CLI settings apply diff")
        );
        let persisted = ProjectSettings::load(project.path()).expect("persisted settings");
        assert_eq!(persisted, replacement);
    }

    #[test]
    fn typed_animation_set_adapters_are_equivalent_and_preserve_editor_undo() {
        let (directory, mut app) = editor_app();
        let project = app.project_root().clone();
        let relative = PathBuf::from("animations/adr0121-parity.animset.json");
        let absolute = project.assets_root().join(&relative);
        std::fs::create_dir_all(absolute.parent().expect("animation parent"))
            .expect("animation directory");

        let baseline = AnimationSet::empty();
        std::fs::write(
            &absolute,
            baseline.to_canonical_json().expect("animation set JSON"),
        )
        .expect("animation set fixture");
        let mut replacement = baseline.clone();
        replacement.graph = Some(AssetId::generate());
        let replacement_path = directory.path().join("animation-set-replacement.json");
        std::fs::write(
            &replacement_path,
            replacement
                .to_canonical_json()
                .expect("animation replacement JSON"),
        )
        .expect("animation replacement fixture");

        let permissions = mcp_authoring_permissions();
        let mut direct =
            AnimationSetEditorState::new(relative.clone(), absolute.clone(), baseline.clone());
        let direct_base = direct
            .structured_inspect(&permissions)
            .expect("direct animation inspect");
        let direct_validation = direct
            .structured_validate(&permissions)
            .expect("direct animation validate");
        let direct_preview = direct
            .structured_preview(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct animation preview");
        let direct_apply = direct
            .structured_apply(
                &permissions,
                direct_base.revision,
                direct_base.generation,
                replacement.clone(),
            )
            .expect("direct animation apply");
        assert!(direct_apply.success);
        assert!(direct.can_undo());

        app.animation_set_editor = Some(AnimationSetEditorState::new(
            relative.clone(),
            absolute.clone(),
            baseline.clone(),
        ));
        let mcp_base = app
            .handle_mcp_tool_call(
                "authoring.inspect",
                json!({"capability": "animation_set.inspect"}),
            )
            .expect("generic animation inspect");
        let revision = mcp_base["revision"].as_u64().expect("animation revision");
        let generation = mcp_base["generation"]
            .as_u64()
            .expect("animation generation");
        let mcp_validation = app
            .handle_mcp_tool_call(
                "authoring.validate",
                json!({"capability": "animation_set.validate"}),
            )
            .expect("generic animation validate");
        assert_eq!(
            mcp_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            mcp_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics).expect("animation diagnostics")
        );
        let arguments = json!({
            "expected_revision": revision,
            "expected_generation": generation,
            "replacement": replacement.clone()
        });
        let domain_preview = app
            .handle_mcp_tool_call("animation_set.preview", arguments.clone())
            .expect("domain animation preview");
        let generic_preview = app
            .handle_mcp_tool_call(
                "authoring.preview",
                json!({"capability": "animation_set.preview", "arguments": arguments.clone()}),
            )
            .expect("generic animation preview");
        assert_eq!(domain_preview, generic_preview);
        assert_eq!(
            generic_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("animation preview diff")
        );
        let generic_apply = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({"capability": "animation_set.apply", "arguments": arguments}),
            )
            .expect("generic animation apply");
        assert_eq!(
            generic_apply["diff"],
            serde_json::to_value(&direct_apply.diff).expect("animation apply diff")
        );
        let editor = app
            .animation_set_editor
            .as_mut()
            .expect("live animation set editor");
        assert!(editor.can_undo());
        assert!(editor.undo());
        assert_eq!(editor.document, baseline);
        assert!(editor.can_redo());
        assert!(editor.redo());
        assert_eq!(editor.document, replacement);
        let stale = app
            .handle_mcp_tool_call(
                "authoring.apply",
                json!({
                    "capability": "animation_set.apply",
                    "arguments": {
                        "expected_revision": revision,
                        "expected_generation": generation,
                        "replacement": replacement.clone()
                    }
                }),
            )
            .expect_err("stale animation apply");
        assert_eq!(stale.code(), "authoring.stale_revision");

        let cli_validation = cli_json(vec![
            "animation_set".into(),
            "validate".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_validation["success"],
            Value::Bool(direct_validation.success)
        );
        assert_eq!(
            cli_validation["diagnostics"],
            serde_json::to_value(&direct_validation.diagnostics)
                .expect("CLI animation diagnostics")
        );
        let cli_preview = cli_json(vec![
            "animation_set".into(),
            "preview".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_preview["diff"],
            serde_json::to_value(&direct_preview.diff).expect("CLI animation preview diff")
        );
        let cli_apply = cli_json(vec![
            "animation_set".into(),
            "apply".into(),
            project.path().to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
            replacement_path.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            cli_apply["diff"],
            serde_json::to_value(&direct_apply.diff).expect("CLI animation apply diff")
        );
        let persisted = AnimationSet::from_json(
            &std::fs::read_to_string(&absolute).expect("persisted animation JSON"),
        )
        .expect("persisted animation set");
        assert_eq!(persisted, replacement);
    }
}
