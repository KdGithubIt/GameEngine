//! Authoring scene data container.
//!
//! An [`AuthoringScene`] holds all [`AuthoringEntity`] values for one scene.
//! All mutations MUST go through a [`Transaction`] to ensure isolation and
//! diagnostic collection.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §7 and §12.
//!
//! [`Transaction`]: crate::transaction::Transaction

use crate::entity::AuthoringEntity;
use crate::id::{AssetId, ComponentTypeId, EntityId};
use crate::value::Value;
use crate::{Diagnostic, DiagnosticTarget};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Schema version written into every serialized scene document.
///
/// `load_scene_from_json` rejects files whose `schema_version` exceeds this
/// value, and treats a missing `schema_version` as version 1.
pub const SCENE_SCHEMA_VERSION: u32 = 1;

/// A collection of authoring entities that forms one scene.
///
/// Mutations to a scene MUST go through a [`Transaction`]. Direct field
/// access is not available; use [`entity`] and [`entities`] to read data.
///
/// [`Transaction`]: crate::transaction::Transaction
/// [`entity`]: AuthoringScene::entity
/// [`entities`]: AuthoringScene::entities
#[derive(Debug, Clone)]
pub struct AuthoringScene {
    entities: BTreeMap<EntityId, AuthoringEntity>,
    identity: u64,
    /// Logical content revision used by editor dirty-state checkpoints.
    ///
    /// A normal commit advances the revision, while undo/redo restores the
    /// revision carried by the restored snapshot. This makes an exact undo to
    /// the last saved snapshot comparable in constant time.
    revision: u64,
    /// Monotonic mutation generation used only to reject stale transactions.
    ///
    /// Unlike `revision`, this value never moves backwards when undo restores
    /// an older snapshot, so a transaction from an abandoned history branch
    /// cannot be committed after undo/redo.
    generation: u64,
}

impl AuthoringScene {
    /// Creates an empty scene with no entities.
    pub fn new() -> Self {
        Self {
            entities: BTreeMap::new(),
            identity: next_scene_identity(),
            revision: next_scene_revision(),
            generation: 0,
        }
    }

    /// Returns the entity with the given stable ID, or `None` when no such
    /// entity exists in this scene.
    pub fn entity(&self, id: &EntityId) -> Option<&AuthoringEntity> {
        self.entities.get(id)
    }

    /// Returns an iterator over all entities in stable ID order.
    pub fn entities(&self) -> impl Iterator<Item = (&EntityId, &AuthoringEntity)> {
        self.entities.iter()
    }

    /// Returns the number of entities in this scene.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Returns structured diagnostics for invalid scene semantics.
    ///
    /// Validation checks parent references and cycles, entity references nested
    /// in component values, and component type identifier syntax. Asset
    /// reference existence is not checked because assets are not owned by a
    /// scene.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for (id, entity) in &self.entities {
            if let Some(parent) = &entity.parent
                && !self.entities.contains_key(parent)
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.missing_parent",
                        format!(
                            "entity `{}` references missing parent `{}`",
                            id.as_str(),
                            parent.as_str()
                        ),
                    )
                    .with_target(DiagnosticTarget::Entity { id: id.clone() }),
                );
            }
            for (component_type, value) in &entity.components {
                if ComponentTypeId::try_new(component_type.as_str()).is_err() {
                    diagnostics.push(
                        Diagnostic::error(
                            "scene.invalid_component_type",
                            format!(
                                "entity `{}` contains invalid component type `{}`",
                                id.as_str(),
                                component_type.as_str()
                            ),
                        )
                        .with_target(DiagnosticTarget::Component {
                            entity: id.clone(),
                            component_type: component_type.clone(),
                        }),
                    );
                }
                validate_value_references(self, id, component_type, value, &mut diagnostics);
            }
        }
        validate_parent_hierarchy(self, &mut diagnostics);
        diagnostics
    }

    /// Serializes this scene to deterministic canonical pretty-printed JSON.
    ///
    /// The output always includes a `schema_version` field (currently `1`) as
    /// the first top-level key, before `entities`. Entities and component maps
    /// are written in stable identifier order.
    ///
    /// This method performs validation and serialization only. It does not
    /// write a file atomically or provide a complete persistence save pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error when scene validation fails or JSON serialization
    /// cannot complete.
    pub fn to_canonical_json(&self) -> Result<String, SceneSaveError> {
        let diagnostics = self.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(SceneSaveError::ValidationFailed { diagnostics });
        }
        let file = SceneFileRef {
            schema_version: SCENE_SCHEMA_VERSION,
            entities: self.entities.values().collect(),
        };
        serde_json::to_string_pretty(&file).map_err(SceneSaveError::Json)
    }

    /// Returns `true` when an entity with `id` exists in this scene.
    pub(crate) fn contains_entity(&self, id: &EntityId) -> bool {
        self.entities.contains_key(id)
    }

    /// Returns a mutable reference to the entity with `id`.
    pub(crate) fn entity_mut(&mut self, id: &EntityId) -> Option<&mut AuthoringEntity> {
        self.entities.get_mut(id)
    }

    /// Inserts `entity` into the scene without checking for duplicates.
    ///
    /// Callers MUST verify that no entity with the same ID exists before
    /// calling this method.
    pub(crate) fn insert_entity(&mut self, entity: AuthoringEntity) {
        self.entities.insert(entity.id.clone(), entity);
    }

    /// Removes the entity with `id` and returns it, or `None` if absent.
    pub(crate) fn remove_entity(&mut self, id: &EntityId) -> Option<AuthoringEntity> {
        self.entities.remove(id)
    }

    /// Returns `true` when any entity in this scene has `id` as its parent.
    pub(crate) fn has_children(&self, id: &EntityId) -> bool {
        self.entities
            .values()
            .any(|e| e.parent.as_ref() == Some(id))
    }

    pub(crate) fn identity(&self) -> u64 {
        self.identity
    }

    /// Returns the logical content revision of this scene.
    ///
    /// Revisions advance after committed authoring changes and are restored by
    /// undo/redo. Editors can therefore compare this value with a saved
    /// checkpoint without serializing the scene on every edit.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn commit_from(&mut self, mut working: AuthoringScene) {
        working.identity = self.identity;
        // Draw a process-unique token instead of incrementing the restored
        // snapshot's number. A new edit after undo must not collide with the
        // revision of a different, previously saved history branch.
        working.revision = next_scene_revision();
        working.generation = self.generation.saturating_add(1);
        *self = working;
    }

    pub(crate) fn restore_from(&mut self, mut snapshot: AuthoringScene) {
        snapshot.identity = self.identity;
        snapshot.generation = self.generation.saturating_add(1);
        *self = snapshot;
    }
}

