use crate::access::{AccessError, SystemAccess};
use crate::commands::Commands;
use crate::error::{SystemBuildError, SystemExecutionError, SystemParamError, SystemRunError};
use crate::query::{Query, QueryData, QueryFilter};
use crate::resource::{Res, ResMut};
use crate::world::World;
use crate::world_cell::UnsafeWorldCell;
use std::marker::PhantomData;

mod sealed {
    use super::*;

    pub(crate) trait SystemSealed {}

    pub(crate) trait IntoSystemSealed<Params, Marker> {}

    pub(crate) trait SystemParamSealed: Sized {
        fn register_access(access: &mut SystemAccess) -> Result<(), AccessError>;

        /// # Safety
        ///
        /// The caller must validate the complete system access declaration
        /// before fetching any parameters from `world`.
        unsafe fn fetch<'world>(
            world: UnsafeWorldCell<'world>,
        ) -> Result<<Self as super::SystemParam>::Item<'world>, SystemParamError>
        where
            Self: super::SystemParam;
    }
}

// The private supertrait prevents external implementations from fetching
// unchecked world references without participating in access validation.
#[allow(private_bounds)]
/// Describes a value that can be fetched for one system run.
pub trait SystemParam: sealed::SystemParamSealed {
    /// The world-bound value passed to the system function.
    type Item<'world>: SystemParam;
}

impl<'param, Q, F> SystemParam for Query<'param, Q, F>
where
    Q: QueryData + 'static,
    F: QueryFilter + 'static,
{
    type Item<'world> = Query<'world, Q, F>;
}

impl<'param, Q, F> sealed::SystemParamSealed for Query<'param, Q, F>
where
    Q: QueryData + 'static,
    F: QueryFilter + 'static,
{
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        Query::<Q, F>::register_access(access)
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: The caller validated this query's component access as part of
        // the complete system access declaration.
        unsafe { Query::from_world_cell(world) }.map_err(SystemParamError::from)
    }
}

impl<'param, T: 'static + Send + Sync> SystemParam for Res<'param, T> {
    type Item<'world> = Res<'world, T>;
}

impl<'param, T: 'static + Send + Sync> sealed::SystemParamSealed for Res<'param, T> {
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_resource_read::<T>()
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: The caller validated shared access to resource T.
        let value = unsafe { world.resource::<T>() }.ok_or(SystemParamError::MissingResource {
            type_name: std::any::type_name::<T>(),
        })?;
        Ok(Res::new(value))
    }
}

impl<'param, T: 'static + Send + Sync> SystemParam for ResMut<'param, T> {
    type Item<'world> = ResMut<'world, T>;
}

impl<'param, T: 'static + Send + Sync> sealed::SystemParamSealed for ResMut<'param, T> {
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_resource_write::<T>()
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: The caller validated exclusive access to resource T.
        let value =
            unsafe { world.resource_mut::<T>() }.ok_or(SystemParamError::MissingResource {
                type_name: std::any::type_name::<T>(),
            })?;
        Ok(ResMut::new(value))
    }
}

impl<'param, T: 'static + Send + Sync> SystemParam for Option<Res<'param, T>> {
    type Item<'world> = Option<Res<'world, T>>;
}

impl<'param, T: 'static + Send + Sync> sealed::SystemParamSealed for Option<Res<'param, T>> {
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_resource_read::<T>()
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: The caller validated shared access to resource T.
        let value = unsafe { world.resource::<T>() };
        Ok(value.map(Res::new))
    }
}

impl<'param, T: 'static + Send + Sync> SystemParam for Option<ResMut<'param, T>> {
    type Item<'world> = Option<ResMut<'world, T>>;
}

impl<'param, T: 'static + Send + Sync> sealed::SystemParamSealed for Option<ResMut<'param, T>> {
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
        access.add_resource_write::<T>()
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: The caller validated exclusive access to resource T.
        let value = unsafe { world.resource_mut::<T>() };
        Ok(value.map(ResMut::new))
    }
}

impl<'param> SystemParam for Commands<'param> {
    type Item<'world> = Commands<'world>;
}

impl<'param> sealed::SystemParamSealed for Commands<'param> {
    fn register_access(_: &mut SystemAccess) -> Result<(), AccessError> {
        Ok(())
    }

    unsafe fn fetch<'world>(
        world: UnsafeWorldCell<'world>,
    ) -> Result<<Self as SystemParam>::Item<'world>, SystemParamError> {
        // SAFETY: Commands only clones the disjoint command sender and entity
        // allocator fields. Structural mutations remain deferred.
        let sender = unsafe { world.command_sender() };
        // SAFETY: See the command sender safety explanation above.
        let allocator = unsafe { world.entity_allocator() };
        Ok(Commands::new(sender, allocator))
    }
}

