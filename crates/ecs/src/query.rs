use crate::access::{AccessError, SystemAccess};
use crate::archetype::Archetype;
use crate::component::Component;
use crate::entity::Entity;
use crate::error::{QueryError, WorldError};
use crate::world::World;
use crate::world_cell::UnsafeWorldCell;
use std::marker::PhantomData;

mod sealed {
    use super::*;

    pub(crate) trait QueryDataSealed {
        type Fetch;

        fn register_access(access: &mut SystemAccess) -> Result<(), AccessError>;
        fn matches_archetype(archetype: &Archetype) -> bool;

        /// # Safety
        ///
        /// The archetype pointer must be valid, structurally immutable for the
        /// query lifetime, and match this query data.
        unsafe fn initialize_fetch(archetype: *const Archetype) -> Result<Self::Fetch, WorldError>;

        /// # Safety
        ///
        /// `index` must be in bounds for the prepared fetch, and all access
        /// conflicts for the query must have been rejected before iteration.
        unsafe fn fetch<'world>(
            fetch: &Self::Fetch,
            index: usize,
        ) -> <Self as super::QueryData>::Item<'world>
        where
            Self: super::QueryData;
    }

    pub(crate) trait ReadOnlyQueryDataSealed {}

    pub(crate) trait QueryFilterSealed {
        fn matches_archetype(archetype: &Archetype) -> bool;
    }
}

// The private supertrait prevents external implementations from bypassing the
// query access and fetch safety contract.
#[allow(private_bounds)]
/// Describes the component values yielded by a [`Query`].
pub trait QueryData: sealed::QueryDataSealed {
    /// The value yielded for one matching entity.
    type Item<'world>;
}

// The private supertrait ensures only query data proven to yield shared
// references can use shared query iteration.
#[allow(private_bounds)]
/// Marks query data that never yields mutable references.
pub trait ReadOnlyQueryData: QueryData + sealed::ReadOnlyQueryDataSealed {}

// The private supertrait prevents filters from observing or mutating component
// values outside the query access declaration.
#[allow(private_bounds)]
/// Filters archetypes included in a [`Query`].
pub trait QueryFilter: sealed::QueryFilterSealed {}

impl QueryFilter for () {}

impl sealed::QueryFilterSealed for () {
    fn matches_archetype(_: &Archetype) -> bool {
        true
    }
}

macro_rules! impl_query_filter_tuple {
    ($($filter:ident),+) => {
        impl<$($filter: QueryFilter),+> QueryFilter for ($($filter,)+) {}

        impl<$($filter: QueryFilter),+> sealed::QueryFilterSealed for ($($filter,)+) {
            fn matches_archetype(archetype: &Archetype) -> bool {
                $(<$filter as sealed::QueryFilterSealed>::matches_archetype(archetype))&&+
            }
        }
    };
}

impl_query_filter_tuple!(F0, F1);
impl_query_filter_tuple!(F0, F1, F2);
impl_query_filter_tuple!(F0, F1, F2, F3);

pub(crate) struct ReadFetch<T> {
    values: *const T,
    len: usize,
}

pub(crate) struct WriteFetch<T> {
    values: *mut T,
    len: usize,
}

impl<T: Component> QueryData for &T {
    type Item<'world> = &'world T;
}

impl<T: Component> sealed::QueryDataSealed for &T {
    type Fetch = ReadFetch<T>;

    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_component_read::<T>()
    }

    fn matches_archetype(archetype: &Archetype) -> bool {
        archetype.has_component::<T>()
    }

    unsafe fn initialize_fetch(archetype: *const Archetype) -> Result<Self::Fetch, WorldError> {
        // SAFETY: The caller guarantees the archetype pointer is valid and no
        // component references have been yielded yet.
        let archetype = unsafe { &*archetype };
        let values = archetype
            .component_slice::<T>()
            .ok_or(WorldError::InternalInvariant(
                "matching archetype is missing a component column",
            ))?;
        Ok(ReadFetch {
            values: values.as_ptr(),
            len: values.len(),
        })
    }

    unsafe fn fetch<'world>(
        fetch: &Self::Fetch,
        index: usize,
    ) -> <Self as QueryData>::Item<'world> {
        debug_assert!(index < fetch.len);
        // SAFETY: Query preparation validated the column length, and read
        // access may be shared with other reads of the same component.
        unsafe { &*fetch.values.add(index) }
    }
}

