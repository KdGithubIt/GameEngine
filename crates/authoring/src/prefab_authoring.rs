//! Shared prefab instantiation semantics over the Scene authoring transaction API.
//!
//! The prefab schema owns reusable entity templates, while this service owns the
//! authoring command batch used to preview or apply one instance. Editor, CLI,
//! and MCP adapters must not add their own ID-remap or instance-marker rules.

use crate::{
    AuthoringCommand, AuthoringPermissions, AuthoringSession, ComponentTypeId, EntityId,
    PrefabAsset, PrefabError, SceneAuthoringError, SceneAuthoringMutation,
    SceneAuthoringService, Value,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// Persisted editor-only component that records the source of a prefab instance.
pub const PREFAB_INSTANCE_COMPONENT: &str = "editor.prefab_instance";

/// Result of previewing or applying one prefab instantiation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrefabInstantiationMutation {
    /// Fresh root ID proposed for this independent instantiation call.
    pub proposed_root: EntityId,
    /// Shared Scene mutation result, including diagnostics and semantic diff.
    pub mutation: SceneAuthoringMutation,
}

/// Failure from shared prefab instantiation semantics.
#[derive(Debug)]
pub enum PrefabAuthoringError {
    /// The prefab definition cannot produce a valid command batch.
    Prefab(PrefabError),
    /// The shared Scene authoring service rejected the operation.
    Scene(SceneAuthoringError),
}

impl PrefabAuthoringError {
    /// Returns the stable diagnostic-style code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Prefab(_) => "prefab.invalid_definition",
            Self::Scene(source) => source.code(),
        }
    }
}

impl fmt::Display for PrefabAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefab(source) => write!(formatter, "prefab definition is invalid: {source}"),
            Self::Scene(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for PrefabAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Prefab(source) => Some(source),
            Self::Scene(source) => Some(source),
        }
    }
}

impl From<PrefabError> for PrefabAuthoringError {
    fn from(source: PrefabError) -> Self {
        Self::Prefab(source)
    }
}

impl From<SceneAuthoringError> for PrefabAuthoringError {
    fn from(source: SceneAuthoringError) -> Self {
        Self::Scene(source)
    }
}

/// GUI-free prefab instantiation behavior shared by structured authoring clients.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrefabAuthoringService;

impl PrefabAuthoringService {
    /// Creates the stateless prefab authoring service.
    pub fn new() -> Self {
        Self
    }

    /// Previews one prefab instantiation without changing the Scene or undo history.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAuthoringError`] when the prefab is structurally invalid,
    /// preview permission is absent, or the supplied revision/generation is stale.
    pub fn preview_instantiation(
        &self,
        session: &AuthoringSession,
        permissions: &AuthoringPermissions,
        prefab: &PrefabAsset,
        source: &Path,
        parent: Option<EntityId>,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<PrefabInstantiationMutation, PrefabAuthoringError> {
        let (proposed_root, commands) = instantiation_commands(prefab, source, parent)?;
        let mutation = SceneAuthoringService::new().preview(
            session,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        Ok(PrefabInstantiationMutation {
            proposed_root,
            mutation,
        })
    }

    /// Applies one prefab instantiation as one Scene transaction and undo entry.
    ///
    /// # Errors
    ///
    /// Returns [`PrefabAuthoringError`] when the prefab is structurally invalid,
    /// project-data-write permission is absent, the supplied revision/generation
    /// is stale, or the Scene transaction cannot be committed.
    pub fn apply_instantiation(
        &self,
        session: &mut AuthoringSession,
        permissions: &AuthoringPermissions,
        prefab: &PrefabAsset,
        source: &Path,
        parent: Option<EntityId>,
        expected_revision: u64,
        expected_generation: u64,
    ) -> Result<PrefabInstantiationMutation, PrefabAuthoringError> {
        let (proposed_root, commands) = instantiation_commands(prefab, source, parent)?;
        let mutation = SceneAuthoringService::new().apply(
            session,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        Ok(PrefabInstantiationMutation {
            proposed_root,
            mutation,
        })
    }
}

fn instantiation_commands(
    prefab: &PrefabAsset,
    source: &Path,
    parent: Option<EntityId>,
) -> Result<(EntityId, Vec<AuthoringCommand>), PrefabAuthoringError> {
    let mut instance = prefab.instantiate_with_root(parent)?;
    instance.commands.push(AuthoringCommand::AddComponent {
        entity: instance.root.clone(),
        component_type: ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT),
        value: Value::Object(BTreeMap::from([(
            "source".to_owned(),
            Value::String(source.to_string_lossy().into_owned()),
        )])),
    });
    Ok((instance.root, instance.commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthoringEntity, AuthoringPermission, AuthoringScene};

    fn permissions() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    fn prefab() -> PrefabAsset {
        let id = EntityId::generate();
        let entity = AuthoringEntity::new(id.clone(), "enemy");
        PrefabAsset::from_selection([(&id, &entity)]).expect("prefab fixture")
    }

    #[test]
    fn preview_is_non_destructive_and_reports_proposed_root() {
        let session = AuthoringSession::new(AuthoringScene::new());
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions())
            .expect("base snapshot");

        let result = PrefabAuthoringService::new()
            .preview_instantiation(
                &session,
                &permissions(),
                &prefab(),
                Path::new("/project/assets/prefabs/enemy.prefab.json"),
                None,
                base.revision,
                base.generation,
            )
            .expect("preview");

        assert!(result.mutation.success);
        assert!(session.scene().entity(&result.proposed_root).is_none());
        assert!(!session.can_undo());
    }

    #[test]
    fn apply_is_one_undoable_scene_transaction_with_source_marker() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let permissions = permissions();
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions)
            .expect("base snapshot");
        let source = Path::new("/project/assets/prefabs/enemy.prefab.json");

        let result = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                source,
                None,
                base.revision,
                base.generation,
            )
            .expect("apply");

        assert!(result.mutation.success);
        let root = session
            .scene()
            .entity(&result.proposed_root)
            .expect("instantiated root");
        assert!(root
            .components
            .contains_key(&ComponentTypeId::new(PREFAB_INSTANCE_COMPONENT)));
        assert!(session.can_undo());
        assert!(session.undo());
        assert!(session.scene().entity(&result.proposed_root).is_none());
    }

    #[test]
    fn stale_base_is_forwarded_from_scene_service() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let permissions = permissions();
        let base = SceneAuthoringService::new()
            .inspect(&session, &permissions)
            .expect("base snapshot");
        let first = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                Path::new("enemy.prefab.json"),
                None,
                base.revision,
                base.generation,
            )
            .expect("first apply");
        assert!(first.mutation.success);

        let error = PrefabAuthoringService::new()
            .apply_instantiation(
                &mut session,
                &permissions,
                &prefab(),
                Path::new("enemy.prefab.json"),
                None,
                base.revision,
                base.generation,
            )
            .expect_err("stale apply must fail");
        assert_eq!(error.code(), "authoring.stale_revision");
    }
}
