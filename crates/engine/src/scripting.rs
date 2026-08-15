//! Rhai scripting runtime for scene-specific behaviour.
//!
//! Implements Phase 42: `ScriptAsset` loading and AST caching,
//! `ScriptComponent` lifecycle dispatch (`on_start`, `on_update`, `on_event`),
//! a sandboxed `ScriptContextProxy` facade, profiling metrics, and runaway-
//! script safety limits via `Engine::set_max_operations`.
//!
//! # Sandbox boundary
//!
//! Rhai scripts may not access raw ECS `World`, `AssetStorage`, filesystem,
//! network, process, or arbitrary module imports. All game-state interaction is
//! mediated by the approved `ScriptContextProxy` API.
//!
//! # Usage
//!
//! 1. Insert a [`ScriptEngine`] resource into the world.
//! 2. Call [`ScriptEngine::compile`] for each `.rhai` asset.
//! 3. Register [`scripting_update_system`] in the fixed-update schedule.

use engine_authoring::id::{AssetId, EntityId};
use engine_authoring::value::Value;
use engine_ecs::{Entity, Query, Res, ResMut};
use hashbrown::HashMap;
use rhai::{Array as RhaiArray, Dynamic, Engine, ImmutableString, Map as RhaiMap, Scope, AST};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::animation::AnimationEvents;
use crate::collision::{collisions_by_entity, CollisionEvents, CollisionInfo};
use crate::input::{Input, KeyCode};
use crate::save::{SaveData, SaveStore, SaveValue};
use crate::script_api::{
    dynamic_to_ui_binding, QueuedScriptCommand, ScriptApiCommand, ScriptLockCommand,
    MAX_SCRIPT_COMMANDS,
};
use crate::time::FixedTime;
use crate::transform::Transform;

// ---------------------------------------------------------------------------
// Constants and type aliases
// ---------------------------------------------------------------------------

/// Authoring component type id for the common script attachment component.
pub const SCRIPT_COMPONENT: &str = "engine.script";

/// Generic value crossing the Rhai/Rust `ScriptContext` boundary.
pub type ScriptValue = Value;

// ---------------------------------------------------------------------------
// Asset and component types
// ---------------------------------------------------------------------------

/// Source asset for one `.rhai` script.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptAsset {
    /// Stable authoring asset id for this script.
    pub asset_id: AssetId,
    /// UTF-8 Rhai source code.
    pub source: String,
}

impl ScriptAsset {
    /// Creates a script asset record from source text.
    pub fn new(asset_id: AssetId, source: impl Into<String>) -> Self {
        Self {
            asset_id,
            source: source.into(),
        }
    }
}

/// Runtime component that attaches a `.rhai` script asset to an entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptComponent {
    /// Script asset referenced by this component.
    pub script: AssetId,
    /// Whether this script instance participates in lifecycle dispatch.
    pub enabled: bool,
    /// Deterministic execution order for entities with multiple scripts.
    pub order: i32,
    /// Private, temporary per-entity script state.
    pub state: ScriptState,
}

impl ScriptComponent {
    /// Creates an enabled script component with default execution order.
    pub fn new(script: AssetId) -> Self {
        Self {
            script,
            enabled: true,
            order: 0,
            state: ScriptState::default(),
        }
    }
}

/// Temporary private state for one entity's script instance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptState {
    /// String-keyed state values owned by the script instance.
    pub values: BTreeMap<String, ScriptValue>,
}

/// Runtime instance data for a script attached to one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptInstance {
    /// Script asset used by this instance.
    pub script: AssetId,
    /// Authoring entity identifier.
    pub entity: EntityId,
    /// Whether `on_start` has already run.
    pub started: bool,
    /// Private state for this entity/script pair.
    pub state: ScriptState,
}

impl ScriptInstance {
    /// Creates instance state for a script attached to an authoring entity.
    pub fn new(script: AssetId, entity: EntityId) -> Self {
        Self {
            script,
            entity,
            started: false,
            state: ScriptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

/// Lifecycle hook names supported by the scripting MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptLifecycleHook {
    /// Runs once before the first update for this script instance.
    Start,
    /// Runs every frame for enabled script instances.
    Update,
    /// Runs when an engine-approved event is delivered.
    Event,
}

impl ScriptLifecycleHook {
    /// Returns the Rhai function name for this lifecycle hook.
    pub fn function_name(self) -> &'static str {
        match self {
            Self::Start => "on_start",
            Self::Update => "on_update",
            Self::Event => "on_event",
        }
    }
}

/// Future lifecycle hooks explicitly excluded from the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FutureScriptLifecycleHook {
    /// Future collision-enter callback.
    CollisionEnter,
    /// Future trigger-enter callback.
    TriggerEnter,
    /// Future enable callback.
    Enable,
    /// Future disable callback.
    Disable,
}

impl FutureScriptLifecycleHook {
    /// Returns the planned Rhai function name for this lifecycle hook.
    pub fn function_name(self) -> &'static str {
        match self {
            Self::CollisionEnter => "on_collision_enter",
            Self::TriggerEnter => "on_trigger_enter",
            Self::Enable => "on_enable",
            Self::Disable => "on_disable",
        }
    }
}

// ---------------------------------------------------------------------------
// Profiling types
// ---------------------------------------------------------------------------

/// `ScriptContext` API calls tracked by profiler/debug instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScriptContextCall {
    /// `ctx.get_component(...)`.
    GetComponent,
    /// `ctx.set_component(...)`.
    SetComponent,
    /// `ctx.send_event(...)`.
    SendEvent,
    /// `ctx.input_pressed(...)`.
    InputPressed,
    /// `ctx.log(...)`.
    Log,
}

/// Count of calls made through one `ScriptContext` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptContextCallCount {
    /// API being counted.
    pub call: ScriptContextCall,
    /// Number of calls during the measured window.
    pub count: u64,
}

/// Timing and safety metrics for one script execution window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptExecutionMetrics {
    /// Most recent measured duration.
    pub last_time: Duration,
    /// Average duration over the profiler window.
    pub average_time: Duration,
    /// Maximum duration over the profiler window.
    pub max_time: Duration,
    /// Hook invocations in this metric.
    pub calls: u64,
    /// Rhai operation count when detailed profiling is active.
    pub operation_count: Option<u64>,
    /// Whether Rhai's maximum operation limit was exceeded.
    pub max_operations_exceeded: bool,
}

impl ScriptExecutionMetrics {
    fn record(&mut self, elapsed: Duration, max_exceeded: bool) {
        self.calls += 1;
        self.last_time = elapsed;
        if elapsed > self.max_time {
            self.max_time = elapsed;
        }
        // Incremental average: new_avg = (old_avg * (n-1) + new) / n
        let n = self.calls;
        self.average_time = (self.average_time * (n as u32 - 1) + elapsed) / n as u32;
        if max_exceeded {
            self.max_operations_exceeded = true;
        }
    }
}

/// Per-frame aggregate profiler data for the scripting runtime.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptProfilerFrame {
    /// Total script execution time for the frame.
    pub total_script_time: Duration,
    /// Per-script timing keyed by script asset.
    pub per_script: BTreeMap<AssetId, ScriptExecutionMetrics>,
    /// Per-entity timing keyed by authoring entity id.
    pub per_entity: BTreeMap<EntityId, ScriptExecutionMetrics>,
    /// Per-hook timing keyed by MVP lifecycle hook.
    pub per_hook: BTreeMap<ScriptLifecycleHook, ScriptExecutionMetrics>,
    /// Script compile time keyed by script asset.
    pub compile_time: BTreeMap<AssetId, Duration>,
    /// `ScriptContext` call counts in profiler/debug mode.
    pub context_calls: BTreeMap<ScriptContextCall, u64>,
}

// ---------------------------------------------------------------------------
// Engine configuration
// ---------------------------------------------------------------------------

/// Runtime configuration for the Rhai scripting layer.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptEngineConfig {
    /// Maximum Rhai operations per hook call. `0` disables the limit.
    pub max_operations: u64,
    /// Wall-clock threshold that emits a slow-script warning.
    pub slow_script_warning: Duration,
    /// Whether detailed operation counts and context call counts are collected.
    pub detailed_profiling: bool,
}

impl Default for ScriptEngineConfig {
    fn default() -> Self {
        Self {
            max_operations: 1_000_000,
            slow_script_warning: Duration::from_millis(2),
            detailed_profiling: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Script error and call result
// ---------------------------------------------------------------------------

/// Error produced during script compilation or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptError {
    /// Rhai could not parse the source.
    Compile(String),
    /// A runtime error terminated execution.
    Runtime(String),
    /// The script exceeded the configured maximum operation count.
    MaxOperationsExceeded,
    /// No compiled AST was found for the requested asset.
    AstNotFound(AssetId),
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(msg) => write!(f, "script.compile_error: {msg}"),
            Self::Runtime(msg) => write!(f, "script.runtime_error: {msg}"),
            Self::MaxOperationsExceeded => write!(f, "script.max_operations_exceeded"),
            Self::AstNotFound(id) => write!(f, "script.ast_not_found: {id}"),
        }
    }
}

/// A deferred command to write one or more numeric fields into a named component.
#[derive(Debug, Clone)]
pub struct ComponentSetCommand {
    /// Runtime entity display string, e.g. `"1v0"`.
    pub entity_id: String,
    /// Engine component name, e.g. `"engine.transform"`.
    pub component: String,
    /// Field values to apply, keyed by field name.
    pub fields: HashMap<String, f64>,
}

/// A deferred command from `ctx.save_set(key, value)` (Phase 56 / ADR 0048 §4).
#[derive(Debug, Clone)]
pub struct SaveSetCommand {
    /// The `SaveData` key to write.
    pub key: String,
    /// The value to store, already converted from the Rhai `Dynamic`.
    pub value: SaveValue,
}

