//! Validated host processors for deferred ABI v3 gameplay commands.
//!
//! Command payloads are decoded and preflighted completely before component or
//! resource patches are applied. Prepared commands then use only operations
//! that cannot fail while the exclusive GameModule bridge retains the world.

mod animation;

use crate::asset::AssetManifest;
use crate::audio::{
    GameAudioCommand, GameAudioCommandQueue, GameSpatialAudioOptions, SpatialRolloff,
    MAX_GAME_AUDIO_COMMANDS,
};
use crate::behavior_tree::{BehaviorStatus, BehaviorTreeBehaviorRegistry};
use crate::character_controller::KinematicCharacterController;
use crate::collision::{Collider, CollisionLayers, PhysicsBody, TriggerVolume};
use crate::game_io::{GameCommand, GameCommandFamily, GameEntityHandle, MAX_GAME_SAVE_KEY_BYTES};
use crate::game_module::{GameComponentDefaults, GameComponentStore};
use crate::game_prefab::{
    GamePrefabEvents, GamePrefabSpawnQueue, GamePrefabSpawnRequest, MAX_GAME_PREFAB_REQUESTS,
};
use crate::game_timer::{query_game_timer, GameTimerEvents, GameTimers, MAX_GAME_TIMERS};
use crate::hitbox::AttackHitbox;
use crate::lock_on::TargetLock;
use crate::navmesh::NavMeshAgent;
use crate::save::{
    GameSaveCommand, GameSaveCommandQueue, SaveData, SaveValue, MAX_GAME_SAVE_COMMANDS,
};
use crate::scene_manager::SceneManager;
use crate::transform::Transform;
use crate::ui_document::{UiBindingValue, UiBindings, UiDocumentRef, UiDocumentVisibility};
use engine_authoring::{AssetId, ComponentTypeId, StableId, Value};
use engine_ecs::{Entity, SystemId, World};
use glam::{Quat, Vec3};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

pub(crate) enum PreparedGameCommand {
    SetTransform {
        entity: Entity,
        translation: Vec3,
        rotation: Quat,
        scale: Vec3,
    },
    Translate {
        entity: Entity,
        delta: Vec3,
    },
    Rotate {
        entity: Entity,
        delta: Quat,
    },
    Despawn {
        entity: Entity,
    },
    SetCharacterMotion {
        entity: Entity,
        velocity: Vec3,
        rotation: Quat,
    },
    SetNavigationTarget {
        entity: Entity,
        target: Option<Vec3>,
    },
    SetBehaviorTreeResult {
        behavior_id: String,
        is_action: bool,
        status: BehaviorStatus,
    },
    LockOn(LockOnOperation),
    Animation(animation::PreparedAnimationCommand),
    SetUiBinding {
        name: String,
        value: UiBindingValue,
    },
    RemoveUiBinding {
        name: String,
    },
    SetUiVisibility {
        entity: Entity,
        visible: bool,
        has_override: bool,
    },
    RequestScene {
        path: String,
    },
    Audio(GameAudioCommand),
    SetSaveValue {
        key: String,
        value: SaveValue,
    },
    RemoveSaveValue {
        key: String,
    },
    SaveSlot(SaveSlotOperation),
    Timer(TimerOperation),
    SpawnPrefab(GamePrefabSpawnRequest),
    AddProjectComponent {
        entity: Entity,
        component_type: ComponentTypeId,
        value: Value,
    },
    RemoveProjectComponent {
        entity: Entity,
        component_type: ComponentTypeId,
    },
    SetProjectComponentEnabled {
        entity: Entity,
        component_type: ComponentTypeId,
        enabled: bool,
    },
    CreateHitbox {
        entity: Entity,
        collider: Collider,
        layers: CollisionLayers,
        hitbox: AttackHitbox,
    },
    SetHitboxEnabled {
        entity: Entity,
        enabled: bool,
    },
    RemoveHitbox {
        entity: Entity,
    },
}

pub(crate) enum SaveSlotOperation {
    Write { slot: u32 },
    Load { slot: u32 },
}

pub(crate) enum TimerOperation {
    Set {
        timer_id: String,
        duration_seconds: f32,
    },
    Cancel {
        timer_id: String,
    },
    Query {
        timer_id: String,
        request_id: u64,
    },
}

pub(crate) enum LockOnOperation {
    Acquire,
    Cycle,
    Release,
}

/// Validates every command payload and target without mutating the world.
pub(crate) fn prepare_game_commands(
    world: &World,
    commands: &[GameCommand],
) -> Result<Vec<PreparedGameCommand>, GameCommandError> {
    let mut prepared = Vec::with_capacity(commands.len());
    let mut despawned = BTreeSet::new();
    let mut inserted_ui_visibility = BTreeSet::new();
    let mut pending_audio_commands = 0_usize;
    let mut pending_save_commands = 0_usize;
    let mut virtual_timer_ids = world
        .get_resource::<GameTimers>()
        .map(|timers| timers.ids().map(str::to_owned).collect::<BTreeSet<_>>());
    let mut pending_prefab_requests = 0_usize;
    let mut virtual_components = BTreeMap::new();
    let mut virtual_hitboxes = BTreeMap::new();
    for (index, command) in commands.iter().enumerate() {
        match command.family {
            GameCommandFamily::Transform => {
                let (target, entity) = targeted_entity(world, command, index, &despawned)?;
                if world.get_component::<Transform>(entity).is_none() {
                    return Err(GameCommandError::MissingTransform { index, target });
                }
                prepared.push(parse_transform_command(index, entity, &command.payload)?);
            }
            GameCommandFamily::Character => {
                let (target, entity) = targeted_entity(world, command, index, &despawned)?;
                if world
                    .get_component::<KinematicCharacterController>(entity)
                    .is_none()
                {
                    return Err(GameCommandError::MissingCharacterController { index, target });
                }
                if world.get_component::<Transform>(entity).is_none() {
                    return Err(GameCommandError::MissingTransform { index, target });
                }
                prepared.push(parse_character_command(index, entity, &command.payload)?);
            }
            GameCommandFamily::Navigation => {
                let (target, entity) = targeted_entity(world, command, index, &despawned)?;
                if world.get_component::<NavMeshAgent>(entity).is_none() {
                    return Err(GameCommandError::MissingNavigationAgent { index, target });
                }
                let fields = object(&command.payload, index, "navigation payload")?;
                let target = match string_field(fields, "operation", index)? {
                    "set_target" => Some(vector3_field(fields, "target", index)?),
                    "clear_target" => None,
                    other => {
                        return Err(GameCommandError::InvalidPayload {
                            index,
                            message: format!("unknown navigation operation `{other}`"),
                        })
                    }
                };
                prepared.push(PreparedGameCommand::SetNavigationTarget { entity, target });
            }
            GameCommandFamily::BehaviorTree => {
                require_targetless(command, index, "behavior tree")?;
                if world
                    .get_resource::<BehaviorTreeBehaviorRegistry>()
                    .is_none()
                {
                    return Err(GameCommandError::MissingBehaviorTreeRegistry { index });
                }
                let fields = object(&command.payload, index, "behavior tree payload")?;
                let behavior_id = string_field(fields, "behavior_id", index)?
                    .trim()
                    .to_owned();
                if behavior_id.is_empty()
                    || behavior_id.len() > 256
                    || behavior_id.chars().any(char::is_control)
                {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: "behavior_id must be a non-empty stable ID of at most 256 bytes"
                            .to_owned(),
                    });
                }
                let is_action = match string_field(fields, "kind", index)? {
                    "action" => true,
                    "condition" => false,
                    other => {
                        return Err(GameCommandError::InvalidPayload {
                            index,
                            message: format!("unknown behavior tree result kind `{other}`"),
                        })
                    }
                };
                let status = match string_field(fields, "status", index)? {
                    "success" => BehaviorStatus::Success,
                    "failure" => BehaviorStatus::Failure,
                    "running" => BehaviorStatus::Running,
                    other => {
                        return Err(GameCommandError::InvalidPayload {
                            index,
                            message: format!("unknown behavior tree status `{other}`"),
                        })
                    }
                };
                prepared.push(PreparedGameCommand::SetBehaviorTreeResult {
                    behavior_id,
                    is_action,
                    status,
                });
            }
            GameCommandFamily::Despawn => {
                let (_target, entity) = targeted_entity(world, command, index, &despawned)?;
                if command.payload != Value::Null {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: "despawn payload must be null".to_owned(),
                    });
                }
                despawned.insert((entity.id(), entity.generation()));
                prepared.push(PreparedGameCommand::Despawn { entity });
            }
            GameCommandFamily::LockOn => {
                if command.target.is_some() {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: "lock-on command must not contain an entity target".to_owned(),
                    });
                }
                if world.get_resource::<TargetLock>().is_none() {
                    return Err(GameCommandError::MissingTargetLock { index });
                }
                prepared.push(parse_lock_on_command(index, &command.payload)?);
            }
            GameCommandFamily::Animation => {
                let (target, entity) = targeted_entity(world, command, index, &despawned)?;
                prepared.push(PreparedGameCommand::Animation(animation::prepare(
                    world,
                    index,
                    target,
                    entity,
                    &command.payload,
                )?));
            }
            GameCommandFamily::Ui => prepared.push(parse_ui_command(
                world,
                command,
                index,
                &despawned,
                &mut inserted_ui_visibility,
            )?),
            GameCommandFamily::Scene => prepared.push(parse_scene_command(world, command, index)?),
            GameCommandFamily::Audio => {
                prepared.push(PreparedGameCommand::Audio(parse_audio_command(
                    world,
                    command,
                    index,
                    &despawned,
                    &mut pending_audio_commands,
                )?));
            }
            GameCommandFamily::Save => prepared.push(parse_save_command(
                world,
                command,
                index,
                &mut pending_save_commands,
            )?),
            GameCommandFamily::Timer => prepared.push(parse_timer_command(
                world,
                command,
                index,
                &mut virtual_timer_ids,
            )?),
            GameCommandFamily::PrefabSpawn => prepared.push(parse_prefab_spawn_command(
                world,
                command,
                index,
                &mut pending_prefab_requests,
            )?),
            GameCommandFamily::Component => prepared.push(parse_component_command(
                world,
                command,
                index,
                &despawned,
                &mut virtual_components,
            )?),
            GameCommandFamily::Hitbox => prepared.push(parse_hitbox_command(
                world,
                command,
                index,
                &despawned,
                &mut virtual_hitboxes,
            )?),
            family => return Err(GameCommandError::UnsupportedFamily { index, family }),
        }
    }
    Ok(prepared)
}