impl<T: Component> sealed::ReadOnlyQueryDataSealed for &T {}
impl<T: Component> ReadOnlyQueryData for &T {}

impl<T: Component> QueryData for &mut T {
    type Item<'world> = &'world mut T;
}

impl<T: Component> sealed::QueryDataSealed for &mut T {
    type Fetch = WriteFetch<T>;

    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_component_write::<T>()
    }

    fn matches_archetype(archetype: &Archetype) -> bool {
        archetype.has_component::<T>()
    }

    unsafe fn initialize_fetch(archetype: *const Archetype) -> Result<Self::Fetch, WorldError> {
        // SAFETY: The caller guarantees the archetype pointer is valid, this
        // component access is exclusive, and no references have been yielded.
        let archetype = unsafe { &*archetype };
        // SAFETY: Query access validation guarantees exclusive logical access
        // to component T for the query lifetime.
        let (values, len) = unsafe { archetype.component_mut_ptr_unchecked::<T>() }.ok_or(
            WorldError::InternalInvariant("matching archetype is missing a component column"),
        )?;
        Ok(WriteFetch { values, len })
    }

    unsafe fn fetch<'world>(
        fetch: &Self::Fetch,
        index: usize,
    ) -> <Self as QueryData>::Item<'world> {
        debug_assert!(index < fetch.len);
        // SAFETY: Query access validation guarantees this is the only mutable
        // access to component T, and mutable queries require a unique iterator.
        unsafe { &mut *fetch.values.add(index) }
    }
}

impl<T: Component> QueryData for Option<&T> {
    type Item<'world> = Option<&'world T>;
}

impl<T: Component> sealed::QueryDataSealed for Option<&T> {
    type Fetch = Option<ReadFetch<T>>;

    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_component_read::<T>()
    }

    fn matches_archetype(_: &Archetype) -> bool {
        true
    }

    unsafe fn initialize_fetch(archetype: *const Archetype) -> Result<Self::Fetch, WorldError> {
        // SAFETY: The caller guarantees the archetype pointer is valid and no
        // component references have been yielded yet.
        let archetype = unsafe { &*archetype };
        Ok(archetype.component_slice::<T>().map(|values| ReadFetch {
            values: values.as_ptr(),
            len: values.len(),
        }))
    }

    unsafe fn fetch<'world>(
        fetch: &Self::Fetch,
        index: usize,
    ) -> <Self as QueryData>::Item<'world> {
        fetch.as_ref().map(|fetch| {
            debug_assert!(index < fetch.len);
            // SAFETY: Query preparation validated the optional column length,
            // and shared component access may be aliased with other reads.
            unsafe { &*fetch.values.add(index) }
        })
    }
}

impl<T: Component> sealed::ReadOnlyQueryDataSealed for Option<&T> {}
impl<T: Component> ReadOnlyQueryData for Option<&T> {}

impl<T: Component> QueryData for Option<&mut T> {
    type Item<'world> = Option<&'world mut T>;
}

