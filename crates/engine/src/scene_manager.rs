//! Runtime scene-switch composition adapter (Phase 55, ADR 0047).
//!
//! Scene lifecycle state belongs to `engine-scene`; this module preserves the
//! historical `engine::scene_manager` path while keeping the world-spawn bridge
//! and exclusive-world switch algorithm in the top-level composition crate.

use crate::scene_bridge::{spawn_from_authoring_scene, SceneBridgeError};
use crate::scene_loader::SceneLoader;
use engine_ecs::{Entity, World};

pub use engine_scene::scene_manager::{SceneManager, SceneSwitchState};

/// Runs the pending scene switch request in `world`, if any.
///
/// Called once per frame by [`crate::App::process_scene_requests`]. Loading and
/// scene bookkeeping are owned by `engine-scene`; despawning and bridging stay
/// here because they compose ECS world mutation with the high-level scene bridge.
pub(crate) fn process_scene_switch(world: &mut World) {
    let Some(pending_path) = take_pending_request(world) else {
        return;
    };

    let Some(loader) = world.get_resource::<SceneLoader>() else {
        log::error!(
            "scene switch to `{pending_path}` was requested but no SceneLoader resource is \
             installed; the host must insert one at startup"
        );
        set_failed(
            world,
            pending_path,
            "no SceneLoader resource is installed".to_string(),
        );
        return;
    };

    let scene = match loader.load(&pending_path) {
        Ok(scene) => scene,
        Err(error) => {
            log::error!("scene switch to `{pending_path}` failed to load: {error}");
            set_failed(world, pending_path, error.to_string());
            return;
        }
    };

    despawn_current_scene(world, &pending_path);

    match spawn_from_authoring_scene(world, &scene) {
        Ok(map) => {
            let entities: Vec<Entity> = map.spawned_entities().collect();
            if let Some(manager) = world.get_resource_mut::<SceneManager>() {
                manager.complete_switch(pending_path, entities);
            }
            if let Some(state) = world.get_resource_mut::<SceneSwitchState>() {
                *state = SceneSwitchState::Idle;
            }
        }
        Err(error) => {
            log::error!("scene switch to `{pending_path}` failed to spawn: {error}");
            set_failed(world, pending_path, format_bridge_error(&error));
        }
    }
}

fn format_bridge_error(error: &SceneBridgeError) -> String {
    error.to_string()
}

fn take_pending_request(world: &mut World) -> Option<String> {
    world
        .get_resource_mut::<SceneManager>()
        .and_then(SceneManager::take_pending_request)
}

fn set_failed(world: &mut World, path: String, message: String) {
    if let Some(state) = world.get_resource_mut::<SceneSwitchState>() {
        *state = SceneSwitchState::Failed { path, message };
    }
}

