//! Skinned Model entities and the renderer parts that reference them.
//!
//! A renderer names its Skinned Model through an `EntityRef` (ADR 0087).
//! Deleting a model therefore has to clear surviving references instead of
//! leaving a dangling one that would block scene validation and saving.

use super::errors::EditorSessionError;
use super::EditorSession;
use engine_authoring::{AuthoringCommand, ComponentTypeId, EntityId, Value};

impl EditorSession {
    /// Clears renderer Model references whose target is being deleted.
    ///
    /// Renderers outside the deleted hierarchy remain valid scene entities in
    /// a recoverable unassigned state instead of retaining a dangling
    /// `EntityRef` that would block scene validation and saving.
    pub(super) fn renderer_model_detach_commands(&self, deleted: &[EntityId]) -> Vec<AuthoringCommand> {
        let Some(scene) = self.scene() else {
            return Vec::new();
        };
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        let mut commands = Vec::new();
        for (entity_id, entity) in scene.entities() {
            if deleted.contains(entity_id) {
                continue;
            }
            let Some(Value::Object(fields)) = entity.components.get(&renderer_type) else {
                continue;
            };
            let referenced = fields
                .get("model")
                .or_else(|| fields.get("rig_source"))
                .or_else(|| fields.get("skeleton"));
            if !matches!(referenced, Some(Value::EntityRef(model)) if deleted.contains(model)) {
                continue;
            }
            let mut fields = fields.clone();
            fields.remove("model");
            fields.remove("rig_source");
            fields.remove("skeleton");
            commands.push(AuthoringCommand::SetComponentValue {
                entity: entity_id.clone(),
                component_type: renderer_type.clone(),
                value: Value::Object(fields),
            });
        }
        commands
    }

