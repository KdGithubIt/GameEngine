//! Host-side compiler for query-scoped project Rust callback input.
//!
//! The compiler walks live entities immediately before a callback and copies
//! only the project components, engine views, actions, resources, and events
//! declared by that system. This replaces the whole-project snapshot behavior
//! incrementally without exposing [`engine_ecs::World`] across the ABI.

use crate::animation::{AnimationEvents, Animator, AnimatorState};
use crate::behavior_tree::{BehaviorDispatchKind, BehaviorStatus, BehaviorTreeRunner};
use crate::character_controller::KinematicCharacterController;
use crate::collision::{CollisionEvents, CollisionPhase};
use crate::combat::{DamageReceiver, HitResults};
use crate::game_io::{
    validate_game_input_bytes, validate_game_output_bytes, EngineViewKind, GameAccessError,
    GameAccessMode, GameClock, GameCommand, GameCommandFamily, GameEntityHandle, GameEventEmission,
    GameEventRecord, GameEventStream, GameHostViewKind, GameInputActionState, GameInvocation,
    GameInvocationOutput, GameIoLimitError, GameQueryResult, GameQueryRow, GameSystemAccess,
    GAME_IO_SCHEMA_VERSION, MAX_GAME_EVENT_RECORDS,
};
use crate::game_module::GameComponentStore;
use crate::game_prefab::GamePrefabEvents;
use crate::game_timer::GameTimerEvents;
use crate::hitbox::AttackHitbox;
use crate::lock_on::TargetLock;
use crate::navmesh::NavMeshAgent;
use crate::player::InputActionMap;
use crate::runtime_metadata::RuntimeMetadata;
use crate::save::{SaveData, SaveValue};
use crate::scene_manager::{SceneManager, SceneSwitchState};
use crate::script_api::RuntimeEntityIdentity;
use crate::time::{FixedTime, Time};
use crate::transform::{GlobalTransform, Transform};
use crate::ui_document::{UiBindingValue, UiBindings, UiEventFrame};
use engine_authoring::Value;
use engine_ecs::{Entity, SystemId, SystemIdError, World};
use serde_json::Error as JsonError;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::time::Duration;

/// Frame-scoped host data available while compiling one project invocation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameHostFrame {
    /// Current rendered-frame and fixed-step clocks.
    pub clock: GameClock,
    /// Resolved Project Settings actions keyed by action name.
    pub input_actions: BTreeMap<String, GameInputActionState>,
    /// Active-save values explicitly declared by the current system.
    pub save_values: BTreeMap<String, Value>,
    /// Host-owned project resources keyed by stable resource ID.
    pub resources: BTreeMap<String, Value>,
    /// Unconsumed host and project event records.
    pub events: Vec<GameEventRecord>,
}

/// Refreshes the clock and only the input actions declared by one system.
///
/// Runtime-owned resources and queued events are preserved. Calling this
/// immediately before each callback guarantees Editor Play and Player observe
/// the same current input transitions and fixed-step counter.
pub(crate) fn refresh_game_host_frame(
    world: &World,
    access: &GameSystemAccess,
    frame: &mut GameHostFrame,
) {
    let time = world.get_resource::<Time>();
    let fixed_time = world.get_resource::<FixedTime>();
    frame.clock = GameClock {
        delta_seconds: time.map_or(0.0, |time| time.delta_seconds),
        fixed_delta_seconds: fixed_time.map_or(0.0, |time| time.fixed_delta),
        elapsed_seconds: time.map_or(0.0, |time| f64::from(time.elapsed_seconds)),
        frame_index: time.map_or(0, |time| time.frame_count),
        fixed_step_index: fixed_time.map_or(0, |time| time.step_count),
    };
    frame.input_actions.clear();
    let action_map = world.get_resource::<InputActionMap>();
    frame
        .input_actions
        .extend(access.input_actions.iter().map(|name| {
            let state = action_map
                .map(|map| map.resolve_action(name, world))
                .unwrap_or_default();
            (name.clone(), state)
        }));
    frame.save_values.clear();
    if let Some(save_data) = world.get_resource::<SaveData>() {
        frame
            .save_values
            .extend(access.save_keys.iter().filter_map(|key| {
                save_data
                    .get(key)
                    .map(|value| (key.clone(), save_value_to_authoring(value)))
            }));
    }
}

/// Compiles the bounded callback input for one declared project system.
///
/// Entity rows are ordered by runtime `(id, generation)` for repeatable tests
/// and diagnostics. Runtime order must still never be persisted as authoring
/// identity.
///
/// # Errors
///
/// Returns [`GameHostCompileError`] when access metadata is invalid, a required
/// game resource is missing, a requested view is not supported yet, the row or
/// encoded-byte cap is exceeded, or deterministic JSON encoding fails.
pub fn compile_game_invocation(
    world: &World,
    system_id: &str,
    access: &GameSystemAccess,
    frame: &GameHostFrame,
) -> Result<GameInvocation, GameHostCompileError> {
    access.validate().map_err(GameHostCompileError::Access)?;

    let input_actions = compile_input_actions(access, frame);
    let save_values = access
        .save_keys
        .iter()
        .filter_map(|key| {
            frame
                .save_values
                .get(key)
                .cloned()
                .map(|value| (key.clone(), value))
        })
        .collect();
    let resources = compile_resources(access, frame)?;
    let host_views = compile_host_views(world, access);
    let events = compile_events(access, frame);

    let mut entities: Vec<Entity> = world.entities().collect();
    entities.sort_by_key(|entity| (entity.id(), entity.generation()));
    let queries = access
        .queries
        .iter()
        .map(|query| compile_query(world, &entities, query))
        .collect::<Result<Vec<_>, _>>()?;

    let invocation = GameInvocation {
        schema_version: GAME_IO_SCHEMA_VERSION,
        system_id: system_id.to_owned(),
        clock: frame.clock,
        input_actions,
        save_values,
        queries,
        resources,
        host_views,
        events,
    };
    invocation
        .validate_collection_limits()
        .map_err(GameHostCompileError::Limit)?;
    let encoded = serde_json::to_vec(&invocation).map_err(GameHostCompileError::Serialize)?;
    validate_game_input_bytes(encoded.len()).map_err(GameHostCompileError::Limit)?;
    Ok(invocation)
}

fn save_value_to_authoring(value: &SaveValue) -> Value {
    match value {
        SaveValue::Text(value) => Value::String(value.clone()),
        SaveValue::Number(value) => Value::F64(*value),
        SaveValue::Flag(value) => Value::Bool(*value),
    }
}

fn compile_input_actions(
    access: &GameSystemAccess,
    frame: &GameHostFrame,
) -> BTreeMap<String, GameInputActionState> {
    access
        .input_actions
        .iter()
        .map(|name| {
            (
                name.clone(),
                frame.input_actions.get(name).copied().unwrap_or_default(),
            )
        })
        .collect()
}

fn compile_resources(
    access: &GameSystemAccess,
    frame: &GameHostFrame,
) -> Result<BTreeMap<String, Value>, GameHostCompileError> {
    access
        .resources
        .iter()
        .map(|declared| {
            frame
                .resources
                .get(&declared.id)
                .cloned()
                .map(|resource| (declared.id.clone(), resource))
                .ok_or_else(|| GameHostCompileError::MissingResource(declared.id.clone()))
        })
        .collect()
}

fn compile_host_views(
    world: &World,
    access: &GameSystemAccess,
) -> BTreeMap<GameHostViewKind, Value> {
    access
        .host_views
        .iter()
        .map(|view| {
            let value = match view {
                GameHostViewKind::SceneState => scene_state_value(world),
            };
            (*view, value)
        })
        .collect()
}

fn scene_state_value(world: &World) -> Value {
    let manager = world.get_resource::<SceneManager>();
    let (status, failure_path, failure_message) = match world.get_resource::<SceneSwitchState>() {
        Some(SceneSwitchState::Failed { path, message }) => (
            "failed",
            Value::String(path.clone()),
            Value::String(message.clone()),
        ),
        Some(SceneSwitchState::Idle) | None => ("idle", Value::Null, Value::Null),
    };
    Value::Object(BTreeMap::from([
        (
            "current_path".to_owned(),
            manager
                .and_then(SceneManager::current_scene_path)
                .map_or(Value::Null, |path| Value::String(path.to_owned())),
        ),
        ("failure_message".to_owned(), failure_message),
        ("failure_path".to_owned(), failure_path),
        (
            "generation".to_owned(),
            Value::String(manager.map_or(0, SceneManager::generation).to_string()),
        ),
        (
            "pending_path".to_owned(),
            manager
                .and_then(SceneManager::pending_scene_path)
                .map_or(Value::Null, |path| Value::String(path.to_owned())),
        ),
        ("status".to_owned(), Value::String(status.to_owned())),
    ]))
}

fn compile_events(access: &GameSystemAccess, frame: &GameHostFrame) -> Vec<GameEventRecord> {
    let event_streams = access
        .event_streams
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    frame
        .events
        .iter()
        .filter(|event| event_streams.contains(&event.stream))
        .cloned()
        .collect()
}

/// Commands and events released for schedule-boundary processing after patches.
#[derive(Debug, Clone, PartialEq)]
pub struct GameDeferredEffects {
    /// Validated commands retained in callback order.
    pub commands: Vec<GameCommand>,
    /// Validated project events retained in callback order.
    pub emitted_events: Vec<GameEventEmission>,
    /// Validated highest consumed sequence for each declared event stream.
    pub consumed_event_sequences: BTreeMap<GameEventStream, u64>,
}

