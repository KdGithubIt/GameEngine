//! Duplicate and paste for scene entities.
//!
//! Copies are created parent-before-child and every stable ID is regenerated.
//! Parent links and nested [`Value::EntityRef`] values that point inside the
//! copied set are remapped to the new IDs, so a duplicated hierarchy never
//! references the entities it was copied from.

use super::errors::EditorSessionError;
use super::scene_transform::offset_transform;
use super::{EditorSession, TRANSFORM_COMPONENT_ID};
use engine_authoring::{
    AuthoringCommand, AuthoringEntity, AuthoringScene, ComponentTypeId, EntityId, Value,
};
use std::collections::BTreeMap;

impl EditorSession {
    /// Duplicates the scene entity with `src_id`, returning the new entity's ID.
    ///
    /// Creates a new entity with the same name (suffixed `_copy`), description,
    /// and all components copied verbatim with fresh IDs.
    pub fn duplicate_scene_entity(
        &mut self,
        src_id: &EntityId,
    ) -> Result<EntityId, EditorSessionError> {
        let (new_name, display_name, description, parent, components) = {
            let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
            let src = scene
                .entity(src_id)
                .ok_or(EditorSessionError::NoSceneDocument)?;
            let new_name = format!("{}_copy", src.name);
            let display_name = src.display_name.clone();
            let description = src.description.clone();
            let parent = src.parent.clone();
            let components: Vec<(ComponentTypeId, Value)> = src
                .components
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (new_name, display_name, description, parent, components)
        };
        self.create_scene_entity_from_template(
            new_name,
            display_name,
            description,
            parent,
            components,
        )
    }

    /// Duplicates a complete selection as one validated transaction.
    ///
    /// Parent links between selected entities are remapped to their new IDs;
    /// links to non-selected parents remain unchanged. `offset` is added to
    /// every copied transform position.
    pub fn duplicate_scene_entities(
        &mut self,
        selected: impl IntoIterator<Item = EntityId>,
        offset: [f64; 3],
    ) -> Result<Vec<EntityId>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        let selected = selected
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let mut ordered = selected.iter().cloned().collect::<Vec<_>>();
        ordered.sort_by_key(|entity| scene_entity_depth(scene, entity));
        let id_map = ordered
            .iter()
            .cloned()
            .map(|old| (old, EntityId::generate()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut commands = Vec::new();
        for old_id in &ordered {
            let source = scene
                .entity(old_id)
                .ok_or(EditorSessionError::NoSceneDocument)?;
            let new_id = id_map
                .get(old_id)
                .expect("every ordered source receives a generated ID")
                .clone();
            let parent = source
                .parent
                .as_ref()
                .and_then(|parent| id_map.get(parent).cloned())
                .or_else(|| source.parent.clone());
            commands.push(AuthoringCommand::CreateEntity {
                id: new_id.clone(),
                name: format!("{}_copy", source.name),
                parent,
            });
            if !source.display_name.is_empty() {
                commands.push(AuthoringCommand::SetEntityDisplayName {
                    entity: new_id.clone(),
                    display_name: source.display_name.clone(),
                });
            }
            if !source.description.is_empty() {
                commands.push(AuthoringCommand::SetEntityDescription {
                    entity: new_id.clone(),
                    description: source.description.clone(),
                });
            }
            if !source.enabled {
                commands.push(AuthoringCommand::SetEntityEnabled {
                    entity: new_id.clone(),
                    enabled: false,
                });
            }
            for (component_type, value) in &source.components {
                let value = if component_type.as_str() == TRANSFORM_COMPONENT_ID {
                    offset_transform(value, old_id, offset)?
                } else {
                    value.clone()
                };
                commands.push(AuthoringCommand::AddComponent {
                    entity: new_id.clone(),
                    component_type: component_type.clone(),
                    value: remap_copied_entity_refs(&value, &id_map),
                });
            }
        }
        let new_selection = ordered
            .iter()
            .filter_map(|old| id_map.get(old).cloned())
            .collect::<Vec<_>>();
        self.apply_scene_commands(commands)?;
        Ok(new_selection)
    }

    /// Pastes copied scene entities as one validated and undoable transaction.
    ///
    /// Every pasted entity receives a fresh stable ID. Parent links and nested
    /// [`Value::EntityRef`] values that point to another copied entity are
    /// remapped to the corresponding pasted ID. References outside the copied
    /// set are retained only when that entity still exists in the open scene.
    pub fn paste_scene_entities(
        &mut self,
        copied: &[AuthoringEntity],
    ) -> Result<Vec<EntityId>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if copied.is_empty() {
            return Ok(Vec::new());
        }

        let copied_by_id = copied
            .iter()
            .map(|entity| (entity.id.clone(), entity))
            .collect::<BTreeMap<_, _>>();
        let mut ordered = copied.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|entity| copied_entity_depth(&copied_by_id, &entity.id));
        let id_map = ordered
            .iter()
            .map(|entity| (entity.id.clone(), EntityId::generate()))
            .collect::<BTreeMap<_, _>>();

        let mut commands = Vec::new();
        for source in &ordered {
            let new_id = id_map
                .get(&source.id)
                .expect("every copied entity receives a generated ID")
                .clone();
            let parent = source.parent.as_ref().and_then(|parent| {
                id_map.get(parent).cloned().or_else(|| {
                    scene
                        .entity(parent)
                        .is_some()
                        .then(|| parent.clone())
                })
            });
            commands.push(AuthoringCommand::CreateEntity {
                id: new_id.clone(),
                name: format!("{}_paste", source.name),
                parent,
            });
            if !source.display_name.is_empty() {
                commands.push(AuthoringCommand::SetEntityDisplayName {
                    entity: new_id.clone(),
                    display_name: source.display_name.clone(),
                });
            }
            if !source.description.is_empty() {
                commands.push(AuthoringCommand::SetEntityDescription {
                    entity: new_id.clone(),
                    description: source.description.clone(),
                });
            }
            if !source.enabled {
                commands.push(AuthoringCommand::SetEntityEnabled {
                    entity: new_id.clone(),
                    enabled: false,
                });
            }
            commands.extend(source.components.iter().map(|(component_type, value)| {
                AuthoringCommand::AddComponent {
                    entity: new_id.clone(),
                    component_type: component_type.clone(),
                    value: remap_copied_entity_refs(value, &id_map),
                }
            }));
        }