/// Applies commands whose complete failure surface was checked by preflight.
pub(crate) fn apply_prepared_game_commands(world: &mut World, commands: Vec<PreparedGameCommand>) {
    for command in commands {
        match command {
            PreparedGameCommand::SetTransform {
                entity,
                translation,
                rotation,
                scale,
            } => {
                let transform = world
                    .get_component_mut::<Transform>(entity)
                    .expect("preflighted transform must remain live during exclusive apply");
                transform.translation = translation;
                transform.rotation = rotation;
                transform.scale = scale;
            }
            PreparedGameCommand::Translate { entity, delta } => {
                let transform = world
                    .get_component_mut::<Transform>(entity)
                    .expect("preflighted transform must remain live during exclusive apply");
                transform.translation += delta;
            }
            PreparedGameCommand::Rotate { entity, delta } => {
                let transform = world
                    .get_component_mut::<Transform>(entity)
                    .expect("preflighted transform must remain live during exclusive apply");
                transform.rotation = (transform.rotation * delta).normalize();
            }
            PreparedGameCommand::Despawn { entity } => {
                world
                    .despawn(entity)
                    .expect("preflighted entity must remain live during exclusive apply");
            }
            PreparedGameCommand::SetCharacterMotion {
                entity,
                velocity,
                rotation,
            } => {
                world
                    .get_component_mut::<KinematicCharacterController>(entity)
                    .expect(
                        "preflighted character controller must remain live during exclusive apply",
                    )
                    .velocity = velocity;
                world
                    .get_component_mut::<Transform>(entity)
                    .expect(
                        "preflighted character transform must remain live during exclusive apply",
                    )
                    .rotation = rotation;
            }
            PreparedGameCommand::SetNavigationTarget { entity, target } => {
                world
                    .get_component_mut::<NavMeshAgent>(entity)
                    .expect("preflighted navigation agent must remain live")
                    .set_target(target);
            }
            PreparedGameCommand::SetBehaviorTreeResult {
                behavior_id,
                is_action,
                status,
            } => {
                let registry = world
                    .get_resource_mut::<BehaviorTreeBehaviorRegistry>()
                    .expect("preflighted behavior tree registry must remain installed");
                if is_action {
                    registry.set_action(behavior_id, status);
                } else {
                    registry.set_condition(behavior_id, status);
                }
            }
            PreparedGameCommand::LockOn(operation) => {
                let lock = world
                    .get_resource_mut::<TargetLock>()
                    .expect("preflighted target-lock resource must remain installed");
                match operation {
                    LockOnOperation::Acquire => lock.request_acquire(),
                    LockOnOperation::Cycle => lock.request_cycle(),
                    LockOnOperation::Release => lock.request_release(),
                }
            }
            PreparedGameCommand::Animation(command) => animation::apply(world, command),
            PreparedGameCommand::SetUiBinding { name, value } => {
                world
                    .get_resource_mut::<UiBindings>()
                    .expect("preflighted UI bindings must remain installed")
                    .set(name, value);
            }
            PreparedGameCommand::RemoveUiBinding { name } => {
                world
                    .get_resource_mut::<UiBindings>()
                    .expect("preflighted UI bindings must remain installed")
                    .remove(&name);
            }
            PreparedGameCommand::SetUiVisibility {
                entity,
                visible,
                has_override,
            } => {
                if has_override {
                    world
                        .get_component_mut::<UiDocumentVisibility>(entity)
                        .expect("preflighted UI visibility override must remain live")
                        .visible = visible;
                } else {
                    world
                        .add_component(entity, UiDocumentVisibility { visible })
                        .expect("exclusive preflight guarantees visibility is still absent");
                }
            }
            PreparedGameCommand::RequestScene { path } => {
                world
                    .get_resource_mut::<SceneManager>()
                    .expect("preflighted scene manager must remain installed")
                    .request_switch(path);
            }
            PreparedGameCommand::Audio(command) => {
                world
                    .get_resource_mut::<GameAudioCommandQueue>()
                    .expect("preflighted game audio queue must remain installed")
                    .push_preflighted(command);
            }
            PreparedGameCommand::SetSaveValue { key, value } => {
                world
                    .get_resource_mut::<SaveData>()
                    .expect("preflighted active save data must remain installed")
                    .set(key, value);
            }
            PreparedGameCommand::RemoveSaveValue { key } => {
                world
                    .get_resource_mut::<SaveData>()
                    .expect("preflighted active save data must remain installed")
                    .remove(&key);
            }
            PreparedGameCommand::SaveSlot(operation) => {
                let command = match operation {
                    SaveSlotOperation::Write { slot } => GameSaveCommand::Write {
                        slot,
                        data: world
                            .get_resource::<SaveData>()
                            .expect("preflighted active save data must remain installed")
                            .clone(),
                    },
                    SaveSlotOperation::Load { slot } => GameSaveCommand::Load { slot },
                };
                world
                    .get_resource_mut::<GameSaveCommandQueue>()
                    .expect("preflighted game save queue must remain installed")
                    .push_preflighted(command);
            }
            PreparedGameCommand::Timer(operation) => match operation {
                TimerOperation::Set {
                    timer_id,
                    duration_seconds,
                } => world
                    .get_resource_mut::<GameTimers>()
                    .expect("preflighted game timer service must remain installed")
                    .set_preflighted(timer_id, duration_seconds),
                TimerOperation::Cancel { timer_id } => world
                    .get_resource_mut::<GameTimers>()
                    .expect("preflighted game timer service must remain installed")
                    .cancel(&timer_id),
                TimerOperation::Query {
                    timer_id,
                    request_id,
                } => {
                    let timers = world
                        .remove_resource::<GameTimers>()
                        .expect("preflighted game timer service must remain installed");
                    query_game_timer(
                        &timers,
                        world
                            .get_resource_mut::<GameTimerEvents>()
                            .expect("preflighted timer event service must remain installed"),
                        &timer_id,
                        request_id,
                    );
                    world.insert_resource(timers);
                }
            },
            PreparedGameCommand::SpawnPrefab(request) => world
                .get_resource_mut::<GamePrefabSpawnQueue>()
                .expect("preflighted game prefab queue must remain installed")
                .push_preflighted(request),
            PreparedGameCommand::AddProjectComponent {
                entity,
                component_type,
                value,
            } => world
                .get_component_mut::<GameComponentStore>(entity)
                .expect("preflighted project component store must remain installed")
                .insert_runtime_value(component_type, value),
            PreparedGameCommand::RemoveProjectComponent {
                entity,
                component_type,
            } => world
                .get_component_mut::<GameComponentStore>(entity)
                .expect("preflighted project component store must remain installed")
                .remove_runtime_value(&component_type),
            PreparedGameCommand::SetProjectComponentEnabled {
                entity,
                component_type,
                enabled,
            } => world
                .get_component_mut::<GameComponentStore>(entity)
                .expect("preflighted project component store must remain installed")
                .set_enabled(&component_type, enabled),
            PreparedGameCommand::CreateHitbox {
                entity,
                collider,
                layers,
                hitbox,
            } => {
                world
                    .add_component(entity, collider)
                    .expect("preflighted hitbox carrier must not have Collider");
                world
                    .add_component(entity, PhysicsBody::Static)
                    .expect("preflighted hitbox carrier must not have PhysicsBody");
                world
                    .add_component(entity, TriggerVolume)
                    .expect("preflighted hitbox carrier must not have TriggerVolume");
                world
                    .add_component(entity, layers)
                    .expect("preflighted hitbox carrier must not have CollisionLayers");
                world
                    .add_component(entity, hitbox)
                    .expect("preflighted hitbox carrier must not have AttackHitbox");
            }
            PreparedGameCommand::SetHitboxEnabled { entity, enabled } => world
                .get_component_mut::<AttackHitbox>(entity)
                .expect("preflighted hitbox must remain installed")
                .set_enabled(enabled),
            PreparedGameCommand::RemoveHitbox { entity } => {
                world
                    .remove_component::<AttackHitbox>(entity)
                    .expect("preflighted AttackHitbox must remain installed");
                world
                    .remove_component::<Collider>(entity)
                    .expect("command-owned Collider must remain installed");
                world
                    .remove_component::<PhysicsBody>(entity)
                    .expect("command-owned PhysicsBody must remain installed");
                world
                    .remove_component::<TriggerVolume>(entity)
                    .expect("command-owned TriggerVolume must remain installed");
                world
                    .remove_component::<CollisionLayers>(entity)
                    .expect("command-owned CollisionLayers must remain installed");
            }
        }
    }
}

fn targeted_entity(
    world: &World,
    command: &GameCommand,
    index: usize,
    despawned: &BTreeSet<(u32, u32)>,
) -> Result<(GameEntityHandle, Entity), GameCommandError> {
    let target = command
        .target
        .ok_or(GameCommandError::MissingTarget { index })?;
    let entity = validate_live_entity(world, target, index)?;
    if despawned.contains(&(entity.id(), entity.generation())) {
        return Err(GameCommandError::CommandAfterDespawn { index, target });
    }
    Ok((target, entity))
}