/// Host-owned state shared by every ABI v3 project-system invocation.
///
/// Runtime hosts update [`Self::frame`] before schedules run. Scoped callback
/// outputs append to the deferred queue so engine services can process them at
/// a schedule boundary without allowing the dynamic module to mutate ECS or
/// service resources re-entrantly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GameHostRuntime {
    frame: GameHostFrame,
    /// Bounded, host-owned history shared by subscribers. Consumption is
    /// tracked separately per project system so one subscriber cannot steal
    /// another subscriber's event.
    event_log: VecDeque<GameEventRecord>,
    next_event_sequences: BTreeMap<GameEventStream, u64>,
    consumed_event_sequences: BTreeMap<String, BTreeMap<GameEventStream, u64>>,
    last_collision_generation: Option<u64>,
    last_hit_generation: Option<u64>,
    last_animation_generation: Option<u64>,
    last_ui_generation: Option<u64>,
    last_timer_source_sequence: u64,
    last_spawn_source_sequence: u64,
    last_scene_observation: Option<SceneObservation>,
    deferred_effects: Vec<GameDeferredEffects>,
    metrics: BTreeMap<String, GameSystemMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SceneObservation {
    generation: u64,
    path: Option<String>,
    failure: Option<(String, String)>,
}

/// Aggregated profiler data for one project Rust system in the current Play run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameSystemMetrics {
    /// Number of attempted invocations, including rejected ones.
    pub invocation_count: u64,
    /// Number of invocations that failed before atomic output application.
    pub failure_count: u64,
    /// Sum of time spent inside the dynamic-library callback.
    pub total_callback_time: Duration,
    /// Most recent dynamic-library callback duration.
    pub last_callback_time: Duration,
    /// Input bytes transferred by the most recent successful invocation.
    pub last_input_bytes: usize,
    /// Output bytes transferred by the most recent successful invocation.
    pub last_output_bytes: usize,
    /// Total query rows copied by the most recent successful invocation.
    pub last_query_rows: usize,
    /// Deferred commands produced by the most recent successful invocation.
    pub last_command_count: usize,
    /// Most recent failure message, cleared by the next successful invocation.
    pub latest_error: Option<String>,
}

impl GameHostRuntime {
    /// Returns the frame data used to compile the next callback invocation.
    pub fn frame(&self) -> &GameHostFrame {
        &self.frame
    }

    /// Replaces clock, resolved actions, resources, and pending input events.
    pub fn set_frame(&mut self, mut frame: GameHostFrame) {
        self.event_log = frame
            .events
            .iter()
            .rev()
            .take(MAX_GAME_EVENT_RECORDS)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.next_event_sequences.clear();
        for event in &self.event_log {
            let next = self.next_event_sequences.entry(event.stream).or_insert(1);
            *next = (*next).max(event.sequence.saturating_add(1));
        }
        frame.events = self.event_log.iter().cloned().collect();
        self.frame = frame;
    }

    /// Drains validated effects accumulated by successful callbacks.
    pub fn drain_deferred_effects(&mut self) -> Vec<GameDeferredEffects> {
        std::mem::take(&mut self.deferred_effects)
    }

    /// Iterates per-system profiler records in stable system-ID order.
    pub fn metrics(&self) -> impl Iterator<Item = (&str, &GameSystemMetrics)> {
        self.metrics
            .iter()
            .map(|(system_id, metrics)| (system_id.as_str(), metrics))
    }

    /// Returns profiler data for one stable project-system ID.
    pub fn system_metrics(&self, system_id: &str) -> Option<&GameSystemMetrics> {
        self.metrics.get(system_id)
    }

    pub(crate) fn frame_mut(&mut self) -> &mut GameHostFrame {
        &mut self.frame
    }

    /// Starts one module generation from its declared runtime-only defaults.
    ///
    /// Values intentionally reset when the retained native module changes.
    /// Persistence must use explicit save commands instead of accidentally
    /// carrying Rust gameplay state across Play generations.
    pub(crate) fn replace_module_resources(&mut self, resources: BTreeMap<String, Value>) {
        self.frame.resources = resources;
    }

    pub(crate) fn refresh_for_system(
        &mut self,
        world: &World,
        system_id: &str,
        access: &GameSystemAccess,
    ) {
        refresh_game_host_frame(world, access, &mut self.frame);
        self.capture_host_events(world, access);

        let cursors = self
            .consumed_event_sequences
            .get(system_id)
            .cloned()
            .unwrap_or_default();
        self.frame.events = self
            .event_log
            .iter()
            .filter(|event| {
                event.sequence > cursors.get(&event.stream).copied().unwrap_or_default()
            })
            .cloned()
            .collect();
    }

    pub(crate) fn accept_effects(&mut self, system_id: &str, mut effects: GameDeferredEffects) {
        let cursors = self
            .consumed_event_sequences
            .entry(system_id.to_owned())
            .or_default();
        for (stream, sequence) in std::mem::take(&mut effects.consumed_event_sequences) {
            let cursor = cursors.entry(stream).or_default();
            *cursor = (*cursor).max(sequence);
        }
        for event in std::mem::take(&mut effects.emitted_events) {
            self.push_event(GameEventStream::Game, game_event_value(event));
        }
        if !effects.commands.is_empty() {
            self.deferred_effects.push(effects);
        }
    }

    fn capture_host_events(&mut self, world: &World, access: &GameSystemAccess) {
        // Only touch producer snapshots declared by at least one callback.
        // Besides avoiding copies for unused services, this prevents busy
        // collision streams from evicting unrelated game/UI events in a
        // project that has no Rust collision subscriber.
        if access.event_streams.contains(&GameEventStream::Collision) {
            self.capture_collision_events(world);
        }
        if access.event_streams.contains(&GameEventStream::Hit) {
            self.capture_hit_events(world);
        }
        if access.event_streams.contains(&GameEventStream::Animation) {
            self.capture_animation_events(world);
        }
        if access.event_streams.contains(&GameEventStream::Ui) {
            self.capture_ui_events(world);
        }
        if access.event_streams.contains(&GameEventStream::Scene) {
            self.capture_scene_events(world);
        }
        if access.event_streams.contains(&GameEventStream::Timer) {
            self.capture_timer_events(world);
        }
        if access.event_streams.contains(&GameEventStream::SpawnResult) {
            self.capture_spawn_events(world);
        }
    }

    fn capture_collision_events(&mut self, world: &World) {
        let Some(events) = world.get_resource::<CollisionEvents>() else {
            return;
        };
        let generation = events.generation();
        if self.last_collision_generation == Some(generation) {
            return;
        }
        self.last_collision_generation = Some(generation);

        let records = events
            .transitions()
            .map(|transition| {
                let phase = match transition.phase {
                    CollisionPhase::Enter => "enter",
                    CollisionPhase::Stay => "stay",
                    CollisionPhase::Exit => "exit",
                };
                collision_event_value(phase, &transition.contact)
            })
            .collect::<Vec<_>>();
        for payload in records {
            self.push_event(GameEventStream::Collision, payload);
        }
    }

