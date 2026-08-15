//! Authoring commands and change records.
//!
//! An [`AuthoringCommand`] is a validated, reversible mutation of authoring
//! data. Commands are applied through a [`Transaction`] rather than directly
//! on [`AuthoringScene`].
//!
//! A [`Change`] records the semantic before-and-after effect of one applied
//! command and forms the basis of preview diffs and future undo
//! implementations.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §11.
//!
//! [`Transaction`]: crate::transaction::Transaction
//! [`AuthoringScene`]: crate::scene::AuthoringScene

use crate::diagnostic::Diagnostic;
use crate::entity::AuthoringEntity;
use crate::id::{ComponentTypeId, EntityId};
use crate::value::Value;
use serde::{Deserialize, Serialize};

/// Identifies one property inside a scene component value.
///
/// Paths are structured data rather than parsed strings. This keeps CLI, MCP,
/// and editor callers on the same serialized contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyPath {
    /// The entity carrying the component to edit.
    pub entity: EntityId,
    /// The component containing the nested property.
    pub component_type: ComponentTypeId,
    /// The nested path inside the component value.
    ///
    /// An empty segment list targets the component value itself. Callers that
    /// intend whole-component replacement should generally use
    /// [`AuthoringCommand::SetComponentValue`] instead.
    pub segments: Vec<PropertyPathSegment>,
}

/// One step in a [`PropertyPath`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyPathSegment {
    /// Selects a named field from a [`Value::Object`].
    Field {
        /// Object field name.
        name: String,
    },
    /// Selects an element from a [`Value::Array`].
    Index {
        /// Zero-based array index.
        index: usize,
    },
}

/// A validated, deterministic mutation of authoring data.
///
/// Commands are applied to an [`AuthoringScene`] through a [`Transaction`].
/// Each applied command produces a [`CommandResult`] describing what changed.
///
/// Applying the same command to the same scene state always produces the same
/// result.
///
/// [`AuthoringScene`]: crate::scene::AuthoringScene
/// [`Transaction`]: crate::transaction::Transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthoringCommand {
    /// Creates a new entity in the scene.
    ///
    /// Fails with `entity.already_exists` when an entity with the same
    /// stable ID already exists.
    CreateEntity {
        /// The stable ID for the new entity. MUST be unique within the scene.
        id: EntityId,
        /// The initial search slug for the new entity.
        name: String,
        /// The optional parent entity.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<EntityId>,
    },
    /// Changes the search slug of an existing entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    SetEntityName {
        /// The entity to modify.
        entity: EntityId,
        /// The new search slug.
        name: String,
    },
    /// Changes the human-readable display label of an existing entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    SetEntityDisplayName {
        /// The entity to modify.
        entity: EntityId,
        /// The new display label.
        display_name: String,
    },
    /// Changes the documentation and AI context description of an entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    SetEntityDescription {
        /// The entity to modify.
        entity: EntityId,
        /// The new description.
        description: String,
    },
    /// Enables or disables an entity (and therefore its whole subtree) for
    /// runtime conversion.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    SetEntityEnabled {
        /// The entity to modify.
        entity: EntityId,
        /// Whether the entity participates in runtime conversion.
        enabled: bool,
    },
    /// Reparents an entity while preserving its stable identity and subtree.
    SetEntityParent {
        /// Entity whose parent link changes.
        entity: EntityId,
        /// New parent, or `None` to make the entity a scene root.
        parent: Option<EntityId>,
    },
    /// Adds a component to an existing entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    /// Fails with `component.already_exists` when the entity already carries
    /// a component of the same type.
    AddComponent {
        /// The entity to add the component to.
        entity: EntityId,
        /// The type of component to add.
        component_type: ComponentTypeId,
        /// The initial component value.
        value: Value,
    },
    /// Removes a component from an existing entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    /// Fails with `component.not_found` when the entity does not carry a
    /// component of the given type.
    RemoveComponent {
        /// The entity to remove the component from.
        entity: EntityId,
        /// The type of component to remove.
        component_type: ComponentTypeId,
    },
    /// Replaces the value of an existing component on an entity.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    /// Fails with `component.not_found` when the entity does not carry a
    /// component of the given type.
    SetComponentValue {
        /// The entity that carries the component.
        entity: EntityId,
        /// The component to update.
        component_type: ComponentTypeId,
        /// The new component value.
        value: Value,
    },
    /// Replaces one nested property inside a component value.
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    /// Fails with `component.not_found` when the entity does not carry a
    /// component of the given type.
    /// Fails with `property.not_found` when a field or index does not exist.
    /// Fails with `property.type_mismatch` when the path expects an object or
    /// array but the current value has another shape.
    SetProperty {
        /// The nested component property to update.
        target: PropertyPath,
        /// The new property value.
        value: Value,
    },
    /// Removes an entity from the scene (v1: leaf entities only).
    ///
    /// Fails with `entity.not_found` when the entity does not exist.
    /// Fails with `entity.has_children` when any other entity in the scene
    /// references this entity as its parent. Remove or reparent all children
    /// before deleting the parent.
    ///
    /// Cascade delete is not implemented in v1.
    DeleteEntity {
        /// The entity to remove.
        entity: EntityId,
    },
}

