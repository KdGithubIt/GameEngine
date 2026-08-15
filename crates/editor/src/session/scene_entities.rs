//! Scene entity lifecycle, hierarchy, and authored metadata.
//!
//! Subtree deletion collects descendants before issuing any command so the
//! whole hierarchy is removed inside one transaction and one undo step;
//! deleting a parent ahead of its children would fail validation instead.

use super::errors::EditorSessionError;
use super::EditorSession;
use engine_authoring::{AuthoringCommand, ComponentTypeId, EntityId, Value};

impl EditorSession {
    /// Creates a new root entity in the current scene.
    pub fn create_scene_entity(
        &mut self,
        name: impl Into<String>,
    ) -> Result<EntityId, EditorSessionError> {
        let id = EntityId::generate();
        self.apply_scene_command(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: name.into(),
            parent: None,
        })?;
        Ok(id)
    }

    /// Creates one entity and all preset metadata/components as one undo step.
    ///
    /// This is the shared authoring path used by editor creation templates.
    /// Callers provide concrete component values obtained from registered
    /// schemas, so the session does not acquire an engine dependency.
    pub fn create_scene_entity_with_components(
        &mut self,
        name: impl Into<String>,
        display_name: impl Into<String>,
        description: impl Into<String>,
        components: Vec<(ComponentTypeId, Value)>,
    ) -> Result<EntityId, EditorSessionError> {
        self.create_scene_entity_from_template(name, display_name, description, None, components)
    }

    /// Creates a complete entity template atomically, including hierarchy.
    pub fn create_scene_entity_from_template(
        &mut self,
        name: impl Into<String>,
        display_name: impl Into<String>,
        description: impl Into<String>,
        parent: Option<EntityId>,
        components: Vec<(ComponentTypeId, Value)>,
    ) -> Result<EntityId, EditorSessionError> {
        let id = EntityId::generate();
        let name = name.into();
        let display_name = display_name.into();
        let description = description.into();
        let mut commands = Vec::with_capacity(3 + components.len());
        commands.push(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name,
            parent,
        });
        if !display_name.is_empty() {
            commands.push(AuthoringCommand::SetEntityDisplayName {
                entity: id.clone(),
                display_name,
            });
        }
        if !description.is_empty() {
            commands.push(AuthoringCommand::SetEntityDescription {
                entity: id.clone(),
                description,
            });
        }
        commands.extend(components.into_iter().map(|(component_type, value)| {
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type,
                value,
            }
        }));
        self.apply_scene_commands(commands)?;
        Ok(id)
    }

    /// Deletes an entity from the current scene.
    pub fn delete_scene_entity(&mut self, entity: EntityId) -> Result<(), EditorSessionError> {
        let mut commands = self.renderer_model_detach_commands(std::slice::from_ref(&entity));
        commands.push(AuthoringCommand::DeleteEntity { entity });
        self.apply_scene_commands(commands)
    }

    /// Deletes an entity and all descendants as one reversible transaction.
    pub fn delete_scene_entity_subtree(
        &mut self,
        entity: EntityId,
    ) -> Result<(), EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if scene.entity(&entity).is_none() {
            return Err(EditorSessionError::NoSceneDocument);
        }
        let mut ordered = vec![entity];
        let mut index = 0;
        while index < ordered.len() {
            let parent = ordered[index].clone();
            ordered.extend(
                scene
                    .entities()
                    .filter(|(_, candidate)| candidate.parent.as_ref() == Some(&parent))
                    .map(|(id, _)| id.clone()),
            );
            index += 1;
        }
        ordered.reverse();
        let detach = self.renderer_model_detach_commands(&ordered);
        self.apply_scene_commands(
            detach.into_iter().chain(
                ordered
                    .into_iter()
                    .map(|entity| AuthoringCommand::DeleteEntity { entity }),
            ),
        )
    }

    /// Deletes multiple selected subtrees as one undoable transaction.
    pub fn delete_scene_entity_subtrees(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
    ) -> Result<(), EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        let requested = entities
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut ordered = Vec::new();
        for entity in &requested {
            if scene.entity(entity).is_none() {
                continue;
            }
            let has_selected_ancestor = std::iter::successors(
                scene.entity(entity).and_then(|item| item.parent.clone()),
                |parent| scene.entity(parent).and_then(|item| item.parent.clone()),
            )
            .any(|ancestor| requested.contains(&ancestor));
            if !has_selected_ancestor {
                ordered.push(entity.clone());
            }
        }
        let mut index = 0;
        while index < ordered.len() {
            let parent = ordered[index].clone();
            ordered.extend(
                scene
                    .entities()
                    .filter(|(_, candidate)| candidate.parent.as_ref() == Some(&parent))
                    .map(|(id, _)| id.clone()),
            );
            index += 1;
        }
        let mut seen = std::collections::BTreeSet::new();
        ordered.retain(|entity| seen.insert(entity.clone()));
        ordered.reverse();
        let detach = self.renderer_model_detach_commands(&ordered);
        self.apply_scene_commands(
            detach.into_iter().chain(
                ordered
                    .into_iter()
                    .map(|entity| AuthoringCommand::DeleteEntity { entity }),
            ),
        )
    }

    /// Changes one entity's parent through the shared authoring command path.
    pub fn set_scene_entity_parent(
        &mut self,
        entity: EntityId,
        parent: Option<EntityId>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityParent { entity, parent })
    }

    /// Sets one scene entity's search slug.
    pub fn set_scene_entity_name(
        &mut self,
        entity: EntityId,
        name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityName {
            entity,
            name: name.into(),
        })
    }

    /// Enables or disables one scene entity (and its subtree) for runtime
    /// conversion.
    pub fn set_scene_entity_enabled(
        &mut self,
        entity: EntityId,
        enabled: bool,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityEnabled { entity, enabled })
    }

    /// Sets one scene entity's display label.
    pub fn set_scene_entity_display_name(
        &mut self,
        entity: EntityId,
        display_name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityDisplayName {
            entity,
            display_name: display_name.into(),
        })
    }

    /// Sets one scene entity's description.
    pub fn set_scene_entity_description(
        &mut self,
        entity: EntityId,
        description: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityDescription {
            entity,
            description: description.into(),
        })
    }

    /// Creates a new scene entity from a dropped mesh asset (Phase 32).
    ///
    /// The entity receives an `engine.transform` component at the origin and
    /// one unified `engine.static_mesh_renderer` referencing `asset_id`. The
    /// operation is routed through [`AuthoringCommand`] and is undoable.
    ///
    /// # Errors
    ///
    /// Returns an error when no scene document is open or when any command
    /// fails validation.
    pub fn create_entity_from_mesh_asset(
        &mut self,
        asset_id: engine_authoring::id::AssetId,
        parent: Option<EntityId>,
    ) -> Result<EntityId, EditorSessionError> {
        use std::collections::BTreeMap;
        let id = EntityId::generate();
        let transform_value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));
        let white_material = engine_authoring::id::AssetId::from_stable_id(
            engine_authoring::StableId::new(engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID),
        )
        .expect("built-in white material ID must be valid");
        let renderer_value = Value::Object(BTreeMap::from([
            ("mesh".into(), Value::AssetRef(asset_id)),
            ("material".into(), Value::AssetRef(white_material)),
            ("material_slots".into(), Value::Array(Vec::new())),
        ]));
        self.apply_scene_commands([
            AuthoringCommand::CreateEntity {
                id: id.clone(),
                name: "mesh_entity".into(),
                parent,
            },
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type: ComponentTypeId::new("engine.transform"),
                value: transform_value,
            },
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type: ComponentTypeId::new(
                    engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
                ),
                value: renderer_value,
            },
        ])?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn scene_entity_create_delete_and_undo_redo_use_authoring_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let id = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");

        assert!(session.scene().unwrap().entity(&id).is_some());
        assert!(session.is_dirty());
        assert!(session.can_undo());

        assert!(session.undo());
        assert!(session.scene().unwrap().entity(&id).is_none());
        assert!(session.can_redo());

        assert!(session.redo());
        assert!(session.scene().unwrap().entity(&id).is_some());

        session
            .delete_scene_entity(id.clone())
            .expect("leaf entity delete should succeed");
        assert!(session.scene().unwrap().entity(&id).is_none());
    }

    #[test]
    fn delete_parent_entity_fails_when_it_has_children() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let parent = session
            .create_scene_entity("parent")
            .expect("parent create");
        let child_id = EntityId::generate();
        session
            .apply_scene_command(AuthoringCommand::CreateEntity {
                id: child_id,
                name: "child".into(),
                parent: Some(parent.clone()),
            })
            .expect("child create must succeed");
        let result = session.delete_scene_entity(parent);
        assert!(
            result.is_err(),
            "deleting a parent entity with children must fail with entity.has_children"
        );
    }

    #[test]
    fn subtree_delete_is_atomic_and_undo_restores_every_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subtree.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let parent = session.create_scene_entity("parent").expect("parent");
        let child = EntityId::generate();
        session
            .apply_scene_command(AuthoringCommand::CreateEntity {
                id: child.clone(),
                name: "child".into(),
                parent: Some(parent.clone()),
            })
            .expect("child");

        session
            .delete_scene_entity_subtree(parent.clone())
            .expect("subtree delete");
        assert!(session.scene_entity(&parent).is_none());
        assert!(session.scene_entity(&child).is_none());
        assert!(session.undo(), "subtree delete must be one undo step");
        assert!(session.scene_entity(&parent).is_some());
        assert!(session.scene_entity(&child).is_some());
    }

    #[test]
    fn create_entity_from_mesh_asset_adds_unified_static_mesh_renderer() {
        use engine_authoring::id::AssetId;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let stable = engine_authoring::StableId::new("asset_01JP0000000000000000000DDD");
        let asset_id = AssetId::from_stable_id(stable).expect("id must be valid");
        let entity = session
            .create_entity_from_mesh_asset(asset_id.clone(), None)
            .expect("create must succeed");

        let scene = session.scene().expect("scene must be open");
        assert!(scene.entity(&entity).is_some(), "entity must exist");
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT);
        assert!(
            scene
                .entity(&entity)
                .unwrap()
                .components
                .contains_key(&renderer_type),
            "entity must have a static mesh renderer"
        );
        let Value::Object(renderer) = scene
            .entity(&entity)
            .and_then(|entity| entity.components.get(&renderer_type))
            .expect("renderer value must exist")
        else {
            panic!("renderer value must be an object");
        };
        assert_eq!(
            renderer.get("mesh"),
            Some(&Value::AssetRef(asset_id)),
            "drop must store the mesh asset inside the renderer"
        );
        assert_eq!(
            renderer.get("material"),
            Some(&Value::AssetRef(
                engine_authoring::id::AssetId::from_stable_id(engine_authoring::StableId::new(
                    engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID,
                ),)
                .expect("built-in white material ID must be valid")
            )),
            "new renderers must start with the white fallback material"
        );
        assert!(session.undo(), "the complete drop must be one undo step");
        assert!(
            session
                .scene()
                .expect("scene remains open")
                .entity(&entity)
                .is_none(),
            "one undo must remove the complete dropped entity"
        );
    }

    #[test]
    fn create_entity_from_mesh_asset_as_child_sets_parent() {
        use engine_authoring::id::AssetId;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let parent = session
            .create_scene_entity("parent_entity")
            .expect("parent create");
        let stable = engine_authoring::StableId::new("asset_01JP0000000000000000000EEE");
        let asset_id = AssetId::from_stable_id(stable).expect("id must be valid");

        let child = session
            .create_entity_from_mesh_asset(asset_id, Some(parent.clone()))
            .expect("child create must succeed");

        let scene = session.scene().expect("scene must be open");
        let child_entity = scene.entity(&child).expect("child must exist");
        assert_eq!(
            child_entity.parent,
            Some(parent),
            "child entity must reference the parent"
        );
    }

    #[test]
    fn scene_reparent_is_command_backed_cycle_safe_and_undoable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hierarchy.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let first = session.create_scene_entity("first").unwrap();
        let second = session.create_scene_entity("second").unwrap();

        session
            .set_scene_entity_parent(second.clone(), Some(first.clone()))
            .unwrap();
        assert_eq!(
            session
                .scene_entity(&second)
                .and_then(|item| item.parent.clone()),
            Some(first.clone())
        );
        assert!(session
            .set_scene_entity_parent(first.clone(), Some(second.clone()))
            .is_err());
        assert!(session.undo());
        assert_eq!(
            session
                .scene_entity(&second)
                .and_then(|item| item.parent.clone()),
            None
        );
    }
}
