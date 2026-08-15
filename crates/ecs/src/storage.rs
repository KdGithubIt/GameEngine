use crate::archetype::{Archetype, ArchetypeId};
use crate::component::{Component, ComponentId};
use crate::entity::Entity;
use crate::error::WorldError;
use hashbrown::HashMap;

#[derive(Debug, Clone, Copy)]
pub(crate) struct EntityMeta {
    archetype_id: ArchetypeId,
    index: usize,
    generation: u32,
}

pub(crate) struct Storage {
    archetypes: Vec<Archetype>,
    archetype_map: HashMap<Vec<ComponentId>, ArchetypeId>,
    entity_meta: HashMap<u32, EntityMeta>,
}

impl Storage {
    pub(crate) fn new() -> Self {
        let mut storage = Self {
            archetypes: Vec::new(),
            archetype_map: HashMap::new(),
            entity_meta: HashMap::new(),
        };
        storage.get_or_create_archetype(Vec::new());
        storage
    }

    pub(crate) fn spawn_empty(&mut self, entity: Entity) -> Result<(), WorldError> {
        self.ensure_id_is_unused(entity)?;
        let archetype_id = self.get_or_create_archetype(Vec::new());
        let index = self
            .archetype_mut(archetype_id)?
            .push_row(entity, Vec::new())?;
        self.insert_meta(entity, archetype_id, index);
        Ok(())
    }

    pub(crate) fn spawn_with<T: Component>(
        &mut self,
        entity: Entity,
        component: T,
    ) -> Result<(), WorldError> {
        self.ensure_id_is_unused(entity)?;
        let component_id = ComponentId::of::<T>();
        let archetype_id = self.get_or_create_archetype(vec![component_id]);
        let archetype = self.archetype_mut(archetype_id)?;
        archetype.initialize_component::<T>();
        let index = archetype.push_row(entity, vec![(component_id, Box::new(component))])?;
        self.insert_meta(entity, archetype_id, index);
        Ok(())
    }

    pub(crate) fn despawn(&mut self, entity: Entity) -> Result<(), WorldError> {
        let meta = self.entity_meta(entity)?;
        let (moved_entity, _) = self
            .archetype_mut(meta.archetype_id)?
            .remove_row(meta.index)?;
        self.entity_meta.remove(&entity.id());
        self.update_moved_entity(moved_entity, meta.index)?;
        Ok(())
    }

    pub(crate) fn add_component<T: Component>(
        &mut self,
        entity: Entity,
        component: T,
    ) -> Result<(), WorldError> {
        let meta = self.entity_meta(entity)?;
        let component_id = ComponentId::of::<T>();
        let old_component_ids = self.archetype(meta.archetype_id)?.component_ids().to_vec();

        if old_component_ids.binary_search(&component_id).is_ok() {
            return Err(WorldError::ComponentAlreadyExists {
                entity,
                component: component_id,
            });
        }

        let mut new_component_ids = old_component_ids;
        new_component_ids.push(component_id);
        let new_archetype_id = self.get_or_create_archetype(new_component_ids);

        {
            let (old_archetype, new_archetype) =
                self.two_archetypes_mut(meta.archetype_id, new_archetype_id)?;
            new_archetype.copy_empty_columns_from(old_archetype);
            new_archetype.initialize_component::<T>();
            new_archetype.validate()?;
        }

        let (moved_entity, mut row) = self
            .archetype_mut(meta.archetype_id)?
            .remove_row(meta.index)?;
        self.update_moved_entity(moved_entity, meta.index)?;
        row.push((component_id, Box::new(component)));

        let new_index = self
            .archetype_mut(new_archetype_id)?
            .push_row(entity, row)?;
        self.insert_meta(entity, new_archetype_id, new_index);
        Ok(())
    }

