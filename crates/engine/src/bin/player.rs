//! Data-driven package player (Phase 51, ADR 0045).
//!
//! Runs a packaged editor project: opens `project.json` in the package
//! root, loads the start scene from `project_settings.json`, and plays it
//! with the same gameplay systems as editor Play.
//!
//! Usage: `player [package_root]` — the package root defaults to the
//! executable's directory. Fatal startup problems exit with code 2.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    match desktop::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("player: {error}");
            desktop::write_startup_failure(&error.to_string());
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // The packaged player targets desktop platforms (ADR 0034 / 0045).
}

#[cfg(not(target_arch = "wasm32"))]
mod desktop {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Arc;

    use engine::scene_bridge::spawn_from_authoring_scene;
    use engine::{register_runtime_systems, AssetManifest, AssetServer, Camera3D, GlobalTransform};
    use engine_authoring::{ProjectRoot, ProjectSettings};

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let root = package_root()?;
        let project = ProjectRoot::open(&root)
            .map_err(|error| format!("cannot open package at {}: {error}", root.display()))?;
        let settings = ProjectSettings::load(project.path())
            .map_err(|error| format!("cannot load project_settings.json: {error}"))?;
        let Some(start_scene) = settings.start_scene.clone() else {
            return Err("project_settings.json has no start_scene; re-package the project".into());
        };

        let manifest = load_manifest(&project);
        let assets_root = project.assets_root();
        let title = project.config().name.clone();
        let portable_saves = std::env::var_os("GAMEENGINE_PORTABLE_SAVES").is_some();
        let save_root = engine::distributed_save_root(&title, &root, portable_saves);

        let loader = engine::SceneLoader::new(project);
        let scene = loader
            .load(&start_scene)
            .map_err(|error| format!("cannot load start scene `{start_scene}`: {error}"))?;

        let mut app = engine::App::new().with_title(title);
        engine::native_2d::apply_project_2d_settings(&mut app, &settings.native_2d);
        let (input_actions, input_diagnostics) =
            engine::InputActionMap::from_project_settings(&settings);
        app.insert_resource(input_actions);
        for diagnostic in input_diagnostics {
            eprintln!(
                "player: input action `{}`: {}",
                diagnostic.action, diagnostic.message
            );
        }
        match engine::AudioSystem::new() {
            Ok(audio) => {
                app.insert_resource(audio);
            }
            Err(error) => {
                eprintln!("player: audio output is unavailable: {error}");
            }
        }
        app.insert_resource(AssetServer::with_assets_root(assets_root));
        app.insert_resource(manifest);
        app.insert_resource(loader);
        app.insert_resource(engine::SaveStore::new(save_root));

        register_runtime_systems(&mut app)
            .map_err(|error| format!("system registration failed: {error}"))?;

        // Inserted unconditionally, whether or not `baked_anim/` exists: a
        // package with no cross-skeleton retargets never consults it, while a
        // package that needed one but shipped without it should fail loudly
        // rather than silently fall back to baking at runtime (ADR 0079 §4,
        // AP-8).
        app.insert_resource(engine::PackagedBakedClips {
            root: root.join("baked_anim"),
        });

        let game_module_path = root.join(engine::game_module::packaged_game_module_file_name());
        let game_module = if game_module_path.is_file() {
            let game_module = engine::game_module::GameModule::load(&game_module_path)
                .map_err(|error| format!("cannot load game module: {error}"))?;
            let game_module = Arc::new(game_module);
            app.retain_game_module(Arc::clone(&game_module));
            Some(game_module)
        } else {
            None
        };

        let entity_map = spawn_from_authoring_scene(app.world_mut(), &scene)
            .map_err(|error| format!("scene conversion failed: {error}"))?;
        for diagnostic in &entity_map.asset_diagnostics {
            eprintln!("player: {}: {}", diagnostic.code, diagnostic.message);
        }

        // Registers the scene this startup already spawned as the
        // `SceneManager`'s current scene, so the first in-game
        // `request_switch` (Phase 60's script API, or a Rust game system
        // using `App::world_mut`) despawns these entities correctly instead
        // of leaking them (ADR 0047).
        let initial_entities = entity_map.spawned_entities().collect();
        if let Some(manager) = app.world_mut().get_resource_mut::<engine::SceneManager>() {
            manager.register_initial_scene(start_scene, initial_entities);
        }

        if let Some(game_module) = game_module {
            app.try_register_game_module_systems(game_module)
                .map_err(|error| format!("game system registration failed: {error}"))?;
        }
        let report = app
            .apply_system_settings(&settings.system_settings)
            .map_err(|error| format!("system order is invalid: {error}"))?;
        for diagnostic in report.update.into_iter().chain(report.fixed_update) {
            eprintln!("player: system settings: {diagnostic:?}");
        }
        for invalid_id in report.invalid_ids {
            eprintln!("player: ignored invalid system ID `{invalid_id}`");
        }

        ensure_camera(app.world_mut())?;

        app.run()
            .map_err(|error| format!("runtime error: {error}"))?;
        Ok(())
    }

    pub fn write_startup_failure(message: &str) {
        let root = package_root().unwrap_or_else(|_| PathBuf::from("."));
        let portable = std::env::var_os("GAMEENGINE_PORTABLE_LOGS").is_some();
        let log_root = engine::distributed_log_root("player", &root, portable);
        if std::fs::create_dir_all(&log_root).is_err() {
            return;
        }
        let path = log_root.join("startup.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "startup failure: {message}");
        }
    }

    /// Resolves the package root from the CLI or the executable location.
    fn package_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
        if let Some(argument) = std::env::args_os().nth(1) {
            return Ok(PathBuf::from(argument));
        }
        let executable = std::env::current_exe()?;
        Ok(executable
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")))
    }

    /// Loads the packaged manifest; a missing file degrades to empty.
    fn load_manifest(project: &ProjectRoot) -> AssetManifest {
        let path = project.path().join("asset_manifest.json");
        let json = match std::fs::read_to_string(&path) {
            Ok(json) => json,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("player: failed to read {}: {error}", path.display());
                }
                return AssetManifest::default();
            }
        };
        match AssetManifest::from_json(&json) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("player: failed to parse {}: {error}", path.display());
                AssetManifest::default()
            }
        }
    }

    /// Inserts a default camera when the scene spawned none.
    fn ensure_camera(world: &mut engine::ecs::World) -> Result<(), Box<dyn std::error::Error>> {
        let has_camera = {
            let query = engine::ecs::Query::<&Camera3D>::new(world);
            query.iter().next().is_some()
        };
        if has_camera {
            return Ok(());
        }
        eprintln!("player: scene has no camera; using a default camera");
        let camera = world
            .spawn_with(engine::camera::default_camera_transform())
            .map_err(|error| format!("default camera spawn failed: {error}"))?;
        world
            .add_component(camera, GlobalTransform::default())
            .map_err(|error| format!("default camera setup failed: {error}"))?;
        world
            .add_component(camera, Camera3D::default())
            .map_err(|error| format!("default camera setup failed: {error}"))?;
        Ok(())
    }
}
