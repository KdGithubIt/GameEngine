use crate::commands::RuntimeCommand;
use crate::component::Component;
use crate::entity::{Entity, EntityAllocator};
use crate::error::{QueryError, WorldError};
use crate::query::{Query, QueryData, QueryFilter};
use crate::storage::Storage;
use hashbrown::HashMap;
use std::any::{Any, TypeId};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Owns all runtime entities, components, resources, and deferred commands.
///
/// `World` is the only public owner of structural ECS mutation. Runtime entity
/// IDs are ephemeral and must not be persisted as authoring identifiers.
pub struct World {
    pub(crate) entity_allocator: Arc<Mutex<EntityAllocator>>,
    pub(crate) storage: Storage,
    resources: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    pub(crate) command_sender: Sender<Box<dyn RuntimeCommand>>,
    command_receiver: Mutex<Receiver<Box<dyn RuntimeCommand>>>,
}

impl World {
    /// Creates an empty runtime world.
    pub fn new() -> Self {
        let (command_sender, command_receiver) = channel();
        Self {
            entity_allocator: Arc::new(Mutex::new(EntityAllocator::new())),
            storage: Storage::new(),
            resources: HashMap::new(),
            command_sender,
            command_receiver: Mutex::new(command_receiver),
        }
    }

    /// Creates an entity without components.
    ///
    /// # Errors
    ///
    /// Returns an error if runtime entity IDs are exhausted or the allocator is
    /// unavailable after a panic.
    pub fn spawn(&mut self) -> Result<Entity, WorldError> {
        let entity = self.allocate_entity()?;
        if let Err(error) = self.storage.spawn_empty(entity) {
            return Err(self.failed_spawn(entity, error));
        }
        Ok(entity)
    }

    /// Creates an entity containing `component`.
    ///
    /// # Errors
    ///
    /// Returns an error if runtime entity IDs are exhausted, the allocator is
    /// unavailable, or an internal storage invariant is broken.
    pub fn spawn_with<T: Component>(&mut self, component: T) -> Result<Entity, WorldError> {
        let entity = self.allocate_entity()?;
        if let Err(error) = self.storage.spawn_with(entity, component) {
            return Err(self.failed_spawn(entity, error));
        }
        Ok(entity)
    }

    /// Removes `entity` and all of its components.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::EntityNotFound`] or [`WorldError::StaleEntity`]
    /// when the handle is not current.
    pub fn despawn(&mut self, entity: Entity) -> Result<(), WorldError> {
        self.storage.despawn(entity)?;
        self.release_reserved_entity(entity)
    }

    /// Adds `component` to `entity`, moving it to a matching archetype.
    ///
    /// # Errors
    ///
    /// Returns an error if the entity is missing, stale, or already has the
    /// requested component type.
    pub fn add_component<T: Component>(
        &mut self,
        entity: Entity,
        component: T,
    ) -> Result<(), WorldError> {
        self.storage.add_component(entity, component)
    }

    /// Removes and returns component `T` from `entity`.
    ///
    /// # Errors
    ///
    /// Returns an error if the entity is missing, stale, or does not have the
    /// requested component type.
    pub fn remove_component<T: Component>(&mut self, entity: Entity) -> Result<T, WorldError> {
        self.storage.remove_component::<T>(entity)
    }

    /// Returns a shared reference to component `T` on `entity`.
    ///
    /// Returns `None` when the entity is missing, stale, or does not have the
    /// requested component.
    pub fn get_component<T: Component>(&self, entity: Entity) -> Option<&T> {
        self.storage.get_component::<T>(entity)
    }

    /// Returns an exclusive reference to component `T` on `entity`.
    ///
    /// Returns `None` when the entity is missing, stale, or does not have the
    /// requested component.
    pub fn get_component_mut<T: Component>(&mut self, entity: Entity) -> Option<&mut T> {
        self.storage.get_component_mut::<T>(entity)
    }

    /// Returns `true` when `entity` is live and contains component `T`.
    pub fn has_component<T: Component>(&self, entity: Entity) -> bool {
        self.storage.has_component::<T>(entity)
    }

    /// Returns `true` when `entity` is a live handle in this world.
    pub fn contains_entity(&self, entity: Entity) -> bool {
        self.storage.contains_entity(entity)
    }

    /// Returns the number of live entities in this world.
    pub fn entity_count(&self) -> usize {
        self.storage.entity_count()
    }

