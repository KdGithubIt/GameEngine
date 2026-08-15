//! Non-blocking Cargo builds for project-local Rust game modules.

use engine_authoring::{initialize_game_project, replace_file_contents, ProjectRoot};
use serde_json::Value as JsonValue;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cargo operation requested by the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameBuildKind {
    /// Fast type-check without producing a loadable library.
    Check,
    /// Debug library used by Editor Play.
    Build,
    /// Optimized library copied into a product package.
    Release,
}

/// Current asynchronous build state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameBuildState {
    /// No Cargo process is running.
    Idle,
    /// Cargo check is running.
    Checking,
    /// A debug library build is running.
    Building,
    /// A release library build is running.
    BuildingRelease,
}

/// One compiler diagnostic parsed from Cargo's JSON stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameBuildDiagnostic {
    /// `error`, `warning`, `note`, or another rustc level.
    pub level: String,
    /// Main compiler message.
    pub message: String,
    /// Primary source file, when rustc supplied one.
    pub file: Option<PathBuf>,
    /// One-based source line, when available.
    pub line: Option<usize>,
    /// Complete rendered diagnostic intended for the Console.
    pub rendered: Option<String>,
}

/// Completed asynchronous Cargo operation.
#[derive(Debug)]
pub struct GameBuildResult {
    /// Requested operation.
    pub kind: GameBuildKind,
    /// Whether Cargo returned success and post-processing completed.
    pub success: bool,
    /// Parsed rustc diagnostics.
    pub diagnostics: Vec<GameBuildDiagnostic>,
    /// Generation-specific loadable library produced by a successful build.
    pub module_path: Option<PathBuf>,
    /// Process or post-processing error not represented by rustc JSON.
    pub error: Option<String>,
}

/// Owns at most one background Cargo process and its completion channel.
pub struct GameBuildManager {
    state: GameBuildState,
    child: Arc<Mutex<Option<Child>>>,
    receiver: Option<Receiver<GameBuildResult>>,
}

impl fmt::Debug for GameBuildManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GameBuildManager")
            .field("state", &self.state)
            .finish()
    }
}

impl Default for GameBuildManager {
    fn default() -> Self {
        Self {
            state: GameBuildState::Idle,
            child: Arc::new(Mutex::new(None)),
            receiver: None,
        }
    }
}

impl GameBuildManager {
    /// Returns the current process state.
    pub fn state(&self) -> GameBuildState {
        self.state
    }

    /// Starts one Cargo operation without blocking the editor thread.
    ///
    /// # Errors
    /// Returns [`GameBuildStartError::AlreadyRunning`] when another operation
    /// is active, or an initialization error before the worker starts.
    pub fn start(
        &mut self,
        project: &ProjectRoot,
        kind: GameBuildKind,
    ) -> Result<(), GameBuildStartError> {
        if self.state != GameBuildState::Idle {
            return Err(GameBuildStartError::AlreadyRunning);
        }
        // Initialization is idempotent and also makes the game manifest declare
        // its own Cargo workspace before Cargo performs package discovery. This
        // is required for projects stored below the engine repository's own
        // workspace root.
        initialize_game_project(project).map_err(GameBuildStartError::Project)?;
        let cargo_config = prepare_cargo_sdk_config(project)?;
        let project_root = project.path().to_path_buf();
        let game_dir = project.game_dir();
        let child_slot = Arc::clone(&self.child);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.state = match kind {
            GameBuildKind::Check => GameBuildState::Checking,
            GameBuildKind::Build => GameBuildState::Building,
            GameBuildKind::Release => GameBuildState::BuildingRelease,
        };

        thread::spawn(move || {
            let result =
                run_cargo_build(&project_root, &game_dir, &cargo_config, kind, &child_slot);
            let _ = sender.send(result);
        });
        Ok(())
    }

