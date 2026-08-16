//! Transaction isolation, commit, rollback, and preview diff.
//!
//! A [`Transaction`] applies authoring commands to an isolated working copy
//! of an [`AuthoringScene`]. Changes accumulate until the transaction is
//! committed or rolled back.
//!
//! Required lifecycle (see `AI_FRIENDLY_AUTHORING_SPEC.md` §12):
//!
//! ```text
//! begin
//!   -> apply commands
//!   -> preview diff
//!   -> commit or rollback
//! ```
//!
//! A command that fails adds an error [`Diagnostic`] to the transaction but
//! does not modify the working copy. Further commands can still be applied.
//! A transaction with blocking errors cannot be committed.
//!
//! [`AuthoringScene`]: crate::scene::AuthoringScene
//! [`Diagnostic`]: crate::diagnostic::Diagnostic

use crate::command::{AuthoringCommand, Change, CommandResult, PropertyPath, PropertyPathSegment};
use crate::diagnostic::{Diagnostic, DiagnosticTarget, Severity};
use crate::entity::AuthoringEntity;
use crate::id::{ComponentTypeId, EntityId};
use crate::scene::AuthoringScene;
use crate::value::Value;
use std::collections::BTreeMap;
use std::fmt;

/// Reports why a [`Transaction`] could not be committed.
#[derive(Debug)]
pub enum TransactionError {
    /// One or more commands produced error-level diagnostics that block commit.
    ValidationFailed {
        /// All diagnostics accumulated during the transaction.
        diagnostics: Vec<Diagnostic>,
    },
    /// The destination scene is not the same revision used to begin the
    /// transaction.
    Conflict {
        /// The identity of the scene used to begin the transaction.
        expected_identity: u64,
        /// The revision of the scene used to begin the transaction.
        expected_revision: u64,
        /// The identity of the scene supplied to commit.
        actual_identity: u64,
        /// The current revision of the scene supplied to commit.
        actual_revision: u64,
    },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => {
                let error_count = diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count();
                write!(
                    f,
                    "transaction blocked by {error_count} validation error(s)"
                )
            }
            Self::Conflict {
                expected_identity,
                expected_revision,
                actual_identity,
                actual_revision,
            } => write!(
                f,
                "transaction began from scene {expected_identity} revision {expected_revision}, but commit target is scene {actual_identity} revision {actual_revision}"
            ),
        }
    }
}

impl std::error::Error for TransactionError {}

/// An isolated editing session over an [`AuthoringScene`].
///
/// A transaction works on a working copy of the scene. Changes do not reach
/// the original scene until [`commit`] is called. [`rollback`] discards all
/// changes and leaves the original scene unchanged.
///
/// # Example
///
/// ```
/// use engine_authoring::scene::AuthoringScene;
/// use engine_authoring::transaction::Transaction;
/// use engine_authoring::command::AuthoringCommand;
/// use engine_authoring::id::EntityId;
///
/// let mut scene = AuthoringScene::new();
/// let id = EntityId::generate();
///
/// let mut tx = Transaction::begin(&scene);
/// tx.apply(AuthoringCommand::CreateEntity {
///     id: id.clone(),
///     name: "player".into(),
///     parent: None,
/// });
/// tx.commit(&mut scene).expect("valid transaction must commit");
/// assert!(scene.entity(&id).is_some());
/// ```
///
/// [`commit`]: Transaction::commit
/// [`rollback`]: Transaction::rollback
pub struct Transaction {
    working: AuthoringScene,
    changes: Vec<Change>,
    diagnostics: Vec<Diagnostic>,
    base_identity: u64,
    base_generation: u64,
}

impl Transaction {
    /// Begins a new transaction over `scene`.
    ///
    /// The transaction works on a snapshot clone of `scene`. The original
    /// `scene` is not modified until [`commit`] is called.
    ///
    /// [`commit`]: Transaction::commit
    pub fn begin(scene: &AuthoringScene) -> Self {
        Self {
            working: scene.clone(),
            changes: Vec::new(),
            diagnostics: Vec::new(),
            base_identity: scene.identity(),
            base_generation: scene.generation(),
        }
    }

    /// Applies `command` to the working copy and returns the result.
    ///
    /// A failed command adds an error diagnostic to this transaction but
    /// leaves the working copy unchanged. Subsequent commands can still be
    /// applied. Use [`has_blocking_errors`] to check accumulated state, or
    /// inspect the returned [`CommandResult`] for per-command diagnostics.
    ///
    /// [`has_blocking_errors`]: Transaction::has_blocking_errors
    pub fn apply(&mut self, command: AuthoringCommand) -> CommandResult {
        match command {
            AuthoringCommand::CreateEntity { id, name, parent } => {
                self.apply_create_entity(id, name, parent)
            }
            AuthoringCommand::SetEntityName { entity, name } => {
                self.apply_set_entity_name(entity, name)
            }
            AuthoringCommand::SetEntityDisplayName {
                entity,
                display_name,
            } => self.apply_set_entity_display_name(entity, display_name),
            AuthoringCommand::SetEntityDescription {
                entity,
                description,
            } => self.apply_set_entity_description(entity, description),
            AuthoringCommand::SetEntityEnabled { entity, enabled } => {
                self.apply_set_entity_enabled(entity, enabled)
            }
            AuthoringCommand::SetEntityParent { entity, parent } => {
                self.apply_set_entity_parent(entity, parent)
            }
            AuthoringCommand::AddComponent {
                entity,
                component_type,
                value,
            } => self.apply_add_component(entity, component_type, value),
            AuthoringCommand::RemoveComponent {
                entity,
                component_type,
            } => self.apply_remove_component(entity, component_type),
            AuthoringCommand::SetComponentValue {
                entity,
                component_type,
                value,
            } => self.apply_set_component_value(entity, component_type, value),
            AuthoringCommand::SetProperty { target, value } => {
                self.apply_set_property(target, value)
            }
            AuthoringCommand::DeleteEntity { entity } => self.apply_delete_entity(entity),
        }
    }

    /// Returns all semantic changes accumulated since the transaction began.
    ///
    /// This is the preview diff: the set of changes that will be applied to
    /// the original scene if [`commit`] succeeds.
    ///
    /// [`commit`]: Transaction::commit
    pub fn preview_diff(&self) -> &[Change] {
        &self.changes
    }