    fn capture_hit_events(&mut self, world: &World) {
        let Some(results) = world.get_resource::<HitResults>() else {
            return;
        };
        let generation = results.generation();
        if self.last_hit_generation == Some(generation) {
            return;
        }
        self.last_hit_generation = Some(generation);
        let records = results
            .iter()
            .map(|hit| {
                Value::Object(BTreeMap::from([
                    ("attacker".to_owned(), entity_handle_value(hit.attacker)),
                    ("hitbox".to_owned(), entity_handle_value(hit.hitbox)),
                    ("target".to_owned(), entity_handle_value(hit.target)),
                    ("damage".to_owned(), Value::F64(f64::from(hit.damage))),
                    (
                        "remaining_health".to_owned(),
                        Value::F64(f64::from(hit.remaining_health)),
                    ),
                    (
                        "activation".to_owned(),
                        Value::String(hit.activation.to_string()),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        for payload in records {
            self.push_event(GameEventStream::Hit, payload);
        }
    }

    fn capture_animation_events(&mut self, world: &World) {
        let Some(events) = world.get_resource::<AnimationEvents>() else {
            return;
        };
        let generation = events.generation();
        if self.last_animation_generation == Some(generation) {
            return;
        }
        self.last_animation_generation = Some(generation);
        let records = events
            .iter()
            .map(|event| {
                Value::Object(BTreeMap::from([
                    ("entity".to_owned(), entity_handle_value(event.entity)),
                    ("name".to_owned(), Value::String(event.name.clone())),
                ]))
            })
            .collect::<Vec<_>>();
        for payload in records {
            self.push_event(GameEventStream::Animation, payload);
        }
    }

    fn capture_ui_events(&mut self, world: &World) {
        let Some(events) = world.get_resource::<UiEventFrame>() else {
            return;
        };
        let generation = events.generation();
        if self.last_ui_generation == Some(generation) {
            return;
        }
        self.last_ui_generation = Some(generation);
        let records = events.events().to_vec();
        for name in records {
            self.push_event(
                GameEventStream::Ui,
                Value::Object(BTreeMap::from([("name".to_owned(), Value::String(name))])),
            );
        }
    }

    fn capture_scene_events(&mut self, world: &World) {
        let Some(manager) = world.get_resource::<SceneManager>() else {
            return;
        };
        let failure = match world.get_resource::<SceneSwitchState>() {
            Some(SceneSwitchState::Failed { path, message }) => {
                Some((path.clone(), message.clone()))
            }
            Some(SceneSwitchState::Idle) | None => None,
        };
        let observation = SceneObservation {
            generation: manager.generation(),
            path: manager.current_scene_path().map(str::to_owned),
            failure,
        };

        let payload = match &self.last_scene_observation {
            Some(previous) if observation.generation != previous.generation => {
                Some(Value::Object(BTreeMap::from([
                    ("status".to_owned(), Value::String("completed".to_owned())),
                    (
                        "path".to_owned(),
                        observation.path.clone().map_or(Value::Null, Value::String),
                    ),
                    ("generation".to_owned(), Value::U64(observation.generation)),
                ])))
            }
            Some(previous) if observation.failure != previous.failure => observation
                .failure
                .as_ref()
                .map(|(path, message)| scene_failure_value(path, message)),
            None => observation
                .failure
                .as_ref()
                .map(|(path, message)| scene_failure_value(path, message)),
            _ => None,
        };
        self.last_scene_observation = Some(observation);
        if let Some(payload) = payload {
            self.push_event(GameEventStream::Scene, payload);
        }
    }

    fn capture_timer_events(&mut self, world: &World) {
        let Some(events) = world.get_resource::<GameTimerEvents>() else {
            return;
        };
        let records = events
            .iter()
            .filter(|event| event.source_sequence > self.last_timer_source_sequence)
            .cloned()
            .collect::<Vec<_>>();
        for event in records {
            self.last_timer_source_sequence =
                self.last_timer_source_sequence.max(event.source_sequence);
            self.push_event(GameEventStream::Timer, event.payload);
        }
    }

    fn capture_spawn_events(&mut self, world: &World) {
        let Some(events) = world.get_resource::<GamePrefabEvents>() else {
            return;
        };
        let records = events
            .iter()
            .filter(|event| event.source_sequence > self.last_spawn_source_sequence)
            .cloned()
            .collect::<Vec<_>>();
        for event in records {
            self.last_spawn_source_sequence =
                self.last_spawn_source_sequence.max(event.source_sequence);
            self.push_event(GameEventStream::SpawnResult, event.payload);
        }
    }

    fn push_event(&mut self, stream: GameEventStream, payload: Value) {
        if self.event_log.len() >= MAX_GAME_EVENT_RECORDS {
            let dropped = self
                .event_log
                .pop_front()
                .expect("a full event log must contain a front record");
            log::warn!(
                "GameHost event log is full ({MAX_GAME_EVENT_RECORDS} records); dropping {:?} sequence {}",
                dropped.stream,
                dropped.sequence
            );
        }
        let sequence = self.next_event_sequences.entry(stream).or_insert(1);
        self.event_log.push_back(GameEventRecord {
            stream,
            sequence: *sequence,
            payload,
        });
        *sequence = sequence.saturating_add(1);
    }

    pub(crate) fn record_success(
        &mut self,
        system_id: &str,
        callback_time: Duration,
        input_bytes: usize,
        output_bytes: usize,
        query_rows: usize,
        command_count: usize,
    ) {
        let metrics = self.metrics.entry(system_id.to_owned()).or_default();
        metrics.invocation_count = metrics.invocation_count.saturating_add(1);
        metrics.total_callback_time = metrics.total_callback_time.saturating_add(callback_time);
        metrics.last_callback_time = callback_time;
        metrics.last_input_bytes = input_bytes;
        metrics.last_output_bytes = output_bytes;
        metrics.last_query_rows = query_rows;
        metrics.last_command_count = command_count;
        metrics.latest_error = None;
    }

    pub(crate) fn record_failure(&mut self, system_id: &str, error: &impl fmt::Display) {
        let metrics = self.metrics.entry(system_id.to_owned()).or_default();
        metrics.invocation_count = metrics.invocation_count.saturating_add(1);
        metrics.failure_count = metrics.failure_count.saturating_add(1);
        metrics.latest_error = Some(error.to_string());
    }
}

/// Validates and atomically applies writable callback output.
///
/// Component and resource patches are validated in full before the first
/// mutation. Deferred service commands and emitted events are returned to the
/// caller because their schedule-boundary processors own those side effects.
///
/// # Errors
///
/// Returns [`GameHostApplyError`] without applying any patch when output uses
/// the wrong schema, exceeds a cap, writes undeclared data, repeats a patch,
/// targets a stale entity, references a missing value, emits an unauthorized
/// command/event, acknowledges an undeclared stream, or cannot be encoded for
/// the byte-cap check.
pub fn apply_game_output(
    world: &mut World,
    access: &GameSystemAccess,
    invocation: &GameInvocation,
    resources: &mut BTreeMap<String, Value>,
    output: GameInvocationOutput,
) -> Result<GameDeferredEffects, GameHostApplyError> {
    access.validate().map_err(GameHostApplyError::Access)?;
    if output.schema_version != GAME_IO_SCHEMA_VERSION {
        return Err(GameHostApplyError::SchemaVersion {
            expected: GAME_IO_SCHEMA_VERSION,
            found: output.schema_version,
        });
    }
    output
        .validate_collection_limits()
        .map_err(GameHostApplyError::Limit)?;
    let encoded = serde_json::to_vec(&output).map_err(GameHostApplyError::Serialize)?;
    validate_game_output_bytes(encoded.len()).map_err(GameHostApplyError::Limit)?;

    let authorized = AuthorizedOutput::new(access, invocation);

    let mut component_targets = Vec::new();
    let mut seen_component_patches = BTreeSet::new();
    for patch in &output.component_patches {
        if !authorized
            .writable_component_targets
            .contains(&(patch.entity, patch.component_type.clone()))
        {
            return Err(GameHostApplyError::UnauthorizedComponentTarget {
                entity: patch.entity,
                component_type: patch.component_type.clone(),
            });
        }
        let entity = validate_live_entity(world, patch.entity)?;
        if !seen_component_patches.insert((patch.entity, patch.component_type.clone())) {
            return Err(GameHostApplyError::DuplicateComponentPatch {
                entity: patch.entity,
                component_type: patch.component_type.clone(),
            });
        }
        let store = world
            .get_component::<GameComponentStore>(entity)
            .ok_or(GameHostApplyError::MissingComponentStore(patch.entity))?;
        if store.value(&patch.component_type).is_none() {
            return Err(GameHostApplyError::MissingComponent {
                entity: patch.entity,
                component_type: patch.component_type.clone(),
            });
        }
        component_targets.push(entity);
    }

    let mut seen_resource_patches = BTreeSet::new();
    for patch in &output.resource_patches {
        if !authorized
            .writable_resources
            .contains(patch.resource_id.as_str())
        {
            return Err(GameHostApplyError::UnauthorizedResource(
                patch.resource_id.clone(),
            ));
        }
        if !seen_resource_patches.insert(patch.resource_id.clone()) {
            return Err(GameHostApplyError::DuplicateResourcePatch(
                patch.resource_id.clone(),
            ));
        }
        if !resources.contains_key(&patch.resource_id) {
            return Err(GameHostApplyError::MissingResource(
                patch.resource_id.clone(),
            ));
        }
    }

    for command in &output.commands {
        if !authorized.command_families.contains(&command.family) {
            return Err(GameHostApplyError::UnauthorizedCommand(command.family));
        }
        if let Some(target) = command.target {
            validate_live_entity(world, target)?;
        }
    }
    if !output.emitted_events.is_empty()
        && !authorized
            .command_families
            .contains(&GameCommandFamily::GameEvent)
    {
        return Err(GameHostApplyError::UnauthorizedCommand(
            GameCommandFamily::GameEvent,
        ));
    }
    for event in &output.emitted_events {
        SystemId::try_new(event.event_id.clone()).map_err(|source| {
            GameHostApplyError::InvalidEventId {
                id: event.event_id.clone(),
                source,
            }
        })?;
        if let Some(target) = event.target {
            validate_live_entity(world, target)?;
        }
    }
    for stream in output.consumed_event_sequences.keys() {
        if !authorized.event_streams.contains(stream) {
            return Err(GameHostApplyError::UnauthorizedEventStream(*stream));
        }
    }
    for (stream, sequence) in &output.consumed_event_sequences {
        let highest_delivered = invocation
            .events
            .iter()
            .filter(|event| event.stream == *stream)
            .map(|event| event.sequence)
            .max();
        if highest_delivered.is_none_or(|highest| *sequence > highest) {
            return Err(GameHostApplyError::InvalidEventCursor {
                stream: *stream,
                sequence: *sequence,
                highest_delivered,
            });
        }
    }

    for (patch, entity) in output.component_patches.iter().zip(component_targets) {
        let store = world
            .get_component_mut::<GameComponentStore>(entity)
            .expect("validated component store must remain present during atomic output apply");
        store.insert_runtime_value(patch.component_type.clone(), patch.value.clone());
    }
    for patch in &output.resource_patches {
        let resource = resources
            .get_mut(&patch.resource_id)
            .expect("validated game resource must remain present during atomic output apply");
        *resource = patch.value.clone();
    }

    Ok(GameDeferredEffects {
        commands: output.commands,
        emitted_events: output.emitted_events,
        consumed_event_sequences: output.consumed_event_sequences,
    })
}

struct AuthorizedOutput<'a> {
    writable_component_targets: BTreeSet<(GameEntityHandle, engine_authoring::ComponentTypeId)>,
    writable_resources: BTreeSet<&'a str>,
    command_families: BTreeSet<GameCommandFamily>,
    event_streams: BTreeSet<GameEventStream>,
}

impl<'a> AuthorizedOutput<'a> {
    fn new(access: &'a GameSystemAccess, invocation: &GameInvocation) -> Self {
        let query_rows = invocation
            .queries
            .iter()
            .map(|query| (query.query_id.as_str(), query.rows.as_slice()))
            .collect::<BTreeMap<_, _>>();
        let writable_component_targets = access
            .queries
            .iter()
            .flat_map(|query| {
                let rows = query_rows
                    .get(query.id.as_str())
                    .copied()
                    .unwrap_or_default();
                query
                    .components
                    .iter()
                    .filter(|component| component.mode == GameAccessMode::Write)
                    .flat_map(move |component| {
                        rows.iter()
                            .map(move |row| (row.entity, component.component_type.clone()))
                    })
            })
            .collect();
        Self {
            writable_component_targets,
            writable_resources: access
                .resources
                .iter()
                .filter(|resource| resource.mode == GameAccessMode::Write)
                .map(|resource| resource.id.as_str())
                .collect(),
            command_families: access.command_families.iter().copied().collect(),
            event_streams: access.event_streams.iter().copied().collect(),
        }
    }
}

fn validate_live_entity(
    world: &World,
    handle: GameEntityHandle,
) -> Result<Entity, GameHostApplyError> {
    let entity = Entity::from_raw(handle.id, handle.generation);
    if world.contains_entity(entity) {
        Ok(entity)
    } else {
        Err(GameHostApplyError::StaleEntity(handle))
    }
}

fn compile_query(
    world: &World,
    entities: &[Entity],
    query: &crate::game_io::GameQueryAccess,
) -> Result<GameQueryResult, GameHostCompileError> {
    let mut rows = Vec::new();
    for entity in entities {
        let component_store = world.get_component::<GameComponentStore>(*entity);
        let mut components = BTreeMap::new();
        let mut matches = true;
        for access in &query.components {
            let component = component_store.and_then(|store| store.value(&access.component_type));
            match component {
                Some(component) => {
                    components.insert(access.component_type.clone(), component.clone());
                }
                None if access.required => {
                    matches = false;
                    break;
                }
                None => {}
            }
        }
        if !matches {
            continue;
        }

        let mut authoring_id = None;
        let mut engine_views = BTreeMap::new();
        for access in &query.engine_views {
            let copied = copy_engine_view(world, *entity, access.view);
            match copied {
                Some(EngineViewCopy { value, identity }) => {
                    if identity.is_some() {
                        authoring_id = identity;
                    }
                    engine_views.insert(access.view, value);
                }
                None if access.required => {
                    matches = false;
                    break;
                }
                None => {}
            }
        }
        if matches {
            rows.push(GameQueryRow {
                entity: GameEntityHandle {
                    id: entity.id(),
                    generation: entity.generation(),
                },
                authoring_id,
                components,
                engine_views,
            });
        }
    }
    Ok(GameQueryResult {
        query_id: query.id.clone(),
        rows,
    })
}

struct EngineViewCopy {
    value: Value,
    identity: Option<engine_authoring::EntityId>,
}

fn copy_engine_view(world: &World, entity: Entity, view: EngineViewKind) -> Option<EngineViewCopy> {
    let value = match view {
        EngineViewKind::AuthoringIdentity => {
            let identity = world.get_component::<RuntimeEntityIdentity>(entity)?;
            let metadata = world.get_component::<RuntimeMetadata>(entity);
            let name = metadata
                .map(|metadata| metadata.name.clone())
                .unwrap_or_else(|| identity.name.clone());
            let tags = metadata
                .map(|metadata| {
                    Value::Array(metadata.tags.iter().cloned().map(Value::String).collect())
                })
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let team = metadata
                .map(|metadata| metadata.team.clone())
                .unwrap_or_default();
            return Some(EngineViewCopy {
                value: Value::Object(BTreeMap::from([
                    (
                        "id".to_owned(),
                        Value::String(identity.authoring_id.as_str().to_owned()),
                    ),
                    ("name".to_owned(), Value::String(name)),
                    ("tags".to_owned(), tags),
                    ("team".to_owned(), Value::String(team)),
                ])),
                identity: Some(identity.authoring_id.clone()),
            });
        }
        EngineViewKind::Transform => transform_value(world.get_component::<Transform>(entity)?),
        EngineViewKind::GlobalTransform => {
            let global = world.get_component::<GlobalTransform>(entity)?;
            let (scale, rotation, translation) = global.matrix().to_scale_rotation_translation();
            transform_parts_value(
                translation.to_array(),
                rotation.to_array(),
                scale.to_array(),
            )
        }
        EngineViewKind::CharacterState => {
            let controller = world.get_component::<KinematicCharacterController>(entity)?;
            let facing = world
                .get_component::<Transform>(entity)
                .map(|transform| transform.rotation * glam::Vec3::NEG_Z);
            Value::Object(BTreeMap::from([
                (
                    "velocity".to_owned(),
                    vec3_value(controller.velocity.to_array()),
                ),
                ("grounded".to_owned(), Value::Bool(controller.grounded)),
                (
                    "facing".to_owned(),
                    facing.map_or(Value::Null, |facing| vec3_value(facing.to_array())),
                ),
                (
                    "gravity_scale".to_owned(),
                    Value::F64(f64::from(controller.gravity_scale)),
                ),
            ]))
        }
        EngineViewKind::AnimationState => {
            let animator = world.get_component::<Animator>(entity)?;
            let state = match animator.state {
                AnimatorState::Stopped => "stopped",
                AnimatorState::Playing => "playing",
                AnimatorState::Paused => "paused",
            };
            Value::Object(BTreeMap::from([
                (
                    "clip_runtime_id".to_owned(),
                    Value::String(animator.clip.id().value().to_string()),
                ),
                ("state".to_owned(), Value::String(state.to_owned())),
                ("time".to_owned(), Value::F64(f64::from(animator.time))),
                ("looping".to_owned(), Value::Bool(animator.looping)),
                ("fading".to_owned(), Value::Bool(animator.is_fading())),
            ]))
        }
        EngineViewKind::LockOnState => {
            let lock = world.get_resource::<TargetLock>()?;
            let current = lock.current();
            Value::Object(BTreeMap::from([
                (
                    "current".to_owned(),
                    current.map_or(Value::Null, entity_handle_value),
                ),
                (
                    "is_current_target".to_owned(),
                    Value::Bool(current == Some(entity)),
                ),
            ]))
        }
        EngineViewKind::HitboxState => {
            let hitbox = world.get_component::<AttackHitbox>(entity)?;
            Value::Object(BTreeMap::from([
                ("owner".to_owned(), entity_handle_value(hitbox.owner)),
                ("team".to_owned(), Value::I64(i64::from(hitbox.team))),
                ("damage".to_owned(), Value::F64(f64::from(hitbox.damage))),
                (
                    "one_hit_per_target".to_owned(),
                    Value::Bool(hitbox.one_hit_per_target),
                ),
                ("enabled".to_owned(), Value::Bool(hitbox.enabled)),
                (
                    "activation".to_owned(),
                    Value::String(hitbox.activation.to_string()),
                ),
                (
                    "hit_count".to_owned(),
                    Value::String(hitbox.hit_entities.len().to_string()),
                ),
            ]))
        }
        EngineViewKind::DamageReceiverState => {
            let receiver = world.get_component::<DamageReceiver>(entity)?;
            Value::Object(BTreeMap::from([
                ("team".to_owned(), Value::I64(i64::from(receiver.team))),
                ("health".to_owned(), Value::F64(f64::from(receiver.health))),
                (
                    "max_health".to_owned(),
                    Value::F64(f64::from(receiver.max_health)),
                ),
                (
                    "invulnerability_remaining".to_owned(),
                    Value::F64(f64::from(receiver.invulnerability_remaining)),
                ),
            ]))
        }
        EngineViewKind::NavigationState => {
            let agent = world.get_component::<NavMeshAgent>(entity)?;
            let status = match agent.status {
                crate::navmesh::NavMeshAgentStatus::Idle => "idle",
                crate::navmesh::NavMeshAgentStatus::MissingNavMesh => "missing_navmesh",
                crate::navmesh::NavMeshAgentStatus::MissingProfile => "missing_profile",
                crate::navmesh::NavMeshAgentStatus::StartOutside => "start_outside",
                crate::navmesh::NavMeshAgentStatus::EndOutside => "end_outside",
                crate::navmesh::NavMeshAgentStatus::NoPath => "no_path",
                crate::navmesh::NavMeshAgentStatus::Moving => "moving",
                crate::navmesh::NavMeshAgentStatus::PartialPath => "partial_path",
                crate::navmesh::NavMeshAgentStatus::Arrived => "arrived",
            };
            Value::Object(BTreeMap::from([
                (
                    "target".to_owned(),
                    agent
                        .target
                        .map_or(Value::Null, |target| vec3_value(target.to_array())),
                ),
                ("speed".to_owned(), Value::F64(f64::from(agent.speed))),
                (
                    "stopping_distance".to_owned(),
                    Value::F64(f64::from(agent.stopping_distance)),
                ),
                ("idle".to_owned(), Value::Bool(agent.is_idle())),
                ("status".to_owned(), Value::String(status.to_owned())),
                (
                    "repath_interval".to_owned(),
                    Value::F64(f64::from(agent.repath_interval)),
                ),
                (
                    "path".to_owned(),
                    Value::Array(
                        agent
                            .path()
                            .iter()
                            .map(|waypoint| vec3_value(waypoint.to_array()))
                            .collect(),
                    ),
                ),
            ]))
        }
        EngineViewKind::BehaviorTreeState => {
            let runner = world.get_component::<BehaviorTreeRunner>(entity)?;
            let status = match runner.last_status() {
                Some(BehaviorStatus::Success) => Value::String("success".to_owned()),
                Some(BehaviorStatus::Failure) => Value::String("failure".to_owned()),
                Some(BehaviorStatus::Running) => Value::String("running".to_owned()),
                None => Value::Null,
            };
            let visited = runner
                .last_dispatches()
                .iter()
                .map(|dispatch| {
                    Value::Object(BTreeMap::from([
                        (
                            "kind".to_owned(),
                            Value::String(
                                match dispatch.kind {
                                    BehaviorDispatchKind::Action => "action",
                                    BehaviorDispatchKind::Condition => "condition",
                                }
                                .to_owned(),
                            ),
                        ),
                        (
                            "behavior_id".to_owned(),
                            Value::String(dispatch.behavior_id.clone()),
                        ),
                    ]))
                })
                .collect();
            Value::Object(BTreeMap::from([
                ("enabled".to_owned(), Value::Bool(runner.is_enabled())),
                ("status".to_owned(), status),
                (
                    "error".to_owned(),
                    runner
                        .last_error()
                        .map_or(Value::Null, |error| Value::String(error.to_owned())),
                ),
                ("visited".to_owned(), Value::Array(visited)),
                (
                    "blackboard".to_owned(),
                    Value::Object(runner.blackboard().clone()),
                ),
            ]))
        }
        EngineViewKind::UiBindings => {
            let bindings = world.get_resource::<UiBindings>()?;
            Value::Object(
                bindings
                    .iter()
                    .map(|(name, value)| (name.to_owned(), ui_binding_value(value)))
                    .collect(),
            )
        }
    };
    Some(EngineViewCopy {
        value,
        identity: None,
    })
}

fn transform_value(transform: &Transform) -> Value {
    transform_parts_value(
        transform.translation.to_array(),
        transform.rotation.to_array(),
        transform.scale.to_array(),
    )
}

fn transform_parts_value(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> Value {
    Value::Object(BTreeMap::from([
        ("translation".to_owned(), vec3_value(translation)),
        ("rotation".to_owned(), vec4_value(rotation)),
        ("scale".to_owned(), vec3_value(scale)),
    ]))
}

fn vec3_value(value: [f32; 3]) -> Value {
    Value::Array(
        value
            .into_iter()
            .map(|part| Value::F64(f64::from(part)))
            .collect(),
    )
}

fn vec4_value(value: [f32; 4]) -> Value {
    Value::Array(
        value
            .into_iter()
            .map(|part| Value::F64(f64::from(part)))
            .collect(),
    )
}

fn entity_handle_value(entity: Entity) -> Value {
    entity_handle_record_value(entity_handle(entity))
}

fn entity_handle(entity: Entity) -> GameEntityHandle {
    GameEntityHandle {
        id: entity.id(),
        generation: entity.generation(),
    }
}

fn entity_handle_record_value(handle: GameEntityHandle) -> Value {
    Value::Object(BTreeMap::from([
        ("id".to_owned(), Value::U64(u64::from(handle.id))),
        (
            "generation".to_owned(),
            Value::U64(u64::from(handle.generation)),
        ),
    ]))
}

fn ui_binding_value(value: &UiBindingValue) -> Value {
    match value {
        UiBindingValue::Text(value) => Value::String(value.clone()),
        UiBindingValue::Number(value) => Value::F64(*value),
        UiBindingValue::Flag(value) => Value::Bool(*value),
    }
}

fn collision_event_value(phase: &str, contact: &crate::collision::CollisionEvent) -> Value {
    Value::Object(BTreeMap::from([
        ("phase".to_owned(), Value::String(phase.to_owned())),
        (
            "entity_a".to_owned(),
            entity_handle_record_value(entity_handle(contact.entity_a)),
        ),
        (
            "entity_b".to_owned(),
            entity_handle_record_value(entity_handle(contact.entity_b)),
        ),
        (
            "push_out".to_owned(),
            vec3_value(contact.push_out.to_array()),
        ),
        ("is_trigger".to_owned(), Value::Bool(contact.is_trigger)),
    ]))
}

fn scene_failure_value(path: &str, message: &str) -> Value {
    Value::Object(BTreeMap::from([
        ("status".to_owned(), Value::String("failed".to_owned())),
        ("path".to_owned(), Value::String(path.to_owned())),
        ("message".to_owned(), Value::String(message.to_owned())),
    ]))
}

fn game_event_value(event: GameEventEmission) -> Value {
    Value::Object(BTreeMap::from([
        ("event_id".to_owned(), Value::String(event.event_id)),
        (
            "target".to_owned(),
            event.target.map_or(Value::Null, entity_handle_record_value),
        ),
        ("payload".to_owned(), event.payload),
    ]))
}

/// Reports why a project-system invocation could not be compiled.
#[derive(Debug)]
pub enum GameHostCompileError {
    /// The exported system access declaration is invalid.
    Access(GameAccessError),
    /// A declared game resource has not been inserted into the host store.
    MissingResource(String),
    /// The requested engine view has no safe copied host adapter yet.
    UnsupportedEngineView(EngineViewKind),
    /// A row, input-byte, output-byte, or command cap was exceeded.
    Limit(GameIoLimitError),
    /// Deterministic callback input encoding failed.
    Serialize(JsonError),
}

impl fmt::Display for GameHostCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Access(source) => write!(formatter, "invalid game system access: {source}"),
            Self::MissingResource(id) => write!(formatter, "game resource `{id}` is missing"),
            Self::UnsupportedEngineView(view) => {
                write!(
                    formatter,
                    "engine view `{view:?}` is not supported by the game host"
                )
            }
            Self::Limit(source) => source.fmt(formatter),
            Self::Serialize(source) => {
                write!(formatter, "game invocation encoding failed: {source}")
            }
        }
    }
}

impl std::error::Error for GameHostCompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Access(source) => Some(source),
            Self::Limit(source) => Some(source),
            Self::Serialize(source) => Some(source),
            Self::MissingResource(_) | Self::UnsupportedEngineView(_) => None,
        }
    }
}

