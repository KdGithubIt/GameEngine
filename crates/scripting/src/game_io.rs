//! Query-scoped input and deferred-output contracts for project Rust systems.
//!
//! These types are serialized inside the bounded ABI buffers defined by ADR
//! 0052. They intentionally contain copied values and stable identifiers only;
//! host ECS components and Rust references never cross the module boundary.

use engine_authoring::{ComponentTypeId, EntityId, Value};
use engine_ecs::{SystemId, SystemIdError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Schema version for [`GameInvocation`] and [`GameInvocationOutput`].
pub const GAME_IO_SCHEMA_VERSION: u32 = 1;
/// Maximum entity rows supplied to one project-system invocation.
pub const MAX_GAME_QUERY_ROWS: usize = 16_384;
/// Maximum encoded input bytes supplied to one project-system invocation.
pub const MAX_GAME_INPUT_BYTES: usize = 1024 * 1024;
/// Maximum encoded output bytes accepted from one project-system invocation.
pub const MAX_GAME_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum deferred commands accepted from one project-system invocation.
pub const MAX_GAME_COMMANDS: usize = 1_024;
/// Maximum event records retained by the host and copied into one invocation.
///
/// This is a second line of defense in addition to the encoded-byte limit:
/// tiny events must not be able to create an unbounded in-memory queue while
/// a project system is paused or neglects to acknowledge its cursor.
pub const MAX_GAME_EVENT_RECORDS: usize = 4_096;
/// Maximum UTF-8 byte length of one explicitly declared save key.
pub const MAX_GAME_SAVE_KEY_BYTES: usize = 256;

/// Declares whether project code only observes or may patch a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameAccessMode {
    /// The value is copied into callback input and cannot be patched.
    Read,
    /// The value is copied into callback input and may be patched in output.
    Write,
}

/// Engine-owned copied views available to an entity query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineViewKind {
    /// Stable authoring identity when the runtime entity came from authoring data.
    AuthoringIdentity,
    /// Local translation, rotation, and scale.
    Transform,
    /// World-space translation, rotation, and scale after propagation.
    GlobalTransform,
    /// Character velocity, desired movement, facing, and grounded state.
    CharacterState,
    /// Current animator clip, playback position, and transition state.
    AnimationState,
    /// Current lock-on target and selection state.
    LockOnState,
    /// Attack-hitbox owner, team, damage, activation, and enabled state.
    HitboxState,
    /// Health, team, and invulnerability state for a combat target.
    DamageReceiverState,
    /// Current navigation path, waypoint, and completion state.
    NavigationState,
    /// Latest Behavior Tree status, diagnostics, blackboard, and visited leaves.
    BehaviorTreeState,
    /// Current UI binding values associated with the entity.
    UiBindings,
}

/// Host event streams that a project system may consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameEventStream {
    /// Collision enter, stay, and exit records.
    Collision,
    /// Accepted combat hit results after team and invulnerability filtering.
    Hit,
    /// Animation events emitted while sampling clips.
    Animation,
    /// UI button and value-change events.
    Ui,
    /// Completed or failed deferred prefab spawn requests.
    SpawnResult,
    /// Scene transition lifecycle events.
    Scene,
    /// Gameplay timer completion events.
    Timer,
    /// Targeted and broadcast project-defined events.
    Game,
}

/// Host command services a project system is authorized to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameCommandFamily {
    /// Local transform translation, rotation, and scale changes.
    Transform,
    /// Desired character-controller velocity and facing changes.
    Character,
    /// NavMeshAgent target assignment and clearing.
    Navigation,
    /// Stable Behavior Tree action and condition result registration.
    BehaviorTree,
    /// Deferred prefab spawning and result delivery.
    PrefabSpawn,
    /// Deferred runtime entity removal.
    Despawn,
    /// Safe component add, remove, enable, and disable operations.
    Component,
    /// Animation clip and Animation Graph control.
    Animation,
    /// Attack hitbox creation, activation, and removal.
    Hitbox,
    /// Sound effect, music, and mixer control.
    Audio,
    /// Lock-on acquire, cycle, and release operations.
    LockOn,
    /// UI binding mutation and document visibility operations.
    Ui,
    /// Scene transition requests.
    Scene,
    /// Versioned save-slot read and write operations.
    Save,
    /// Gameplay timer creation, cancellation, and queries.
    Timer,
    /// Targeted and broadcast project-defined event emission.
    GameEvent,
}

/// Collision shape for a command-created attack hitbox.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameHitboxShape {
    /// Axis-aligned box centered on the carrier entity.
    Aabb {
        /// Positive half-size on each local axis.
        half_extents: [f32; 3],
    },
    /// Sphere centered on the carrier entity.
    Sphere {
        /// Positive local-space radius.
        radius: f32,
    },
    /// Y-axis capsule centered on the carrier entity.
    CapsuleY {
        /// Non-negative half-length of the core segment excluding caps.
        half_height: f32,
        /// Positive local-space cap and core radius.
        radius: f32,
    },
}

/// Distance attenuation curve exposed to project Rust spatial-audio requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameAudioRolloffMode {
    /// Linear attenuation from minimum to maximum distance.
    Linear,
    /// Inverse-distance attenuation normalized to the authored distance range.
    Inverse,
}

/// Typed spatial-audio policy copied into a deferred project Rust command.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GameSpatialAudioOptions {
    /// Per-voice gain from 0.0 to 1.0.
    pub volume: f32,
    /// Blend from centered 2D (0.0) to positional 3D (1.0).
    pub spatial_blend: f32,
    /// Distance where positional attenuation begins.
    pub min_distance: f32,
    /// Distance where positional attenuation reaches silence.
    pub max_distance: f32,
    /// Distance attenuation curve.
    pub rolloff: GameAudioRolloffMode,
    /// Whether the managed voice repeats until its source disappears.
    pub looping: bool,
}

impl Default for GameSpatialAudioOptions {
    fn default() -> Self {
        Self {
            volume: 1.0,
            spatial_blend: 1.0,
            min_distance: 1.0,
            max_distance: 20.0,
            rolloff: GameAudioRolloffMode::Linear,
            looping: false,
        }
    }
}

/// Behavior Tree leaf result supplied by project Rust gameplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameBehaviorStatus {
    /// The behavior completed successfully.
    Success,
    /// The behavior completed unsuccessfully.
    Failure,
    /// The behavior remains active for a later tick.
    Running,
}

/// Access declaration for one project-defined component in an entity query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameComponentAccess {
    /// Stable authoring component type identifier.
    pub component_type: ComponentTypeId,
    /// Whether the callback only reads or may patch this component.
    pub mode: GameAccessMode,
    /// Whether entities without this component are excluded from the query.
    pub required: bool,
}

/// Access declaration for one host-owned game resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameResourceAccess {
    /// Stable dotted resource ID declared by the project module.
    pub id: String,
    /// Whether the callback only reads or may patch this resource.
    pub mode: GameAccessMode,
}

/// Engine-owned global state that can be copied into a project callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameHostViewKind {
    /// Current scene path, pending request, generation, and switch outcome.
    SceneState,
}

/// Access declaration for one engine-owned copied entity view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEngineViewAccess {
    /// Engine view copied into a matching row.
    pub view: EngineViewKind,
    /// Whether entities missing this engine component are excluded.
    pub required: bool,
}