    /// Returns the diagnostics that would be considered by a commit.
    ///
    /// This combines diagnostics produced while applying commands with
    /// validation of the isolated working copy. The transaction remains open
    /// and the source Scene is not mutated, which makes this suitable for
    /// structured preview clients.
    pub fn preview_diagnostics(&self) -> Vec<Diagnostic> {
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.extend(self.working.validate());
        diagnostics
    }

    /// Returns all diagnostics accumulated during this transaction.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Returns `true` when any accumulated diagnostic blocks commit.
    pub fn has_blocking_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Commits the transaction to `scene` when there are no blocking errors.
    ///
    /// On success, `scene` is replaced with the working copy and the
    /// accumulated [`Change`] list is returned.
    ///
    /// # Errors
    ///
    /// Returns [`TransactionError::ValidationFailed`] when any accumulated
    /// diagnostic has [`Severity::Error`] level. The original `scene` is
    /// not modified.
    pub fn commit(mut self, scene: &mut AuthoringScene) -> Result<Vec<Change>, TransactionError> {
        if scene.identity() != self.base_identity || scene.generation() != self.base_generation {
            return Err(TransactionError::Conflict {
                expected_identity: self.base_identity,
                expected_revision: self.base_generation,
                actual_identity: scene.identity(),
                actual_revision: scene.generation(),
            });
        }
        self.diagnostics.extend(self.working.validate());
        if self.has_blocking_errors() {
            return Err(TransactionError::ValidationFailed {
                diagnostics: self.diagnostics,
            });
        }
        scene.commit_from(self.working);
        Ok(self.changes)
    }

    /// Discards the working copy without modifying the original scene.
    ///
    /// This is equivalent to dropping the transaction. The explicit method
    /// documents intent at the call site.
    pub fn rollback(self) {
        // Dropping self discards the working copy.
    }

    // --- private command implementations ---

    fn fail(&mut self, diag: Diagnostic) -> CommandResult {
        self.diagnostics.push(diag.clone());
        CommandResult {
            changes: vec![],
            diagnostics: vec![diag],
            inverse: None,
        }
    }

    fn entity_not_found(&mut self, id: &EntityId) -> CommandResult {
        self.fail(
            Diagnostic::error(
                "entity.not_found",
                format!("entity `{}` not found", id.as_str()),
            )
            .with_target(DiagnosticTarget::Entity { id: id.clone() }),
        )
    }

    fn component_not_found(&mut self, entity: &EntityId, ct: &ComponentTypeId) -> CommandResult {
        self.fail(
            Diagnostic::error(
                "component.not_found",
                format!(
                    "component `{}` not found on entity `{}`",
                    ct.as_str(),
                    entity.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: entity.clone(),
                component_type: ct.clone(),
            }),
        )
    }

    fn property_not_found(&mut self, target: &PropertyPath) -> CommandResult {
        self.fail(
            Diagnostic::error(
                "property.not_found",
                format!(
                    "property path not found in component `{}` on entity `{}`",
                    target.component_type.as_str(),
                    target.entity.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: target.entity.clone(),
                component_type: target.component_type.clone(),
            }),
        )
    }

    fn property_type_mismatch(&mut self, target: &PropertyPath) -> CommandResult {
        self.fail(
            Diagnostic::error(
                "property.type_mismatch",
                format!(
                    "property path shape does not match component `{}` on entity `{}`",
                    target.component_type.as_str(),
                    target.entity.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Component {
                entity: target.entity.clone(),
                component_type: target.component_type.clone(),
            }),
        )
    }

    fn apply_create_entity(
        &mut self,
        id: EntityId,
        name: String,
        parent: Option<EntityId>,
    ) -> CommandResult {
        if self.working.contains_entity(&id) {
            return self.fail(
                Diagnostic::error(
                    "entity.already_exists",
                    format!("entity `{}` already exists in this scene", id.as_str()),
                )
                .with_target(DiagnosticTarget::Entity { id: id.clone() }),
            );
        }
        let entity = AuthoringEntity {
            id: id.clone(),
            name: name.clone(),
            display_name: String::new(),
            description: String::new(),
            parent: parent.clone(),
            enabled: true,
            components: BTreeMap::new(),
        };
        self.working.insert_entity(entity);
        let change = Change::EntityCreated { id, name, parent };
        self.changes.push(change.clone());
        // CreateEntity inverse (DestroyEntity) is not yet a defined command.
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: None,
        }
    }