    /// Returns each render part of `model` paired with the mesh it draws.
    ///
    /// Parts that carry no resolvable mesh are omitted: they cannot be
    /// matched against the source either way.
    pub fn model_render_parts(
        &self,
        model: &EntityId,
    ) -> Vec<(EntityId, engine_authoring::AssetId)> {
        let Some(scene) = self.scene() else {
            return Vec::new();
        };
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        if !scene.entity(model).is_some_and(|entity| {
            entity.components.contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT,
            ))
        }) {
            return Vec::new();
        }
        scene
            .entities()
            .filter_map(|(part, entity)| {
                let Value::Object(fields) = entity.components.get(&renderer_type)? else {
                    return None;
                };
                let referenced = fields
                    .get("model")
                    .or_else(|| fields.get("rig_source"))
                    .or_else(|| fields.get("skeleton"));
                if !matches!(referenced, Some(Value::EntityRef(owner)) if owner == model) {
                    return None;
                }
                match fields.get("mesh") {
                    Some(Value::AssetRef(mesh)) => Some((part.clone(), mesh.clone())),
                    _ => None,
                }
            })
            .collect()
    }

    /// Returns the Skeleton sub-asset a Skinned Model entity owns.
    pub fn model_skeleton(&self, model: &EntityId) -> Option<engine_authoring::AssetId> {
        let model_type = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
        let Some(Value::Object(fields)) = self
            .scene()?
            .entity(model)
            .and_then(|entity| entity.components.get(&model_type))
        else {
            return None;
        };
        match fields.get("skeleton") {
            Some(Value::AssetRef(skeleton)) => Some(skeleton.clone()),
            _ => None,
        }
    }

    /// Applies a model-part difference by adding one renderer that references
    /// `model` per missing mesh (ADR 0087 5).
    ///
    /// Stale parts are reported by [`engine::model_part_sync`] and
    /// deliberately left in place; only additive repair is automatic, so no
    /// authored part is ever lost to a resync.
    pub fn resync_model_render_parts(
        &mut self,
        model: EntityId,
        sync: &engine::ModelPartSync,
    ) -> Result<usize, EditorSessionError> {
        if sync.missing.is_empty() {
            return Ok(0);
        }
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if !scene.entity(&model).is_some_and(|entity| {
            entity.components.contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT,
            ))
        }) {
            return Err(EditorSessionError::NoSceneDocument);
        }

        let mut commands = Vec::new();
        for (mesh, materials) in &sync.missing {
            let part_id = EntityId::generate();
            commands.push(AuthoringCommand::CreateEntity {
                id: part_id.clone(),
                name: mesh.as_str().to_owned(),
                parent: Some(model.clone()),
            });
            commands.push(AuthoringCommand::AddComponent {
                entity: part_id.clone(),
                component_type: ComponentTypeId::new(
                    engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                ),
                value: engine::skinned_render_part_value(mesh, materials, &model),
            });
        }
        let added = sync.missing.len();
        self.apply_scene_commands(commands)?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TRANSFORM_COMPONENT_ID;
    use std::collections::BTreeMap;

    #[test]
    fn removing_a_skinned_model_clears_surviving_renderer_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skinned_model.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let model = session.create_scene_entity("model").expect("model");
        let renderer = session.create_scene_entity("renderer").expect("renderer");
        let model_type = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        session
            .add_scene_component(
                model.clone(),
                model_type.clone(),
                Value::Object(BTreeMap::new()),
            )
            .expect("model component");
        session
            .add_scene_component(
                renderer.clone(),
                renderer_type.clone(),
                Value::Object(BTreeMap::from([(
                    "model".to_owned(),
                    Value::EntityRef(model.clone()),
                )])),
            )
            .expect("renderer component");

        session
            .remove_scene_component(model.clone(), model_type.clone())
            .expect("remove model and detach renderer");

        assert!(!session
            .scene_entity(&model)
            .expect("model entity remains")
            .components
            .contains_key(&model_type));
        let Value::Object(renderer_fields) = session
            .scene_entity(&renderer)
            .expect("renderer remains")
            .components
            .get(&renderer_type)
            .expect("renderer component remains")
        else {
            panic!("renderer component must remain an object");
        };
        assert!(!renderer_fields.contains_key("model"));
        assert!(session.undo(), "detach and removal must be one undo step");
        assert_eq!(
            session
                .scene_entity(&renderer)
                .and_then(|entity| entity.components.get(&renderer_type))
                .and_then(|value| match value {
                    Value::Object(fields) => fields.get("model"),
                    _ => None,
                }),
            Some(&Value::EntityRef(model))
        );
    }

    #[test]
    fn deleting_a_model_entity_deletes_its_generated_renderer_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete_skinned_model.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let model = session.create_scene_entity("model").expect("model");
        let renderer = EntityId::generate();
        session
            .apply_scene_commands([
                AuthoringCommand::AddComponent {
                    entity: model.clone(),
                    component_type: ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    value: Value::Object(BTreeMap::from([
                        ("x".to_owned(), Value::F64(3.0)),
                        ("y".to_owned(), Value::F64(0.0)),
                        ("z".to_owned(), Value::F64(0.0)),
                    ])),
                },
                AuthoringCommand::AddComponent {
                    entity: model.clone(),
                    component_type: ComponentTypeId::new(
                        engine::scene_bridge::SKINNED_MODEL_COMPONENT,
                    ),
                    value: Value::Object(BTreeMap::new()),
                },
                AuthoringCommand::CreateEntity {
                    id: renderer.clone(),
                    name: "renderer".to_owned(),
                    parent: Some(model.clone()),
                },
                AuthoringCommand::AddComponent {
                    entity: renderer.clone(),
                    component_type: ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    value: Value::Object(BTreeMap::from([
                        ("x".to_owned(), Value::F64(2.0)),
                        ("y".to_owned(), Value::F64(0.0)),
                        ("z".to_owned(), Value::F64(0.0)),
                    ])),
                },
                AuthoringCommand::AddComponent {
                    entity: renderer.clone(),
                    component_type: ComponentTypeId::new(
                        engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                    ),
                    value: Value::Object(BTreeMap::from([(
                        "model".to_owned(),
                        Value::EntityRef(model.clone()),
                    )])),
                },
            ])
            .expect("model hierarchy");

        session
            .delete_scene_entity_subtree(model.clone())
            .expect("delete model");

        assert!(session.scene_entity(&model).is_none());
        assert!(session.scene_entity(&renderer).is_none());
        assert!(session.undo(), "subtree deletion must be one undo step");
        assert_eq!(
            session
                .scene_entity(&renderer)
                .and_then(|entity| entity.parent.as_ref()),
            Some(&model)
        );
    }
}