/// A deferred persistence command from `ctx.save_write(slot)` or
/// `ctx.save_load(slot)` (Phase 56 / ADR 0048 §4).
#[derive(Debug, Clone, Copy)]
pub enum SavePersistCommand {
    /// Persist the current `SaveData` resource to `slot`.
    Write {
        /// Target slot number.
        slot: u32,
    },
    /// Replace the `SaveData` resource with the contents of `slot`.
    Load {
        /// Source slot number.
        slot: u32,
    },
}

/// Output produced by one lifecycle hook call.
#[derive(Debug, Clone, Default)]
pub struct ScriptCallResult {
    /// Messages from `ctx.log(...)`.
    pub logs: Vec<String>,
    /// Deferred component mutations from `ctx.set_component(...)`.
    pub component_sets: Vec<ComponentSetCommand>,
    /// Events from `ctx.send_event(target, event_name)`.
    pub events_sent: Vec<(String, String)>,
    /// Deferred Script API v2 commands.
    pub api_commands: Vec<ScriptApiCommand>,
    /// Deferred `SaveData` mutations from `ctx.save_set(...)`.
    pub save_sets: Vec<SaveSetCommand>,
    /// Deferred persistence commands from `ctx.save_write(...)` / `ctx.save_load(...)`.
    pub save_ops: Vec<SavePersistCommand>,
    /// Script-visible errors from `ctx.save_set(...)` calls with an
    /// unconvertible value (for example a Rhai array or map).
    pub save_errors: Vec<String>,
    /// Timing and safety metrics.
    pub metrics: ScriptExecutionMetrics,
    /// Error that terminated execution, if any.
    pub error: Option<ScriptError>,
}

// ---------------------------------------------------------------------------
// Internal context proxy (Rhai-registered type)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ContextOutput {
    logs: Vec<String>,
    component_sets: Vec<ComponentSetCommand>,
    events_sent: Vec<(String, String)>,
    api_commands: Vec<ScriptApiCommand>,
    save_sets: Vec<SaveSetCommand>,
    save_ops: Vec<SavePersistCommand>,
    save_errors: Vec<String>,
    log_calls: u64,
    input_pressed_calls: u64,
    get_component_calls: u64,
    set_component_calls: u64,
    send_event_calls: u64,
    consumed_timers: BTreeSet<String>,
}

/// Sandboxed ECS context passed as `ctx` inside Rhai scripts.
///
/// Mutations accumulate in an interior `Arc<Mutex<...>>` buffer so that
/// Rhai's internal cloning of `Dynamic` values still refers to the same
/// buffer after the call returns.
#[derive(Clone)]
pub struct ScriptContextProxy {
    entity_id: String,
    inner: Arc<Mutex<ContextOutput>>,
    input_map: Arc<HashMap<String, bool>>,
    transform_snapshot: Option<Arc<RhaiMap>>,
    /// Snapshot of the active `SaveData` resource taken before this hook
    /// call, read by `ctx.save_get` (ADR 0048 §4).
    save_snapshot: Option<Arc<SaveData>>,
    /// This entity's collision events from the last collision-detection
    /// step, read by `ctx.collisions()` (Phase 57).
    collision_snapshot: Option<Arc<Vec<CollisionInfo>>>,
    entity_snapshot: Arc<BTreeMap<String, Vec<String>>>,
    finished_timers: Arc<BTreeSet<String>>,
}

impl ScriptContextProxy {
    fn new(
        entity_id: String,
        input_map: Arc<HashMap<String, bool>>,
        transform_snapshot: Option<Arc<RhaiMap>>,
        save_snapshot: Option<Arc<SaveData>>,
        collision_snapshot: Option<Arc<Vec<CollisionInfo>>>,
        entity_snapshot: Arc<BTreeMap<String, Vec<String>>>,
        finished_timers: Arc<BTreeSet<String>>,
    ) -> Self {
        Self {
            entity_id,
            inner: Arc::new(Mutex::new(ContextOutput::default())),
            input_map,
            transform_snapshot,
            save_snapshot,
            collision_snapshot,
            entity_snapshot,
            finished_timers,
        }
    }

    fn push_api_command(&self, command: ScriptApiCommand) {
        let mut output = self
            .inner
            .lock()
            .expect("script context mutex is unpoisoned");
        if output.api_commands.len() < MAX_SCRIPT_COMMANDS {
            output.api_commands.push(command);
        }
    }

    fn take_output(inner: &Arc<Mutex<ContextOutput>>) -> ContextOutput {
        // SAFETY invariant: scripts execute on the game-loop thread only; the
        // mutex cannot be poisoned by a thread panic while the lock is held.
        let mut guard = inner
            .lock()
            .expect("script context mutex is unpoisoned in the single-threaded game loop");
        std::mem::take(&mut *guard)
    }
}

// ---------------------------------------------------------------------------
// Rhai API registration
// ---------------------------------------------------------------------------

