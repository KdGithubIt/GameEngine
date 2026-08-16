//! MCP adapters for shared prefab asset and Scene authoring services.

use crate::capability::domain_tool_descriptors;
use crate::{McpToolDescriptor, McpToolError};
use engine_assets::asset::AssetManifest;
use engine_assets::prefab::{PrefabAssetService, PrefabCreation};
use engine_authoring::{
    AuthoringCapabilityRegistry, AuthoringDomain, AuthoringPermissions, AuthoringScene,
    AuthoringSession, EntityId, PrefabAuthoringService, PrefabInstantiationMutation,
    PrefabInstantiationRequest, ProjectRoot,
};
use serde::Deserialize;

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
    ///
    /// Names, descriptions, and argument schemas come from the canonical
    /// authoring capability registry (ADR 0132).
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        domain_tool_descriptors(
            &AuthoringCapabilityRegistry::builtin(),
            &[AuthoringDomain::Prefab],
        )
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
        let request = PrefabInstantiationRequest::new(
            loaded.source,
            input.parent,
            input.expected_revision,
            input.expected_generation,
        );
        self.authoring
            .preview_instantiation(
                session,
                permissions,
                &loaded.prefab,
                request,
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
        let request = PrefabInstantiationRequest::new(
            loaded.source,
            input.parent,
            input.expected_revision,
            input.expected_generation,
        );
        self.authoring
            .apply_instantiation(
                session,
                permissions,
                &loaded.prefab,
                request,
            )
            .map_err(McpToolError::from)
    }
}

impl Default for PrefabMcpTools {
    fn default() -> Self {
        Self::new()
    }
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
            vec!["prefab.create", "prefab.instantiate", "prefab.preview"]
        );
    }
}