/// One query compiled by the host for a project-system callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameQueryAccess {
    /// Stable dotted query ID used in payloads and profiler records.
    pub id: String,
    /// Project components included in each matching row.
    pub components: Vec<GameComponentAccess>,
    /// Engine-owned copied views included in each matching row.
    pub engine_views: Vec<GameEngineViewAccess>,
}

/// Complete data and service declaration for one project system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSystemAccess {
    /// Project Settings input actions copied for the callback.
    pub input_actions: Vec<String>,
    /// Active-save values copied for the callback. Missing keys are omitted.
    pub save_keys: Vec<String>,
    /// Entity queries captured immediately before the callback.
    pub queries: Vec<GameQueryAccess>,
    /// Host-owned game resources copied for the callback.
    pub resources: Vec<GameResourceAccess>,
    /// Engine-owned global views copied for the callback.
    pub host_views: Vec<GameHostViewKind>,
    /// Event streams copied for the callback.
    pub event_streams: Vec<GameEventStream>,
    /// Deferred command families the host accepts from the callback.
    pub command_families: Vec<GameCommandFamily>,
}

impl GameSystemAccess {
    /// Validates stable IDs and rejects ambiguous duplicate declarations.
    ///
    /// Empty access is valid for systems that need no copied host data or services.
    ///
    /// # Errors
    ///
    /// Returns [`GameAccessError`] when an ID is invalid or the same query,
    /// component, resource, view, event stream, or command family is repeated.
    pub fn validate(&self) -> Result<(), GameAccessError> {
        let mut input_actions = BTreeSet::new();
        for action in &self.input_actions {
            if action.is_empty() {
                return Err(GameAccessError::EmptyInputAction);
            }
            if !input_actions.insert(action.clone()) {
                return Err(GameAccessError::DuplicateInputAction(action.clone()));
            }
        }

        let mut save_keys = BTreeSet::new();
        for key in &self.save_keys {
            if key.trim().is_empty()
                || key.len() > MAX_GAME_SAVE_KEY_BYTES
                || key.chars().any(char::is_control)
            {
                return Err(GameAccessError::InvalidSaveKey(key.clone()));
            }
            if !save_keys.insert(key.clone()) {
                return Err(GameAccessError::DuplicateSaveKey(key.clone()));
            }
        }

        let mut query_ids = BTreeSet::new();
        for query in &self.queries {
            validate_dotted_id(&query.id).map_err(|source| GameAccessError::InvalidQueryId {
                id: query.id.clone(),
                source,
            })?;
            if !query_ids.insert(query.id.clone()) {
                return Err(GameAccessError::DuplicateQuery(query.id.clone()));
            }

            let mut component_ids = BTreeSet::new();
            for component in &query.components {
                if !component_ids.insert(component.component_type.clone()) {
                    return Err(GameAccessError::DuplicateQueryComponent {
                        query: query.id.clone(),
                        component_type: component.component_type.clone(),
                    });
                }
            }
            let mut engine_views = BTreeSet::new();
            for access in &query.engine_views {
                if !engine_views.insert(access.view) {
                    return Err(GameAccessError::DuplicateEngineView {
                        query: query.id.clone(),
                        view: access.view,
                    });
                }
            }
        }

        let mut resource_ids = BTreeSet::new();
        for resource in &self.resources {
            validate_dotted_id(&resource.id).map_err(|source| {
                GameAccessError::InvalidResourceId {
                    id: resource.id.clone(),
                    source,
                }
            })?;
            if !resource_ids.insert(resource.id.clone()) {
                return Err(GameAccessError::DuplicateResource(resource.id.clone()));
            }
        }
        reject_duplicate_copy_values(&self.host_views)
            .map_err(GameAccessError::DuplicateHostView)?;
        reject_duplicate_copy_values(&self.event_streams)
            .map_err(GameAccessError::DuplicateEventStream)?;
        reject_duplicate_copy_values(&self.command_families)
            .map_err(GameAccessError::DuplicateCommandFamily)?;
        Ok(())
    }
}

fn validate_dotted_id(id: &str) -> Result<(), SystemIdError> {
    SystemId::try_new(id.to_owned()).map(|_| ())
}

fn reject_duplicate_copy_values<T>(values: &[T]) -> Result<(), T>
where
    T: Copy + Ord,
{
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(*value) {
            return Err(*value);
        }
    }
    Ok(())
}

/// Reports an invalid or ambiguous project-system access declaration.
#[derive(Debug)]
pub enum GameAccessError {
    /// An input action declaration uses an empty Project Settings action name.
    EmptyInputAction,
    /// The same Project Settings input action is declared more than once.
    DuplicateInputAction(String),
    /// A save key is empty, too long, or contains a control character.
    InvalidSaveKey(String),
    /// The same active-save key is declared more than once.
    DuplicateSaveKey(String),
    /// A query ID is not a valid stable dotted ID.
    InvalidQueryId {
        /// Rejected query ID.
        id: String,
        /// Stable-ID validation error.
        source: SystemIdError,
    },
    /// A resource ID is not a valid stable dotted ID.
    InvalidResourceId {
        /// Rejected resource ID.
        id: String,
        /// Stable-ID validation error.
        source: SystemIdError,
    },
    /// Two queries use the same ID.
    DuplicateQuery(String),
    /// One query declares the same project component more than once.
    DuplicateQueryComponent {
        /// Query containing the duplicate.
        query: String,
        /// Repeated component type.
        component_type: ComponentTypeId,
    },
    /// One query declares the same engine view more than once.
    DuplicateEngineView {
        /// Query containing the duplicate.
        query: String,
        /// Repeated engine view.
        view: EngineViewKind,
    },
    /// Two resource declarations use the same ID.
    DuplicateResource(String),
    /// The same global host view is declared more than once.
    DuplicateHostView(GameHostViewKind),
    /// The same event stream is declared more than once.
    DuplicateEventStream(GameEventStream),
    /// The same command family is declared more than once.
    DuplicateCommandFamily(GameCommandFamily),
}

impl fmt::Display for GameAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInputAction => formatter.write_str("game input action name cannot be empty"),
            Self::DuplicateInputAction(action) => {
                write!(formatter, "duplicate game input action `{action}`")
            }
            Self::InvalidSaveKey(key) => write!(
                formatter,
                "invalid game save key `{key}` (must be non-empty, contain no control characters, and be at most {MAX_GAME_SAVE_KEY_BYTES} UTF-8 bytes)"
            ),
            Self::DuplicateSaveKey(key) => write!(formatter, "duplicate game save key `{key}`"),
            Self::InvalidQueryId { id, source } => {
                write!(formatter, "invalid game query ID `{id}`: {source}")
            }
            Self::InvalidResourceId { id, source } => {
                write!(formatter, "invalid game resource ID `{id}`: {source}")
            }
            Self::DuplicateQuery(id) => write!(formatter, "duplicate game query `{id}`"),
            Self::DuplicateQueryComponent {
                query,
                component_type,
            } => write!(
                formatter,
                "game query `{query}` repeats component `{component_type}`"
            ),
            Self::DuplicateEngineView { query, view } => {
                write!(
                    formatter,
                    "game query `{query}` repeats engine view `{view:?}`"
                )
            }
            Self::DuplicateResource(id) => write!(formatter, "duplicate game resource `{id}`"),
            Self::DuplicateHostView(view) => {
                write!(formatter, "duplicate game host view `{view:?}`")
            }
            Self::DuplicateEventStream(stream) => {
                write!(formatter, "duplicate game event stream `{stream:?}`")
            }
            Self::DuplicateCommandFamily(family) => {
                write!(formatter, "duplicate game command family `{family:?}`")
            }
        }
    }
}

