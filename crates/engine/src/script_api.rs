//! Command-oriented Rhai Script API v2 integration (Phase 60 / ADR 0049).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use engine_authoring::{AssetId, StableId};
use engine_ecs::{Commands, Entity, Query, Res, ResMut, World};
use glam::Vec3;

use crate::anim_graph::AnimGraphPlayer;
use crate::animation::Animator;
use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::audio::{AudioAsset, AudioSystem};
use crate::game_prefab::spawn_prefab_at;
use crate::lock_on::TargetLock;
use crate::scene_manager::SceneManager;
use crate::scripting::{ScriptComponent, ScriptEngine};
use crate::time::FixedTime;
use crate::ui_document::{UiBindingValue, UiBindings};

/// Maximum number of pending script commands or events.
pub const MAX_SCRIPT_COMMANDS: usize = 256;

/// Identifies a requested lock-on operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptLockCommand {
    /// Acquire the nearest valid target.
    Acquire,
    /// Cycle to the next valid target.
    Cycle,
    /// Release the current target.
    Release,
}

/// A mutation requested through the sandboxed Rhai context.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptApiCommand {
    /// Request a runtime scene switch.
    RequestScene {
        /// Project-relative scene path.
        path: String,
    },
    /// Set one UI binding.
    SetUiBinding {
        /// Binding name.
        name: String,
        /// New binding value.
        value: UiBindingValue,
    },
    /// Remove one UI binding.
    RemoveUiBinding {
        /// Binding name.
        name: String,
    },
    /// Request lock-on control.
    LockTarget {
        /// Requested lock-on operation.
        command: ScriptLockCommand,
    },
    /// Play an animation graph clip.
    PlayAnimation {
        /// Runtime target string.
        target: String,
        /// Clip ID registered on the target graph player.
        clip_id: String,
        /// Crossfade duration in seconds.
        fade_seconds: f32,
    },
    /// Set one animation graph condition.
    SetAnimationCondition {
        /// Runtime target string.
        target: String,
        /// Condition name.
        condition: String,
        /// New flag value.
        value: bool,
    },
    /// Play a sound effect by authoring asset ID.
    PlaySoundEffect {
        /// Stable authoring asset ID.
        asset_id: String,
    },
    /// Replace the active background music.
    PlayBackgroundMusic {
        /// Stable authoring asset ID.
        asset_id: String,
    },
    /// Crossfade to background music by authoring asset ID.
    CrossfadeBackgroundMusic {
        /// Stable authoring asset ID.
        asset_id: String,
        /// Crossfade duration in seconds.
        fade_seconds: f32,
    },
    /// Set the BGM bus volume.
    SetBackgroundMusicVolume {
        /// New volume before master-volume multiplication.
        volume: f32,
    },
    /// Set the sound-effect bus volume.
    SetSoundEffectVolume {
        /// New volume before master-volume multiplication.
        volume: f32,
    },
    /// Stop background music.
    StopBackgroundMusic,
    /// Despawn a runtime entity.
    Despawn {
        /// Runtime target string.
        target: String,
    },
    /// Spawn a prefab at a world position.
    SpawnPrefab {
        /// Asset-root-relative `.prefab.json` path.
        path: String,
        /// World-space root translation.
        position: Vec3,
    },
    /// Subscribe the issuer to an event name.
    Subscribe {
        /// Event name.
        event: String,
    },
    /// Remove an event subscription.
    Unsubscribe {
        /// Event name.
        event: String,
    },
    /// Broadcast an event to subscribers.
    Emit {
        /// Event name.
        event: String,
    },
    /// Send an event to one runtime entity.
    SendEvent {
        /// Runtime target string.
        target: String,
        /// Event name.
        event: String,
    },
    /// Start or reset a one-shot timer.
    SetTimer {
        /// Timer name scoped to the issuer.
        name: String,
        /// Duration in seconds.
        seconds: f32,
    },
    /// Remove a timer.
    CancelTimer {
        /// Timer name scoped to the issuer.
        name: String,
    },
    /// Consume a completed timer.
    ConsumeTimer {
        /// Timer name scoped to the issuer.
        name: String,
    },
}

