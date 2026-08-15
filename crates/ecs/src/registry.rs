use hashbrown::HashMap;
use std::any::TypeId;

/// Stores minimal runtime metadata for registered Rust types.
///
/// Authoring descriptions, field schemas, serialized validation constraints,
/// and migrations belong to a separate future authoring schema registry.
#[derive(Default)]
pub struct TypeRegistry {
    registrations: HashMap<TypeId, TypeRegistration>,
}

impl TypeRegistry {
    /// Creates an empty runtime type registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers Rust type `T`, replacing any existing registration.
    pub fn register<T: 'static + Send + Sync>(&mut self) {
        self.registrations
            .insert(TypeId::of::<T>(), TypeRegistration::of::<T>());
    }

    /// Returns the registration for `type_id`.
    pub fn get(&self, type_id: TypeId) -> Option<&TypeRegistration> {
        self.registrations.get(&type_id)
    }
}

/// Describes one registered runtime Rust type.
#[derive(Debug, Clone, Copy)]
pub struct TypeRegistration {
    /// The fully qualified Rust type name.
    pub type_name: &'static str,
    /// The runtime Rust type ID.
    pub type_id: TypeId,
}

impl TypeRegistration {
    /// Creates runtime metadata for Rust type `T`.
    pub fn of<T: 'static>() -> Self {
        Self {
            type_name: std::any::type_name::<T>(),
            type_id: TypeId::of::<T>(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Position;

    #[test]
    fn registry_returns_registered_type() {
        let mut registry = TypeRegistry::new();
        registry.register::<Position>();

        let registration = registry.get(TypeId::of::<Position>()).unwrap();
        assert_eq!(registration.type_id, TypeId::of::<Position>());
    }
}
