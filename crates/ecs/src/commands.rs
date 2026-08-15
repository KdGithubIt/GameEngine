use crate::component::Component;
use crate::entity::{Entity, EntityAllocator};
use crate::error::WorldError;
use crate::world::World;
use std::marker::PhantomData;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Applies one deferred structural mutation to a runtime [`World`].
///
/// Runtime commands are intentionally separate from future authoring commands,
/// which require transactions, diffs, validation, and undo support.
pub trait RuntimeCommand: Send {
    /// Applies this command to `world`.
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError>;

    /// Discards this command without applying its requested mutation.
    ///
    /// Most commands have nothing to clean up. Commands that reserve runtime
    /// state before application, such as entity IDs, must release it here.
    fn discard(self: Box<Self>, _: &mut World) -> Result<(), WorldError> {
        Ok(())
    }
}

/// Queues deferred runtime world mutations from a system.
///
/// Commands are applied after all systems in the current schedule have run, so
/// queries remain structurally stable during iteration.
pub struct Commands<'world> {
    sender: Sender<Box<dyn RuntimeCommand>>,
    allocator: Arc<Mutex<EntityAllocator>>,
    marker: PhantomData<&'world World>,
}

impl<'world> Commands<'world> {
    pub(crate) fn new(
        sender: Sender<Box<dyn RuntimeCommand>>,
        allocator: Arc<Mutex<EntityAllocator>>,
    ) -> Self {
        Self {
            sender,
            allocator,
            marker: PhantomData,
        }
    }

    /// Reserves an entity ID and queues creation of an empty entity.
    ///
    /// # Panics
    ///
    /// Panics if entity IDs are exhausted, the allocator was poisoned by a
    /// previous panic, or the command queue is disconnected. Use
    /// [`Commands::try_spawn`] to handle those failures.
    pub fn spawn(&mut self) -> EntityCommands<'world> {
        self.try_spawn()
            .expect("runtime command spawn must be able to reserve and queue an entity")
    }

    /// Tries to reserve an entity ID and queue creation of an empty entity.
    pub fn try_spawn(&mut self) -> Result<EntityCommands<'world>, WorldError> {
        let entity = {
            let mut allocator = self
                .allocator
                .lock()
                .map_err(|_| WorldError::EntityAllocatorUnavailable)?;
            allocator.allocate().ok_or(WorldError::EntityIdExhausted)?
        };

        if self
            .sender
            .send(Box::new(SpawnReservedEntityCommand { entity }))
            .is_err()
        {
            let cleanup = self
                .allocator
                .lock()
                .map_err(|_| WorldError::EntityAllocatorUnavailable)
                .map(|mut allocator| allocator.deallocate(entity));
            return Err(WorldError::with_cleanup(
                WorldError::CommandQueueDisconnected,
                cleanup,
            ));
        }

        Ok(EntityCommands {
            entity,
            sender: self.sender.clone(),
            marker: PhantomData,
        })
    }

    /// Queues removal of `entity`.
    ///
    /// # Panics
    ///
    /// Panics if the command queue is disconnected. Use
    /// [`Commands::try_despawn`] to handle that failure.
    pub fn despawn(&mut self, entity: Entity) {
        self.try_despawn(entity)
            .expect("runtime command queue must exist while Commands is alive");
    }

    /// Tries to queue removal of `entity`.
    pub fn try_despawn(&mut self, entity: Entity) -> Result<(), WorldError> {
        self.sender
            .send(Box::new(DespawnCommand { entity }))
            .map_err(|_| WorldError::CommandQueueDisconnected)
    }

    /// Queues removal of component `T` from `entity`.
    ///
    /// # Panics
    ///
    /// Panics if the command queue is disconnected. Use
    /// [`Commands::try_remove`] to handle that failure.
    pub fn remove<T: Component>(&mut self, entity: Entity) {
        self.try_remove::<T>(entity)
            .expect("runtime command queue must exist while Commands is alive");
    }

    /// Tries to queue removal of component `T` from `entity`.
    pub fn try_remove<T: Component>(&mut self, entity: Entity) -> Result<(), WorldError> {
        self.sender
            .send(Box::new(RemoveComponentCommand::<T> {
                entity,
                marker: PhantomData,
            }))
            .map_err(|_| WorldError::CommandQueueDisconnected)
    }
}

/// Queues deferred commands for one reserved runtime entity.
pub struct EntityCommands<'world> {
    entity: Entity,
    sender: Sender<Box<dyn RuntimeCommand>>,
    marker: PhantomData<&'world World>,
}

impl EntityCommands<'_> {
    /// Queues insertion of `component` on this entity.
    ///
    /// # Panics
    ///
    /// Panics if the command queue is disconnected. Use
    /// [`EntityCommands::try_insert`] to handle that failure.
    pub fn insert<T: Component>(&mut self, component: T) -> &mut Self {
        self.try_insert(component)
            .expect("runtime command queue must exist while EntityCommands is alive")
    }

    /// Tries to queue insertion of `component` on this entity.
    pub fn try_insert<T: Component>(&mut self, component: T) -> Result<&mut Self, WorldError> {
        self.sender
            .send(Box::new(InsertComponentCommand {
                entity: self.entity,
                component,
            }))
            .map_err(|_| WorldError::CommandQueueDisconnected)?;
        Ok(self)
    }

    /// Returns the reserved runtime entity ID.
    pub fn id(&self) -> Entity {
        self.entity
    }
}

struct SpawnReservedEntityCommand {
    entity: Entity,
}

impl RuntimeCommand for SpawnReservedEntityCommand {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError> {
        world.spawn_reserved(self.entity)
    }

    fn discard(self: Box<Self>, world: &mut World) -> Result<(), WorldError> {
        world.release_reserved_entity(self.entity)
    }
}

struct DespawnCommand {
    entity: Entity,
}

impl RuntimeCommand for DespawnCommand {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError> {
        world.despawn(self.entity)
    }
}

struct InsertComponentCommand<T> {
    entity: Entity,
    component: T,
}

struct RemoveComponentCommand<T> {
    entity: Entity,
    marker: PhantomData<fn() -> T>,
}

impl<T: Component> RuntimeCommand for InsertComponentCommand<T> {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError> {
        world.add_component(self.entity, self.component)
    }
}

impl<T: Component> RuntimeCommand for RemoveComponentCommand<T> {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError> {
        world.remove_component::<T>(self.entity).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn disconnected_queue_releases_reserved_entity_id() {
        let (sender, receiver) = channel();
        drop(receiver);
        let allocator = Arc::new(Mutex::new(EntityAllocator::new()));
        let mut commands = Commands::new(sender, Arc::clone(&allocator));

        let error = match commands.try_spawn() {
            Ok(_) => panic!("disconnected command queue must reject spawn"),
            Err(error) => error,
        };
        assert_eq!(error, WorldError::CommandQueueDisconnected);

        let entity = allocator.lock().unwrap().allocate().unwrap();
        assert_eq!(entity.id(), 1);
        assert_eq!(entity.generation(), 1);
    }

    #[test]
    fn remove_command_removes_component_after_application() {
        let mut world = World::new();
        let entity = world.spawn_with(42_i32).unwrap();
        let mut commands = Commands::new(
            world.command_sender.clone(),
            Arc::clone(&world.entity_allocator),
        );

        commands.remove::<i32>(entity);
        world.apply_commands().unwrap();

        assert!(!world.has_component::<i32>(entity));
    }
}