fn register_context_api(engine: &mut Engine) {
    engine.register_type_with_name::<ScriptContextProxy>("Ctx");

    engine.register_fn(
        "log",
        |ctx: &mut ScriptContextProxy, msg: ImmutableString| {
            let mut g = ctx
                .inner
                .lock()
                .expect("script context mutex is unpoisoned");
            g.logs.push(msg.to_string());
            g.log_calls += 1;
        },
    );

    engine.register_fn(
        "input_pressed",
        |ctx: &mut ScriptContextProxy, action: ImmutableString| -> bool {
            let pressed = ctx.input_map.get(action.as_str()).copied().unwrap_or(false);
            ctx.inner
                .lock()
                .expect("script context mutex is unpoisoned")
                .input_pressed_calls += 1;
            pressed
        },
    );

    engine.register_fn(
        "self_entity",
        |ctx: &mut ScriptContextProxy| -> ImmutableString { ctx.entity_id.clone().into() },
    );

    engine.register_fn(
        "get_component",
        |ctx: &mut ScriptContextProxy,
         _entity: ImmutableString,
         component: ImmutableString|
         -> Dynamic {
            ctx.inner
                .lock()
                .expect("script context mutex is unpoisoned")
                .get_component_calls += 1;
            if component.as_str() == "engine.transform"
                && let Some(snap) = &ctx.transform_snapshot {
                    return Dynamic::from((**snap).clone());
                }
            Dynamic::UNIT
        },
    );

    engine.register_fn(
        "set_component",
        |ctx: &mut ScriptContextProxy,
         entity: ImmutableString,
         component: ImmutableString,
         value: Dynamic| {
            let fields = extract_f64_fields(&value);
            let mut g = ctx
                .inner
                .lock()
                .expect("script context mutex is unpoisoned");
            g.set_component_calls += 1;
            g.component_sets.push(ComponentSetCommand {
                entity_id: entity.to_string(),
                component: component.to_string(),
                fields,
            });
        },
    );

    engine.register_fn(
        "send_event",
        |ctx: &mut ScriptContextProxy, target: ImmutableString, event_name: ImmutableString| {
            let mut g = ctx
                .inner
                .lock()
                .expect("script context mutex is unpoisoned");
            g.send_event_calls += 1;
            if g.events_sent.len() < MAX_SCRIPT_COMMANDS {
                g.events_sent
                    .push((target.to_string(), event_name.to_string()));
            }
        },
    );

    engine.register_fn(
        "find_entity",
        |ctx: &mut ScriptContextProxy, name: ImmutableString| -> ImmutableString {
            ctx.entity_snapshot
                .get(name.as_str())
                .and_then(|entities| entities.first())
                .cloned()
                .unwrap_or_default()
                .into()
        },
    );

    engine.register_fn(
        "find_entities",
        |ctx: &mut ScriptContextProxy, name: ImmutableString| -> RhaiArray {
            ctx.entity_snapshot
                .get(name.as_str())
                .into_iter()
                .flatten()
                .cloned()
                .map(Dynamic::from)
                .collect()
        },
    );

    engine.register_fn(
        "spawn_prefab",
        |ctx: &mut ScriptContextProxy, path: ImmutableString, x: f64, y: f64, z: f64| {
            ctx.push_api_command(ScriptApiCommand::SpawnPrefab {
                path: path.to_string(),
                position: glam::Vec3::new(x as f32, y as f32, z as f32),
            });
        },
    );

    engine.register_fn(
        "despawn",
        |ctx: &mut ScriptContextProxy, target: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::Despawn {
                target: target.to_string(),
            });
        },
    );

    engine.register_fn(
        "play_anim",
        |ctx: &mut ScriptContextProxy,
         target: ImmutableString,
         clip_id: ImmutableString,
         fade_seconds: f64| {
            ctx.push_api_command(ScriptApiCommand::PlayAnimation {
                target: target.to_string(),
                clip_id: clip_id.to_string(),
                fade_seconds: fade_seconds as f32,
            });
        },
    );

    engine.register_fn(
        "set_anim_condition",
        |ctx: &mut ScriptContextProxy,
         target: ImmutableString,
         condition: ImmutableString,
         value: bool| {
            ctx.push_api_command(ScriptApiCommand::SetAnimationCondition {
                target: target.to_string(),
                condition: condition.to_string(),
                value,
            });
        },
    );

    engine.register_fn(
        "play_se",
        |ctx: &mut ScriptContextProxy, asset_id: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::PlaySoundEffect {
                asset_id: asset_id.to_string(),
            });
        },
    );

    engine.register_fn(
        "play_bgm",
        |ctx: &mut ScriptContextProxy, asset_id: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::PlayBackgroundMusic {
                asset_id: asset_id.to_string(),
            });
        },
    );

    engine.register_fn(
        "crossfade_bgm",
        |ctx: &mut ScriptContextProxy, asset_id: ImmutableString, fade_seconds: f64| {
            ctx.push_api_command(ScriptApiCommand::CrossfadeBackgroundMusic {
                asset_id: asset_id.to_string(),
                fade_seconds: fade_seconds as f32,
            });
        },
    );

    engine.register_fn(
        "set_bgm_volume",
        |ctx: &mut ScriptContextProxy, volume: f64| {
            ctx.push_api_command(ScriptApiCommand::SetBackgroundMusicVolume {
                volume: volume as f32,
            });
        },
    );

    engine.register_fn(
        "set_se_volume",
        |ctx: &mut ScriptContextProxy, volume: f64| {
            ctx.push_api_command(ScriptApiCommand::SetSoundEffectVolume {
                volume: volume as f32,
            });
        },
    );

    engine.register_fn("stop_bgm", |ctx: &mut ScriptContextProxy| {
        ctx.push_api_command(ScriptApiCommand::StopBackgroundMusic);
    });

    engine.register_fn(
        "ui_set",
        |ctx: &mut ScriptContextProxy, name: ImmutableString, value: Dynamic| {
            if let Some(value) = dynamic_to_ui_binding(&value) {
                ctx.push_api_command(ScriptApiCommand::SetUiBinding {
                    name: name.to_string(),
                    value,
                });
            }
        },
    );

    engine.register_fn(
        "ui_remove",
        |ctx: &mut ScriptContextProxy, name: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::RemoveUiBinding {
                name: name.to_string(),
            });
        },
    );

    engine.register_fn("lock_target", |ctx: &mut ScriptContextProxy| {
        ctx.push_api_command(ScriptApiCommand::LockTarget {
            command: ScriptLockCommand::Acquire,
        });
    });
    engine.register_fn("cycle_target", |ctx: &mut ScriptContextProxy| {
        ctx.push_api_command(ScriptApiCommand::LockTarget {
            command: ScriptLockCommand::Cycle,
        });
    });
    engine.register_fn("release_target", |ctx: &mut ScriptContextProxy| {
        ctx.push_api_command(ScriptApiCommand::LockTarget {
            command: ScriptLockCommand::Release,
        });
    });

    engine.register_fn(
        "request_scene",
        |ctx: &mut ScriptContextProxy, path: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::RequestScene {
                path: path.to_string(),
            });
        },
    );

    engine.register_fn(
        "set_timer",
        |ctx: &mut ScriptContextProxy, name: ImmutableString, seconds: f64| {
            ctx.push_api_command(ScriptApiCommand::SetTimer {
                name: name.to_string(),
                seconds: seconds as f32,
            });
        },
    );
    engine.register_fn(
        "cancel_timer",
        |ctx: &mut ScriptContextProxy, name: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::CancelTimer {
                name: name.to_string(),
            });
        },
    );
    engine.register_fn(
        "timer_finished",
        |ctx: &mut ScriptContextProxy, name: ImmutableString| -> bool {
            let mut output = ctx
                .inner
                .lock()
                .expect("script context mutex is unpoisoned");
            let finished = ctx.finished_timers.contains(name.as_str())
                && !output.consumed_timers.contains(name.as_str());
            if finished {
                output.consumed_timers.insert(name.to_string());
                if output.api_commands.len() < MAX_SCRIPT_COMMANDS {
                    output.api_commands.push(ScriptApiCommand::ConsumeTimer {
                        name: name.to_string(),
                    });
                }
            }
            finished
        },
    );

    engine.register_fn(
        "subscribe",
        |ctx: &mut ScriptContextProxy, event: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::Subscribe {
                event: event.to_string(),
            });
        },
    );
    engine.register_fn(
        "unsubscribe",
        |ctx: &mut ScriptContextProxy, event: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::Unsubscribe {
                event: event.to_string(),
            });
        },
    );
    engine.register_fn(
        "emit",
        |ctx: &mut ScriptContextProxy, event: ImmutableString| {
            ctx.push_api_command(ScriptApiCommand::Emit {
                event: event.to_string(),
            });
        },
    );

    engine.register_fn(
        "save_get",
        |ctx: &mut ScriptContextProxy, key: ImmutableString| -> Dynamic {
            match ctx
                .save_snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.get(key.as_str()))
            {
                Some(value) => save_value_to_dynamic(value),
                None => Dynamic::UNIT,
            }
        },
    );

    engine.register_fn(
        "save_set",
        |ctx: &mut ScriptContextProxy, key: ImmutableString, value: Dynamic| {
            let mut g = ctx
                .inner
                .lock()
                .expect("script context mutex is unpoisoned");
            match dynamic_to_save_value(&value) {
                Some(save_value) => g.save_sets.push(SaveSetCommand {
                    key: key.to_string(),
                    value: save_value,
                }),
                None => g.save_errors.push(format!(
                    "save_set: value for key '{key}' is not a Text/Number/Flag-compatible type"
                )),
            }
        },
    );

    engine.register_fn("save_write", |ctx: &mut ScriptContextProxy, slot: i64| {
        let mut g = ctx
            .inner
            .lock()
            .expect("script context mutex is unpoisoned");
        match u32::try_from(slot) {
            Ok(slot) => g.save_ops.push(SavePersistCommand::Write { slot }),
            Err(_) => g
                .save_errors
                .push(format!("save_write: slot must be non-negative, got {slot}")),
        }
    });

    engine.register_fn("save_load", |ctx: &mut ScriptContextProxy, slot: i64| {
        let mut g = ctx
            .inner
            .lock()
            .expect("script context mutex is unpoisoned");
        match u32::try_from(slot) {
            Ok(slot) => g.save_ops.push(SavePersistCommand::Load { slot }),
            Err(_) => g
                .save_errors
                .push(format!("save_load: slot must be non-negative, got {slot}")),
        }
    });

    engine.register_fn("collisions", |ctx: &mut ScriptContextProxy| -> RhaiArray {
        match &ctx.collision_snapshot {
            Some(list) => list.iter().map(collision_info_to_dynamic).collect(),
            None => RhaiArray::new(),
        }
    });
}

/// Converts one [`CollisionInfo`] into the Rhai map returned by
/// `ctx.collisions()`: `other` (entity string, formatted the same way
/// `ctx.self_entity()` formats entities), `push_x`/`push_y`/`push_z`
/// (floats), and `is_trigger` (bool).
fn collision_info_to_dynamic(info: &CollisionInfo) -> Dynamic {
    let mut map = RhaiMap::new();
    map.insert("other".into(), Dynamic::from(format!("{}", info.other)));
    map.insert("push_x".into(), Dynamic::from_float(info.push.x as f64));
    map.insert("push_y".into(), Dynamic::from_float(info.push.y as f64));
    map.insert("push_z".into(), Dynamic::from_float(info.push.z as f64));
    map.insert("is_trigger".into(), Dynamic::from_bool(info.is_trigger));
    Dynamic::from(map)
}

/// Converts a Rhai `Dynamic` into a [`SaveValue`] for `ctx.save_set`.
///
/// Only `bool`, integer, floating-point, and string values convert; any other
/// type (arrays, maps, unit, ...) returns `None` so the caller can record a
/// script-visible error instead of queuing a command.
fn dynamic_to_save_value(value: &Dynamic) -> Option<SaveValue> {
    if let Ok(flag) = value.as_bool() {
        return Some(SaveValue::Flag(flag));
    }
    if let Ok(i) = value.as_int() {
        return Some(SaveValue::Number(i as f64));
    }
    if let Ok(f) = value.as_float() {
        return Some(SaveValue::Number(f));
    }
    if let Ok(text) = value.clone().into_immutable_string() {
        return Some(SaveValue::Text(text.to_string()));
    }
    None
}

/// Converts a [`SaveValue`] into a Rhai `Dynamic` for `ctx.save_get`.
fn save_value_to_dynamic(value: &SaveValue) -> Dynamic {
    match value {
        SaveValue::Text(text) => Dynamic::from(text.clone()),
        SaveValue::Number(number) => Dynamic::from_float(*number),
        SaveValue::Flag(flag) => Dynamic::from_bool(*flag),
    }
}

fn extract_f64_fields(value: &Dynamic) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    if let Some(map) = value.read_lock::<RhaiMap>() {
        for (k, v) in map.iter() {
            if let Ok(f) = v.as_float() {
                out.insert(k.to_string(), f);
            } else if let Ok(i) = v.as_int() {
                out.insert(k.to_string(), i as f64);
            }
        }
    }
    out
}

fn is_function_not_found(err: &rhai::EvalAltResult) -> bool {
    matches!(err, rhai::EvalAltResult::ErrorFunctionNotFound(_, _))
}

fn is_max_ops_error(err: &rhai::EvalAltResult) -> bool {
    matches!(err, rhai::EvalAltResult::ErrorTooManyOperations(_))
}

// ---------------------------------------------------------------------------
// ScriptEngine — runtime wrapper, AST cache, per-entity state
// ---------------------------------------------------------------------------

/// Rhai runtime, AST cache, and per-entity `on_start` tracking.
///
/// Insert this as a world resource and register [`scripting_update_system`] to
/// drive the scripting layer each fixed update.
pub struct ScriptEngine {
    engine: Engine,
    ast_cache: HashMap<AssetId, AST>,
    compile_times: HashMap<AssetId, Duration>,
    /// Tracks which runtime entities have had `on_start` dispatched.
    instances_started: HashMap<Entity, bool>,
    /// Runtime configuration.
    pub config: ScriptEngineConfig,
    /// Accumulated profiler data for the current frame.
    frame_profiler: ScriptProfilerFrame,
    /// Snapshot of the active `SaveData` resource exposed to `ctx.save_get`
    /// for the next hook calls, set via [`ScriptEngine::set_save_snapshot`].
    save_snapshot: Option<Arc<SaveData>>,
    /// Per-entity collision snapshot exposed to `ctx.collisions()` for the
    /// next hook calls, set via [`ScriptEngine::set_collision_snapshot`].
    collision_snapshot: HashMap<Entity, Arc<Vec<CollisionInfo>>>,
    /// Deterministic authoring-name lookup exposed to entity search APIs.
    entity_snapshot: Arc<BTreeMap<String, Vec<String>>>,
    /// Completed timer names exposed per entity for the next hook call.
    timer_snapshot: HashMap<Entity, Arc<BTreeSet<String>>>,
    /// Script API commands waiting to move into the world queue.
    pending_api_commands: Vec<QueuedScriptCommand>,
    /// Deferred Script API event deliveries for the next scripting pass.
    pending_event_deliveries: Vec<(Entity, String)>,
}