impl std::error::Error for GameAccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidQueryId { source, .. } | Self::InvalidResourceId { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

/// Generation-checked runtime entity handle copied across the module boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GameEntityHandle {
    /// Process-local ECS entity index.
    pub id: u32,
    /// Generation that prevents a stale handle from targeting a reused index.
    pub generation: u32,
}

/// Frame and fixed-step timing copied for one callback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct GameClock {
    /// Rendered-frame delta in seconds.
    pub delta_seconds: f32,
    /// Fixed simulation delta in seconds.
    pub fixed_delta_seconds: f32,
    /// Seconds elapsed since this runtime started.
    pub elapsed_seconds: f64,
    /// Rendered-frame index.
    pub frame_index: u64,
    /// Fixed-step index, including all catch-up steps.
    pub fixed_step_index: u64,
}

/// Resolved action state copied from project settings and active input devices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct GameInputActionState {
    /// Whether a digital source is held or an analog source exceeds deadzone.
    pub pressed: bool,
    /// Whether the action became pressed during the current input frame.
    pub just_pressed: bool,
    /// Whether the action became released during the current input frame.
    pub just_released: bool,
    /// Resolved scalar action value after deadzone, scale, and inversion.
    pub scalar: f32,
    /// Resolved two-dimensional action value.
    pub vector: [f32; 2],
}

/// One entity row returned by a declared query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameQueryRow {
    /// Generation-checked runtime entity handle.
    pub entity: GameEntityHandle,
    /// Stable authoring identity when this entity originated from authoring data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoring_id: Option<EntityId>,
    /// Requested project-component values keyed by stable component type.
    pub components: BTreeMap<ComponentTypeId, Value>,
    /// Requested engine views keyed by their ABI-stable view kind.
    pub engine_views: BTreeMap<EngineViewKind, Value>,
}

/// Rows captured for one declared query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameQueryResult {
    /// Query ID from [`GameQueryAccess`].
    pub query_id: String,
    /// Matching entity rows in stable runtime iteration order.
    pub rows: Vec<GameQueryRow>,
}

/// One host or project event copied into a callback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEventRecord {
    /// Event stream containing this record.
    pub stream: GameEventStream,
    /// Monotonic cursor used to acknowledge consumed records.
    pub sequence: u64,
    /// Stream-specific payload validated by the host.
    pub payload: Value,
}

/// Query-scoped callback input generated immediately before a project system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameInvocation {
    /// Payload schema version, currently [`GAME_IO_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable ID of the system receiving this input.
    pub system_id: String,
    /// Current rendered-frame and fixed-step clocks.
    pub clock: GameClock,
    /// Requested input actions keyed by Project Settings action name.
    pub input_actions: BTreeMap<String, GameInputActionState>,
    /// Values for explicitly declared active-save keys. Missing keys are absent.
    pub save_values: BTreeMap<String, Value>,
    /// Results for the system's declared entity queries.
    pub queries: Vec<GameQueryResult>,
    /// Requested host-owned game resources keyed by stable resource ID.
    pub resources: BTreeMap<String, Value>,
    /// Requested engine-owned global views keyed by stable view kind.
    pub host_views: BTreeMap<GameHostViewKind, Value>,
    /// Requested unconsumed event records.
    pub events: Vec<GameEventRecord>,
}

/// Patch for one writable project component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameComponentPatch {
    /// Entity whose project component is patched.
    pub entity: GameEntityHandle,
    /// Stable component type declared writable by the system.
    pub component_type: ComponentTypeId,
    /// Complete replacement value validated against the component schema.
    pub value: Value,
}

/// Patch for one writable host-owned game resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameResourcePatch {
    /// Stable resource ID declared writable by the system.
    pub resource_id: String,
    /// Complete replacement value validated against the resource schema.
    pub value: Value,
}

/// One host-validated deferred command requested by project code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameCommand {
    /// Service family used for access validation and schedule-boundary routing.
    pub family: GameCommandFamily,
    /// Optional caller-generated ID used by asynchronous result events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<u64>,
    /// Optional generation-checked target entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<GameEntityHandle>,
    /// Family-specific command payload validated by the host before application.
    pub payload: Value,
}

