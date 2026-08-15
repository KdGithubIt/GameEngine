use crate::error::{ScheduleError, SystemBuildError};
use crate::registry::TypeRegistry;
use crate::schedule::Schedule;
use crate::system::IntoSystem;
use crate::world::World;
use crate::{
    ScheduleConfiguration, ScheduleDiagnostic, ScheduleEditError, ScheduleEntryInfo,
    SystemDescriptor, SystemId, SystemRegistrationError,
};

type AppRunner = Box<dyn Fn(App) -> Result<(), ScheduleError>>;

/// Combines a runtime [`World`] and [`Schedule`] into an application builder.
pub struct App {
    world: World,
    schedule: Schedule,
    /// Systems registered for the fixed-timestep update loop.
    fixed_schedule: Schedule,
    runner: Option<AppRunner>,
}

impl App {
    /// Creates an application with an empty world and schedule.
    pub fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(TypeRegistry::new());

        Self {
            world,
            schedule: Schedule::new(),
            fixed_schedule: Schedule::new(),
            runner: Some(Box::new(run_once)),
        }
    }

    /// Adds `system` after validating its parameter access.
    ///
    /// # Panics
    ///
    /// Panics when the system requests conflicting component or resource
    /// access. Use [`App::try_add_system`] to handle that failure.
    pub fn add_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.schedule.add_system(system);
        self
    }

    /// Tries to add `system` after validating its parameter access.
    pub fn try_add_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemBuildError> {
        self.schedule.try_add_system(system)?;
        Ok(self)
    }

    /// Adds an explicitly identified system to the per-frame schedule.
    pub fn add_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.try_add_system_with_descriptor(descriptor, system)
            .expect("explicit system descriptor and parameter access must be valid")
    }

    /// Tries to add an explicitly identified system to the per-frame schedule.
    pub fn try_add_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        if let Some(conflict) = std::iter::once(descriptor.id())
            .chain(descriptor.aliases())
            .find(|id| self.fixed_schedule.contains_system_id(id))
        {
            return Err(ScheduleEditError::DuplicateId(conflict.clone()).into());
        }
        self.schedule
            .try_add_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Registers a host-owned per-frame bridge with exclusive world access.
    ///
    /// Normal gameplay and engine systems should use typed system parameters.
    /// This method exists for dynamic-module bridges whose validated access
    /// list is data rather than a Rust generic type.
    pub fn try_add_exclusive_system_with_descriptor<F, E>(
        &mut self,
        descriptor: SystemDescriptor,
        system: F,
    ) -> Result<&mut Self, SystemRegistrationError>
    where
        F: FnMut(&mut World) -> Result<(), E> + Send + Sync + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        if let Some(conflict) = std::iter::once(descriptor.id())
            .chain(descriptor.aliases())
            .find(|id| self.fixed_schedule.contains_system_id(id))
        {
            return Err(ScheduleEditError::DuplicateId(conflict.clone()).into());
        }
        self.schedule
            .try_add_exclusive_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Adds `system` to the fixed-timestep schedule.
    ///
    /// The fixed schedule runs zero or more times per frame based on the
    /// accumulated time since the last frame.
    ///
    /// # Panics
    ///
    /// Panics when the system requests conflicting component or resource
    /// access. Use [`App::try_add_fixed_system`] to handle that failure.
    pub fn add_fixed_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.fixed_schedule.add_system(system);
        self
    }

    /// Tries to add `system` to the fixed-timestep schedule.
    pub fn try_add_fixed_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemBuildError> {
        self.fixed_schedule.try_add_system(system)?;
        Ok(self)
    }

    /// Adds an explicitly identified system to the fixed-timestep schedule.
    pub fn add_fixed_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.try_add_fixed_system_with_descriptor(descriptor, system)
            .expect("explicit fixed system descriptor and parameter access must be valid")
    }

    /// Tries to add an explicitly identified system to the fixed schedule.
    pub fn try_add_fixed_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        if let Some(conflict) = std::iter::once(descriptor.id())
            .chain(descriptor.aliases())
            .find(|id| self.schedule.contains_system_id(id))
        {
            return Err(ScheduleEditError::DuplicateId(conflict.clone()).into());
        }
        self.fixed_schedule
            .try_add_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Registers a host-owned fixed-step bridge with exclusive world access.
    ///
    /// This is the fixed-schedule counterpart of
    /// [`App::try_add_exclusive_system_with_descriptor`].
    pub fn try_add_exclusive_fixed_system_with_descriptor<F, E>(
        &mut self,
        descriptor: SystemDescriptor,
        system: F,
    ) -> Result<&mut Self, SystemRegistrationError>
    where
        F: FnMut(&mut World) -> Result<(), E> + Send + Sync + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        if let Some(conflict) = std::iter::once(descriptor.id())
            .chain(descriptor.aliases())
            .find(|id| self.schedule.contains_system_id(id))
        {
            return Err(ScheduleEditError::DuplicateId(conflict.clone()).into());
        }
        self.fixed_schedule
            .try_add_exclusive_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Runs one step of the fixed-timestep schedule.
    ///
    /// The caller is responsible for invoking this the correct number of times
    /// per frame based on the accumulated delta from [`crate::world::World`]
    /// resources such as `FixedTime`.
    pub fn run_fixed_update(&mut self) -> Result<(), ScheduleError> {
        self.fixed_schedule.run(&mut self.world)
    }

    /// Inserts or replaces a world resource.
    pub fn insert_resource<T: 'static + Send + Sync>(&mut self, resource: T) -> &mut Self {
        self.world.insert_resource(resource);
        self
    }

    /// Registers runtime type metadata for `T`.
    pub fn register_type<T: 'static + Send + Sync>(&mut self) -> &mut Self {
        if self.world.get_resource::<TypeRegistry>().is_none() {
            self.world.insert_resource(TypeRegistry::new());
        }
        self.world
            .get_resource_mut::<TypeRegistry>()
            .expect("TypeRegistry was inserted immediately before access")
            .register::<T>();
        self
    }

    /// Runs this application with its configured runner.
    pub fn run(mut self) -> Result<(), ScheduleError> {
        if let Some(runner) = self.runner.take() {
            runner(self)
        } else {
            Ok(())
        }
    }

    /// Returns a shared reference to the runtime world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Returns an exclusive reference to the runtime world.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Runs one schedule update.
    pub fn update(&mut self) -> Result<(), ScheduleError> {
        self.schedule.run(&mut self.world)
    }

    /// Returns per-frame system metadata in current execution order.
    pub fn update_system_infos(&self) -> Vec<ScheduleEntryInfo> {
        self.schedule.system_infos()
    }

    /// Returns fixed-timestep system metadata in current execution order.
    pub fn fixed_system_infos(&self) -> Vec<ScheduleEntryInfo> {
        self.fixed_schedule.system_infos()
    }

    /// Applies project preferences to the per-frame schedule.
    pub fn apply_update_configuration(
        &mut self,
        configuration: &ScheduleConfiguration,
    ) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        self.schedule.apply_configuration(configuration)
    }

    /// Applies project preferences to the fixed-timestep schedule.
    pub fn apply_fixed_configuration(
        &mut self,
        configuration: &ScheduleConfiguration,
    ) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        self.fixed_schedule.apply_configuration(configuration)
    }

    /// Enables or disables a per-frame system by stable ID.
    pub fn set_update_system_enabled(
        &mut self,
        id: &SystemId,
        is_enabled: bool,
    ) -> Result<(), ScheduleEditError> {
        self.schedule.set_enabled(id, is_enabled)
    }

    /// Enables or disables a fixed-timestep system by stable ID.
    pub fn set_fixed_system_enabled(
        &mut self,
        id: &SystemId,
        is_enabled: bool,
    ) -> Result<(), ScheduleEditError> {
        self.fixed_schedule.set_enabled(id, is_enabled)
    }

    /// Moves a per-frame system to an exact position when constraints allow.
    pub fn move_update_system(
        &mut self,
        id: &SystemId,
        position: usize,
    ) -> Result<(), ScheduleEditError> {
        self.schedule.move_to(id, position)
    }

    /// Moves a fixed-timestep system to an exact position when constraints allow.
    pub fn move_fixed_system(
        &mut self,
        id: &SystemId,
        position: usize,
    ) -> Result<(), ScheduleEditError> {
        self.fixed_schedule.move_to(id, position)
    }

    /// Restores per-frame registration order and enabled states.
    pub fn reset_update_systems(&mut self) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        self.schedule.reset_to_default()
    }

    /// Restores fixed-timestep registration order and enabled states.
    pub fn reset_fixed_systems(&mut self) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        self.fixed_schedule.reset_to_default()
    }

    /// Returns the canonical per-frame configuration.
    pub fn update_configuration(&self) -> ScheduleConfiguration {
        self.schedule.configuration()
    }

    /// Returns the canonical fixed-timestep configuration.
    pub fn fixed_configuration(&self) -> ScheduleConfiguration {
        self.fixed_schedule.configuration()
    }
}