impl ScriptEngine {
    /// Creates a scripting engine and registers the sandboxed context API.
    pub fn new(config: ScriptEngineConfig) -> Self {
        let mut engine = Engine::new();
        engine.set_max_operations(config.max_operations);
        register_context_api(&mut engine);
        Self {
            engine,
            ast_cache: HashMap::new(),
            compile_times: HashMap::new(),
            instances_started: HashMap::new(),
            config,
            frame_profiler: ScriptProfilerFrame::default(),
            save_snapshot: None,
            collision_snapshot: HashMap::new(),
            entity_snapshot: Arc::new(BTreeMap::new()),
            timer_snapshot: HashMap::new(),
            pending_api_commands: Vec::new(),
            pending_event_deliveries: Vec::new(),
        }
    }

    /// Sets the [`SaveData`] snapshot exposed to `ctx.save_get` for
    /// subsequent hook calls, until replaced again.
    ///
    /// Dispatch systems ([`scripting_update_system`],
    /// [`crate::ui_document::ui_script_event_system`]) call this once per
    /// dispatch cycle with the current `SaveData` resource, mirroring the
    /// per-cycle input snapshot passed to [`ScriptEngine::run_on_update`]. A
    /// script's `save_get` therefore always observes the resource state as
    /// of the start of the current cycle, never a value written by another
    /// entity's script earlier in the same cycle.
    pub fn set_save_snapshot(&mut self, snapshot: Option<Arc<SaveData>>) {
        self.save_snapshot = snapshot;
    }

    /// Sets the per-entity collision snapshot exposed to `ctx.collisions()`
    /// for subsequent hook calls, until replaced again (Phase 57).
    ///
    /// Dispatch systems call this once per dispatch cycle with a per-entity
    /// view built by [`crate::collision::collisions_by_entity`], mirroring
    /// [`ScriptEngine::set_save_snapshot`]'s per-cycle resource snapshot
    /// pattern. An entity absent from `snapshot` sees an empty array from
    /// `ctx.collisions()`.
    pub fn set_collision_snapshot(&mut self, snapshot: HashMap<Entity, Arc<Vec<CollisionInfo>>>) {
        self.collision_snapshot = snapshot;
    }

    /// Sets the deterministic entity-name lookup exposed to search APIs.
    pub fn set_entity_snapshot(&mut self, snapshot: BTreeMap<String, Vec<String>>) {
        self.entity_snapshot = Arc::new(snapshot);
    }

    /// Sets completed timer names exposed to each entity's next hook call.
    pub fn set_timer_snapshot(&mut self, snapshot: HashMap<Entity, Arc<BTreeSet<String>>>) {
        self.timer_snapshot = snapshot;
    }

    /// Replaces the deferred Script API event deliveries for the next pass.
    pub fn set_event_deliveries(&mut self, deliveries: Vec<(Entity, String)>) {
        self.pending_event_deliveries = deliveries;
    }

    /// Removes and returns deferred event deliveries.
    pub fn take_event_deliveries(&mut self) -> Vec<(Entity, String)> {
        std::mem::take(&mut self.pending_event_deliveries)
    }

    /// Adds one hook result's Script API commands to the engine-owned queue.
    pub fn enqueue_api_commands(&mut self, entity: Entity, result: &ScriptCallResult) {
        let commands = result
            .api_commands
            .iter()
            .cloned()
            .map(|command| QueuedScriptCommand {
                issuer: entity,
                command,
            })
            .chain(
                result
                    .events_sent
                    .iter()
                    .map(|(target, event)| QueuedScriptCommand {
                        issuer: entity,
                        command: ScriptApiCommand::SendEvent {
                            target: target.clone(),
                            event: event.clone(),
                        },
                    }),
            );
        for command in commands {
            if self.pending_api_commands.len() >= MAX_SCRIPT_COMMANDS {
                log::warn!(
                    "ScriptEngine pending API queue is full ({MAX_SCRIPT_COMMANDS}); dropping command"
                );
                break;
            }
            self.pending_api_commands.push(command);
        }
    }

    /// Removes commands waiting to enter the world Script API queue.
    pub fn take_api_commands(&mut self) -> Vec<QueuedScriptCommand> {
        std::mem::take(&mut self.pending_api_commands)
    }

    /// Compiles `source` and stores the resulting AST under `asset_id`.
    ///
    /// Calling this again with the same `asset_id` replaces the cached AST,
    /// enabling hot reload.
    ///
    /// # Errors
    ///
    /// Returns [`ScriptError::Compile`] if Rhai cannot parse the source.
    pub fn compile(&mut self, asset_id: &AssetId, source: &str) -> Result<(), ScriptError> {
        let t0 = Instant::now();
        match self.engine.compile(source) {
            Ok(ast) => {
                let elapsed = t0.elapsed();
                self.ast_cache.insert(asset_id.clone(), ast);
                self.compile_times.insert(asset_id.clone(), elapsed);
                self.frame_profiler
                    .compile_time
                    .insert(asset_id.clone(), elapsed);
                Ok(())
            }
            Err(err) => Err(ScriptError::Compile(err.to_string())),
        }
    }

    /// Returns `true` if a compiled AST exists for `asset_id`.
    pub fn is_compiled(&self, asset_id: &AssetId) -> bool {
        self.ast_cache.contains_key(asset_id)
    }

    /// Returns `true` if `on_start` has been dispatched for this entity.
    pub fn is_started(&self, entity: Entity) -> bool {
        self.instances_started
            .get(&entity)
            .copied()
            .unwrap_or(false)
    }

    /// Marks the entity's `on_start` as dispatched.
    pub fn mark_started(&mut self, entity: Entity) {
        self.instances_started.insert(entity, true);
    }

    /// Removes tracking state for an entity (use on despawn).
    pub fn remove_instance(&mut self, entity: Entity) {
        self.instances_started.remove(&entity);
    }

    /// Clears all instance state. Call at the start of each play session.
    pub fn reset_instances(&mut self) {
        self.instances_started.clear();
    }

    /// Takes the accumulated frame profiler data, replacing it with a fresh frame.
    pub fn take_profiler_frame(&mut self) -> ScriptProfilerFrame {
        std::mem::take(&mut self.frame_profiler)
    }

    /// Dispatches `on_start(ctx)` for one entity.
    ///
    /// Missing hooks are silently skipped (not an error).
    pub fn run_on_start(
        &mut self,
        entity: Entity,
        script_id: &AssetId,
        input_map: Arc<HashMap<String, bool>>,
        transform_snapshot: Option<Arc<RhaiMap>>,
    ) -> ScriptCallResult {
        let Some(ast) = self.ast_cache.get(script_id).cloned() else {
            return ScriptCallResult {
                error: Some(ScriptError::AstNotFound(script_id.clone())),
                ..Default::default()
            };
        };
        let ctx = ScriptContextProxy::new(
            format!("{entity}"),
            input_map,
            transform_snapshot,
            self.save_snapshot.clone(),
            self.collision_snapshot.get(&entity).cloned(),
            Arc::clone(&self.entity_snapshot),
            self.timer_snapshot
                .get(&entity)
                .cloned()
                .unwrap_or_else(|| Arc::new(BTreeSet::new())),
        );
        let inner = Arc::clone(&ctx.inner);
        let mut scope = Scope::new();
        let t0 = Instant::now();
        let call_result = self
            .engine
            .call_fn::<()>(&mut scope, &ast, "on_start", (ctx,));
        self.finish_call(
            call_result,
            t0,
            inner,
            script_id,
            ScriptLifecycleHook::Start,
        )
    }

    /// Dispatches `on_update(ctx, dt)` for one entity.
    pub fn run_on_update(
        &mut self,
        entity: Entity,
        script_id: &AssetId,
        dt: f32,
        input_map: Arc<HashMap<String, bool>>,
        transform_snapshot: Option<Arc<RhaiMap>>,
    ) -> ScriptCallResult {
        let Some(ast) = self.ast_cache.get(script_id).cloned() else {
            return ScriptCallResult {
                error: Some(ScriptError::AstNotFound(script_id.clone())),
                ..Default::default()
            };
        };
        let ctx = ScriptContextProxy::new(
            format!("{entity}"),
            input_map,
            transform_snapshot,
            self.save_snapshot.clone(),
            self.collision_snapshot.get(&entity).cloned(),
            Arc::clone(&self.entity_snapshot),
            self.timer_snapshot
                .get(&entity)
                .cloned()
                .unwrap_or_else(|| Arc::new(BTreeSet::new())),
        );
        let inner = Arc::clone(&ctx.inner);
        let mut scope = Scope::new();
        let t0 = Instant::now();
        let call_result =
            self.engine
                .call_fn::<()>(&mut scope, &ast, "on_update", (ctx, dt as f64));
        self.finish_call(
            call_result,
            t0,
            inner,
            script_id,
            ScriptLifecycleHook::Update,
        )
    }

    /// Dispatches `on_event(ctx, event)` for one entity.
    pub fn run_on_event(
        &mut self,
        entity: Entity,
        script_id: &AssetId,
        event: impl Into<ImmutableString>,
        input_map: Arc<HashMap<String, bool>>,
        transform_snapshot: Option<Arc<RhaiMap>>,
    ) -> ScriptCallResult {
        let Some(ast) = self.ast_cache.get(script_id).cloned() else {
            return ScriptCallResult {
                error: Some(ScriptError::AstNotFound(script_id.clone())),
                ..Default::default()
            };
        };
        let ctx = ScriptContextProxy::new(
            format!("{entity}"),
            input_map,
            transform_snapshot,
            self.save_snapshot.clone(),
            self.collision_snapshot.get(&entity).cloned(),
            Arc::clone(&self.entity_snapshot),
            self.timer_snapshot
                .get(&entity)
                .cloned()
                .unwrap_or_else(|| Arc::new(BTreeSet::new())),
        );
        let inner = Arc::clone(&ctx.inner);
        let event_val: ImmutableString = event.into();
        let mut scope = Scope::new();
        let t0 = Instant::now();
        let call_result = self
            .engine
            .call_fn::<()>(&mut scope, &ast, "on_event", (ctx, event_val));
        self.finish_call(
            call_result,
            t0,
            inner,
            script_id,
            ScriptLifecycleHook::Event,
        )
    }