impl GameCommand {
    /// Replaces a target's complete local transform.
    pub fn set_transform(
        target: GameEntityHandle,
        translation: [f32; 3],
        rotation: [f32; 4],
        scale: [f32; 3],
    ) -> Self {
        Self {
            family: GameCommandFamily::Transform,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                ("operation".to_owned(), Value::String("set".to_owned())),
                ("translation".to_owned(), vector3_value(translation)),
                ("rotation".to_owned(), quaternion_value(rotation)),
                ("scale".to_owned(), vector3_value(scale)),
            ])),
        }
    }

    /// Adds a local-space translation delta to a target transform.
    pub fn translate(target: GameEntityHandle, delta: [f32; 3]) -> Self {
        Self {
            family: GameCommandFamily::Transform,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                (
                    "operation".to_owned(),
                    Value::String("translate".to_owned()),
                ),
                ("delta".to_owned(), vector3_value(delta)),
            ])),
        }
    }

    /// Applies a normalized quaternion delta after the current local rotation.
    pub fn rotate(target: GameEntityHandle, delta: [f32; 4]) -> Self {
        Self {
            family: GameCommandFamily::Transform,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                ("operation".to_owned(), Value::String("rotate".to_owned())),
                ("delta".to_owned(), quaternion_value(delta)),
            ])),
        }
    }

    /// Removes a live runtime entity after the callback succeeds.
    pub fn despawn(target: GameEntityHandle) -> Self {
        Self {
            family: GameCommandFamily::Despawn,
            request_id: None,
            target: Some(target),
            payload: Value::Null,
        }
    }

    /// Adds a project component using its active module schema default.
    pub fn add_game_component(target: GameEntityHandle, component_type: ComponentTypeId) -> Self {
        component_command("add", target, component_type)
    }

    /// Removes one runtime project component without changing authoring data.
    pub fn remove_game_component(
        target: GameEntityHandle,
        component_type: ComponentTypeId,
    ) -> Self {
        component_command("remove", target, component_type)
    }

    /// Makes a retained project component visible to project queries again.
    pub fn enable_game_component(
        target: GameEntityHandle,
        component_type: ComponentTypeId,
    ) -> Self {
        component_command("enable", target, component_type)
    }

    /// Retains a project component value while excluding it from queries.
    pub fn disable_game_component(
        target: GameEntityHandle,
        component_type: ComponentTypeId,
    ) -> Self {
        component_command("disable", target, component_type)
    }

    /// Sets a kinematic character's desired velocity and world-facing vector.
    pub fn set_character_motion(
        target: GameEntityHandle,
        velocity: [f32; 3],
        facing: [f32; 3],
    ) -> Self {
        Self {
            family: GameCommandFamily::Character,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                (
                    "operation".to_owned(),
                    Value::String("set_motion".to_owned()),
                ),
                ("velocity".to_owned(), vector3_value(velocity)),
                ("facing".to_owned(), vector3_value(facing)),
            ])),
        }
    }

    /// Assigns a world-space destination to an authorable NavMeshAgent.
    pub fn set_navigation_target(target: GameEntityHandle, destination: [f32; 3]) -> Self {
        Self {
            family: GameCommandFamily::Navigation,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                (
                    "operation".to_owned(),
                    Value::String("set_target".to_owned()),
                ),
                ("target".to_owned(), vector3_value(destination)),
            ])),
        }
    }

    /// Clears the destination and path of an authorable NavMeshAgent.
    pub fn clear_navigation_target(target: GameEntityHandle) -> Self {
        simple_target_command(GameCommandFamily::Navigation, "clear_target", target)
    }

    /// Registers the current result for one stable Behavior Tree action ID.
    pub fn set_behavior_tree_action(
        behavior_id: impl Into<String>,
        status: GameBehaviorStatus,
    ) -> Self {
        behavior_tree_result_command("action", behavior_id.into(), status)
    }

    /// Registers the current result for one stable Behavior Tree condition ID.
    pub fn set_behavior_tree_condition(
        behavior_id: impl Into<String>,
        status: GameBehaviorStatus,
    ) -> Self {
        behavior_tree_result_command("condition", behavior_id.into(), status)
    }

    /// Requests a prefab spawn and a later `SpawnResult` event.
    pub fn spawn_prefab(path: impl Into<String>, position: [f32; 3], request_id: u64) -> Self {
        Self {
            family: GameCommandFamily::PrefabSpawn,
            request_id: Some(request_id),
            target: None,
            payload: Value::Object(BTreeMap::from([
                ("operation".to_owned(), Value::String("spawn".to_owned())),
                ("path".to_owned(), Value::String(path.into())),
                ("position".to_owned(), vector3_value(position)),
            ])),
        }
    }

    /// Requests selection of the nearest valid lock-on target.
    pub fn acquire_lock_on() -> Self {
        lock_on_command("acquire")
    }

    /// Requests selection of the next valid lock-on target.
    pub fn cycle_lock_on() -> Self {
        lock_on_command("cycle")
    }

    /// Requests release of the current lock-on target.
    pub fn release_lock_on() -> Self {
        lock_on_command("release")
    }

    /// Starts or resumes the animator's current clip.
    pub fn play_animation(target: GameEntityHandle, looping: bool) -> Self {
        animation_command(
            target,
            BTreeMap::from([
                ("operation".to_owned(), Value::String("play".to_owned())),
                ("looping".to_owned(), Value::Bool(looping)),
            ]),
        )
    }

    /// Crossfades to a runtime clip ID observed through `AnimationState`.
    pub fn crossfade_animation(
        target: GameEntityHandle,
        clip_runtime_id: u64,
        duration_seconds: f32,
        looping: bool,
    ) -> Self {
        animation_command(
            target,
            BTreeMap::from([
                (
                    "operation".to_owned(),
                    Value::String("crossfade".to_owned()),
                ),
                (
                    "clip_runtime_id".to_owned(),
                    Value::String(clip_runtime_id.to_string()),
                ),
                (
                    "duration_seconds".to_owned(),
                    Value::F64(f64::from(duration_seconds)),
                ),
                ("looping".to_owned(), Value::Bool(looping)),
            ]),
        )
    }

    /// Stops an animator and resets its playback position.
    pub fn stop_animation(target: GameEntityHandle) -> Self {
        animation_command(
            target,
            BTreeMap::from([("operation".to_owned(), Value::String("stop".to_owned()))]),
        )
    }

    /// Creates an initially enabled trigger hitbox on an empty carrier entity.
    #[allow(clippy::too_many_arguments)]
    pub fn create_hitbox(
        target: GameEntityHandle,
        owner: GameEntityHandle,
        shape: GameHitboxShape,
        team: i32,
        damage: f32,
        membership: u32,
        mask: u32,
        one_hit_per_target: bool,
    ) -> Self {
        let shape = match shape {
            GameHitboxShape::Aabb { half_extents } => Value::Object(BTreeMap::from([
                ("kind".to_owned(), Value::String("aabb".to_owned())),
                ("half_extents".to_owned(), vector3_value(half_extents)),
            ])),
            GameHitboxShape::Sphere { radius } => Value::Object(BTreeMap::from([
                ("kind".to_owned(), Value::String("sphere".to_owned())),
                ("radius".to_owned(), Value::F64(f64::from(radius))),
            ])),
            GameHitboxShape::CapsuleY {
                half_height,
                radius,
            } => Value::Object(BTreeMap::from([
                ("kind".to_owned(), Value::String("capsule_y".to_owned())),
                ("half_height".to_owned(), Value::F64(f64::from(half_height))),
                ("radius".to_owned(), Value::F64(f64::from(radius))),
            ])),
        };
        Self {
            family: GameCommandFamily::Hitbox,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                ("operation".to_owned(), Value::String("create".to_owned())),
                ("owner".to_owned(), entity_handle_value(owner)),
                ("shape".to_owned(), shape),
                ("team".to_owned(), Value::I64(i64::from(team))),
                ("damage".to_owned(), Value::F64(f64::from(damage))),
                ("membership".to_owned(), Value::I64(i64::from(membership))),
                ("mask".to_owned(), Value::I64(i64::from(mask))),
                (
                    "one_hit_per_target".to_owned(),
                    Value::Bool(one_hit_per_target),
                ),
            ])),
        }
    }

    /// Creates an enabled trigger hitbox that also requests character knockback.
    #[allow(clippy::too_many_arguments)]
    pub fn create_hitbox_with_knockback(
        target: GameEntityHandle,
        owner: GameEntityHandle,
        shape: GameHitboxShape,
        team: i32,
        damage: f32,
        membership: u32,
        mask: u32,
        one_hit_per_target: bool,
        knockback: [f32; 3],
    ) -> Self {
        let mut command = Self::create_hitbox(
            target,
            owner,
            shape,
            team,
            damage,
            membership,
            mask,
            one_hit_per_target,
        );
        if let Value::Object(fields) = &mut command.payload {
            fields.insert("knockback".to_owned(), vector3_value(knockback));
        }
        command
    }

    /// Starts a new activation and clears this hitbox's one-hit history.
    pub fn enable_hitbox(target: GameEntityHandle) -> Self {
        simple_target_command(GameCommandFamily::Hitbox, "enable", target)
    }

    /// Excludes a hitbox from collision detection while retaining its setup.
    pub fn disable_hitbox(target: GameEntityHandle) -> Self {
        simple_target_command(GameCommandFamily::Hitbox, "disable", target)
    }

    /// Removes a command-owned hitbox and all collision components it owns.
    pub fn remove_hitbox(target: GameEntityHandle) -> Self {
        simple_target_command(GameCommandFamily::Hitbox, "remove", target)
    }

    /// Publishes a text value to the runtime UI binding table.
    pub fn set_ui_text(name: impl Into<String>, value: impl Into<String>) -> Self {
        ui_binding_command(
            "set_binding",
            name.into(),
            Some(Value::String(value.into())),
        )
    }

    /// Publishes a finite numeric value to the runtime UI binding table.
    pub fn set_ui_number(name: impl Into<String>, value: f64) -> Self {
        ui_binding_command("set_binding", name.into(), Some(Value::F64(value)))
    }

    /// Publishes a boolean value to the runtime UI binding table.
    pub fn set_ui_flag(name: impl Into<String>, value: bool) -> Self {
        ui_binding_command("set_binding", name.into(), Some(Value::Bool(value)))
    }

    /// Removes a named value from the runtime UI binding table.
    pub fn remove_ui_binding(name: impl Into<String>) -> Self {
        ui_binding_command("remove_binding", name.into(), None)
    }

    /// Shows or hides one scene-placed UI document at runtime.
    pub fn set_ui_document_visible(target: GameEntityHandle, visible: bool) -> Self {
        GameCommand {
            family: GameCommandFamily::Ui,
            request_id: None,
            target: Some(target),
            payload: Value::Object(BTreeMap::from([
                (
                    "operation".to_owned(),
                    Value::String("set_visibility".to_owned()),
                ),
                ("visible".to_owned(), Value::Bool(visible)),
            ])),
        }
    }

    /// Requests a project-relative scene transition at the next frame boundary.
    pub fn request_scene(path: impl Into<String>) -> Self {
        GameCommand {
            family: GameCommandFamily::Scene,
            request_id: None,
            target: None,
            payload: Value::Object(BTreeMap::from([
                ("operation".to_owned(), Value::String("request".to_owned())),
                ("path".to_owned(), Value::String(path.into())),
            ])),
        }
    }

    /// Queues a one-shot sound effect by stable authoring asset ID.
    pub fn play_sound_effect(asset_id: impl Into<String>) -> Self {
        audio_asset_command("play_se", asset_id.into(), None)
    }

    /// Queues a spatial sound effect attached to a generation-checked runtime entity.
    pub fn play_spatial_sound_effect(
        target: GameEntityHandle,
        asset_id: impl Into<String>,
        options: GameSpatialAudioOptions,
    ) -> Self {
        let mut command = audio_asset_command("play_spatial_se", asset_id.into(), None);
        command.target = Some(target);
        let Value::Object(payload) = &mut command.payload else {
            unreachable!("audio asset command always has an object payload");
        };
        payload.insert("volume".to_owned(), Value::F64(f64::from(options.volume)));
        payload.insert(
            "spatial_blend".to_owned(),
            Value::F64(f64::from(options.spatial_blend)),
        );
        payload.insert(
            "min_distance".to_owned(),
            Value::F64(f64::from(options.min_distance)),
        );
        payload.insert(
            "max_distance".to_owned(),
            Value::F64(f64::from(options.max_distance)),
        );
        payload.insert(
            "rolloff".to_owned(),
            Value::String(match options.rolloff {
                GameAudioRolloffMode::Linear => "linear",
                GameAudioRolloffMode::Inverse => "inverse",
            }.to_owned()),
        );
        payload.insert("looping".to_owned(), Value::Bool(options.looping));
        command
    }

    /// Replaces the active background music by stable authoring asset ID.
    pub fn play_background_music(asset_id: impl Into<String>) -> Self {
        audio_asset_command("play_bgm", asset_id.into(), None)
    }

    /// Crossfades to background music over a finite non-negative duration.
    pub fn crossfade_background_music(asset_id: impl Into<String>, fade_seconds: f32) -> Self {
        audio_asset_command("crossfade_bgm", asset_id.into(), Some(fade_seconds))
    }

    /// Stops the active background music.
    pub fn stop_background_music() -> Self {
        simple_audio_command("stop_bgm")
    }

    /// Sets the clamped master mixer volume.
    pub fn set_master_volume(volume: f32) -> Self {
        audio_volume_command("set_master_volume", volume)
    }

    /// Sets the clamped background-music bus volume.
    pub fn set_background_music_volume(volume: f32) -> Self {
        audio_volume_command("set_bgm_volume", volume)
    }

    /// Sets the clamped sound-effect bus volume.
    pub fn set_sound_effect_volume(volume: f32) -> Self {
        audio_volume_command("set_se_volume", volume)
    }

    /// Sets a text value in the active versioned save document.
    pub fn set_save_text(key: impl Into<String>, value: impl Into<String>) -> Self {
        save_value_command("set", key.into(), Some(Value::String(value.into())))
    }

    /// Sets a finite numeric value in the active versioned save document.
    pub fn set_save_number(key: impl Into<String>, value: f64) -> Self {
        save_value_command("set", key.into(), Some(Value::F64(value)))
    }

    /// Sets a boolean value in the active versioned save document.
    pub fn set_save_flag(key: impl Into<String>, value: bool) -> Self {
        save_value_command("set", key.into(), Some(Value::Bool(value)))
    }

    /// Removes a key from the active save document.
    pub fn remove_save_value(key: impl Into<String>) -> Self {
        save_value_command("remove", key.into(), None)
    }

    /// Atomically writes the active save document to a numbered slot.
    pub fn write_save_slot(slot: u32) -> Self {
        save_slot_command("write", slot)
    }

    /// Loads a numbered slot at the next save-service boundary.
    pub fn load_save_slot(slot: u32) -> Self {
        save_slot_command("load", slot)
    }

    /// Starts or replaces a fixed-step gameplay timer with a stable ID.
    pub fn set_timer(timer_id: impl Into<String>, duration_seconds: f32) -> Self {
        timer_command(
            "set",
            timer_id.into(),
            None,
            Some(Value::F64(f64::from(duration_seconds))),
        )
    }

    /// Cancels and removes a gameplay timer.
    pub fn cancel_timer(timer_id: impl Into<String>) -> Self {
        timer_command("cancel", timer_id.into(), None, None)
    }

    /// Requests a timer-state event carrying the caller's request ID.
    pub fn query_timer(timer_id: impl Into<String>, request_id: u64) -> Self {
        timer_command("query", timer_id.into(), Some(request_id), None)
    }
}

