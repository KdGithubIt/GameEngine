use crate::error::{ScheduleError, SystemBuildError};
use crate::system::{exclusive_system, IntoSystem, System};
use crate::system_descriptor::{
    ScheduleConfiguration, ScheduleDiagnostic, ScheduleEditError, ScheduleEntryInfo,
    SystemDescriptor, SystemId, SystemRegistrationError,
};
use crate::world::World;
use std::collections::{BTreeMap, BTreeSet};

struct ScheduleEntry {
    descriptor: SystemDescriptor,
    system: Box<dyn System>,
    is_enabled: bool,
    default_order: usize,
}

/// Runs an ordered list of runtime systems.
///
/// Systems execute sequentially for now. Metadata and validated parameter
/// access remain attached to each entry so editor ordering never needs to
/// inspect or downcast the concrete system object.
pub struct Schedule {
    systems: Vec<ScheduleEntry>,
}

impl Schedule {
    /// Creates an empty schedule.
    pub fn new() -> Self {
        Self {
            systems: Vec::new(),
        }
    }

    /// Adds `system` through the compatibility API.
    ///
    /// New persistent registrations should use
    /// [`Schedule::add_system_with_descriptor`].
    ///
    /// # Panics
    ///
    /// Panics when the system requests conflicting component or resource
    /// access. Use [`Schedule::try_add_system`] to handle that failure.
    pub fn add_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.try_add_system(system)
            .expect("system parameter access must not conflict")
    }

    /// Tries to add `system` without an explicit stable identifier.
    pub fn try_add_system<Params, Marker>(
        &mut self,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemBuildError> {
        let system = system.into_system()?;
        let descriptor = SystemDescriptor::unnamed(system.name(), self.systems.len());
        self.systems.push(ScheduleEntry {
            descriptor,
            system: Box::new(system),
            is_enabled: true,
            default_order: self.systems.len(),
        });
        Ok(self)
    }

    /// Adds an explicitly identified system entry.
    ///
    /// # Panics
    ///
    /// Panics when access validation fails or the descriptor ID conflicts
    /// with another ID or alias. Use
    /// [`Schedule::try_add_system_with_descriptor`] to handle the error.
    pub fn add_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> &mut Self {
        self.try_add_system_with_descriptor(descriptor, system)
            .expect("explicit system descriptor and parameter access must be valid")
    }

    /// Tries to add an explicitly identified system entry.
    pub fn try_add_system_with_descriptor<Params, Marker>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<Params, Marker>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        self.validate_descriptor_identity(&descriptor)?;
        let system = system.into_system()?;
        self.systems.push(ScheduleEntry {
            descriptor,
            system: Box::new(system),
            is_enabled: true,
            default_order: self.systems.len(),
        });
        Ok(self)
    }

    /// Adds one host bridge that selects its accesses from validated runtime
    /// metadata and therefore requires exclusive world access.
    pub(crate) fn try_add_exclusive_system_with_descriptor<F, E>(
        &mut self,
        descriptor: SystemDescriptor,
        system: F,
    ) -> Result<&mut Self, SystemRegistrationError>
    where
        F: FnMut(&mut World) -> Result<(), E> + Send + Sync + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.validate_descriptor_identity(&descriptor)?;
        self.systems.push(ScheduleEntry {
            descriptor,
            system: Box::new(exclusive_system(system)),
            is_enabled: true,
            default_order: self.systems.len(),
        });
        Ok(self)
    }

    /// Runs every enabled system and then applies deferred runtime commands.
    ///
    /// If a system fails, commands queued earlier in the same run are
    /// discarded so they cannot unexpectedly apply later.
    pub fn run(&mut self, world: &mut World) -> Result<(), ScheduleError> {
        for entry in &mut self.systems {
            if !entry.is_enabled {
                continue;
            }
            if let Err(system_error) = entry.system.run(world) {
                return match world.discard_commands() {
                    Ok(()) => Err(ScheduleError::System(system_error)),
                    Err(discard_errors) => Err(ScheduleError::SystemAndCommandDiscard {
                        system: system_error,
                        discard_errors,
                    }),
                };
            }
        }

        world.apply_commands().map_err(ScheduleError::Commands)
    }

    /// Returns the number of systems in this schedule.
    pub fn len(&self) -> usize {
        self.systems.len()
    }

    /// Returns `true` when the schedule has no systems.
    pub fn is_empty(&self) -> bool {
        self.systems.is_empty()
    }

    /// Returns the validated access declaration for each system.
    pub fn system_accesses(&self) -> impl Iterator<Item = &crate::SystemAccess> {
        self.systems.iter().map(|entry| entry.system.access())
    }

    /// Returns a detached metadata snapshot in current execution order.
    pub fn system_infos(&self) -> Vec<ScheduleEntryInfo> {
        self.systems
            .iter()
            .enumerate()
            .map(|(order, entry)| ScheduleEntryInfo {
                descriptor: entry.descriptor.clone(),
                order,
                is_enabled: entry.is_enabled,
            })
            .collect()
    }

    /// Returns `true` when an ID or registered alias resolves in this schedule.
    pub fn contains_system_id(&self, id: &SystemId) -> bool {
        self.systems
            .iter()
            .any(|entry| entry.descriptor.id() == id || entry.descriptor.aliases().contains(id))
    }

    /// Enables or disables an entry without removing its registration.
    pub fn set_enabled(
        &mut self,
        id: &SystemId,
        is_enabled: bool,
    ) -> Result<(), ScheduleEditError> {
        let index = self
            .resolve_id(id)
            .ok_or_else(|| ScheduleEditError::UnknownSystem(id.clone()))?;
        self.systems[index].is_enabled = is_enabled;
        Ok(())
    }

    /// Moves an entry to an exact zero-based position when constraints allow.
    pub fn move_to(&mut self, id: &SystemId, position: usize) -> Result<(), ScheduleEditError> {
        if position >= self.systems.len() {
            return Err(ScheduleEditError::InvalidPosition {
                position,
                len: self.systems.len(),
            });
        }
        let source = self
            .resolve_id(id)
            .ok_or_else(|| ScheduleEditError::UnknownSystem(id.clone()))?;
        let mut preferred: Vec<usize> = (0..self.systems.len()).collect();
        let moved = preferred.remove(source);
        preferred.insert(position, moved);
        let (resolved, _) = self.resolve_order(&preferred)?;
        if resolved != preferred {
            return Err(ScheduleEditError::ConstraintViolation);
        }
        self.reorder(&resolved);
        Ok(())
    }

    /// Moves an entry one position earlier.
    pub fn move_up(&mut self, id: &SystemId) -> Result<(), ScheduleEditError> {
        let position = self
            .resolve_id(id)
            .ok_or_else(|| ScheduleEditError::UnknownSystem(id.clone()))?;
        if position == 0 {
            return Ok(());
        }
        self.move_to(id, position - 1)
    }

    /// Moves an entry one position later.
    pub fn move_down(&mut self, id: &SystemId) -> Result<(), ScheduleEditError> {
        let position = self
            .resolve_id(id)
            .ok_or_else(|| ScheduleEditError::UnknownSystem(id.clone()))?;
        if position + 1 >= self.systems.len() {
            return Ok(());
        }
        self.move_to(id, position + 1)
    }

    /// Merges persisted preferences with current registrations and constraints.
    pub fn apply_configuration(
        &mut self,
        configuration: &ScheduleConfiguration,
    ) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        let mut diagnostics = Vec::new();
        let mut seen = BTreeSet::new();
        let mut preferred = Vec::new();
        for id in &configuration.order {
            match self.resolve_id_with_alias(id) {
                Some((index, alias)) => {
                    if let Some(canonical) = alias {
                        diagnostics.push(ScheduleDiagnostic::MigratedAlias {
                            from: id.clone(),
                            to: canonical,
                        });
                    }
                    if seen.insert(index) {
                        preferred.push(index);
                    }
                }
                None => diagnostics.push(ScheduleDiagnostic::UnknownConfiguredSystem(id.clone())),
            }
        }
        let mut missing: Vec<_> = (0..self.systems.len())
            .filter(|index| !seen.contains(index))
            .collect();
        missing.sort_by_key(|index| self.systems[*index].default_order);
        preferred.extend(missing);

        let (resolved, mut constraint_diagnostics) = self.resolve_order(&preferred)?;
        if resolved != preferred {
            diagnostics.push(ScheduleDiagnostic::ConstraintAdjusted);
        }
        diagnostics.append(&mut constraint_diagnostics);
        self.reorder(&resolved);

        for entry in &mut self.systems {
            entry.is_enabled = true;
        }
        for id in &configuration.disabled {
            match self.resolve_id_with_alias(id) {
                Some((index, alias)) => {
                    if let Some(canonical) = alias {
                        diagnostics.push(ScheduleDiagnostic::MigratedAlias {
                            from: id.clone(),
                            to: canonical,
                        });
                    }
                    self.systems[index].is_enabled = false;
                }
                None => diagnostics.push(ScheduleDiagnostic::UnknownConfiguredSystem(id.clone())),
            }
        }
        Ok(diagnostics)
    }

    /// Restores registration order, applies constraints, and enables entries.
    pub fn reset_to_default(&mut self) -> Result<Vec<ScheduleDiagnostic>, ScheduleEditError> {
        self.apply_configuration(&ScheduleConfiguration::default())
    }

    /// Returns the current persistent configuration using canonical IDs.
    pub fn configuration(&self) -> ScheduleConfiguration {
        ScheduleConfiguration {
            order: self
                .systems
                .iter()
                .filter(|entry| entry.descriptor.is_persistent())
                .map(|entry| entry.descriptor.id().clone())
                .collect(),
            disabled: self
                .systems
                .iter()
                .filter(|entry| !entry.is_enabled && entry.descriptor.is_persistent())
                .map(|entry| entry.descriptor.id().clone())
                .collect(),
        }
    }

    fn validate_descriptor_identity(
        &self,
        descriptor: &SystemDescriptor,
    ) -> Result<(), ScheduleEditError> {
        let mut incoming = BTreeSet::new();
        for id in std::iter::once(descriptor.id()).chain(descriptor.aliases()) {
            if !incoming.insert(id.clone()) || self.contains_system_id(id) {
                return Err(ScheduleEditError::DuplicateId(id.clone()));
            }
        }
        for entry in &self.systems {
            if descriptor.aliases().contains(entry.descriptor.id())
                || entry.descriptor.aliases().contains(descriptor.id())
            {
                return Err(ScheduleEditError::DuplicateId(descriptor.id().clone()));
            }
        }
        Ok(())
    }

    fn resolve_id(&self, id: &SystemId) -> Option<usize> {
        self.resolve_id_with_alias(id).map(|(index, _)| index)
    }

    fn resolve_id_with_alias(&self, id: &SystemId) -> Option<(usize, Option<SystemId>)> {
        self.systems.iter().enumerate().find_map(|(index, entry)| {
            if entry.descriptor.id() == id {
                Some((index, None))
            } else if entry.descriptor.aliases().contains(id) {
                Some((index, Some(entry.descriptor.id().clone())))
            } else {
                None
            }
        })
    }

    fn resolve_order(
        &self,
        preferred: &[usize],
    ) -> Result<(Vec<usize>, Vec<ScheduleDiagnostic>), ScheduleEditError> {
        let mut diagnostics = Vec::new();
        let aliases: BTreeMap<SystemId, usize> = self
            .systems
            .iter()
            .enumerate()
            .flat_map(|(index, entry)| {
                std::iter::once((entry.descriptor.id().clone(), index)).chain(
                    entry
                        .descriptor
                        .aliases()
                        .iter()
                        .cloned()
                        .map(move |id| (id, index)),
                )
            })
            .collect();
        let mut edges = BTreeSet::new();
        for (index, entry) in self.systems.iter().enumerate() {
            for target in entry.descriptor.before() {
                if let Some(target_index) = aliases.get(target) {
                    edges.insert((index, *target_index));
                } else {
                    diagnostics.push(ScheduleDiagnostic::MissingConstraintTarget {
                        system: entry.descriptor.id().clone(),
                        target: target.clone(),
                    });
                }
            }
            for target in entry.descriptor.after() {
                if let Some(target_index) = aliases.get(target) {
                    edges.insert((*target_index, index));
                } else {
                    diagnostics.push(ScheduleDiagnostic::MissingConstraintTarget {
                        system: entry.descriptor.id().clone(),
                        target: target.clone(),
                    });
                }
            }
        }

        let mut outgoing = vec![Vec::new(); self.systems.len()];
        let mut indegree = vec![0_usize; self.systems.len()];
        for (from, to) in edges {
            if from == to {
                return Err(ScheduleEditError::ConstraintCycle {
                    systems: vec![self.systems[from].descriptor.id().clone()],
                });
            }
            outgoing[from].push(to);
            indegree[to] += 1;
        }
        let rank: BTreeMap<usize, usize> = preferred
            .iter()
            .enumerate()
            .map(|(rank, index)| (*index, rank))
            .collect();
        let mut emitted = vec![false; self.systems.len()];
        let mut resolved = Vec::with_capacity(self.systems.len());
        while resolved.len() < self.systems.len() {
            let next = (0..self.systems.len())
                .filter(|index| !emitted[*index] && indegree[*index] == 0)
                .min_by_key(|index| rank.get(index).copied().unwrap_or(usize::MAX));
            let Some(next) = next else {
                let systems = (0..self.systems.len())
                    .filter(|index| !emitted[*index])
                    .map(|index| self.systems[index].descriptor.id().clone())
                    .collect();
                return Err(ScheduleEditError::ConstraintCycle { systems });
            };
            emitted[next] = true;
            resolved.push(next);
            for target in &outgoing[next] {
                indegree[*target] = indegree[*target].saturating_sub(1);
            }
        }
        Ok((resolved, diagnostics))
    }

    fn reorder(&mut self, order: &[usize]) {
        let mut entries: Vec<_> = self.systems.drain(..).map(Some).collect();
        self.systems = order
            .iter()
            .map(|index| {
                entries[*index]
                    .take()
                    .expect("resolved order must contain each entry exactly once")
            })
            .collect();
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Commands;
    use crate::resource::{Res, ResMut};
    use crate::SystemOrigin;

    fn descriptor(id: &str) -> SystemDescriptor {
        SystemDescriptor::new(id, id, SystemOrigin::Engine).unwrap()
    }

    #[test]
    fn schedule_runs_systems_in_order() {
        let mut world = World::new();
        world.insert_resource(0_i32);
        let mut schedule = Schedule::new();
        schedule.add_system(|mut value: ResMut<i32>| *value = 1);
        schedule.add_system(|mut value: ResMut<i32>| *value += 1);

        schedule.run(&mut world).unwrap();

        assert_eq!(world.get_resource::<i32>(), Some(&2));
    }

    #[test]
    fn named_systems_move_and_keep_execution_order() {
        let mut world = World::new();
        world.insert_resource(Vec::<u8>::new());
        let mut schedule = Schedule::new();
        schedule.add_system_with_descriptor(descriptor("engine.a"), |mut log: ResMut<Vec<u8>>| {
            log.push(1)
        });
        schedule.add_system_with_descriptor(descriptor("engine.b"), |mut log: ResMut<Vec<u8>>| {
            log.push(2)
        });
        schedule
            .move_up(&SystemId::try_new("engine.b").unwrap())
            .unwrap();

        schedule.run(&mut world).unwrap();

        assert_eq!(world.get_resource::<Vec<u8>>().unwrap(), &[2, 1]);
    }

    #[test]
    fn named_system_moves_down() {
        let mut schedule = Schedule::new();
        schedule.add_system_with_descriptor(descriptor("engine.a"), || {});
        schedule.add_system_with_descriptor(descriptor("engine.b"), || {});

        schedule
            .move_down(&SystemId::try_new("engine.a").unwrap())
            .unwrap();

        let ids: Vec<_> = schedule
            .system_infos()
            .into_iter()
            .map(|info| info.descriptor.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["engine.b", "engine.a"]);
    }

    #[test]
    fn disabled_system_is_skipped_and_reenabled_in_place() {
        let mut world = World::new();
        world.insert_resource(0_i32);
        let mut schedule = Schedule::new();
        schedule
            .add_system_with_descriptor(descriptor("engine.value"), |mut value: ResMut<i32>| {
                *value += 1
            });
        let id = SystemId::try_new("engine.value").unwrap();
        schedule.set_enabled(&id, false).unwrap();
        schedule.run(&mut world).unwrap();
        assert_eq!(world.get_resource::<i32>(), Some(&0));
        schedule.set_enabled(&id, true).unwrap();
        schedule.run(&mut world).unwrap();
        assert_eq!(world.get_resource::<i32>(), Some(&1));
    }

    #[test]
    fn before_and_after_constraints_use_stable_topological_order() {
        let mut schedule = Schedule::new();
        let a = descriptor("engine.a").try_before("engine.c").unwrap();
        let c = descriptor("engine.c").try_after("engine.a").unwrap();
        schedule.add_system_with_descriptor(a, || {});
        schedule.add_system_with_descriptor(descriptor("engine.b"), || {});
        schedule.add_system_with_descriptor(c, || {});
        let configuration = ScheduleConfiguration {
            order: ["engine.b", "engine.c", "engine.a"]
                .into_iter()
                .map(|id| SystemId::try_new(id).unwrap())
                .collect(),
            disabled: Vec::new(),
        };

        schedule.apply_configuration(&configuration).unwrap();

        let ids: Vec<_> = schedule
            .system_infos()
            .into_iter()
            .map(|info| info.descriptor.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["engine.b", "engine.a", "engine.c"]);
    }

    #[test]
    fn saved_order_migrates_aliases_appends_new_systems_and_reports_removed_ids() {
        let mut schedule = Schedule::new();
        schedule.add_system_with_descriptor(descriptor("engine.a"), || {});
        schedule.add_system_with_descriptor(
            descriptor("engine.b").try_alias("engine.old_b").unwrap(),
            || {},
        );
        schedule.add_system_with_descriptor(descriptor("engine.new"), || {});
        let configuration = ScheduleConfiguration {
            order: ["engine.old_b", "engine.removed", "engine.a"]
                .into_iter()
                .map(|id| SystemId::try_new(id).unwrap())
                .collect(),
            disabled: vec![SystemId::try_new("engine.old_b").unwrap()],
        };

        let diagnostics = schedule.apply_configuration(&configuration).unwrap();

        let infos = schedule.system_infos();
        let ids: Vec<_> = infos
            .iter()
            .map(|info| info.descriptor.id().as_str())
            .collect();
        assert_eq!(ids, ["engine.b", "engine.a", "engine.new"]);
        assert!(!infos[0].is_enabled);
        assert!(diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            ScheduleDiagnostic::MigratedAlias { from, to }
                if from.as_str() == "engine.old_b" && to.as_str() == "engine.b"
        )));
        assert!(diagnostics.iter().any(|diagnostic| matches!(
            diagnostic,
            ScheduleDiagnostic::UnknownConfiguredSystem(id)
                if id.as_str() == "engine.removed"
        )));
    }

    #[test]
    fn cycle_and_duplicate_ids_are_rejected() {
        let mut schedule = Schedule::new();
        schedule.add_system_with_descriptor(
            descriptor("engine.a").try_after("engine.b").unwrap(),
            || {},
        );
        schedule.add_system_with_descriptor(
            descriptor("engine.b").try_after("engine.a").unwrap(),
            || {},
        );
        assert!(matches!(
            schedule.reset_to_default(),
            Err(ScheduleEditError::ConstraintCycle { .. })
        ));
        assert!(matches!(
            schedule.try_add_system_with_descriptor(descriptor("engine.a"), || {}),
            Err(SystemRegistrationError::Schedule(
                ScheduleEditError::DuplicateId(_)
            ))
        ));
    }

    #[test]
    fn unknown_operations_fail_without_mutating_the_schedule() {
        let mut schedule = Schedule::new();
        schedule.add_system_with_descriptor(descriptor("engine.a"), || {});
        let unknown = SystemId::try_new("engine.missing").unwrap();
        assert!(matches!(
            schedule.move_down(&unknown),
            Err(ScheduleEditError::UnknownSystem(_))
        ));
        assert_eq!(
            schedule.system_infos()[0].descriptor.id().as_str(),
            "engine.a"
        );
    }

    #[test]
    fn system_failure_discards_queued_entity_spawn() {
        struct MissingResource;

        let mut world = World::new();
        let mut schedule = Schedule::new();
        schedule.add_system(|mut commands: Commands| {
            commands.spawn();
        });
        schedule.add_system(|_: Res<MissingResource>| {});

        assert!(schedule.run(&mut world).is_err());
        assert_eq!(world.entity_count(), 0);
        let entity = world.spawn().unwrap();
        assert_eq!(entity.id(), 1);
        assert_eq!(entity.generation(), 1);
    }
}
