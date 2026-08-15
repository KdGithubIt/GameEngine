use std::fmt;
use std::num::NonZeroU32;

/// Identifies a runtime entity by numeric ID and generation.
///
/// A generation prevents a stale handle from referring to a newer entity that
/// reused the same numeric ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entity {
    id: NonZeroU32,
    generation: u32,
}

impl Entity {
    pub(crate) fn new(id: u32, generation: u32) -> Self {
        Self {
            id: NonZeroU32::new(id).expect("entity allocator must never produce ID zero"),
            generation,
        }
    }

    /// Returns the numeric portion of this runtime entity ID.
    pub fn id(&self) -> u32 {
        self.id.get()
    }

    /// Returns the generation used to reject stale handles.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Reconstructs a runtime entity handle from its numeric parts.
    ///
    /// This does not prove that the entity is live. Every [`World`](crate::World)
    /// operation still validates the ID and generation before accessing data.
    /// The API exists for the versioned native game-module boundary, which may
    /// not pass Rust's layout-dependent `Entity` value through a C ABI.
    pub fn from_raw(id: u32, generation: u32) -> Self {
        Self::new(id, generation)
    }
}

impl fmt::Display for Entity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}v{}", self.id(), self.generation())
    }
}

#[derive(Debug)]
pub(crate) struct EntityAllocator {
    next_id: u64,
    free_list: Vec<(u32, u32)>,
}

impl EntityAllocator {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            free_list: Vec::new(),
        }
    }

    pub(crate) fn allocate(&mut self) -> Option<Entity> {
        if let Some((id, generation)) = self.free_list.pop() {
            return Some(Entity::new(id, generation));
        }

        if self.next_id > u32::MAX as u64 {
            return None;
        }

        let id = self.next_id as u32;
        self.next_id += 1;
        Some(Entity::new(id, 0))
    }

    pub(crate) fn deallocate(&mut self, entity: Entity) {
        if let Some(next_generation) = entity.generation().checked_add(1) {
            self.free_list.push((entity.id(), next_generation));
        }
    }
}

impl Default for EntityAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_returns_unique_entities() {
        let mut allocator = EntityAllocator::new();
        let first = allocator.allocate().unwrap();
        let second = allocator.allocate().unwrap();

        assert_ne!(first, second);
        assert_eq!(first.id(), 1);
        assert_eq!(second.id(), 2);
    }

    #[test]
    fn reused_entity_id_has_a_new_generation() {
        let mut allocator = EntityAllocator::new();
        let first = allocator.allocate().unwrap();
        allocator.deallocate(first);

        let second = allocator.allocate().unwrap();
        assert_eq!(first.id(), second.id());
        assert_ne!(first.generation(), second.generation());
    }
}