    fn apply_set_entity_name(&mut self, entity: EntityId, name: String) -> CommandResult {
        let old = {
            let e = match self.working.entity_mut(&entity) {
                None => return self.entity_not_found(&entity),
                Some(e) => e,
            };
            let old = e.name.clone();
            e.name = name.clone();
            old
        };
        let change = Change::EntityNameChanged {
            entity: entity.clone(),
            old: old.clone(),
            new: name.clone(),
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::SetEntityName { entity, name: old }),
        }
    }

    fn apply_set_entity_display_name(
        &mut self,
        entity: EntityId,
        display_name: String,
    ) -> CommandResult {
        let old = {
            let e = match self.working.entity_mut(&entity) {
                None => return self.entity_not_found(&entity),
                Some(e) => e,
            };
            let old = e.display_name.clone();
            e.display_name = display_name.clone();
            old
        };
        let change = Change::EntityDisplayNameChanged {
            entity: entity.clone(),
            old: old.clone(),
            new: display_name.clone(),
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::SetEntityDisplayName {
                entity,
                display_name: old,
            }),
        }
    }

    fn apply_set_entity_enabled(&mut self, entity: EntityId, enabled: bool) -> CommandResult {
        let old = {
            let e = match self.working.entity_mut(&entity) {
                None => return self.entity_not_found(&entity),
                Some(e) => e,
            };
            let old = e.enabled;
            e.enabled = enabled;
            old
        };
        let change = Change::EntityEnabledChanged {
            entity: entity.clone(),
            old,
            new: enabled,
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::SetEntityEnabled {
                entity,
                enabled: old,
            }),
        }
    }

    fn apply_set_entity_description(
        &mut self,
        entity: EntityId,
        description: String,
    ) -> CommandResult {
        let old = {
            let e = match self.working.entity_mut(&entity) {
                None => return self.entity_not_found(&entity),
                Some(e) => e,
            };
            let old = e.description.clone();
            e.description = description.clone();
            old
        };
        let change = Change::EntityDescriptionChanged {
            entity: entity.clone(),
            old: old.clone(),
            new: description.clone(),
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::SetEntityDescription {
                entity,
                description: old,
            }),
        }
    }

    fn apply_set_entity_parent(
        &mut self,
        entity: EntityId,
        parent: Option<EntityId>,
    ) -> CommandResult {
        if !self.working.contains_entity(&entity) {
            return self.entity_not_found(&entity);
        }
        if parent.as_ref() == Some(&entity) {
            return self.fail(
                Diagnostic::error(
                    "entity.parent_cycle",
                    format!("entity `{}` cannot be its own parent", entity.as_str()),
                )
                .with_target(DiagnosticTarget::Entity { id: entity }),
            );
        }
        if let Some(parent_id) = &parent {
            if !self.working.contains_entity(parent_id) {
                return self.entity_not_found(parent_id);
            }
            let mut ancestor = Some(parent_id.clone());
            while let Some(candidate) = ancestor {
                if candidate == entity {
                    return self.fail(
                        Diagnostic::error(
                            "entity.parent_cycle",
                            format!(
                                "reparenting `{}` below `{}` would create a hierarchy cycle",
                                entity.as_str(),
                                parent_id.as_str()
                            ),
                        )
                        .with_target(DiagnosticTarget::Entity { id: entity }),
                    );
                }
                ancestor = self
                    .working
                    .entity(&candidate)
                    .and_then(|item| item.parent.clone());
            }
        }
        let old = self
            .working
            .entity(&entity)
            .and_then(|item| item.parent.clone());
        if old == parent {
            return CommandResult {
                changes: Vec::new(),
                diagnostics: Vec::new(),
                inverse: None,
            };
        }
        self.working
            .entity_mut(&entity)
            .expect("entity existence checked above")
            .parent = parent.clone();
        let change = Change::EntityParentChanged {
            entity: entity.clone(),
            old: old.clone(),
            new: parent.clone(),
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(AuthoringCommand::SetEntityParent {
                entity,
                parent: old,
            }),
        }
    }

    fn apply_add_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> CommandResult {
        if !self.working.contains_entity(&entity) {
            return self.entity_not_found(&entity);
        }
        // Check and mutate in a scoped borrow so `self.changes` can be
        // accessed afterwards.
        let already_exists = {
            let e = self
                .working
                .entity_mut(&entity)
                .expect("entity existence was verified by contains_entity immediately above");
            if e.components.contains_key(&component_type) {
                true
            } else {
                // Clone `value` for the working copy. The original is moved
                // into Change::ComponentAdded below so the diff record and the
                // inverse RemoveComponent command have access to it without an
                // extra allocation.
                e.components.insert(component_type.clone(), value.clone());
                false
            }
        };
        if already_exists {
            return self.fail(
                Diagnostic::error(
                    "component.already_exists",
                    format!(
                        "component `{}` already on entity `{}`",
                        component_type.as_str(),
                        entity.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Component {
                    entity: entity.clone(),
                    component_type: component_type.clone(),
                }),
            );
        }
        let change = Change::ComponentAdded {
            entity: entity.clone(),
            component_type: component_type.clone(),
            value,
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::RemoveComponent {
                entity,
                component_type,
            }),
        }
    }

    fn apply_remove_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
    ) -> CommandResult {
        if !self.working.contains_entity(&entity) {
            return self.entity_not_found(&entity);
        }
        let removed = {
            let e = self
                .working
                .entity_mut(&entity)
                .expect("entity existence was verified by contains_entity immediately above");
            e.components.remove(&component_type)
        };
        match removed {
            None => self.component_not_found(&entity, &component_type),
            Some(old_value) => {
                let change = Change::ComponentRemoved {
                    entity: entity.clone(),
                    component_type: component_type.clone(),
                    old_value: old_value.clone(),
                };
                self.changes.push(change.clone());
                CommandResult {
                    changes: vec![change],
                    diagnostics: vec![],
                    inverse: Some(AuthoringCommand::AddComponent {
                        entity,
                        component_type,
                        value: old_value,
                    }),
                }
            }
        }
    }

    fn apply_delete_entity(&mut self, entity: EntityId) -> CommandResult {
        if !self.working.contains_entity(&entity) {
            return self.entity_not_found(&entity);
        }
        if self.working.has_children(&entity) {
            return self.fail(
                Diagnostic::error(
                    "entity.has_children",
                    format!(
                        "entity `{}` cannot be deleted because it has child entities; \
                         remove or reparent all children first",
                        entity.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Entity { id: entity.clone() }),
            );
        }
        let removed = self
            .working
            .remove_entity(&entity)
            .expect("entity existence was verified by contains_entity immediately above");
        let change = Change::EntityRemoved {
            entity: removed.clone(),
        };
        self.changes.push(change.clone());
        // No single-command inverse; Change::EntityRemoved holds the full
        // entity state for future reconstruction via snapshot undo (ADR 0018).
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: None,
        }
    }

    fn apply_set_component_value(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> CommandResult {
        if !self.working.contains_entity(&entity) {
            return self.entity_not_found(&entity);
        }
        let old_value = {
            let e = self
                .working
                .entity_mut(&entity)
                .expect("entity existence was verified by contains_entity immediately above");
            match e.components.get_mut(&component_type) {
                None => None,
                Some(existing) => {
                    let old = existing.clone();
                    *existing = value.clone();
                    Some(old)
                }
            }
        };
        match old_value {
            None => self.component_not_found(&entity, &component_type),
            Some(old) => {
                let change = Change::ComponentValueChanged {
                    entity: entity.clone(),
                    component_type: component_type.clone(),
                    old_value: old.clone(),
                    new_value: value,
                };
                self.changes.push(change.clone());
                CommandResult {
                    changes: vec![change],
                    diagnostics: vec![],
                    inverse: Some(AuthoringCommand::SetComponentValue {
                        entity,
                        component_type,
                        value: old,
                    }),
                }
            }
        }
    }

    fn apply_set_property(&mut self, target: PropertyPath, value: Value) -> CommandResult {
        if !self.working.contains_entity(&target.entity) {
            return self.entity_not_found(&target.entity);
        }

        let current_component = {
            let entity = self
                .working
                .entity(&target.entity)
                .expect("entity existence was verified by contains_entity immediately above");
            entity.components.get(&target.component_type).cloned()
        };
        let Some(mut new_component) = current_component else {
            return self.component_not_found(&target.entity, &target.component_type);
        };

        let old_value =
            match set_property_value(&mut new_component, &target.segments, value.clone()) {
                Ok(old_value) => old_value,
                Err(PropertyPathError::NotFound) => return self.property_not_found(&target),
                Err(PropertyPathError::TypeMismatch) => {
                    return self.property_type_mismatch(&target)
                }
            };

        {
            let entity = self
                .working
                .entity_mut(&target.entity)
                .expect("entity existence was verified by contains_entity immediately above");
            entity
                .components
                .insert(target.component_type.clone(), new_component);
        }

        let change = Change::ComponentPropertyChanged {
            entity: target.entity.clone(),
            component_type: target.component_type.clone(),
            path: target.segments.clone(),
            old_value: old_value.clone(),
            new_value: value,
        };
        self.changes.push(change.clone());
        CommandResult {
            changes: vec![change],
            diagnostics: vec![],
            inverse: Some(AuthoringCommand::SetProperty {
                target,
                value: old_value,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropertyPathError {
    NotFound,
    TypeMismatch,
}

fn set_property_value(
    component: &mut Value,
    segments: &[PropertyPathSegment],
    value: Value,
) -> Result<Value, PropertyPathError> {
    let target = value_at_path_mut(component, segments)?;
    let old = target.clone();
    *target = value;
    Ok(old)
}

fn value_at_path_mut<'a>(
    value: &'a mut Value,
    segments: &[PropertyPathSegment],
) -> Result<&'a mut Value, PropertyPathError> {
    let Some((first, rest)) = segments.split_first() else {
        return Ok(value);
    };

    match first {
        PropertyPathSegment::Field { name } => match value {
            Value::Object(object) => {
                let child = object.get_mut(name).ok_or(PropertyPathError::NotFound)?;
                value_at_path_mut(child, rest)
            }
            _ => Err(PropertyPathError::TypeMismatch),
        },
        PropertyPathSegment::Index { index } => match value {
            Value::Array(values) => {
                let child = values.get_mut(*index).ok_or(PropertyPathError::NotFound)?;
                value_at_path_mut(child, rest)
            }
            _ => Err(PropertyPathError::TypeMismatch),
        },
    }
}

/// Owns one authoring scene and an in-memory undo and redo history.
///
/// History exists only for the lifetime of this value and is never serialized
/// into project files. Each committed transaction creates one undo entry.
const AUTHORING_UNDO_LIMIT: usize = 100;

/// Owns a committed scene plus bounded, in-memory undo and redo snapshots.
pub struct AuthoringSession {
    scene: AuthoringScene,
    undo: Vec<AuthoringScene>,
    redo: Vec<AuthoringScene>,
}

impl AuthoringSession {
    /// Creates a session around `scene` with empty history.
    pub fn new(scene: AuthoringScene) -> Self {
        Self {
            scene,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Returns the current committed scene.
    pub fn scene(&self) -> &AuthoringScene {
        &self.scene
    }

    /// Returns `true` when this session has a previous committed scene.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns `true` when this session has a redo scene.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Begins a transaction from the current committed scene.
    pub fn begin_transaction(&self) -> Transaction {
        Transaction::begin(&self.scene)
    }

    /// Commits `transaction` and records one session-level undo entry.
    ///
    /// # Errors
    ///
    /// Returns the same validation or conflict errors as
    /// [`Transaction::commit`]. Failed commits do not change the scene or
    /// history.
    pub fn commit(&mut self, transaction: Transaction) -> Result<Vec<Change>, TransactionError> {
        let previous = self.scene.clone();
        let changes = transaction.commit(&mut self.scene)?;
        if changes.is_empty() {
            return Ok(changes);
        }
        if self.undo.len() >= AUTHORING_UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(previous);
        self.redo.clear();
        Ok(changes)
    }

    /// Restores the previous committed scene.
    ///
    /// Returns `false` when no undo entry exists.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.scene.clone());
        self.scene.restore_from(previous);
        true
    }

    /// Reapplies the most recently undone committed scene.
    ///
    /// Returns `false` when no redo entry exists.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        if self.undo.len() >= AUTHORING_UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(self.scene.clone());
        self.scene.restore_from(next);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ComponentTypeId, EntityId};
    use crate::value::Value;

    fn make_scene_with_entity() -> (AuthoringScene, EntityId) {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let entity = AuthoringEntity::new(id.clone(), "player");
        scene.insert_entity(entity);
        (scene, id)
    }

    // --- commit ---

    #[test]
    fn commit_applies_create_entity_to_scene() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "hero".into(),
            parent: None,
        });
        tx.commit(&mut scene).expect("commit must succeed");
        assert!(scene.entity(&id).is_some());
        assert_eq!(scene.entity(&id).unwrap().name, "hero");
    }

    #[test]
    fn commit_returns_accumulated_changes() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "hero".into(),
            parent: None,
        });
        let changes = tx.commit(&mut scene).expect("commit must succeed");
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::EntityCreated { .. }));
    }

    #[test]
    fn commit_applies_multiple_commands_atomically() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "guard".into(),
            parent: None,
        });
        tx.apply(AuthoringCommand::SetEntityDisplayName {
            entity: id.clone(),
            display_name: "Enemy Guard".into(),
        });
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ComponentTypeId::new("gameplay.health"),
            value: Value::I64(100),
        });
        let changes = tx.commit(&mut scene).expect("commit must succeed");
        assert_eq!(changes.len(), 3);
        let entity = scene.entity(&id).unwrap();
        assert_eq!(entity.display_name, "Enemy Guard");
        assert!(entity
            .components
            .contains_key(&ComponentTypeId::new("gameplay.health")));
    }

    // --- rollback ---

    #[test]
    fn rollback_leaves_scene_unchanged() {
        let scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "hero".into(),
            parent: None,
        });
        tx.rollback();
        assert!(
            scene.entity(&id).is_none(),
            "rollback must not modify scene"
        );
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn rolled_back_transaction_leaves_multi_step_edits_undone() {
        let (scene, entity_id) = make_scene_with_entity();
        let original_name = "player".to_owned();

        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::SetEntityName {
            entity: entity_id.clone(),
            name: "renamed".into(),
        });
        tx.rollback();

        assert_eq!(
            scene.entity(&entity_id).unwrap().name,
            original_name,
            "rollback must restore original name"
        );
    }

    // --- preview diff ---

    #[test]
    fn preview_diff_shows_pending_changes_before_commit() {
        let scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "enemy".into(),
            parent: None,
        });
        let diff = tx.preview_diff();
        assert_eq!(diff.len(), 1);
        assert!(matches!(&diff[0], Change::EntityCreated { .. }));
    }

    #[test]
    fn preview_diff_is_empty_when_no_commands_applied() {
        let scene = AuthoringScene::new();
        let tx = Transaction::begin(&scene);
        assert!(tx.preview_diff().is_empty());
    }

    // --- blocking errors ---

    #[test]
    fn transaction_with_errors_cannot_be_committed() {
        let mut scene = AuthoringScene::new();
        let missing_id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::SetEntityName {
            entity: missing_id,
            name: "should_fail".into(),
        });
        assert!(tx.has_blocking_errors());
        assert!(tx.commit(&mut scene).is_err());
    }

    #[test]
    fn failed_command_accumulates_diagnostic_not_change() {
        let scene = AuthoringScene::new();
        let missing_id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetEntityName {
            entity: missing_id,
            name: "fail".into(),
        });
        assert!(!result.is_ok());
        assert!(!result.diagnostics.is_empty());
        assert!(result.changes.is_empty());
        assert_eq!(tx.preview_diff().len(), 0);
        assert_eq!(tx.diagnostics().len(), 1);
    }

    #[test]
    fn failed_command_does_not_corrupt_subsequent_commands() {
        let mut scene = AuthoringScene::new();
        let missing = EntityId::generate();
        let valid_id = EntityId::generate();

        let mut tx = Transaction::begin(&scene);
        // First command fails
        tx.apply(AuthoringCommand::SetEntityName {
            entity: missing,
            name: "bad".into(),
        });
        // Second command would succeed but the transaction has a blocking error
        tx.apply(AuthoringCommand::CreateEntity {
            id: valid_id.clone(),
            name: "valid".into(),
            parent: None,
        });
        // The create was applied to the working copy
        assert_eq!(tx.preview_diff().len(), 1);
        // But commit fails because of the earlier error
        assert!(tx.commit(&mut scene).is_err());
        // Scene is unchanged
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn failed_commit_leaves_scene_unchanged() {
        // This test proves that blocking diagnostics prevent commit and that
        // changes applied to the working copy do NOT reach the original scene.
        let (mut scene, entity_id) = make_scene_with_entity();
        let original_name = scene.entity(&entity_id).unwrap().name.clone();
        let original_count = scene.entity_count();

        let mut tx = Transaction::begin(&scene);

        // This command succeeds in the working copy, changing the entity name.
        tx.apply(AuthoringCommand::SetEntityName {
            entity: entity_id.clone(),
            name: "modified_by_transaction".into(),
        });

        // This command fails and adds a blocking diagnostic.
        let missing = EntityId::generate();
        tx.apply(AuthoringCommand::SetEntityName {
            entity: missing.clone(),
            name: "should_fail".into(),
        });

        assert!(tx.has_blocking_errors());

        // Commit must fail with ValidationFailed.
        let err = tx.commit(&mut scene).unwrap_err();
        assert!(
            matches!(err, TransactionError::ValidationFailed { .. }),
            "commit must return ValidationFailed when blocking errors exist"
        );

        // Entity count must be exactly as before the transaction.
        assert_eq!(
            scene.entity_count(),
            original_count,
            "entity count must not change after failed commit"
        );
        // The entity that was modified in the working copy must retain its
        // original name in the committed scene.
        assert_eq!(
            scene.entity(&entity_id).unwrap().name,
            original_name,
            "working copy name change must not leak into scene after failed commit"
        );
        // An entity that exists only in the working copy must remain absent.
        assert!(
            scene.entity(&missing).is_none(),
            "entities absent from original scene must remain absent after failed commit"
        );
    }

    #[test]
    fn commit_rejects_invalid_working_scene_without_changing_original() {
        let mut scene = AuthoringScene::new();
        let original_count = scene.entity_count();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: EntityId::generate(),
            name: "orphan".into(),
            parent: Some(EntityId::generate()),
        });

        assert!(matches!(
            tx.commit(&mut scene),
            Err(TransactionError::ValidationFailed { .. })
        ));
        assert_eq!(scene.entity_count(), original_count);
    }

    #[test]
    fn commit_rejects_parent_cycle_without_changing_original() {
        let mut scene = AuthoringScene::new();
        let first_id = EntityId::generate();
        let second_id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: first_id.clone(),
            name: "first".into(),
            parent: Some(second_id.clone()),
        });
        tx.apply(AuthoringCommand::CreateEntity {
            id: second_id,
            name: "second".into(),
            parent: Some(first_id),
        });

        assert!(matches!(
            tx.commit(&mut scene),
            Err(TransactionError::ValidationFailed { .. })
        ));
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn commit_rejects_unrelated_scene() {
        let source = AuthoringScene::new();
        let mut unrelated = AuthoringScene::new();
        let mut tx = Transaction::begin(&source);
        tx.apply(AuthoringCommand::CreateEntity {
            id: EntityId::generate(),
            name: "wrong_target".into(),
            parent: None,
        });

        assert!(matches!(
            tx.commit(&mut unrelated),
            Err(TransactionError::Conflict { .. })
        ));
        assert_eq!(unrelated.entity_count(), 0);
    }

    #[test]
    fn commit_rejects_stale_scene_revision() {
        let mut scene = AuthoringScene::new();
        let stale = Transaction::begin(&scene);
        let mut current = Transaction::begin(&scene);
        current.apply(AuthoringCommand::CreateEntity {
            id: EntityId::generate(),
            name: "current".into(),
            parent: None,
        });
        current
            .commit(&mut scene)
            .expect("current transaction must commit");

        assert!(matches!(
            stale.commit(&mut scene),
            Err(TransactionError::Conflict { .. })
        ));
        assert_eq!(scene.entity_count(), 1);
    }

    #[test]
    fn authoring_session_undoes_and_redoes_committed_transaction() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let id = EntityId::generate();
        let mut tx = session.begin_transaction();
        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "session_entity".into(),
            parent: None,
        });
        session.commit(tx).expect("session transaction must commit");
        assert!(session.scene().entity(&id).is_some());

        assert!(session.undo());
        assert!(session.scene().entity(&id).is_none());
        assert!(session.redo());
        assert!(session.scene().entity(&id).is_some());
    }

    #[test]
    fn authoring_session_caps_undo_history_and_ignores_empty_transactions() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        for index in 0..(AUTHORING_UNDO_LIMIT + 5) {
            let mut transaction = session.begin_transaction();
            transaction.apply(AuthoringCommand::CreateEntity {
                id: EntityId::generate(),
                name: format!("entity_{index}"),
                parent: None,
            });
            session.commit(transaction).expect("valid create");
        }
        assert_eq!(session.undo.len(), AUTHORING_UNDO_LIMIT);

        assert!(session.undo());
        let undo_after_one = session.undo.len();
        let redo_after_one = session.redo.len();
        let empty = session.begin_transaction();
        assert!(session.commit(empty).expect("empty transaction").is_empty());
        assert_eq!(session.undo.len(), undo_after_one);
        assert_eq!(session.redo.len(), redo_after_one);
    }

    #[test]
    fn undo_then_new_commit_rejects_transaction_from_old_branch() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let first_id = EntityId::generate();
        let mut first = session.begin_transaction();
        first.apply(AuthoringCommand::CreateEntity {
            id: first_id,
            name: "first".into(),
            parent: None,
        });
        session.commit(first).expect("first commit must succeed");

        let stale = session.begin_transaction();
        assert!(session.undo());

        let replacement_id = EntityId::generate();
        let mut replacement = session.begin_transaction();
        replacement.apply(AuthoringCommand::CreateEntity {
            id: replacement_id,
            name: "replacement".into(),
            parent: None,
        });
        session
            .commit(replacement)
            .expect("replacement commit must succeed");

        assert!(matches!(
            session.commit(stale),
            Err(TransactionError::Conflict { .. })
        ));
    }

    #[test]
    fn new_history_branch_receives_a_distinct_content_revision() {
        let (scene, entity) = make_scene_with_entity();
        let mut session = AuthoringSession::new(scene);

        let mut first = session.begin_transaction();
        first.apply(AuthoringCommand::SetEntityName {
            entity: entity.clone(),
            name: "first_branch".into(),
        });
        session.commit(first).expect("first branch commit");
        let first_branch_revision = session.scene().revision();

        assert!(session.undo());
        let mut second = session.begin_transaction();
        second.apply(AuthoringCommand::SetEntityName {
            entity,
            name: "second_branch".into(),
        });
        session.commit(second).expect("second branch commit");

        assert_ne!(session.scene().revision(), first_branch_revision);
    }

    // --- CreateEntity ---

    #[test]
    fn create_entity_adds_entity_with_correct_name() {
        let mut scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "turret".into(),
            parent: None,
        });
        assert!(result.is_ok());
        tx.commit(&mut scene).unwrap();
        assert_eq!(scene.entity(&id).unwrap().name, "turret");
    }

    #[test]
    fn create_entity_with_duplicate_id_produces_already_exists_diagnostic() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "duplicate".into(),
            parent: None,
        });
        assert!(!result.is_ok());
        assert_eq!(result.diagnostics[0].code, "entity.already_exists");
    }

    // --- SetEntityName ---

    #[test]
    fn set_entity_name_updates_name_and_records_change() {
        let (mut scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetEntityName {
            entity: id.clone(),
            name: "hero".into(),
        });
        assert!(result.is_ok());
        assert!(matches!(
            &result.changes[0],
            Change::EntityNameChanged { new, .. } if new == "hero"
        ));
        tx.commit(&mut scene).unwrap();
        assert_eq!(scene.entity(&id).unwrap().name, "hero");
    }

    #[test]
    fn set_entity_name_on_missing_entity_produces_not_found_diagnostic() {
        let scene = AuthoringScene::new();
        let missing = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetEntityName {
            entity: missing,
            name: "fail".into(),
        });
        assert_eq!(result.diagnostics[0].code, "entity.not_found");
    }

    #[test]
    fn set_entity_name_provides_inverse_command() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetEntityName {
            entity: id.clone(),
            name: "renamed".into(),
        });
        // Inverse should restore the original name "player"
        assert!(matches!(
            result.inverse,
            Some(AuthoringCommand::SetEntityName { ref name, .. }) if name == "player"
        ));
    }

    #[test]
    fn set_entity_enabled_toggles_flag_and_provides_inverse() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetEntityEnabled {
            entity: id.clone(),
            enabled: false,
        });
        assert!(matches!(
            result.changes.as_slice(),
            [Change::EntityEnabledChanged {
                old: true,
                new: false,
                ..
            }]
        ));
        assert!(matches!(
            result.inverse,
            Some(AuthoringCommand::SetEntityEnabled { enabled: true, .. })
        ));
        let mut scene = scene;
        tx.commit(&mut scene).expect("commit disabled flag");
        assert!(!scene.entity(&id).expect("entity").enabled);
    }

    // --- AddComponent ---

    #[test]
    fn add_component_adds_component_to_entity() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(100),
        });
        assert!(result.is_ok());
        tx.commit(&mut scene).unwrap();
        assert!(scene.entity(&id).unwrap().components.contains_key(&ct));
    }

    #[test]
    fn add_component_on_missing_entity_produces_not_found_diagnostic() {
        let scene = AuthoringScene::new();
        let missing = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::AddComponent {
            entity: missing,
            component_type: ComponentTypeId::new("x.y"),
            value: Value::Null,
        });
        assert_eq!(result.diagnostics[0].code, "entity.not_found");
    }

    #[test]
    fn add_duplicate_component_produces_already_exists_diagnostic() {
        let (scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(100),
        });
        let result = tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct,
            value: Value::I64(200),
        });
        assert_eq!(result.diagnostics[0].code, "component.already_exists");
    }

    // --- RemoveComponent ---

    #[test]
    fn remove_component_removes_it_from_entity() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(100),
        });
        let result = tx.apply(AuthoringCommand::RemoveComponent {
            entity: id.clone(),
            component_type: ct.clone(),
        });
        assert!(result.is_ok());
        assert!(matches!(
            &result.changes[0],
            Change::ComponentRemoved { .. }
        ));
        tx.commit(&mut scene).unwrap();
        assert!(!scene.entity(&id).unwrap().components.contains_key(&ct));
    }

    #[test]
    fn remove_nonexistent_component_produces_not_found_diagnostic() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::RemoveComponent {
            entity: id,
            component_type: ComponentTypeId::new("missing.component"),
        });
        assert_eq!(result.diagnostics[0].code, "component.not_found");
    }

    #[test]
    fn remove_component_provides_inverse_add_command_with_old_value() {
        let (scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(75),
        });
        let result = tx.apply(AuthoringCommand::RemoveComponent {
            entity: id,
            component_type: ct,
        });
        assert!(matches!(
            result.inverse,
            Some(AuthoringCommand::AddComponent {
                value: Value::I64(75),
                ..
            })
        ));
    }

    // --- SetComponentValue ---

    #[test]
    fn set_component_value_updates_and_records_change() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(100),
        });
        let result = tx.apply(AuthoringCommand::SetComponentValue {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(50),
        });
        assert!(result.is_ok());
        assert!(matches!(
            &result.changes[0],
            Change::ComponentValueChanged {
                new_value: Value::I64(50),
                old_value: Value::I64(100),
                ..
            }
        ));
        tx.commit(&mut scene).unwrap();
        assert_eq!(scene.entity(&id).unwrap().components[&ct], Value::I64(50));
    }

    #[test]
    fn set_component_value_on_missing_component_produces_not_found_diagnostic() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetComponentValue {
            entity: id,
            component_type: ComponentTypeId::new("missing.type"),
            value: Value::Null,
        });
        assert_eq!(result.diagnostics[0].code, "component.not_found");
    }

    #[test]
    fn set_component_value_provides_inverse_with_old_value() {
        let (scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.score");
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(10),
        });
        let result = tx.apply(AuthoringCommand::SetComponentValue {
            entity: id,
            component_type: ct,
            value: Value::I64(99),
        });
        assert!(matches!(
            result.inverse,
            Some(AuthoringCommand::SetComponentValue {
                value: Value::I64(10),
                ..
            })
        ));
    }

    // --- SetProperty ---

    #[test]
    fn set_property_updates_nested_component_field_and_records_change() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("engine.transform");
        let mut transform = BTreeMap::new();
        transform.insert("x".into(), Value::F64(0.0));
        transform.insert("y".into(), Value::F64(1.0));
        transform.insert("z".into(), Value::F64(2.0));
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::Object(transform),
        });
        setup.commit(&mut scene).unwrap();

        let target = PropertyPath {
            entity: id.clone(),
            component_type: ct.clone(),
            segments: vec![PropertyPathSegment::Field { name: "x".into() }],
        };
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetProperty {
            target,
            value: Value::F64(3.5),
        });

        assert!(result.is_ok());
        assert!(matches!(
            &result.changes[0],
            Change::ComponentPropertyChanged {
                old_value: Value::F64(0.0),
                new_value: Value::F64(3.5),
                ..
            }
        ));
        tx.commit(&mut scene).unwrap();
        let Value::Object(component) = &scene.entity(&id).unwrap().components[&ct] else {
            panic!("transform must remain an object");
        };
        assert_eq!(component.get("x"), Some(&Value::F64(3.5)));
        assert_eq!(component.get("y"), Some(&Value::F64(1.0)));
    }

    #[test]
    fn set_property_provides_inverse_with_old_value() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.stats");
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::Object(BTreeMap::from([("health".into(), Value::I64(10))])),
        });
        setup.commit(&mut scene).unwrap();

        let target = PropertyPath {
            entity: id,
            component_type: ct,
            segments: vec![PropertyPathSegment::Field {
                name: "health".into(),
            }],
        };
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetProperty {
            target,
            value: Value::I64(25),
        });

        assert!(matches!(
            result.inverse,
            Some(AuthoringCommand::SetProperty {
                value: Value::I64(10),
                ..
            })
        ));
    }

    #[test]
    fn set_property_missing_path_produces_diagnostic_without_mutating() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.stats");
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::Object(BTreeMap::from([("health".into(), Value::I64(10))])),
        });
        setup.commit(&mut scene).unwrap();

        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity: id.clone(),
                component_type: ct.clone(),
                segments: vec![PropertyPathSegment::Field {
                    name: "missing".into(),
                }],
            },
            value: Value::I64(25),
        });

        assert_eq!(result.diagnostics[0].code, "property.not_found");
        tx.rollback();
        let Value::Object(component) = &scene.entity(&id).unwrap().components[&ct] else {
            panic!("stats must remain an object");
        };
        assert_eq!(component.get("health"), Some(&Value::I64(10)));
    }

    #[test]
    fn set_property_type_mismatch_produces_diagnostic() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(10),
        });
        setup.commit(&mut scene).unwrap();

        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity: id,
                component_type: ct,
                segments: vec![PropertyPathSegment::Field {
                    name: "current".into(),
                }],
            },
            value: Value::I64(25),
        });

        assert_eq!(result.diagnostics[0].code, "property.type_mismatch");
    }

    // --- loaded entity via transaction ---

    #[test]
    fn entity_loaded_into_scene_can_be_modified_through_transaction() {
        // Simulate loading an entity (e.g. from disk) into a scene, then
        // modifying it through a transaction.
        let entity_id = EntityId::generate();
        let json = format!(
            r#"{{"id":"{id}","name":"loaded_entity","display_name":"Loaded","description":"from file","components":{{}}}}"#,
            id = entity_id.as_str()
        );
        let entity: crate::entity::AuthoringEntity = serde_json::from_str(&json).unwrap();

        let mut scene = AuthoringScene::new();
        scene.insert_entity(entity);

        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::SetEntityDescription {
            entity: entity_id.clone(),
            description: "modified by transaction".into(),
        });
        tx.commit(&mut scene).unwrap();

        assert_eq!(
            scene.entity(&entity_id).unwrap().description,
            "modified by transaction"
        );
        // Original name was not touched
        assert_eq!(scene.entity(&entity_id).unwrap().name, "loaded_entity");
    }

    // --- DeleteEntity ---

    #[test]
    fn delete_entity_removes_entity_from_scene() {
        let (mut scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::DeleteEntity { entity: id.clone() });
        assert!(result.is_ok());
        tx.commit(&mut scene).unwrap();
        assert!(scene.entity(&id).is_none());
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn delete_entity_returns_entity_removed_change_with_full_entity() {
        let (mut scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("gameplay.health");
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::I64(100),
        });
        setup.commit(&mut scene).unwrap();

        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::DeleteEntity { entity: id.clone() });
        assert!(result.is_ok());
        assert_eq!(result.changes.len(), 1);
        match &result.changes[0] {
            Change::EntityRemoved { entity } => {
                assert_eq!(entity.id, id);
                assert_eq!(entity.name, "player");
                assert!(entity.components.contains_key(&ct));
            }
            other => panic!("expected EntityRemoved, got {other:?}"),
        }
    }

    #[test]
    fn delete_entity_on_missing_entity_produces_not_found_diagnostic() {
        let scene = AuthoringScene::new();
        let missing = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::DeleteEntity { entity: missing });
        assert!(!result.is_ok());
        assert_eq!(result.diagnostics[0].code, "entity.not_found");
    }

    #[test]
    fn delete_entity_with_children_produces_has_children_diagnostic() {
        let mut scene = AuthoringScene::new();
        let parent_id = EntityId::generate();
        let child_id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: parent_id.clone(),
            name: "parent".into(),
            parent: None,
        });
        tx.apply(AuthoringCommand::CreateEntity {
            id: child_id,
            name: "child".into(),
            parent: Some(parent_id.clone()),
        });
        tx.commit(&mut scene).unwrap();

        let mut tx2 = Transaction::begin(&scene);
        let result = tx2.apply(AuthoringCommand::DeleteEntity { entity: parent_id });
        assert!(!result.is_ok());
        assert_eq!(result.diagnostics[0].code, "entity.has_children");
    }

    #[test]
    fn delete_entity_has_children_code_is_stable() {
        let mut scene = AuthoringScene::new();
        let parent_id = EntityId::generate();
        let child_id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::CreateEntity {
            id: parent_id.clone(),
            name: "parent".into(),
            parent: None,
        });
        tx.apply(AuthoringCommand::CreateEntity {
            id: child_id,
            name: "child".into(),
            parent: Some(parent_id.clone()),
        });
        tx.commit(&mut scene).unwrap();

        let mut tx2 = Transaction::begin(&scene);
        let r = tx2.apply(AuthoringCommand::DeleteEntity { entity: parent_id });
        assert_eq!(r.diagnostics[0].code, "entity.has_children");
    }

    #[test]
    fn delete_entity_allowed_after_children_removed_in_same_transaction() {
        let mut scene = AuthoringScene::new();
        let parent_id = EntityId::generate();
        let child_id = EntityId::generate();
        let mut setup = Transaction::begin(&scene);
        setup.apply(AuthoringCommand::CreateEntity {
            id: parent_id.clone(),
            name: "parent".into(),
            parent: None,
        });
        setup.apply(AuthoringCommand::CreateEntity {
            id: child_id.clone(),
            name: "child".into(),
            parent: Some(parent_id.clone()),
        });
        setup.commit(&mut scene).unwrap();

        let mut tx = Transaction::begin(&scene);
        tx.apply(AuthoringCommand::DeleteEntity { entity: child_id });
        let result = tx.apply(AuthoringCommand::DeleteEntity { entity: parent_id });
        assert!(
            result.is_ok(),
            "parent delete must succeed after child removed"
        );
        tx.commit(&mut scene).unwrap();
        assert_eq!(scene.entity_count(), 0);
    }

    #[test]
    fn delete_entity_provides_no_inverse_command() {
        let (scene, id) = make_scene_with_entity();
        let mut tx = Transaction::begin(&scene);
        let result = tx.apply(AuthoringCommand::DeleteEntity { entity: id });
        assert!(
            result.inverse.is_none(),
            "DeleteEntity has no single-command inverse"
        );
    }

    #[test]
    fn authoring_session_undoes_delete_entity_via_snapshot() {
        let mut session = AuthoringSession::new(AuthoringScene::new());
        let id = EntityId::generate();

        let mut create = session.begin_transaction();
        create.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "target".into(),
            parent: None,
        });
        session.commit(create).unwrap();
        assert!(session.scene().entity(&id).is_some());

        let mut delete = session.begin_transaction();
        delete.apply(AuthoringCommand::DeleteEntity { entity: id.clone() });
        session.commit(delete).unwrap();
        assert!(session.scene().entity(&id).is_none());

        assert!(session.undo());
        assert!(
            session.scene().entity(&id).is_some(),
            "undo must restore deleted entity"
        );
    }

    // --- diagnostic codes ---

    #[test]
    fn entity_not_found_code_is_stable() {
        let scene = AuthoringScene::new();
        let missing = EntityId::generate();
        let mut tx = Transaction::begin(&scene);

        let r = tx.apply(AuthoringCommand::SetEntityName {
            entity: missing,
            name: "x".into(),
        });
        assert_eq!(r.diagnostics[0].code, "entity.not_found");
    }

    #[test]
    fn entity_already_exists_code_is_stable() {
        let scene = AuthoringScene::new();
        let id = EntityId::generate();
        let mut tx = Transaction::begin(&scene);

        tx.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: "first".into(),
            parent: None,
        });
        let r = tx.apply(AuthoringCommand::CreateEntity {
            id,
            name: "duplicate".into(),
            parent: None,
        });
        assert_eq!(r.diagnostics[0].code, "entity.already_exists");
    }

    #[test]
    fn component_already_exists_code_is_stable() {
        let (scene, id) = make_scene_with_entity();
        let ct = ComponentTypeId::new("test.component");
        let mut tx = Transaction::begin(&scene);

        tx.apply(AuthoringCommand::AddComponent {
            entity: id.clone(),
            component_type: ct.clone(),
            value: Value::Null,
        });
        let r = tx.apply(AuthoringCommand::AddComponent {
            entity: id,
            component_type: ct,
            value: Value::Null,
        });
        assert_eq!(r.diagnostics[0].code, "component.already_exists");
    }

    #[test]
    fn component_not_found_code_is_stable() {
        let (scene, id) = make_scene_with_entity();
        let missing_ct = ComponentTypeId::new("missing.component");
        let mut tx = Transaction::begin(&scene);

        let r = tx.apply(AuthoringCommand::RemoveComponent {
            entity: id.clone(),
            component_type: missing_ct.clone(),
        });
        assert_eq!(r.diagnostics[0].code, "component.not_found");

        let r = tx.apply(AuthoringCommand::SetComponentValue {
            entity: id,
            component_type: missing_ct,
            value: Value::Null,
        });
        assert_eq!(r.diagnostics[0].code, "component.not_found");
    }
}