/// Reports why a callback output was rejected before atomic application.
#[derive(Debug)]
pub enum GameHostApplyError {
    /// The exported system access declaration is invalid.
    Access(GameAccessError),
    /// Input and host expect different game-I/O payload schemas.
    SchemaVersion {
        /// Host schema version.
        expected: u32,
        /// Callback output schema version.
        found: u32,
    },
    /// A collection or encoded byte cap was exceeded.
    Limit(GameIoLimitError),
    /// Deterministic callback output encoding failed.
    Serialize(JsonError),
    /// An entity/component pair was not present in a writable query result.
    UnauthorizedComponentTarget {
        /// Entity omitted from every matching writable query row.
        entity: GameEntityHandle,
        /// Component absent from that row's writable declarations.
        component_type: engine_authoring::ComponentTypeId,
    },
    /// A game resource was not declared writable.
    UnauthorizedResource(String),
    /// A command family was not declared by the system.
    UnauthorizedCommand(GameCommandFamily),
    /// An acknowledged event stream was not declared by the system.
    UnauthorizedEventStream(GameEventStream),
    /// A callback attempted to acknowledge an event it was not delivered.
    InvalidEventCursor {
        /// Stream being acknowledged.
        stream: GameEventStream,
        /// Cursor returned by the callback.
        sequence: u64,
        /// Highest sequence included in this invocation, if any.
        highest_delivered: Option<u64>,
    },
    /// A runtime entity handle is missing or stale.
    StaleEntity(GameEntityHandle),
    /// A live entity does not contain the project component store.
    MissingComponentStore(GameEntityHandle),
    /// A patch attempted to add a component without a structural command.
    MissingComponent {
        /// Entity missing the component.
        entity: GameEntityHandle,
        /// Missing component type.
        component_type: engine_authoring::ComponentTypeId,
    },
    /// The same entity/component pair was patched more than once.
    DuplicateComponentPatch {
        /// Repeated patch target.
        entity: GameEntityHandle,
        /// Repeated component type.
        component_type: engine_authoring::ComponentTypeId,
    },
    /// A patched game resource does not exist in the host store.
    MissingResource(String),
    /// The same game resource was patched more than once.
    DuplicateResourcePatch(String),
    /// A project event ID is not a valid stable dotted ID.
    InvalidEventId {
        /// Rejected event ID.
        id: String,
        /// Stable-ID validation error.
        source: SystemIdError,
    },
}