// The private supertrait ensures every system has access metadata produced by
// this crate before a future scheduler relies on it for parallel execution.
#[allow(private_bounds)]
/// Runs one unit of ECS behavior.
///
/// This trait is implemented only by ECS-owned wrappers so its access metadata
/// can be trusted by a future parallel scheduler. Use a closure that captures
/// state to create stateful systems.
///
/// ```compile_fail
/// use engine_ecs::{System, SystemAccess, SystemRunError, World};
///
/// struct ExternalSystem;
///
/// impl System for ExternalSystem {
///     fn run(&mut self, _: &mut World) -> Result<(), SystemRunError> {
///         Ok(())
///     }
///
///     fn name(&self) -> &str {
///         "ExternalSystem"
///     }
///
///     fn access(&self) -> &SystemAccess {
///         unreachable!()
///     }
/// }
/// ```
pub trait System: sealed::SystemSealed + Send + Sync {
    /// Runs this system against `world`.
    fn run(&mut self, world: &mut World) -> Result<(), SystemRunError>;

    /// Returns the Rust type name of this system.
    fn name(&self) -> &str;

    /// Returns the component and resource access required by this system.
    fn access(&self) -> &SystemAccess;
}

// The private supertrait prevents external conversions from constructing a
// system whose access declaration does not match its runtime behavior.
#[allow(private_bounds)]
/// Converts a function or closure into a [`System`].
///
/// Implementations are restricted to conversions that validate their complete
/// component and resource access declaration.
pub trait IntoSystem<Params, Marker>: sealed::IntoSystemSealed<Params, Marker> + Sized {
    /// The concrete system wrapper produced by this conversion.
    type System: System + 'static;

    /// Builds the system and validates its parameter access.
    fn into_system(self) -> Result<Self::System, SystemBuildError>;
}

/// Wraps a function or closure as a runtime [`System`].
pub struct FunctionSystem<F, Marker> {
    function: F,
    name: &'static str,
    access: SystemAccess,
    marker: PhantomData<fn() -> Marker>,
}

type ExclusiveCallback =
    dyn FnMut(&mut World) -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + Sync;

/// Host-owned system whose validated callback needs exclusive world access.
///
/// This type is crate-private so game and engine code cannot bypass normal
/// parameter access declarations. [`crate::App`] exposes only registration
/// methods that keep the exclusive marker attached to scheduling metadata.
pub(crate) struct ExclusiveFunctionSystem {
    function: Box<ExclusiveCallback>,
    name: &'static str,
    access: SystemAccess,
}

impl sealed::SystemSealed for ExclusiveFunctionSystem {}

impl System for ExclusiveFunctionSystem {
    fn run(&mut self, world: &mut World) -> Result<(), SystemRunError> {
        (self.function)(world)
            .map_err(|error| SystemRunError::new(self.name, SystemExecutionError::Callback(error)))
    }

    fn name(&self) -> &str {
        self.name
    }

    fn access(&self) -> &SystemAccess {
        &self.access
    }
}

pub(crate) fn exclusive_system<F, E>(function: F) -> ExclusiveFunctionSystem
where
    F: FnMut(&mut World) -> Result<(), E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let name = std::any::type_name::<F>();
    let mut function = function;
    ExclusiveFunctionSystem {
        function: Box::new(move |world| {
            function(world)
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        }),
        name,
        access: SystemAccess::exclusive_world(),
    }
}

impl<F, Marker> sealed::SystemSealed for FunctionSystem<F, Marker> {}

impl<F, Marker> System for FunctionSystem<F, Marker>
where
    F: SystemParamFunction<Marker> + Send + Sync + 'static,
{
    fn run(&mut self, world: &mut World) -> Result<(), SystemRunError> {
        let world = UnsafeWorldCell::new(world);
        self.function
            .run(world)
            .map_err(|error| SystemRunError::new(self.name, error))
    }

    fn name(&self) -> &str {
        self.name
    }

    fn access(&self) -> &SystemAccess {
        &self.access
    }
}

trait SystemParamFunction<Marker>: Send + Sync + 'static {
    fn register_access(access: &mut SystemAccess) -> Result<(), AccessError>;
    fn run(&mut self, world: UnsafeWorldCell<'_>) -> Result<(), SystemExecutionError>;
}