/// One command together with the entity that requested it.
#[derive(Debug, Clone)]
pub struct QueuedScriptCommand {
    /// Entity whose script produced the command.
    pub issuer: Entity,
    /// Requested operation.
    pub command: ScriptApiCommand,
}

/// Bounded FIFO of Script API commands.
#[derive(Debug, Default)]
pub struct ScriptCommandQueue {
    commands: VecDeque<QueuedScriptCommand>,
}

impl ScriptCommandQueue {
    /// Adds a command, dropping and logging it when the queue is full.
    pub fn push(&mut self, command: QueuedScriptCommand) {
        if self.commands.len() >= MAX_SCRIPT_COMMANDS {
            log::warn!("ScriptCommandQueue is full ({MAX_SCRIPT_COMMANDS}); dropping command");
            return;
        }
        self.commands.push_back(command);
    }

    /// Returns the number of pending commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Returns `true` when no commands are pending.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn drain(&mut self) -> impl Iterator<Item = QueuedScriptCommand> + '_ {
        self.commands.drain(..)
    }
}

/// Runtime identity copied from an authoring entity during scene conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEntityIdentity {
    /// Stable authoring ID for the current play/build session.
    pub authoring_id: engine_authoring::EntityId,
    /// Searchable authoring entity name.
    pub name: String,
}

#[derive(Debug, Clone, Copy)]
struct ScriptTimer {
    remaining: f32,
    finished: bool,
}

/// Timer state keyed by runtime entity and script-visible name.
#[derive(Debug, Default)]
pub struct ScriptTimers {
    timers: BTreeMap<(Entity, String), ScriptTimer>,
}

impl ScriptTimers {
    /// Advances active timers without allowing negative remaining values.
    pub fn advance(&mut self, delta: f32) {
        for timer in self.timers.values_mut() {
            if timer.finished {
                continue;
            }
            timer.remaining = (timer.remaining - delta.max(0.0)).max(0.0);
            timer.finished = timer.remaining <= f32::EPSILON;
        }
    }

    /// Starts or resets a timer.
    pub fn set(&mut self, entity: Entity, name: String, seconds: f32) {
        let seconds = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        self.timers.insert(
            (entity, name),
            ScriptTimer {
                remaining: seconds,
                finished: seconds <= f32::EPSILON,
            },
        );
    }

    /// Returns completed timer names for an entity.
    pub fn finished_for(&self, entity: Entity) -> BTreeSet<String> {
        self.timers
            .iter()
            .filter(|((owner, _), timer)| *owner == entity && timer.finished)
            .map(|((_, name), _)| name.clone())
            .collect()
    }

    /// Removes one timer.
    pub fn remove(&mut self, entity: Entity, name: &str) {
        self.timers.remove(&(entity, name.to_string()));
    }

    /// Removes every timer owned by an entity.
    pub fn remove_entity(&mut self, entity: Entity) {
        self.timers.retain(|(owner, _), _| *owner != entity);
    }
}

/// One deferred script event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEvent {
    /// Explicit target, or `None` for a subscription broadcast.
    pub target: Option<Entity>,
    /// Event name delivered to `on_event`.
    pub name: String,
}

/// Event subscriptions and next-pass event delivery.
#[derive(Debug, Default)]
pub struct ScriptEventBus {
    subscriptions: BTreeMap<String, BTreeSet<Entity>>,
    pending: VecDeque<ScriptEvent>,
}

impl ScriptEventBus {
    /// Subscribes an entity to an event.
    pub fn subscribe(&mut self, entity: Entity, event: String) {
        self.subscriptions.entry(event).or_default().insert(entity);
    }

    /// Removes an entity's subscription.
    pub fn unsubscribe(&mut self, entity: Entity, event: &str) {
        if let Some(subscribers) = self.subscriptions.get_mut(event) {
            subscribers.remove(&entity);
            if subscribers.is_empty() {
                self.subscriptions.remove(event);
            }
        }
    }

    /// Removes an entity from every subscription set.
    pub fn remove_entity(&mut self, entity: Entity) {
        self.subscriptions.retain(|_, subscribers| {
            subscribers.remove(&entity);
            !subscribers.is_empty()
        });
    }