fn parse_transform_command(
    index: usize,
    entity: Entity,
    payload: &Value,
) -> Result<PreparedGameCommand, GameCommandError> {
    let fields = object(payload, index, "transform payload")?;
    let operation = string_field(fields, "operation", index)?;
    match operation {
        "set" => Ok(PreparedGameCommand::SetTransform {
            entity,
            translation: vector3_field(fields, "translation", index)?,
            rotation: quaternion_field(fields, "rotation", index)?,
            scale: vector3_field(fields, "scale", index)?,
        }),
        "translate" => Ok(PreparedGameCommand::Translate {
            entity,
            delta: vector3_field(fields, "delta", index)?,
        }),
        "rotate" => Ok(PreparedGameCommand::Rotate {
            entity,
            delta: quaternion_field(fields, "delta", index)?,
        }),
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown transform operation `{other}`"),
        }),
    }
}

fn parse_character_command(
    index: usize,
    entity: Entity,
    payload: &Value,
) -> Result<PreparedGameCommand, GameCommandError> {
    let fields = object(payload, index, "character payload")?;
    if string_field(fields, "operation", index)? != "set_motion" {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "character operation must be `set_motion`".to_owned(),
        });
    }
    let velocity = vector3_field(fields, "velocity", index)?;
    let facing = vector3_field(fields, "facing", index)?;
    if facing.length_squared() <= f32::EPSILON {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "field `facing` must be a finite non-zero vector".to_owned(),
        });
    }
    Ok(PreparedGameCommand::SetCharacterMotion {
        entity,
        velocity,
        rotation: Quat::from_rotation_arc(Vec3::NEG_Z, facing.normalize()),
    })
}

fn parse_lock_on_command(
    index: usize,
    payload: &Value,
) -> Result<PreparedGameCommand, GameCommandError> {
    let fields = object(payload, index, "lock-on payload")?;
    let operation = match string_field(fields, "operation", index)? {
        "acquire" => LockOnOperation::Acquire,
        "cycle" => LockOnOperation::Cycle,
        "release" => LockOnOperation::Release,
        other => {
            return Err(GameCommandError::InvalidPayload {
                index,
                message: format!("unknown lock-on operation `{other}`"),
            })
        }
    };
    Ok(PreparedGameCommand::LockOn(operation))
}

fn parse_ui_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    despawned: &BTreeSet<(u32, u32)>,
    inserted_ui_visibility: &mut BTreeSet<(u32, u32)>,
) -> Result<PreparedGameCommand, GameCommandError> {
    let fields = object(&command.payload, index, "UI payload")?;
    match string_field(fields, "operation", index)? {
        "set_binding" => {
            require_targetless(command, index, "UI binding")?;
            require_ui_bindings(world, index)?;
            let name = non_empty_name(fields, index)?;
            let value = match fields.get("value") {
                Some(Value::String(value)) => UiBindingValue::Text(value.clone()),
                Some(Value::Bool(value)) => UiBindingValue::Flag(*value),
                Some(Value::F64(value)) if value.is_finite() => UiBindingValue::Number(*value),
                Some(Value::I64(value)) => UiBindingValue::Number(*value as f64),
                Some(Value::U64(value)) => UiBindingValue::Number(*value as f64),
                _ => {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: "field `value` must be a finite string, number, or boolean"
                            .to_owned(),
                    })
                }
            };
            Ok(PreparedGameCommand::SetUiBinding { name, value })
        }
        "remove_binding" => {
            require_targetless(command, index, "UI binding")?;
            require_ui_bindings(world, index)?;
            Ok(PreparedGameCommand::RemoveUiBinding {
                name: non_empty_name(fields, index)?,
            })
        }
        "set_visibility" => {
            let (target, entity) = targeted_entity(world, command, index, despawned)?;
            if world.get_component::<UiDocumentRef>(entity).is_none() {
                return Err(GameCommandError::MissingUiDocument { index, target });
            }
            Ok(PreparedGameCommand::SetUiVisibility {
                entity,
                visible: bool_field(fields, "visible", index)?,
                has_override: world
                    .get_component::<UiDocumentVisibility>(entity)
                    .is_some()
                    || !inserted_ui_visibility.insert((entity.id(), entity.generation())),
            })
        }
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown UI operation `{other}`"),
        }),
    }
}

fn parse_scene_command(
    world: &World,
    command: &GameCommand,
    index: usize,
) -> Result<PreparedGameCommand, GameCommandError> {
    require_targetless(command, index, "scene")?;
    if world.get_resource::<SceneManager>().is_none() {
        return Err(GameCommandError::MissingSceneManager { index });
    }
    let fields = object(&command.payload, index, "scene payload")?;
    if string_field(fields, "operation", index)? != "request" {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "scene operation must be `request`".to_owned(),
        });
    }
    let path = string_field(fields, "path", index)?;
    let parsed = Path::new(path);
    if path.is_empty()
        || !parsed.is_relative()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.ends_with(".scene.json")
    {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "field `path` must be a safe project-relative `*.scene.json` path".to_owned(),
        });
    }
    Ok(PreparedGameCommand::RequestScene {
        path: path.to_owned(),
    })
}

fn parse_audio_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    despawned: &BTreeSet<(u32, u32)>,
    pending_audio_commands: &mut usize,
) -> Result<GameAudioCommand, GameCommandError> {
    let queue = world
        .get_resource::<GameAudioCommandQueue>()
        .ok_or(GameCommandError::MissingAudioQueue { index })?;
    if queue.len().saturating_add(*pending_audio_commands) >= MAX_GAME_AUDIO_COMMANDS {
        return Err(GameCommandError::AudioQueueFull {
            index,
            maximum: MAX_GAME_AUDIO_COMMANDS,
        });
    }

    let fields = object(&command.payload, index, "audio payload")?;
    let operation = string_field(fields, "operation", index)?;
    let prepared = match operation {
        "play_spatial_se" => {
            let (target, source) = targeted_entity(world, command, index, despawned)?;
            if world.get_component::<Transform>(source).is_none() {
                return Err(GameCommandError::MissingTransform { index, target });
            }
            let volume = number_field(fields, "volume", index)?;
            let spatial_blend = number_field(fields, "spatial_blend", index)?;
            let min_distance = number_field(fields, "min_distance", index)?;
            let max_distance = number_field(fields, "max_distance", index)?;
            if !(0.0..=1.0).contains(&volume) || !(0.0..=1.0).contains(&spatial_blend) {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "audio volume and spatial_blend must be between zero and one".to_owned(),
                });
            }
            if min_distance < 0.0 || max_distance < min_distance {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "audio distances must satisfy 0 <= min_distance <= max_distance".to_owned(),
                });
            }
            let rolloff = match string_field(fields, "rolloff", index)? {
                "linear" => SpatialRolloff::Linear,
                "inverse" => SpatialRolloff::Inverse,
                value => {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: format!("field `rolloff` has unknown value `{value}`"),
                    });
                }
            };
            GameAudioCommand::PlaySpatialSoundEffect {
                asset_id: validate_audio_asset(world, fields, index)?,
                source,
                options: GameSpatialAudioOptions {
                    volume,
                    spatial_blend,
                    min_distance,
                    max_distance,
                    rolloff,
                    looping: bool_field(fields, "looping", index)?,
                },
            }
        }
        "play_se" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::PlaySoundEffect {
                asset_id: validate_audio_asset(world, fields, index)?,
            }
        }
        "play_bgm" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::PlayBackgroundMusic {
                asset_id: validate_audio_asset(world, fields, index)?,
                fade_seconds: 0.0,
            }
        }
        "crossfade_bgm" => {
            require_targetless(command, index, "audio")?;
            let fade_seconds = number_field(fields, "fade_seconds", index)?;
            if fade_seconds < 0.0 {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "field `fade_seconds` must be zero or positive".to_owned(),
                });
            }
            GameAudioCommand::PlayBackgroundMusic {
                asset_id: validate_audio_asset(world, fields, index)?,
                fade_seconds,
            }
        }
        "stop_bgm" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::StopBackgroundMusic
        }
        "set_master_volume" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::SetMasterVolume(number_field(fields, "volume", index)?)
        }
        "set_bgm_volume" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::SetBackgroundMusicVolume(number_field(fields, "volume", index)?)
        }
        "set_se_volume" => {
            require_targetless(command, index, "audio")?;
            GameAudioCommand::SetSoundEffectVolume(number_field(fields, "volume", index)?)
        }
        other => {
            return Err(GameCommandError::InvalidPayload {
                index,
                message: format!("unknown audio operation `{other}`"),
            })
        }
    };
    *pending_audio_commands += 1;
    Ok(prepared)
}

fn parse_save_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    pending_save_commands: &mut usize,
) -> Result<PreparedGameCommand, GameCommandError> {
    require_targetless(command, index, "save")?;
    if world.get_resource::<SaveData>().is_none() {
        return Err(GameCommandError::MissingSaveData { index });
    }
    let fields = object(&command.payload, index, "save payload")?;
    match string_field(fields, "operation", index)? {
        "set" => {
            let key = validate_save_key(fields, index)?;
            let value = match fields.get("value") {
                Some(Value::String(value)) => SaveValue::Text(value.clone()),
                Some(Value::Bool(value)) => SaveValue::Flag(*value),
                Some(Value::F64(value)) if value.is_finite() => SaveValue::Number(*value),
                Some(Value::I64(value)) => SaveValue::Number(*value as f64),
                Some(Value::U64(value)) => SaveValue::Number(*value as f64),
                _ => {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: "field `value` must be a finite string, number, or boolean"
                            .to_owned(),
                    });
                }
            };
            Ok(PreparedGameCommand::SetSaveValue { key, value })
        }
        "remove" => Ok(PreparedGameCommand::RemoveSaveValue {
            key: validate_save_key(fields, index)?,
        }),
        operation @ ("write" | "load") => {
            let queue = world
                .get_resource::<GameSaveCommandQueue>()
                .ok_or(GameCommandError::MissingSaveQueue { index })?;
            if queue.len().saturating_add(*pending_save_commands) >= MAX_GAME_SAVE_COMMANDS {
                return Err(GameCommandError::SaveQueueFull {
                    index,
                    maximum: MAX_GAME_SAVE_COMMANDS,
                });
            }
            let slot = u32_field(fields, "slot", index)?;
            *pending_save_commands += 1;
            Ok(PreparedGameCommand::SaveSlot(match operation {
                "write" => SaveSlotOperation::Write { slot },
                "load" => SaveSlotOperation::Load { slot },
                _ => unreachable!("operation match is exhaustive"),
            }))
        }
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown save operation `{other}`"),
        }),
    }
}

