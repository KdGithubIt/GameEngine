use crate::component::{Component, ComponentId};
use crate::entity::Entity;
use crate::error::WorldError;
use hashbrown::HashMap;
use std::any::Any;
use std::cell::UnsafeCell;

pub(crate) type ErasedComponent = Box<dyn Any + Send + Sync>;
pub(crate) type ComponentRow = Vec<(ComponentId, ErasedComponent)>;

/// Identifies an archetype inside one runtime world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ArchetypeId(usize);

impl ArchetypeId {
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

type SwapRemoveFn = fn(&mut ErasedComponent, usize) -> ErasedComponent;
type PushErasedFn = fn(&mut ErasedComponent, ErasedComponent);
type CloneEmptyFn = fn() -> ErasedComponent;
type LenFn = fn(&ErasedComponent) -> usize;

struct ComponentColumn {
    component_id: ComponentId,
    values: UnsafeCell<ErasedComponent>,
    swap_remove: SwapRemoveFn,
    push_erased: PushErasedFn,
    clone_empty: CloneEmptyFn,
    len: LenFn,
}

// SAFETY: Component values are Send + Sync, safe shared methods only read
// values, and all mutation through shared references is restricted to the
// validated query access path.
unsafe impl Sync for ComponentColumn {}

impl ComponentColumn {
    fn new<T: Component>() -> Self {
        Self {
            component_id: ComponentId::of::<T>(),
            values: UnsafeCell::new(Box::new(Vec::<T>::new())),
            swap_remove: swap_remove_impl::<T>,
            push_erased: push_erased_impl::<T>,
            clone_empty: clone_empty_impl::<T>,
            len: len_impl::<T>,
        }
    }

    fn clone_empty(&self) -> Self {
        Self {
            component_id: self.component_id,
            values: UnsafeCell::new((self.clone_empty)()),
            swap_remove: self.swap_remove,
            push_erased: self.push_erased,
            clone_empty: self.clone_empty,
            len: self.len,
        }
    }

    fn len(&self) -> usize {
        // SAFETY: Safe shared column methods never mutate values, and
        // structural mutation requires an exclusive archetype reference.
        unsafe { (self.len)(&*self.values.get()) }
    }

    fn accepts(&self, value: &ErasedComponent) -> bool {
        value.as_ref().type_id() == self.component_id.type_id()
    }

    fn swap_remove(&mut self, index: usize) -> ErasedComponent {
        (self.swap_remove)(self.values.get_mut(), index)
    }

    fn push_erased(&mut self, value: ErasedComponent) {
        (self.push_erased)(self.values.get_mut(), value);
    }

    fn slice<T: Component>(&self) -> Option<&[T]> {
        // SAFETY: Safe shared column methods never mutate values. Query
        // mutation is validated to exclude shared access to the same column.
        unsafe { (&*self.values.get()).downcast_ref::<Vec<T>>() }.map(Vec::as_slice)
    }

    fn slice_mut<T: Component>(&mut self) -> Option<&mut [T]> {
        self.values
            .get_mut()
            .downcast_mut::<Vec<T>>()
            .map(Vec::as_mut_slice)
    }

    /// Returns a mutable value pointer through a shared column reference.
    ///
    /// # Safety
    ///
    /// The caller must have exclusive logical access to component `T` for the
    /// returned pointer lifetime.
    unsafe fn mut_ptr_unchecked<T: Component>(&self) -> Option<(*mut T, usize)> {
        // SAFETY: The caller guarantees exclusive logical access to this
        // component column.
        let values = unsafe { (&mut *self.values.get()).downcast_mut::<Vec<T>>() }?;
        Some((values.as_mut_ptr(), values.len()))
    }
}

fn swap_remove_impl<T: Component>(values: &mut ErasedComponent, index: usize) -> ErasedComponent {
    let values = values
        .downcast_mut::<Vec<T>>()
        .expect("component column type must match its component ID");
    Box::new(values.swap_remove(index))
}

fn push_erased_impl<T: Component>(values: &mut ErasedComponent, value: ErasedComponent) {
    let values = values
        .downcast_mut::<Vec<T>>()
        .expect("component column type must match its component ID");
    let value = value
        .downcast::<T>()
        .expect("validated component row value must match its component column");
    values.push(*value);
}

fn clone_empty_impl<T: Component>() -> ErasedComponent {
    Box::new(Vec::<T>::new())
}

fn len_impl<T: Component>(values: &ErasedComponent) -> usize {
    values
        .downcast_ref::<Vec<T>>()
        .expect("component column type must match its component ID")
        .len()
}

/// Stores entities that have the same component type set.
pub(crate) struct Archetype {
    id: ArchetypeId,
    component_ids: Vec<ComponentId>,
    columns: HashMap<ComponentId, ComponentColumn>,
    entities: Vec<Entity>,
}

impl Archetype {
    pub(crate) fn new(id: ArchetypeId, mut component_ids: Vec<ComponentId>) -> Self {
        component_ids.sort_unstable();
        component_ids.dedup();
        Self {
            id,
            component_ids,
            columns: HashMap::new(),
            entities: Vec::new(),
        }
    }

    pub(crate) fn id(&self) -> ArchetypeId {
        self.id
    }