impl fmt::Display for GameHostApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Access(source) => write!(formatter, "invalid game system access: {source}"),
            Self::SchemaVersion { expected, found } => write!(
                formatter,
                "game output schema mismatch: host requires {expected}, callback returned {found}"
            ),
            Self::Limit(source) => source.fmt(formatter),
            Self::Serialize(source) => write!(formatter, "game output encoding failed: {source}"),
            Self::UnauthorizedComponentTarget {
                entity,
                component_type,
            } => write!(
                formatter,
                "game component `{component_type}` on entity {} generation {} was not present in a writable query result",
                entity.id, entity.generation
            ),
            Self::UnauthorizedResource(id) => {
                write!(formatter, "game resource `{id}` was not declared writable")
            }
            Self::UnauthorizedCommand(family) => {
                write!(
                    formatter,
                    "game command family `{family:?}` was not declared"
                )
            }
            Self::UnauthorizedEventStream(stream) => {
                write!(formatter, "game event stream `{stream:?}` was not declared")
            }
            Self::InvalidEventCursor {
                stream,
                sequence,
                highest_delivered,
            } => write!(
                formatter,
                "game event cursor {sequence} for `{stream:?}` exceeds the highest delivered sequence {highest_delivered:?}"
            ),
            Self::StaleEntity(entity) => write!(
                formatter,
                "game entity {} generation {} is missing or stale",
                entity.id, entity.generation
            ),
            Self::MissingComponentStore(entity) => write!(
                formatter,
                "game entity {} generation {} has no project component store",
                entity.id, entity.generation
            ),
            Self::MissingComponent {
                entity,
                component_type,
            } => write!(
                formatter,
                "game entity {} generation {} has no component `{component_type}`",
                entity.id, entity.generation
            ),
            Self::DuplicateComponentPatch {
                entity,
                component_type,
            } => write!(
                formatter,
                "game entity {} generation {} repeats patch for `{component_type}`",
                entity.id, entity.generation
            ),
            Self::MissingResource(id) => write!(formatter, "game resource `{id}` is missing"),
            Self::DuplicateResourcePatch(id) => {
                write!(formatter, "game resource `{id}` is patched more than once")
            }
            Self::InvalidEventId { id, source } => {
                write!(formatter, "invalid game event ID `{id}`: {source}")
            }
        }
    }
}