fn behavior_tree_result_command(
    kind: &str,
    behavior_id: String,
    status: GameBehaviorStatus,
) -> GameCommand {
    let status = match status {
        GameBehaviorStatus::Success => "success",
        GameBehaviorStatus::Failure => "failure",
        GameBehaviorStatus::Running => "running",
    };
    GameCommand {
        family: GameCommandFamily::BehaviorTree,
        request_id: None,
        target: None,
        payload: Value::Object(BTreeMap::from([
            ("kind".to_owned(), Value::String(kind.to_owned())),
            ("behavior_id".to_owned(), Value::String(behavior_id)),
            ("status".to_owned(), Value::String(status.to_owned())),
        ])),
    }
}

fn timer_command(
    operation: &str,
    timer_id: String,
    request_id: Option<u64>,
    duration_seconds: Option<Value>,
) -> GameCommand {
    let mut fields = BTreeMap::from([
        ("operation".to_owned(), Value::String(operation.to_owned())),
        ("timer_id".to_owned(), Value::String(timer_id)),
    ]);
    if let Some(duration_seconds) = duration_seconds {
        fields.insert("duration_seconds".to_owned(), duration_seconds);
    }
    GameCommand {
        family: GameCommandFamily::Timer,
        request_id,
        target: None,
        payload: Value::Object(fields),
    }
}

fn component_command(
    operation: &str,
    target: GameEntityHandle,
    component_type: ComponentTypeId,
) -> GameCommand {
    GameCommand {
        family: GameCommandFamily::Component,
        request_id: None,
        target: Some(target),
        payload: Value::Object(BTreeMap::from([
            ("operation".to_owned(), Value::String(operation.to_owned())),
            (
                "component_type".to_owned(),
                Value::String(component_type.to_string()),
            ),
        ])),
    }
}

fn simple_target_command(
    family: GameCommandFamily,
    operation: &str,
    target: GameEntityHandle,
) -> GameCommand {
    GameCommand {
        family,
        request_id: None,
        target: Some(target),
        payload: Value::Object(BTreeMap::from([(
            "operation".to_owned(),
            Value::String(operation.to_owned()),
        )])),
    }
}