impl Default for AuthoringScene {
    fn default() -> Self {
        Self::new()
    }
}

/// Reports why a scene could not be serialized.
#[derive(Debug)]
pub enum SceneSaveError {
    /// Scene semantics are invalid.
    ValidationFailed {
        /// All diagnostics produced by scene validation.
        diagnostics: Vec<Diagnostic>,
    },
    /// JSON serialization failed.
    Json(serde_json::Error),
}

impl fmt::Display for SceneSaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => write!(
                formatter,
                "scene cannot be saved because validation produced {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::Json(error) => write!(formatter, "scene JSON serialization failed: {error}"),
        }
    }
}

impl std::error::Error for SceneSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ValidationFailed { .. } => None,
            Self::Json(error) => Some(error),
        }
    }
}

#[derive(Serialize)]
struct SceneFileRef<'a> {
    schema_version: u32,
    entities: Vec<&'a AuthoringEntity>,
}

fn next_scene_identity() -> u64 {
    static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

fn next_scene_revision() -> u64 {
    static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

fn validate_parent_hierarchy(scene: &AuthoringScene, diagnostics: &mut Vec<Diagnostic>) {
    for (id, entity) in &scene.entities {
        if entity.parent.as_ref() == Some(id) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.self_parent",
                    format!("entity `{}` cannot be its own parent", id.as_str()),
                )
                .with_target(DiagnosticTarget::Entity { id: id.clone() }),
            );
            continue;
        }

        let mut visited = BTreeSet::new();
        let mut current = id;
        while let Some(parent) = scene
            .entities
            .get(current)
            .and_then(|current_entity| current_entity.parent.as_ref())
        {
            if !scene.entities.contains_key(parent) {
                break;
            }
            if !visited.insert(current.clone()) {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.parent_cycle",
                        format!("entity `{}` belongs to a parent cycle", id.as_str()),
                    )
                    .with_target(DiagnosticTarget::Entity { id: id.clone() }),
                );
                break;
            }
            current = parent;
        }
    }
}

fn validate_value_references(
    scene: &AuthoringScene,
    entity: &EntityId,
    component_type: &ComponentTypeId,
    value: &Value,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        Value::EntityRef(target) if !scene.entities.contains_key(target) => diagnostics.push(
            Diagnostic::error(
                "scene.bad_entity_ref",
                format!(
                    "component `{}` on entity `{}` references missing entity `{}`",
                    component_type.as_str(),
                    entity.as_str(),
                    target.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: entity.clone(),
                component_type: component_type.clone(),
            }),
        ),
        Value::AssetRef(asset) => validate_asset_ref(asset, entity, component_type, diagnostics),
        Value::F64(number) if !number.is_finite() => diagnostics.push(
            Diagnostic::error(
                "scene.non_finite_number",
                format!(
                    "component `{}` on entity `{}` contains a non-finite number",
                    component_type.as_str(),
                    entity.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: entity.clone(),
                component_type: component_type.clone(),
            }),
        ),
        Value::Array(values) => {
            for value in values {
                validate_value_references(scene, entity, component_type, value, diagnostics);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_value_references(scene, entity, component_type, value, diagnostics);
            }
        }
        Value::Null
        | Value::Bool(_)
        | Value::I64(_)
        | Value::U64(_)
        | Value::F64(_)
        | Value::String(_)
        | Value::EntityRef(_) => {}
    }
}

