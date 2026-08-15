//! Per-entity parameters used by `#[game_system(each)]` callbacks.
//!
//! The project module still receives copied, query-scoped values. These types
//! only make the user-facing function signature concise; they never expose the
//! host ECS world or live component references across the module boundary.

use crate::game_api::LocalTransformView;
use crate::game_io::GameEntityHandle;
use crate::game_contracts::GameComponent;
use engine_authoring::value::Value;
use glam::{Quat, Vec3};
use std::collections::BTreeMap;
use std::marker::PhantomData;

/// Generation-checked runtime entity handle passed to one `each` callback.
pub type Entity = GameEntityHandle;

/// Editable local transform copy passed to an `each` callback.
///
/// A mutable parameter is converted into one validated `set_transform` command
/// after the callback. It is not a direct ECS reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Local-space translation.
    pub translation: Vec3,
    /// Local-space rotation.
    pub rotation: Quat,
    /// Local-space scale.
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl From<LocalTransformView> for Transform {
    fn from(view: LocalTransformView) -> Self {
        Self {
            translation: view.translation,
            rotation: view.rotation,
            scale: view.scale,
        }
    }
}

/// Requires component `T` without passing its decoded value to gameplay code.
#[derive(Debug, Clone, Copy, Default)]
pub struct With<T>(PhantomData<fn() -> T>);

impl<T> With<T> {
    /// Creates the zero-sized marker used by generated `each` adapters.
    #[doc(hidden)]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

/// Excludes entities containing component `T`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Without<T>(PhantomData<fn() -> T>);

impl<T> Without<T> {
    /// Creates the zero-sized marker used by generated `each` adapters.
    #[doc(hidden)]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

/// Values for an OR-filter declared as `AnyOf<(&A, &B, ...)>`.
///
/// The callback runs only when at least one listed component exists. Use
/// [`has`](Self::has) for branching and [`get`](Self::get) when the component
/// value itself is needed.
#[derive(Debug, Clone)]
pub struct AnyOf<T> {
    values: BTreeMap<&'static str, Value>,
    marker: PhantomData<fn() -> T>,
}

impl<T> Default for AnyOf<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AnyOf<T> {
    /// Creates an empty value used by generated `each` adapters.
    #[doc(hidden)]
    pub fn new() -> Self {
        Self {
            values: BTreeMap::new(),
            marker: PhantomData,
        }
    }

    /// Inserts one present component into the generated OR-filter value.
    #[doc(hidden)]
    pub fn insert<C: GameComponent>(&mut self, component: C) {
        self.values
            .insert(C::TYPE_ID, component.to_authoring_value());
    }

    /// Returns whether component `C` was present on this entity.
    pub fn has<C: GameComponent>(&self) -> bool {
        self.values.contains_key(C::TYPE_ID)
    }

    /// Decodes component `C` when it was present on this entity.
    ///
    /// The returned component is another owned copy, matching the project
    /// module's copy-and-patch execution model.
    pub fn get<C: GameComponent>(&self) -> Result<Option<C>, String> {
        self.values
            .get(C::TYPE_ID)
            .map(C::from_authoring_value)
            .transpose()
    }

    /// Returns whether no declared OR component was present.
    #[doc(hidden)]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}
