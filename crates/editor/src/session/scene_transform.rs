//! Batch position edits: offset, absolute set, align, and distribute.
//!
//! Every operation validates all targets before issuing a command, so a
//! selection containing one entity without a usable transform leaves the other
//! entities untouched rather than partially applying the edit.

use super::errors::EditorSessionError;
use super::{EditorSession, TRANSFORM_COMPONENT_ID};
use engine_authoring::{AuthoringCommand, ComponentTypeId, EntityId, Value};

/// Cartesian axis used by repeated scene-authoring operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneAxis {
    /// Horizontal X axis.
    X,
    /// Vertical Y axis.
    Y,
    /// Depth Z axis.
    Z,
}

/// Reference coordinate used when aligning selected entity origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneAlignment {
    /// Smallest coordinate in the selection.
    Minimum,
    /// Midpoint between the smallest and largest coordinates.
    Center,
    /// Largest coordinate in the selection.
    Maximum,
}

impl EditorSession {
    /// Adds a relative position offset to each selected transform atomically.
    pub fn offset_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        offset: [f64; 3],
    ) -> Result<(), EditorSessionError> {
        let commands = self.transform_commands(entities, |_, position| {
            [
                position[0] + offset[0],
                position[1] + offset[1],
                position[2] + offset[2],
            ]
        })?;
        self.apply_scene_commands(commands)
    }

    /// Returns transform positions after validating every requested entity.
    pub fn scene_entity_positions(
        &self,
        entities: &[EntityId],
    ) -> Result<Vec<(EntityId, [f64; 3])>, EditorSessionError> {
        self.scene_positions(entities)
    }

    /// Sets selected position axes to absolute values in one transaction.
    /// `None` preserves the current value for that axis.
    pub fn set_scene_entity_positions(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axes: [Option<f64>; 3],
    ) -> Result<(), EditorSessionError> {
        let commands = self.transform_commands(entities, |_, mut position| {
            for (index, value) in axes.into_iter().enumerate() {
                if let Some(value) = value {
                    position[index] = value;
                }
            }
            position
        })?;
        self.apply_scene_commands(commands)
    }

    /// Aligns selected entity origins on one axis as one undo step.
    pub fn align_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axis: SceneAxis,
        alignment: SceneAlignment,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let positions = self.scene_positions(&entities)?;
        if positions.is_empty() {
            return Ok(());
        }
        let axis_index = scene_axis_index(axis);
        let minimum = positions
            .iter()
            .map(|(_, position)| position[axis_index])
            .fold(f64::INFINITY, f64::min);
        let maximum = positions
            .iter()
            .map(|(_, position)| position[axis_index])
            .fold(f64::NEG_INFINITY, f64::max);
        let target = match alignment {
            SceneAlignment::Minimum => minimum,
            SceneAlignment::Center => (minimum + maximum) * 0.5,
            SceneAlignment::Maximum => maximum,
        };
        let commands = self.transform_commands(entities, |_, mut position| {
            position[axis_index] = target;
            position
        })?;
        self.apply_scene_commands(commands)
    }

    /// Distributes selected entity origins evenly between the endpoints.
    pub fn distribute_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axis: SceneAxis,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let mut positions = self.scene_positions(&entities)?;
        if positions.len() < 3 {
            return Ok(());
        }
        let axis_index = scene_axis_index(axis);
        positions.sort_by(|left, right| left.1[axis_index].total_cmp(&right.1[axis_index]));
        let minimum = positions[0].1[axis_index];
        let maximum = positions[positions.len() - 1].1[axis_index];
        let denominator = (positions.len() - 1) as f64;
        let targets = positions
            .iter()
            .enumerate()
            .map(|(index, (entity, _))| {
                (
                    entity.clone(),
                    minimum + (maximum - minimum) * index as f64 / denominator,
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let commands = self.transform_commands(entities, |entity, mut position| {
            if let Some(target) = targets.get(entity) {
                position[axis_index] = *target;
            }
            position
        })?;
        self.apply_scene_commands(commands)
    }

    fn scene_positions(
        &self,
        entities: &[EntityId],
    ) -> Result<Vec<(EntityId, [f64; 3])>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        entities
            .iter()
            .map(|entity| {
                let value = scene
                    .entity(entity)
                    .and_then(|item| {
                        item.components
                            .get(&ComponentTypeId::new(TRANSFORM_COMPONENT_ID))
                    })
                    .ok_or(EditorSessionError::MissingTransform(entity.clone()))?;
                Ok((entity.clone(), transform_position(value, entity)?))
            })
            .collect()
    }

    fn transform_commands(
        &self,
        entities: impl IntoIterator<Item = EntityId>,
        update: impl Fn(&EntityId, [f64; 3]) -> [f64; 3],
    ) -> Result<Vec<AuthoringCommand>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        entities
            .into_iter()
            .map(|entity| {
                let component_type = ComponentTypeId::new(TRANSFORM_COMPONENT_ID);
                let current = scene
                    .entity(&entity)
                    .and_then(|item| item.components.get(&component_type))
                    .ok_or_else(|| EditorSessionError::MissingTransform(entity.clone()))?;
                let position = transform_position(current, &entity)?;
                let next = set_transform_position(current, &entity, update(&entity, position))?;
                Ok(AuthoringCommand::SetComponentValue {
                    entity,
                    component_type,
                    value: next,
                })
            })
            .collect()
    }
}