impl std::error::Error for GameHostApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Access(source) => Some(source),
            Self::Limit(source) => Some(source),
            Self::Serialize(source) => Some(source),
            Self::InvalidEventId { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::{CollisionEvent, CollisionEvents};
    use crate::game_io::{
        GameComponentAccess, GameComponentPatch, GameEngineViewAccess, GameQueryAccess,
        GameResourceAccess,
    };
    use crate::game_prefab::GamePrefabEvents;
    use crate::game_timer::GameTimers;
    use engine_authoring::ComponentTypeId;
    use glam::Vec3;

    fn frame() -> GameHostFrame {
        GameHostFrame {
            clock: GameClock {
                delta_seconds: 0.016,
                fixed_delta_seconds: 1.0 / 60.0,
                elapsed_seconds: 3.0,
                frame_index: 180,
                fixed_step_index: 180,
            },
            input_actions: BTreeMap::from([(
                "attack".to_owned(),
                GameInputActionState {
                    pressed: true,
                    just_pressed: true,
                    just_released: false,
                    scalar: 1.0,
                    vector: [0.0, 0.0],
                },
            )]),
            save_values: BTreeMap::new(),
            resources: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    #[test]
    fn runtime_metrics_accumulate_success_and_track_latest_failure() {
        let mut runtime = GameHostRuntime::default();
        runtime.record_success("game.combat", Duration::from_micros(30), 120, 80, 4, 2);
        runtime.record_success("game.combat", Duration::from_micros(20), 100, 60, 3, 1);
        runtime.record_failure("game.combat", &"callback failed");

        let metrics = runtime.system_metrics("game.combat").unwrap();
        assert_eq!(metrics.invocation_count, 3);
        assert_eq!(metrics.failure_count, 1);
        assert_eq!(metrics.total_callback_time, Duration::from_micros(50));
        assert_eq!(metrics.last_callback_time, Duration::from_micros(20));
        assert_eq!(metrics.last_input_bytes, 100);
        assert_eq!(metrics.last_output_bytes, 60);
        assert_eq!(metrics.last_query_rows, 3);
        assert_eq!(metrics.last_command_count, 1);
        assert_eq!(metrics.latest_error.as_deref(), Some("callback failed"));
    }

    #[test]
    fn module_resource_defaults_replace_previous_play_generation_values() {
        let mut runtime = GameHostRuntime::default();
        runtime
            .frame_mut()
            .resources
            .insert("game.old".to_owned(), Value::String("stale".to_owned()));

        runtime
            .replace_module_resources(BTreeMap::from([("game.mission".to_owned(), Value::I64(0))]));

        assert_eq!(
            runtime.frame().resources,
            BTreeMap::from([("game.mission".to_owned(), Value::I64(0))])
        );
    }

    #[test]
    fn save_input_contains_only_declared_existing_keys() {
        let mut world = World::new();
        let mut save = SaveData::new();
        save.set("mission.rank", SaveValue::Text("S".to_owned()));
        save.set("secret.debug", SaveValue::Flag(true));
        world.insert_resource(save);
        let access = GameSystemAccess {
            save_keys: vec!["mission.rank".to_owned(), "missing".to_owned()],
            ..GameSystemAccess::default()
        };
        let mut host_frame = GameHostFrame::default();

        refresh_game_host_frame(&world, &access, &mut host_frame);
        let invocation =
            compile_game_invocation(&world, "game.save_reader", &access, &host_frame).unwrap();

        assert_eq!(
            invocation.save_values,
            BTreeMap::from([("mission.rank".to_owned(), Value::String("S".to_owned()))])
        );
    }

    #[test]
    fn disabled_project_component_is_excluded_from_required_queries() {
        let mut world = World::new();
        let component_type = ComponentTypeId::new("game.status.stunned");
        let mut store = GameComponentStore::default();
        store.insert_runtime_value(component_type.clone(), Value::Bool(true));
        store.set_enabled(&component_type, false);
        let entity = world.spawn_with(store).unwrap();
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.stunned".to_owned(),
                components: vec![GameComponentAccess {
                    component_type: component_type.clone(),
                    mode: GameAccessMode::Read,
                    required: true,
                }],
                engine_views: Vec::new(),
            }],
            ..GameSystemAccess::default()
        };

        let disabled =
            compile_game_invocation(&world, "game.reader", &access, &GameHostFrame::default())
                .unwrap();
        assert!(disabled.queries[0].rows.is_empty());

        world
            .get_component_mut::<GameComponentStore>(entity)
            .unwrap()
            .set_enabled(&component_type, true);
        let enabled =
            compile_game_invocation(&world, "game.reader", &access, &GameHostFrame::default())
                .unwrap();
        assert_eq!(enabled.queries[0].rows.len(), 1);
    }

    #[test]
    fn ten_scoped_system_inputs_do_not_serialize_irrelevant_world_entities() {
        let mut world = World::new();
        let relevant_type = ComponentTypeId::new("game.health");
        let irrelevant_type = ComponentTypeId::new("game.debug_blob");
        for health in 0..100 {
            let mut store = GameComponentStore::default();
            store.insert_runtime_value(relevant_type.clone(), Value::I64(health));
            world.spawn_with(store).unwrap();
        }
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.health".to_owned(),
                components: vec![required_component("game.health")],
                engine_views: Vec::new(),
            }],
            ..GameSystemAccess::default()
        };
        let frame = GameHostFrame::default();
        let baseline = compile_game_invocation(&world, "game.system.00", &access, &frame).unwrap();
        let baseline_bytes = serde_json::to_vec(&baseline).unwrap().len();

        for _ in 0..100 {
            let mut store = GameComponentStore::default();
            store.insert_runtime_value(
                irrelevant_type.clone(),
                Value::String("IRRELEVANT_WORLD_SENTINEL".repeat(64)),
            );
            world.spawn_with(store).unwrap();
        }

        for index in 0..10 {
            let invocation = compile_game_invocation(
                &world,
                &format!("game.system.{index:02}"),
                &access,
                &frame,
            )
            .unwrap();
            assert_eq!(invocation.queries[0].rows.len(), 100);
            let bytes = serde_json::to_vec(&invocation).unwrap();
            assert_eq!(bytes.len(), baseline_bytes);
            assert!(!bytes
                .windows("IRRELEVANT_WORLD_SENTINEL".len())
                .any(|window| window == b"IRRELEVANT_WORLD_SENTINEL"));
        }
    }

    #[test]
    fn hitbox_view_exposes_stable_activation_metadata() {
        let mut world = World::new();
        let owner = world.spawn().unwrap();
        let carrier = world
            .spawn_with(AttackHitbox::new(owner, 3, 24.0, true, true))
            .unwrap();
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.hitboxes".to_owned(),
                components: Vec::new(),
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::HitboxState,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(
            &world,
            "game.hitbox_reader",
            &access,
            &GameHostFrame::default(),
        )
        .unwrap();
        assert_eq!(invocation.queries[0].rows[0].entity, entity_handle(carrier));
        let Value::Object(view) =
            &invocation.queries[0].rows[0].engine_views[&EngineViewKind::HitboxState]
        else {
            panic!("hitbox view must be an object");
        };
        assert_eq!(view["team"], Value::I64(3));
        assert_eq!(view["damage"], Value::F64(24.0));
        assert_eq!(view["activation"], Value::String("1".to_owned()));
        assert_eq!(view["owner"], entity_handle_value(owner));
    }

    #[test]
    fn authoring_identity_view_includes_runtime_name_tags_and_team() {
        let mut world = World::new();
        let authoring_id = engine_authoring::EntityId::from_stable_id(
            engine_authoring::StableId::new("entity_01JP0000000000000000000001"),
        )
        .expect("fixture entity id");
        let entity = world
            .spawn_with(RuntimeEntityIdentity {
                authoring_id: authoring_id.clone(),
                name: "authoring_name".to_owned(),
            })
            .unwrap();
        world
            .add_component(
                entity,
                RuntimeMetadata {
                    name: "captain".to_owned(),
                    tags: vec!["enemy".to_owned(), "boss".to_owned()],
                    team: "monsters".to_owned(),
                },
            )
            .unwrap();
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.identity".to_owned(),
                components: Vec::new(),
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::AuthoringIdentity,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(
            &world,
            "game.identity_reader",
            &access,
            &GameHostFrame::default(),
        )
        .unwrap();
        let Value::Object(identity) =
            &invocation.queries[0].rows[0].engine_views[&EngineViewKind::AuthoringIdentity]
        else {
            panic!("identity view must be an object");
        };
        assert_eq!(identity["id"], Value::String(authoring_id.to_string()));
        assert_eq!(identity["name"], Value::String("captain".to_owned()));
        assert_eq!(
            identity["tags"],
            Value::Array(vec![
                Value::String("enemy".to_owned()),
                Value::String("boss".to_owned())
            ])
        );
        assert_eq!(identity["team"], Value::String("monsters".to_owned()));
    }

    #[test]
    fn timer_source_events_enter_the_shared_log_once() {
        let mut world = World::new();
        let mut timers = GameTimers::default();
        timers.set_preflighted("game.timer.attack".to_owned(), 1.0);
        let mut timer_events = GameTimerEvents::default();
        crate::game_timer::query_game_timer(&timers, &mut timer_events, "game.timer.attack", 7);
        world.insert_resource(timers);
        world.insert_resource(timer_events);
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Timer],
            ..GameSystemAccess::default()
        };
        let mut runtime = GameHostRuntime::default();

        runtime.refresh_for_system(&world, "game.timer_reader", &access);
        let first =
            compile_game_invocation(&world, "game.timer_reader", &access, runtime.frame_mut())
                .unwrap();
        runtime.refresh_for_system(&world, "game.timer_reader", &access);
        let second =
            compile_game_invocation(&world, "game.timer_reader", &access, runtime.frame_mut())
                .unwrap();

        assert_eq!(first.events.len(), 1);
        assert_eq!(
            second.events.len(),
            1,
            "unconsumed host event remains visible"
        );
        assert_eq!(first.events[0].sequence, second.events[0].sequence);
    }

    #[test]
    fn prefab_source_result_enters_spawn_stream() {
        let mut world = World::new();
        let mut source = GamePrefabEvents::default();
        source.push(Value::Object(BTreeMap::from([(
            "status".to_owned(),
            Value::String("failed".to_owned()),
        )])));
        world.insert_resource(source);
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::SpawnResult],
            ..GameSystemAccess::default()
        };
        let mut runtime = GameHostRuntime::default();

        runtime.refresh_for_system(&world, "game.spawner", &access);

        assert_eq!(runtime.frame().events.len(), 1);
        assert_eq!(
            runtime.frame().events[0].stream,
            GameEventStream::SpawnResult
        );
    }

    #[test]
    fn runtime_does_not_retain_empty_effect_batches() {
        let mut runtime = GameHostRuntime::default();
        runtime.accept_effects(
            "game.empty",
            GameDeferredEffects {
                commands: Vec::new(),
                emitted_events: Vec::new(),
                consumed_event_sequences: BTreeMap::new(),
            },
        );

        assert!(runtime.drain_deferred_effects().is_empty());
    }

    #[test]
    fn event_consumption_is_isolated_per_project_system() {
        let mut runtime = GameHostRuntime::default();
        let mut initial = GameHostFrame::default();
        initial.events.push(GameEventRecord {
            stream: GameEventStream::Game,
            sequence: 7,
            payload: Value::String("ready".to_owned()),
        });
        runtime.set_frame(initial);
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Game],
            ..GameSystemAccess::default()
        };
        let world = World::new();

        runtime.refresh_for_system(&world, "game.first", &access);
        assert_eq!(runtime.frame().events.len(), 1);
        runtime.accept_effects(
            "game.first",
            GameDeferredEffects {
                commands: Vec::new(),
                emitted_events: Vec::new(),
                consumed_event_sequences: BTreeMap::from([(GameEventStream::Game, 7)]),
            },
        );
        runtime.refresh_for_system(&world, "game.first", &access);
        assert!(runtime.frame().events.is_empty());

        runtime.refresh_for_system(&world, "game.second", &access);
        assert_eq!(runtime.frame().events.len(), 1);
        assert_eq!(runtime.frame().events[0].sequence, 7);
    }

    #[test]
    fn emitted_game_event_enters_the_bounded_host_log() {
        let mut runtime = GameHostRuntime::default();
        runtime.accept_effects(
            "game.producer",
            GameDeferredEffects {
                commands: Vec::new(),
                emitted_events: vec![GameEventEmission::broadcast(
                    "game.combat.hit",
                    Value::I64(12),
                )],
                consumed_event_sequences: BTreeMap::new(),
            },
        );
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Game],
            ..GameSystemAccess::default()
        };

        runtime.refresh_for_system(&World::new(), "game.consumer", &access);

        assert_eq!(runtime.frame().events.len(), 1);
        let Value::Object(payload) = &runtime.frame().events[0].payload else {
            panic!("game event payload must be an object");
        };
        assert_eq!(
            payload["event_id"],
            Value::String("game.combat.hit".to_owned())
        );
        assert_eq!(payload["payload"], Value::I64(12));
        assert_eq!(payload["target"], Value::Null);
    }

    #[test]
    fn stale_targeted_game_event_rejects_every_output_patch() {
        let mut world = World::new();
        let live = world.spawn().unwrap();
        let stale = GameEntityHandle {
            id: live.id(),
            generation: live.generation().saturating_add(1),
        };
        let access = GameSystemAccess {
            command_families: vec![GameCommandFamily::GameEvent],
            ..GameSystemAccess::default()
        };
        let invocation =
            compile_game_invocation(&world, "game.producer", &access, &GameHostFrame::default())
                .unwrap();
        let output = GameInvocationOutput {
            emitted_events: vec![GameEventEmission::targeted(
                "game.combat.hit",
                stale,
                Value::Null,
            )],
            ..GameInvocationOutput::default()
        };

        assert!(matches!(
            apply_game_output(
                &mut world,
                &access,
                &invocation,
                &mut BTreeMap::new(),
                output,
            ),
            Err(GameHostApplyError::StaleEntity(handle)) if handle == stale
        ));
    }

    #[test]
    fn collision_snapshots_produce_enter_stay_and_exit_once_per_generation() {
        let mut world = World::new();
        let first = world.spawn().expect("first collider entity must spawn");
        let second = world.spawn().expect("second collider entity must spawn");
        world.insert_resource(CollisionEvents::default());
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Collision],
            ..GameSystemAccess::default()
        };
        let mut runtime = GameHostRuntime::default();

        // Reading before collision detection runs observes generation zero but
        // must not prevent the next producer generation from being captured.
        runtime.refresh_for_system(&world, "game.contact", &access);
        assert!(runtime.frame().events.is_empty());

        let contact = || CollisionEvent {
            entity_a: first,
            entity_b: second,
            push_out: Vec3::new(-0.25, 0.0, 0.0),
            is_trigger: true,
        };
        world
            .get_resource_mut::<CollisionEvents>()
            .unwrap()
            .replace_for_test(vec![contact()]);
        runtime.refresh_for_system(&world, "game.contact", &access);
        assert_eq!(event_phase(runtime.frame().events.last().unwrap()), "enter");

        world
            .get_resource_mut::<CollisionEvents>()
            .unwrap()
            .replace_for_test(vec![contact()]);
        runtime.refresh_for_system(&world, "game.contact", &access);
        assert_eq!(event_phase(runtime.frame().events.last().unwrap()), "stay");

        world
            .get_resource_mut::<CollisionEvents>()
            .unwrap()
            .replace_for_test(Vec::new());
        runtime.refresh_for_system(&world, "game.contact", &access);
        assert_eq!(event_phase(runtime.frame().events.last().unwrap()), "exit");
    }

    #[test]
    fn scene_failure_resource_is_delivered_once_on_scene_stream() {
        let mut world = World::new();
        world.insert_resource(SceneManager::new());
        world.insert_resource(SceneSwitchState::Failed {
            path: "scenes/missing.scene.json".to_owned(),
            message: "not found".to_owned(),
        });
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Scene],
            ..GameSystemAccess::default()
        };
        let mut runtime = GameHostRuntime::default();

        runtime.refresh_for_system(&world, "game.scene_flow", &access);
        assert_eq!(runtime.frame().events.len(), 1);
        let Value::Object(payload) = &runtime.frame().events[0].payload else {
            panic!("scene event payload must be an object");
        };
        assert_eq!(payload["status"], Value::String("failed".to_owned()));
        assert_eq!(
            payload["path"],
            Value::String("scenes/missing.scene.json".to_owned())
        );

        // Refreshing another callback against unchanged scene state must not
        // append a duplicate host record.
        runtime.refresh_for_system(&world, "game.scene_flow", &access);
        assert_eq!(runtime.frame().events.len(), 1);
    }

    #[test]
    fn declared_scene_state_view_copies_current_pending_and_failure_state() {
        let mut world = World::new();
        let mut manager = SceneManager::new();
        manager.register_initial_scene("scenes/hub.scene.json", Vec::new());
        manager.request_switch("scenes/boss.scene.json");
        world.insert_resource(manager);
        world.insert_resource(SceneSwitchState::Failed {
            path: "scenes/old_missing.scene.json".to_owned(),
            message: "not found".to_owned(),
        });
        let access = GameSystemAccess {
            host_views: vec![GameHostViewKind::SceneState],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(
            &world,
            "game.scene_reader",
            &access,
            &GameHostFrame::default(),
        )
        .unwrap();
        let Value::Object(state) = &invocation.host_views[&GameHostViewKind::SceneState] else {
            panic!("scene state view must be an object");
        };
        assert_eq!(state["generation"], Value::String("0".to_owned()));
        assert_eq!(
            state["current_path"],
            Value::String("scenes/hub.scene.json".to_owned())
        );
        assert_eq!(
            state["pending_path"],
            Value::String("scenes/boss.scene.json".to_owned())
        );
        assert_eq!(state["status"], Value::String("failed".to_owned()));
        assert_eq!(
            state["failure_message"],
            Value::String("not found".to_owned())
        );
    }

    fn event_phase(event: &GameEventRecord) -> &str {
        let Value::Object(payload) = &event.payload else {
            panic!("collision event payload must be an object");
        };
        let Value::String(phase) = &payload["phase"] else {
            panic!("collision phase must be a string");
        };
        phase
    }

    #[test]
    fn frame_refresh_uses_runtime_clock_and_project_input_map() {
        let mut world = World::new();
        world.insert_resource(Time {
            delta_seconds: 0.02,
            elapsed_seconds: 3.5,
            frame_count: 12,
        });
        let mut fixed_time = FixedTime::with_delta(0.01);
        let steps = fixed_time.step(0.03);
        for _ in 0..steps {
            fixed_time.begin_step();
        }
        world.insert_resource(fixed_time);
        let settings = engine_authoring::ProjectSettings {
            input_actions: vec![engine_authoring::InputAction {
                name: "attack".to_owned(),
                keys: vec!["Space".to_owned()],
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            }],
            ..engine_authoring::ProjectSettings::default()
        };
        let (action_map, diagnostics) = InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        world.insert_resource(action_map);
        let mut keyboard = crate::Input::<crate::KeyCode>::new();
        keyboard.press(crate::KeyCode::Space);
        world.insert_resource(keyboard);
        let access = GameSystemAccess {
            input_actions: vec!["attack".to_owned()],
            ..GameSystemAccess::default()
        };
        let mut frame = GameHostFrame::default();

        refresh_game_host_frame(&world, &access, &mut frame);

        assert_eq!(frame.clock.delta_seconds, 0.02);
        assert_eq!(frame.clock.fixed_delta_seconds, 0.01);
        assert_eq!(frame.clock.elapsed_seconds, 3.5);
        assert_eq!(frame.clock.frame_index, 12);
        assert_eq!(frame.clock.fixed_step_index, 3);
        assert!(frame.input_actions["attack"].just_pressed);
    }

    fn required_component(component_type: &str) -> GameComponentAccess {
        GameComponentAccess {
            component_type: ComponentTypeId::new(component_type),
            mode: GameAccessMode::Write,
            required: true,
        }
    }

    #[test]
    fn compiler_copies_only_declared_actions_components_and_views() {
        let mut world = World::new();
        let matching = world.spawn().expect("matching entity must spawn");
        world
            .add_component(
                matching,
                Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)),
            )
            .expect("transform must attach");
        let mut store = GameComponentStore::default();
        store.insert_runtime_value(ComponentTypeId::new("game.health"), Value::I64(80));
        store.insert_runtime_value(ComponentTypeId::new("game.secret"), Value::I64(99));
        world
            .add_component(matching, store)
            .expect("game component store must attach");
        world.spawn().expect("unmatched entity must spawn");

        let access = GameSystemAccess {
            input_actions: vec!["attack".to_owned(), "missing_action".to_owned()],
            queries: vec![GameQueryAccess {
                id: "game.query.combatants".to_owned(),
                components: vec![required_component("game.health")],
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::Transform,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(&world, "game.combat", &access, &frame())
            .expect("declared access must compile");

        assert_eq!(invocation.input_actions.len(), 2);
        assert!(invocation.input_actions["attack"].just_pressed);
        assert_eq!(
            invocation.input_actions["missing_action"],
            GameInputActionState::default()
        );
        assert_eq!(invocation.queries[0].rows.len(), 1);
        let row = &invocation.queries[0].rows[0];
        assert_eq!(row.entity.id, matching.id());
        assert_eq!(row.components.len(), 1);
        assert_eq!(
            row.components.get(&ComponentTypeId::new("game.health")),
            Some(&Value::I64(80))
        );
        assert!(row.engine_views.contains_key(&EngineViewKind::Transform));
    }

    #[test]
    fn required_engine_view_filters_entities_missing_that_component() {
        let mut world = World::new();
        let with_transform = world
            .spawn_with(Transform::default())
            .expect("transformed entity must spawn");
        world.spawn().expect("plain entity must spawn");
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.transformed".to_owned(),
                components: Vec::new(),
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::Transform,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(&world, "game.observe", &access, &frame())
            .expect("transform query must compile");

        assert_eq!(invocation.queries[0].rows.len(), 1);
        assert_eq!(invocation.queries[0].rows[0].entity.id, with_transform.id());
    }

    #[test]
    fn undeclared_resource_and_event_data_do_not_cross_boundary() {
        let world = World::new();
        let mut frame = frame();
        frame
            .resources
            .insert("game.mission".to_owned(), Value::I64(2));
        frame.events.push(GameEventRecord {
            stream: crate::game_io::GameEventStream::Collision,
            sequence: 4,
            payload: Value::String("contact".to_owned()),
        });

        let invocation =
            compile_game_invocation(&world, "game.empty", &GameSystemAccess::default(), &frame)
                .expect("empty access must compile");

        assert!(invocation.resources.is_empty());
        assert!(invocation.events.is_empty());
        assert!(invocation.input_actions.is_empty());
    }

    #[test]
    fn ui_binding_view_copies_stable_typed_values() {
        let mut world = World::new();
        world.spawn().expect("query entity must spawn");
        let mut bindings = UiBindings::new();
        bindings.set("hud.name", UiBindingValue::Text("Jibanyan".to_owned()));
        bindings.set("hud.hp", UiBindingValue::Number(120.0));
        bindings.set("hud.ready", UiBindingValue::Flag(true));
        world.insert_resource(bindings);
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.ui".to_owned(),
                components: Vec::new(),
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::UiBindings,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };

        let invocation = compile_game_invocation(&world, "game.ui", &access, &frame())
            .expect("UI bindings are an approved copied view");
        let Value::Object(values) =
            &invocation.queries[0].rows[0].engine_views[&EngineViewKind::UiBindings]
        else {
            panic!("UI binding view must be an object");
        };
        assert_eq!(values["hud.name"], Value::String("Jibanyan".to_owned()));
        assert_eq!(values["hud.hp"], Value::F64(120.0));
        assert_eq!(values["hud.ready"], Value::Bool(true));
    }

    #[test]
    fn authorized_output_patches_component_resource_and_returns_command() {
        let mut world = World::new();
        let entity = world.spawn().expect("entity must spawn");
        let mut store = GameComponentStore::default();
        let health = ComponentTypeId::new("game.health");
        store.insert_runtime_value(health.clone(), Value::I64(80));
        world
            .add_component(entity, store)
            .expect("game component store must attach");
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.combatants".to_owned(),
                components: vec![required_component("game.health")],
                engine_views: Vec::new(),
            }],
            resources: vec![GameResourceAccess {
                id: "game.mission".to_owned(),
                mode: GameAccessMode::Write,
            }],
            command_families: vec![GameCommandFamily::Audio],
            ..GameSystemAccess::default()
        };
        let handle = GameEntityHandle {
            id: entity.id(),
            generation: entity.generation(),
        };
        let command = GameCommand {
            family: GameCommandFamily::Audio,
            request_id: Some(7),
            target: None,
            payload: Value::String("hit".to_owned()),
        };
        let output = GameInvocationOutput {
            component_patches: vec![GameComponentPatch {
                entity: handle,
                component_type: health.clone(),
                value: Value::I64(55),
            }],
            resource_patches: vec![crate::game_io::GameResourcePatch {
                resource_id: "game.mission".to_owned(),
                value: Value::I64(3),
            }],
            commands: vec![command.clone()],
            ..GameInvocationOutput::default()
        };
        let mut resources = BTreeMap::from([("game.mission".to_owned(), Value::I64(2))]);
        let mut host_frame = frame();
        host_frame.resources = resources.clone();
        let invocation = compile_game_invocation(&world, "game.combat", &access, &host_frame)
            .expect("authorized input must compile");

        let effects = apply_game_output(&mut world, &access, &invocation, &mut resources, output)
            .expect("authorized output must apply");

        assert_eq!(
            world
                .get_component::<GameComponentStore>(entity)
                .and_then(|store| store.value(&health)),
            Some(&Value::I64(55))
        );
        assert_eq!(resources["game.mission"], Value::I64(3));
        assert_eq!(effects.commands, vec![command]);
    }

    #[test]
    fn output_cannot_acknowledge_an_event_that_was_not_delivered() {
        let mut world = World::new();
        let access = GameSystemAccess {
            event_streams: vec![GameEventStream::Collision],
            ..GameSystemAccess::default()
        };
        let mut host_frame = frame();
        host_frame.events.push(GameEventRecord {
            stream: GameEventStream::Collision,
            sequence: 4,
            payload: Value::Null,
        });
        let invocation = compile_game_invocation(&world, "game.contact", &access, &host_frame)
            .expect("declared collision event must compile");
        let output = GameInvocationOutput {
            consumed_event_sequences: BTreeMap::from([(GameEventStream::Collision, 5)]),
            ..GameInvocationOutput::default()
        };

        assert!(matches!(
            apply_game_output(
                &mut world,
                &access,
                &invocation,
                &mut BTreeMap::new(),
                output
            ),
            Err(GameHostApplyError::InvalidEventCursor {
                stream: GameEventStream::Collision,
                sequence: 5,
                highest_delivered: Some(4),
            })
        ));
    }

    #[test]
    fn unauthorized_patch_rejects_complete_output_without_mutation() {
        let mut world = World::new();
        let entity = world.spawn().expect("entity must spawn");
        let mut store = GameComponentStore::default();
        let health = ComponentTypeId::new("game.health");
        store.insert_runtime_value(health.clone(), Value::I64(80));
        world
            .add_component(entity, store)
            .expect("game component store must attach");
        let output = GameInvocationOutput {
            component_patches: vec![GameComponentPatch {
                entity: GameEntityHandle {
                    id: entity.id(),
                    generation: entity.generation(),
                },
                component_type: health.clone(),
                value: Value::I64(0),
            }],
            ..GameInvocationOutput::default()
        };
        let invocation =
            compile_game_invocation(&world, "game.empty", &GameSystemAccess::default(), &frame())
                .expect("empty input must compile");

        assert!(matches!(
            apply_game_output(
                &mut world,
                &GameSystemAccess::default(),
                &invocation,
                &mut BTreeMap::new(),
                output
            ),
            Err(GameHostApplyError::UnauthorizedComponentTarget { .. })
        ));
        assert_eq!(
            world
                .get_component::<GameComponentStore>(entity)
                .and_then(|store| store.value(&health)),
            Some(&Value::I64(80))
        );
    }

    #[test]
    fn writable_component_patch_is_limited_to_returned_query_rows() {
        let mut world = World::new();
        let matching = world
            .spawn_with(Transform::default())
            .expect("matching entity must spawn");
        let outside_query = world.spawn().expect("outside entity must spawn");
        let health = ComponentTypeId::new("game.health");
        for entity in [matching, outside_query] {
            let mut store = GameComponentStore::default();
            store.insert_runtime_value(health.clone(), Value::I64(80));
            world
                .add_component(entity, store)
                .expect("game component store must attach");
        }
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.transformed_health".to_owned(),
                components: vec![required_component("game.health")],
                engine_views: vec![GameEngineViewAccess {
                    view: EngineViewKind::Transform,
                    required: true,
                }],
            }],
            ..GameSystemAccess::default()
        };
        let invocation = compile_game_invocation(&world, "game.combat", &access, &frame())
            .expect("declared input must compile");
        assert_eq!(invocation.queries[0].rows.len(), 1);
        let output = GameInvocationOutput {
            component_patches: vec![GameComponentPatch {
                entity: GameEntityHandle {
                    id: outside_query.id(),
                    generation: outside_query.generation(),
                },
                component_type: health.clone(),
                value: Value::I64(0),
            }],
            ..GameInvocationOutput::default()
        };

        assert!(matches!(
            apply_game_output(
                &mut world,
                &access,
                &invocation,
                &mut BTreeMap::new(),
                output
            ),
            Err(GameHostApplyError::UnauthorizedComponentTarget { .. })
        ));
        assert_eq!(
            world
                .get_component::<GameComponentStore>(outside_query)
                .and_then(|store| store.value(&health)),
            Some(&Value::I64(80))
        );
    }

    #[test]
    fn stale_command_target_rejects_component_patch_atomically() {
        let mut world = World::new();
        let entity = world.spawn().expect("entity must spawn");
        let mut store = GameComponentStore::default();
        let health = ComponentTypeId::new("game.health");
        store.insert_runtime_value(health.clone(), Value::I64(80));
        world
            .add_component(entity, store)
            .expect("game component store must attach");
        let access = GameSystemAccess {
            queries: vec![GameQueryAccess {
                id: "game.query.combatants".to_owned(),
                components: vec![required_component("game.health")],
                engine_views: Vec::new(),
            }],
            command_families: vec![GameCommandFamily::Despawn],
            ..GameSystemAccess::default()
        };
        let output = GameInvocationOutput {
            component_patches: vec![GameComponentPatch {
                entity: GameEntityHandle {
                    id: entity.id(),
                    generation: entity.generation(),
                },
                component_type: health.clone(),
                value: Value::I64(10),
            }],
            commands: vec![GameCommand {
                family: GameCommandFamily::Despawn,
                request_id: None,
                target: Some(GameEntityHandle {
                    id: entity.id(),
                    generation: entity.generation().saturating_add(1),
                }),
                payload: Value::Null,
            }],
            ..GameInvocationOutput::default()
        };
        let invocation = compile_game_invocation(&world, "game.combat", &access, &frame())
            .expect("authorized input must compile");

        assert!(matches!(
            apply_game_output(
                &mut world,
                &access,
                &invocation,
                &mut BTreeMap::new(),
                output
            ),
            Err(GameHostApplyError::StaleEntity(_))
        ));
        assert_eq!(
            world
                .get_component::<GameComponentStore>(entity)
                .and_then(|store| store.value(&health)),
            Some(&Value::I64(80))
        );
    }
}