/// Type marker separating fallible function systems from unit-returning ones.
pub struct FallibleSystemMarker<Signature, Error>(PhantomData<fn() -> (Signature, Error)>);

fn make_system<F, Marker>(function: F) -> Result<FunctionSystem<F, Marker>, SystemBuildError>
where
    F: SystemParamFunction<Marker>,
{
    let name = std::any::type_name::<F>();
    let mut access = SystemAccess::new();
    F::register_access(&mut access).map_err(|error| SystemBuildError::new(name, error))?;
    Ok(FunctionSystem {
        function,
        name,
        access,
        marker: PhantomData,
    })
}

impl<F> SystemParamFunction<fn()> for F
where
    F: FnMut() + Send + Sync + 'static,
{
    fn register_access(_: &mut SystemAccess) -> Result<(), AccessError> {
        Ok(())
    }

    fn run(&mut self, _: UnsafeWorldCell<'_>) -> Result<(), SystemExecutionError> {
        (self)();
        Ok(())
    }
}

impl<F> IntoSystem<(), fn()> for F
where
    F: FnMut() + Send + Sync + 'static,
{
    type System = FunctionSystem<F, fn()>;

    fn into_system(self) -> Result<Self::System, SystemBuildError> {
        make_system(self)
    }
}

impl<F> sealed::IntoSystemSealed<(), fn()> for F where F: FnMut() + Send + Sync + 'static {}

impl<F, E> SystemParamFunction<FallibleSystemMarker<fn(), E>> for F
where
    F: FnMut() -> Result<(), E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    fn register_access(_: &mut SystemAccess) -> Result<(), AccessError> {
        Ok(())
    }

    fn run(&mut self, _: UnsafeWorldCell<'_>) -> Result<(), SystemExecutionError> {
        (self)().map_err(|error| SystemExecutionError::Callback(Box::new(error)))
    }
}

impl<F, E> IntoSystem<(), FallibleSystemMarker<fn(), E>> for F
where
    F: FnMut() -> Result<(), E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    type System = FunctionSystem<F, FallibleSystemMarker<fn(), E>>;

    fn into_system(self) -> Result<Self::System, SystemBuildError> {
        make_system(self)
    }
}

impl<F, E> sealed::IntoSystemSealed<(), FallibleSystemMarker<fn(), E>> for F
where
    F: FnMut() -> Result<(), E> + Send + Sync + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
}

macro_rules! impl_system_function {
    ($(($param:ident, $value:ident)),+) => {
        impl<F, $($param),+> SystemParamFunction<fn($($param),+)> for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) + Send + Sync + 'static,
            for<'function> &'function mut F: FnMut($($param),+),
        {
            fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
                $(<$param as sealed::SystemParamSealed>::register_access(access)?;)+
                Ok(())
            }

            fn run(&mut self, world: UnsafeWorldCell<'_>) -> Result<(), SystemExecutionError> {
                $(
                    // SAFETY: The complete system access declaration was
                    // validated when the system was built.
                    let $value = unsafe {
                        <$param as sealed::SystemParamSealed>::fetch(world)?
                    };
                )+
                (&mut *self)($($value),+);
                Ok(())
            }
        }

        impl<F, $($param),+> IntoSystem<($($param,)+), fn($($param),+)> for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) + Send + Sync + 'static,
            for<'function> &'function mut F: FnMut($($param),+),
        {
            type System = FunctionSystem<F, fn($($param),+)>;

            fn into_system(self) -> Result<Self::System, SystemBuildError> {
                make_system(self)
            }
        }

        impl<F, $($param),+> sealed::IntoSystemSealed<($($param,)+), fn($($param),+)> for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) + Send + Sync + 'static,
            for<'function> &'function mut F: FnMut($($param),+),
        {}

        impl<F, E, $($param),+>
            SystemParamFunction<FallibleSystemMarker<fn($($param),+), E>> for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) -> Result<(), E>
                + Send
                + Sync
                + 'static,
            for<'function> &'function mut F: FnMut($($param),+) -> Result<(), E>,
            E: std::error::Error + Send + Sync + 'static,
        {
            fn register_access(access: &mut SystemAccess) -> Result<(), AccessError> {
                $(<$param as sealed::SystemParamSealed>::register_access(access)?;)+
                Ok(())
            }

            fn run(&mut self, world: UnsafeWorldCell<'_>) -> Result<(), SystemExecutionError> {
                $(
                    // SAFETY: The complete system access declaration was
                    // validated when the system was built.
                    let $value = unsafe {
                        <$param as sealed::SystemParamSealed>::fetch(world)?
                    };
                )+
                (&mut *self)($($value),+)
                    .map_err(|error| SystemExecutionError::Callback(Box::new(error)))
            }
        }

        impl<F, E, $($param),+>
            IntoSystem<
                ($($param,)+),
                FallibleSystemMarker<fn($($param),+), E>,
            > for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) -> Result<(), E>
                + Send
                + Sync
                + 'static,
            for<'function> &'function mut F: FnMut($($param),+) -> Result<(), E>,
            E: std::error::Error + Send + Sync + 'static,
        {
            type System = FunctionSystem<
                F,
                FallibleSystemMarker<fn($($param),+), E>,
            >;

            fn into_system(self) -> Result<Self::System, SystemBuildError> {
                make_system(self)
            }
        }

        impl<F, E, $($param),+>
            sealed::IntoSystemSealed<
                ($($param,)+),
                FallibleSystemMarker<fn($($param),+), E>,
            > for F
        where
            $($param: SystemParam + 'static,)+
            F: for<'world> FnMut($($param::Item<'world>),+) -> Result<(), E>
                + Send
                + Sync
                + 'static,
            for<'function> &'function mut F: FnMut($($param),+) -> Result<(), E>,
            E: std::error::Error + Send + Sync + 'static,
        {}
    };
}

