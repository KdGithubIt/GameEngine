//! Transport-agnostic Scene and project MCP handlers.
//!
//! The caller supplies the live [`AuthoringSession`] owned by the application.
//! This keeps `engine-mcp` free of persistence, process lifecycle, and
//! Editor-specific mutation rules while still exercising the same shared Scene
//! authoring service as human and CLI clients.

use crate::{McpToolDescriptor, McpToolError};
use engine_authoring::{
    AuthoringCommand, AuthoringEntity, AuthoringPermission, AuthoringPermissions,
    AuthoringSession, ComponentSchema, ComponentSchemaRegistry, EntityId, ProjectId, ProjectRoot,
    SceneAuthoringMutation, SceneAuthoringService, SceneAuthoringSnapshot,
    SceneAuthoringValidation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Project metadata and structured authoring capability coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDescribeOutput {
    /// Stable logical project identity.
    pub project_id: ProjectId,
    /// Human-readable project name.
    pub name: String,
    /// Engine association from `project.json`.
    pub engine_version: String,
    /// MCP capabilities currently backed by shared authoring services.
    pub capabilities: Vec<String>,
}

/// Input for `scene.preview` and `scene.apply`.
#[derive(Debug, Deserialize)]
pub struct SceneMutationInput {
    /// Logical Scene revision observed by the caller.
    pub expected_revision: u64,
    /// Monotonic mutation generation observed by the caller.
    pub expected_generation: u64,
    /// Commands applied together as one transaction.
    pub commands: Vec<AuthoringCommand>,
}

/// Input for `entity.find`.
#[derive(Debug, Deserialize)]
pub struct EntityFindInput {
    /// Case-insensitive substring matched against ID, slug, display name, and
    /// description. An empty string returns every entity.
    #[serde(default)]
    pub query: String,
}

/// Input for `entity.inspect`.
#[derive(Debug, Deserialize)]
pub struct EntityInspectInput {
    /// Stable entity identifier.
    pub entity: EntityId,
}

/// Output for `entity.find`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntityFindOutput {
    /// Entities in stable-ID order.
    pub entities: Vec<AuthoringEntity>,
}

/// Output for `entity.inspect`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntityInspectOutput {
    /// The requested entity, or `None` when the stable ID is absent.
    pub entity: Option<AuthoringEntity>,
}

/// Output for `component.schemas`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComponentSchemasOutput {
    /// Schemas in stable component-type order.
    pub schemas: Vec<ComponentSchema>,
}

/// Scene/project MCP handler collection.
#[derive(Debug, Default, Clone, Copy)]
pub struct SceneMcpTools {
    service: SceneAuthoringService,
}

impl SceneMcpTools {
    /// Creates Scene/project MCP handlers over the shared authoring service.
    pub fn new() -> Self {
        Self {
            service: SceneAuthoringService::new(),
        }
    }