fn entity_handle_value(handle: GameEntityHandle) -> Value {
    Value::Object(BTreeMap::from([
        ("id".to_owned(), Value::I64(i64::from(handle.id))),
        (
            "generation".to_owned(),
            Value::I64(i64::from(handle.generation)),
        ),
    ]))
}

fn save_value_command(operation: &str, key: String, value: Option<Value>) -> GameCommand {
    let mut fields = BTreeMap::from([
        ("operation".to_owned(), Value::String(operation.to_owned())),
        ("key".to_owned(), Value::String(key)),
    ]);
    if let Some(value) = value {
        fields.insert("value".to_owned(), value);
    }
    GameCommand {
        family: GameCommandFamily::Save,
        request_id: None,
        target: None,
        payload: Value::Object(fields),
    }
}

fn save_slot_command(operation: &str, slot: u32) -> GameCommand {
    GameCommand {
        family: GameCommandFamily::Save,
        request_id: None,
        target: None,
        payload: Value::Object(BTreeMap::from([
            ("operation".to_owned(), Value::String(operation.to_owned())),
            // JSON has no unsigned integer type. Every u32 fits in i64, so
            // canonicalizing here preserves exact command round-trips across
            // the ABI envelope instead of decoding U64 back as I64.
            ("slot".to_owned(), Value::I64(i64::from(slot))),
        ])),
    }
}

fn simple_audio_command(operation: &str) -> GameCommand {
    GameCommand {
        family: GameCommandFamily::Audio,
        request_id: None,
        target: None,
        payload: Value::Object(BTreeMap::from([(
            "operation".to_owned(),
            Value::String(operation.to_owned()),
        )])),
    }
}

fn audio_asset_command(
    operation: &str,
    asset_id: String,
    fade_seconds: Option<f32>,
) -> GameCommand {
    let mut command = simple_audio_command(operation);
    let Value::Object(payload) = &mut command.payload else {
        unreachable!("simple audio command always has an object payload");
    };
    payload.insert("asset_id".to_owned(), Value::String(asset_id));
    if let Some(fade_seconds) = fade_seconds {
        payload.insert(
            "fade_seconds".to_owned(),
            Value::F64(f64::from(fade_seconds)),
        );
    }
    command
}

fn audio_volume_command(operation: &str, volume: f32) -> GameCommand {
    let mut command = simple_audio_command(operation);
    let Value::Object(payload) = &mut command.payload else {
        unreachable!("simple audio command always has an object payload");
    };
    payload.insert("volume".to_owned(), Value::F64(f64::from(volume)));
    command
}

fn ui_binding_command(operation: &str, name: String, value: Option<Value>) -> GameCommand {
    let mut payload = BTreeMap::from([
        ("operation".to_owned(), Value::String(operation.to_owned())),
        ("name".to_owned(), Value::String(name)),
    ]);
    if let Some(value) = value {
        payload.insert("value".to_owned(), value);
    }
    GameCommand {
        family: GameCommandFamily::Ui,
        request_id: None,
        target: None,
        payload: Value::Object(payload),
    }
}

fn animation_command(target: GameEntityHandle, payload: BTreeMap<String, Value>) -> GameCommand {
    GameCommand {
        family: GameCommandFamily::Animation,
        request_id: None,
        target: Some(target),
        payload: Value::Object(payload),
    }
}

fn lock_on_command(operation: &str) -> GameCommand {
    GameCommand {
        family: GameCommandFamily::LockOn,
        request_id: None,
        target: None,
        payload: Value::Object(BTreeMap::from([(
            "operation".to_owned(),
            Value::String(operation.to_owned()),
        )])),
    }
}

fn vector3_value(value: [f32; 3]) -> Value {
    Value::Object(BTreeMap::from([
        ("x".to_owned(), Value::F64(f64::from(value[0]))),
        ("y".to_owned(), Value::F64(f64::from(value[1]))),
        ("z".to_owned(), Value::F64(f64::from(value[2]))),
    ]))
}

fn quaternion_value(value: [f32; 4]) -> Value {
    let mut fields = match vector3_value([value[0], value[1], value[2]]) {
        Value::Object(fields) => fields,
        _ => unreachable!("vector helper always returns an object"),
    };
    fields.insert("w".to_owned(), Value::F64(f64::from(value[3])));
    Value::Object(fields)
}

/// Project-defined event emitted after a callback succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameEventEmission {
    /// Stable dotted event ID owned by the project.
    pub event_id: String,
    /// Optional generation-checked target; `None` broadcasts the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<GameEntityHandle>,
    /// Project-defined payload.
    pub payload: Value,
}

impl GameEventEmission {
    /// Creates a project event visible to every `Game` stream subscriber.
    pub fn broadcast(event_id: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id: event_id.into(),
            target: None,
            payload,
        }
    }

    /// Creates a project event carrying one generation-checked target.
    ///
    /// Project systems are not entity-owned, so every `Game` stream subscriber
    /// receives the record and uses this target to select its relevant query
    /// row. The host rejects the complete callback if the target is stale.
    pub fn targeted(event_id: impl Into<String>, target: GameEntityHandle, payload: Value) -> Self {
        Self {
            event_id: event_id.into(),
            target: Some(target),
            payload,
        }
    }
}

/// Atomic callback output decoded and validated before any mutation is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameInvocationOutput {
    /// Payload schema version, currently [`GAME_IO_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Writable project-component replacements.
    pub component_patches: Vec<GameComponentPatch>,
    /// Writable game-resource replacements.
    pub resource_patches: Vec<GameResourcePatch>,
    /// Deferred host commands in application order.
    pub commands: Vec<GameCommand>,
    /// Project-defined events emitted after mutations succeed.
    pub emitted_events: Vec<GameEventEmission>,
    /// Highest consumed sequence per host event stream.
    pub consumed_event_sequences: BTreeMap<GameEventStream, u64>,
}

impl Default for GameInvocationOutput {
    fn default() -> Self {
        Self {
            schema_version: GAME_IO_SCHEMA_VERSION,
            component_patches: Vec::new(),
            resource_patches: Vec::new(),
            commands: Vec::new(),
            emitted_events: Vec::new(),
            consumed_event_sequences: BTreeMap::new(),
        }
    }
}

impl GameInvocationOutput {
    /// Rejects output collections that exceed the ABI v3 command cap.
    ///
    /// Encoded byte limits are checked by the host because they depend on the
    /// actual serialized buffer rather than the in-memory Rust representation.
    ///
    /// # Errors
    ///
    /// Returns [`GameIoLimitError::Commands`] when too many commands were
    /// produced.
    pub fn validate_collection_limits(&self) -> Result<(), GameIoLimitError> {
        if self.commands.len() > MAX_GAME_COMMANDS {
            return Err(GameIoLimitError::Commands {
                actual: self.commands.len(),
                maximum: MAX_GAME_COMMANDS,
            });
        }
        if self.emitted_events.len() > MAX_GAME_EVENT_RECORDS {
            return Err(GameIoLimitError::Events {
                actual: self.emitted_events.len(),
                maximum: MAX_GAME_EVENT_RECORDS,
            });
        }
        Ok(())
    }
}