    pub(crate) fn remove_component<T: Component>(
        &mut self,
        entity: Entity,
    ) -> Result<T, WorldError> {
        let meta = self.entity_meta(entity)?;
        let component_id = ComponentId::of::<T>();
        let mut new_component_ids = self.archetype(meta.archetype_id)?.component_ids().to_vec();
        let component_index = new_component_ids
            .binary_search(&component_id)
            .map_err(|_| WorldError::ComponentNotFound {
                entity,
                component: component_id,
            })?;
        new_component_ids.remove(component_index);
        let new_archetype_id = self.get_or_create_archetype(new_component_ids);

        {
            let (old_archetype, new_archetype) =
                self.two_archetypes_mut(meta.archetype_id, new_archetype_id)?;
            new_archetype.copy_empty_columns_from(old_archetype);
            new_archetype.validate()?;
        }

        let (moved_entity, mut row) = self
            .archetype_mut(meta.archetype_id)?
            .remove_row(meta.index)?;
        self.update_moved_entity(moved_entity, meta.index)?;
        let removed_index = row
            .binary_search_by_key(&component_id, |(id, _)| *id)
            .expect("validated archetype row must contain the removed component");
        let (_, removed) = row.remove(removed_index);
        let removed = removed
            .downcast::<T>()
            .expect("component column type must match its component ID");

        let new_index = self
            .archetype_mut(new_archetype_id)?
            .push_row(entity, row)?;
        self.insert_meta(entity, new_archetype_id, new_index);
        Ok(*removed)
    }

    pub(crate) fn get_component<T: Component>(&self, entity: Entity) -> Option<&T> {
        let meta = self.entity_meta(entity).ok()?;
        self.archetype(meta.archetype_id)
            .ok()?
            .component_slice::<T>()?
            .get(meta.index)
    }

    pub(crate) fn get_component_mut<T: Component>(&mut self, entity: Entity) -> Option<&mut T> {
        let meta = self.entity_meta(entity).ok()?;
        self.archetype_mut(meta.archetype_id)
            .ok()?
            .component_slice_mut::<T>()?
            .get_mut(meta.index)
    }

    pub(crate) fn has_component<T: Component>(&self, entity: Entity) -> bool {
        self.entity_meta(entity)
            .ok()
            .and_then(|meta| self.archetype(meta.archetype_id).ok())
            .is_some_and(Archetype::has_component::<T>)
    }

    pub(crate) fn contains_entity(&self, entity: Entity) -> bool {
        self.entity_meta(entity).is_ok()
    }

    pub(crate) fn entity_count(&self) -> usize {
        self.entity_meta.len()
    }