fn validate_asset_ref(
    asset: &AssetId,
    entity: &EntityId,
    component_type: &ComponentTypeId,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if AssetId::from_stable_id(asset.as_stable_id().clone()).is_err() {
        diagnostics.push(
            Diagnostic::error(
                "scene.bad_asset_ref",
                format!(
                    "component `{}` on entity `{}` contains invalid asset reference `{}`",
                    component_type.as_str(),
                    entity.as_str(),
                    asset.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: entity.clone(),
                component_type: component_type.clone(),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::EntityId;

    #[test]
    fn empty_scene_has_no_entities() {
        let scene = AuthoringScene::new();
        assert_eq!(scene.entity_count(), 0);
        assert_eq!(scene.entities().count(), 0);
    }

    #[test]
    fn entity_is_findable_after_insertion() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let entity = crate::entity::AuthoringEntity::new(id.clone(), "player");
        scene.insert_entity(entity);
        assert!(scene.entity(&id).is_some());
        assert_eq!(scene.entity_count(), 1);
    }

    #[test]
    fn unknown_entity_returns_none() {
        let scene = AuthoringScene::new();
        let id = EntityId::generate();
        assert!(scene.entity(&id).is_none());
    }

    #[test]
    fn validate_reports_missing_parent_and_entity_reference() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let missing_parent = EntityId::generate();
        let missing_target = EntityId::generate();
        let mut entity = crate::entity::AuthoringEntity::new(id, "child");
        entity.parent = Some(missing_parent);
        entity.components.insert(
            ComponentTypeId::new("gameplay.target"),
            Value::EntityRef(missing_target),
        );
        scene.insert_entity(entity);

        let diagnostics = scene.validate();
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.missing_parent"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.bad_entity_ref"));
    }

    #[test]
    fn canonical_save_rejects_invalid_scene() {
        let mut scene = AuthoringScene::new();
        let mut entity = crate::entity::AuthoringEntity::new(EntityId::generate(), "invalid_child");
        entity.parent = Some(EntityId::generate());
        scene.insert_entity(entity);

        assert!(matches!(
            scene.to_canonical_json(),
            Err(SceneSaveError::ValidationFailed { .. })
        ));
    }

    #[test]
    fn validate_reports_non_finite_number() {
        let mut scene = AuthoringScene::new();
        let mut entity = crate::entity::AuthoringEntity::new(EntityId::generate(), "invalid");
        entity.components.insert(
            ComponentTypeId::new("engine.transform"),
            Value::F64(f64::NAN),
        );
        scene.insert_entity(entity);

        assert!(scene
            .validate()
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.non_finite_number"));
    }

    #[test]
    fn validate_reports_self_parent() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut entity = crate::entity::AuthoringEntity::new(id.clone(), "self_parent");
        entity.parent = Some(id);
        scene.insert_entity(entity);

        assert!(scene
            .validate()
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.self_parent"));
    }

    #[test]
    fn canonical_json_includes_schema_version_field() {
        let scene = AuthoringScene::new();
        let json = scene
            .to_canonical_json()
            .expect("empty scene must serialize");
        assert!(
            json.contains("\"schema_version\""),
            "schema_version must appear in canonical JSON: {json}"
        );
        assert!(
            json.contains("\"schema_version\": 1"),
            "schema_version must be 1: {json}"
        );
    }

    #[test]
    fn canonical_json_schema_version_appears_before_entities() {
        let scene = AuthoringScene::new();
        let json = scene
            .to_canonical_json()
            .expect("empty scene must serialize");
        let sv_pos = json
            .find("schema_version")
            .expect("schema_version must exist");
        let ent_pos = json.find("entities").expect("entities must exist");
        assert!(
            sv_pos < ent_pos,
            "schema_version must appear before entities in canonical JSON"
        );
    }

    #[test]
    fn validate_reports_parent_cycle() {
        let mut scene = AuthoringScene::new();
        let first_id = EntityId::generate();
        let second_id = EntityId::generate();
        let mut first = crate::entity::AuthoringEntity::new(first_id.clone(), "first");
        let mut second = crate::entity::AuthoringEntity::new(second_id.clone(), "second");
        first.parent = Some(second_id);
        second.parent = Some(first_id);
        scene.insert_entity(first);
        scene.insert_entity(second);

        assert!(scene
            .validate()
            .iter()
            .any(|diagnostic| diagnostic.code == "scene.parent_cycle"));
    }
}