    /// Iterates over all live runtime entity IDs.
    ///
    /// The iteration order is not stable and must not be persisted or used as
    /// deterministic authoring order.
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.storage.entities()
    }

    /// Creates a direct query that exclusively borrows this world.
    pub fn query<Q: QueryData>(&mut self) -> Result<Query<'_, Q>, QueryError> {
        Query::try_new(self)
    }

    /// Creates a filtered direct query that exclusively borrows this world.
    pub fn query_filtered<Q: QueryData, F: QueryFilter>(
        &mut self,
    ) -> Result<Query<'_, Q, F>, QueryError> {
        Query::try_new(self)
    }

    /// Inserts or replaces a resource of type `T`.
    pub fn insert_resource<T: 'static + Send + Sync>(&mut self, resource: T) {
        self.resources.insert(TypeId::of::<T>(), Box::new(resource));
    }

    /// Returns a shared reference to resource `T`.
    pub fn get_resource<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.resources
            .get(&TypeId::of::<T>())
            .and_then(|resource| resource.downcast_ref::<T>())
    }

    /// Returns an exclusive reference to resource `T`.
    pub fn get_resource_mut<T: 'static + Send + Sync>(&mut self) -> Option<&mut T> {
        self.resources
            .get_mut(&TypeId::of::<T>())
            .and_then(|resource| resource.downcast_mut::<T>())
    }

    /// Removes and returns resource `T` when it exists.
    pub fn remove_resource<T: 'static + Send + Sync>(&mut self) -> Option<T> {
        self.resources
            .remove(&TypeId::of::<T>())
            .and_then(|resource| resource.downcast::<T>().ok())
            .map(|resource| *resource)
    }

    /// Applies every currently queued deferred runtime command.
    ///
    /// Commands are applied in queue order. Independent commands continue to
    /// run after a failure, and all failures are returned together.
    pub fn apply_commands(&mut self) -> Result<(), Vec<WorldError>> {
        let commands = match self.drain_commands() {
            Ok(commands) => commands,
            Err(error) => return Err(vec![error]),
        };

        let mut errors = Vec::new();
        for command in commands {
            if let Err(error) = command.apply(self) {
                errors.push(error);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Validates internal archetype and entity metadata invariants.
    ///
    /// This is primarily useful in tests and diagnostics.
    pub fn validate(&self) -> Result<(), WorldError> {
        self.storage.validate()
    }

    pub(crate) fn spawn_reserved(&mut self, entity: Entity) -> Result<(), WorldError> {
        if let Err(error) = self.storage.spawn_empty(entity) {
            return Err(self.failed_spawn(entity, error));
        }
        Ok(())
    }

    pub(crate) fn release_reserved_entity(&mut self, entity: Entity) -> Result<(), WorldError> {
        let mut allocator = self
            .entity_allocator
            .lock()
            .map_err(|_| WorldError::EntityAllocatorUnavailable)?;
        allocator.deallocate(entity);
        Ok(())
    }

    pub(crate) fn discard_commands(&mut self) -> Result<(), Vec<WorldError>> {
        let commands = match self.drain_commands() {
            Ok(commands) => commands,
            Err(error) => return Err(vec![error]),
        };

        let mut errors = Vec::new();
        for command in commands {
            if let Err(error) = command.discard(self) {
                errors.push(error);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn allocate_entity(&self) -> Result<Entity, WorldError> {
        let mut allocator = self
            .entity_allocator
            .lock()
            .map_err(|_| WorldError::EntityAllocatorUnavailable)?;
        allocator.allocate().ok_or(WorldError::EntityIdExhausted)
    }

    fn failed_spawn(&mut self, entity: Entity, error: WorldError) -> WorldError {
        if matches!(error, WorldError::EntityIdAlreadyInUse(_)) {
            return error;
        }
        WorldError::with_cleanup(error, self.release_reserved_entity(entity))
    }

    fn drain_commands(&self) -> Result<Vec<Box<dyn RuntimeCommand>>, WorldError> {
        let receiver = self
            .command_receiver
            .lock()
            .map_err(|_| WorldError::CommandQueueUnavailable)?;
        Ok(receiver.try_iter().collect())
    }
}

impl Default for World {
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

    #[test]
    fn spawn_and_despawn_update_entity_liveness() {
        let mut world = World::new();
        let entity = world.spawn().unwrap();
        assert!(world.contains_entity(entity));

        world.despawn(entity).unwrap();
        assert!(!world.contains_entity(entity));
    }

    #[test]
    fn stale_entity_cannot_reach_reused_entity() {
        let mut world = World::new();
        let stale = world.spawn_with(Position { x: 1.0, y: 2.0 }).unwrap();
        world.despawn(stale).unwrap();
        let current = world.spawn_with(Position { x: 3.0, y: 4.0 }).unwrap();

        assert_eq!(stale.id(), current.id());
        assert!(world.get_component::<Position>(stale).is_none());
        assert_eq!(
            world.get_component::<Position>(current),
            Some(&Position { x: 3.0, y: 4.0 })
        );
    }

    #[test]
    fn resources_can_be_inserted_and_removed() {
        let mut world = World::new();
        world.insert_resource(42_i32);

        assert_eq!(world.get_resource::<i32>(), Some(&42));
        assert_eq!(world.remove_resource::<i32>(), Some(42));
        assert!(world.get_resource::<i32>().is_none());
    }

    #[test]
    fn duplicate_allocated_id_is_not_returned_to_free_list() {
        let mut world = World::new();
        let occupied = Entity::new(1, 0);
        world.storage.spawn_empty(occupied).unwrap();

        let error = world.spawn().unwrap_err();
        assert_eq!(error, WorldError::EntityIdAlreadyInUse(occupied));

        let next = world.spawn().unwrap();
        assert_eq!(next.id(), 2);
        world.validate().unwrap();
    }

    #[test]
    fn removing_component_returns_its_value() {
        let mut world = World::new();
        let entity = world.spawn_with(Position { x: 1.0, y: 2.0 }).unwrap();

        let position = world.remove_component::<Position>(entity).unwrap();

        assert_eq!(position, Position { x: 1.0, y: 2.0 });
        assert!(!world.has_component::<Position>(entity));
        world.validate().unwrap();
    }
}
