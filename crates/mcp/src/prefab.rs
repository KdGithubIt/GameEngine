//! MCP adapters for shared prefab asset and Scene authoring services.

use crate::{McpToolDescriptor, McpToolError};
use engine_assets::asset::AssetManifest;
use engine_assets::prefab::{PrefabAssetService, PrefabCreation};
use engine_authoring::{
    AuthoringPermissions, AuthoringScene, AuthoringSession, EntityId, PrefabAuthoringService,
    PrefabInstantiationMutation, ProjectRoot,
};
use serde::Deserialize;
use serde_json::json;

/// Input for `prefab.create`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PrefabCreateInput {
    /// Stable Scene entity whose complete subtree becomes the prefab.
    pub root_entity: EntityId,
    /// Asset-root-relative destination, normally ending in `.prefab.json`.
    pub destination: String,
}

/// Input shared by `prefab.preview` and `prefab.instantiate`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PrefabInstantiateInput {
    /// Asset-root-relative prefab source path.
    pub source: String,
    /// Optional existing Scene entity that becomes the new root parent.
    #[serde(default)]
    pub parent: Option<EntityId>,
    /// Scene revision used as the mutation base.
    pub expected_revision: u64,
    /// Scene generation used as the mutation base.
    pub expected_generation: u64,
}

/// Prefab MCP handlers backed exclusively by shared GUI-free services.
pub struct PrefabMcpTools {
    assets: PrefabAssetService,
    authoring: PrefabAuthoringService,
}

impl PrefabMcpTools {
    /// Creates prefab MCP handlers.
    pub fn new() -> Self {
        Self {
            assets: PrefabAssetService::new(),
            authoring: PrefabAuthoringService::new(),
        }
    }

    /// Returns descriptors for the prefab parity surface.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        vec![
            McpToolDescriptor {
                name: "prefab.create".into(),
                description:
                    "Create a prefab from one Scene entity and all descendants, then register it with a fresh AssetId."
                        .into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["root_entity", "destination"],
                    "properties": {
                        "root_entity": {"type": "string"},
                        "destination": {"type": "string"}
                    },
                    "additionalProperties": false
                }),
            },
            McpToolDescriptor {
                name: "prefab.preview".into(),
                description:
                    "Preview one prefab instantiation against an exact Scene revision/generation without mutating it."
                        .into(),
                input_schema: instantiation_schema(),
            },
            McpToolDescriptor {
                name: "prefab.instantiate".into(),
                description:
                    "Instantiate a prefab into the live Scene as one validated transaction and one undo entry."
                        .into(),
                input_schema: instantiation_schema(),
            },
        ]
    }

    /// Creates and registers one prefab asset through the shared asset service.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when permissions, project paths, selection, or
    /// persistence prevent prefab creation.
    pub fn prefab_create(
        &self,
        project: &ProjectRoot,
        manifest: &mut AssetManifest,
        permissions: &AuthoringPermissions,
        scene: &AuthoringScene,
        input: PrefabCreateInput,
    ) -> Result<PrefabCreation, McpToolError> {
        self.assets
            .create_from_root(
                project,
                manifest,
                permissions,
                scene,
                &input.root_entity,
                &input.destination,
            )
            .map_err(McpToolError::from)
    }

    /// Previews one prefab instantiation without mutating the Scene.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when loading, permissions, stale identity, or
    /// authoring validation reject the request.
    pub fn prefab_preview(
        &self,
        project: &ProjectRoot,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        input: PrefabInstantiateInput,
    ) -> Result<PrefabInstantiationMutation, McpToolError> {
        let loaded = self.assets.load(project, permissions, &input.source)?;
        self.authoring
            .preview_instantiation(
                session,
                permissions,
                &loaded.prefab,
                &loaded.source,
                input.parent,
                input.expected_revision,
                input.expected_generation,
            )
            .map_err(McpToolError::from)
    }

    /// Applies one prefab instantiation to the supplied live Scene session.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when loading, permissions, stale identity, or
    /// authoring validation reject the request.
    pub fn prefab_instantiate(
        &self,
        project: &ProjectRoot,
        session: &mut AuthoringSession,
        permissions: &AuthoringPermissions,
        input: PrefabInstantiateInput,
    ) -> Result<PrefabInstantiationMutation, McpToolError> {
        let loaded = self.assets.load(project, permissions, &input.source)?;
        self.authoring
            .apply_instantiation(
                session,
                permissions,
                &loaded.prefab,
                &loaded.source,
                input.parent,
                input.expected_revision,
                input.expected_generation,
            )
            .map_err(McpToolError::from)
    }
}

impl Default for PrefabMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

fn instantiation_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["source", "expected_revision", "expected_generation"],
        "properties": {
            "source": {"type": "string"},
            "parent": {"type": ["string", "null"]},
            "expected_revision": {"type": "integer", "minimum": 0},
            "expected_generation": {"type": "integer", "minimum": 0}
        },
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_expose_create_preview_and_instantiate() {
        let names = PrefabMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["prefab.create", "prefab.preview", "prefab.instantiate"]
        );
    }
}