fn parse_timer_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    virtual_timer_ids: &mut Option<BTreeSet<String>>,
) -> Result<PreparedGameCommand, GameCommandError> {
    require_targetless(command, index, "timer")?;
    let Some(timer_ids) = virtual_timer_ids.as_mut() else {
        return Err(GameCommandError::MissingTimerService { index });
    };
    if world.get_resource::<GameTimerEvents>().is_none() {
        return Err(GameCommandError::MissingTimerEvents { index });
    }
    let fields = object(&command.payload, index, "timer payload")?;
    let timer_id = string_field(fields, "timer_id", index)?;
    SystemId::try_new(timer_id.to_owned()).map_err(|error| GameCommandError::InvalidPayload {
        index,
        message: format!("field `timer_id` must be a stable dotted ID: {error}"),
    })?;
    match string_field(fields, "operation", index)? {
        "set" => {
            if command.request_id.is_some() {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "set timer command must not contain a request ID".to_owned(),
                });
            }
            let duration_seconds = number_field(fields, "duration_seconds", index)?;
            if duration_seconds < 0.0 {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "field `duration_seconds` must be zero or positive".to_owned(),
                });
            }
            if !timer_ids.contains(timer_id) && timer_ids.len() >= MAX_GAME_TIMERS {
                return Err(GameCommandError::TimerCapacityFull {
                    index,
                    maximum: MAX_GAME_TIMERS,
                });
            }
            timer_ids.insert(timer_id.to_owned());
            Ok(PreparedGameCommand::Timer(TimerOperation::Set {
                timer_id: timer_id.to_owned(),
                duration_seconds,
            }))
        }
        "cancel" => {
            if command.request_id.is_some() {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "cancel timer command must not contain a request ID".to_owned(),
                });
            }
            timer_ids.remove(timer_id);
            Ok(PreparedGameCommand::Timer(TimerOperation::Cancel {
                timer_id: timer_id.to_owned(),
            }))
        }
        "query" => Ok(PreparedGameCommand::Timer(TimerOperation::Query {
            timer_id: timer_id.to_owned(),
            request_id: command
                .request_id
                .ok_or_else(|| GameCommandError::InvalidPayload {
                    index,
                    message: "query timer command requires a request ID".to_owned(),
                })?,
        })),
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown timer operation `{other}`"),
        }),
    }
}

fn parse_prefab_spawn_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    pending_prefab_requests: &mut usize,
) -> Result<PreparedGameCommand, GameCommandError> {
    require_targetless(command, index, "prefab spawn")?;
    let queue = world
        .get_resource::<GamePrefabSpawnQueue>()
        .ok_or(GameCommandError::MissingPrefabQueue { index })?;
    if world.get_resource::<GamePrefabEvents>().is_none() {
        return Err(GameCommandError::MissingPrefabEvents { index });
    }
    if queue.len().saturating_add(*pending_prefab_requests) >= MAX_GAME_PREFAB_REQUESTS {
        return Err(GameCommandError::PrefabQueueFull {
            index,
            maximum: MAX_GAME_PREFAB_REQUESTS,
        });
    }
    let fields = object(&command.payload, index, "prefab spawn payload")?;
    if string_field(fields, "operation", index)? != "spawn" {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "prefab spawn operation must be `spawn`".to_owned(),
        });
    }
    let path = string_field(fields, "path", index)?;
    let parsed = Path::new(path);
    if path.is_empty()
        || !parsed.is_relative()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.ends_with(".prefab.json")
    {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "field `path` must be a safe project-relative `*.prefab.json` path".to_owned(),
        });
    }
    let request_id = command
        .request_id
        .ok_or_else(|| GameCommandError::InvalidPayload {
            index,
            message: "prefab spawn command requires a request ID".to_owned(),
        })?;
    *pending_prefab_requests += 1;
    Ok(PreparedGameCommand::SpawnPrefab(GamePrefabSpawnRequest {
        path: path.to_owned(),
        position: vector3_field(fields, "position", index)?,
        request_id,
    }))
}

fn parse_component_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    despawned: &BTreeSet<(u32, u32)>,
    virtual_components: &mut BTreeMap<(u32, u32, ComponentTypeId), Option<bool>>,
) -> Result<PreparedGameCommand, GameCommandError> {
    let (target, entity) = targeted_entity(world, command, index, despawned)?;
    if command.request_id.is_some() {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "component command must not contain a request ID".to_owned(),
        });
    }
    let store = world
        .get_component::<GameComponentStore>(entity)
        .ok_or(GameCommandError::MissingProjectComponentStore { index, target })?;
    let fields = object(&command.payload, index, "component payload")?;
    let component_text = string_field(fields, "component_type", index)?;
    let component_type = ComponentTypeId::try_new(component_text.to_owned()).map_err(|error| {
        GameCommandError::InvalidPayload {
            index,
            message: format!("field `component_type` is invalid: {error}"),
        }
    })?;
    let key = (entity.id(), entity.generation(), component_type.clone());
    let state = virtual_components
        .entry(key)
        .or_insert_with(|| store.is_enabled(&component_type));
    match string_field(fields, "operation", index)? {
        "add" => {
            if state.is_some() {
                return Err(GameCommandError::ProjectComponentAlreadyPresent {
                    index,
                    target,
                    component_type,
                });
            }
            let value = world
                .get_resource::<GameComponentDefaults>()
                .and_then(|defaults| defaults.get(&component_type))
                .cloned()
                .ok_or_else(|| GameCommandError::UnknownProjectComponent {
                    index,
                    component_type: component_type.clone(),
                })?;
            *state = Some(true);
            Ok(PreparedGameCommand::AddProjectComponent {
                entity,
                component_type,
                value,
            })
        }
        "remove" => {
            if state.is_none() {
                return Err(GameCommandError::MissingProjectComponent {
                    index,
                    target,
                    component_type,
                });
            }
            *state = None;
            Ok(PreparedGameCommand::RemoveProjectComponent {
                entity,
                component_type,
            })
        }
        operation @ ("enable" | "disable") => {
            if state.is_none() {
                return Err(GameCommandError::MissingProjectComponent {
                    index,
                    target,
                    component_type,
                });
            }
            let enabled = operation == "enable";
            *state = Some(enabled);
            Ok(PreparedGameCommand::SetProjectComponentEnabled {
                entity,
                component_type,
                enabled,
            })
        }
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown component operation `{other}`"),
        }),
    }
}

fn parse_hitbox_command(
    world: &World,
    command: &GameCommand,
    index: usize,
    despawned: &BTreeSet<(u32, u32)>,
    virtual_hitboxes: &mut BTreeMap<(u32, u32), Option<bool>>,
) -> Result<PreparedGameCommand, GameCommandError> {
    let (target, entity) = targeted_entity(world, command, index, despawned)?;
    if command.request_id.is_some() {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "hitbox command must not contain a request ID".to_owned(),
        });
    }
    if world.get_component::<Transform>(entity).is_none() {
        return Err(GameCommandError::MissingTransform { index, target });
    }
    let fields = object(&command.payload, index, "hitbox payload")?;
    let state = virtual_hitboxes
        .entry((entity.id(), entity.generation()))
        .or_insert_with(|| {
            world
                .get_component::<AttackHitbox>(entity)
                .map(|hitbox| hitbox.enabled)
        });
    match string_field(fields, "operation", index)? {
        "create" => {
            if state.is_some() {
                return Err(GameCommandError::HitboxAlreadyPresent { index, target });
            }
            let replaces_command_hitbox = world.get_component::<AttackHitbox>(entity).is_some();
            if !replaces_command_hitbox
                && (world.get_component::<Collider>(entity).is_some()
                    || world.get_component::<PhysicsBody>(entity).is_some()
                    || world.get_component::<TriggerVolume>(entity).is_some()
                    || world.get_component::<CollisionLayers>(entity).is_some())
            {
                return Err(GameCommandError::HitboxCollisionConflict { index, target });
            }
            let owner = entity_handle_field(fields, "owner", index)?;
            let owner_entity = validate_live_entity(world, owner, index)?;
            if despawned.contains(&(owner_entity.id(), owner_entity.generation())) {
                return Err(GameCommandError::CommandAfterDespawn {
                    index,
                    target: owner,
                });
            }
            let shape = object(
                fields
                    .get("shape")
                    .ok_or_else(|| GameCommandError::InvalidPayload {
                        index,
                        message: "field `shape` is missing".to_owned(),
                    })?,
                index,
                "shape",
            )?;
            let collider = match string_field(shape, "kind", index)? {
                "aabb" => {
                    let half_extents = vector3_field(shape, "half_extents", index)?;
                    if half_extents.min_element() <= 0.0 {
                        return Err(GameCommandError::InvalidPayload {
                            index,
                            message: "hitbox AABB half-extents must be positive".to_owned(),
                        });
                    }
                    Collider::aabb(half_extents)
                }
                "sphere" => Collider::sphere(positive_field(shape, "radius", index)?),
                "capsule_y" => {
                    let half_height = number_field(shape, "half_height", index)?;
                    if half_height < 0.0 {
                        return Err(GameCommandError::InvalidPayload {
                            index,
                            message: "hitbox capsule half-height must be zero or positive"
                                .to_owned(),
                        });
                    }
                    Collider::capsule_y(half_height, positive_field(shape, "radius", index)?)
                }
                other => {
                    return Err(GameCommandError::InvalidPayload {
                        index,
                        message: format!("unknown hitbox shape `{other}`"),
                    })
                }
            };
            let damage = number_field(fields, "damage", index)?;
            if damage < 0.0 {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "field `damage` must be zero or positive".to_owned(),
                });
            }
            *state = Some(true);
            Ok(PreparedGameCommand::CreateHitbox {
                entity,
                collider,
                layers: CollisionLayers {
                    membership: u32_field(fields, "membership", index)?,
                    mask: u32_field(fields, "mask", index)?,
                },
                hitbox: AttackHitbox::new(
                    owner_entity,
                    i32_field(fields, "team", index)?,
                    damage,
                    bool_field(fields, "one_hit_per_target", index)?,
                    true,
                )
                .with_knockback(match fields.get("knockback") {
                    Some(_) => vector3_field(fields, "knockback", index)?,
                    None => Vec3::ZERO,
                }),
            })
        }
        operation @ ("enable" | "disable") => {
            if state.is_none() {
                return Err(GameCommandError::MissingHitbox { index, target });
            }
            let enabled = operation == "enable";
            *state = Some(enabled);
            Ok(PreparedGameCommand::SetHitboxEnabled { entity, enabled })
        }
        "remove" => {
            if state.is_none() {
                return Err(GameCommandError::MissingHitbox { index, target });
            }
            *state = None;
            Ok(PreparedGameCommand::RemoveHitbox { entity })
        }
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown hitbox operation `{other}`"),
        }),
    }
}

