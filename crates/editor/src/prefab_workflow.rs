//! Editor prefab creation, placement, override, revert, unpack, and dependency workflow.
//!
//! Runtime spawning remains in `engine::game_prefab`; this module owns only
//! authoring transactions and the editor-only source marker persisted on an
//! instance root. Unknown `editor.*` components are intentionally ignored by
//! the runtime scene bridge.

use crate::session::{EditorSession, EditorSessionError};
use engine_authoring::{
    replace_file_contents, AuthoringCommand, AuthoringEntity, AuthoringScene, ComponentTypeId,
    AuthoringPermission, AuthoringPermissions, EntityId, PersistError, PrefabAsset,
    PrefabAuthoringError, PrefabAuthoringService, PrefabError, PrefabInstantiationRequest,
    SceneAuthoringService, Value,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// Editor-only component that records the source asset of a prefab instance.
pub const EDITOR_PREFAB_INSTANCE_COMPONENT: &str = engine_authoring::PREFAB_INSTANCE_COMPONENT;

/// Read-only information shown for a selected prefab instance root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefabInstanceInfo {
    /// Asset-root-relative or absolute source path stored at instantiation.
    pub source: PathBuf,
    /// Whether the source currently exists and parses as a supported prefab.
    pub source_valid: bool,
    /// Diagnostic shown when the reference is broken.
    pub diagnostic: Option<String>,
}

/// Failure returned by an editor prefab workflow operation.
#[derive(Debug)]
pub enum PrefabWorkflowError {
    /// No scene is open or an authoring transaction failed.
    Session(EditorSessionError),
    /// Shared prefab authoring semantics rejected an instantiation.
    Authoring(PrefabAuthoringError),
    /// Prefab structure or JSON was invalid.
    Prefab(PrefabError),
    /// Source or destination IO failed.
    Io(std::io::Error),
    /// Atomic persistence failed.
    Persist(PersistError),
    /// Prefab JSON serialization failed.
    Json(serde_json::Error),
    /// The requested entity is not a prefab instance root.
    NotInstance(EntityId),
    /// The selection did not contain an entity from the open scene.
    MissingEntity(EntityId),
    /// A dependency cycle was found while traversing nested prefab markers.
    DependencyCycle(PathBuf),
}

impl fmt::Display for PrefabWorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "prefab scene transaction failed: {error}"),
            Self::Authoring(error) => write!(formatter, "prefab authoring failed: {error}"),
            Self::Prefab(error) => write!(formatter, "prefab asset is invalid: {error}"),
            Self::Io(error) => write!(formatter, "prefab IO failed: {error}"),
            Self::Persist(error) => write!(formatter, "prefab persistence failed: {error}"),
            Self::Json(error) => write!(formatter, "prefab serialization failed: {error}"),
            Self::NotInstance(entity) => {
                write!(formatter, "entity `{entity}` is not a prefab instance root")
            }
            Self::MissingEntity(entity) => {
                write!(formatter, "entity `{entity}` is absent from the open scene")
            }
            Self::DependencyCycle(path) => write!(
                formatter,
                "prefab dependency cycle includes `{}`",
                path.display()
            ),
        }
    }
}

impl std::error::Error for PrefabWorkflowError {}

impl From<EditorSessionError> for PrefabWorkflowError {
    fn from(value: EditorSessionError) -> Self {
        Self::Session(value)
    }
}
impl From<PrefabAuthoringError> for PrefabWorkflowError {
    fn from(value: PrefabAuthoringError) -> Self {
        Self::Authoring(value)
    }
}
impl From<PrefabError> for PrefabWorkflowError {
    fn from(value: PrefabError) -> Self {
        Self::Prefab(value)
    }
}

/// Saves the exact selection as a canonical prefab asset.
pub fn create_prefab_from_selection(
    scene: &AuthoringScene,
    selection: &[EntityId],
    destination: &Path,
) -> Result<PrefabAsset, PrefabWorkflowError> {
    let mut entities = Vec::with_capacity(selection.len());
    for id in selection {
        let entity = scene
            .entity(id)
            .ok_or_else(|| PrefabWorkflowError::MissingEntity(id.clone()))?;
        entities.push((id, entity));
    }
    let prefab = PrefabAsset::from_selection(entities)?;
    let json = prefab.to_json().map_err(PrefabWorkflowError::Json)?;
    replace_file_contents(destination, &json).map_err(PrefabWorkflowError::Persist)?;
    Ok(prefab)
}