    pub(crate) fn component_ids(&self) -> &[ComponentId] {
        &self.component_ids
    }

    pub(crate) fn entities(&self) -> &[Entity] {
        &self.entities
    }

    pub(crate) fn has_component<T: Component>(&self) -> bool {
        self.has_component_id(ComponentId::of::<T>())
    }

    pub(crate) fn has_component_id(&self, component_id: ComponentId) -> bool {
        self.component_ids.binary_search(&component_id).is_ok()
    }

    pub(crate) fn initialize_component<T: Component>(&mut self) {
        let component_id = ComponentId::of::<T>();
        self.columns
            .entry(component_id)
            .or_insert_with(ComponentColumn::new::<T>);
    }

    pub(crate) fn copy_empty_columns_from(&mut self, source: &Self) {
        for (component_id, column) in &source.columns {
            if self.has_component_id(*component_id) {
                self.columns
                    .entry(*component_id)
                    .or_insert_with(|| column.clone_empty());
            }
        }
    }

    pub(crate) fn push_row(
        &mut self,
        entity: Entity,
        mut row: ComponentRow,
    ) -> Result<usize, WorldError> {
        self.validate()?;

        row.sort_unstable_by_key(|(component_id, _)| *component_id);
        if row.len() != self.component_ids.len()
            || !row
                .iter()
                .zip(&self.component_ids)
                .all(|((row_id, _), component_id)| row_id == component_id)
        {
            return Err(WorldError::InternalInvariant(
                "component row does not match destination archetype",
            ));
        }

        for (component_id, value) in &row {
            let column = self
                .columns
                .get(component_id)
                .ok_or(WorldError::InternalInvariant(
                    "archetype is missing a declared component column",
                ))?;
            if !column.accepts(value) {
                return Err(WorldError::InternalInvariant(
                    "component row value type does not match its component ID",
                ));
            }
        }

        let index = self.entities.len();
        for (component_id, value) in row {
            self.columns
                .get_mut(&component_id)
                .expect("validated archetype must contain every row component")
                .push_erased(value);
        }
        self.entities.push(entity);
        Ok(index)
    }

    pub(crate) fn remove_row(
        &mut self,
        index: usize,
    ) -> Result<(Option<Entity>, ComponentRow), WorldError> {
        self.validate()?;
        if index >= self.entities.len() {
            return Err(WorldError::InternalInvariant(
                "entity metadata row is outside its archetype",
            ));
        }

        let mut row = Vec::with_capacity(self.component_ids.len());
        for component_id in &self.component_ids {
            let value = self
                .columns
                .get_mut(component_id)
                .expect("validated archetype must contain every declared column")
                .swap_remove(index);
            row.push((*component_id, value));
        }

        self.entities.swap_remove(index);
        Ok((self.entities.get(index).copied(), row))
    }

    pub(crate) fn component_slice<T: Component>(&self) -> Option<&[T]> {
        self.columns
            .get(&ComponentId::of::<T>())
            .and_then(ComponentColumn::slice::<T>)
    }

    pub(crate) fn component_slice_mut<T: Component>(&mut self) -> Option<&mut [T]> {
        self.columns
            .get_mut(&ComponentId::of::<T>())
            .and_then(ComponentColumn::slice_mut::<T>)
    }

    /// Returns a mutable component pointer through a shared archetype reference.
    ///
    /// # Safety
    ///
    /// The caller must have exclusive logical access to component `T`, and
    /// structural mutation must be forbidden while the pointer is in use.
    pub(crate) unsafe fn component_mut_ptr_unchecked<T: Component>(
        &self,
    ) -> Option<(*mut T, usize)> {
        let column = self.columns.get(&ComponentId::of::<T>())?;
        // SAFETY: The caller guarantees exclusive logical access to T.
        unsafe { column.mut_ptr_unchecked::<T>() }
    }

    pub(crate) fn validate(&self) -> Result<(), WorldError> {
        if self.component_ids.len() != self.columns.len() {
            return Err(WorldError::InternalInvariant(
                "archetype component ID set and column set differ",
            ));
        }

        for component_id in &self.component_ids {
            let column = self
                .columns
                .get(component_id)
                .ok_or(WorldError::InternalInvariant(
                    "archetype is missing a declared component column",
                ))?;
            if column.component_id != *component_id {
                return Err(WorldError::InternalInvariant(
                    "component column ID does not match its archetype key",
                ));
            }
            if column.len() != self.entities.len() {
                return Err(WorldError::InternalInvariant(
                    "component column length does not match entity column length",
                ));
            }
        }

        Ok(())
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
    fn archetype_keeps_component_columns_aligned() {
        let component_id = ComponentId::of::<Position>();
        let mut archetype = Archetype::new(ArchetypeId::new(0), vec![component_id]);
        archetype.initialize_component::<Position>();
        let entity = Entity::new(1, 0);

        archetype
            .push_row(
                entity,
                vec![(component_id, Box::new(Position { x: 1.0, y: 2.0 }))],
            )
            .unwrap();

        assert_eq!(archetype.entities().len(), 1);
        assert_eq!(
            archetype.component_slice::<Position>().unwrap(),
            &[Position { x: 1.0, y: 2.0 }]
        );
        archetype.validate().unwrap();
    }
}