    /// Queues a broadcast for the next scripting pass.
    pub fn emit(&mut self, event: String) {
        self.push_event(ScriptEvent {
            target: None,
            name: event,
        });
    }

    /// Queues a targeted event for the next scripting pass.
    pub fn send(&mut self, target: Entity, event: String) {
        self.push_event(ScriptEvent {
            target: Some(target),
            name: event,
        });
    }

    /// Removes pending events and expands broadcasts into deterministic targets.
    pub fn take_deliveries(&mut self) -> Vec<(Entity, String)> {
        let mut deliveries = Vec::new();
        for event in self.pending.drain(..) {
            if let Some(target) = event.target {
                deliveries.push((target, event.name));
            } else if let Some(subscribers) = self.subscriptions.get(&event.name) {
                deliveries.extend(
                    subscribers
                        .iter()
                        .copied()
                        .map(|entity| (entity, event.name.clone())),
                );
            }
        }
        deliveries
    }

    fn push_event(&mut self, event: ScriptEvent) {
        if self.pending.len() >= MAX_SCRIPT_COMMANDS {
            log::warn!("ScriptEventBus is full ({MAX_SCRIPT_COMMANDS}); dropping event");
            return;
        }
        self.pending.push_back(event);
    }
}

#[derive(Debug, Clone)]
struct SpawnPrefabRequest {
    path: String,
    position: Vec3,
}

/// Frame-boundary commands that require exclusive world access.
#[derive(Debug, Default)]
pub struct ScriptWorldCommandQueue {
    prefab_spawns: VecDeque<SpawnPrefabRequest>,
}

#[derive(Debug, Default)]
pub(crate) struct PendingScriptEffects {
    core: VecDeque<QueuedScriptCommand>,
    animation: VecDeque<QueuedScriptCommand>,
    audio: VecDeque<QueuedScriptCommand>,
}

/// Advances timers and snapshots entity lookup and deferred events for scripts.
pub(crate) fn script_api_snapshot_system(
    time: Res<FixedTime>,
    mut timers: ResMut<ScriptTimers>,
    mut event_bus: ResMut<ScriptEventBus>,
    mut script_engine: Option<ResMut<ScriptEngine>>,
    identities: Query<&RuntimeEntityIdentity>,
    scripts: Query<&ScriptComponent>,
) {
    timers.advance(time.fixed_delta);
    let Some(engine) = script_engine.as_deref_mut() else {
        return;
    };
    engine.set_entity_snapshot(build_entity_snapshot(&identities));
    let timer_snapshot = scripts
        .iter()
        .map(|(entity, _)| (entity, std::sync::Arc::new(timers.finished_for(entity))))
        .collect();
    engine.set_timer_snapshot(timer_snapshot);
    engine.set_event_deliveries(event_bus.take_deliveries());
}

/// Moves commands produced by hook calls into the bounded world queue.
pub(crate) fn script_api_collect_system(
    mut script_engine: Option<ResMut<ScriptEngine>>,
    mut queue: ResMut<ScriptCommandQueue>,
) {
    let Some(engine) = script_engine.as_deref_mut() else {
        return;
    };
    for command in engine.take_api_commands() {
        queue.push(command);
    }
}