fn scene_axis_index(axis: SceneAxis) -> usize {
    match axis {
        SceneAxis::X => 0,
        SceneAxis::Y => 1,
        SceneAxis::Z => 2,
    }
}

fn transform_position(value: &Value, entity: &EntityId) -> Result<[f64; 3], EditorSessionError> {
    let Value::Object(fields) = value else {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    };
    let number = |field: &str| match fields.get(field) {
        Some(Value::F64(value)) if value.is_finite() => Some(*value),
        Some(Value::I64(value)) => Some(*value as f64),
        Some(Value::U64(value)) => Some(*value as f64),
        _ => None,
    };
    Ok([
        number("x").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
        number("y").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
        number("z").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
    ])
}

fn set_transform_position(
    value: &Value,
    entity: &EntityId,
    position: [f64; 3],
) -> Result<Value, EditorSessionError> {
    if position.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    }
    let Value::Object(mut fields) = value.clone() else {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    };
    fields.insert("x".to_owned(), Value::F64(position[0]));
    fields.insert("y".to_owned(), Value::F64(position[1]));
    fields.insert("z".to_owned(), Value::F64(position[2]));
    Ok(Value::Object(fields))
}

pub(super) fn offset_transform(
    value: &Value,
    entity: &EntityId,
    offset: [f64; 3],
) -> Result<Value, EditorSessionError> {
    let position = transform_position(value, entity)?;
    set_transform_position(
        value,
        entity,
        [
            position[0] + offset[0],
            position[1] + offset[1],
            position[2] + offset[2],
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::transform_value;

    #[test]
    fn failed_batch_transform_does_not_modify_valid_targets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let valid = session.create_scene_entity("valid").unwrap();
        let invalid = session.create_scene_entity("invalid").unwrap();
        session
            .add_scene_component(
                valid.clone(),
                ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                transform_value(4.0, 0.0, 0.0),
            )
            .unwrap();

        assert!(session
            .offset_scene_entities([valid.clone(), invalid], [10.0, 0.0, 0.0])
            .is_err());
        let value = session
            .scene_entity(&valid)
            .unwrap()
            .components
            .get(&ComponentTypeId::new(TRANSFORM_COMPONENT_ID))
            .unwrap();
        assert_eq!(transform_position(value, &valid).unwrap(), [4.0, 0.0, 0.0]);
    }
}
