//! Deferred prefab spawning for project Rust gameplay callbacks.

use crate::asset::AssetServer;
use crate::game_io::GameEntityHandle;
use crate::game_service_events::{GameSourceEvent, GameSourceEventLog};
use crate::scene_bridge::spawn_from_authoring_scene;
use crate::transform::Transform;
use engine_authoring::{AuthoringScene, PrefabAsset, Transaction, Value};
use engine_ecs::{Entity, World};
use glam::Vec3;
use std::collections::{BTreeMap, VecDeque};

/// Maximum pending project Rust prefab requests.
pub const MAX_GAME_PREFAB_REQUESTS: usize = 256;
/// Maximum prefab results retained before the host event bridge copies them.
pub const MAX_GAME_PREFAB_EVENTS: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GamePrefabSpawnRequest {
    pub(crate) path: String,
    pub(crate) position: Vec3,
    pub(crate) request_id: u64,
}

/// Bounded exclusive-world work queue for project prefab requests.
#[derive(Debug, Default)]
pub(crate) struct GamePrefabSpawnQueue {
    requests: VecDeque<GamePrefabSpawnRequest>,
}

impl GamePrefabSpawnQueue {
    pub(crate) fn len(&self) -> usize {
        self.requests.len()
    }

    pub(crate) fn push_preflighted(&mut self, request: GamePrefabSpawnRequest) {
        assert!(
            self.requests.len() < MAX_GAME_PREFAB_REQUESTS,
            "prefab request capacity must be checked during atomic preflight"
        );
        self.requests.push_back(request);
    }
}

/// Bounded source log for asynchronous spawn success and failure results.
#[derive(Debug)]
pub(crate) struct GamePrefabEvents {
    log: GameSourceEventLog,
}

impl Default for GamePrefabEvents {
    fn default() -> Self {
        Self {
            log: GameSourceEventLog::new("game prefab", MAX_GAME_PREFAB_EVENTS),
        }
    }
}

impl GamePrefabEvents {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &GameSourceEvent> {
        self.log.iter()
    }

    pub(crate) fn push(&mut self, payload: Value) {
        self.log.push(payload);
    }
}

/// Uses the normal authoring prefab bridge and returns the new runtime root.
pub(crate) fn spawn_prefab_at(
    world: &mut World,
    path: &str,
    position: Vec3,
) -> Result<Entity, String> {
    let bytes = world
        .get_resource::<AssetServer>()
        .ok_or_else(|| "AssetServer resource is missing".to_string())?
        .load_bytes(path)
        .map_err(|error| error.to_string())?;
    let json = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    let prefab = PrefabAsset::from_json(&json).map_err(|error| error.to_string())?;
    let instantiation = prefab
        .instantiate_with_root(None)
        .map_err(|error| error.to_string())?;

    let mut scene = AuthoringScene::new();
    let mut transaction = Transaction::begin(&scene);
    for command in instantiation.commands {
        transaction.apply(command);
    }
    transaction
        .commit(&mut scene)
        .map_err(|error| error.to_string())?;
    let entity_map =
        spawn_from_authoring_scene(world, &scene).map_err(|error| error.to_string())?;
    let root = entity_map
        .get(&instantiation.root)
        .ok_or_else(|| "spawned prefab root is missing from the runtime map".to_string())?;
    let transform = world
        .get_component_mut::<Transform>(root)
        .ok_or_else(|| "spawned prefab root has no Transform".to_string())?;
    transform.translation = position;
    Ok(root)
}