impl GameInvocation {
    /// Rejects query results that exceed the ABI v3 total row cap.
    ///
    /// # Errors
    ///
    /// Returns [`GameIoLimitError::QueryRows`] when all query results together
    /// contain more than [`MAX_GAME_QUERY_ROWS`] entity rows.
    pub fn validate_collection_limits(&self) -> Result<(), GameIoLimitError> {
        let row_count = self.queries.iter().fold(0_usize, |total, query| {
            total.saturating_add(query.rows.len())
        });
        if row_count > MAX_GAME_QUERY_ROWS {
            return Err(GameIoLimitError::QueryRows {
                actual: row_count,
                maximum: MAX_GAME_QUERY_ROWS,
            });
        }
        if self.events.len() > MAX_GAME_EVENT_RECORDS {
            return Err(GameIoLimitError::Events {
                actual: self.events.len(),
                maximum: MAX_GAME_EVENT_RECORDS,
            });
        }
        Ok(())
    }
}

/// Validates the encoded input byte count before a callback runs.
///
/// # Errors
///
/// Returns [`GameIoLimitError::InputBytes`] when `byte_count` exceeds
/// [`MAX_GAME_INPUT_BYTES`].
pub fn validate_game_input_bytes(byte_count: usize) -> Result<(), GameIoLimitError> {
    if byte_count > MAX_GAME_INPUT_BYTES {
        return Err(GameIoLimitError::InputBytes {
            actual: byte_count,
            maximum: MAX_GAME_INPUT_BYTES,
        });
    }
    Ok(())
}

/// Validates the encoded output byte count before host-side decoding.
///
/// # Errors
///
/// Returns [`GameIoLimitError::OutputBytes`] when `byte_count` exceeds
/// [`MAX_GAME_OUTPUT_BYTES`].
pub fn validate_game_output_bytes(byte_count: usize) -> Result<(), GameIoLimitError> {
    if byte_count > MAX_GAME_OUTPUT_BYTES {
        return Err(GameIoLimitError::OutputBytes {
            actual: byte_count,
            maximum: MAX_GAME_OUTPUT_BYTES,
        });
    }
    Ok(())
}

/// Reports a bounded game-module input or output violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameIoLimitError {
    /// A compiled invocation contains too many entity rows.
    QueryRows {
        /// Observed row count.
        actual: usize,
        /// Configured maximum row count.
        maximum: usize,
    },
    /// Encoded callback input exceeds its byte budget.
    InputBytes {
        /// Observed encoded byte count.
        actual: usize,
        /// Configured maximum byte count.
        maximum: usize,
    },
    /// Encoded callback output exceeds its byte budget.
    OutputBytes {
        /// Observed encoded byte count.
        actual: usize,
        /// Configured maximum byte count.
        maximum: usize,
    },
    /// A callback emitted too many deferred commands.
    Commands {
        /// Observed command count.
        actual: usize,
        /// Configured maximum command count.
        maximum: usize,
    },
    /// An invocation contains too many unconsumed event records.
    Events {
        /// Observed event count.
        actual: usize,
        /// Configured maximum event count.
        maximum: usize,
    },
}

impl fmt::Display for GameIoLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueryRows { actual, maximum } => {
                write!(
                    formatter,
                    "game query returned {actual} rows; maximum is {maximum}"
                )
            }
            Self::InputBytes { actual, maximum } => {
                write!(
                    formatter,
                    "game input uses {actual} bytes; maximum is {maximum}"
                )
            }
            Self::OutputBytes { actual, maximum } => {
                write!(
                    formatter,
                    "game output uses {actual} bytes; maximum is {maximum}"
                )
            }
            Self::Commands { actual, maximum } => {
                write!(
                    formatter,
                    "game output has {actual} commands; maximum is {maximum}"
                )
            }
            Self::Events { actual, maximum } => {
                write!(
                    formatter,
                    "game invocation has {actual} events; maximum is {maximum}"
                )
            }
        }
    }
}