/// Partitions the global FIFO into domain effects while applying timers and events.
pub(crate) fn script_api_command_dispatch_system(
    mut queue: ResMut<ScriptCommandQueue>,
    mut world_queue: ResMut<ScriptWorldCommandQueue>,
    mut timers: ResMut<ScriptTimers>,
    mut event_bus: ResMut<ScriptEventBus>,
    mut effects: ResMut<PendingScriptEffects>,
) {
    for queued in queue.drain() {
        match &queued.command {
            ScriptApiCommand::SetTimer { name, seconds } => {
                timers.set(queued.issuer, name.clone(), *seconds);
            }
            ScriptApiCommand::CancelTimer { name } | ScriptApiCommand::ConsumeTimer { name } => {
                timers.remove(queued.issuer, name);
            }
            ScriptApiCommand::Subscribe { event } => {
                event_bus.subscribe(queued.issuer, event.clone());
            }
            ScriptApiCommand::Unsubscribe { event } => {
                event_bus.unsubscribe(queued.issuer, event);
            }
            ScriptApiCommand::Emit { event } => event_bus.emit(event.clone()),
            ScriptApiCommand::SpawnPrefab { path, position } => {
                if world_queue.prefab_spawns.len() >= MAX_SCRIPT_COMMANDS {
                    log::warn!(
                        "ScriptWorldCommandQueue is full ({MAX_SCRIPT_COMMANDS}); dropping prefab spawn"
                    );
                } else {
                    world_queue.prefab_spawns.push_back(SpawnPrefabRequest {
                        path: path.clone(),
                        position: *position,
                    });
                }
            }
            ScriptApiCommand::PlayAnimation { .. }
            | ScriptApiCommand::SetAnimationCondition { .. } => {
                effects.animation.push_back(queued);
            }
            ScriptApiCommand::PlaySoundEffect { .. }
            | ScriptApiCommand::PlayBackgroundMusic { .. }
            | ScriptApiCommand::CrossfadeBackgroundMusic { .. }
            | ScriptApiCommand::SetBackgroundMusicVolume { .. }
            | ScriptApiCommand::SetSoundEffectVolume { .. }
            | ScriptApiCommand::StopBackgroundMusic => effects.audio.push_back(queued),
            _ => effects.core.push_back(queued),
        }
    }
}

/// Applies scene, UI, lock-on, despawn, and event effects.
// Each ECS parameter declares an independently validated resource or component
// access; grouping them would hide scheduler conflicts behind an opaque type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn script_core_effect_system(
    mut effects: ResMut<PendingScriptEffects>,
    mut scene_manager: ResMut<SceneManager>,
    mut bindings: ResMut<UiBindings>,
    mut target_lock: ResMut<TargetLock>,
    mut event_bus: ResMut<ScriptEventBus>,
    mut timers: ResMut<ScriptTimers>,
    mut commands: Commands,
    identities: Query<&RuntimeEntityIdentity>,
) {
    for queued in effects.core.drain(..) {
        match queued.command {
            ScriptApiCommand::RequestScene { path } => scene_manager.request_switch(path),
            ScriptApiCommand::SetUiBinding { name, value } => bindings.set(name, value),
            ScriptApiCommand::RemoveUiBinding { name } => {
                bindings.remove(&name);
            }
            ScriptApiCommand::LockTarget { command } => match command {
                ScriptLockCommand::Acquire => target_lock.request_acquire(),
                ScriptLockCommand::Cycle => target_lock.request_cycle(),
                ScriptLockCommand::Release => target_lock.request_release(),
            },
            ScriptApiCommand::Despawn { target } => {
                if let Some(entity) = resolve_target(&target, queued.issuer, &identities) {
                    event_bus.remove_entity(entity);
                    timers.remove_entity(entity);
                    if let Err(error) = commands.try_despawn(entity) {
                        log::error!("script despawn failed for {target}: {error}");
                    }
                } else {
                    log::error!("script despawn target `{target}` was not found");
                }
            }
            ScriptApiCommand::SendEvent { target, event } => {
                if let Some(entity) = resolve_target(&target, queued.issuer, &identities) {
                    event_bus.send(entity, event);
                } else {
                    log::error!("script event target `{target}` was not found");
                }
            }
            _ => {}
        }
    }
}

/// Applies animation clip and condition effects.
pub(crate) fn script_animation_effect_system(
    mut effects: ResMut<PendingScriptEffects>,
    mut animation: Query<(&mut Animator, &mut AnimGraphPlayer)>,
) {
    for queued in effects.animation.drain(..) {
        for (entity, (animator, player)) in animation.iter_mut() {
            let target = match &queued.command {
                ScriptApiCommand::PlayAnimation { target, .. }
                | ScriptApiCommand::SetAnimationCondition { target, .. } => target,
                _ => continue,
            };
            if target != &entity.to_string()
                && !(entity == queued.issuer && target == &queued.issuer.to_string())
            {
                continue;
            }
            match &queued.command {
                ScriptApiCommand::PlayAnimation {
                    clip_id,
                    fade_seconds,
                    ..
                } => {
                    if !player.play_clip(animator, clip_id, *fade_seconds) {
                        log::error!("animation clip `{clip_id}` is not registered on {entity}");
                    }
                }
                ScriptApiCommand::SetAnimationCondition {
                    condition, value, ..
                } => {
                    if let Err(error) = player.set_bool_parameter(condition.clone(), *value) {
                        log::error!(
                            "script animation parameter `{condition}` could not be set: {error}"
                        );
                    }
                }
                _ => {}
            }
            break;
        }
    }
}