    /// Returns tool descriptors for registration by an MCP transport layer.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        vec![
            descriptor(
                "project.describe",
                "Describe the active GameEngine project and structured authoring capabilities.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            descriptor(
                "scene.inspect",
                "Inspect the live committed Scene with revision and generation tokens.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            descriptor(
                "scene.validate",
                "Validate the live committed Scene without mutation.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            descriptor(
                "scene.preview",
                "Preview one atomic Scene command batch against an exact revision/generation base.",
                mutation_input_schema(),
            ),
            descriptor(
                "scene.apply",
                "Apply one atomic Scene command batch through the shared authoring transaction boundary.",
                mutation_input_schema(),
            ),
            descriptor(
                "entity.find",
                "Find live Scene entities by stable ID, slug, display name, or description.",
                json!({
                    "type":"object",
                    "properties":{"query":{"type":"string"}},
                    "additionalProperties":false
                }),
            ),
            descriptor(
                "entity.inspect",
                "Inspect one live Scene entity by stable ID.",
                json!({
                    "type":"object",
                    "required":["entity"],
                    "properties":{"entity":{"type":"string"}},
                    "additionalProperties":false
                }),
            ),
            descriptor(
                "component.schemas",
                "Discover component schemas supplied by the live authoring host.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
        ]
    }

    /// Describes the active project and implemented parity capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when read permission is absent.
    pub fn project_describe(
        &self,
        project: &ProjectRoot,
        permissions: &AuthoringPermissions,
    ) -> Result<ProjectDescribeOutput, McpToolError> {
        permissions
            .require(AuthoringPermission::Read)
            .map_err(engine_authoring::SceneAuthoringError::from)?;
        Ok(ProjectDescribeOutput {
            project_id: project.project_id().clone(),
            name: project.config().name.clone(),
            engine_version: project.engine_version().to_owned(),
            capabilities: self
                .tool_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.name)
                .collect(),
        })
    }

    /// Inspects the live Scene supplied by the application host.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] on shared authoring rejection.
    pub fn scene_inspect(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<SceneAuthoringSnapshot, McpToolError> {
        Ok(self.service.inspect(session, permissions)?)
    }

    /// Validates the live Scene supplied by the application host.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] on shared authoring rejection.
    pub fn scene_validate(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<SceneAuthoringValidation, McpToolError> {
        Ok(self.service.validate(session, permissions)?)
    }

    /// Previews one Scene command batch without changing the live session.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] on permission or stale-base rejection.
    pub fn scene_preview(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        input: SceneMutationInput,
    ) -> Result<SceneAuthoringMutation, McpToolError> {
        Ok(self.service.preview(
            session,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Applies one Scene command batch to the live session.
    ///
    /// One handler call maps to one shared [`engine_authoring::Transaction`]
    /// commit and therefore at most one [`AuthoringSession`] undo entry.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] on permission or stale-base rejection.
    pub fn scene_apply(
        &self,
        session: &mut AuthoringSession,
        permissions: &AuthoringPermissions,
        input: SceneMutationInput,
    ) -> Result<SceneAuthoringMutation, McpToolError> {
        Ok(self.service.apply(
            session,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Finds entities in the live Scene.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when read permission is absent.
    pub fn entity_find(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        input: EntityFindInput,
    ) -> Result<EntityFindOutput, McpToolError> {
        Ok(EntityFindOutput {
            entities: self
                .service
                .find_entities(session, permissions, &input.query)?,
        })
    }

    /// Inspects one entity in the live Scene.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when read permission is absent.
    pub fn entity_inspect(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        input: EntityInspectInput,
    ) -> Result<EntityInspectOutput, McpToolError> {
        Ok(EntityInspectOutput {
            entity: self.service.entity(session, permissions, &input.entity)?,
        })
    }

    /// Returns the component schema catalog supplied by the application host.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when read permission is absent.
    pub fn component_schemas(
        &self,
        registry: &ComponentSchemaRegistry,
        permissions: &AuthoringPermissions,
    ) -> Result<ComponentSchemasOutput, McpToolError> {
        permissions
            .require(AuthoringPermission::Read)
            .map_err(engine_authoring::SceneAuthoringError::from)?;
        Ok(ComponentSchemasOutput {
            schemas: registry.schemas().cloned().collect(),
        })
    }
}

fn descriptor(
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn mutation_input_schema() -> serde_json::Value {
    json!({
        "type":"object",
        "required":["expected_revision","expected_generation","commands"],
        "properties":{
            "expected_revision":{"type":"integer","minimum":0},
            "expected_generation":{"type":"integer","minimum":0},
            "commands":{"type":"array","items":{"type":"object"}}
        },
        "additionalProperties":false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AuthoringScene, SceneAuthoringService};

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    #[test]
    fn descriptors_cover_scene_entity_and_schema_slice() {
        let names = SceneMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "project.describe",
                "scene.inspect",
                "scene.validate",
                "scene.preview",
                "scene.apply",
                "entity.find",
                "entity.inspect",
                "component.schemas",
            ]
        );
    }

    #[test]
    fn mcp_apply_matches_shared_scene_service_semantics() {
        let service = SceneAuthoringService::new();
        let tools = SceneMcpTools::new();
        let permissions = writable();
        let mut direct = AuthoringSession::new(AuthoringScene::new());
        let mut through_mcp = AuthoringSession::new(direct.scene().clone());
        let direct_base = service.inspect(&direct, &permissions).expect("direct inspect");
        let mcp_base = service
            .inspect(&through_mcp, &permissions)
            .expect("mcp inspect");
        let entity = EntityId::generate();
        let command = AuthoringCommand::CreateEntity {
            id: entity.clone(),
            name: "shared_path".into(),
            parent: None,
        };

        let direct_result = service
            .apply(
                &mut direct,
                &permissions,
                direct_base.revision,
                direct_base.generation,
                vec![command.clone()],
            )
            .expect("direct apply");
        let mcp_result = tools
            .scene_apply(
                &mut through_mcp,
                &permissions,
                SceneMutationInput {
                    expected_revision: mcp_base.revision,
                    expected_generation: mcp_base.generation,
                    commands: vec![command],
                },
            )
            .expect("mcp apply");

        assert!(direct_result.success);
        assert!(mcp_result.success);
        assert_eq!(direct_result.diff, mcp_result.diff);
        assert_eq!(
            direct.scene().entity(&entity),
            through_mcp.scene().entity(&entity)
        );
    }

    #[test]
    fn stale_revision_is_preserved_as_structured_tool_error() {
        let permissions = writable();
        let tools = SceneMcpTools::new();
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let base = tools
            .scene_inspect(&session, &permissions)
            .expect("inspect");
        tools
            .scene_apply(
                &mut session,
                &permissions,
                SceneMutationInput {
                    expected_revision: base.revision,
                    expected_generation: base.generation,
                    commands: vec![AuthoringCommand::CreateEntity {
                        id: EntityId::generate(),
                        name: "first".into(),
                        parent: None,
                    }],
                },
            )
            .expect("first apply");

        let error = tools
            .scene_apply(
                &mut session,
                &permissions,
                SceneMutationInput {
                    expected_revision: base.revision,
                    expected_generation: base.generation,
                    commands: Vec::new(),
                },
            )
            .expect_err("stale apply must reject");

        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn component_schema_query_uses_host_registry() {
        let tools = SceneMcpTools::new();
        let output = tools
            .component_schemas(
                &ComponentSchemaRegistry::builtin(),
                &AuthoringPermissions::read_only(),
            )
            .expect("schema query");

        assert!(!output.schemas.is_empty());
    }
}