/// Places a prefab through one undoable authoring transaction.
pub fn instantiate_prefab(
    session: &mut EditorSession,
    source: &Path,
    parent: Option<EntityId>,
) -> Result<EntityId, PrefabWorkflowError> {
    let prefab = load_prefab(source)?;
    let permissions =
        AuthoringPermissions::read_only().with(AuthoringPermission::ProjectDataWrite);
    let base = {
        let authoring = session
            .scene_authoring_session()
            .ok_or(EditorSessionError::NoSceneDocument)?;
        SceneAuthoringService::new()
            .inspect(authoring, &permissions)
            .map_err(PrefabAuthoringError::from)?
    };
    let result = {
        let authoring = session
            .scene_authoring_session_mut()
            .ok_or(EditorSessionError::NoSceneDocument)?;
        PrefabAuthoringService::new().apply_instantiation(
            authoring,
            &permissions,
            &prefab,
            PrefabInstantiationRequest::new(
                source,
                parent,
                base.revision,
                base.generation,
            ),
        )?
    };
    session.extend_diagnostics(result.mutation.diagnostics.iter().cloned());
    if !result.mutation.success {
        return Err(EditorSessionError::Diagnostics.into());
    }
    session.finish_external_scene_mutation();
    Ok(result.proposed_root)
}

/// Returns source validity and a recovery diagnostic for an instance root.
pub fn inspect_prefab_instance(
    scene: &AuthoringScene,
    root: &EntityId,
) -> Result<PrefabInstanceInfo, PrefabWorkflowError> {
    let source = instance_source(scene, root)?;
    match load_prefab(&source) {
        Ok(_) => Ok(PrefabInstanceInfo {
            source,
            source_valid: true,
            diagnostic: None,
        }),
        Err(error) => Ok(PrefabInstanceInfo {
            source,
            source_valid: false,
            diagnostic: Some(error.to_string()),
        }),
    }
}

/// Replaces the prefab source with the current instance subtree.
pub fn apply_prefab_overrides(
    scene: &AuthoringScene,
    root: &EntityId,
) -> Result<PrefabAsset, PrefabWorkflowError> {
    let source = instance_source(scene, root)?;
    let ids = subtree_ids(scene, root)?;
    let marker = ComponentTypeId::new(EDITOR_PREFAB_INSTANCE_COMPONENT);
    let mut owned = BTreeMap::new();
    for id in ids {
        let mut entity = scene
            .entity(&id)
            .cloned()
            .ok_or_else(|| PrefabWorkflowError::MissingEntity(id.clone()))?;
        entity.components.remove(&marker);
        owned.insert(id, entity);
    }
    let prefab = PrefabAsset::from_selection(owned.iter())?;
    let json = prefab.to_json().map_err(PrefabWorkflowError::Json)?;
    replace_file_contents(&source, &json).map_err(PrefabWorkflowError::Persist)?;
    Ok(prefab)
}

/// Discards instance changes and recreates the subtree from its source.
pub fn revert_prefab_instance(
    session: &mut EditorSession,
    root: &EntityId,
) -> Result<EntityId, PrefabWorkflowError> {
    let scene = session.scene().ok_or(EditorSessionError::NoSceneDocument)?;
    let source = instance_source(scene, root)?;
    let parent = scene.entity(root).and_then(|entity| entity.parent.clone());
    let ids = subtree_ids(scene, root)?;
    let mut commands = ids
        .into_iter()
        .rev()
        .map(|entity| AuthoringCommand::DeleteEntity { entity })
        .collect::<Vec<_>>();
    let prefab = load_prefab(&source)?;
    let instance = prefab.instantiate_with_root(parent)?;
    let new_root = instance.root.clone();
    commands.extend(instance.commands);
    commands.push(AuthoringCommand::AddComponent {
        entity: new_root.clone(),
        component_type: ComponentTypeId::new(EDITOR_PREFAB_INSTANCE_COMPONENT),
        value: marker_value(&source),
    });
    session.apply_scene_commands(commands)?;
    Ok(new_root)
}

/// Removes the source marker while keeping all authored entities unchanged.
pub fn unpack_prefab_instance(
    session: &mut EditorSession,
    root: &EntityId,
) -> Result<(), PrefabWorkflowError> {
    let scene = session.scene().ok_or(EditorSessionError::NoSceneDocument)?;
    instance_source(scene, root)?;
    session.apply_scene_commands([AuthoringCommand::RemoveComponent {
        entity: root.clone(),
        component_type: ComponentTypeId::new(EDITOR_PREFAB_INSTANCE_COMPONENT),
    }])?;
    Ok(())
}