/// Applies audio playback effects through existing asset resources.
pub(crate) fn script_audio_effect_system(
    mut effects: ResMut<PendingScriptEffects>,
    mut audio_system: Option<ResMut<AudioSystem>>,
    mut asset_server: Option<ResMut<AssetServer>>,
    manifest: Option<Res<AssetManifest>>,
    mut audio_assets: Option<ResMut<Assets<AudioAsset>>>,
) {
    for queued in effects.audio.drain(..) {
        if matches!(queued.command, ScriptApiCommand::StopBackgroundMusic) {
            if let Some(audio) = audio_system.as_deref_mut()
                && let Err(error) = audio.stop_bgm() {
                    log::error!("script stop_bgm failed: {error}");
                }
            continue;
        }

        let volume_result = match &queued.command {
            ScriptApiCommand::SetBackgroundMusicVolume { volume } => audio_system
                .as_deref_mut()
                .map(|audio| audio.set_bgm_volume(*volume)),
            ScriptApiCommand::SetSoundEffectVolume { volume } => audio_system
                .as_deref_mut()
                .map(|audio| audio.set_se_volume(*volume)),
            _ => None,
        };
        if let Some(result) = volume_result {
            if let Err(error) = result {
                log::error!("script audio bus volume failed: {error}");
            }
            continue;
        }

        let asset_id_text = match &queued.command {
            ScriptApiCommand::PlaySoundEffect { asset_id }
            | ScriptApiCommand::PlayBackgroundMusic { asset_id }
            | ScriptApiCommand::CrossfadeBackgroundMusic { asset_id, .. } => asset_id,
            _ => continue,
        };
        let Ok(asset_id) = AssetId::from_stable_id(StableId::new(asset_id_text)) else {
            log::error!("script audio asset ID `{asset_id_text}` is invalid");
            continue;
        };
        let Some(entry) = manifest
            .as_deref()
            .and_then(|manifest| manifest.get(&asset_id))
        else {
            log::error!("script audio asset `{asset_id}` is not registered in the manifest");
            continue;
        };
        let (Some(server), Some(assets), Some(audio)) = (
            asset_server.as_deref_mut(),
            audio_assets.as_deref_mut(),
            audio_system.as_deref_mut(),
        ) else {
            log::error!(
                "script audio command requires AssetServer, Assets<AudioAsset>, and AudioSystem"
            );
            continue;
        };
        let handle = match server.load_audio(asset_id, &entry.path, assets) {
            Ok(handle) => handle,
            Err(error) => {
                log::error!("script audio load failed: {error}");
                continue;
            }
        };
        let Some(asset) = assets.get(&handle) else {
            log::error!("loaded script audio handle did not resolve");
            continue;
        };
        let result = match queued.command {
            ScriptApiCommand::PlaySoundEffect { .. } => audio.play_se(asset),
            ScriptApiCommand::PlayBackgroundMusic { .. } => audio.play_bgm(asset),
            ScriptApiCommand::CrossfadeBackgroundMusic { fade_seconds, .. } => {
                audio.crossfade_bgm(asset, fade_seconds)
            }
            _ => continue,
        };
        if let Err(error) = result {
            log::error!("script audio playback failed: {error}");
        }
    }
}

/// Processes prefab spawn commands at the exclusive frame boundary.
pub fn process_script_world_commands(world: &mut World) {
    let Some(mut queue) = world.remove_resource::<ScriptWorldCommandQueue>() else {
        return;
    };
    while let Some(request) = queue.prefab_spawns.pop_front() {
        if let Err(error) = spawn_prefab_request(world, &request) {
            log::error!("script prefab spawn `{}` failed: {error}", request.path);
        }
    }
    world.insert_resource(queue);
}

fn spawn_prefab_request(world: &mut World, request: &SpawnPrefabRequest) -> Result<(), String> {
    spawn_prefab_at(world, &request.path, request.position)?;
    Ok(())
}