impl<T: Component> sealed::QueryDataSealed for Option<&mut T> {
    type Fetch = Option<WriteFetch<T>>;

    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_component_write::<T>()
    }

    fn matches_archetype(_: &Archetype) -> bool {
        true
    }

    unsafe fn initialize_fetch(archetype: *const Archetype) -> Result<Self::Fetch, WorldError> {
        // SAFETY: The caller guarantees the archetype pointer is valid, this
        // component access is exclusive, and no references have been yielded.
        let archetype = unsafe { &*archetype };
        // SAFETY: Query access validation guarantees exclusive logical access
        // to component T for the query lifetime.
        Ok(unsafe { archetype.component_mut_ptr_unchecked::<T>() }
            .map(|(values, len)| WriteFetch { values, len }))
    }

    unsafe fn fetch<'world>(
        fetch: &Self::Fetch,
        index: usize,
    ) -> <Self as QueryData>::Item<'world> {
        fetch.as_ref().map(|fetch| {
            debug_assert!(index < fetch.len);
            // SAFETY: Query access validation guarantees this is the only
            // mutable access to component T.
            unsafe { &mut *fetch.values.add(index) }
        })
    }
}

macro_rules! impl_query_data_tuple {
    ($(($name:ident, $index:tt)),+) => {
        impl<$($name: QueryData),+> QueryData for ($($name,)+) {
            type Item<'world> = ($($name::Item<'world>,)+);
        }

        impl<$($name: QueryData),+> sealed::QueryDataSealed for ($($name,)+) {
            type Fetch = ($(<$name as sealed::QueryDataSealed>::Fetch,)+);

            fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
                $(<$name as sealed::QueryDataSealed>::register_access(access)?;)+
                Ok(())
            }

            fn matches_archetype(archetype: &Archetype) -> bool {
                $(<$name as sealed::QueryDataSealed>::matches_archetype(archetype))&&+
            }

            unsafe fn initialize_fetch(
                archetype: *const Archetype,
            ) -> Result<Self::Fetch, WorldError> {
                Ok((
                    $(
                        // SAFETY: The caller validated the tuple access before
                        // preparing any component references.
                        unsafe {
                            <$name as sealed::QueryDataSealed>::initialize_fetch(archetype)?
                        },
                    )+
                ))
            }

            unsafe fn fetch<'world>(
                fetch: &Self::Fetch,
                index: usize,
            ) -> <Self as QueryData>::Item<'world> {
                (
                    $(
                        // SAFETY: The tuple access declaration rejected
                        // overlapping mutable component access.
                        unsafe {
                            <$name as sealed::QueryDataSealed>::fetch(&fetch.$index, index)
                        },
                    )+
                )
            }
        }

        impl<$($name: ReadOnlyQueryData),+> sealed::ReadOnlyQueryDataSealed for ($($name,)+) {}
        impl<$($name: ReadOnlyQueryData),+> ReadOnlyQueryData for ($($name,)+) {}
    };
}

impl_query_data_tuple!((Q0, 0), (Q1, 1));
impl_query_data_tuple!((Q0, 0), (Q1, 1), (Q2, 2));
impl_query_data_tuple!((Q0, 0), (Q1, 1), (Q2, 2), (Q3, 3));
impl_query_data_tuple!((Q0, 0), (Q1, 1), (Q2, 2), (Q3, 3), (Q4, 4));
impl_query_data_tuple!((Q0, 0), (Q1, 1), (Q2, 2), (Q3, 3), (Q4, 4), (Q5, 5));
impl_query_data_tuple!(
    (Q0, 0),
    (Q1, 1),
    (Q2, 2),
    (Q3, 3),
    (Q4, 4),
    (Q5, 5),
    (Q6, 6)
);
impl_query_data_tuple!(
    (Q0, 0),
    (Q1, 1),
    (Q2, 2),
    (Q3, 3),
    (Q4, 4),
    (Q5, 5),
    (Q6, 6),
    (Q7, 7)
);

/// Includes only archetypes that contain component `T`.
pub struct With<T>(PhantomData<T>);

impl<T: Component> QueryFilter for With<T> {}

impl<T: Component> sealed::QueryFilterSealed for With<T> {
    fn matches_archetype(archetype: &Archetype) -> bool {
        archetype.has_component::<T>()
    }
}

/// Includes only archetypes that do not contain component `T`.
pub struct Without<T>(PhantomData<T>);

impl<T: Component> QueryFilter for Without<T> {}