    pub(crate) fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.entity_meta
            .iter()
            .map(|(&id, meta)| Entity::from_raw(id, meta.generation))
    }

    pub(crate) fn archetypes(&self) -> &[Archetype] {
        &self.archetypes
    }

    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        for archetype in &self.archetypes {
            archetype.validate()?;
            for (index, entity) in archetype.entities().iter().copied().enumerate() {
                let meta = self.entity_meta(entity)?;
                if meta.archetype_id != archetype.id() || meta.index != index {
                    return Err(WorldError::InternalInvariant(
                        "entity metadata does not point to its archetype row",
                    ));
                }
            }
        }
        Ok(())
    }

    fn get_or_create_archetype(&mut self, mut component_ids: Vec<ComponentId>) -> ArchetypeId {
        component_ids.sort_unstable();
        component_ids.dedup();

        if let Some(&id) = self.archetype_map.get(&component_ids) {
            return id;
        }

        let id = ArchetypeId::new(self.archetypes.len());
        self.archetypes
            .push(Archetype::new(id, component_ids.clone()));
        self.archetype_map.insert(component_ids, id);
        id
    }

    fn entity_meta(&self, entity: Entity) -> Result<EntityMeta, WorldError> {
        let meta = self
            .entity_meta
            .get(&entity.id())
            .copied()
            .ok_or(WorldError::EntityNotFound(entity))?;
        if meta.generation != entity.generation() {
            return Err(WorldError::StaleEntity(entity));
        }
        Ok(meta)
    }

    fn ensure_id_is_unused(&self, entity: Entity) -> Result<(), WorldError> {
        if self.entity_meta.contains_key(&entity.id()) {
            return Err(WorldError::EntityIdAlreadyInUse(entity));
        }
        Ok(())
    }

    fn insert_meta(&mut self, entity: Entity, archetype_id: ArchetypeId, index: usize) {
        self.entity_meta.insert(
            entity.id(),
            EntityMeta {
                archetype_id,
                index,
                generation: entity.generation(),
            },
        );
    }

    fn update_moved_entity(
        &mut self,
        moved_entity: Option<Entity>,
        new_index: usize,
    ) -> Result<(), WorldError> {
        if let Some(moved_entity) = moved_entity {
            let meta = self.entity_meta.get_mut(&moved_entity.id()).ok_or(
                WorldError::InternalInvariant("moved entity is missing metadata"),
            )?;
            meta.index = new_index;
        }
        Ok(())
    }

    fn archetype(&self, id: ArchetypeId) -> Result<&Archetype, WorldError> {
        self.archetypes
            .get(id.index())
            .ok_or(WorldError::InternalInvariant(
                "entity metadata references a missing archetype",
            ))
    }

    fn archetype_mut(&mut self, id: ArchetypeId) -> Result<&mut Archetype, WorldError> {
        self.archetypes
            .get_mut(id.index())
            .ok_or(WorldError::InternalInvariant(
                "entity metadata references a missing archetype",
            ))
    }

    fn two_archetypes_mut(
        &mut self,
        first: ArchetypeId,
        second: ArchetypeId,
    ) -> Result<(&mut Archetype, &mut Archetype), WorldError> {
        if first == second {
            return Err(WorldError::InternalInvariant(
                "component transition source and destination archetypes are identical",
            ));
        }

        let first_index = first.index();
        let second_index = second.index();
        if first_index < second_index {
            let (left, right) = self.archetypes.split_at_mut(second_index);
            let first = left
                .get_mut(first_index)
                .ok_or(WorldError::InternalInvariant("source archetype is missing"))?;
            let second = right.first_mut().ok_or(WorldError::InternalInvariant(
                "destination archetype is missing",
            ))?;
            Ok((first, second))
        } else {
            let (left, right) = self.archetypes.split_at_mut(first_index);
            let second = left
                .get_mut(second_index)
                .ok_or(WorldError::InternalInvariant(
                    "destination archetype is missing",
                ))?;
            let first = right
                .first_mut()
                .ok_or(WorldError::InternalInvariant("source archetype is missing"))?;
            Ok((first, second))
        }
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Position {
        x: f32,
        y: f32,
    }

    struct Velocity;

    #[test]
    fn stale_entity_cannot_access_reused_id() {
        let mut storage = Storage::new();
        let old_entity = Entity::new(1, 0);
        storage
            .spawn_with(old_entity, Position { x: 1.0, y: 2.0 })
            .unwrap();
        storage.despawn(old_entity).unwrap();

        let new_entity = Entity::new(1, 1);
        storage
            .spawn_with(new_entity, Position { x: 3.0, y: 4.0 })
            .unwrap();

        assert!(storage.get_component::<Position>(old_entity).is_none());
        assert!(storage.get_component::<Position>(new_entity).is_some());
    }

    #[test]
    fn adding_component_moves_entity_without_breaking_metadata() {
        let mut storage = Storage::new();
        let entity = Entity::new(1, 0);
        storage
            .spawn_with(entity, Position { x: 1.0, y: 2.0 })
            .unwrap();

        storage.add_component(entity, Velocity).unwrap();

        assert!(storage.has_component::<Position>(entity));
        assert!(storage.has_component::<Velocity>(entity));
        storage.validate().unwrap();
    }

    #[test]
    fn removing_component_moves_entity_without_breaking_metadata() {
        let mut storage = Storage::new();
        let entity = Entity::new(1, 0);
        storage
            .spawn_with(entity, Position { x: 1.0, y: 2.0 })
            .unwrap();
        storage.add_component(entity, Velocity).unwrap();

        storage.remove_component::<Velocity>(entity).unwrap();

        assert!(storage.has_component::<Position>(entity));
        assert!(!storage.has_component::<Velocity>(entity));
        storage.validate().unwrap();
    }
}