fn validate_save_key(
    fields: &BTreeMap<String, Value>,
    index: usize,
) -> Result<String, GameCommandError> {
    let key = string_field(fields, "key", index)?;
    if key.trim().is_empty()
        || key.len() > MAX_GAME_SAVE_KEY_BYTES
        || key.chars().any(char::is_control)
    {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!(
                "field `key` must be non-empty, contain no control characters, and be at most {MAX_GAME_SAVE_KEY_BYTES} UTF-8 bytes"
            ),
        });
    }
    Ok(key.to_owned())
}

fn validate_audio_asset(
    world: &World,
    fields: &BTreeMap<String, Value>,
    index: usize,
) -> Result<String, GameCommandError> {
    let text = string_field(fields, "asset_id", index)?;
    let asset_id = AssetId::from_stable_id(StableId::new(text)).map_err(|_| {
        GameCommandError::InvalidPayload {
            index,
            message: format!("field `asset_id` contains invalid stable ID `{text}`"),
        }
    })?;
    let manifest = world
        .get_resource::<AssetManifest>()
        .ok_or(GameCommandError::MissingAssetManifest { index })?;
    if manifest.get(&asset_id).is_none() {
        return Err(GameCommandError::UnknownAudioAsset {
            index,
            asset_id: text.to_owned(),
        });
    }
    Ok(text.to_owned())
}

fn require_targetless(
    command: &GameCommand,
    index: usize,
    label: &str,
) -> Result<(), GameCommandError> {
    if command.target.is_some() {
        Err(GameCommandError::InvalidPayload {
            index,
            message: format!("{label} command must not contain an entity target"),
        })
    } else {
        Ok(())
    }
}

fn require_ui_bindings(world: &World, index: usize) -> Result<(), GameCommandError> {
    if world.get_resource::<UiBindings>().is_none() {
        Err(GameCommandError::MissingUiBindings { index })
    } else {
        Ok(())
    }
}

fn non_empty_name(
    fields: &BTreeMap<String, Value>,
    index: usize,
) -> Result<String, GameCommandError> {
    let name = string_field(fields, "name", index)?;
    if name.trim().is_empty() {
        Err(GameCommandError::InvalidPayload {
            index,
            message: "field `name` must not be empty".to_owned(),
        })
    } else {
        Ok(name.to_owned())
    }
}

fn validate_live_entity(
    world: &World,
    handle: GameEntityHandle,
    index: usize,
) -> Result<Entity, GameCommandError> {
    let entity = Entity::from_raw(handle.id, handle.generation);
    world
        .contains_entity(entity)
        .then_some(entity)
        .ok_or(GameCommandError::StaleTarget {
            index,
            target: handle,
        })
}

fn object<'a>(
    value: &'a Value,
    index: usize,
    label: &str,
) -> Result<&'a BTreeMap<String, Value>, GameCommandError> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("{label} must be an object"),
        }),
    }
}

fn string_field<'a>(
    fields: &'a BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<&'a str, GameCommandError> {
    match fields.get(name) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be a string"),
        }),
    }
}

fn bool_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<bool, GameCommandError> {
    match fields.get(name) {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be a boolean"),
        }),
    }
}

fn runtime_id_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<u64, GameCommandError> {
    let Some(Value::String(value)) = fields.get(name) else {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be a decimal string"),
        });
    };
    value.parse().map_err(|_| GameCommandError::InvalidPayload {
        index,
        message: format!("field `{name}` must contain an unsigned decimal integer"),
    })
}

fn vector3_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<Vec3, GameCommandError> {
    let value = fields
        .get(name)
        .ok_or_else(|| GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` is missing"),
        })?;
    let vector = object(value, index, name)?;
    Ok(Vec3::new(
        number_field(vector, "x", index)?,
        number_field(vector, "y", index)?,
        number_field(vector, "z", index)?,
    ))
}

fn quaternion_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<Quat, GameCommandError> {
    let value = fields
        .get(name)
        .ok_or_else(|| GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` is missing"),
        })?;
    let quaternion = object(value, index, name)?;
    let value = Quat::from_xyzw(
        number_field(quaternion, "x", index)?,
        number_field(quaternion, "y", index)?,
        number_field(quaternion, "z", index)?,
        number_field(quaternion, "w", index)?,
    );
    if !value.is_finite() || value.length_squared() <= f32::EPSILON {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be a finite non-zero quaternion"),
        });
    }
    Ok(value.normalize())
}

fn number_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<f32, GameCommandError> {
    let value = match fields.get(name) {
        Some(Value::F64(value)) => *value as f32,
        Some(Value::I64(value)) => *value as f32,
        Some(Value::U64(value)) => *value as f32,
        _ => {
            return Err(GameCommandError::InvalidPayload {
                index,
                message: format!("field `{name}` must be numeric"),
            })
        }
    };
    if !value.is_finite() {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be finite"),
        });
    }
    Ok(value)
}

fn u32_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<u32, GameCommandError> {
    let value = match fields.get(name) {
        Some(Value::U64(value)) => u32::try_from(*value).ok(),
        Some(Value::I64(value)) => u32::try_from(*value).ok(),
        _ => None,
    };
    value.ok_or_else(|| GameCommandError::InvalidPayload {
        index,
        message: format!("field `{name}` must be an unsigned 32-bit integer"),
    })
}

fn i32_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<i32, GameCommandError> {
    let value = match fields.get(name) {
        Some(Value::I64(value)) => i32::try_from(*value).ok(),
        Some(Value::U64(value)) => i32::try_from(*value).ok(),
        _ => None,
    };
    value.ok_or_else(|| GameCommandError::InvalidPayload {
        index,
        message: format!("field `{name}` must be a signed 32-bit integer"),
    })
}