    fn finish_call(
        &mut self,
        call_result: Result<(), Box<rhai::EvalAltResult>>,
        t0: Instant,
        inner: Arc<Mutex<ContextOutput>>,
        script_id: &AssetId,
        hook: ScriptLifecycleHook,
    ) -> ScriptCallResult {
        let elapsed = t0.elapsed();
        let max_exceeded = call_result
            .as_ref()
            .err()
            .map(|e| is_max_ops_error(e))
            .unwrap_or(false);

        let script_error = match call_result {
            Ok(()) => None,
            Err(err) if is_function_not_found(&err) => None,
            Err(err) if is_max_ops_error(&err) => Some(ScriptError::MaxOperationsExceeded),
            Err(err) => Some(ScriptError::Runtime(err.to_string())),
        };

        let slow = elapsed >= self.config.slow_script_warning;
        if slow {
            log::warn!(
                "script.slow: {} hook={} elapsed={:.3}ms",
                script_id,
                hook.function_name(),
                elapsed.as_secs_f64() * 1000.0
            );
        }

        let output = ScriptContextProxy::take_output(&inner);

        let mut metrics = ScriptExecutionMetrics::default();
        metrics.record(elapsed, max_exceeded);

        // Accumulate frame profiler data.
        self.frame_profiler.total_script_time += elapsed;
        self.frame_profiler
            .per_script
            .entry(script_id.clone())
            .or_default()
            .record(elapsed, max_exceeded);
        self.frame_profiler
            .per_hook
            .entry(hook)
            .or_default()
            .record(elapsed, max_exceeded);
        for (&call, &count) in &collect_call_counts(&output) {
            *self.frame_profiler.context_calls.entry(call).or_insert(0) += count;
        }

        ScriptCallResult {
            logs: output.logs,
            component_sets: output.component_sets,
            events_sent: output.events_sent,
            api_commands: output.api_commands,
            save_sets: output.save_sets,
            save_ops: output.save_ops,
            save_errors: output.save_errors,
            metrics,
            error: script_error,
        }
    }
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new(ScriptEngineConfig::default())
    }
}

fn collect_call_counts(output: &ContextOutput) -> BTreeMap<ScriptContextCall, u64> {
    let mut map = BTreeMap::new();
    if output.log_calls > 0 {
        map.insert(ScriptContextCall::Log, output.log_calls);
    }
    if output.input_pressed_calls > 0 {
        map.insert(ScriptContextCall::InputPressed, output.input_pressed_calls);
    }
    if output.get_component_calls > 0 {
        map.insert(ScriptContextCall::GetComponent, output.get_component_calls);
    }
    if output.set_component_calls > 0 {
        map.insert(ScriptContextCall::SetComponent, output.set_component_calls);
    }
    if output.send_event_calls > 0 {
        map.insert(ScriptContextCall::SendEvent, output.send_event_calls);
    }
    map
}

// ---------------------------------------------------------------------------
// Input snapshot helper
// ---------------------------------------------------------------------------

/// Builds a string-keyed action map from the current keyboard state.
///
/// Action names match the strings expected by `ctx.input_pressed(action)`.
pub fn build_input_snapshot(input: &Input<KeyCode>) -> HashMap<String, bool> {
    let bindings: &[(&str, KeyCode)] = &[
        ("left", KeyCode::ArrowLeft),
        ("right", KeyCode::ArrowRight),
        ("up", KeyCode::ArrowUp),
        ("down", KeyCode::ArrowDown),
        ("forward", KeyCode::KeyW),
        ("backward", KeyCode::KeyS),
        ("strafe_left", KeyCode::KeyA),
        ("strafe_right", KeyCode::KeyD),
        ("jump", KeyCode::Space),
        ("interact", KeyCode::KeyE),
        ("sprint", KeyCode::ShiftLeft),
        ("attack", KeyCode::KeyF),
    ];
    bindings
        .iter()
        .map(|(name, key)| ((*name).to_string(), input.pressed(*key)))
        .collect()
}

// ---------------------------------------------------------------------------
// Transform helpers
// ---------------------------------------------------------------------------

pub(crate) fn transform_to_rhai_map(t: &Transform) -> RhaiMap {
    let mut map = RhaiMap::new();
    map.insert("x".into(), Dynamic::from_float(t.translation.x as f64));
    map.insert("y".into(), Dynamic::from_float(t.translation.y as f64));
    map.insert("z".into(), Dynamic::from_float(t.translation.z as f64));
    map.insert("scale_x".into(), Dynamic::from_float(t.scale.x as f64));
    map.insert("scale_y".into(), Dynamic::from_float(t.scale.y as f64));
    map.insert("scale_z".into(), Dynamic::from_float(t.scale.z as f64));
    map
}

pub(crate) fn apply_transform_fields(transform: &mut Transform, fields: &HashMap<String, f64>) {
    if let Some(&x) = fields.get("x") {
        transform.translation.x = x as f32;
    }
    if let Some(&y) = fields.get("y") {
        transform.translation.y = y as f32;
    }
    if let Some(&z) = fields.get("z") {
        transform.translation.z = z as f32;
    }
    if let Some(&sx) = fields.get("scale_x") {
        transform.scale.x = sx as f32;
    }
    if let Some(&sy) = fields.get("scale_y") {
        transform.scale.y = sy as f32;
    }
    if let Some(&sz) = fields.get("scale_z") {
        transform.scale.z = sz as f32;
    }
}

// ---------------------------------------------------------------------------
// Collision snapshot (Phase 57)
// ---------------------------------------------------------------------------

