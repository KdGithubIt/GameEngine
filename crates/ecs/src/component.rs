use std::any::TypeId;
use std::fmt;

/// Marks a type that may be stored as an ECS component.
///
/// Components must be owned, thread-safe values because archetype storage may
/// outlive the system that inserted them and may later participate in parallel
/// scheduling.
pub trait Component: 'static + Send + Sync {}

impl<T: 'static + Send + Sync> Component for T {}

/// Identifies a runtime component Rust type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(TypeId);

impl ComponentId {
    /// Returns the runtime ID for component type `T`.
    pub fn of<T: Component>() -> Self {
        Self(TypeId::of::<T>())
    }

    pub(crate) fn of_type<T: 'static>() -> Self {
        Self(TypeId::of::<T>())
    }

    /// Returns the underlying Rust [`TypeId`].
    pub fn type_id(&self) -> TypeId {
        self.0
    }
}

impl fmt::Display for ComponentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ComponentId({:?})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Position;
    struct Velocity;

    #[test]
    fn component_ids_are_stable_per_rust_type() {
        let position_id = ComponentId::of::<Position>();
        let velocity_id = ComponentId::of::<Velocity>();

        assert_ne!(position_id, velocity_id);
        assert_eq!(position_id, ComponentId::of::<Position>());
    }
}