impl std::error::Error for GameIoLimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn component_access(component_type: &str) -> GameComponentAccess {
        GameComponentAccess {
            component_type: ComponentTypeId::new(component_type),
            mode: GameAccessMode::Write,
            required: true,
        }
    }

    #[test]
    fn empty_access_manifest_is_valid_without_host_dependencies() {
        GameSystemAccess::default()
            .validate()
            .expect("a system may declare no copied host data or services");
    }

    #[test]
    fn save_key_access_rejects_duplicates_and_unsafe_keys() {
        let duplicate = GameSystemAccess {
            save_keys: vec!["mission.rank".to_owned(), "mission.rank".to_owned()],
            ..GameSystemAccess::default()
        };
        assert!(matches!(
            duplicate.validate(),
            Err(GameAccessError::DuplicateSaveKey(key)) if key == "mission.rank"
        ));

        let unsafe_key = GameSystemAccess {
            save_keys: vec!["mission\nrank".to_owned()],
            ..GameSystemAccess::default()
        };
        assert!(matches!(
            unsafe_key.validate(),
            Err(GameAccessError::InvalidSaveKey(_))
        ));
    }

    #[test]
    fn current_game_io_requires_serialized_collection_fields() {
        assert!(
            serde_json::from_str::<GameQueryAccess>(r#"{"id":"game.query.test"}"#).is_err(),
            "current query access must carry components and engine_views"
        );
        assert!(
            serde_json::from_str::<GameQueryRow>(
                r#"{"entity":{"id":1,"generation":0}}"#
            )
            .is_err(),
            "current query rows must carry components and engine_views"
        );
        assert!(
            serde_json::from_str::<GameSystemAccess>(
                r#"{"input_actions":[],"queries":[],"resources":[],"event_streams":[],"command_families":[]}"#
            )
            .is_err(),
            "current access manifests must carry save_keys and host_views"
        );
        assert!(
            serde_json::from_str::<GameInvocation>(
                r#"{"schema_version":1,"system_id":"game.current","clock":{"delta_seconds":0.0,"fixed_delta_seconds":0.0,"elapsed_seconds":0.0,"frame_index":0,"fixed_step_index":0},"input_actions":{},"queries":[],"resources":{},"events":[]}"#
            )
            .is_err(),
            "current invocations must carry save_values and host_views"
        );
        assert!(
            serde_json::from_str::<GameInvocationOutput>(r#"{"schema_version":1}"#).is_err(),
            "current outputs must carry every collection field"
        );
    }

    #[test]
    fn access_manifest_rejects_duplicate_component_in_one_query() {
        let access = GameSystemAccess {
            input_actions: Vec::new(),
            queries: vec![GameQueryAccess {
                id: "game.query.combatants".to_owned(),
                components: vec![
                    component_access("game.health"),
                    component_access("game.health"),
                ],
                engine_views: Vec::new(),
            }],
            ..GameSystemAccess::default()
        };

        assert!(matches!(
            access.validate(),
            Err(GameAccessError::DuplicateQueryComponent { .. })
        ));
    }

    #[test]
    fn access_manifest_rejects_invalid_resource_id() {
        let access = GameSystemAccess {
            resources: vec![GameResourceAccess {
                id: "Mission Phase".to_owned(),
                mode: GameAccessMode::Read,
            }],
            ..GameSystemAccess::default()
        };

        assert!(matches!(
            access.validate(),
            Err(GameAccessError::InvalidResourceId { .. })
        ));
    }

    #[test]
    fn access_manifest_rejects_duplicate_host_view() {
        let access = GameSystemAccess {
            host_views: vec![GameHostViewKind::SceneState, GameHostViewKind::SceneState],
            ..GameSystemAccess::default()
        };

        assert!(matches!(
            access.validate(),
            Err(GameAccessError::DuplicateHostView(
                GameHostViewKind::SceneState
            ))
        ));
    }

    #[test]
    fn access_manifest_rejects_duplicate_input_action() {
        let access = GameSystemAccess {
            input_actions: vec!["attack".to_owned(), "attack".to_owned()],
            ..GameSystemAccess::default()
        };

        assert!(matches!(
            access.validate(),
            Err(GameAccessError::DuplicateInputAction(action)) if action == "attack"
        ));
    }

    #[test]
    fn typed_synchronous_commands_roundtrip() {
        let target = GameEntityHandle {
            id: 4,
            generation: 2,
        };
        let output = GameInvocationOutput {
            commands: vec![
                GameCommand::set_transform(
                    target,
                    [1.0, 2.0, 3.0],
                    [0.0, 0.0, 0.0, 1.0],
                    [1.0, 1.0, 1.0],
                ),
                GameCommand::despawn(target),
                GameCommand::add_game_component(
                    target,
                    ComponentTypeId::new("game.status.stunned"),
                ),
                GameCommand::disable_game_component(
                    target,
                    ComponentTypeId::new("game.status.stunned"),
                ),
                GameCommand::enable_game_component(
                    target,
                    ComponentTypeId::new("game.status.stunned"),
                ),
                GameCommand::remove_game_component(
                    target,
                    ComponentTypeId::new("game.status.stunned"),
                ),
                GameCommand::set_character_motion(target, [2.0, 0.0, 1.0], [0.0, 0.0, -1.0]),
                GameCommand::spawn_prefab("prefabs/enemy.prefab.json", [1.0, 0.0, 2.0], u64::MAX),
                GameCommand::acquire_lock_on(),
                GameCommand::cycle_lock_on(),
                GameCommand::release_lock_on(),
                GameCommand::play_animation(target, true),
                GameCommand::crossfade_animation(target, 42, 0.2, false),
                GameCommand::stop_animation(target),
                GameCommand::create_hitbox(
                    target,
                    target,
                    GameHitboxShape::CapsuleY {
                        half_height: 0.5,
                        radius: 0.25,
                    },
                    1,
                    12.0,
                    2,
                    4,
                    true,
                ),
                GameCommand::disable_hitbox(target),
                GameCommand::enable_hitbox(target),
                GameCommand::remove_hitbox(target),
                GameCommand::set_animation_bool(target, "attacking", true),
                GameCommand::set_ui_text("hud.status", "ready"),
                GameCommand::set_ui_number("hud.hp", 120.0),
                GameCommand::set_ui_flag("hud.boss", true),
                GameCommand::remove_ui_binding("hud.status"),
                GameCommand::set_ui_document_visible(target, false),
                GameCommand::request_scene("scenes/mission.scene.json"),
                GameCommand::play_sound_effect("asset_01JP0000000000000000000601"),
                GameCommand::play_background_music("asset_01JP0000000000000000000602"),
                GameCommand::crossfade_background_music("asset_01JP0000000000000000000602", 0.5),
                GameCommand::stop_background_music(),
                GameCommand::set_master_volume(0.8),
                GameCommand::set_background_music_volume(0.7),
                GameCommand::set_sound_effect_volume(0.6),
                GameCommand::set_save_text("profile.name", "Rin"),
                GameCommand::set_save_number("profile.score", 120.0),
                GameCommand::set_save_flag("profile.cleared", true),
                GameCommand::remove_save_value("profile.legacy"),
                GameCommand::write_save_slot(3),
                GameCommand::load_save_slot(3),
                GameCommand::set_timer("game.timer.attack", 0.5),
                GameCommand::query_timer("game.timer.attack", u64::MAX),
                GameCommand::cancel_timer("game.timer.attack"),
            ],
            ..GameInvocationOutput::default()
        };

        let encoded = serde_json::to_vec(&output).unwrap();
        let decoded: GameInvocationOutput = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded, output);
    }

    #[test]
    fn output_rejects_more_than_command_limit() {
        let command = GameCommand {
            family: GameCommandFamily::GameEvent,
            request_id: None,
            target: None,
            payload: Value::Null,
        };
        let output = GameInvocationOutput {
            commands: vec![command; MAX_GAME_COMMANDS + 1],
            ..GameInvocationOutput::default()
        };

        assert_eq!(
            output.validate_collection_limits(),
            Err(GameIoLimitError::Commands {
                actual: MAX_GAME_COMMANDS + 1,
                maximum: MAX_GAME_COMMANDS,
            })
        );
    }

    #[test]
    fn invocation_rejects_total_rows_across_queries_above_limit() {
        let row = GameQueryRow {
            entity: GameEntityHandle {
                id: 1,
                generation: 0,
            },
            authoring_id: None,
            components: BTreeMap::new(),
            engine_views: BTreeMap::new(),
        };
        let invocation = GameInvocation {
            schema_version: GAME_IO_SCHEMA_VERSION,
            system_id: "game.combat".to_owned(),
            clock: GameClock {
                delta_seconds: 1.0 / 60.0,
                fixed_delta_seconds: 1.0 / 60.0,
                elapsed_seconds: 1.0,
                frame_index: 60,
                fixed_step_index: 60,
            },
            input_actions: BTreeMap::new(),
            save_values: BTreeMap::new(),
            queries: vec![
                GameQueryResult {
                    query_id: "game.query.first".to_owned(),
                    rows: vec![row.clone(); MAX_GAME_QUERY_ROWS],
                },
                GameQueryResult {
                    query_id: "game.query.second".to_owned(),
                    rows: vec![row],
                },
            ],
            resources: BTreeMap::new(),
            host_views: BTreeMap::new(),
            events: Vec::new(),
        };

        assert_eq!(
            invocation.validate_collection_limits(),
            Err(GameIoLimitError::QueryRows {
                actual: MAX_GAME_QUERY_ROWS + 1,
                maximum: MAX_GAME_QUERY_ROWS,
            })
        );
    }

    #[test]
    fn output_json_roundtrip_preserves_generation_and_command_order() {
        let first_target = GameEntityHandle {
            id: 7,
            generation: 3,
        };
        let second_target = GameEntityHandle {
            id: 11,
            generation: 8,
        };
        let output = GameInvocationOutput {
            commands: vec![
                GameCommand {
                    family: GameCommandFamily::Transform,
                    request_id: None,
                    target: Some(first_target),
                    payload: Value::Null,
                },
                GameCommand {
                    family: GameCommandFamily::Despawn,
                    request_id: Some(42),
                    target: Some(second_target),
                    payload: Value::Null,
                },
            ],
            ..GameInvocationOutput::default()
        };

        let json = serde_json::to_vec(&output).expect("game output must serialize");
        let decoded: GameInvocationOutput =
            serde_json::from_slice(&json).expect("game output must deserialize");

        assert_eq!(decoded, output);
        assert_eq!(decoded.commands[0].target, Some(first_target));
        assert_eq!(decoded.commands[1].target, Some(second_target));
    }

    #[test]
    fn encoded_byte_limits_accept_boundary_and_reject_next_byte() {
        assert_eq!(validate_game_input_bytes(MAX_GAME_INPUT_BYTES), Ok(()));
        assert!(matches!(
            validate_game_input_bytes(MAX_GAME_INPUT_BYTES + 1),
            Err(GameIoLimitError::InputBytes { .. })
        ));
        assert_eq!(validate_game_output_bytes(MAX_GAME_OUTPUT_BYTES), Ok(()));
        assert!(matches!(
            validate_game_output_bytes(MAX_GAME_OUTPUT_BYTES + 1),
            Err(GameIoLimitError::OutputBytes { .. })
        ));
    }
}