/// Traverses editor prefab markers to produce a deterministic package dependency set.
pub fn prefab_dependencies(root: &Path) -> Result<BTreeSet<PathBuf>, PrefabWorkflowError> {
    fn visit(
        path: &Path,
        active: &mut BTreeSet<PathBuf>,
        output: &mut BTreeSet<PathBuf>,
    ) -> Result<(), PrefabWorkflowError> {
        let canonical = std::fs::canonicalize(path).map_err(PrefabWorkflowError::Io)?;
        if !active.insert(canonical.clone()) {
            return Err(PrefabWorkflowError::DependencyCycle(canonical));
        }
        let prefab = load_prefab(&canonical)?;
        output.insert(canonical.clone());
        for entity in prefab.entities.values() {
            if let Some(source) = marker_source(entity) {
                let dependency = canonical
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(source);
                visit(&dependency, active, output)?;
            }
        }
        active.remove(&canonical);
        Ok(())
    }
    let mut active = BTreeSet::new();
    let mut output = BTreeSet::new();
    visit(root, &mut active, &mut output)?;
    Ok(output)
}

fn load_prefab(path: &Path) -> Result<PrefabAsset, PrefabWorkflowError> {
    let json = std::fs::read_to_string(path).map_err(PrefabWorkflowError::Io)?;
    PrefabAsset::from_json(&json).map_err(PrefabWorkflowError::Prefab)
}

fn marker_value(source: &Path) -> Value {
    Value::Object(BTreeMap::from([(
        "source".to_string(),
        Value::String(source.to_string_lossy().into_owned()),
    )]))
}

fn marker_source(entity: &AuthoringEntity) -> Option<PathBuf> {
    let Value::Object(fields) = entity
        .components
        .get(&ComponentTypeId::new(EDITOR_PREFAB_INSTANCE_COMPONENT))?
    else {
        return None;
    };
    let Value::String(source) = fields.get("source")? else {
        return None;
    };
    Some(PathBuf::from(source))
}

fn instance_source(
    scene: &AuthoringScene,
    root: &EntityId,
) -> Result<PathBuf, PrefabWorkflowError> {
    scene
        .entity(root)
        .and_then(marker_source)
        .ok_or_else(|| PrefabWorkflowError::NotInstance(root.clone()))
}

fn subtree_ids(
    scene: &AuthoringScene,
    root: &EntityId,
) -> Result<Vec<EntityId>, PrefabWorkflowError> {
    if scene.entity(root).is_none() {
        return Err(PrefabWorkflowError::MissingEntity(root.clone()));
    }
    let mut result = Vec::new();
    let mut frontier = vec![root.clone()];
    while let Some(parent) = frontier.pop() {
        result.push(parent.clone());
        let mut children = scene
            .entities()
            .filter(|(_, entity)| entity.parent.as_ref() == Some(&parent))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        children.reverse();
        frontier.extend(children);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AuthoringEntity, AuthoringScene, Transaction};

    fn scene_with_root() -> (AuthoringScene, EntityId) {
        let mut scene = AuthoringScene::new();
        let root = EntityId::generate();
        let mut transaction = Transaction::begin(&scene);
        transaction.apply(AuthoringCommand::CreateEntity {
            id: root.clone(),
            name: "enemy".into(),
            parent: None,
        });
        transaction.commit(&mut scene).expect("fixture must commit");
        (scene, root)
    }

    #[test]
    fn create_and_inspect_prefab_round_trip() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("enemy.prefab.json");
        let (scene, root) = scene_with_root();
        create_prefab_from_selection(&scene, std::slice::from_ref(&root), &path)
            .expect("prefab must save");
        let prefab = load_prefab(&path).expect("prefab must load");
        assert_eq!(prefab.entities.len(), 1);
    }

    #[test]
    fn marker_source_parses_editor_metadata() {
        let mut entity = AuthoringEntity::new(EntityId::generate(), "instance");
        entity.components.insert(
            ComponentTypeId::new(EDITOR_PREFAB_INSTANCE_COMPONENT),
            marker_value(Path::new("prefabs/enemy.prefab.json")),
        );
        assert_eq!(
            marker_source(&entity),
            Some(PathBuf::from("prefabs/enemy.prefab.json"))
        );
    }
}