fn positive_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<f32, GameCommandError> {
    let value = number_field(fields, name, index)?;
    if value <= 0.0 {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` must be positive"),
        });
    }
    Ok(value)
}

fn entity_handle_field(
    fields: &BTreeMap<String, Value>,
    name: &str,
    index: usize,
) -> Result<GameEntityHandle, GameCommandError> {
    let value = fields
        .get(name)
        .ok_or_else(|| GameCommandError::InvalidPayload {
            index,
            message: format!("field `{name}` is missing"),
        })?;
    let handle = object(value, index, name)?;
    Ok(GameEntityHandle {
        id: u32_field(handle, "id", index)?,
        generation: u32_field(handle, "generation", index)?,
    })
}

/// Reports why a deferred gameplay command was rejected before mutation.
#[derive(Debug, Clone, PartialEq)]
pub enum GameCommandError {
    /// A command family does not have a host processor yet.
    UnsupportedFamily {
        /// Zero-based command index in callback order.
        index: usize,
        /// Rejected family.
        family: GameCommandFamily,
    },
    /// A command requiring an entity omitted its target.
    MissingTarget {
        /// Zero-based command index.
        index: usize,
    },
    /// A target is no longer live at command preflight.
    StaleTarget {
        /// Zero-based command index.
        index: usize,
        /// Rejected generation-checked handle.
        target: GameEntityHandle,
    },
    /// A transform command targeted an entity without a local transform.
    MissingTransform {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// A character command targeted an entity without a controller.
    MissingCharacterController {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// A navigation command targeted an entity without a NavMeshAgent.
    MissingNavigationAgent {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// The host has no Behavior Tree behavior registry installed.
    MissingBehaviorTreeRegistry {
        /// Zero-based command index.
        index: usize,
    },
    /// The host has no lock-on resource installed.
    MissingTargetLock {
        /// Zero-based command index.
        index: usize,
    },
    /// An animation command targeted an entity without an animator.
    MissingAnimator {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// A graph-parameter command targeted an entity without a graph player.
    MissingAnimationGraph {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// A crossfade referenced a runtime animation clip that is not loaded.
    MissingAnimationClip {
        /// Zero-based command index.
        index: usize,
        /// Missing process-local clip ID.
        clip_id: u64,
    },
    /// The host has no UI binding table installed.
    MissingUiBindings {
        /// Zero-based command index.
        index: usize,
    },
    /// A visibility command targeted an entity without a UI document.
    MissingUiDocument {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// The host has no scene-transition service installed.
    MissingSceneManager {
        /// Zero-based command index.
        index: usize,
    },
    /// The host has no bounded project-audio queue installed.
    MissingAudioQueue {
        /// Zero-based command index.
        index: usize,
    },
    /// The bounded project-audio queue has no remaining capacity.
    AudioQueueFull {
        /// Zero-based command index.
        index: usize,
        /// Configured queue capacity.
        maximum: usize,
    },
    /// The host has no active save document installed.
    MissingSaveData {
        /// Zero-based command index.
        index: usize,
    },
    /// The host has no bounded project-save queue installed.
    MissingSaveQueue {
        /// Zero-based command index.
        index: usize,
    },
    /// The bounded project-save queue has no remaining capacity.
    SaveQueueFull {
        /// Zero-based command index.
        index: usize,
        /// Configured queue capacity.
        maximum: usize,
    },
    /// The host has no project gameplay timer service installed.
    MissingTimerService {
        /// Zero-based command index.
        index: usize,
    },
    /// The host has no timer result source log installed.
    MissingTimerEvents {
        /// Zero-based command index.
        index: usize,
    },
    /// A new timer would exceed the bounded timer table.
    TimerCapacityFull {
        /// Zero-based command index.
        index: usize,
        /// Configured timer capacity.
        maximum: usize,
    },
    /// The host has no exclusive-world prefab queue installed.
    MissingPrefabQueue {
        /// Zero-based command index.
        index: usize,
    },
    /// The host has no prefab result source log installed.
    MissingPrefabEvents {
        /// Zero-based command index.
        index: usize,
    },
    /// The bounded prefab request queue has no remaining capacity.
    PrefabQueueFull {
        /// Zero-based command index.
        index: usize,
        /// Configured request capacity.
        maximum: usize,
    },
    /// A component command targeted an entity without project storage.
    MissingProjectComponentStore {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// The active module generation does not define a requested component.
    UnknownProjectComponent {
        /// Zero-based command index.
        index: usize,
        /// Unknown stable component type.
        component_type: ComponentTypeId,
    },
    /// An add command targeted a component already retained by the entity.
    ProjectComponentAlreadyPresent {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
        /// Existing stable component type.
        component_type: ComponentTypeId,
    },
    /// A remove/enable/disable command targeted an absent component.
    MissingProjectComponent {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
        /// Missing stable component type.
        component_type: ComponentTypeId,
    },
    /// A create command targeted an entity that already owns a hitbox.
    HitboxAlreadyPresent {
        /// Zero-based command index.
        index: usize,
        /// Rejected carrier entity.
        target: GameEntityHandle,
    },
    /// A create command would overwrite collision components it does not own.
    HitboxCollisionConflict {
        /// Zero-based command index.
        index: usize,
        /// Rejected carrier entity.
        target: GameEntityHandle,
    },
    /// An enable/disable/remove command targeted an absent hitbox.
    MissingHitbox {
        /// Zero-based command index.
        index: usize,
        /// Rejected carrier entity.
        target: GameEntityHandle,
    },
    /// Audio asset lookup requires the project's manifest.
    MissingAssetManifest {
        /// Zero-based command index.
        index: usize,
    },
    /// An audio asset ID is valid but absent from the manifest.
    UnknownAudioAsset {
        /// Zero-based command index.
        index: usize,
        /// Missing stable asset ID.
        asset_id: String,
    },
    /// A later command targeted an entity already scheduled for despawn.
    CommandAfterDespawn {
        /// Zero-based command index.
        index: usize,
        /// Rejected target.
        target: GameEntityHandle,
    },
    /// A family-specific payload had the wrong shape or value.
    InvalidPayload {
        /// Zero-based command index.
        index: usize,
        /// Actionable payload diagnostic.
        message: String,
    },
}

impl fmt::Display for GameCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFamily { index, family } => {
                write!(
                    formatter,
                    "game command {index} uses unsupported family `{family:?}`"
                )
            }
            Self::MissingTarget { index } => {
                write!(formatter, "game command {index} requires an entity target")
            }
            Self::StaleTarget { index, target } => write!(
                formatter,
                "game command {index} targets stale entity {} generation {}",
                target.id, target.generation
            ),
            Self::MissingTransform { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without Transform",
                target.id, target.generation
            ),
            Self::MissingCharacterController { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without KinematicCharacterController",
                target.id, target.generation
            ),
            Self::MissingNavigationAgent { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without NavMeshAgent",
                target.id, target.generation
            ),
            Self::MissingBehaviorTreeRegistry { index } => write!(
                formatter,
                "game command {index} requires the BehaviorTreeBehaviorRegistry resource"
            ),
            Self::MissingTargetLock { index } => {
                write!(formatter, "game command {index} requires the TargetLock resource")
            }
            Self::MissingAnimator { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without Animator",
                target.id, target.generation
            ),
            Self::MissingAnimationGraph { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without AnimGraphPlayer",
                target.id, target.generation
            ),
            Self::MissingAnimationClip { index, clip_id } => write!(
                formatter,
                "game command {index} references unloaded animation clip runtime ID {clip_id}"
            ),
            Self::MissingUiBindings { index } => {
                write!(formatter, "game command {index} requires the UiBindings resource")
            }
            Self::MissingUiDocument { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without UiDocumentRef",
                target.id, target.generation
            ),
            Self::MissingSceneManager { index } => {
                write!(formatter, "game command {index} requires the SceneManager resource")
            }
            Self::MissingAudioQueue { index } => {
                write!(formatter, "game command {index} requires the project audio queue")
            }
            Self::AudioQueueFull { index, maximum } => write!(
                formatter,
                "game command {index} exceeds the project audio queue capacity of {maximum}"
            ),
            Self::MissingSaveData { index } => {
                write!(formatter, "game command {index} requires the active SaveData resource")
            }
            Self::MissingSaveQueue { index } => {
                write!(formatter, "game command {index} requires the project save queue")
            }
            Self::SaveQueueFull { index, maximum } => write!(
                formatter,
                "game command {index} exceeds the project save queue capacity of {maximum}"
            ),
            Self::MissingTimerService { index } => {
                write!(formatter, "game command {index} requires the project timer service")
            }
            Self::MissingTimerEvents { index } => {
                write!(formatter, "game command {index} requires the timer event source log")
            }
            Self::TimerCapacityFull { index, maximum } => write!(
                formatter,
                "game command {index} exceeds the project timer capacity of {maximum}"
            ),
            Self::MissingPrefabQueue { index } => {
                write!(formatter, "game command {index} requires the project prefab queue")
            }
            Self::MissingPrefabEvents { index } => {
                write!(formatter, "game command {index} requires the prefab result source log")
            }
            Self::PrefabQueueFull { index, maximum } => write!(
                formatter,
                "game command {index} exceeds the project prefab queue capacity of {maximum}"
            ),
            Self::MissingProjectComponentStore { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without GameComponentStore",
                target.id, target.generation
            ),
            Self::UnknownProjectComponent {
                index,
                component_type,
            } => write!(
                formatter,
                "game command {index} references project component `{component_type}` missing from the active module generation"
            ),
            Self::ProjectComponentAlreadyPresent {
                index,
                target,
                component_type,
            } => write!(
                formatter,
                "game command {index} cannot add existing project component `{component_type}` to entity {} generation {}",
                target.id, target.generation
            ),
            Self::MissingProjectComponent {
                index,
                target,
                component_type,
            } => write!(
                formatter,
                "game command {index} cannot mutate absent project component `{component_type}` on entity {} generation {}",
                target.id, target.generation
            ),
            Self::HitboxAlreadyPresent { index, target } => write!(
                formatter,
                "game command {index} cannot create a second hitbox on entity {} generation {}",
                target.id, target.generation
            ),
            Self::HitboxCollisionConflict { index, target } => write!(
                formatter,
                "game command {index} cannot create a hitbox on entity {} generation {} because it has collision components not owned by a hitbox",
                target.id, target.generation
            ),
            Self::MissingHitbox { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} without AttackHitbox",
                target.id, target.generation
            ),
            Self::MissingAssetManifest { index } => {
                write!(formatter, "game command {index} requires the AssetManifest resource")
            }
            Self::UnknownAudioAsset { index, asset_id } => write!(
                formatter,
                "game command {index} references unregistered audio asset `{asset_id}`"
            ),
            Self::CommandAfterDespawn { index, target } => write!(
                formatter,
                "game command {index} targets entity {} generation {} after despawn",
                target.id, target.generation
            ),
            Self::InvalidPayload { index, message } => {
                write!(
                    formatter,
                    "game command {index} payload is invalid: {message}"
                )
            }
        }
    }
}

impl std::error::Error for GameCommandError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::{AnimationClip, Animator};
    use crate::asset::Assets;
    use engine_authoring::id::AssetId;
    use engine_authoring::ui::UiDocument;

    fn handle(entity: Entity) -> GameEntityHandle {
        GameEntityHandle {
            id: entity.id(),
            generation: entity.generation(),
        }
    }

    #[test]
    fn transform_commands_preflight_then_apply_in_order() {
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        let commands = vec![
            GameCommand::translate(handle(entity), [1.0, 2.0, 3.0]),
            GameCommand::rotate(handle(entity), [0.0, 0.0, 0.0, 1.0]),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        assert_eq!(
            world
                .get_component::<Transform>(entity)
                .unwrap()
                .translation,
            Vec3::ZERO
        );
        apply_prepared_game_commands(&mut world, prepared);

        assert_eq!(
            world
                .get_component::<Transform>(entity)
                .unwrap()
                .translation,
            Vec3::new(1.0, 2.0, 3.0)
        );
    }

    #[test]
    fn save_commands_mutate_active_data_and_queue_bounded_slot_io() {
        let mut world = World::new();
        world.insert_resource(SaveData::new());
        world.insert_resource(GameSaveCommandQueue::default());
        let commands = vec![
            GameCommand::set_save_number("mission.score", 120.0),
            GameCommand::write_save_slot(1),
            GameCommand::set_save_flag("mission.cleared", true),
            GameCommand::remove_save_value("mission.score"),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        apply_prepared_game_commands(&mut world, prepared);

        let save = world.get_resource::<SaveData>().unwrap();
        assert_eq!(save.get("mission.cleared"), Some(&SaveValue::Flag(true)));
        assert!(save.get("mission.score").is_none());
        assert_eq!(
            world.get_resource::<GameSaveCommandQueue>().unwrap().len(),
            1
        );
    }

    #[test]
    fn full_save_queue_rejects_the_complete_callback_before_mutation() {
        let mut world = World::new();
        let mut save = SaveData::new();
        save.set("mission.phase", SaveValue::Number(1.0));
        world.insert_resource(save);
        let mut queue = GameSaveCommandQueue::default();
        for slot in 0..MAX_GAME_SAVE_COMMANDS {
            queue.push_preflighted(GameSaveCommand::Load {
                slot: u32::try_from(slot).unwrap(),
            });
        }
        world.insert_resource(queue);
        let commands = vec![
            GameCommand::set_save_number("mission.phase", 2.0),
            GameCommand::write_save_slot(0),
        ];

        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::SaveQueueFull {
                index: 1,
                maximum: MAX_GAME_SAVE_COMMANDS,
            })
        ));
        assert_eq!(
            world
                .get_resource::<SaveData>()
                .unwrap()
                .get("mission.phase"),
            Some(&SaveValue::Number(1.0))
        );
    }

    #[test]
    fn timer_commands_apply_in_order_and_query_uses_decimal_request_id() {
        let mut world = World::new();
        world.insert_resource(GameTimers::default());
        world.insert_resource(GameTimerEvents::default());
        let commands = vec![
            GameCommand::set_timer("game.timer.attack", 0.5),
            GameCommand::query_timer("game.timer.attack", u64::MAX),
            GameCommand::cancel_timer("game.timer.attack"),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        apply_prepared_game_commands(&mut world, prepared);

        assert!(world
            .get_resource::<GameTimers>()
            .unwrap()
            .ids()
            .next()
            .is_none());
        let payload = &world
            .get_resource::<GameTimerEvents>()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .payload;
        let Value::Object(fields) = payload else {
            panic!("timer query result must be an object");
        };
        assert_eq!(fields["status"], Value::String("active".to_owned()));
        assert_eq!(fields["request_id"], Value::String(u64::MAX.to_string()));
    }

    #[test]
    fn timer_capacity_preflight_accounts_for_cancellation_order() {
        let mut world = World::new();
        let mut timers = GameTimers::default();
        for index in 0..MAX_GAME_TIMERS {
            timers.set_preflighted(format!("game.timer.slot_{index}"), 1.0);
        }
        world.insert_resource(timers);
        world.insert_resource(GameTimerEvents::default());

        assert!(matches!(
            prepare_game_commands(&world, &[GameCommand::set_timer("game.timer.extra", 1.0)]),
            Err(GameCommandError::TimerCapacityFull {
                index: 0,
                maximum: MAX_GAME_TIMERS,
            })
        ));

        let commands = vec![
            GameCommand::cancel_timer("game.timer.slot_0"),
            GameCommand::set_timer("game.timer.extra", 1.0),
        ];
        let prepared = prepare_game_commands(&world, &commands).unwrap();
        apply_prepared_game_commands(&mut world, prepared);
        let ids = world
            .get_resource::<GameTimers>()
            .unwrap()
            .ids()
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), MAX_GAME_TIMERS);
        assert!(ids.contains("game.timer.extra"));
        assert!(!ids.contains("game.timer.slot_0"));
    }

    #[test]
    fn prefab_spawn_is_validated_before_entering_exclusive_queue() {
        let mut world = World::new();
        world.insert_resource(GamePrefabSpawnQueue::default());
        world.insert_resource(GamePrefabEvents::default());

        assert!(matches!(
            prepare_game_commands(
                &world,
                &[GameCommand::spawn_prefab(
                    "../enemy.prefab.json",
                    [0.0, 0.0, 0.0],
                    1,
                )],
            ),
            Err(GameCommandError::InvalidPayload { .. })
        ));
        assert_eq!(
            world.get_resource::<GamePrefabSpawnQueue>().unwrap().len(),
            0
        );

        let prepared = prepare_game_commands(
            &world,
            &[GameCommand::spawn_prefab(
                "prefabs/enemy.prefab.json",
                [1.0, 0.0, 2.0],
                u64::MAX,
            )],
        )
        .unwrap();
        apply_prepared_game_commands(&mut world, prepared);
        assert_eq!(
            world.get_resource::<GamePrefabSpawnQueue>().unwrap().len(),
            1
        );
    }

    #[test]
    fn project_component_commands_use_defaults_and_virtual_ordering() {
        let mut world = World::new();
        let entity = world.spawn_with(GameComponentStore::default()).unwrap();
        let component_type = ComponentTypeId::new("game.status.stunned");
        let default_value =
            Value::Object(BTreeMap::from([("remaining".to_owned(), Value::F64(0.0))]));
        let mut defaults = GameComponentDefaults::default();
        defaults.insert(component_type.clone(), default_value.clone());
        world.insert_resource(defaults);
        let commands = vec![
            GameCommand::add_game_component(handle(entity), component_type.clone()),
            GameCommand::disable_game_component(handle(entity), component_type.clone()),
            GameCommand::enable_game_component(handle(entity), component_type.clone()),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        apply_prepared_game_commands(&mut world, prepared);

        let store = world.get_component::<GameComponentStore>(entity).unwrap();
        assert_eq!(store.value(&component_type), Some(&default_value));
        assert_eq!(store.is_enabled(&component_type), Some(true));

        let remove = prepare_game_commands(
            &world,
            &[GameCommand::remove_game_component(
                handle(entity),
                component_type.clone(),
            )],
        )
        .unwrap();
        apply_prepared_game_commands(&mut world, remove);
        assert_eq!(
            world
                .get_component::<GameComponentStore>(entity)
                .unwrap()
                .is_enabled(&component_type),
            None
        );
    }

    #[test]
    fn invalid_component_sequence_keeps_prior_add_unapplied() {
        let mut world = World::new();
        let entity = world.spawn_with(GameComponentStore::default()).unwrap();
        let component_type = ComponentTypeId::new("game.status.stunned");
        let mut defaults = GameComponentDefaults::default();
        defaults.insert(component_type.clone(), Value::Object(BTreeMap::new()));
        world.insert_resource(defaults);
        let commands = vec![
            GameCommand::add_game_component(handle(entity), component_type.clone()),
            GameCommand::add_game_component(handle(entity), component_type.clone()),
        ];

        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::ProjectComponentAlreadyPresent { index: 1, .. })
        ));
        assert_eq!(
            world
                .get_component::<GameComponentStore>(entity)
                .unwrap()
                .is_enabled(&component_type),
            None
        );
    }

    #[test]
    fn hitbox_commands_own_collision_components_and_reactivate_in_order() {
        let mut world = World::new();
        let owner = world.spawn_with(Transform::default()).unwrap();
        let carrier = world.spawn_with(Transform::default()).unwrap();
        let commands = vec![
            GameCommand::create_hitbox(
                handle(carrier),
                handle(owner),
                crate::game_io::GameHitboxShape::Sphere { radius: 0.75 },
                2,
                15.0,
                4,
                8,
                true,
            ),
            GameCommand::disable_hitbox(handle(carrier)),
            GameCommand::enable_hitbox(handle(carrier)),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        apply_prepared_game_commands(&mut world, prepared);

        let hitbox = world.get_component::<AttackHitbox>(carrier).unwrap();
        assert_eq!(hitbox.owner, owner);
        assert_eq!(hitbox.team, 2);
        assert_eq!(hitbox.damage, 15.0);
        assert!(hitbox.enabled);
        assert_eq!(hitbox.activation, 2);
        assert!(world.has_component::<Collider>(carrier));
        assert!(world.has_component::<TriggerVolume>(carrier));
        assert_eq!(
            world.get_component::<CollisionLayers>(carrier).unwrap(),
            &CollisionLayers {
                membership: 4,
                mask: 8,
            }
        );

        let remove =
            prepare_game_commands(&world, &[GameCommand::remove_hitbox(handle(carrier))]).unwrap();
        apply_prepared_game_commands(&mut world, remove);
        assert!(!world.has_component::<AttackHitbox>(carrier));
        assert!(!world.has_component::<Collider>(carrier));
        assert!(!world.has_component::<TriggerVolume>(carrier));
    }

    #[test]
    fn hitbox_create_rejects_foreign_collision_components_atomically() {
        let mut world = World::new();
        let owner = world.spawn_with(Transform::default()).unwrap();
        let carrier = world.spawn_with(Transform::default()).unwrap();
        world.add_component(carrier, Collider::sphere(1.0)).unwrap();

        assert!(matches!(
            prepare_game_commands(
                &world,
                &[GameCommand::create_hitbox(
                    handle(carrier),
                    handle(owner),
                    crate::game_io::GameHitboxShape::Sphere { radius: 0.5 },
                    1,
                    1.0,
                    1,
                    u32::MAX,
                    true,
                )],
            ),
            Err(GameCommandError::HitboxCollisionConflict { .. })
        ));
        assert!(world.has_component::<Collider>(carrier));
        assert!(!world.has_component::<AttackHitbox>(carrier));
    }

    #[test]
    fn invalid_later_command_keeps_every_transform_unchanged() {
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        let commands = vec![
            GameCommand::translate(handle(entity), [1.0, 0.0, 0.0]),
            GameCommand {
                family: GameCommandFamily::Transform,
                request_id: None,
                target: Some(handle(entity)),
                payload: Value::Null,
            },
        ];

        assert!(prepare_game_commands(&world, &commands).is_err());
        assert_eq!(
            world
                .get_component::<Transform>(entity)
                .unwrap()
                .translation,
            Vec3::ZERO
        );
    }

    #[test]
    fn command_after_despawn_is_rejected_during_preflight() {
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        let commands = vec![
            GameCommand::despawn(handle(entity)),
            GameCommand::translate(handle(entity), [1.0, 0.0, 0.0]),
        ];

        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::CommandAfterDespawn { index: 1, .. })
        ));
        assert!(world.contains_entity(entity));
    }

    #[test]
    fn character_motion_is_preflighted_and_applied_atomically() {
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        world
            .add_component(entity, KinematicCharacterController::default())
            .unwrap();
        let command =
            GameCommand::set_character_motion(handle(entity), [3.0, -1.0, 2.0], [1.0, 0.0, 0.0]);

        let prepared = prepare_game_commands(&world, &[command]).unwrap();
        assert_eq!(
            world
                .get_component::<KinematicCharacterController>(entity)
                .unwrap()
                .velocity,
            Vec3::ZERO
        );
        apply_prepared_game_commands(&mut world, prepared);

        assert_eq!(
            world
                .get_component::<KinematicCharacterController>(entity)
                .unwrap()
                .velocity,
            Vec3::new(3.0, -1.0, 2.0)
        );
        let facing = world.get_component::<Transform>(entity).unwrap().rotation * Vec3::NEG_Z;
        assert!(facing.abs_diff_eq(Vec3::X, 1e-5));
    }

    #[test]
    fn zero_character_facing_rejects_without_changing_velocity() {
        let mut world = World::new();
        let entity = world.spawn_with(Transform::default()).unwrap();
        world
            .add_component(entity, KinematicCharacterController::default())
            .unwrap();
        let command =
            GameCommand::set_character_motion(handle(entity), [3.0, 0.0, 0.0], [0.0, 0.0, 0.0]);

        assert!(matches!(
            prepare_game_commands(&world, &[command]),
            Err(GameCommandError::InvalidPayload { .. })
        ));
        assert_eq!(
            world
                .get_component::<KinematicCharacterController>(entity)
                .unwrap()
                .velocity,
            Vec3::ZERO
        );
    }

    #[test]
    fn lock_on_operations_require_resource_and_keep_callback_order() {
        let commands = vec![
            GameCommand::acquire_lock_on(),
            GameCommand::cycle_lock_on(),
            GameCommand::release_lock_on(),
        ];
        let world = World::new();
        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::MissingTargetLock { index: 0 })
        ));

        let mut world = World::new();
        world.insert_resource(TargetLock::default());
        let prepared = prepare_game_commands(&world, &commands).unwrap();
        assert!(matches!(
            prepared.as_slice(),
            [
                PreparedGameCommand::LockOn(LockOnOperation::Acquire),
                PreparedGameCommand::LockOn(LockOnOperation::Cycle),
                PreparedGameCommand::LockOn(LockOnOperation::Release)
            ]
        ));
        apply_prepared_game_commands(&mut world, prepared);
    }

    #[test]
    fn animation_play_crossfade_and_stop_use_loaded_clip_handles() {
        let mut world = World::new();
        let mut clips = Assets::new();
        let first_clip = clips.add(AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let second_clip = clips.add(AnimationClip {
            duration: 2.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let second_id = second_clip.id().value();
        world.insert_resource(clips);
        let entity = world.spawn_with(Animator::playing(first_clip)).unwrap();

        let prepared = prepare_game_commands(
            &world,
            &[GameCommand::crossfade_animation(
                handle(entity),
                second_id,
                0.25,
                true,
            )],
        )
        .unwrap();
        assert_eq!(
            world.get_component::<Animator>(entity).unwrap().clip,
            first_clip
        );
        apply_prepared_game_commands(&mut world, prepared);
        let animator = world.get_component::<Animator>(entity).unwrap();
        assert_eq!(animator.clip, second_clip);
        assert!(animator.looping);
        assert!(animator.is_fading());

        let prepared = prepare_game_commands(
            &world,
            &[
                GameCommand::stop_animation(handle(entity)),
                GameCommand::play_animation(handle(entity), false),
            ],
        )
        .unwrap();
        apply_prepared_game_commands(&mut world, prepared);
        let animator = world.get_component::<Animator>(entity).unwrap();
        assert_eq!(animator.state, crate::animation::AnimatorState::Playing);
        assert!(!animator.looping);
        assert_eq!(animator.time, 0.0);
    }

    #[test]
    fn missing_crossfade_clip_rejects_without_changing_animator() {
        let mut world = World::new();
        let mut clips = Assets::new();
        let first_clip = clips.add(AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        world.insert_resource(clips);
        let entity = world.spawn_with(Animator::playing(first_clip)).unwrap();

        assert!(matches!(
            prepare_game_commands(
                &world,
                &[GameCommand::crossfade_animation(
                    handle(entity),
                    u64::MAX,
                    0.2,
                    false,
                )]
            ),
            Err(GameCommandError::MissingAnimationClip {
                clip_id: u64::MAX,
                ..
            })
        ));
        assert_eq!(
            world.get_component::<Animator>(entity).unwrap().clip,
            first_clip
        );
    }

    #[test]
    fn ui_bindings_and_visibility_apply_in_callback_order() {
        let mut world = World::new();
        world.insert_resource(UiBindings::default());
        let document = world
            .spawn_with(UiDocumentRef {
                asset: AssetId::generate(),
                document: UiDocument::default(),
                source_path: None,
                modified: None,
            })
            .unwrap();
        let commands = vec![
            GameCommand::set_ui_text("hud.status", "ready"),
            GameCommand::set_ui_number("hud.hp", 120.0),
            GameCommand::set_ui_flag("hud.boss", true),
            GameCommand::remove_ui_binding("hud.status"),
            GameCommand::set_ui_document_visible(handle(document), false),
            GameCommand::set_ui_document_visible(handle(document), true),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        assert!(world
            .get_component::<UiDocumentVisibility>(document)
            .is_none());
        apply_prepared_game_commands(&mut world, prepared);

        let bindings = world.get_resource::<UiBindings>().unwrap();
        assert_eq!(bindings.get("hud.status"), None);
        assert_eq!(bindings.get("hud.hp"), Some(&UiBindingValue::Number(120.0)));
        assert_eq!(bindings.get("hud.boss"), Some(&UiBindingValue::Flag(true)));
        assert!(
            world
                .get_component::<UiDocumentVisibility>(document)
                .unwrap()
                .visible
        );

        let prepared = prepare_game_commands(
            &world,
            &[GameCommand::set_ui_document_visible(
                handle(document),
                false,
            )],
        )
        .unwrap();
        apply_prepared_game_commands(&mut world, prepared);
        assert!(!crate::ui_document::ui_document_is_visible(
            &world, document
        ));
    }

    #[test]
    fn invalid_ui_value_rejects_all_prior_binding_changes() {
        let mut world = World::new();
        world.insert_resource(UiBindings::default());
        let commands = vec![
            GameCommand::set_ui_text("hud.status", "ready"),
            GameCommand::set_ui_number("hud.hp", f64::NAN),
        ];

        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::InvalidPayload { index: 1, .. })
        ));
        assert!(world
            .get_resource::<UiBindings>()
            .unwrap()
            .get("hud.status")
            .is_none());
    }

    #[test]
    fn scene_request_is_deferred_to_the_existing_frame_boundary_service() {
        let mut world = World::new();
        world.insert_resource(SceneManager::new());
        let command = GameCommand::request_scene("scenes/mission_02.scene.json");

        let prepared = prepare_game_commands(&world, &[command]).unwrap();
        assert_eq!(
            world
                .get_resource::<SceneManager>()
                .unwrap()
                .pending_scene_path(),
            None
        );
        apply_prepared_game_commands(&mut world, prepared);
        assert_eq!(
            world
                .get_resource::<SceneManager>()
                .unwrap()
                .pending_scene_path(),
            Some("scenes/mission_02.scene.json")
        );
    }

    #[test]
    fn unsafe_scene_path_is_rejected_before_queue_mutation() {
        let mut world = World::new();
        world.insert_resource(SceneManager::new());

        assert!(matches!(
            prepare_game_commands(
                &world,
                &[GameCommand::request_scene("../outside.scene.json")]
            ),
            Err(GameCommandError::InvalidPayload { .. })
        ));
        assert!(world
            .get_resource::<SceneManager>()
            .unwrap()
            .pending_scene_path()
            .is_none());
    }

    #[test]
    fn audio_commands_enter_bounded_queue_only_after_complete_preflight() {
        let mut world = World::new();
        world.insert_resource(GameAudioCommandQueue::default());
        world.insert_resource(
            AssetManifest::from_json(
                r#"{"schema_version":2,"assets":{"asset_01JP0000000000000000000601":{"path":"audio/hit.wav"},"asset_01JP0000000000000000000602":{"path":"audio/bgm.ogg"}}}"#,
            )
            .unwrap(),
        );
        let commands = vec![
            GameCommand::play_sound_effect("asset_01JP0000000000000000000601"),
            GameCommand::play_background_music("asset_01JP0000000000000000000602"),
            GameCommand::crossfade_background_music("asset_01JP0000000000000000000602", 0.5),
            GameCommand::set_master_volume(0.8),
            GameCommand::set_background_music_volume(0.7),
            GameCommand::set_sound_effect_volume(0.6),
            GameCommand::stop_background_music(),
        ];

        let prepared = prepare_game_commands(&world, &commands).unwrap();
        assert_eq!(
            world.get_resource::<GameAudioCommandQueue>().unwrap().len(),
            0
        );
        apply_prepared_game_commands(&mut world, prepared);
        assert_eq!(
            world.get_resource::<GameAudioCommandQueue>().unwrap().len(),
            7
        );
    }

    #[test]
    fn unknown_audio_asset_rejects_without_enqueuing_prior_commands() {
        let mut world = World::new();
        world.insert_resource(GameAudioCommandQueue::default());
        world.insert_resource(AssetManifest::default());
        let commands = vec![
            GameCommand::set_master_volume(0.5),
            GameCommand::play_sound_effect("asset_01JP0000000000000000000699"),
        ];

        assert!(matches!(
            prepare_game_commands(&world, &commands),
            Err(GameCommandError::UnknownAudioAsset { index: 1, .. })
        ));
        assert_eq!(
            world.get_resource::<GameAudioCommandQueue>().unwrap().len(),
            0
        );
    }
}