fn despawn_current_scene(world: &mut World, requested_path: &str) {
    let entities = world
        .get_resource_mut::<SceneManager>()
        .map(SceneManager::take_current_entities)
        .unwrap_or_default();
    for entity in entities {
        if let Err(error) = world.despawn(entity) {
            log::warn!(
                "scene switch to `{requested_path}` could not despawn a previous-scene entity \
                 (it may have already been despawned by gameplay code): {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{ProjectConfig, ProjectRoot, PROJECT_SCHEMA_VERSION};

    fn make_project() -> (tempfile::TempDir, ProjectRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ProjectRoot::create(
            dir.path(),
            ProjectConfig {
                name: "SceneManagerTest".into(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project create must succeed");
        (dir, root)
    }

    fn write_scene_with_one_entity(root: &ProjectRoot, file_name: &str, entity_name: &str) {
        let scene_path = root.scenes_dir().join(file_name);
        let json = engine_authoring::test_fixtures::complete_scene_document(&format!(
            r#"{{"entities":[{{"id":"entity_01JP0000000000000000000001","name":"{entity_name}","components":{{}}}}]}}"#
        ))
        .expect("scene fixture must be valid JSON");
        std::fs::write(scene_path, json).expect("scene fixture write must succeed");
    }

    fn world_with_initial_scene(
        root: ProjectRoot,
        initial_scene_file: &str,
    ) -> (World, Vec<Entity>) {
        let loader = SceneLoader::new(root);
        let relative = format!("scenes/{initial_scene_file}");
        let scene = loader.load(&relative).expect("initial scene must load");

        let mut world = World::new();
        let map =
            spawn_from_authoring_scene(&mut world, &scene).expect("initial scene must bridge");
        let entities: Vec<Entity> = map.spawned_entities().collect();

        world.insert_resource(loader);
        world.insert_resource(SceneManager::new());
        world.insert_resource(SceneSwitchState::default());
        world
            .get_resource_mut::<SceneManager>()
            .expect("SceneManager was just inserted")
            .register_initial_scene(relative, entities.clone());

        (world, entities)
    }

    fn request_switch(world: &mut World, path: &str) {
        world
            .get_resource_mut::<SceneManager>()
            .expect("SceneManager must be installed")
            .request_switch(path);
    }

    #[test]
    fn process_with_no_pending_request_is_a_no_op() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        let (mut world, _entities) = world_with_initial_scene(root, "a.scene.json");
        let entity_count_before = world.entity_count();

        process_scene_switch(&mut world);

        assert_eq!(world.entity_count(), entity_count_before);
        assert!(matches!(
            world.get_resource::<SceneSwitchState>().unwrap(),
            SceneSwitchState::Idle
        ));
        drop(dir);
    }

    #[test]
    fn successful_switch_despawns_old_scene_and_spawns_new_one() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        write_scene_with_one_entity(&root, "b.scene.json", "b");
        let (mut world, entities_a) = world_with_initial_scene(root, "a.scene.json");

        request_switch(&mut world, "scenes/b.scene.json");
        process_scene_switch(&mut world);

        for entity in &entities_a {
            assert!(
                !world.contains_entity(*entity),
                "scene A entity must be despawned after switching away"
            );
        }
        assert_eq!(world.entity_count(), 1, "scene B's single entity must exist");
        let manager = world.get_resource::<SceneManager>().unwrap();
        assert_eq!(manager.generation(), 1);
        assert_eq!(manager.current_scene_path(), Some("scenes/b.scene.json"));
        assert!(matches!(
            world.get_resource::<SceneSwitchState>().unwrap(),
            SceneSwitchState::Idle
        ));
        drop(dir);
    }

    #[test]
    fn switch_to_missing_path_leaves_current_scene_intact_and_reports_failed() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        let (mut world, entities_a) = world_with_initial_scene(root, "a.scene.json");

        request_switch(&mut world, "scenes/missing.scene.json");
        process_scene_switch(&mut world);

        for entity in &entities_a {
            assert!(
                world.contains_entity(*entity),
                "current scene must survive a failed switch"
            );
        }
        let manager = world.get_resource::<SceneManager>().unwrap();
        assert_eq!(manager.generation(), 0, "failed switch must not bump generation");
        assert_eq!(manager.current_scene_path(), Some("scenes/a.scene.json"));
        match world.get_resource::<SceneSwitchState>().unwrap() {
            SceneSwitchState::Failed { path, .. } => {
                assert_eq!(path, "scenes/missing.scene.json");
            }
            SceneSwitchState::Idle => panic!("expected Failed state for a missing scene path"),
        }
        drop(dir);
    }

    #[test]
    fn switch_to_malformed_json_leaves_current_scene_intact_and_reports_failed() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        std::fs::write(root.scenes_dir().join("bad.scene.json"), "not valid json")
            .expect("malformed fixture write must succeed");
        let (mut world, entities_a) = world_with_initial_scene(root, "a.scene.json");

        request_switch(&mut world, "scenes/bad.scene.json");
        process_scene_switch(&mut world);

        for entity in &entities_a {
            assert!(
                world.contains_entity(*entity),
                "current scene must survive a malformed-JSON switch"
            );
        }
        let manager = world.get_resource::<SceneManager>().unwrap();
        assert_eq!(manager.generation(), 0);
        assert_eq!(manager.current_scene_path(), Some("scenes/a.scene.json"));
        assert!(matches!(
            world.get_resource::<SceneSwitchState>().unwrap(),
            SceneSwitchState::Failed { .. }
        ));
        drop(dir);
    }

    #[test]
    fn missing_loader_records_failed_without_panicking() {
        let mut world = World::new();
        world.insert_resource(SceneManager::new());
        world.insert_resource(SceneSwitchState::default());
        request_switch(&mut world, "scenes/anything.scene.json");

        process_scene_switch(&mut world);

        match world.get_resource::<SceneSwitchState>().unwrap() {
            SceneSwitchState::Failed { path, .. } => {
                assert_eq!(path, "scenes/anything.scene.json");
            }
            SceneSwitchState::Idle => panic!("expected Failed state without a SceneLoader"),
        }
    }

    #[test]
    fn double_request_in_one_frame_only_executes_the_last_one() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        write_scene_with_one_entity(&root, "b.scene.json", "b");
        write_scene_with_one_entity(&root, "c.scene.json", "c");
        let (mut world, _entities_a) = world_with_initial_scene(root, "a.scene.json");

        request_switch(&mut world, "scenes/b.scene.json");
        request_switch(&mut world, "scenes/c.scene.json");
        process_scene_switch(&mut world);

        let manager = world.get_resource::<SceneManager>().unwrap();
        assert_eq!(manager.current_scene_path(), Some("scenes/c.scene.json"));
        assert_eq!(manager.generation(), 1, "only the last request in the frame must execute");
        drop(dir);
    }

    #[test]
    fn duplicate_pending_and_current_requests_are_ignored() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        write_scene_with_one_entity(&root, "b.scene.json", "b");
        let (mut world, _entities_a) = world_with_initial_scene(root, "a.scene.json");

        request_switch(&mut world, "scenes/a.scene.json");
        assert_eq!(
            world
                .get_resource::<SceneManager>()
                .and_then(SceneManager::pending_scene_path),
            None
        );

        request_switch(&mut world, "scenes/b.scene.json");
        request_switch(&mut world, "scenes/b.scene.json");
        process_scene_switch(&mut world);

        let manager = world.get_resource::<SceneManager>().unwrap();
        assert_eq!(manager.current_scene_path(), Some("scenes/b.scene.json"));
        assert_eq!(manager.generation(), 1);
        drop(dir);
    }

    #[test]
    fn gameplay_spawned_entity_survives_a_scene_switch() {
        let (dir, root) = make_project();
        write_scene_with_one_entity(&root, "a.scene.json", "a");
        write_scene_with_one_entity(&root, "b.scene.json", "b");
        let (mut world, _entities_a) = world_with_initial_scene(root, "a.scene.json");

        let gameplay_entity = world
            .spawn()
            .expect("gameplay entity must spawn outside the scene bridge");

        request_switch(&mut world, "scenes/b.scene.json");
        process_scene_switch(&mut world);

        assert!(
            world.contains_entity(gameplay_entity),
            "entities spawned outside the bridge must survive a scene switch"
        );
        drop(dir);
    }
}