    /// Polls for completion without blocking.
    pub fn poll(&mut self) -> Option<GameBuildResult> {
        let result = self.receiver.as_ref()?.try_recv().ok()?;
        self.receiver = None;
        self.state = GameBuildState::Idle;
        Some(result)
    }

    /// Requests cancellation of the active Cargo process.
    ///
    /// Returns `true` when a child process existed and received `kill`.
    pub fn cancel(&mut self) -> bool {
        let Ok(mut child) = self.child.lock() else {
            return false;
        };
        child.as_mut().is_some_and(|process| process.kill().is_ok())
    }
}

/// Finds the newest successfully shadow-copied module generation.
pub fn latest_shadow_module(project: &ProjectRoot) -> Option<PathBuf> {
    let root = project.game_dir().join(".iroha").join("modules");
    let mut candidates: Vec<_> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .path()
                .join(engine::game_module::packaged_game_module_file_name())
        })
        .filter(|path| path.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

fn run_cargo_build(
    project_root: &Path,
    game_dir: &Path,
    cargo_config: &Path,
    kind: GameBuildKind,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> GameBuildResult {
    let mut command = Command::new("cargo");
    match kind {
        GameBuildKind::Check => {
            command.arg("check");
        }
        GameBuildKind::Build | GameBuildKind::Release => {
            command.arg("build");
        }
    }
    if kind == GameBuildKind::Release {
        command.arg("--release");
    }
    command
        .arg("--message-format=json")
        .arg("--config")
        .arg(cargo_config)
        .current_dir(game_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed_result(kind, format!("Cargo could not be started: {error}")),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Ok(mut slot) = child_slot.lock() {
        *slot = Some(child);
    }

    let stdout_reader = thread::spawn(move || {
        stdout
            .map(|stdout| {
                BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let stderr_reader = thread::spawn(move || {
        stderr
            .map(|stderr| {
                BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });

    let status = loop {
        let poll = match child_slot.lock() {
            Ok(mut slot) => match slot.as_mut() {
                Some(child) => child.try_wait(),
                None => {
                    return failed_result(kind, "Cargo process state was lost".to_owned());
                }
            },
            Err(_) => {
                return failed_result(kind, "Cargo process lock was poisoned".to_owned());
            }
        };
        match poll {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                return failed_result(kind, format!("Cargo status could not be read: {error}"))
            }
        }
    };
    if let Ok(mut slot) = child_slot.lock() {
        *slot = None;
    }
    let stdout_lines = stdout_reader.join().unwrap_or_default();
    let stderr_lines = stderr_reader.join().unwrap_or_default();
    let diagnostics = stdout_lines
        .iter()
        .filter_map(|line| parse_cargo_diagnostic(line))
        .collect();
    let compiled_module = stdout_lines
        .iter()
        .filter_map(|line| parse_cargo_cdylib_artifact(line))
        .next_back()
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                game_dir.join(path)
            }
        });
    if !status.success() {
        let error = if stderr_lines.is_empty() {
            format!("Cargo exited with status {status}")
        } else {
            stderr_lines.join("\n")
        };
        return GameBuildResult {
            kind,
            success: false,
            diagnostics,
            module_path: None,
            error: Some(error),
        };
    }
    let module_path = if kind == GameBuildKind::Check {
        None
    } else {
        let Some(compiled_module) = compiled_module else {
            return GameBuildResult {
                kind,
                success: false,
                diagnostics,
                module_path: None,
                error: Some(
                    "Cargo succeeded but reported no cdylib artifact; ensure game/Cargo.toml declares [lib] crate-type = [\"cdylib\"]"
                        .to_owned(),
                ),
            };
        };
        match shadow_copy_module(project_root, &compiled_module) {
            Ok(path) => Some(path),
            Err(error) => {
                return GameBuildResult {
                    kind,
                    success: false,
                    diagnostics,
                    module_path: None,
                    error: Some(error),
                }
            }
        }
    };
    GameBuildResult {
        kind,
        success: true,
        diagnostics,
        module_path,
        error: None,
    }
}

fn parse_cargo_diagnostic(line: &str) -> Option<GameBuildDiagnostic> {
    let value: JsonValue = serde_json::from_str(line).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }
    let message = value.get("message")?;
    let primary = message
        .get("spans")?
        .as_array()?
        .iter()
        .find(|span| span.get("is_primary").and_then(JsonValue::as_bool) == Some(true));
    Some(GameBuildDiagnostic {
        level: message
            .get("level")
            .and_then(JsonValue::as_str)
            .unwrap_or("error")
            .to_owned(),
        message: message
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("compiler diagnostic")
            .to_owned(),
        file: primary
            .and_then(|span| span.get("file_name"))
            .and_then(JsonValue::as_str)
            .map(PathBuf::from),
        line: primary
            .and_then(|span| span.get("line_start"))
            .and_then(JsonValue::as_u64)
            .map(|line| line as usize),
        rendered: message
            .get("rendered")
            .and_then(JsonValue::as_str)
            .map(str::to_owned),
    })
}

/// Extracts the host dynamic library from one Cargo `compiler-artifact` line.
///
/// Cargo derives the output basename from the package or library target, so
/// the editor must consume Cargo's reported filename instead of assuming a
/// project-specific name such as `iroha_game`.
fn parse_cargo_cdylib_artifact(line: &str) -> Option<PathBuf> {
    let value: JsonValue = serde_json::from_str(line).ok()?;
    if value.get("reason")?.as_str()? != "compiler-artifact" {
        return None;
    }
    let kinds = value.get("target")?.get("kind")?.as_array()?;
    if !kinds.iter().any(|kind| kind.as_str() == Some("cdylib")) {
        return None;
    }
    let expected_extension = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    value
        .get("filenames")?
        .as_array()?
        .iter()
        .filter_map(JsonValue::as_str)
        .map(PathBuf::from)
        .find(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension))
        })
}

/// Resolves the selected SDK using environment override, packaged layout, then workspace fallback.
pub fn engine_sdk_root() -> Result<PathBuf, GameBuildStartError> {
    if let Some(path) = std::env::var_os("IROHA_ENGINE_SDK") {
        let path = PathBuf::from(path);
        if path
            .join("crates")
            .join("engine")
            .join("Cargo.toml")
            .is_file()
        {
            return Ok(path);
        }
        return Err(GameBuildStartError::SdkMissing(path));
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    path.canonicalize()
        .map_err(|_| GameBuildStartError::SdkMissing(path))
}

/// Writes the standard Cargo configuration that makes the selected SDK dependency visible.
pub fn prepare_cargo_sdk_config(project: &ProjectRoot) -> Result<PathBuf, GameBuildStartError> {
    let sdk_root = engine_sdk_root()?;
    write_cargo_sdk_config(project, &sdk_root)
}

fn write_cargo_sdk_config(
    project: &ProjectRoot,
    sdk_root: &Path,
) -> Result<PathBuf, GameBuildStartError> {
    let cargo_dir = project.game_dir().join(".cargo");
    std::fs::create_dir_all(&cargo_dir).map_err(|source| GameBuildStartError::Io {
        path: cargo_dir.clone(),
        source,
    })?;
    let path = cargo_dir.join("config.toml");
    let engine_path_buf = sdk_root.join("crates").join("engine");
    let engine_path_text = engine_path_buf.to_string_lossy();
    // Windows canonicalization may add the verbatim `\\?\` prefix, which
    // Cargo's TOML path parser does not accept in a patch URL.
    let engine_path_text = engine_path_text
        .strip_prefix(r"\\?\")
        .unwrap_or(&engine_path_text);
    let engine_path = engine_path_text.replace('\\', "/");
    let contents = format!("[patch.crates-io]\nengine = {{ path = '{engine_path}' }}\n");
    replace_file_contents(&path, &contents).map_err(GameBuildStartError::Persist)?;
    Ok(path)
}

fn shadow_copy_module(project_root: &Path, source: &Path) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err(format!(
            "Cargo succeeded but {} was not produced",
            source.display()
        ));
    }
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let destination_dir = project_root
        .join("game")
        .join(".iroha")
        .join("modules")
        .join(generation.to_string());
    std::fs::create_dir_all(&destination_dir)
        .map_err(|error| format!("could not create {}: {error}", destination_dir.display()))?;
    let destination = destination_dir.join(engine::game_module::packaged_game_module_file_name());
    std::fs::copy(source, &destination).map_err(|error| {
        format!(
            "could not copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn failed_result(kind: GameBuildKind, error: String) -> GameBuildResult {
    GameBuildResult {
        kind,
        success: false,
        diagnostics: Vec::new(),
        module_path: None,
        error: Some(error),
    }
}

/// Errors that prevent a background Cargo process from starting.
#[derive(Debug)]
pub enum GameBuildStartError {
    /// Another build is active.
    AlreadyRunning,
    /// Project initialization or module indexing failed.
    Project(engine_authoring::GameProjectError),
    /// The engine SDK root could not be located.
    SdkMissing(PathBuf),
    /// Writing the ignored Cargo patch configuration failed.
    Persist(engine_authoring::PersistError),
    /// Creating the standard Cargo configuration directory failed.
    Io {
        /// Directory that could not be created.
        path: PathBuf,
        /// Operating-system error.
        source: std::io::Error,
    },
}

impl fmt::Display for GameBuildStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("a game-code build is already running"),
            Self::Project(source) => source.fmt(formatter),
            Self::SdkMissing(path) => write!(
                formatter,
                "Iroha engine SDK was not found at {}",
                path.display()
            ),
            Self::Persist(source) => source.fmt(formatter),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for GameBuildStartError {}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        create_rust_script, initialize_game_project, load_scene_from_json, move_rust_script,
        ProjectConfig, RustScriptKind, RustScriptSchedule, PROJECT_SCHEMA_VERSION,
    };

    #[test]
    fn cargo_json_diagnostic_preserves_file_and_line() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"bad type","rendered":"error: bad type","spans":[{"is_primary":true,"file_name":"src/lib.rs","line_start":7}]}}"#;
        let diagnostic = parse_cargo_diagnostic(line).unwrap();
        assert_eq!(diagnostic.level, "error");
        assert_eq!(diagnostic.file, Some(PathBuf::from("src/lib.rs")));
        assert_eq!(diagnostic.line, Some(7));
    }

    #[test]
    fn cargo_cdylib_artifact_uses_reported_non_default_package_name() {
        let extension = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        let file = format!("target/debug/custom_named_game.{extension}");
        let line = format!(
            r#"{{"reason":"compiler-artifact","target":{{"kind":["cdylib"]}},"filenames":["target/debug/custom_named_game.rlib","{file}"]}}"#
        );

        assert_eq!(
            parse_cargo_cdylib_artifact(&line),
            Some(PathBuf::from(file))
        );
    }

    #[test]
    fn cargo_non_cdylib_artifact_is_ignored() {
        let line = r#"{"reason":"compiler-artifact","target":{"kind":["lib"]},"filenames":["target/debug/libgame.rlib"]}"#;
        assert_eq!(parse_cargo_cdylib_artifact(line), None);
    }

    #[test]
    #[ignore = "requires a host Rust toolchain and performs a nested Cargo build"]
    fn generated_game_module_crosses_scoped_host_io_in_editor_play() {
        let directory = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            directory.path(),
            ProjectConfig {
                name: "game_module_test".to_owned(),
                schema_version: PROJECT_SCHEMA_VERSION,
            },
        )
        .unwrap();
        initialize_game_project(&project).unwrap();
        // Every kind is authored in its recommended folder and then relocated
        // into feature folders, which is the shape the unified module tree has
        // to compile.
        create_rust_script(
            &project,
            RustScriptKind::Component,
            "Health",
            RustScriptSchedule::Update,
        )
        .unwrap();
        move_rust_script(
            &project,
            Path::new("components/health.rs"),
            Path::new("player/stats/health.rs"),
        )
        .unwrap();
        create_rust_script(
            &project,
            RustScriptKind::Resource,
            "MissionState",
            RustScriptSchedule::Update,
        )
        .unwrap();
        move_rust_script(
            &project,
            Path::new("resources/mission_state.rs"),
            Path::new("gameplay/mission_state.rs"),
        )
        .unwrap();
        create_rust_script(
            &project,
            RustScriptKind::SharedModule,
            "Math",
            RustScriptSchedule::Update,
        )
        .unwrap();
        move_rust_script(
            &project,
            Path::new("shared/math.rs"),
            Path::new("common/math.rs"),
        )
        .unwrap();
        replace_file_contents(
            &project.rust_scripts_dir().join("common/math.rs"),
            "//! Ordinary helper module compiled without any engine declaration.\n\n/// Returns the flat damage applied by the E2E system.\npub fn calculate_damage() -> f32 {\n    10.0\n}\n",
        )
        .unwrap();
        create_rust_script(
            &project,
            RustScriptKind::System,
            "HealthDecay",
            RustScriptSchedule::Update,
        )
        .unwrap();
        move_rust_script(
            &project,
            Path::new("systems/health_decay.rs"),
            Path::new("player/combat/health_decay.rs"),
        )
        .unwrap();
        let system_path = project
            .rust_scripts_dir()
            .join("player/combat/health_decay.rs");
        replace_file_contents(
            &system_path,
            r#"//! E2E system.

use crate::common::math::calculate_damage;
use crate::gameplay::mission_state::MissionState;
use crate::player::stats::health::Health;
use engine::prelude::*;

#[derive(InputAction)]
#[input_action(name = "attack")]
struct Attack;

#[derive(GameQuerySpec)]
#[game_query(id = "game.query.health", write = Health, view = LocalTransformView)]
struct HealthQuery;

#[engine::game_system(
    id = "game.health_decay",
    display_name = "Health Decay",
    description = "Exercises scoped input, query, resource, host-view, and command IO.",
    schedule = "update",
    after = ["engine.player_controller"]
)]
fn health_decay(
    time: Time,
    attack: Action<Attack>,
    mut health: Query<HealthQuery>,
    mut mission: ResMut<MissionState>,
    _scene: View<SceneStateView>,
    mut commands: Commands,
) -> Result<(), GameApiError> {
    let should_enable = time.frame_index() == 7
        && time.fixed_delta_seconds() > 0.0
        && attack.just_pressed()
        && calculate_damage() > 0.0;
    mission.enabled = should_enable;
    let entities = health.rows().iter().map(QueryRow::entity).collect::<Vec<_>>();
    for entity in entities {
        health.set(entity, Health { enabled: should_enable })?;
        commands.translate(entity, Vec3::X);
    }
    Ok(())
}
"#,
        )
        .unwrap();

        let mut manager = GameBuildManager::default();
        manager.start(&project, GameBuildKind::Build).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        let result = loop {
            if let Some(result) = manager.poll() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nested Cargo build timed out"
            );
            thread::sleep(Duration::from_millis(50));
        };
        assert!(
            result.success,
            "nested build failed: {:?}\n{:?}",
            result.error, result.diagnostics
        );
        let module = Arc::new(
            engine::game_module::GameModule::load(
                result
                    .module_path
                    .as_deref()
                    .expect("build must return a module"),
            )
            .unwrap(),
        );
        let schemas: Vec<_> = module.component_schemas().cloned().collect();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].display_name, "Health");
        let resource_schemas: Vec<_> = module.resource_schemas().cloned().collect();
        assert_eq!(resource_schemas.len(), 1);
        assert_eq!(resource_schemas[0].id, "game.mission_state");
        let systems: Vec<_> = module.system_metadata().collect();
        assert_eq!(systems.len(), 1);
        assert_eq!(systems[0].id, "game.health_decay");
        assert_eq!(systems[0].display_name, "Health Decay");
        assert_eq!(systems[0].after, ["engine.player_controller"]);

        // The same library must survive the normal Editor Play constructor,
        // including authoring conversion, shared runtime registration, and a
        // real schedule tick. The detailed assertions below then inspect the
        // same host contracts without requiring a GPU window.
        let scene = load_scene_from_json(&format!(
            r#"{{
              "entities": [{{
                "id": "entity_01JP0000000000000000000001",
                "name": "player",
                "components": {{
                  "engine.transform": {{ "x": 0.0, "y": 0.0, "z": 0.0 }},
                  "{}": {{ "enabled": false }}
                }}
              }}]
            }}"#,
            schemas[0].type_id
        ))
        .unwrap();
        let mut editor_play =
            crate::runtime::RuntimePlayState::start_from_document_with_game_module(
                &scene,
                Some(&project),
                None,
                Some(Arc::clone(&module)),
            )
            .expect("generated module must start through Editor Play")
            .state;
        editor_play
            .tick()
            .expect("generated module must tick through Editor Play");

        // Retaining through App is the shared Editor/player host path that
        // installs module resource defaults before the first callback.
        let mut app = engine::App::new();
        app.retain_game_module(Arc::clone(&module));
        let world = app.world_mut();
        let entity = world.spawn().unwrap();
        world
            .add_component(entity, engine::Transform::default())
            .unwrap();
        module
            .spawn_component(
                world,
                entity,
                &schemas[0].type_id,
                &schemas[0].default_value(),
            )
            .unwrap();
        world.insert_resource(engine::time::Time {
            delta_seconds: 0.016,
            elapsed_seconds: 1.0,
            frame_count: 7,
        });
        world.insert_resource(engine::time::FixedTime::default());
        world.insert_resource(engine::Input::<engine::KeyCode>::new());
        let mut queue = engine::VirtualInputQueue::new();
        queue.push(
            engine::InputSource::Test,
            engine::InputCommand::Key {
                key: engine::KeyCode::Space,
                pressed: true,
            },
        );
        world.insert_resource(queue);
        engine::drain_virtual_input(world);
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
        let (input_actions, diagnostics) = engine::InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        world.insert_resource(input_actions);
        module
            .run_systems(engine::game_module::GameSystemSchedule::Update, world)
            .unwrap();
        let stored = world
            .get_component::<engine::game_module::GameComponentStore>(entity)
            .and_then(|store| store.value(&schemas[0].type_id))
            .expect("runtime game component must remain stored");
        let engine_authoring::Value::Object(fields) = stored else {
            panic!("component must remain an object");
        };
        assert_eq!(
            fields.get("enabled"),
            Some(&engine_authoring::Value::Bool(true))
        );
        assert_eq!(
            world
                .get_component::<engine::Transform>(entity)
                .unwrap()
                .translation
                .x,
            1.0
        );
        let metrics = world
            .get_resource::<engine::game_host::GameHostRuntime>()
            .and_then(|runtime| runtime.system_metrics("game.health_decay"))
            .expect("ABI v3 dispatch must record per-system metrics");
        assert_eq!(metrics.invocation_count, 1);
        assert_eq!(metrics.failure_count, 0);
        assert_eq!(metrics.last_query_rows, 1);
        assert!(metrics.last_input_bytes > 0);
        assert!(metrics.last_output_bytes > 0);
        let mission = world
            .get_resource::<engine::game_host::GameHostRuntime>()
            .and_then(|runtime| runtime.frame().resources.get("game.mission_state"))
            .expect("module default and validated patch must remain host-owned");
        let engine_authoring::Value::Object(mission) = mission else {
            panic!("mission resource must remain an object");
        };
        assert_eq!(
            mission.get("enabled"),
            Some(&engine_authoring::Value::Bool(true))
        );
        world.validate().unwrap();
    }
}