impl<T: Component> sealed::QueryFilterSealed for Without<T> {
    fn matches_archetype(archetype: &Archetype) -> bool {
        !archetype.has_component::<T>()
    }
}

struct ArchetypeFetch<Q: QueryData> {
    entities: *const Entity,
    len: usize,
    components: <Q as sealed::QueryDataSealed>::Fetch,
}

/// Provides typed iteration over entities and components in a [`World`].
///
/// A direct query exclusively borrows its world for the query lifetime.
/// Systems receive queries through validated [`crate::SystemParam`] access.
///
/// Mutable items remain tied to the iterator's query borrow, so safe code
/// cannot retain one mutable item and create a second iterator over the same
/// query.
///
/// ```compile_fail
/// use engine_ecs::{Query, World};
///
/// let mut world = World::new();
/// let mut query = Query::<&mut i32>::new(&mut world);
/// let first = query.iter_mut().next().unwrap().1;
/// let second = query.iter_mut().next().unwrap().1;
/// *first += *second;
/// ```
pub struct Query<'world, Q: QueryData, F: QueryFilter = ()> {
    archetypes: Vec<ArchetypeFetch<Q>>,
    marker: PhantomData<(&'world mut World, F)>,
}

impl<'world, Q: QueryData, F: QueryFilter> Query<'world, Q, F> {
    /// Creates a direct query over `world`.
    ///
    /// # Panics
    ///
    /// Panics if the query requests conflicting component access, such as
    /// `(&mut T, &mut T)`, or if an internal world invariant is broken. Use
    /// [`Query::try_new`] to handle those failures.
    pub fn new(world: &'world mut World) -> Self {
        Self::try_new(world).expect("direct query must have valid access and world storage")
    }

    /// Tries to create a direct query over `world`.
    pub fn try_new(world: &'world mut World) -> Result<Self, QueryError> {
        let mut access = SystemAccess::new();
        <Q as sealed::QueryDataSealed>::register_access(&mut access)?;
        let world = UnsafeWorldCell::new(world);
        // SAFETY: The direct query exclusively borrows the world, and its
        // component access was validated immediately above.
        unsafe { Self::from_world_cell(world) }
    }

    /// Returns an iterator that may yield mutable component references.
    ///
    /// Requiring `&mut self` prevents multiple mutable iterators from being
    /// created from one query through safe code.
    pub fn iter_mut(&mut self) -> QueryIter<'_, Q> {
        QueryIter::new(&self.archetypes)
    }

    pub(crate) fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        <Q as sealed::QueryDataSealed>::register_access(access)
    }

    pub(crate) unsafe fn from_world_cell(
        world: UnsafeWorldCell<'world>,
    ) -> Result<Self, QueryError> {
        let storage = world.storage_ptr();
        let mut archetypes = Vec::new();

        // SAFETY: System access validation prevents structural world mutation
        // while system parameters are alive. Direct queries exclusively borrow
        // the world. No component references have been yielded yet.
        for archetype in unsafe { (&*storage).archetypes() } {
            if !<Q as sealed::QueryDataSealed>::matches_archetype(archetype)
                || !<F as sealed::QueryFilterSealed>::matches_archetype(archetype)
            {
                continue;
            }

            archetype.validate()?;
            let entities = archetype.entities();
            let entities_ptr = entities.as_ptr();
            let len = entities.len();
            let archetype_ptr = std::ptr::from_ref(archetype);
            // SAFETY: The archetype matches Q, access conflicts were rejected,
            // and no references have been yielded from this query.
            let components =
                unsafe { <Q as sealed::QueryDataSealed>::initialize_fetch(archetype_ptr)? };
            archetypes.push(ArchetypeFetch {
                entities: entities_ptr,
                len,
                components,
            });
        }

        Ok(Self {
            archetypes,
            marker: PhantomData,
        })
    }
}