        let pasted = ordered
            .iter()
            .filter_map(|source| id_map.get(&source.id).cloned())
            .collect::<Vec<_>>();
        self.apply_scene_commands(commands)?;
        Ok(pasted)
    }
}

fn scene_entity_depth(scene: &AuthoringScene, entity: &EntityId) -> usize {
    std::iter::successors(
        scene.entity(entity).and_then(|item| item.parent.clone()),
        |parent| scene.entity(parent).and_then(|item| item.parent.clone()),
    )
    .take(scene.entity_count())
    .count()
}

fn copied_entity_depth(
    copied: &BTreeMap<EntityId, &AuthoringEntity>,
    entity: &EntityId,
) -> usize {
    std::iter::successors(
        copied.get(entity).and_then(|item| item.parent.clone()),
        |parent| copied.get(parent).and_then(|item| item.parent.clone()),
    )
    .take(copied.len())
    .count()
}

fn remap_copied_entity_refs(value: &Value, id_map: &BTreeMap<EntityId, EntityId>) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| remap_copied_entity_refs(value, id_map))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        remap_copied_entity_refs(value, id_map),
                    )
                })
                .collect(),
        ),
        Value::EntityRef(entity) => Value::EntityRef(
            id_map
                .get(entity)
                .cloned()
                .unwrap_or_else(|| entity.clone()),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::transform_value;

    #[test]
    fn duplicate_is_atomic_and_preserves_metadata_and_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let parent = session.create_scene_entity("parent").expect("parent");
        let source = session
            .create_scene_entity_from_template(
                "source",
                "Display Name",
                "Description",
                Some(parent.clone()),
                vec![(ComponentTypeId::new("gameplay.health"), Value::I64(100))],
            )
            .expect("source");

        let duplicate = session.duplicate_scene_entity(&source).expect("duplicate");
        let entity = session.scene_entity(&duplicate).expect("duplicated entity");
        assert_eq!(entity.display_name, "Display Name");
        assert_eq!(entity.description, "Description");
        assert_eq!(entity.parent, Some(parent));
        assert_eq!(
            entity.components[&ComponentTypeId::new("gameplay.health")],
            Value::I64(100)
        );

        assert!(session.undo(), "duplicate must be one undo step");
        assert!(session.scene_entity(&duplicate).is_none());
        assert!(session.scene_entity(&source).is_some());
    }

    #[test]
    fn duplicate_selection_remaps_parent_and_is_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let parent = session.create_scene_entity("parent").unwrap();
        let child = session.create_scene_entity("child").unwrap();
        session
            .set_scene_entity_parent(child.clone(), Some(parent.clone()))
            .unwrap();
        for (entity, x) in [(parent.clone(), 1.0), (child.clone(), 2.0)] {
            session
                .add_scene_component(
                    entity,
                    ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    transform_value(x, 0.0, 0.0),
                )
                .unwrap();
        }

        let duplicated = session
            .duplicate_scene_entities([parent.clone(), child.clone()], [3.0, 0.0, 0.0])
            .unwrap();
        let duplicated_parent = duplicated[0].clone();
        let duplicated_child = duplicated[1].clone();
        assert_eq!(
            session.scene_entity(&duplicated_child).unwrap().parent,
            Some(duplicated_parent)
        );
        assert!(session.undo());
        assert!(duplicated
            .iter()
            .all(|id| session.scene_entity(id).is_none()));
        assert!(session.scene_entity(&parent).is_some());
        assert!(session.scene_entity(&child).is_some());
    }

    #[test]
    fn paste_selection_remaps_parent_and_nested_entity_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paste.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let parent = session.create_scene_entity("parent").unwrap();
        let child = session.create_scene_entity("child").unwrap();
        session
            .set_scene_entity_parent(child.clone(), Some(parent.clone()))
            .unwrap();
        session
            .add_scene_component(
                parent.clone(),
                ComponentTypeId::new("game.target"),
                Value::Object(BTreeMap::from([(
                    "nested".to_owned(),
                    Value::Array(vec![Value::EntityRef(child.clone())]),
                )])),
            )
            .unwrap();
        session
            .set_scene_entity_enabled(child.clone(), false)
            .unwrap();
        let copied = [parent, child]
            .iter()
            .map(|id| session.scene_entity(id).unwrap().clone())
            .collect::<Vec<_>>();

        let pasted = session.paste_scene_entities(&copied).unwrap();
        let pasted_parent = pasted[0].clone();
        let pasted_child = pasted[1].clone();
        assert_eq!(
            session.scene_entity(&pasted_child).unwrap().parent,
            Some(pasted_parent.clone())
        );
        assert!(!session.scene_entity(&pasted_child).unwrap().enabled);
        assert_eq!(
            session.scene_entity(&pasted_parent).unwrap().components
                [&ComponentTypeId::new("game.target")],
            Value::Object(BTreeMap::from([(
                "nested".to_owned(),
                Value::Array(vec![Value::EntityRef(pasted_child)]),
            )]))
        );
        assert!(session.undo(), "multi-paste must be one undo step");
        assert!(pasted
            .iter()
            .all(|entity| session.scene_entity(entity).is_none()));
    }
}