/// Records the semantic before-and-after effect of one applied
/// [`AuthoringCommand`].
///
/// Changes include both old and new values for all modified fields, enabling
/// human-readable preview diffs and future undo implementations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// A new entity was added to the scene.
    EntityCreated {
        /// The stable ID of the new entity.
        id: EntityId,
        /// The initial name of the new entity.
        name: String,
        /// The parent entity, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<EntityId>,
    },
    /// An entity's search slug changed.
    EntityNameChanged {
        /// The affected entity.
        entity: EntityId,
        /// The name before the change.
        old: String,
        /// The name after the change.
        new: String,
    },
    /// An entity's display label changed.
    EntityDisplayNameChanged {
        /// The affected entity.
        entity: EntityId,
        /// The display name before the change.
        old: String,
        /// The display name after the change.
        new: String,
    },
    /// An entity's description changed.
    EntityDescriptionChanged {
        /// The affected entity.
        entity: EntityId,
        /// The description before the change.
        old: String,
        /// The description after the change.
        new: String,
    },
    /// An entity's enabled flag changed.
    EntityEnabledChanged {
        /// The affected entity.
        entity: EntityId,
        /// The flag before the change.
        old: bool,
        /// The flag after the change.
        new: bool,
    },
    /// An entity's parent link changed.
    EntityParentChanged {
        /// Entity whose hierarchy position changed.
        entity: EntityId,
        /// Previous parent.
        #[serde(skip_serializing_if = "Option::is_none")]
        old: Option<EntityId>,
        /// New parent.
        #[serde(skip_serializing_if = "Option::is_none")]
        new: Option<EntityId>,
    },
    /// A component was added to an entity.
    ComponentAdded {
        /// The entity that received the component.
        entity: EntityId,
        /// The type of the added component.
        component_type: ComponentTypeId,
        /// The initial value of the added component.
        value: Value,
    },
    /// A component was removed from an entity.
    ComponentRemoved {
        /// The entity the component was removed from.
        entity: EntityId,
        /// The type of the removed component.
        component_type: ComponentTypeId,
        /// The value of the component before removal.
        old_value: Value,
    },
    /// A component's value was replaced.
    ComponentValueChanged {
        /// The entity that carries the component.
        entity: EntityId,
        /// The type of the changed component.
        component_type: ComponentTypeId,
        /// The value before the change.
        old_value: Value,
        /// The value after the change.
        new_value: Value,
    },
    /// A nested component property was replaced.
    ComponentPropertyChanged {
        /// The entity that carries the component.
        entity: EntityId,
        /// The type of the changed component.
        component_type: ComponentTypeId,
        /// The nested property that changed.
        path: Vec<PropertyPathSegment>,
        /// The property value before the change.
        old_value: Value,
        /// The property value after the change.
        new_value: Value,
    },
    /// An entity was removed from the scene.
    ///
    /// The full [`AuthoringEntity`] state is preserved so that the entity can
    /// be reconstructed when this change is reversed.
    EntityRemoved {
        /// The complete state of the removed entity, including all components.
        entity: AuthoringEntity,
    },
}

/// The result of applying one [`AuthoringCommand`] to a [`Transaction`].
///
/// A result with `diagnostics` at [`Severity::Error`] level indicates a
/// failed command. Changes and diagnostics from all commands accumulate on
/// the transaction until commit or rollback.
///
/// [`Transaction`]: crate::transaction::Transaction
/// [`Severity::Error`]: crate::diagnostic::Severity::Error
#[derive(Debug)]
pub struct CommandResult {
    /// Semantic changes produced by this command.
    ///
    /// Empty when the command failed validation.
    pub changes: Vec<Change>,

    /// Diagnostics produced by this command.
    ///
    /// Non-empty when the command failed or produced warnings.
    pub diagnostics: Vec<Diagnostic>,

    /// The inverse command that would undo this command, when available.
    ///
    /// `None` when no inverse is defined for the command type or when the
    /// persistent undo storage strategy (Open Decision #4) has not been
    /// selected. Callers MUST NOT depend on this field being non-`None`.
    pub inverse: Option<AuthoringCommand>,
}

impl CommandResult {
    /// Returns `true` when the command produced no blocking error diagnostics.
    pub fn is_ok(&self) -> bool {
        use crate::diagnostic::Severity;
        !self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}