impl<'world, Q: ReadOnlyQueryData, F: QueryFilter> Query<'world, Q, F> {
    /// Returns an iterator over read-only query results.
    pub fn iter(&self) -> QueryIter<'_, Q> {
        QueryIter::new(&self.archetypes)
    }
}

/// Iterates through the prepared archetype columns of a [`Query`].
pub struct QueryIter<'query, Q: QueryData> {
    archetypes: &'query [ArchetypeFetch<Q>],
    archetype_index: usize,
    entity_index: usize,
    marker: PhantomData<&'query mut Q>,
}

impl<'query, Q: QueryData> QueryIter<'query, Q> {
    fn new(archetypes: &'query [ArchetypeFetch<Q>]) -> Self {
        Self {
            archetypes,
            archetype_index: 0,
            entity_index: 0,
            marker: PhantomData,
        }
    }
}

impl<'query, Q: QueryData> Iterator for QueryIter<'query, Q> {
    type Item = (Entity, Q::Item<'query>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let archetype = self.archetypes.get(self.archetype_index)?;
            if self.entity_index < archetype.len {
                let index = self.entity_index;
                self.entity_index += 1;

                // SAFETY: Query preparation captured an entity column with the
                // same length as every component column, and structural
                // mutation is forbidden for the query lifetime.
                let entity = unsafe { *archetype.entities.add(index) };
                // SAFETY: The iterator only visits in-bounds rows, and query
                // access validation rejected overlapping mutable access.
                let item =
                    unsafe { <Q as sealed::QueryDataSealed>::fetch(&archetype.components, index) };
                return Some((entity, item));
            }

            self.archetype_index += 1;
            self.entity_index = 0;
        }
    }
}

impl<'query, 'world, Q: ReadOnlyQueryData, F: QueryFilter> IntoIterator
    for &'query Query<'world, Q, F>
{
    type Item = (Entity, Q::Item<'query>);
    type IntoIter = QueryIter<'query, Q>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'query, 'world, Q: QueryData, F: QueryFilter> IntoIterator
    for &'query mut Query<'world, Q, F>
{
    type Item = (Entity, Q::Item<'query>);
    type IntoIter = QueryIter<'query, Q>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
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
    fn filters_include_expected_archetypes() {
        let mut world = World::new();
        let first = world.spawn_with(Position { x: 0.0, y: 0.0 }).unwrap();
        world.add_component(first, Velocity).unwrap();
        world.spawn_with(Position { x: 10.0, y: 10.0 }).unwrap();

        let with_velocity = Query::<&Position, With<Velocity>>::new(&mut world);
        assert_eq!(with_velocity.iter().count(), 1);
        drop(with_velocity);

        let without_velocity = Query::<&Position, Without<Velocity>>::new(&mut world);
        assert_eq!(without_velocity.iter().count(), 1);
    }

    #[test]
    fn mutable_query_updates_each_component_once() {
        let mut world = World::new();
        world.spawn_with(Position { x: 1.0, y: 2.0 }).unwrap();

        let mut query = Query::<&mut Position>::new(&mut world);
        for (_, position) in &mut query {
            position.x += 2.0;
            position.y += 3.0;
        }
        drop(query);

        let entity = world.entities().next().unwrap();
        assert_eq!(
            world.get_component::<Position>(entity),
            Some(&Position { x: 3.0, y: 5.0 })
        );
    }

    #[test]
    fn conflicting_query_tuple_is_rejected() {
        let mut world = World::new();
        let result = Query::<(&mut Position, &mut Position)>::try_new(&mut world);

        assert!(matches!(result, Err(QueryError::Access(_))));
    }

    #[test]
    fn filter_tuples_combine_requirements() {
        let mut world = World::new();
        let entity = world.spawn_with(Position { x: 1.0, y: 2.0 }).unwrap();
        world.add_component(entity, Velocity).unwrap();

        let query = Query::<&Position, (With<Velocity>, Without<String>)>::new(&mut world);

        assert_eq!(query.iter().count(), 1);
    }
}