fn run_once(mut app: App) -> Result<(), ScheduleError> {
    app.update()
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Commands;
    use crate::query::{Query, Without};
    use crate::resource::{Res, ResMut};

    #[derive(Debug, PartialEq)]
    struct Position {
        x: f32,
        y: f32,
    }

    struct Name;

    struct Config {
        value: i32,
    }

    #[test]
    fn app_runs_queries_resources_and_commands() {
        let mut app = App::new();
        app.insert_resource(Config { value: 100 });
        app.insert_resource(0_usize);

        {
            let world = app.world_mut();
            world.spawn_with(Position { x: 10.0, y: 20.0 }).unwrap();
            let second = world.spawn_with(Position { x: 30.0, y: 40.0 }).unwrap();
            world.add_component(second, Name).unwrap();
        }

        app.add_system(
            |query: Query<&Position, Without<Name>>,
             config: Res<Config>,
             mut hits: ResMut<usize>| {
                assert_eq!(config.value, 100);
                for (_, position) in &query {
                    assert_eq!(position.x, 10.0);
                    *hits += 1;
                }
            },
        );
        app.add_system(|mut commands: Commands| {
            commands.spawn().insert(Position { x: 1.0, y: 2.0 });
        });

        app.update().unwrap();

        assert_eq!(app.world().entity_count(), 3);
        assert_eq!(app.world().get_resource::<usize>(), Some(&1));
        app.world().validate().unwrap();
    }

    #[test]
    fn register_type_restores_missing_runtime_registry() {
        struct Registered;

        let mut app = App::new();
        app.world_mut().remove_resource::<TypeRegistry>();

        app.register_type::<Registered>();

        let registry = app.world().get_resource::<TypeRegistry>().unwrap();
        assert!(registry.get(std::any::TypeId::of::<Registered>()).is_some());
    }

    #[test]
    fn update_and_fixed_schedule_preferences_are_independent() {
        let mut app = App::new();
        app.add_system_with_descriptor(
            SystemDescriptor::new("game.update", "Update", crate::SystemOrigin::Game).unwrap(),
            || {},
        );
        app.add_fixed_system_with_descriptor(
            SystemDescriptor::new("game.fixed", "Fixed", crate::SystemOrigin::Game).unwrap(),
            || {},
        );

        app.apply_update_configuration(&ScheduleConfiguration {
            order: vec![SystemId::try_new("game.update").unwrap()],
            disabled: vec![SystemId::try_new("game.update").unwrap()],
        })
        .unwrap();
        app.apply_fixed_configuration(&ScheduleConfiguration {
            order: vec![SystemId::try_new("game.fixed").unwrap()],
            disabled: Vec::new(),
        })
        .unwrap();

        assert!(!app.update_system_infos()[0].is_enabled);
        assert!(app.fixed_system_infos()[0].is_enabled);
        assert_eq!(app.update_configuration().order[0].as_str(), "game.update");
        assert_eq!(app.fixed_configuration().order[0].as_str(), "game.fixed");
    }

    #[test]
    fn exclusive_host_bridge_is_marked_and_runs_with_complete_world() {
        let mut app = App::new();
        app.try_add_exclusive_system_with_descriptor(
            SystemDescriptor::new(
                "engine.dynamic_bridge",
                "Bridge",
                crate::SystemOrigin::Engine,
            )
            .unwrap(),
            |world: &mut World| {
                world.insert_resource(17_u32);
                Ok::<(), std::io::Error>(())
            },
        )
        .unwrap();

        assert!(app
            .schedule
            .system_accesses()
            .next()
            .expect("bridge access must exist")
            .is_exclusive_world());
        app.update().unwrap();
        assert_eq!(app.world().get_resource::<u32>(), Some(&17));
    }
}