/// Builds the per-entity, `Arc`-wrapped collision snapshot consumed by
/// [`ScriptEngine::set_collision_snapshot`], from an optional
/// [`CollisionEvents`] resource.
///
/// Shared by [`scripting_update_system`] and
/// [`crate::ui_document::ui_script_event_system`] so both dispatch sites
/// build the snapshot identically. Returns an empty map when `events` is
/// `None` (a game that never inserts [`CollisionEvents`]), matching
/// `ctx.collisions()`'s documented empty-array behavior for entities with no
/// recorded overlaps.
pub(crate) fn arc_collision_snapshot(
    events: Option<&CollisionEvents>,
) -> HashMap<Entity, Arc<Vec<CollisionInfo>>> {
    events
        .map(|events| {
            collisions_by_entity(events)
                .into_iter()
                .map(|(entity, list)| (entity, Arc::new(list)))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Save command application (Phase 56 / ADR 0048 §4)
// ---------------------------------------------------------------------------

/// Applies one hook call's queued `save_set` / `save_write` / `save_load`
/// commands to the `SaveData` and `SaveStore` resources.
///
/// Shared by [`scripting_update_system`] and
/// [`crate::ui_document::ui_script_event_system`] so both dispatch sites
/// apply save commands identically. Sets are applied before persistence
/// commands regardless of the order the script queued them in, so a
/// `save_write` in the same hook call always observes values `save_set` in
/// that same call (Phase 56 spec, "command ordering"). A missing `SaveData`
/// or `SaveStore` resource is logged via `log::error!` and otherwise
/// ignored — this never panics, matching ADR 0048 §4's "never panics"
/// contract for save IO failures.
pub fn apply_save_commands(
    result: &ScriptCallResult,
    mut save_data: Option<&mut SaveData>,
    mut save_store: Option<&mut SaveStore>,
) {
    if result.save_sets.is_empty() && result.save_ops.is_empty() {
        return;
    }

    match save_data.as_deref_mut() {
        Some(data) => {
            for set in &result.save_sets {
                data.set(set.key.clone(), set.value.clone());
            }
        }
        None if !result.save_sets.is_empty() => {
            log::error!("script queued save_set command(s) but no SaveData resource is present");
        }
        None => {}
    }

    for op in &result.save_ops {
        match op {
            SavePersistCommand::Write { slot } => {
                match (save_store.as_deref_mut(), save_data.as_deref()) {
                    (Some(store), Some(data)) => {
                        if let Err(error) = store.write_slot(*slot, data) {
                            log::error!("save_write(slot={slot}) failed: {error}");
                        }
                    }
                    (None, _) => log::error!(
                        "save_write(slot={slot}) queued but no SaveStore resource is present"
                    ),
                    (_, None) => log::error!(
                        "save_write(slot={slot}) queued but no SaveData resource is present"
                    ),
                }
            }
            SavePersistCommand::Load { slot } => {
                match (save_store.as_deref_mut(), save_data.as_deref_mut()) {
                    (Some(store), Some(data)) => match store.read_slot(*slot) {
                        Ok(loaded) => *data = loaded,
                        Err(error) => log::error!("save_load(slot={slot}) failed: {error}"),
                    },
                    (None, _) => log::error!(
                        "save_load(slot={slot}) queued but no SaveStore resource is present"
                    ),
                    (_, None) => log::error!(
                        "save_load(slot={slot}) queued but no SaveData resource is present"
                    ),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ECS system
// ---------------------------------------------------------------------------

/// Logs a hook call's diagnostics and applies its self-entity transform and
/// save mutations.
///
/// Shared by [`scripting_update_system`]'s `on_start`/`on_update` dispatch and
/// its animation-event dispatch pass so both apply a [`ScriptCallResult`]
/// identically.
fn apply_hook_result(
    entity: Entity,
    entity_str: &str,
    result: &ScriptCallResult,
    transform_opt: Option<&mut Transform>,
    save_data: Option<&mut SaveData>,
    save_store: Option<&mut SaveStore>,
) {
    if let Some(ref err) = result.error {
        log::error!("script error entity={entity}: {err}");
    }
    for msg in &result.logs {
        log::info!("[script {entity}] {msg}");
    }
    for msg in &result.save_errors {
        log::error!("[script {entity}] {msg}");
    }

    // Apply self-entity transform mutations.
    if let Some(transform) = transform_opt {
        for cmd in &result.component_sets {
            if cmd.component == "engine.transform" && cmd.entity_id == entity_str {
                apply_transform_fields(transform, &cmd.fields);
            }
        }
    }

    apply_save_commands(result, save_data, save_store);
}

/// Dispatches Rhai lifecycle hooks for all active [`ScriptComponent`] entities.
///
/// Register this in the fixed-update schedule. The system compiles scripts on
/// first encounter, calls `on_start` once per entity, then `on_update` every
/// frame. Transform mutations queued by `ctx.set_component` are applied to the
/// entity's own [`Transform`] component. `SaveData` / `SaveStore` commands
/// queued by `ctx.save_set` / `ctx.save_write` / `ctx.save_load` are applied
/// via [`apply_save_commands`] (Phase 56); both resources are optional so
/// games that never use saves are unaffected.
///
/// After every entity's `on_start`/`on_update` has run, this system reads
/// [`AnimationEvents`] (Phase 59) and, for each record, dispatches
/// `on_event(ctx, name)` to **only** the record's own `entity` — provided
/// that entity carries an enabled [`ScriptComponent`] with a compiled script.
/// This is narrower than `UiEvents`, which broadcasts to every enabled
/// script; an animation event on entity A never reaches entity B's script.
/// `AnimationEvents` is optional, so games that never insert it are
/// unaffected. For an event fired this fixed step to be visible in the same
/// step's dispatch, register [`crate::animation::animation_system`] before
/// this system.
///
/// Script errors and slow-script warnings are emitted through the `log` crate
/// and should be forwarded to the editor Console panel by the caller.
// ECS system functions expose resource access in their signature so the
// scheduler can validate every dependency independently.
#[allow(clippy::too_many_arguments)]
pub fn scripting_update_system(
    time: Res<FixedTime>,
    input: Res<Input<KeyCode>>,
    mut script_engine: ResMut<ScriptEngine>,
    mut save_data: Option<ResMut<SaveData>>,
    mut save_store: Option<ResMut<SaveStore>>,
    collision_events: Option<Res<CollisionEvents>>,
    animation_events: Option<Res<AnimationEvents>>,
    mut query: Query<(&mut ScriptComponent, Option<&mut Transform>)>,
) {
    let dt = time.fixed_delta;
    let input_map = Arc::new(build_input_snapshot(&input));
    script_engine.set_save_snapshot(save_data.as_deref().map(|data| Arc::new(data.clone())));
    script_engine.set_collision_snapshot(arc_collision_snapshot(collision_events.as_deref()));

    for (entity, (script_comp, transform_opt)) in query.iter_mut() {
        if !script_comp.enabled {
            continue;
        }
        let script_id = script_comp.script.clone();
        if !script_engine.is_compiled(&script_id) {
            log::debug!("script {script_id} not compiled — skipping entity {entity}");
            continue;
        }

        let transform_snap = transform_opt
            .as_ref()
            .map(|t| Arc::new(transform_to_rhai_map(t)));

        let entity_str = format!("{entity}");
        let result = if !script_engine.is_started(entity) {
            let r = script_engine.run_on_start(
                entity,
                &script_id,
                Arc::clone(&input_map),
                transform_snap,
            );
            script_engine.mark_started(entity);
            r
        } else {
            script_engine.run_on_update(
                entity,
                &script_id,
                dt,
                Arc::clone(&input_map),
                transform_snap,
            )
        };

        apply_hook_result(
            entity,
            &entity_str,
            &result,
            transform_opt,
            save_data.as_deref_mut(),
            save_store.as_deref_mut(),
        );
        script_engine.enqueue_api_commands(entity, &result);
    }

    // Targeted animation-event dispatch (Phase 59): only the firing entity's
    // own script receives `on_event`, unlike `UiEvents`' broadcast. A linear
    // scan per event over the same query keeps this dispatch pass consistent
    // with the O(n²) collision-detection pattern already accepted for the
    // small scenes this engine targets (`crate::collision`).
    if let Some(animation_events) = animation_events.as_deref() {
        for record in animation_events.iter() {
            for (entity, (script_comp, transform_opt)) in query.iter_mut() {
                if entity != record.entity {
                    continue;
                }
                if !script_comp.enabled {
                    break;
                }
                let script_id = script_comp.script.clone();
                if !script_engine.is_compiled(&script_id) {
                    log::debug!(
                        "script {script_id} not compiled — skipping animation event '{}' for entity {entity}",
                        record.name
                    );
                    break;
                }

                let transform_snap = transform_opt
                    .as_ref()
                    .map(|t| Arc::new(transform_to_rhai_map(t)));
                let entity_str = format!("{entity}");
                let result = script_engine.run_on_event(
                    entity,
                    &script_id,
                    record.name.clone(),
                    Arc::clone(&input_map),
                    transform_snap,
                );

                apply_hook_result(
                    entity,
                    &entity_str,
                    &result,
                    transform_opt,
                    save_data.as_deref_mut(),
                    save_store.as_deref_mut(),
                );
                script_engine.enqueue_api_commands(entity, &result);
                break;
            }
        }
    }

    for (target, event_name) in script_engine.take_event_deliveries() {
        for (entity, (script_comp, transform_opt)) in query.iter_mut() {
            if entity != target {
                continue;
            }
            if !script_comp.enabled {
                break;
            }
            let script_id = script_comp.script.clone();
            if !script_engine.is_compiled(&script_id) {
                break;
            }
            let transform_snap = transform_opt
                .as_ref()
                .map(|transform| Arc::new(transform_to_rhai_map(transform)));
            let entity_str = entity.to_string();
            let result = script_engine.run_on_event(
                entity,
                &script_id,
                event_name,
                Arc::clone(&input_map),
                transform_snap,
            );
            apply_hook_result(
                entity,
                &entity_str,
                &result,
                transform_opt,
                save_data.as_deref_mut(),
                save_store.as_deref_mut(),
            );
            script_engine.enqueue_api_commands(entity, &result);
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_asset_id() -> AssetId {
        AssetId::generate()
    }

    fn sample_entity() -> Entity {
        let mut world = engine_ecs::World::new();
        world
            .spawn()
            .expect("test world must be able to spawn an entity")
    }

    #[test]
    fn compile_valid_rhai_script_succeeds() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        assert!(engine
            .compile(&id, r#"fn on_start(ctx) { ctx.log("hello"); }"#)
            .is_ok());
        assert!(engine.is_compiled(&id));
    }

    #[test]
    fn compile_invalid_rhai_returns_compile_error() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        let result = engine.compile(&id, "!!! not valid rhai !!!");
        assert!(matches!(result, Err(ScriptError::Compile(_))));
    }

    #[test]
    fn missing_on_update_hook_does_not_produce_error() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(&id, r#"fn on_start(ctx) { ctx.log("only start"); }"#)
            .unwrap();

        let result =
            engine.run_on_update(sample_entity(), &id, 0.016, Arc::new(HashMap::new()), None);
        assert!(
            result.error.is_none(),
            "missing hook must not produce an error"
        );
    }

    #[test]
    fn on_start_collects_log_messages() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(&id, r#"fn on_start(ctx) { ctx.log("started"); }"#)
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert_eq!(result.logs, vec!["started".to_string()]);
    }

    #[test]
    fn on_update_receives_dt_parameter() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(&id, r#"fn on_update(ctx, dt) { ctx.log(dt.to_string()); }"#)
            .unwrap();

        let result =
            engine.run_on_update(sample_entity(), &id, 0.016, Arc::new(HashMap::new()), None);
        assert!(!result.logs.is_empty(), "on_update must receive dt");
    }

    #[test]
    fn script_api_v2_calls_produce_typed_commands_and_snapshots() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        let entity = sample_entity();
        let mut entities = BTreeMap::new();
        entities.insert("enemy".to_string(), vec!["9v0".to_string()]);
        engine.set_entity_snapshot(entities);
        engine.set_timer_snapshot(HashMap::from([(
            entity,
            Arc::new(BTreeSet::from(["ready".to_string()])),
        )]));
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.log(ctx.find_entity("enemy"));
                    ctx.spawn_prefab("enemy.prefab.json", 1.0, 2.0, 3.0);
                    ctx.despawn(ctx.self_entity());
                    ctx.play_anim(ctx.self_entity(), "attack", 0.2);
                    ctx.set_anim_condition(ctx.self_entity(), "moving", true);
                    ctx.play_se("asset_01JP0000000000000000000001");
                    ctx.play_bgm("asset_01JP0000000000000000000002");
                    ctx.crossfade_bgm("asset_01JP0000000000000000000003", 0.5);
                    ctx.set_bgm_volume(0.7);
                    ctx.set_se_volume(0.8);
                    ctx.stop_bgm();
                    ctx.ui_set("score", 10);
                    ctx.ui_remove("old");
                    ctx.lock_target();
                    ctx.cycle_target();
                    ctx.release_target();
                    ctx.request_scene("scenes/result.scene.json");
                    ctx.set_timer("cooldown", 1.0);
                    ctx.cancel_timer("old_timer");
                    if ctx.timer_finished("ready") { ctx.log("ready"); }
                    if ctx.timer_finished("ready") { ctx.log("duplicate"); }
                    ctx.subscribe("alert");
                    ctx.unsubscribe("old_event");
                    ctx.emit("alert");
                    ctx.send_event(ctx.self_entity(), "direct");
                }"#,
            )
            .expect("Script API v2 fixture must compile");

        let result = engine.run_on_start(entity, &id, Arc::new(HashMap::new()), None);

        assert!(result.error.is_none(), "script failed: {:?}", result.error);
        assert_eq!(result.logs, vec!["9v0".to_string(), "ready".to_string()]);
        assert!(result
            .api_commands
            .iter()
            .any(|command| matches!(command, ScriptApiCommand::SpawnPrefab { .. })));
        assert!(result
            .api_commands
            .iter()
            .any(|command| matches!(command, ScriptApiCommand::PlayAnimation { .. })));
        assert!(result.api_commands.iter().any(|command| matches!(
            command,
            ScriptApiCommand::CrossfadeBackgroundMusic { fade_seconds, .. }
                if (*fade_seconds - 0.5).abs() < f32::EPSILON
        )));
        assert!(result.api_commands.iter().any(|command| matches!(
            command,
            ScriptApiCommand::SetBackgroundMusicVolume { volume }
                if (*volume - 0.7).abs() < f32::EPSILON
        )));
        assert!(result.api_commands.iter().any(|command| matches!(
            command,
            ScriptApiCommand::SetSoundEffectVolume { volume }
                if (*volume - 0.8).abs() < f32::EPSILON
        )));
        assert!(result
            .api_commands
            .iter()
            .any(|command| matches!(command, ScriptApiCommand::SetUiBinding { .. })));
        assert!(result
            .api_commands
            .iter()
            .any(|command| matches!(command, ScriptApiCommand::RequestScene { .. })));
        assert!(result.api_commands.iter().any(
            |command| matches!(command, ScriptApiCommand::ConsumeTimer { name } if name == "ready")
        ));
        assert_eq!(
            result.events_sent,
            vec![(entity.to_string(), "direct".to_string())]
        );
    }

    #[test]
    fn animation_event_is_dispatched_only_to_the_firing_entity() {
        use crate::animation::{
            animation_system, AnimEvent, AnimationClip, AnimationEvents, Animator,
        };
        use crate::asset::Assets;

        let firing_script = sample_asset_id();
        let other_script = sample_asset_id();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &firing_script,
                r#"fn on_event(ctx, name) {
                    if name == "hit" {
                        ctx.set_component(ctx.self_entity(), "engine.transform", #{ x: 10.0 });
                    }
                }"#,
            )
            .expect("firing script must compile");
        script_engine
            .compile(
                &other_script,
                r#"fn on_event(ctx, name) {
                    ctx.set_component(ctx.self_entity(), "engine.transform", #{ x: 99.0 });
                }"#,
            )
            .expect("other script must compile");

        let mut clips = Assets::<AnimationClip>::default();
        let clip = clips.add(AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: vec![AnimEvent {
                time: 0.5,
                name: "hit".to_string(),
            }],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::with_delta(0.6));
        app.insert_resource(Input::<KeyCode>::new());
        app.insert_resource(script_engine);
        app.insert_resource(clips);
        app.insert_resource(AnimationEvents::default());

        let firing_entity = app
            .world_mut()
            .spawn_with(ScriptComponent::new(firing_script))
            .expect("firing entity must spawn");
        app.world_mut()
            .add_component(firing_entity, Transform::default())
            .expect("firing entity must accept Transform");
        app.world_mut()
            .add_component(firing_entity, Animator::playing(clip))
            .expect("firing entity must accept Animator");

        let other_entity = app
            .world_mut()
            .spawn_with(ScriptComponent::new(other_script))
            .expect("other entity must spawn");
        app.world_mut()
            .add_component(other_entity, Transform::default())
            .expect("other entity must accept Transform");

        app.add_system(animation_system);
        app.add_system(scripting_update_system);
        app.update().expect("animation event dispatch must run");

        let firing_transform = app
            .world()
            .get_component::<Transform>(firing_entity)
            .expect("firing transform must exist");
        let other_transform = app
            .world()
            .get_component::<Transform>(other_entity)
            .expect("other transform must exist");
        assert_eq!(firing_transform.translation.x, 10.0);
        assert_eq!(
            other_transform.translation.x, 0.0,
            "animation events must not be broadcast to unrelated scripts"
        );
    }

    #[test]
    fn input_pressed_returns_snapshotted_key_state() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_update(ctx, dt) {
                    if ctx.input_pressed("jump") { ctx.log("jumped"); }
                }"#,
            )
            .unwrap();

        let mut input_map = HashMap::new();
        input_map.insert("jump".to_string(), true);
        let result = engine.run_on_update(sample_entity(), &id, 0.016, Arc::new(input_map), None);
        assert_eq!(result.logs, vec!["jumped".to_string()]);
    }

    #[test]
    fn ast_not_found_returns_error_without_panic() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert!(matches!(result.error, Some(ScriptError::AstNotFound(_))));
    }

    #[test]
    fn set_component_queues_transform_command() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.set_component(ctx.self_entity(), "engine.transform", #{ x: 5.0, y: 1.0, z: 0.0 });
                }"#,
            )
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert_eq!(result.component_sets.len(), 1);
        assert_eq!(result.component_sets[0].component, "engine.transform");
        let x = result.component_sets[0]
            .fields
            .get("x")
            .copied()
            .unwrap_or(0.0);
        assert!((x - 5.0).abs() < 1e-6, "x field must be 5.0, got {x}");
    }

    #[test]
    fn get_component_returns_transform_snapshot() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    let t = ctx.get_component(ctx.self_entity(), "engine.transform");
                    ctx.log(t.x.to_string());
                }"#,
            )
            .unwrap();

        let mut snap = RhaiMap::new();
        snap.insert("x".into(), Dynamic::from_float(3.0_f64));
        snap.insert("y".into(), Dynamic::from_float(0.0_f64));
        snap.insert("z".into(), Dynamic::from_float(0.0_f64));

        let result = engine.run_on_start(
            sample_entity(),
            &id,
            Arc::new(HashMap::new()),
            Some(Arc::new(snap)),
        );
        assert!(
            result.logs.iter().any(|l| l.contains('3')),
            "transform x must be logged as 3, logs={:?}",
            result.logs
        );
    }

    #[test]
    fn is_started_tracks_first_dispatch() {
        let mut engine = ScriptEngine::default();
        let entity = sample_entity();
        assert!(!engine.is_started(entity));
        engine.mark_started(entity);
        assert!(engine.is_started(entity));
        engine.reset_instances();
        assert!(!engine.is_started(entity));
    }

    #[test]
    fn build_input_snapshot_contains_all_actions() {
        let input: Input<KeyCode> = Input::new();
        let snap = build_input_snapshot(&input);
        for action in ["left", "right", "up", "down", "jump", "interact"] {
            assert!(
                snap.contains_key(action),
                "snapshot must contain action '{action}'"
            );
        }
    }

    #[test]
    fn transform_to_rhai_map_round_trips_translation() {
        let t = Transform::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let map = transform_to_rhai_map(&t);
        let x = map.get("x").and_then(|v| v.as_float().ok()).unwrap_or(0.0);
        let y = map.get("y").and_then(|v| v.as_float().ok()).unwrap_or(0.0);
        assert!((x - 1.0).abs() < 1e-6);
        assert!((y - 2.0).abs() < 1e-6);
    }

    #[test]
    fn apply_transform_fields_updates_translation() {
        let mut t = Transform::default();
        let mut fields = HashMap::new();
        fields.insert("x".to_string(), 7.0_f64);
        fields.insert("y".to_string(), -3.0_f64);
        apply_transform_fields(&mut t, &fields);
        assert!((t.translation.x - 7.0).abs() < 1e-6);
        assert!((t.translation.y + 3.0).abs() < 1e-6);
    }

    #[test]
    fn script_error_display_includes_diagnostic_code() {
        let err = ScriptError::Compile("unexpected token".to_string());
        assert!(err.to_string().contains("script.compile_error"));
        let err2 = ScriptError::MaxOperationsExceeded;
        assert!(err2.to_string().contains("script.max_operations_exceeded"));
    }

    // --- Save script API (Phase 56 / ADR 0048) ------------------------------

    #[test]
    fn save_get_reads_pre_call_snapshot_and_missing_key_resolves_to_unit() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.log(ctx.save_get("gold").to_string());
                    if ctx.save_get("missing") == () {
                        ctx.log("missing_is_unit");
                    }
                }"#,
            )
            .unwrap();

        let mut snapshot = SaveData::new();
        snapshot.set("gold", SaveValue::Number(50.0));
        engine.set_save_snapshot(Some(Arc::new(snapshot)));

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert_eq!(
            result.logs,
            vec!["50.0".to_string(), "missing_is_unit".to_string()]
        );
    }

    #[test]
    fn save_set_queues_text_number_and_flag_commands() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.save_set("name", "Rin");
                    ctx.save_set("score", 42);
                    ctx.save_set("hardcore", true);
                }"#,
            )
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert!(result.save_errors.is_empty());
        assert_eq!(result.save_sets.len(), 3);
        assert_eq!(result.save_sets[0].key, "name");
        assert_eq!(
            result.save_sets[0].value,
            SaveValue::Text("Rin".to_string())
        );
        assert_eq!(result.save_sets[1].key, "score");
        assert_eq!(result.save_sets[1].value, SaveValue::Number(42.0));
        assert_eq!(result.save_sets[2].key, "hardcore");
        assert_eq!(result.save_sets[2].value, SaveValue::Flag(true));
    }

    #[test]
    fn save_set_with_array_value_records_error_and_queues_nothing() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) { ctx.save_set("bad", [1, 2, 3]); }"#,
            )
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert!(result.save_sets.is_empty());
        assert_eq!(result.save_errors.len(), 1);
        assert!(result.save_errors[0].contains("bad"));
    }

    #[test]
    fn save_write_and_save_load_queue_persist_commands_for_valid_slots() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.save_write(0);
                    ctx.save_load(2);
                }"#,
            )
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert!(result.save_errors.is_empty());
        assert_eq!(result.save_ops.len(), 2);
        assert!(matches!(
            result.save_ops[0],
            SavePersistCommand::Write { slot: 0 }
        ));
        assert!(matches!(
            result.save_ops[1],
            SavePersistCommand::Load { slot: 2 }
        ));
    }

    #[test]
    fn save_write_with_negative_slot_records_error_and_queues_nothing() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(&id, r#"fn on_start(ctx) { ctx.save_write(-1); }"#)
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert!(result.save_ops.is_empty());
        assert_eq!(result.save_errors.len(), 1);
    }

    #[test]
    fn apply_save_commands_applies_set_before_write_in_the_same_call() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        let mut data = SaveData::new();

        let result = ScriptCallResult {
            save_sets: vec![SaveSetCommand {
                key: "chapter".to_string(),
                value: SaveValue::Number(3.0),
            }],
            save_ops: vec![SavePersistCommand::Write { slot: 0 }],
            ..Default::default()
        };

        apply_save_commands(&result, Some(&mut data), Some(&mut store));

        assert_eq!(data.get("chapter"), Some(&SaveValue::Number(3.0)));
        let loaded = store
            .read_slot(0)
            .expect("write must have produced a readable slot");
        assert_eq!(loaded.get("chapter"), Some(&SaveValue::Number(3.0)));
    }

    #[test]
    fn apply_save_commands_without_a_save_store_does_not_panic() {
        let mut data = SaveData::new();
        let result = ScriptCallResult {
            save_ops: vec![SavePersistCommand::Write { slot: 0 }],
            ..Default::default()
        };
        apply_save_commands(&result, Some(&mut data), None);
    }

    #[test]
    fn save_load_restores_values_that_a_later_save_get_snapshot_can_read() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut store = SaveStore::new(dir.path().to_path_buf());
        let mut written = SaveData::new();
        written.set("chapter", SaveValue::Number(7.0));
        store.write_slot(0, &written).expect("write must succeed");

        // Fresh "session": a new engine only knows the slot exists on disk.
        let mut engine = ScriptEngine::default();
        let load_script = sample_asset_id();
        engine
            .compile(&load_script, r#"fn on_start(ctx) { ctx.save_load(0); }"#)
            .expect("load script must compile");
        let load_result = engine.run_on_start(
            sample_entity(),
            &load_script,
            Arc::new(HashMap::new()),
            None,
        );

        let mut save_data = SaveData::new();
        apply_save_commands(&load_result, Some(&mut save_data), Some(&mut store));
        assert_eq!(save_data.get("chapter"), Some(&SaveValue::Number(7.0)));

        // The next dispatch cycle's snapshot reflects the just-loaded state.
        engine.set_save_snapshot(Some(Arc::new(save_data)));
        let read_script = sample_asset_id();
        engine
            .compile(
                &read_script,
                r#"fn on_start(ctx) { ctx.log(ctx.save_get("chapter").to_string()); }"#,
            )
            .expect("read script must compile");
        let read_result = engine.run_on_start(
            sample_entity(),
            &read_script,
            Arc::new(HashMap::new()),
            None,
        );
        assert_eq!(read_result.logs, vec!["7.0".to_string()]);
    }

    #[test]
    fn scripting_update_system_applies_save_set_then_save_write_to_disk_and_resource() {
        let dir = tempfile::tempdir().expect("must create temp dir");

        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::default());
        app.insert_resource(Input::<KeyCode>::new());
        app.insert_resource(SaveData::default());
        app.insert_resource(SaveStore::new(dir.path().to_path_buf()));

        let asset_id = sample_asset_id();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &asset_id,
                r#"fn on_start(ctx) {
                    ctx.save_set("chapter", 5);
                    ctx.save_write(0);
                }"#,
            )
            .expect("script must compile");
        app.insert_resource(script_engine);

        app.world_mut()
            .spawn_with(ScriptComponent::new(asset_id))
            .expect("must spawn script entity");

        app.add_system(scripting_update_system);
        app.update().expect("scripting_update_system must run");

        let save_data = app
            .world()
            .get_resource::<SaveData>()
            .expect("SaveData resource must remain present");
        assert_eq!(save_data.get("chapter"), Some(&SaveValue::Number(5.0)));

        let mut store = SaveStore::new(dir.path().to_path_buf());
        let loaded = store
            .read_slot(0)
            .expect("slot 0 file must exist on disk after save_write");
        assert_eq!(loaded.get("chapter"), Some(&SaveValue::Number(5.0)));
    }

    #[test]
    fn scripting_update_system_save_set_with_array_value_leaves_resource_unchanged() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::default());
        app.insert_resource(Input::<KeyCode>::new());
        app.insert_resource(SaveData::default());

        let asset_id = sample_asset_id();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &asset_id,
                r#"fn on_start(ctx) { ctx.save_set("bad", [1, 2]); }"#,
            )
            .expect("script must compile");
        app.insert_resource(script_engine);

        app.world_mut()
            .spawn_with(ScriptComponent::new(asset_id))
            .expect("must spawn script entity");

        app.add_system(scripting_update_system);
        app.update()
            .expect("update must not fail even though save_set was rejected");

        let save_data = app
            .world()
            .get_resource::<SaveData>()
            .expect("SaveData resource must remain present");
        assert!(save_data.get("bad").is_none());
    }

    #[test]
    fn scripting_update_system_save_write_without_a_save_store_does_not_panic() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(FixedTime::default());
        app.insert_resource(Input::<KeyCode>::new());
        app.insert_resource(SaveData::default());
        // No SaveStore resource inserted.

        let asset_id = sample_asset_id();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(&asset_id, r#"fn on_start(ctx) { ctx.save_write(0); }"#)
            .expect("script must compile");
        app.insert_resource(script_engine);

        app.world_mut()
            .spawn_with(ScriptComponent::new(asset_id))
            .expect("must spawn script entity");

        app.add_system(scripting_update_system);
        assert!(app.update().is_ok());
    }

    // --- Collision script API (Phase 57) ------------------------------------

    #[test]
    fn ctx_collisions_returns_other_push_and_is_trigger_for_colliding_entity() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    let list = ctx.collisions();
                    ctx.log(list.len().to_string());
                    ctx.log(list[0].other);
                    ctx.log(list[0].push_x.to_string());
                    ctx.log(list[0].is_trigger.to_string());
                }"#,
            )
            .unwrap();

        let self_entity = sample_entity();
        let other_entity = sample_entity();
        let mut snapshot = HashMap::new();
        snapshot.insert(
            self_entity,
            Arc::new(vec![CollisionInfo {
                other: other_entity,
                push: glam::Vec3::new(1.0, 0.0, 0.0),
                is_trigger: true,
            }]),
        );
        engine.set_collision_snapshot(snapshot);

        let result = engine.run_on_start(self_entity, &id, Arc::new(HashMap::new()), None);
        assert_eq!(result.logs[0], "1");
        assert_eq!(result.logs[1], format!("{other_entity}"));
        assert_eq!(result.logs[2], "1.0");
        assert_eq!(result.logs[3], "true");
    }

    #[test]
    fn ctx_collisions_is_empty_array_when_entity_has_no_collisions() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.log(ctx.collisions().len().to_string());
                }"#,
            )
            .unwrap();

        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert_eq!(result.logs, vec!["0".to_string()]);
    }

    #[test]
    fn ctx_collisions_is_empty_when_no_snapshot_was_set() {
        let mut engine = ScriptEngine::default();
        let id = sample_asset_id();
        engine
            .compile(
                &id,
                r#"fn on_start(ctx) {
                    ctx.log(ctx.collisions().len().to_string());
                }"#,
            )
            .unwrap();

        // No `set_collision_snapshot` call at all: the entity must still see
        // an empty array rather than panicking.
        let result = engine.run_on_start(sample_entity(), &id, Arc::new(HashMap::new()), None);
        assert_eq!(result.logs, vec!["0".to_string()]);
    }

    #[test]
    fn arc_collision_snapshot_negates_push_for_the_second_entity() {
        let mut app = engine_ecs::App::new();
        let entity_a = app.world_mut().spawn().expect("spawn a");
        let entity_b = app.world_mut().spawn().expect("spawn b");
        app.insert_resource(FixedTime::default());
        app.insert_resource(Input::<KeyCode>::new());

        // `CollisionEvents::push` is private outside `collision.rs`, so this
        // test drives the same public entry point `arc_collision_snapshot`
        // uses (`collisions_by_entity`) through a resource built via the
        // real detection system instead of poking private fields.
        let floor_transform = Transform::default();
        app.world_mut()
            .add_component(entity_a, floor_transform.clone())
            .expect("attach transform to a");
        app.world_mut()
            .add_component(
                entity_a,
                crate::transform::GlobalTransform(floor_transform.to_matrix()),
            )
            .expect("attach global transform to a");
        app.world_mut()
            .add_component(entity_a, crate::collision::Collider::aabb_cube(1.0))
            .expect("attach collider to a");
        app.world_mut()
            .add_component(entity_a, crate::collision::PhysicsBody::Static)
            .expect("attach physics body to a");

        let b_transform = Transform::from_translation(glam::Vec3::new(1.5, 0.0, 0.0));
        app.world_mut()
            .add_component(entity_b, b_transform.clone())
            .expect("attach transform to b");
        app.world_mut()
            .add_component(
                entity_b,
                crate::transform::GlobalTransform(b_transform.to_matrix()),
            )
            .expect("attach global transform to b");
        app.world_mut()
            .add_component(entity_b, crate::collision::Collider::aabb_cube(1.0))
            .expect("attach collider to b");
        app.world_mut()
            .add_component(entity_b, crate::collision::PhysicsBody::Static)
            .expect("attach physics body to b");

        app.insert_resource(crate::collision::CollisionEvents::default());
        app.add_system(crate::collision::collision_detection_system);
        app.update().expect("collision detection must run");

        let world = app.world();
        let stored = world
            .get_resource::<crate::collision::CollisionEvents>()
            .expect("collision events resource must remain present");
        // Sanity: the two overlapping cubes must have produced exactly one event.
        assert_eq!(stored.iter().count(), 1);

        let snapshot = arc_collision_snapshot(Some(stored));
        let info_a = &snapshot.get(&entity_a).expect("a must have an entry")[0];
        let info_b = &snapshot.get(&entity_b).expect("b must have an entry")[0];
        assert_eq!(info_a.push, -info_b.push);
    }
}
