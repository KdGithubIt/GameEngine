//! Component add, remove, and value edits on scene entities.
//!
//! Every edit is routed through [`AuthoringCommand`] so validation, undo, and
//! diagnostics behave identically for single-entity and multi-selection edits.

use super::errors::EditorSessionError;
use super::EditorSession;
use engine_authoring::{
    AuthoringCommand, ComponentTypeId, EntityId, PropertyPath, PropertyPathSegment, Value,
};

impl EditorSession {
    /// Adds a component with an initial value to one scene entity.
    pub fn add_scene_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::AddComponent {
            entity,
            component_type,
            value,
        })
    }

    /// Removes one component through the reversible authoring command path.
    pub fn remove_scene_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
    ) -> Result<(), EditorSessionError> {
        let mut commands =
            if component_type.as_str() == engine::scene_bridge::SKINNED_MODEL_COMPONENT {
                self.renderer_model_detach_commands(std::slice::from_ref(&entity))
            } else {
                Vec::new()
            };
        commands.push(AuthoringCommand::RemoveComponent {
            entity,
            component_type,
        });
        self.apply_scene_commands(commands)
    }

    /// Replaces one component's complete value on a scene entity.
    pub fn set_scene_component_value(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetComponentValue {
            entity,
            component_type,
            value,
        })
    }

    /// Replaces one nested property inside a scene component value.
    pub fn set_scene_component_property(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        path: Vec<PropertyPathSegment>,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity,
                component_type,
                segments: path,
            },
            value,
        })
    }

    /// Assigns a dropped asset to a component field on a scene entity (Phase 32).
    ///
    /// The field at `path_segments` inside `component_type` on `entity` is
    /// replaced with a typed [`Value::AssetRef`].
    ///
    /// # Errors
    ///
    /// Returns an error when no scene document is open or when any command
    /// fails validation.
    pub fn assign_asset_to_field(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        path_segments: Vec<PropertyPathSegment>,
        asset_id: engine_authoring::id::AssetId,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity,
                component_type,
                segments: path_segments,
            },
            value: Value::AssetRef(asset_id),
        })
    }

    /// Adds one component to every selected entity atomically.
    pub fn add_scene_component_to_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_commands(entities.into_iter().map(|entity| {
            AuthoringCommand::AddComponent {
                entity,
                component_type: component_type.clone(),
                value: value.clone(),
            }
        }))
    }

    /// Removes one component from every selected entity atomically.
    pub fn remove_scene_component_from_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        component_type: ComponentTypeId,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let mut commands =
            if component_type.as_str() == engine::scene_bridge::SKINNED_MODEL_COMPONENT {
                self.renderer_model_detach_commands(&entities)
            } else {
                Vec::new()
            };
        commands.extend(
            entities
                .into_iter()
                .map(|entity| AuthoringCommand::RemoveComponent {
                    entity,
                    component_type: component_type.clone(),
                }),
        );
        self.apply_scene_commands(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn scene_component_add_and_set_value_update_current_document_scene() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));

        session
            .add_scene_component(entity.clone(), component_type.clone(), value)
            .expect("component add should succeed");
        let edited = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(4.0)),
            ("y".into(), Value::F64(2.0)),
            ("z".into(), Value::F64(1.0)),
        ]));
        session
            .set_scene_component_value(entity.clone(), component_type.clone(), edited.clone())
            .expect("component set should succeed");

        assert_eq!(
            session.scene().unwrap().entity(&entity).unwrap().components[&component_type],
            edited
        );

        session
            .remove_scene_component(entity.clone(), component_type.clone())
            .expect("component remove should succeed");
        assert!(!session
            .scene()
            .unwrap()
            .entity(&entity)
            .unwrap()
            .components
            .contains_key(&component_type));
        assert!(session.undo(), "component removal must be undoable");
        assert_eq!(
            session.scene().unwrap().entity(&entity).unwrap().components[&component_type],
            edited
        );
    }

    #[test]
    fn scene_component_set_property_updates_current_document_scene() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));

        session
            .add_scene_component(entity.clone(), component_type.clone(), value)
            .expect("component add should succeed");
        session
            .set_scene_component_property(
                entity.clone(),
                component_type.clone(),
                vec![PropertyPathSegment::Field { name: "x".into() }],
                Value::F64(4.0),
            )
            .expect("component property set should succeed");

        let Value::Object(component) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object");
        };
        assert_eq!(component.get("x"), Some(&Value::F64(4.0)));
        assert_eq!(component.get("y"), Some(&Value::F64(0.0)));
        assert_eq!(component.get("z"), Some(&Value::F64(0.0)));
    }

    #[test]
    fn set_scene_component_property_undo_restores_previous_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let initial = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));
        session
            .add_scene_component(entity.clone(), component_type.clone(), initial)
            .expect("component add should succeed");

        session
            .set_scene_component_property(
                entity.clone(),
                component_type.clone(),
                vec![PropertyPathSegment::Field { name: "x".into() }],
                Value::F64(7.0),
            )
            .expect("property set should succeed");

        let Value::Object(after) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object after set");
        };
        assert_eq!(after.get("x"), Some(&Value::F64(7.0)));

        assert!(session.undo(), "undo must return true after SetProperty");

        let Value::Object(restored) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object after undo");
        };
        assert_eq!(
            restored.get("x"),
            Some(&Value::F64(0.0)),
            "undo must restore the original field value"
        );
        assert_eq!(restored.get("y"), Some(&Value::F64(0.0)));
    }
}