/// Processes project prefab work while the host owns exclusive world access.
pub(crate) fn process_game_prefab_requests(world: &mut World) {
    let Some(mut queue) = world.remove_resource::<GamePrefabSpawnQueue>() else {
        return;
    };
    let Some(mut events) = world.remove_resource::<GamePrefabEvents>() else {
        log::error!("game prefab queue exists without its result source log");
        queue.requests.clear();
        world.insert_resource(queue);
        return;
    };

    while let Some(request) = queue.requests.pop_front() {
        let result = spawn_prefab_at(world, &request.path, request.position);
        let mut fields = BTreeMap::from([
            (
                "request_id".to_owned(),
                Value::String(request.request_id.to_string()),
            ),
            ("path".to_owned(), Value::String(request.path)),
        ]);
        match result {
            Ok(root) => {
                fields.insert("status".to_owned(), Value::String("completed".to_owned()));
                fields.insert("root".to_owned(), entity_handle_value(root));
            }
            Err(message) => {
                fields.insert("status".to_owned(), Value::String("failed".to_owned()));
                fields.insert("message".to_owned(), Value::String(message));
            }
        }
        events.push(Value::Object(fields));
    }
    world.insert_resource(events);
    world.insert_resource(queue);
}

fn entity_handle_value(entity: Entity) -> Value {
    let handle = GameEntityHandle {
        id: entity.id(),
        generation: entity.generation(),
    };
    Value::Object(BTreeMap::from([
        ("id".to_owned(), Value::U64(u64::from(handle.id))),
        (
            "generation".to_owned(),
            Value::U64(u64::from(handle.generation)),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{AssetManifest, Assets};
    use crate::material::Material;
    use crate::mesh::Mesh;
    use crate::script_api::RuntimeEntityIdentity;
    use engine_authoring::{AuthoringEntity, EntityId, PREFAB_SCHEMA_VERSION};

    #[test]
    fn prefab_requests_emit_success_and_failure_with_request_ids() {
        let dir = tempfile::tempdir().unwrap();
        let root_id = EntityId::generate();
        let prefab = PrefabAsset {
            schema_version: PREFAB_SCHEMA_VERSION,
            root: root_id.clone(),
            entities: BTreeMap::from([(
                root_id.clone(),
                AuthoringEntity::new(root_id, "spawned_enemy"),
            )]),
        };
        std::fs::write(
            dir.path().join("enemy.prefab.json"),
            prefab.to_json().unwrap(),
        )
        .unwrap();

        let mut world = World::new();
        world.insert_resource(AssetServer::new(dir.path()));
        world.insert_resource(AssetManifest::default());
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(Assets::<Material>::default());
        let mut queue = GamePrefabSpawnQueue::default();
        queue.push_preflighted(GamePrefabSpawnRequest {
            path: "enemy.prefab.json".to_owned(),
            position: Vec3::new(1.0, 2.0, 3.0),
            request_id: u64::MAX,
        });
        queue.push_preflighted(GamePrefabSpawnRequest {
            path: "missing.prefab.json".to_owned(),
            position: Vec3::ZERO,
            request_id: 2,
        });
        world.insert_resource(queue);
        world.insert_resource(GamePrefabEvents::default());

        process_game_prefab_requests(&mut world);

        let spawned = world
            .query::<(&RuntimeEntityIdentity, &Transform)>()
            .unwrap()
            .iter()
            .find_map(|(_, (identity, transform))| {
                (identity.name == "spawned_enemy").then_some(transform.translation)
            })
            .unwrap();
        assert_eq!(spawned, Vec3::new(1.0, 2.0, 3.0));
        let events = world.get_resource::<GamePrefabEvents>().unwrap();
        let payloads = events
            .iter()
            .map(|event| &event.payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads.len(), 2);
        let Value::Object(success) = payloads[0] else {
            panic!("success result must be an object");
        };
        assert_eq!(success["status"], Value::String("completed".to_owned()));
        assert_eq!(success["request_id"], Value::String(u64::MAX.to_string()));
        assert!(success.contains_key("root"));
        let Value::Object(failure) = payloads[1] else {
            panic!("failure result must be an object");
        };
        assert_eq!(failure["status"], Value::String("failed".to_owned()));
        assert!(failure.contains_key("message"));
    }
}