impl_system_function!((P0, p0));
impl_system_function!((P0, p0), (P1, p1));
impl_system_function!((P0, p0), (P1, p1), (P2, p2));
impl_system_function!((P0, p0), (P1, p1), (P2, p2), (P3, p3));
impl_system_function!((P0, p0), (P1, p1), (P2, p2), (P3, p3), (P4, p4));
impl_system_function!((P0, p0), (P1, p1), (P2, p2), (P3, p3), (P4, p4), (P5, p5));
impl_system_function!(
    (P0, p0),
    (P1, p1),
    (P2, p2),
    (P3, p3),
    (P4, p4),
    (P5, p5),
    (P6, p6)
);
impl_system_function!(
    (P0, p0),
    (P1, p1),
    (P2, p2),
    (P3, p3),
    (P4, p4),
    (P5, p5),
    (P6, p6),
    (P7, p7)
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Query;

    #[test]
    fn resource_param_is_fetched_for_system_run() {
        let mut world = World::new();
        world.insert_resource(99_i32);
        let mut system = (|value: Res<i32>| {
            assert_eq!(*value, 99);
        })
        .into_system()
        .unwrap();

        system.run(&mut world).unwrap();
    }

    #[test]
    fn conflicting_resource_params_are_rejected() {
        let result = (|_: ResMut<i32>, _: ResMut<i32>| {}).into_system();

        assert!(result.is_err());
    }

    #[test]
    fn missing_resource_returns_run_error() {
        let mut world = World::new();
        let mut system = (|_: Res<i32>| {}).into_system().unwrap();

        assert!(system.run(&mut world).is_err());
    }

    #[test]
    fn optional_resource_mut_is_none_when_resource_is_absent() {
        let mut world = World::new();
        let mut system = (|value: Option<ResMut<i32>>| {
            assert!(value.is_none());
        })
        .into_system()
        .unwrap();

        system.run(&mut world).unwrap();
    }

    #[test]
    fn optional_resource_mut_mutates_present_resource() {
        let mut world = World::new();
        world.insert_resource(1_i32);
        let mut system = (|value: Option<ResMut<i32>>| {
            if let Some(mut value) = value {
                *value += 41;
            }
        })
        .into_system()
        .unwrap();

        system.run(&mut world).unwrap();
        assert_eq!(world.get_resource::<i32>(), Some(&42));
    }

    #[test]
    fn conflicting_query_params_are_rejected() {
        struct Position;

        let result = (|_: Query<&mut Position>, _: Query<&mut Position>| {}).into_system();

        assert!(result.is_err());
    }

    #[derive(Debug)]
    struct ExpectedFailure;

    impl std::fmt::Display for ExpectedFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("expected failure")
        }
    }

    impl std::error::Error for ExpectedFailure {}

    #[test]
    fn fallible_system_error_is_reported() {
        let mut world = World::new();
        world.insert_resource(1_i32);
        let mut system = (|_: Res<i32>| -> Result<(), ExpectedFailure> { Err(ExpectedFailure) })
            .into_system()
            .unwrap();

        let error = system.run(&mut world).expect_err("callback must fail");
        assert!(error.to_string().contains("expected failure"));
    }
}
