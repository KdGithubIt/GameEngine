use crate::commands::RuntimeCommand;
use crate::entity::EntityAllocator;
use crate::storage::Storage;
use crate::world::World;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Provides internal raw access to a world after system access validation.
///
/// This type is intentionally crate-private. Safe public APIs must never expose
/// it or allow callers to bypass access validation.
#[derive(Clone, Copy)]
pub(crate) struct UnsafeWorldCell<'world> {
    world: NonNull<World>,
    marker: PhantomData<&'world mut World>,
}

impl<'world> UnsafeWorldCell<'world> {
    pub(crate) fn new(world: &'world mut World) -> Self {
        Self {
            world: NonNull::from(world),
            marker: PhantomData,
        }
    }

    pub(crate) fn storage_ptr(self) -> *const Storage {
        // SAFETY: The pointer was created from a live exclusive world borrow.
        // Callers still need validated component access before dereferencing
        // any component column reached through this pointer.
        unsafe { std::ptr::addr_of!((*self.world.as_ptr()).storage) }
    }

    pub(crate) unsafe fn resource<T: 'static + Send + Sync>(self) -> Option<&'world T> {
        // SAFETY: The caller guarantees shared access to resource T was
        // registered and no mutable reference to the same resource exists.
        unsafe { (&*self.world.as_ptr()).get_resource::<T>() }
    }

    pub(crate) unsafe fn resource_mut<T: 'static + Send + Sync>(self) -> Option<&'world mut T> {
        // SAFETY: The caller guarantees exclusive access to resource T was
        // registered and no other reference to the same resource exists.
        unsafe { (&mut *self.world.as_ptr()).get_resource_mut::<T>() }
    }

    pub(crate) unsafe fn command_sender(self) -> Sender<Box<dyn RuntimeCommand>> {
        // SAFETY: The command sender field is disjoint from component storage
        // and resources, and cloning it does not mutate the world.
        unsafe { (&*std::ptr::addr_of!((*self.world.as_ptr()).command_sender)).clone() }
    }

    pub(crate) unsafe fn entity_allocator(self) -> Arc<Mutex<EntityAllocator>> {
        // SAFETY: The entity allocator field is disjoint from component
        // storage and resources, and cloning the Arc does not mutate the world.
        unsafe { (&*std::ptr::addr_of!((*self.world.as_ptr()).entity_allocator)).clone() }
    }
}
