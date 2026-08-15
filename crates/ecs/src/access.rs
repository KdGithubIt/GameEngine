use crate::component::ComponentId;
use hashbrown::HashMap;
use std::any::TypeId;
use std::fmt;

/// Describes whether a system reads or writes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// Shared read access.
    Read,
    /// Exclusive write access.
    Write,
}

/// Identifies the category of an access conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessTargetKind {
    /// A component stored on entities.
    Component,
    /// A resource stored on the world.
    Resource,
}

/// Reports a conflicting component or resource access declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessError {
    target_kind: AccessTargetKind,
    type_name: &'static str,
    existing: AccessKind,
    requested: AccessKind,
}

impl AccessError {
    fn new(
        target_kind: AccessTargetKind,
        type_name: &'static str,
        existing: AccessKind,
        requested: AccessKind,
    ) -> Self {
        Self {
            target_kind,
            type_name,
            existing,
            requested,
        }
    }

    /// Returns the category of the conflicting value.
    pub fn target_kind(&self) -> AccessTargetKind {
        self.target_kind
    }

    /// Returns the Rust type name of the conflicting value.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// Returns the access that was already registered.
    pub fn existing(&self) -> AccessKind {
        self.existing
    }

    /// Returns the access that caused the conflict.
    pub fn requested(&self) -> AccessKind {
        self.requested
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "conflicting {:?} access for {}: existing {:?}, requested {:?}",
            self.target_kind, self.type_name, self.existing, self.requested
        )
    }
}

impl std::error::Error for AccessError {}

#[derive(Debug, Clone, Copy)]
struct AccessEntry {
    kind: AccessKind,
}

/// Records the component and resource access required by a system.
///
/// The scheduler currently executes systems sequentially, but this metadata is
/// sufficient to determine whether two systems may run in parallel later.
#[derive(Debug, Clone, Default)]
pub struct SystemAccess {
    components: HashMap<ComponentId, AccessEntry>,
    resources: HashMap<TypeId, AccessEntry>,
    exclusive_world: bool,
}

impl SystemAccess {
    /// Creates an empty access declaration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a declaration for a system that receives the complete world.
    ///
    /// Exclusive systems are a deliberate escape hatch for host-owned bridge
    /// code whose data access is selected from validated runtime metadata and
    /// therefore cannot be represented by Rust generic system parameters.
    /// They are never compatible with another system for parallel execution.
    pub(crate) fn exclusive_world() -> Self {
        Self {
            components: HashMap::new(),
            resources: HashMap::new(),
            exclusive_world: true,
        }
    }

    /// Returns whether this declaration requires exclusive world access.
    pub fn is_exclusive_world(&self) -> bool {
        self.exclusive_world
    }

    /// Returns `true` when this declaration can run alongside `other`.
    pub fn is_compatible(&self, other: &Self) -> bool {
        !self.exclusive_world
            && !other.exclusive_world
            && self.components.iter().all(|(id, left)| {
                other
                    .components
                    .get(id)
                    .is_none_or(|right| accesses_are_compatible(left.kind, right.kind))
            })
            && self.resources.iter().all(|(id, left)| {
                other
                    .resources
                    .get(id)
                    .is_none_or(|right| accesses_are_compatible(left.kind, right.kind))
            })
    }

    pub(crate) fn add_component_read<T: 'static>(&mut self) -> Result<(), AccessError> {
        self.add_component(
            ComponentId::of_type::<T>(),
            std::any::type_name::<T>(),
            AccessKind::Read,
        )
    }

    pub(crate) fn add_component_write<T: 'static>(&mut self) -> Result<(), AccessError> {
        self.add_component(
            ComponentId::of_type::<T>(),
            std::any::type_name::<T>(),
            AccessKind::Write,
        )
    }

    pub(crate) fn add_resource_read<T: 'static>(&mut self) -> Result<(), AccessError> {
        self.add_resource(
            TypeId::of::<T>(),
            std::any::type_name::<T>(),
            AccessKind::Read,
        )
    }

    pub(crate) fn add_resource_write<T: 'static>(&mut self) -> Result<(), AccessError> {
        self.add_resource(
            TypeId::of::<T>(),
            std::any::type_name::<T>(),
            AccessKind::Write,
        )
    }

    fn add_component(
        &mut self,
        id: ComponentId,
        type_name: &'static str,
        requested: AccessKind,
    ) -> Result<(), AccessError> {
        add_access(
            &mut self.components,
            id,
            AccessTargetKind::Component,
            type_name,
            requested,
        )
    }

    fn add_resource(
        &mut self,
        id: TypeId,
        type_name: &'static str,
        requested: AccessKind,
    ) -> Result<(), AccessError> {
        add_access(
            &mut self.resources,
            id,
            AccessTargetKind::Resource,
            type_name,
            requested,
        )
    }
}

fn add_access<K>(
    accesses: &mut HashMap<K, AccessEntry>,
    id: K,
    target_kind: AccessTargetKind,
    type_name: &'static str,
    requested: AccessKind,
) -> Result<(), AccessError>
where
    K: Eq + std::hash::Hash,
{
    if let Some(existing) = accesses.get(&id) {
        if !accesses_are_compatible(existing.kind, requested) {
            return Err(AccessError::new(
                target_kind,
                type_name,
                existing.kind,
                requested,
            ));
        }
        return Ok(());
    }

    accesses.insert(id, AccessEntry { kind: requested });
    Ok(())
}

fn accesses_are_compatible(left: AccessKind, right: AccessKind) -> bool {
    left == AccessKind::Read && right == AccessKind::Read
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Position;

    #[test]
    fn repeated_reads_are_compatible() {
        let mut access = SystemAccess::new();
        access.add_component_read::<Position>().unwrap();
        access.add_component_read::<Position>().unwrap();
    }

    #[test]
    fn read_and_write_conflict() {
        let mut access = SystemAccess::new();
        access.add_component_read::<Position>().unwrap();

        let error = access.add_component_write::<Position>().unwrap_err();
        assert_eq!(error.target_kind(), AccessTargetKind::Component);
        assert_eq!(error.existing(), AccessKind::Read);
        assert_eq!(error.requested(), AccessKind::Write);
    }

    #[test]
    fn exclusive_world_access_conflicts_with_every_system() {
        let exclusive = SystemAccess::exclusive_world();
        let ordinary = SystemAccess::new();

        assert!(exclusive.is_exclusive_world());
        assert!(!exclusive.is_compatible(&ordinary));
        assert!(!ordinary.is_compatible(&exclusive));
        assert!(!exclusive.is_compatible(&exclusive));
    }
}