fn resolve_target(
    target: &str,
    issuer: Entity,
    identities: &Query<&RuntimeEntityIdentity>,
) -> Option<Entity> {
    if target == issuer.to_string() {
        return Some(issuer);
    }
    identities
        .iter()
        .find_map(|(entity, _)| (entity.to_string() == target).then_some(entity))
}

/// Builds a deterministic name-to-runtime-entity snapshot for Rhai lookup.
pub fn build_entity_snapshot(
    identities: &Query<&RuntimeEntityIdentity>,
) -> BTreeMap<String, Vec<String>> {
    let mut snapshot: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (entity, identity) in identities.iter() {
        snapshot
            .entry(identity.name.clone())
            .or_default()
            .push(entity.to_string());
    }
    for entities in snapshot.values_mut() {
        entities.sort();
    }
    snapshot
}

/// Converts a script-compatible value into a UI binding.
pub fn dynamic_to_ui_binding(value: &rhai::Dynamic) -> Option<UiBindingValue> {
    if let Ok(text) = value.clone().into_string() {
        return Some(UiBindingValue::Text(text));
    }
    if value.is::<bool>() {
        return Some(UiBindingValue::Flag(value.clone_cast::<bool>()));
    }
    if value.is::<rhai::INT>() {
        return Some(UiBindingValue::Number(
            value.clone_cast::<rhai::INT>() as f64
        ));
    }
    if value.is::<rhai::FLOAT>() {
        return Some(UiBindingValue::Number(value.clone_cast::<rhai::FLOAT>()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::Transform;
    use engine_authoring::{
        AnimState, AuthoringEntity, CompiledAnimGraph, EntityId, NodeId, PrefabAsset,
    };

    #[test]
    fn command_queue_drops_entries_past_the_cap() {
        let mut queue = ScriptCommandQueue::default();
        let mut world = World::new();
        let entity = world.spawn().expect("entity must spawn");
        for index in 0..(MAX_SCRIPT_COMMANDS + 1) {
            queue.push(QueuedScriptCommand {
                issuer: entity,
                command: ScriptApiCommand::RequestScene {
                    path: format!("scene_{index}"),
                },
            });
        }
        assert_eq!(queue.len(), MAX_SCRIPT_COMMANDS);
    }

    #[test]
    fn completed_timer_is_reported_until_removed() {
        let mut world = World::new();
        let entity = world.spawn().expect("entity must spawn");
        let mut timers = ScriptTimers::default();
        timers.set(entity, "attack".to_string(), 0.5);
        timers.advance(0.25);
        assert!(timers.finished_for(entity).is_empty());
        timers.advance(0.25);
        assert!(timers.finished_for(entity).contains("attack"));
        timers.remove(entity, "attack");
        assert!(timers.finished_for(entity).is_empty());
    }

    #[test]
    fn broadcast_events_expand_to_subscribers_in_entity_order() {
        let mut world = World::new();
        let first = world.spawn().expect("first entity must spawn");
        let second = world.spawn().expect("second entity must spawn");
        let mut bus = ScriptEventBus::default();
        bus.subscribe(second, "alert".to_string());
        bus.subscribe(first, "alert".to_string());
        bus.emit("alert".to_string());
        assert_eq!(
            bus.take_deliveries(),
            vec![(first, "alert".to_string()), (second, "alert".to_string())]
        );
    }

    #[test]
    fn dynamic_ui_binding_accepts_supported_scalar_types() {
        assert_eq!(
            dynamic_to_ui_binding(&rhai::Dynamic::from("player")),
            Some(UiBindingValue::Text("player".to_string()))
        );
        assert_eq!(
            dynamic_to_ui_binding(&rhai::Dynamic::from(true)),
            Some(UiBindingValue::Flag(true))
        );
        assert_eq!(
            dynamic_to_ui_binding(&rhai::Dynamic::from_int(7)),
            Some(UiBindingValue::Number(7.0))
        );
    }

    #[test]
    fn core_effect_applies_ui_binding_and_despawns_issuer() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(PendingScriptEffects::default());
        app.insert_resource(SceneManager::new());
        app.insert_resource(UiBindings::default());
        app.insert_resource(TargetLock::default());
        app.insert_resource(ScriptEventBus::default());
        app.insert_resource(ScriptTimers::default());
        let entity = app
            .world_mut()
            .spawn_with(RuntimeEntityIdentity {
                authoring_id: EntityId::generate(),
                name: "player".to_string(),
            })
            .expect("entity must spawn");
        {
            let effects = app
                .world_mut()
                .get_resource_mut::<PendingScriptEffects>()
                .expect("effects must exist");
            effects.core.push_back(QueuedScriptCommand {
                issuer: entity,
                command: ScriptApiCommand::SetUiBinding {
                    name: "score".to_string(),
                    value: UiBindingValue::Number(42.0),
                },
            });
            effects.core.push_back(QueuedScriptCommand {
                issuer: entity,
                command: ScriptApiCommand::Despawn {
                    target: entity.to_string(),
                },
            });
        }
        app.add_system(script_core_effect_system);

        app.update().expect("core effects must run");

        assert_eq!(
            app.world()
                .get_resource::<UiBindings>()
                .and_then(|bindings| bindings.get("score")),
            Some(&UiBindingValue::Number(42.0))
        );
        assert!(!app.world().contains_entity(entity));
    }

    #[test]
    fn prefab_world_command_spawns_and_positions_runtime_root() {
        let dir = tempfile::tempdir().expect("temp directory must be created");
        let root_id = EntityId::generate();
        let root_entity = AuthoringEntity::new(root_id.clone(), "spawned_enemy");
        let prefab = PrefabAsset {
            schema_version: engine_authoring::PREFAB_SCHEMA_VERSION,
            root: root_id.clone(),
            entities: BTreeMap::from([(root_id, root_entity)]),
        };
        std::fs::write(
            dir.path().join("enemy.prefab.json"),
            prefab.to_json().expect("prefab must serialize"),
        )
        .expect("prefab fixture must write");

        let mut world = World::new();
        world.insert_resource(AssetServer::new(dir.path()));
        world.insert_resource(AssetManifest::default());
        world.insert_resource(Assets::<crate::mesh::Mesh>::default());
        world.insert_resource(Assets::<crate::material::Material>::default());
        let mut queue = ScriptWorldCommandQueue::default();
        queue.prefab_spawns.push_back(SpawnPrefabRequest {
            path: "enemy.prefab.json".to_string(),
            position: Vec3::new(1.0, 2.0, 3.0),
        });
        world.insert_resource(queue);

        process_script_world_commands(&mut world);

        let spawned = world
            .query::<(&RuntimeEntityIdentity, &Transform)>()
            .expect("identity query must build")
            .iter()
            .find_map(|(entity, (identity, transform))| {
                (identity.name == "spawned_enemy").then_some((entity, transform.translation))
            })
            .expect("prefab root must spawn");
        assert_eq!(spawned.1, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn subscribed_event_is_delivered_on_the_next_scripting_pass() {
        let subscriber_script = AssetId::generate();
        let emitter_script = AssetId::generate();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &subscriber_script,
                r#"fn on_start(ctx) { ctx.subscribe("alert"); }
                   fn on_event(ctx, event) {
                       if event == "alert" { ctx.ui_set("event_received", true); }
                   }"#,
            )
            .expect("subscriber script must compile");
        script_engine
            .compile(
                &emitter_script,
                r#"fn on_start(ctx) { ctx.emit("alert"); }"#,
            )
            .expect("emitter script must compile");

        let mut app = crate::App::new();
        app.insert_resource(script_engine);
        let subscriber = app
            .world_mut()
            .spawn_with(ScriptComponent::new(subscriber_script))
            .expect("subscriber must spawn");
        app.world_mut()
            .add_component(
                subscriber,
                RuntimeEntityIdentity {
                    authoring_id: EntityId::generate(),
                    name: "subscriber".to_string(),
                },
            )
            .expect("subscriber identity must attach");
        let emitter = app
            .world_mut()
            .spawn_with(ScriptComponent::new(emitter_script))
            .expect("emitter must spawn");
        app.world_mut()
            .add_component(
                emitter,
                RuntimeEntityIdentity {
                    authoring_id: EntityId::generate(),
                    name: "emitter".to_string(),
                },
            )
            .expect("emitter identity must attach");
        app.add_fixed_system(crate::scripting_update_system);

        app.ecs_mut()
            .run_fixed_update()
            .expect("first scripting pass must run");
        app.ecs_mut()
            .update()
            .expect("commands must enter the event bus");
        assert!(
            app.world()
                .get_resource::<UiBindings>()
                .and_then(|bindings| bindings.get("event_received"))
                .is_none(),
            "emitted events must not reenter scripts in the same pass"
        );

        app.ecs_mut()
            .run_fixed_update()
            .expect("second scripting pass must deliver the event");
        app.ecs_mut()
            .update()
            .expect("event result commands must apply");

        assert_eq!(
            app.world()
                .get_resource::<UiBindings>()
                .and_then(|bindings| bindings.get("event_received")),
            Some(&UiBindingValue::Flag(true))
        );
    }

    #[test]
    fn animation_effect_plays_registered_clip_and_sets_condition() {
        let mut clips = Assets::<crate::AnimationClip>::default();
        let idle = clips.add(crate::AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let attack = clips.add(crate::AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let idle_slot = engine_authoring::id::MotionSlotId::generate();
        let attack_slot = engine_authoring::id::MotionSlotId::generate();
        let graph = CompiledAnimGraph {
            states: vec![AnimState {
                node_id: NodeId::generate(),
                motion_slot: Some(idle_slot.clone()),
                playback_mode: engine_authoring::AnimationStatePlaybackMode::Loop,
            }],
            transitions: Vec::new(),
            entry_state: 0,
            compile_warnings: Vec::new(),
        };
        let mut clip_map = BTreeMap::new();
        clip_map.insert(idle_slot.as_str().to_owned(), idle);
        clip_map.insert(attack_slot.as_str().to_owned(), attack);

        let mut app = engine_ecs::App::new();
        app.insert_resource(PendingScriptEffects::default());
        let entity = app
            .world_mut()
            .spawn_with(Animator::playing(idle))
            .expect("animator must spawn");
        app.world_mut()
            .add_component(entity, AnimGraphPlayer::new(graph, clip_map))
            .expect("graph player must attach");
        {
            let effects = app
                .world_mut()
                .get_resource_mut::<PendingScriptEffects>()
                .expect("effects must exist");
            effects.animation.push_back(QueuedScriptCommand {
                issuer: entity,
                command: ScriptApiCommand::PlayAnimation {
                    target: entity.to_string(),
                    clip_id: attack_slot.as_str().to_owned(),
                    fade_seconds: 0.25,
                },
            });
            effects.animation.push_back(QueuedScriptCommand {
                issuer: entity,
                command: ScriptApiCommand::SetAnimationCondition {
                    target: entity.to_string(),
                    condition: "attacking".to_string(),
                    value: true,
                },
            });
        }
        app.add_system(script_animation_effect_system);

        app.update().expect("animation effects must run");

        let animator = app
            .world()
            .get_component::<Animator>(entity)
            .expect("animator must remain present");
        let player = app
            .world()
            .get_component::<AnimGraphPlayer>(entity)
            .expect("graph player must remain present");
        assert_eq!(animator.clip, attack);
        assert!(animator.is_fading());
        assert_eq!(
            player.parameter_value("attacking"),
            Some(crate::AnimationParameterValue::Bool(true))
        );
    }

    #[test]
    fn audio_effect_without_runtime_resources_is_a_non_panicking_no_op() {
        let mut app = engine_ecs::App::new();
        let mut effects = PendingScriptEffects::default();
        let issuer = app.world_mut().spawn().expect("issuer must spawn");
        effects.audio.push_back(QueuedScriptCommand {
            issuer,
            command: ScriptApiCommand::PlaySoundEffect {
                asset_id: "asset_01JP0000000000000000000001".to_string(),
            },
        });
        app.insert_resource(effects);
        app.add_system(script_audio_effect_system);

        app.update()
            .expect("missing optional audio resources must not fail the schedule");

        assert!(app
            .world()
            .get_resource::<PendingScriptEffects>()
            .expect("effects must remain present")
            .audio
            .is_empty());
    }
}
