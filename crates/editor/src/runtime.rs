//! Editor-owned runtime play state.
//!
//! The runtime world is created from the open authoring scene, ticked
//! in-process, rendered into an editor-owned offscreen texture, and discarded
//! on Stop.

use eframe::{egui, egui_wgpu, wgpu};
use engine::scene_bridge::{
    spawn_from_authoring_scene, AuthoringToRuntimeMap, SceneBridgeError,
};
use engine::{
    register_runtime_systems, AssetManifest, AssetServer, Camera3D, DebugLines, FixedTime,
    GlobalTransform, InputCommand, InputSource, SceneLoader, SceneManager, ViewportSize,
    VirtualInputQueue,
};
use engine::{InputReplay, ReplayPlayer, ReplayRecorder};
use engine_authoring::id::{AssetId, EdgeId, EntityId, NodeId};
use engine_authoring::DiagnosticTarget;
use engine_authoring::{
    load_scene_from_json, AuthoringScene, Diagnostic, ProjectRoot, ProjectSettings,
};
use std::fmt;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

/// Read-only runtime entity row used by the editor debugger.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeEntityDebugSnapshot {
    pub(crate) entity: engine::ecs::Entity,
    pub(crate) name: String,
    pub(crate) authoring_id: Option<String>,
    pub(crate) components: Vec<&'static str>,
    pub(crate) transform: Option<([f32; 3], [f32; 3], [f32; 3])>,
}

/// Frame-level performance counters shown without mutating the runtime world.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RuntimePerformanceSnapshot {
    pub(crate) last_tick_ms: f64,
    pub(crate) maximum_tick_ms: f64,
    pub(crate) average_tick_ms: f64,
    pub(crate) entity_count: usize,
    pub(crate) fixed_steps: u64,
}

/// Current editor play mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    /// Authoring documents are editable.
    Edit,
    /// A separate runtime world is ticking.
    Playing,
}

/// Read-only physical and logical input state shown by the Input Debugger.
#[derive(Debug, Clone, Default)]
pub(crate) struct RuntimeInputDebugSnapshot {
    pub(crate) keyboard: Vec<String>,
    pub(crate) mouse_buttons: Vec<String>,
    pub(crate) gamepad_buttons: Vec<String>,
    pub(crate) gamepad_axes: Vec<String>,
    pub(crate) connected_gamepads: Vec<u32>,
    pub(crate) connection_generation: u64,
    pub(crate) last_connection_change: Option<(u32, bool)>,
    pub(crate) actions: Vec<(String, engine::game_io::GameInputActionState)>,
}

/// GUI-free read-only evidence from the actual Play-mode Animation Controller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeAnimationDebugSnapshot {
    pub(crate) runtime_entity: (u32, u32),
    pub(crate) authoring_entity: EntityId,
    pub(crate) playback_state: String,
    pub(crate) clip_runtime_id: u64,
    pub(crate) clip_time: f32,
    pub(crate) playback_speed: f32,
    pub(crate) looping: bool,
    pub(crate) root_motion_mode: String,
    pub(crate) root_motion_delta: [f32; 3],
    pub(crate) crossfade_progress: Option<f32>,
    pub(crate) graph_state: Option<String>,
    pub(crate) graph_transition_sequence: u64,
    pub(crate) graph_last_transition: Option<String>,
    pub(crate) graph_parameters: Vec<(String, bool)>,
    pub(crate) graph_parameter_values: Vec<(String, String)>,
    pub(crate) graph_asset: Option<AssetId>,
    pub(crate) graph_id: Option<engine_authoring::GraphId>,
    pub(crate) animation_set_asset: Option<AssetId>,
    pub(crate) current_state: Option<NodeId>,
    pub(crate) previous_state: Option<NodeId>,
    pub(crate) next_state: Option<NodeId>,
    pub(crate) active_transition: Option<EdgeId>,
    pub(crate) transition_condition: Option<String>,
    pub(crate) motion_slot: Option<engine_authoring::MotionSlotId>,
    pub(crate) motion_slot_name: Option<String>,
    pub(crate) motion_source: Option<engine_authoring::MotionSourceRef>,
    pub(crate) resolved_motion_variant: Option<engine_authoring::MotionSourceVariant>,
    pub(crate) recent_events: Vec<String>,
    pub(crate) runtime_error: Option<String>,
}

/// Runtime world owned by editor play mode.
pub struct RuntimePlayState {
    app: engine::App,
    /// Stable authoring-to-runtime mapping created for this Play world.
    ///
    /// The Play Scene View reads it in reverse when a runtime-space pick must
    /// select the corresponding authoring entity. It never mutates the map or
    /// writes runtime IDs into the authoring document.
    entity_map: AuthoringToRuntimeMap,
    mapped_entity_count: usize,
    started_at: Instant,
    last_tick: Instant,
    ticks: u64,
    last_tick_duration: std::time::Duration,
    maximum_tick_duration: std::time::Duration,
    total_tick_duration: std::time::Duration,
    paused: bool,
    single_step_requested: bool,
    replay_recorder: Option<ReplayRecorder>,
    replay_player: Option<ReplayPlayer>,
    /// Offscreen renderer kept across Game View resizes so panel drags do not
    /// rebuild shaders and pipelines every frame.
    renderer: Option<engine::PreviewRenderer>,
    game_view: Option<RuntimeGameView>,
    scene_view: Option<RuntimeGameView>,
}

impl RuntimePlayState {
    /// Builds a runtime world from an authoring scene.
    ///
    /// The source scene is borrowed only during conversion. Runtime mutations
    /// never write back into the authoring document.
    ///
    /// The runtime [`SceneManager`] resource is installed but has no
    /// registered current scene, so a [`SceneManager::request_switch`] made
    /// before any successful switch has run will despawn nothing on its way
    /// in (there is no tracked "current scene" to despawn). Use
    /// [`RuntimePlayState::start_with_scene_path`] when the scene's
    /// project-relative path is known so the first switch cleans this scene
    /// up correctly.
    pub fn start(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
    ) -> Result<PlayStart, PlayError> {
        Self::start_impl(scene, project, None, None, None)
    }

    /// Same as [`RuntimePlayState::start`], but also registers
    /// `initial_scene_path` as the manager's current scene (ADR 0047), so a
    /// later [`SceneManager::request_switch`] despawns this scene's entities
    /// before spawning the next one.
    ///
    /// `initial_scene_path` is the project-relative path [`SceneLoader::load`]
    /// would accept to reload this same scene (for example
    /// `"scenes/main.scene.json"`).
    pub fn start_with_scene_path(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
        initial_scene_path: &str,
    ) -> Result<PlayStart, PlayError> {
        Self::start_impl(scene, project, Some(initial_scene_path), None, None)
    }

    /// Convenience wrapper that derives the initial scene path from an
    /// absolute document path when possible, falling back to
    /// [`RuntimePlayState::start`] otherwise.
    ///
    /// `document_path` is typically the absolute path of the currently open
    /// scene document. When both `project` and `document_path` are given and
    /// `document_path` lives under the project's `assets/` directory, this
    /// behaves like [`RuntimePlayState::start_with_scene_path`]; otherwise it
    /// behaves like [`RuntimePlayState::start`] (for example, an unsaved
    /// document has no path to register).
    pub fn start_from_document(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
        document_path: Option<&Path>,
    ) -> Result<PlayStart, PlayError> {
        Self::start_from_document_with_game_module(scene, project, document_path, None)
    }

    /// Starts Play with an already-built project-local Rust game module.
    pub fn start_from_document_with_game_module(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
        document_path: Option<&Path>,
        game_module: Option<Arc<engine::game_module::GameModule>>,
    ) -> Result<PlayStart, PlayError> {
        Self::start_from_document_with_game_module_and_overlay(
            scene, project, document_path, game_module, None,
        )
    }

    /// Starts Play from the same scene while supplying an immutable snapshot of
    /// unsaved project documents that must override saved asset files.
    pub fn start_from_document_with_game_module_and_overlay(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
        document_path: Option<&Path>,
        game_module: Option<Arc<engine::game_module::GameModule>>,
        overlay: Option<engine::authoring_overlay::AuthoringDocumentOverlay>,
    ) -> Result<PlayStart, PlayError> {
        match document_path.and_then(|path| relative_scene_path(project, path)) {
            Some(relative) => Self::start_impl(scene, project, Some(&relative), game_module, overlay),
            None => Self::start_impl(scene, project, None, game_module, overlay),
        }
    }

    fn start_impl(
        scene: &AuthoringScene,
        project: Option<&ProjectRoot>,
        initial_scene_path: Option<&str>,
        game_module: Option<Arc<engine::game_module::GameModule>>,
        authoring_overlay: Option<engine::authoring_overlay::AuthoringDocumentOverlay>,
    ) -> Result<PlayStart, PlayError> {
        let diagnostics = scene.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(PlayError::InvalidScene { diagnostics });
        }

        let mut diagnostics = Vec::new();
        let mut app = engine::App::new();
        if let Some(module) = game_module.as_ref() {
            // Scene conversion needs component schemas before callbacks are
            // appended to the final runtime schedule.
            app.retain_game_module(Arc::clone(module));
        }
        // Unit tests create and destroy many Play sessions in one process.
        // Opening the host audio backend for each fixture is unrelated to the
        // scene conversion being tested and crashes headless Windows runners
        // inside the native audio driver. Production builds still initialize
        // the real output device and retain the existing fallback diagnostic.
        #[cfg(not(test))]
        {
            match engine::AudioSystem::new() {
                Ok(audio) => {
                    app.insert_resource(audio);
                }
                Err(error) => {
                    diagnostics.push(
                        RuntimeDiagnosticKind::AudioUnavailable
                            .to_diagnostic(format!("audio output is unavailable: {error}")),
                    );
                }
            }
        }
        if let Some(project) = project {
            app.insert_resource(AssetServer::with_assets_root(project.assets_root()));
            let (manifest, manifest_diagnostics) = load_project_asset_manifest(project);
            app.insert_resource(manifest);
            diagnostics.extend(manifest_diagnostics);
            // Installed unconditionally whenever a project is known, even if
            // `initial_scene_path` is not: a game may still request its first
            // switch to a path this Play session never itself loaded from.
            app.insert_resource(SceneLoader::new(project.clone()));
            // Editor Play saves live under the project root, not the assets
            // root, so they are distinct from authoring content (ADR 0048 §3).
            app.insert_resource(engine::SaveStore::new(project.path().join("saves")));
        }

        if let Some(overlay) = authoring_overlay {
            app.insert_resource(overlay);
        }

        let entity_map =
            spawn_from_authoring_scene(app.world_mut(), scene).map_err(PlayError::SceneBridge)?;

        if let Some(path) = initial_scene_path {
            let entities = entity_map.spawned_entities().collect();
            if let Some(manager) = app.world_mut().get_resource_mut::<SceneManager>() {
                manager.register_initial_scene(path.to_string(), entities);
            }
        }

        register_runtime_systems(&mut app)
            .map_err(|source| PlayError::SystemRegistration { source })?;
        // Game systems are appended after every Engine system by default,
        // preserving ADR 0050 behavior until project settings explicitly move
        // them. Each callback becomes an ordinary named ECS schedule entry.
        if let Some(game_module) = game_module {
            app.try_register_game_module_systems(game_module)
                .map_err(|source| PlayError::SystemRegistration { source })?;
        }

        let project_settings = match project {
            Some(project) => match ProjectSettings::load(project.path()) {
                Ok(settings) => settings,
                Err(error) => {
                    diagnostics.push(Diagnostic::warning(
                        "editor.runtime.project_settings_load_failed",
                        format!("could not load project settings; using defaults: {error}"),
                    ));
                    ProjectSettings::default()
                }
            },
            None => ProjectSettings::default(),
        };
        engine::native_2d::apply_project_2d_settings(&mut app, &project_settings.native_2d);
        let (input_actions, input_diagnostics) =
            engine::InputActionMap::from_project_settings(&project_settings);
        app.insert_resource(input_actions);
        diagnostics.extend(input_diagnostics.into_iter().map(|diagnostic| {
            Diagnostic::warning(
                "editor.runtime.input_binding_ignored",
                format!(
                    "input action `{}`: {}",
                    diagnostic.action, diagnostic.message
                ),
            )
        }));
        let report = app
            .apply_system_settings(&project_settings.system_settings)
            .map_err(PlayError::SystemOrdering)?;
        diagnostics.extend(system_settings_diagnostics(report));

        diagnostics.extend(entity_map.asset_diagnostics.iter().cloned());

        if !has_camera(app.world_mut()) {
            insert_default_camera(app.world_mut()).map_err(PlayError::DefaultCamera)?;
            diagnostics.push(RuntimeDiagnosticKind::NoCamera.to_diagnostic(
                "scene has no runtime camera; inserted a temporary default camera for Play",
            ));
        }

        let now = Instant::now();
        Ok(PlayStart {
            state: Self {
                app,
                entity_map,
                mapped_entity_count: scene.entity_count(),
                started_at: now,
                last_tick: now,
                ticks: 0,
                last_tick_duration: std::time::Duration::ZERO,
                maximum_tick_duration: std::time::Duration::ZERO,
                total_tick_duration: std::time::Duration::ZERO,
                paused: false,
                single_step_requested: false,
                replay_recorder: None,
                replay_player: None,
                renderer: None,
                game_view: None,
                scene_view: None,
            },
            diagnostics,
        })
    }

    /// Advances the runtime schedule once.
    ///
    /// Panics are caught at this boundary so Play mode can be aborted without
    /// taking down the editor process.
    pub fn tick(&mut self) -> Result<(), PlayTickError> {
        let tick_started = Instant::now();
        let now = Instant::now();
        let wall_delta = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        let single_step = self.paused && std::mem::take(&mut self.single_step_requested);
        if self.paused && !single_step {
            return Ok(());
        }
        let replay_delta = self
            .replay_player
            .as_ref()
            .map(ReplayPlayer::fixed_step_seconds);
        let delta = if let Some(replay_delta) = replay_delta {
            replay_delta
        } else if single_step {
            self.app
                .world()
                .get_resource::<FixedTime>()
                .map(|time| time.fixed_delta)
                .unwrap_or(engine::time::FIXED_DELTA_SECONDS)
        } else {
            wall_delta
        };

        if let Some(player) = &mut self.replay_player {
            let next_tick = self
                .app
                .world()
                .get_resource::<FixedTime>()
                .map_or(0, |time| time.step_count);
            player
                .inject_tick(self.app.world_mut(), next_tick)
                .map_err(PlayTickError::Replay)?;
        }
        let world = self.app.world_mut();
        engine::clear_input_transitions(world);
        engine::drain_virtual_input(world);
        engine::prepare_mouse_frame(world);

        if let Some(time) = self
            .app
            .world_mut()
            .get_resource_mut::<engine::time::Time>()
        {
            time.advance(delta);
        }

        // Runs before schedule execution, with exclusive world access, so a
        // pending `SceneManager::request_switch` is honored before this
        // tick's fixed/frame updates run against the (possibly new) scene
        // (ADR 0047 §1).
        self.app.process_scene_requests();

        let fixed_steps = if single_step {
            1
        } else {
            self.app
                .world_mut()
                .get_resource_mut::<FixedTime>()
                .map(|ft| ft.step(delta))
                .unwrap_or(0)
        };
        for _ in 0..fixed_steps {
            if let Some(time) = self.app.world_mut().get_resource_mut::<FixedTime>() {
                time.begin_step();
            }
            let fixed_result =
                catch_unwind(AssertUnwindSafe(|| self.app.ecs_mut().run_fixed_update()));
            match fixed_result {
                Ok(Ok(())) => {}
                Ok(Err(source)) => return Err(PlayTickError::Schedule { source }),
                Err(_) => return Err(PlayTickError::Panicked),
            }
        }

        let result = catch_unwind(AssertUnwindSafe(|| self.app.ecs_mut().update()));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(source)) => return Err(PlayTickError::Schedule { source }),
            Err(_) => return Err(PlayTickError::Panicked),
        }
        self.ticks = self.ticks.saturating_add(1);
        self.last_tick_duration = tick_started.elapsed();
        self.maximum_tick_duration = self.maximum_tick_duration.max(self.last_tick_duration);
        self.total_tick_duration = self
            .total_tick_duration
            .saturating_add(self.last_tick_duration);
        Ok(())
    }

    /// Pauses or resumes runtime schedules without destroying the Play world.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.single_step_requested = false;
        self.last_tick = Instant::now();
    }

    /// Returns whether Play schedules are currently paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Requests exactly one fixed and frame schedule pass while paused.
    pub fn request_single_step(&mut self) {
        if self.paused {
            self.single_step_requested = true;
        }
    }

    /// Returns the total fixed-step count owned by the runtime clock.
    pub fn fixed_step_count(&self) -> u64 {
        self.app
            .world()
            .get_resource::<FixedTime>()
            .map_or(0, |time| time.step_count)
    }

    /// Returns collision broad-phase and contact statistics for the last fixed step.
    pub fn collision_stats(&self) -> engine::CollisionStats {
        self.app
            .world()
            .get_resource::<engine::CollisionStats>()
            .copied()
            .unwrap_or_default()
    }

    /// Queues one virtual input command for the next runtime tick.
    pub fn queue_input(&mut self, source: InputSource, command: InputCommand) {
        if source != InputSource::Replay
            && let Some(recorder) = &mut self.replay_recorder {
                let tick = self
                    .app
                    .world()
                    .get_resource::<FixedTime>()
                    .map_or(0, |time| time.step_count);
                recorder.record(tick, command);
            }
        if let Some(queue) = self.app.world_mut().get_resource_mut::<VirtualInputQueue>() {
            queue.push(source, command);
        }
    }

    /// Starts recording virtual input from the next fixed boundary.
    pub fn start_replay_recording(&mut self) -> Result<(), engine::ReplayError> {
        let fixed_step = self
            .app
            .world()
            .get_resource::<FixedTime>()
            .map_or(engine::time::FIXED_DELTA_SECONDS, |time| time.fixed_delta);
        self.replay_player = None;
        self.replay_recorder = Some(ReplayRecorder::new(fixed_step)?);
        Ok(())
    }

    /// Stops recording and returns the completed artifact.
    pub fn stop_replay_recording(&mut self) -> Option<InputReplay> {
        self.replay_recorder.take().map(ReplayRecorder::finish)
    }

    /// Returns whether the runtime is currently recording input.
    pub fn is_replay_recording(&self) -> bool {
        self.replay_recorder.is_some()
    }

    /// Starts deterministic playback and adopts the recorded fixed interval.
    pub fn start_replay(&mut self, replay: InputReplay) -> Result<(), engine::ReplayError> {
        let player = ReplayPlayer::new(replay)?;
        if let Some(time) = self.app.world_mut().get_resource_mut::<FixedTime>() {
            *time = FixedTime::with_delta(player.fixed_step_seconds());
        }
        self.replay_recorder = None;
        self.replay_player = Some(player);
        self.last_tick = Instant::now();
        Ok(())
    }

    /// Reports whether deterministic playback still has queued tick batches.
    pub fn is_replaying(&self) -> bool {
        self.replay_player
            .as_ref()
            .is_some_and(|player| !player.is_finished())
    }

    /// Polls the desktop controller adapter into the shared virtual queue.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn poll_gamepads(&mut self, context: &mut engine::gamepad::GilrsContext) {
        if let Some(queue) = self.app.world_mut().get_resource_mut::<VirtualInputQueue>() {
            context.poll(queue);
        }
    }

    /// Queues a focus-boundary release for the next runtime tick.
    ///
    /// Queuing instead of mutating immediately preserves the previous-frame
    /// analog snapshot, so action consumers observe `just_released` once.
    pub fn release_all_input(&mut self) {
        self.queue_input(InputSource::Human, InputCommand::ReleaseAll);
    }

    /// Returns the number of runtime entities.
    pub fn entity_count(&self) -> usize {
        self.app.world().entity_count()
    }

    /// Read-only component values for one runtime entity, formatted for the
    /// Runtime Debugger's value pane.
    ///
    /// Combined with Pause/Step this answers "where is it and what is it
    /// doing" without adding logging to game code.
    pub(crate) fn entity_component_values(
        &self,
        entity: engine::ecs::Entity,
    ) -> Vec<(String, String)> {
        let world = self.app.world();
        let format_vec3 =
            |value: engine::glam::Vec3| format!("{:.2}, {:.2}, {:.2}", value.x, value.y, value.z);
        let mut values = Vec::new();
        if let Some(transform) = world.get_component::<engine::Transform>(entity) {
            let (x, y, z) = transform.rotation.to_euler(engine::glam::EulerRot::XYZ);
            values.push(("Position".to_owned(), format_vec3(transform.translation)));
            values.push((
                "Rotation (deg)".to_owned(),
                format!(
                    "{:.1}, {:.1}, {:.1}",
                    x.to_degrees(),
                    y.to_degrees(),
                    z.to_degrees()
                ),
            ));
            values.push(("Scale".to_owned(), format_vec3(transform.scale)));
        }
        if let Some(global) = world.get_component::<engine::GlobalTransform>(entity) {
            let world_position = global.matrix().col(3).truncate();
            values.push(("World Position".to_owned(), format_vec3(world_position)));
        }
        if let Some(velocity) = world.get_component::<engine::Velocity>(entity) {
            values.push(("Velocity".to_owned(), format_vec3(velocity.linear)));
        }
        if let Some(animator) = world.get_component::<engine::Animator>(entity) {
            values.push((
                "Animator".to_owned(),
                format!(
                    "{:?} at {:.2}s x{:.2}{}",
                    animator.state,
                    animator.time,
                    animator.playback_speed,
                    if animator.looping { " (loop)" } else { "" }
                ),
            ));
        }
        values
    }

    /// Captures runtime hierarchy rows and commonly authored components.
    pub(crate) fn entity_debug_snapshot(&self) -> Vec<RuntimeEntityDebugSnapshot> {
        let world = self.app.world();
        let mut rows = world
            .entities()
            .map(|entity| {
                let identity = world.get_component::<engine::RuntimeEntityIdentity>(entity);
                let mut components = Vec::new();
                macro_rules! component {
                    ($type:ty, $label:literal) => {
                        if world.has_component::<$type>(entity) {
                            components.push($label);
                        }
                    };
                }
                component!(engine::Transform, "Transform");
                component!(engine::GlobalTransform, "GlobalTransform");
                component!(engine::Camera3D, "Camera3D");
                component!(engine::Collider, "Collider");
                component!(engine::PhysicsBody, "PhysicsBody");
                component!(engine::Velocity, "Velocity");
                component!(engine::KinematicCharacterController, "CharacterController");
                component!(engine::ScriptComponent, "Script");
                component!(engine::UiDocumentRef, "UiDocument");
                component!(engine::ParticleEmitter, "ParticleEmitter");
                component!(engine::Animator, "Animator");
                component!(engine::behavior_tree::BehaviorTreeRunner, "BehaviorTreeRunner");
                component!(engine::NavMeshAgent, "NavMeshAgent");
                component!(engine::DamageReceiver, "DamageReceiver");
                component!(engine::AttackHitbox, "AttackHitbox");
                component!(engine::AudioEmitter, "AudioEmitter");
                component!(engine::DirectionalLight, "DirectionalLight");
                component!(engine::Parent, "Parent");
                component!(engine::Children, "Children");
                let transform = world
                    .get_component::<engine::Transform>(entity)
                    .map(|transform| {
                        let (x, y, z) = transform.rotation.to_euler(engine::glam::EulerRot::XYZ);
                        (
                            transform.translation.to_array(),
                            [x.to_degrees(), y.to_degrees(), z.to_degrees()],
                            transform.scale.to_array(),
                        )
                    });
                RuntimeEntityDebugSnapshot {
                    entity,
                    name: identity
                        .map(|identity| identity.name.clone())
                        .unwrap_or_else(|| format!("runtime_entity_{}", entity.id())),
                    authoring_id: identity
                        .map(|identity| identity.authoring_id.as_str().to_owned()),
                    components,
                    transform,
                }
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| (row.entity.id(), row.entity.generation()));
        rows
    }

    /// Returns timing and population counters for the current Play session.
    pub(crate) fn performance_snapshot(&self) -> RuntimePerformanceSnapshot {
        RuntimePerformanceSnapshot {
            last_tick_ms: self.last_tick_duration.as_secs_f64() * 1_000.0,
            maximum_tick_ms: self.maximum_tick_duration.as_secs_f64() * 1_000.0,
            average_tick_ms: if self.ticks == 0 {
                0.0
            } else {
                self.total_tick_duration.as_secs_f64() * 1_000.0 / self.ticks as f64
            },
            entity_count: self.entity_count(),
            fixed_steps: self.fixed_step_count(),
        }
    }

    /// Captures current runtime input without mutating transition state.
    pub(crate) fn input_debug_snapshot(&self) -> RuntimeInputDebugSnapshot {
        let world = self.app.world();
        let keyboard =
            sorted_pressed_values(world.get_resource::<engine::Input<engine::KeyCode>>());
        let mouse_buttons =
            sorted_pressed_values(world.get_resource::<engine::Input<engine::MouseButton>>());
        let gamepad_buttons =
            sorted_pressed_values(world.get_resource::<engine::Input<engine::GamepadButton>>());
        let mut gamepad_axes = world
            .get_resource::<engine::GamepadAxisState>()
            .map(|axes| {
                axes.active_axes()
                    .map(|(gamepad, axis, value)| format!("pad {} {axis:?}: {value:.3}", gamepad.0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        gamepad_axes.sort();

        let (mut connected_gamepads, connection_generation, last_connection_change) = world
            .get_resource::<engine::GamepadConnectionState>()
            .map(|connections| {
                (
                    connections
                        .connected()
                        .map(|gamepad| gamepad.0)
                        .collect::<Vec<_>>(),
                    connections.generation(),
                    connections
                        .last_change()
                        .map(|(gamepad, connected)| (gamepad.0, connected)),
                )
            })
            .unwrap_or_default();
        connected_gamepads.sort_unstable();
        let actions = world
            .get_resource::<engine::InputActionMap>()
            .map(|map| map.resolved_actions(world))
            .unwrap_or_default();

        RuntimeInputDebugSnapshot {
            keyboard,
            mouse_buttons,
            gamepad_buttons,
            gamepad_axes,
            connected_gamepads,
            connection_generation,
            last_connection_change,
            actions,
        }
    }

    /// Captures Animator and Animation Graph state for one authoring entity.
    pub(crate) fn animation_debug_snapshot(
        &mut self,
        authoring_id: &EntityId,
    ) -> Option<RuntimeAnimationDebugSnapshot> {
        collect_animation_debug_snapshot(self.app.world_mut(), authoring_id)
    }

    /// Captures the actual Animation Graph controller for one runtime Entity.
    pub(crate) fn animation_graph_debug_snapshot(
        &mut self,
        key: (u32, u32),
    ) -> Option<RuntimeAnimationDebugSnapshot> {
        collect_animation_graph_debug_snapshot(self.app.world_mut(), key)
    }

    /// Captures one runtime Behavior Tree runner without mutating its execution state.
    pub(crate) fn behavior_tree_debug_snapshot(
        &self,
        key: (u32, u32),
    ) -> Option<engine::behavior_tree::BehaviorExecutionSnapshot> {
        let world = self.app.world();
        let entity = world
            .entities()
            .find(|entity| (entity.id(), entity.generation()) == key)?;
        world
            .get_component::<engine::behavior_tree::BehaviorTreeRunner>(entity)
            .map(engine::behavior_tree::BehaviorTreeRunner::snapshot)
    }

    /// Returns the first runtime entity that currently owns a Behavior Tree runner.
    pub(crate) fn first_behavior_tree_entity_key(&self) -> Option<(u32, u32)> {
        let world = self.app.world();
        world
            .entities()
            .find(|entity| {
                world.has_component::<engine::behavior_tree::BehaviorTreeRunner>(*entity)
            })
            .map(|entity| (entity.id(), entity.generation()))
    }

    /// Returns the number of authoring entities mapped into runtime entities.
    pub fn mapped_entity_count(&self) -> usize {
        self.mapped_entity_count
    }

    /// Returns the number of successful runtime ticks.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Returns elapsed play time in seconds.
    pub fn elapsed_seconds(&self) -> f32 {
        self.started_at.elapsed().as_secs_f32()
    }

    /// Runs all UI systems registered on the runtime app against the editor egui context.
    ///
    /// Call this once per egui frame after [`RuntimePlayState::render_game_view`] returns
    /// the game texture so that HUD widgets appear on top of the rendered scene.
    /// `viewport` carries both the rectangle the game image occupies and the
    /// render-target resolution it was drawn at, which differ whenever the Game
    /// View letterboxes or downscales the image (ADR 0090).
    pub fn run_ui_systems(&mut self, ctx: &egui::Context, viewport: engine::UiViewport) {
        // Idempotent: only the first call per play session rebuilds the font
        // atlas (see `App::install_ui_fonts`), so calling it every frame here
        // is safe and keeps CJK fonts installed as soon as UI documents run.
        self.app.install_ui_fonts(ctx);
        self.app.run_ui_systems(ctx, viewport);
    }

    /// Enables or disables debug line rendering in the runtime world.
    ///
    /// When disabled the render pipeline still clears accumulated lines each
    /// frame, so no stale lines accumulate across ticks.
    pub fn set_debug_draw_enabled(&mut self, enabled: bool) {
        if let Some(dl) = self.app.world_mut().get_resource_mut::<DebugLines>() {
            dl.enabled = enabled;
        }
    }

    /// Renders the runtime world into the Game View texture.
    pub fn render_game_view(
        &mut self,
        render_state: &egui_wgpu::RenderState,
        size: [u32; 2],
    ) -> Result<egui::TextureId, GameViewError> {
        let size = [size[0].max(1), size[1].max(1)];
        if let Some(vp) = self.app.world_mut().get_resource_mut::<ViewportSize>() {
            vp.width = size[0];
            vp.height = size[1];
        }
        let recreate = self
            .game_view
            .as_ref()
            .is_none_or(|game_view| game_view.size != size);
        if recreate {
            self.release_game_view(render_state);
            self.game_view = Some(RuntimeGameView::new(render_state, size, "Game View")?);
        }
        if self.renderer.is_none() {
            let renderer = pollster::block_on(engine::PreviewRenderer::new(
                &render_state.device,
                &render_state.queue,
                engine::PREVIEW_RENDER_FORMAT,
            ))
            .map_err(|source| GameViewError::Renderer {
                message: source.to_string(),
            })?;
            self.renderer = Some(renderer);
        }

        let Self {
            app,
            renderer,
            game_view,
            ..
        } = self;
        let renderer = renderer
            .as_mut()
            .expect("preview renderer must exist after creation");
        let game_view = game_view
            .as_mut()
            .expect("game view target must exist after creation");
        renderer
            .render_to_view(
                app.world_mut(),
                &render_state.device,
                &render_state.queue,
                &game_view.render_view,
                &game_view.depth_view,
            )
            .map_err(|source| GameViewError::Render {
                message: source.to_string(),
            })?;

        Ok(game_view.texture_id)
    }

    /// Renders the live Play world from an editor-owned camera.
    ///
    /// Unlike [`Self::render_game_view`], this method deliberately leaves the
    /// world's [`ViewportSize`] unchanged. Runtime UI, gameplay cameras, and
    /// screen-space queries therefore continue to observe only the Game View
    /// dimensions even when the editor panel has a different aspect ratio.
    pub(crate) fn render_scene_view(
        &mut self,
        render_state: &egui_wgpu::RenderState,
        size: [u32; 2],
        camera: &Camera3D,
        camera_transform: &engine::Transform,
    ) -> Result<egui::TextureId, GameViewError> {
        let size = [size[0].max(1), size[1].max(1)];
        let recreate = self
            .scene_view
            .as_ref()
            .is_none_or(|scene_view| scene_view.size != size);
        if recreate {
            if let Some(scene_view) = self.scene_view.take() {
                scene_view.release(render_state);
            }
            self.scene_view = Some(RuntimeGameView::new(render_state, size, "Play Scene View")?);
        }
        if self.renderer.is_none() {
            self.renderer = Some(
                pollster::block_on(engine::PreviewRenderer::new(
                    &render_state.device,
                    &render_state.queue,
                    engine::PREVIEW_RENDER_FORMAT,
                ))
                .map_err(|source| GameViewError::Renderer {
                    message: source.to_string(),
                })?,
            );
        }

        let scene_view = self
            .scene_view
            .as_mut()
            .expect("scene view target must exist after creation");
        self.renderer
            .as_mut()
            .expect("preview renderer must exist after creation")
            .render_to_view_with_camera(
                self.app.world_mut(),
                camera,
                camera_transform,
                &render_state.device,
                &render_state.queue,
                &scene_view.render_view,
                &scene_view.depth_view,
            )
            .map_err(|source| GameViewError::Render {
                message: source.to_string(),
            })?;
        Ok(scene_view.texture_id)
    }

    /// Returns live runtime-space picking bounds keyed by authoring entity.
    pub(crate) fn scene_view_pick_info(&self) -> Vec<(EntityId, engine::glam::Vec3, engine::glam::Vec3)> {
        self.entity_map
            .entities()
            .filter_map(|(authoring, runtime)| {
                let global = self.app.world().get_component::<GlobalTransform>(runtime)?;
                let world_matrix = global.matrix();
                let center = world_matrix.col(3).truncate();
                let half = self
                    .app
                    .world()
                    .get_component::<engine::Collider>(runtime)
                    .map(|collider| collider.world_aabb(global).half_extents)
                    .unwrap_or_else(|| {
                        let scale = engine::glam::Vec3::new(
                            world_matrix.x_axis.truncate().length(),
                            world_matrix.y_axis.truncate().length(),
                            world_matrix.z_axis.truncate().length(),
                        );
                        (scale * 0.5).max(engine::glam::Vec3::splat(0.15))
                    });
                Some((authoring.clone(), center, half))
            })
            .collect()
    }

    /// Captures the current Game View texture as raw RGBA8 pixels.
    pub fn capture_game_view(
        &self,
        render_state: &egui_wgpu::RenderState,
    ) -> Result<FrameCapture, GameViewError> {
        let game_view = self
            .game_view
            .as_ref()
            .ok_or(GameViewError::CaptureUnavailable)?;
        game_view.capture(render_state)
    }

    /// Releases the egui texture registration for the current Game View.
    pub fn release_game_view(&mut self, render_state: &egui_wgpu::RenderState) {
        if let Some(game_view) = self.game_view.take() {
            game_view.release(render_state);
        }
        if let Some(scene_view) = self.scene_view.take() {
            scene_view.release(render_state);
        }
    }

    /// Removes the Game View and returns its egui texture registration.
    ///
    /// Used when Play stops without a render state at hand; the caller must
    /// free the returned id once a render state is available again, otherwise
    /// the egui renderer retains the texture forever.
    pub fn take_view_textures(&mut self) -> Vec<egui::TextureId> {
        self.game_view
            .take()
            .into_iter()
            .chain(self.scene_view.take())
            .map(|view| view.texture_id)
            .collect()
    }
}

fn collect_animation_debug_snapshot(
    world: &mut engine::ecs::World,
    authoring_id: &EntityId,
) -> Option<RuntimeAnimationDebugSnapshot> {
    collect_animation_debug_snapshot_matching(world, |_, identity| {
        &identity.authoring_id == authoring_id
    })
    .or_else(|| collect_animator_debug_snapshot(world, authoring_id))
}

fn collect_animator_debug_snapshot(
    world: &mut engine::ecs::World,
    authoring_id: &EntityId,
) -> Option<RuntimeAnimationDebugSnapshot> {
    let recent_events = world
        .get_resource::<engine::AnimationEvents>()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    (
                        (event.entity.id(), event.entity.generation()),
                        format!("{} @ {:.3}s", event.name, event.clip_time),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let query = engine::Query::<(&engine::RuntimeEntityIdentity, &engine::Animator)>::new(world);
    query
        .iter()
        .find(|(_, (identity, _))| &identity.authoring_id == authoring_id)
        .map(|(entity, (identity, animator))| {
            let delta = animator.root_motion_delta();
            RuntimeAnimationDebugSnapshot {
                runtime_entity: (entity.id(), entity.generation()),
                authoring_entity: identity.authoring_id.clone(),
                playback_state: format!("{:?}", animator.state),
                clip_runtime_id: animator.clip.id().value(),
                clip_time: animator.time,
                playback_speed: animator.playback_speed,
                looping: animator.looping,
                root_motion_mode: format!("{:?}", animator.root_motion_mode),
                root_motion_delta: delta.to_array(),
                crossfade_progress: animator.crossfade_progress(),
                graph_state: None,
                graph_transition_sequence: 0,
                graph_last_transition: None,
                graph_parameters: Vec::new(),
                graph_parameter_values: Vec::new(),
                graph_asset: None,
                graph_id: None,
                animation_set_asset: None,
                current_state: None,
                previous_state: None,
                next_state: None,
                active_transition: None,
                transition_condition: None,
                motion_slot: None,
                motion_slot_name: None,
                motion_source: None,
                resolved_motion_variant: None,
                recent_events: recent_events
                    .iter()
                    .filter(|(key, _)| *key == (entity.id(), entity.generation()))
                    .map(|(_, event)| event.clone())
                    .collect(),
                runtime_error: None,
            }
        })
}

fn collect_animation_graph_debug_snapshot(
    world: &mut engine::ecs::World,
    runtime_key: (u32, u32),
) -> Option<RuntimeAnimationDebugSnapshot> {
    collect_animation_debug_snapshot_matching(world, |entity, _| {
        (entity.id(), entity.generation()) == runtime_key
    })
}

fn collect_animation_debug_snapshot_matching(
    world: &mut engine::ecs::World,
    matches: impl Fn(engine::ecs::Entity, &engine::RuntimeEntityIdentity) -> bool,
) -> Option<RuntimeAnimationDebugSnapshot> {
    let recent_events = world
        .get_resource::<engine::AnimationEvents>()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    (
                        (event.entity.id(), event.entity.generation()),
                        format!("{} @ {:.3}s", event.name, event.clip_time),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let query = engine::Query::<(
        &engine::RuntimeEntityIdentity,
        &engine::Animator,
        Option<&engine::AnimGraphPlayer>,
    )>::new(world);
    query
        .iter()
        .find(|(entity, (identity, _, graph))| graph.is_some() && matches(*entity, identity))
        .map(|(entity, (identity, animator, graph))| {
            let graph = graph.expect("filtered Animation Graph target must have a player");
            let state = graph.current_state_info();
            let transition = graph.last_transition();
            let fading = animator.crossfade_progress().is_some();
            let debug_source = graph.debug_source();
            let binding = state
                .and_then(|state| state.motion_slot.as_ref())
                .and_then(|slot| debug_source.and_then(|source| source.motion_bindings.get(slot)));
            let graph_state = state.map(|state| {
                state.motion_key().map(str::to_owned).unwrap_or_else(|| {
                    format!("node {} (no motion)", state.node_id.as_str())
                })
            });
            let graph_last_transition = transition.map(|transition| {
                let condition = if transition.condition.is_empty() {
                    "unconditional"
                } else {
                    transition.condition.as_str()
                };
                format!(
                    "{} -> {} ({condition})",
                    transition.from_node.as_str(),
                    transition.to_node.as_str()
                )
            });
            let graph_parameters = graph
                .parameters()
                .filter_map(|(name, value)| match value {
                    engine::AnimationParameterValue::Bool(value) => Some((name.to_owned(), value)),
                    engine::AnimationParameterValue::Float(_)
                    | engine::AnimationParameterValue::Trigger(_) => None,
                })
                .collect();
            let graph_parameter_values = graph
                .parameters()
                .map(|(name, value)| {
                    let value = match value {
                        engine::AnimationParameterValue::Bool(value) => value.to_string(),
                        engine::AnimationParameterValue::Float(value) => format!("{value:.3}"),
                        engine::AnimationParameterValue::Trigger(pending) => {
                            if pending { "trigger(pending)" } else { "trigger(idle)" }.to_owned()
                        }
                    };
                    (name.to_owned(), value)
                })
                .collect();
            let delta = animator.root_motion_delta();
            RuntimeAnimationDebugSnapshot {
                runtime_entity: (entity.id(), entity.generation()),
                authoring_entity: identity.authoring_id.clone(),
                playback_state: format!("{:?}", animator.state),
                clip_runtime_id: animator.clip.id().value(),
                clip_time: animator.time,
                playback_speed: animator.playback_speed,
                looping: animator.looping,
                root_motion_mode: format!("{:?}", animator.root_motion_mode),
                root_motion_delta: delta.to_array(),
                crossfade_progress: animator.crossfade_progress(),
                graph_state,
                graph_transition_sequence: graph.transition_sequence(),
                graph_last_transition,
                graph_parameters,
                graph_parameter_values,
                graph_asset: debug_source.map(|source| source.graph_asset.clone()),
                graph_id: debug_source.map(|source| source.graph_id.clone()),
                animation_set_asset: debug_source.map(|source| source.animation_set_asset.clone()),
                current_state: state.map(|state| state.node_id.clone()),
                previous_state: (fading)
                    .then(|| transition.map(|transition| transition.from_node.clone()))
                    .flatten(),
                next_state: (fading)
                    .then(|| transition.map(|transition| transition.to_node.clone()))
                    .flatten(),
                active_transition: (fading)
                    .then(|| graph.last_transition_edge().cloned())
                    .flatten(),
                transition_condition: transition.map(|transition| {
                    if transition.condition.is_empty() {
                        "unconditional".to_owned()
                    } else {
                        transition.condition.clone()
                    }
                }),
                motion_slot: state.and_then(|state| state.motion_slot.clone()),
                motion_slot_name: binding.map(|binding| binding.display_name.clone()),
                motion_source: binding.map(|binding| binding.source.clone()),
                resolved_motion_variant: binding.map(|binding| binding.resolved_variant),
                recent_events: recent_events
                    .iter()
                    .filter(|(key, _)| *key == (entity.id(), entity.generation()))
                    .map(|(_, event)| event.clone())
                    .collect(),
                runtime_error: debug_source
                    .is_none()
                    .then(|| "Animation Graph runtime source provenance is unavailable.".to_owned()),
            }
        })
}

fn sorted_pressed_values<T>(input: Option<&engine::Input<T>>) -> Vec<String>
where
    T: Copy + Eq + std::hash::Hash + std::fmt::Debug,
{
    let mut values = input
        .map(|input| {
            input
                .pressed_values()
                .map(|value| format!("{value:?}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    values.sort();
    values
}

/// Raw Game View frame capture in render-target physical pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameCapture {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Contiguous sRGB-encoded RGBA8 pixels, row-major, with no row padding.
    pub rgba8: Vec<u8>,
}

struct RuntimeGameView {
    color_texture: wgpu::Texture,
    /// sRGB view used as the render attachment so linear shader output is
    /// gamma-encoded on store, matching an sRGB window surface.
    render_view: wgpu::TextureView,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    texture_id: egui::TextureId,
    size: [u32; 2],
}

impl RuntimeGameView {
    fn new(
        render_state: &egui_wgpu::RenderState,
        size: [u32; 2],
        label: &str,
    ) -> Result<Self, GameViewError> {
        let device = &render_state.device;
        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Editor {label} color texture")),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: engine::PREVIEW_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[engine::PREVIEW_RENDER_FORMAT],
        });
        let render_view = color_texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(engine::PREVIEW_RENDER_FORMAT),
            ..Default::default()
        });
        // egui-wgpu samples registered textures as gamma-encoded data, and
        // capture copies raw texel bytes, so both go through the non-sRGB
        // default view.
        let sample_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Editor {label} depth texture")),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: engine::PREVIEW_MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: engine::PREVIEW_DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_id = render_state.renderer.write().register_native_texture(
            device,
            &sample_view,
            wgpu::FilterMode::Linear,
        );

        Ok(Self {
            color_texture,
            render_view,
            _depth_texture: depth_texture,
            depth_view,
            texture_id,
            size,
        })
    }

    fn release(self, render_state: &egui_wgpu::RenderState) {
        render_state
            .renderer
            .write()
            .free_texture(&self.texture_id);
    }

    fn capture(
        &self,
        render_state: &egui_wgpu::RenderState,
    ) -> Result<FrameCapture, GameViewError> {
        let width = self.size[0];
        let height = self.size[1];
        let unpadded_bytes_per_row = width * 4;
        let padded_bytes_per_row = align_to_copy_row(unpadded_bytes_per_row);
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let device = &render_state.device;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Editor Game View capture buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Editor Game View capture encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        render_state.queue.submit(Some(encoder.finish()));

        let buffer_slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|source| GameViewError::CapturePoll {
                message: source.to_string(),
            })?;
        receiver
            .recv()
            .map_err(|source| GameViewError::CaptureCallback {
                message: source.to_string(),
            })?
            .map_err(|source| GameViewError::CaptureMap {
                message: source.to_string(),
            })?;

        let mapped = buffer_slice.get_mapped_range();
        let mut rgba8 = Vec::with_capacity((width * height * 4) as usize);
        for padded_row in mapped.chunks(padded_bytes_per_row as usize) {
            rgba8.extend_from_slice(&padded_row[..unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        buffer.unmap();

        Ok(FrameCapture {
            width,
            height,
            rgba8,
        })
    }
}

fn align_to_copy_row(bytes_per_row: u32) -> u32 {
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let remainder = bytes_per_row % alignment;
    if remainder == 0 {
        bytes_per_row
    } else {
        bytes_per_row + alignment - remainder
    }
}

/// Errors that can prevent the Game View from rendering.
#[derive(Debug)]
pub enum GameViewError {
    /// Preview renderer setup failed.
    Renderer {
        /// Human-readable source error.
        message: String,
    },
    /// A Game View frame could not be rendered.
    Render {
        /// Human-readable source error.
        message: String,
    },
    /// No Game View frame has been rendered yet.
    CaptureUnavailable,
    /// Waiting for the capture copy failed.
    CapturePoll {
        /// Human-readable source error.
        message: String,
    },
    /// The capture callback did not return a result.
    CaptureCallback {
        /// Human-readable source error.
        message: String,
    },
    /// Mapping the capture buffer failed.
    CaptureMap {
        /// Human-readable source error.
        message: String,
    },
}

impl fmt::Display for GameViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renderer { message } | Self::Render { message } => formatter.write_str(message),
            Self::CaptureUnavailable => formatter.write_str("no Game View frame is available"),
            Self::CapturePoll { message }
            | Self::CaptureCallback { message }
            | Self::CaptureMap { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GameViewError {}

/// Runtime diagnostic categories surfaced in the editor Diagnostics panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDiagnosticKind {
    /// Play was requested without an open scene document.
    NoScene,
    /// Scene-to-runtime conversion failed.
    SceneConversionFailed,
    /// Play inserted a temporary camera because the scene had none.
    NoCamera,
    /// A referenced asset could not be resolved for runtime preview.
    MissingAsset {
        /// The unresolved authoring asset identifier.
        asset: AssetId,
    },
    /// Inserting the temporary default camera failed.
    DefaultCameraFailed,
    /// A required ECS system could not be registered at Play startup.
    SystemRegistrationFailed,
    /// Audio output could not be initialized, but Play can continue without sound.
    AudioUnavailable,
    /// A runtime schedule tick failed.
    TickFailed,
    /// A runtime schedule panicked.
    Panicked,
    /// Rendering the Game View failed.
    RenderError,
    /// Reloading the scene from disk failed.
    ReloadFailed,
}

impl RuntimeDiagnosticKind {
    /// Returns the stable diagnostic code for this runtime diagnostic.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoScene => "editor.runtime.no_scene",
            Self::SceneConversionFailed => "editor.runtime.scene_conversion_failed",
            Self::NoCamera => "editor.runtime.no_camera",
            Self::MissingAsset { .. } => "editor.runtime.missing_asset",
            Self::DefaultCameraFailed => "editor.runtime.default_camera_failed",
            Self::SystemRegistrationFailed => "editor.runtime.system_registration_failed",
            Self::AudioUnavailable => "editor.runtime.audio_unavailable",
            Self::TickFailed => "editor.runtime.tick_failed",
            Self::Panicked => "editor.runtime.panicked",
            Self::RenderError => "editor.runtime.render_error",
            Self::ReloadFailed => "editor.runtime.reload_failed",
        }
    }

    /// Converts this runtime diagnostic into an authoring diagnostic.
    pub fn to_diagnostic(&self, message: impl Into<String>) -> Diagnostic {
        let diagnostic = match self {
            Self::NoCamera | Self::AudioUnavailable => Diagnostic::warning(self.code(), message),
            _ => Diagnostic::error(self.code(), message),
        };

        match self {
            Self::MissingAsset { asset } => {
                diagnostic.with_target(DiagnosticTarget::Asset { id: asset.clone() })
            }
            _ => diagnostic,
        }
    }
}

/// Error returned when a scene reload fails.
///
/// On any variant, the previous runtime world remains unchanged.
#[derive(Debug)]
pub enum ReloadError {
    /// The scene file could not be read from disk.
    Io {
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The scene file contained invalid JSON.
    JsonParse {
        /// Human-readable parse error message.
        message: String,
    },
    /// Scene conversion or system registration failed.
    Play(PlayError),
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { source } => write!(f, "scene I/O error: {source}"),
            Self::JsonParse { message } => write!(f, "scene parse error: {message}"),
            Self::Play(err) => write!(f, "play error: {err}"),
        }
    }
}

impl ReloadError {
    /// Converts this reload error into editor diagnostics for display.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        match self {
            Self::Io { source } => vec![RuntimeDiagnosticKind::ReloadFailed
                .to_diagnostic(format!("failed to read scene file: {source}"))],
            Self::JsonParse { message } => vec![RuntimeDiagnosticKind::ReloadFailed
                .to_diagnostic(format!("scene parse error: {message}"))],
            Self::Play(err) => err.into_diagnostics(),
        }
    }
}

impl RuntimePlayState {
    /// Loads a scene from `scene_path` and builds a fresh runtime world.
    ///
    /// Returns a new [`PlayStart`] on success.  On failure the current world
    /// is unchanged and callers should surface the error diagnostics.
    ///
    /// # Errors
    ///
    /// - [`ReloadError::Io`] when the file cannot be read.
    /// - [`ReloadError::JsonParse`] when the file is not valid scene JSON.
    /// - [`ReloadError::Play`] when scene conversion or system registration fails.
    pub fn reload_from_path(
        scene_path: &Path,
        project: Option<&ProjectRoot>,
    ) -> Result<PlayStart, ReloadError> {
        let json = fs::read_to_string(scene_path).map_err(|source| ReloadError::Io { source })?;
        let scene = load_scene_from_json(&json).map_err(|source| ReloadError::JsonParse {
            message: source.to_string(),
        })?;
        Self::start_from_document(&scene, project, Some(scene_path)).map_err(ReloadError::Play)
    }

    /// Reloads a scene while retaining the current game-module generation.
    pub fn reload_from_path_with_game_module(
        scene_path: &Path,
        project: Option<&ProjectRoot>,
        game_module: Option<Arc<engine::game_module::GameModule>>,
    ) -> Result<PlayStart, ReloadError> {
        let json = fs::read_to_string(scene_path).map_err(|source| ReloadError::Io { source })?;
        let scene = load_scene_from_json(&json).map_err(|source| ReloadError::JsonParse {
            message: source.to_string(),
        })?;
        Self::start_from_document_with_game_module(&scene, project, Some(scene_path), game_module)
            .map_err(ReloadError::Play)
    }
}

/// Computes the project-relative scene path [`SceneLoader::load`] would
/// accept for `absolute_scene_path`, or `None` when `project` is absent or
/// `absolute_scene_path` does not live under the project's `assets/`
/// directory (for example, an unsaved document or one opened from outside
/// the project).
fn relative_scene_path(
    project: Option<&ProjectRoot>,
    absolute_scene_path: &Path,
) -> Option<String> {
    let project = project?;
    let relative = absolute_scene_path
        .strip_prefix(project.assets_root())
        .ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    if relative.is_empty() {
        None
    } else {
        Some(relative)
    }
}

fn load_project_asset_manifest(project: &ProjectRoot) -> (AssetManifest, Vec<Diagnostic>) {
    let path = project.path().join("asset_manifest.json");
    let json = match fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (AssetManifest::default(), Vec::new());
        }
        Err(error) => {
            return (
                AssetManifest::default(),
                vec![Diagnostic::error(
                    "editor.asset_manifest_load_failed",
                    format!("failed to read {}: {error}", path.display()),
                )],
            );
        }
    };

    match AssetManifest::from_json(&json) {
        Ok(manifest) => (manifest, Vec::new()),
        Err(error) => (
            AssetManifest::default(),
            vec![Diagnostic::error(
                "editor.asset_manifest_load_failed",
                format!("failed to parse {}: {error}", path.display()),
            )],
        ),
    }
}

/// Successful Play startup output.
pub struct PlayStart {
    /// Runtime state to retain until Stop.
    pub state: RuntimePlayState,
    /// Diagnostics produced while setting up Play.
    pub diagnostics: Vec<Diagnostic>,
}

/// Errors that can prevent Play from starting.
#[derive(Debug)]
pub enum PlayError {
    /// No scene document is open.
    NoScene,
    /// Authoring validation blocked conversion.
    InvalidScene {
        /// Diagnostics returned by scene validation.
        diagnostics: Vec<Diagnostic>,
    },
    /// Authoring-to-runtime conversion failed.
    SceneBridge(SceneBridgeError),
    /// Inserting the temporary default camera failed.
    DefaultCamera(engine::ecs::WorldError),
    /// Registering a required ECS system failed.
    SystemRegistration {
        /// The underlying build error.
        source: engine::ecs::SystemRegistrationError,
    },
    /// Current before/after constraints cannot produce a valid order.
    SystemOrdering(engine::ecs::ScheduleEditError),
}

impl fmt::Display for PlayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoScene => write!(formatter, "no scene document is open"),
            Self::InvalidScene { diagnostics } => write!(
                formatter,
                "scene validation blocked Play with {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::SceneBridge(source) => write!(formatter, "{source}"),
            Self::DefaultCamera(source) => {
                write!(
                    formatter,
                    "failed to insert default runtime camera: {source}"
                )
            }
            Self::SystemRegistration { source } => {
                write!(formatter, "failed to register runtime system: {source}")
            }
            Self::SystemOrdering(source) => {
                write!(
                    formatter,
                    "failed to resolve runtime system order: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PlayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SceneBridge(source) => Some(source),
            Self::DefaultCamera(source) => Some(source),
            Self::SystemRegistration { source } => Some(source),
            Self::SystemOrdering(source) => Some(source),
            Self::NoScene | Self::InvalidScene { .. } => None,
        }
    }
}

impl PlayError {
    /// Converts a Play startup error into diagnostics for the editor panel.
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        match self {
            Self::NoScene => {
                vec![RuntimeDiagnosticKind::NoScene.to_diagnostic("no scene document is open")]
            }
            Self::InvalidScene { mut diagnostics } => {
                diagnostics.push(
                    RuntimeDiagnosticKind::SceneConversionFailed
                        .to_diagnostic("scene validation blocked Play"),
                );
                diagnostics
            }
            Self::SceneBridge(source) => scene_bridge_error_diagnostics(source),
            Self::DefaultCamera(source) => vec![RuntimeDiagnosticKind::DefaultCameraFailed
                .to_diagnostic(format!("failed to insert default runtime camera: {source}"))],
            Self::SystemRegistration { source } => {
                vec![RuntimeDiagnosticKind::SystemRegistrationFailed
                    .to_diagnostic(format!("failed to register runtime system: {source}"))]
            }
            Self::SystemOrdering(source) => {
                vec![RuntimeDiagnosticKind::SystemRegistrationFailed
                    .to_diagnostic(format!("failed to resolve runtime system order: {source}"))]
            }
        }
    }
}

fn system_settings_diagnostics(report: engine::app::SystemSettingsApplyReport) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for id in report.invalid_ids {
        diagnostics.push(Diagnostic::warning(
            "editor.runtime.system_settings_invalid_id",
            format!("ignored invalid configured system ID `{id}`"),
        ));
    }
    for (schedule, items) in [
        ("Update", report.update),
        ("FixedUpdate", report.fixed_update),
    ] {
        for item in items {
            let message = match item {
                engine::ecs::ScheduleDiagnostic::UnknownConfiguredSystem(id) => {
                    format!("{schedule} settings reference removed system `{id}`")
                }
                engine::ecs::ScheduleDiagnostic::MissingConstraintTarget { system, target } => {
                    format!(
                        "{schedule} system `{system}` references missing constraint target `{target}`"
                    )
                }
                engine::ecs::ScheduleDiagnostic::MigratedAlias { from, to } => {
                    format!("{schedule} system ID `{from}` migrated to `{to}`")
                }
                engine::ecs::ScheduleDiagnostic::ConstraintAdjusted => {
                    format!("{schedule} saved order was adjusted to satisfy constraints")
                }
            };
            diagnostics.push(Diagnostic::warning(
                "editor.runtime.system_settings_diagnostic",
                message,
            ));
        }
    }
    diagnostics
}

fn scene_bridge_error_diagnostics(error: SceneBridgeError) -> Vec<Diagnostic> {
    match error {
        SceneBridgeError::InvalidScene { mut diagnostics } => {
            diagnostics.push(
                RuntimeDiagnosticKind::SceneConversionFailed
                    .to_diagnostic("authoring scene validation failed during runtime conversion"),
            );
            diagnostics
        }
        SceneBridgeError::InvalidComponentValue {
            entity,
            component_type,
            expected,
        } => vec![RuntimeDiagnosticKind::SceneConversionFailed
            .to_diagnostic(format!(
                "component `{}` on entity `{}` must be {expected}",
                component_type.as_str(),
                entity.as_str()
            ))
            .with_target(DiagnosticTarget::Component {
                entity,
                component_type,
            })],
        SceneBridgeError::MissingGameComponent {
            entity,
            component_type,
        } => vec![RuntimeDiagnosticKind::SceneConversionFailed
            .to_diagnostic(format!(
                "game component `{}` is unavailable; build the project game module before Play",
                component_type.as_str()
            ))
            .with_target(DiagnosticTarget::Component {
                entity,
                component_type,
            })],
        SceneBridgeError::GameModule { source } => {
            vec![RuntimeDiagnosticKind::SceneConversionFailed
                .to_diagnostic(format!("game module conversion failed: {source}"))]
        }
        SceneBridgeError::UnknownAsset { asset } => {
            vec![RuntimeDiagnosticKind::MissingAsset {
                asset: asset.clone(),
            }
            .to_diagnostic(format!(
                "authoring asset `{}` is not available for runtime preview",
                asset.as_str()
            ))]
        }
        SceneBridgeError::AssetLoad { asset, source } => {
            vec![RuntimeDiagnosticKind::MissingAsset {
                asset: asset.clone(),
            }
            .to_diagnostic(format!(
                "failed to load asset `{}`: {source}",
                asset.as_str()
            ))]
        }
        SceneBridgeError::WorldMutation {
            source,
            cleanup_errors,
        } => {
            let cleanup = if cleanup_errors.is_empty() {
                String::new()
            } else {
                format!(
                    " (bridge rollback also produced {} error(s))",
                    cleanup_errors.len()
                )
            };
            vec![RuntimeDiagnosticKind::SceneConversionFailed
                .to_diagnostic(format!("runtime world mutation failed: {source}{cleanup}"))]
        }
    }
}

/// Errors that can stop an active runtime tick.
#[derive(Debug)]
pub enum PlayTickError {
    /// The runtime schedule returned an error.
    Schedule {
        /// Source schedule error.
        source: engine::ecs::ScheduleError,
    },
    /// A project-local Rust system callback failed.
    GameModule(engine::game_module::GameModuleRunError),
    /// A persisted replay could not be decoded or injected.
    Replay(engine::ReplayError),
    /// A runtime system panicked.
    Panicked,
}

impl fmt::Display for PlayTickError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schedule { source } => write!(formatter, "{source}"),
            Self::GameModule(source) => write!(formatter, "{source}"),
            Self::Replay(source) => write!(formatter, "{source}"),
            Self::Panicked => write!(formatter, "runtime schedule panicked"),
        }
    }
}

impl std::error::Error for PlayTickError {}

fn has_camera(world: &mut engine::ecs::World) -> bool {
    let query = engine::ecs::Query::<&Camera3D>::new(world);
    query.iter().next().is_some()
}

fn insert_default_camera(world: &mut engine::ecs::World) -> Result<(), engine::ecs::WorldError> {
    let camera = world.spawn_with(engine::camera::default_camera_transform())?;
    world.add_component(camera, GlobalTransform::default())?;
    world.add_component(camera, Camera3D::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{KeyCode, PlayerMarker, Transform};
    use engine_authoring::load_scene_from_json;
    use engine_authoring::test_fixtures::{complete_scene_document, load_scene_fixture};

    const MINIMAL_SCENE: &str = include_str!("../../engine/assets/scenes/minimal.scene.json");
    const UNKNOWN_ASSET_SCENE: &str = r#"{
      "schema_version": 1,
      "entities": [
        {
          "id": "entity_01JP0000000000000000000001",
          "name": "player",
          "display_name": "Player",
          "description": "",
          "components": {
            "engine.transform": { "x": 0.0, "y": 0.0, "z": 0.0 },
            "engine.static_mesh_renderer": {
                "mesh": { "$type": "asset_ref", "id": "asset_01JP0000000000000000000999" },
                "material": { "$type": "asset_ref", "id": "asset_01JP0000000000000000000203" },
                "material_slots": []
            }
          }
        }
      ]
    }"#;

    const CAMERA_SCENE: &str = r#"{
      "entities": [
        {
          "id": "entity_01JP0000000000000000000001",
          "name": "camera",
          "components": {
            "engine.camera": {
              "enabled": true,
              "priority": 0,
              "fov_y_degrees": 60.0,
              "near": 0.1,
              "far": 1000.0
            }
          }
        }
      ]
    }"#;

    #[test]
    fn animation_debug_snapshot_matches_selected_authoring_identity() {
        let selected = EntityId::generate();
        let other = EntityId::generate();
        let mut clips = engine::Assets::<engine::AnimationClip>::new();
        let clip = clips.add(engine::AnimationClip {
            duration: 1.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let mut world = engine::ecs::World::new();
        let runtime = world
            .spawn_with(engine::RuntimeEntityIdentity {
                authoring_id: selected.clone(),
                name: "selected".to_owned(),
            })
            .expect("runtime entity");
        let mut animator = engine::Animator::playing(clip);
        animator.time = 0.25;
        animator.looping = true;
        assert!(animator.set_playback_speed(1.5));
        world
            .add_component(runtime, animator)
            .expect("animator component");

        assert!(collect_animation_debug_snapshot(&mut world, &other).is_none());
        let snapshot = collect_animation_debug_snapshot(&mut world, &selected)
            .expect("selected runtime animation state");

        assert_eq!(snapshot.playback_state, "Playing");
        assert_eq!(snapshot.clip_runtime_id, clip.id().value());
        assert_eq!(snapshot.clip_time, 0.25);
        assert_eq!(snapshot.playback_speed, 1.5);
        assert!(snapshot.looping);
        assert_eq!(snapshot.graph_state, None);
    }

    #[test]
    fn start_play_builds_runtime_world_without_mutating_scene() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let before = scene.to_canonical_json().expect("scene must serialize");

        let start = RuntimePlayState::start(&scene, None).expect("play must start");

        assert_eq!(
            scene.to_canonical_json().expect("scene must serialize"),
            before
        );
        assert_eq!(start.state.mapped_entity_count(), scene.entity_count());
        assert!(
            start.state.entity_count() >= scene.entity_count(),
            "default camera may add one runtime entity"
        );
        assert!(start
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.runtime.no_camera"));
    }

    #[test]
    fn tick_advances_runtime_state() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;
        let before_frame_count = state
            .app
            .world()
            .get_resource::<engine::time::Time>()
            .expect("runtime world must have Time")
            .frame_count;

        state.tick().expect("tick must succeed");

        assert_eq!(state.ticks(), 1);
        let after_frame_count = state
            .app
            .world()
            .get_resource::<engine::time::Time>()
            .expect("runtime world must have Time")
            .frame_count;
        assert_eq!(after_frame_count, before_frame_count + 1);
    }

    #[test]
    fn scene_camera_suppresses_temporary_default_camera() {
        let scene = load_scene_fixture(CAMERA_SCENE).expect("fixture must load");

        let start = RuntimePlayState::start(&scene, None).expect("play must start");

        assert_eq!(start.state.entity_count(), scene.entity_count());
        assert!(!start
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.runtime.no_camera"));
    }

    #[test]
    fn queued_virtual_input_is_visible_for_one_tick_transition() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;

        state.queue_input(
            InputSource::Test,
            InputCommand::Key {
                key: engine::KeyCode::KeyW,
                pressed: true,
            },
        );
        state.tick().expect("tick must succeed");

        let keyboard = state
            .app
            .world_mut()
            .get_resource_mut::<engine::Input<engine::KeyCode>>()
            .expect("runtime world must have keyboard input");
        assert!(keyboard.pressed(engine::KeyCode::KeyW));
        assert!(keyboard.just_pressed(engine::KeyCode::KeyW));

        state.tick().expect("second tick must succeed");

        let keyboard = state
            .app
            .world_mut()
            .get_resource_mut::<engine::Input<engine::KeyCode>>()
            .expect("runtime world must have keyboard input");
        assert!(keyboard.pressed(engine::KeyCode::KeyW));
        assert!(!keyboard.just_pressed(engine::KeyCode::KeyW));
    }

    #[test]
    fn focus_release_is_visible_on_the_next_runtime_tick() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;
        state.queue_input(
            InputSource::Test,
            InputCommand::Key {
                key: engine::KeyCode::KeyW,
                pressed: true,
            },
        );
        state.tick().expect("press tick must succeed");

        state.release_all_input();
        state.tick().expect("release tick must succeed");

        let keyboard = state
            .app
            .world()
            .get_resource::<engine::Input<engine::KeyCode>>()
            .expect("runtime world must have keyboard input");
        assert!(!keyboard.pressed(engine::KeyCode::KeyW));
        assert!(keyboard.just_released(engine::KeyCode::KeyW));
    }

    #[test]
    fn input_debug_snapshot_reports_physical_and_resolved_state() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;
        let settings = ProjectSettings {
            input_actions: vec![engine_authoring::InputAction {
                name: "debug_action".to_owned(),
                keys: vec!["KeyW".to_owned()],
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            }],
            ..ProjectSettings::default()
        };
        let (map, diagnostics) = engine::InputActionMap::from_project_settings(&settings);
        assert!(diagnostics.is_empty());
        state.app.world_mut().insert_resource(map);
        state.queue_input(
            InputSource::Test,
            InputCommand::Key {
                key: engine::KeyCode::KeyW,
                pressed: true,
            },
        );
        state.queue_input(
            InputSource::Test,
            InputCommand::GamepadConnected {
                gamepad: engine::GamepadId(3),
            },
        );
        state.queue_input(
            InputSource::Test,
            InputCommand::GamepadAxis {
                gamepad: engine::GamepadId(3),
                axis: engine::GamepadAxis::LeftStickX,
                value: 0.25,
            },
        );
        state.tick().expect("debug input tick must succeed");

        let snapshot = state.input_debug_snapshot();
        assert_eq!(snapshot.keyboard, ["KeyW"]);
        assert_eq!(snapshot.connected_gamepads, [3]);
        assert_eq!(snapshot.gamepad_axes, ["pad 3 LeftStickX: 0.250"]);
        assert_eq!(snapshot.actions.len(), 1);
        assert_eq!(snapshot.actions[0].0, "debug_action");
        assert!(snapshot.actions[0].1.just_pressed);
    }

    #[test]
    fn queued_mouse_motion_is_published_on_same_tick() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;

        state.queue_input(
            InputSource::Test,
            InputCommand::MouseDelta { delta: (5.0, -4.0) },
        );
        state.queue_input(InputSource::Test, InputCommand::MouseScroll { amount: 2.0 });
        state.tick().expect("tick must succeed");

        let mouse = state
            .app
            .world_mut()
            .get_resource_mut::<engine::MouseInput>()
            .expect("runtime world must have mouse input");
        assert_eq!(mouse.delta, (5.0, -4.0));
        assert_eq!(mouse.scroll, 2.0);
    }

    #[test]
    fn tick_clamps_delta_after_editor_stall() {
        let scene = load_scene_from_json(MINIMAL_SCENE).expect("fixture must load");
        let mut state = RuntimePlayState::start(&scene, None)
            .expect("play must start")
            .state;
        state.last_tick = Instant::now() - std::time::Duration::from_secs(5);

        state.tick().expect("tick must succeed");

        let time = state
            .app
            .world_mut()
            .get_resource_mut::<engine::time::Time>()
            .expect("runtime world must have a Time resource");
        assert!(
            time.delta_seconds <= engine::time::MAX_DELTA_SECONDS,
            "stalled editor must not produce a runaway delta: {}",
            time.delta_seconds
        );
    }

    #[test]
    fn no_scene_play_error_maps_to_runtime_diagnostic() {
        let diagnostics = PlayError::NoScene.into_diagnostics();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "editor.runtime.no_scene");
        assert!(diagnostics[0].is_blocking());
    }

    #[test]
    fn unknown_asset_emits_unregistered_file_warning_and_play_continues() {
        let scene = load_scene_fixture(UNKNOWN_ASSET_SCENE).expect("fixture must load");
        let start =
            RuntimePlayState::start(&scene, None).expect("unknown asset must not block Play");

        let diagnostic = start
            .diagnostics
            .iter()
            .find(|d| d.code == "asset.unregistered_file")
            .expect("unregistered asset must emit asset.unregistered_file diagnostic");

        assert!(
            !diagnostic.is_blocking(),
            "unregistered asset must be a warning, not an error"
        );
        assert!(matches!(
            &diagnostic.target,
            Some(DiagnosticTarget::Asset { id })
                if id.as_str() == "asset_01JP0000000000000000000999"
        ));
        let world = start.state.app.world();
        let mesh_entity = world
            .entities()
            .find(|entity| world.has_component::<engine::Handle<engine::Mesh>>(*entity))
            .expect("fallback mesh entity must exist");
        let handle = *world
            .get_component::<engine::Handle<engine::Mesh>>(mesh_entity)
            .expect("fallback mesh handle must exist");
        let mesh = world
            .get_resource::<engine::Assets<engine::Mesh>>()
            .and_then(|meshes| meshes.get(&handle))
            .expect("fallback mesh handle must resolve");
        assert_eq!(
            mesh.vertices.len(),
            24,
            "unregistered assets must use the visible unit-cube fallback"
        );
    }

    #[test]
    fn start_play_with_project_loads_manifest_obj_mesh() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "RuntimeAssetTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project create must succeed");
        std::fs::write(
            project.meshes_dir().join("external.obj"),
            "v 0.0 0.0 0.0\n\
             v 1.0 0.0 0.0\n\
             v 0.0 1.0 0.0\n\
             v 0.0 0.0 1.0\n\
             f 1 2 3\n\
             f 1 3 4\n",
        )
        .expect("OBJ write must succeed");
        // Test fixture: plain write is fine here; production manifest saves use replace_file_contents.
        std::fs::write(
            project.path().join("asset_manifest.json"),
            r#"{
  "schema_version": 2,
  "assets": {
    "asset_01JP0000000000000000000999": {
      "path": "meshes/external.obj",
      "name": "external"
    }
  }
}"#,
        )
        .expect("manifest write must succeed");
        let scene = load_scene_fixture(UNKNOWN_ASSET_SCENE).expect("fixture must load");

        let start =
            RuntimePlayState::start(&scene, Some(&project)).expect("project Play must start");

        assert!(
            !start
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "asset.unregistered_file"),
            "manifest-registered mesh must not fall back as unregistered"
        );
        let world = start.state.app.world();
        let mesh_entity = world
            .entities()
            .find(|entity| world.has_component::<engine::Handle<engine::Mesh>>(*entity))
            .expect("runtime mesh entity must exist");
        let handle = *world
            .get_component::<engine::Handle<engine::Mesh>>(mesh_entity)
            .expect("mesh entity must carry a mesh handle");
        let meshes = world
            .get_resource::<engine::Assets<engine::Mesh>>()
            .expect("runtime world must contain mesh assets");
        let mesh = meshes.get(&handle).expect("mesh handle must resolve");
        assert!(
            mesh.vertices.len() > 3,
            "external OBJ should load instead of the 3-vertex fallback triangle"
        );
    }

    #[test]
    fn start_play_with_project_reports_missing_manifest_file_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "MissingRuntimeAssetTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project create must succeed");
        // Test fixture: plain write is fine here; production manifest saves use replace_file_contents.
        std::fs::write(
            project.path().join("asset_manifest.json"),
            r#"{
  "schema_version": 2,
  "assets": {
    "asset_01JP0000000000000000000999": {
      "path": "meshes/missing.obj",
      "name": "missing"
    }
  }
}"#,
        )
        .expect("manifest write must succeed");
        let scene = load_scene_fixture(UNKNOWN_ASSET_SCENE).expect("fixture must load");

        let start = RuntimePlayState::start(&scene, Some(&project))
            .expect("missing asset file must not block Play");

        assert!(
            start
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "asset.missing_file"),
            "registered missing file must emit asset.missing_file"
        );
        assert!(
            !start
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "asset.unregistered_file"),
            "registered missing file must not be reported as unregistered"
        );
        let world = start.state.app.world();
        let mesh_entity = world
            .entities()
            .find(|entity| world.has_component::<engine::Handle<engine::Mesh>>(*entity))
            .expect("fallback mesh entity must still exist");
        let handle = *world
            .get_component::<engine::Handle<engine::Mesh>>(mesh_entity)
            .expect("mesh entity must carry a mesh handle");
        let meshes = world
            .get_resource::<engine::Assets<engine::Mesh>>()
            .expect("runtime world must contain mesh assets");
        let mesh = meshes.get(&handle).expect("mesh handle must resolve");
        assert_eq!(
            mesh.vertices.len(),
            24,
            "missing file should use the visible unit-cube fallback"
        );
    }

    #[test]
    fn reload_from_path_produces_fresh_runtime_world() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(
            &path,
            complete_scene_document(
                r#"{"entities":[
                  {"id":"entity_01JP0000000000000000000001","name":"a","components":{}}
                ]}"#,
            )
            .expect("fixture must be valid JSON"),
        )
        .unwrap();
        let scene = load_scene_from_json(&std::fs::read_to_string(&path).unwrap())
            .expect("fixture must load");

        let initial = RuntimePlayState::start(&scene, None)
            .expect("initial play must start")
            .state;
        assert_eq!(initial.mapped_entity_count(), 1);

        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let reloaded = RuntimePlayState::reload_from_path(&path, None)
            .expect("reload must succeed")
            .state;
        assert_eq!(
            reloaded.mapped_entity_count(),
            0,
            "reloaded world must reflect the updated scene"
        );
    }

    #[test]
    fn reload_from_path_fails_on_missing_file_and_emits_diagnostic() {
        let path = std::env::temp_dir().join("nonexistent_reload_test_12345.scene.json");
        let _ = std::fs::remove_file(&path);
        let result = RuntimePlayState::reload_from_path(&path, None);
        let err = match result {
            Err(e @ ReloadError::Io { .. }) => e,
            Err(e) => panic!("expected ReloadError::Io, got: {e:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        };
        let diagnostics = err.into_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "editor.runtime.reload_failed");
    }

    #[test]
    fn reload_from_path_fails_on_invalid_json_and_emits_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.scene.json");
        std::fs::write(&path, "not valid json").unwrap();
        let result = RuntimePlayState::reload_from_path(&path, None);
        let err = match result {
            Err(e @ ReloadError::JsonParse { .. }) => e,
            Err(e) => panic!("expected ReloadError::JsonParse, got: {e:?}"),
            Ok(_) => panic!("expected Err, got Ok"),
        };
        let diagnostics = err.into_diagnostics();
        assert_eq!(diagnostics[0].code, "editor.runtime.reload_failed");
    }

    #[test]
    fn camera_fov_edit_reflects_in_play() {
        const SCENE: &str = r#"{
          "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "camera",
            "components": {
              "engine.camera": {
                "enabled": true,
                "priority": 0,
                "fov_y_degrees": 45.0,
                "near": 0.1,
                "far": 500.0
              }
            }
          }]
        }"#;
        let scene = load_scene_fixture(SCENE).expect("fixture must load");
        let start = RuntimePlayState::start(&scene, None).expect("play must start");
        let world = start.state.app.world();
        let camera_entity = world
            .entities()
            .find(|e| world.has_component::<engine::Camera3D>(*e))
            .expect("camera entity must exist");
        let camera = world
            .get_component::<engine::Camera3D>(camera_entity)
            .expect("camera entity must have Camera3D");
        assert!(
            (camera.fov_y_radians.to_degrees() - 45.0).abs() < 0.01,
            "authored fov_y_degrees=45 must produce matching fov_y_radians; got {}",
            camera.fov_y_radians.to_degrees()
        );
    }

    #[test]
    fn directional_light_edit_reflects_in_play() {
        const SCENE: &str = r#"{
          "schema_version": 1,
          "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "light",
            "components": {
              "engine.directional_light": {
                "direction_x": 0.0,
                "direction_y": -1.0,
                "direction_z": 0.0,
                "color_r": 1.0,
                "color_g": 0.8,
                "color_b": 0.6,
                "intensity": 2.5
              }
            }
          }]
        }"#;
        let scene = load_scene_fixture(SCENE).expect("fixture must load");
        let start = RuntimePlayState::start(&scene, None).expect("play must start");
        let world = start.state.app.world();
        let light_entity = world
            .entities()
            .find(|e| world.has_component::<engine::DirectionalLight>(*e))
            .expect("directional light entity must exist");
        let light = world
            .get_component::<engine::DirectionalLight>(light_entity)
            .expect("light entity must have DirectionalLight");
        assert!(
            (light.intensity - 2.5).abs() < f32::EPSILON,
            "authored intensity=2.5 must be reflected in runtime; got {}",
            light.intensity
        );
        assert!(
            (light.color.y - 0.8).abs() < 0.001,
            "authored color_g=0.8 must be reflected; got {}",
            light.color.y
        );
    }

    #[test]
    fn scene_presentation_settings_reflect_in_editor_play_resources() {
        const SCENE: &str = r#"{
          "schema_version": 1,
          "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "presentation",
            "components": {
              "engine.shadow_settings": {
                "enabled": false,
                "cascade_near_split": 0.25,
                "cascade_far_split": 0.95,
                "depth_bias": 0.001,
                "normal_bias": 0.02
              },
              "engine.environment_lighting": {
                "diffuse_ibl_enabled": true,
                "color_r": 0.4,
                "color_g": 0.5,
                "color_b": 0.6,
                "intensity": 1.75
              },
              "engine.post_process": {
                "enabled": true,
                "exposure": 1.4,
                "tone_map": "reinhard",
                "bloom_enabled": true,
                "bloom_threshold": 0.9,
                "bloom_intensity": 0.3,
                "bloom_radius": 5.0
              }
            }
          }]
        }"#;
        let scene = load_scene_fixture(SCENE).expect("fixture must load");
        let mut start = RuntimePlayState::start(&scene, None).expect("play must start");
        start.state.tick().expect("runtime tick must succeed");
        let world = start.state.app.world();

        assert!(
            !world
                .get_resource::<engine::ShadowSettings>()
                .expect("shadow resource must exist")
                .enabled
        );
        assert_eq!(
            world
                .get_resource::<engine::EnvironmentLighting>()
                .expect("environment resource must exist")
                .intensity,
            1.75
        );
        let post_process = world
            .get_resource::<engine::PostProcessSettings>()
            .expect("post-process resource must exist");
        assert_eq!(post_process.exposure, 1.4);
        assert_eq!(post_process.tone_map, engine::ToneMapOperator::Reinhard);
        assert!(post_process.bloom.enabled);
    }

    #[test]
    fn material_edit_reflects_in_play() {
        const SCENE: &str = r#"{
          "schema_version": 1,
          "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "mesh_entity",
            "components": {
              "engine.transform": { "x": 0.0, "y": 0.0, "z": 0.0 },
              "engine.static_mesh_renderer": {
                "mesh": { "$type": "asset_ref", "id": "asset_01JP0000000000000000000101" },
                "material": { "$type": "asset_ref", "id": "asset_01JP0000000000000000000202" },
                "material_slots": []
              }
            }
          }]
        }"#;
        let scene = load_scene_fixture(SCENE).expect("fixture must load");
        let start = RuntimePlayState::start(&scene, None).expect("play must start");
        let world = start.state.app.world();
        let material_entity = world
            .entities()
            .find(|e| world.has_component::<engine::Material>(*e))
            .expect("material entity must exist");
        let material = world
            .get_component::<engine::Material>(material_entity)
            .expect("material entity must have Material");
        // Built-in orange material: color(0.9, 0.4, 0.1)
        assert!(
            (material.color[0] - 0.9).abs() < 0.01,
            "orange material red channel must be ~0.9; got {}",
            material.color[0]
        );
        assert!(
            (material.color[1] - 0.4).abs() < 0.01,
            "orange material green channel must be ~0.4; got {}",
            material.color[1]
        );
    }

    #[test]
    fn player_controller_moves_entity_from_input_command() {
        const SCENE: &str = r#"{
          "schema_version": 1,
          "entities": [{
            "id": "entity_01JP0000000000000000000001",
            "name": "player",
            "components": {
              "engine.player_marker": {},
              "engine.transform": {"x": 0.0, "y": 0.0, "z": 0.0},
              "engine.player_controller": {"move_speed": 10.0, "move_plane": "xz"},
              "engine.character_controller": {
                "gravity_scale": 0.0,
                "max_resolve_iterations": 3
              },
              "engine.collider": {
                "shape": "aabb",
                "half_extent_x": 0.5,
                "half_extent_y": 0.5,
                "half_extent_z": 0.5,
                "radius": 0.5,
                "half_height": 0.5,
                "is_trigger": false,
                "membership": 1,
                "mask": 4294967295
              },
              "engine.physics_body": {"kind": "kinematic"}
            }
          }]
        }"#;
        let scene = load_scene_fixture(SCENE).expect("fixture must load");
        let PlayStart { mut state, .. } =
            RuntimePlayState::start(&scene, None).expect("play must start");

        // Advance time by 1 second so the movement is large enough to measure.
        if let Some(t) = state
            .app
            .world_mut()
            .get_resource_mut::<engine::time::Time>()
        {
            t.advance(1.0);
        }

        // Queue a W key press and drain it into the Input resource.
        state.queue_input(
            InputSource::Test,
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            },
        );
        engine::drain_virtual_input(state.app.world_mut());

        // Update captures logical input without writing Transform directly.
        state.app.ecs_mut().update().expect("update");
        state
            .app
            .world_mut()
            .get_resource_mut::<FixedTime>()
            .expect("fixed time")
            .begin_step();
        state
            .app
            .ecs_mut()
            .run_fixed_update()
            .expect("fixed update");

        // W on the Xz plane decreases Z.
        let world = state.app.world();
        let player_entity = world
            .entities()
            .find(|e| world.has_component::<PlayerMarker>(*e))
            .expect("player entity must exist");
        let transform = world
            .get_component::<Transform>(player_entity)
            .expect("player entity must have Transform");
        assert!(
            transform.translation.z < 0.0,
            "W key on Xz plane must decrease Z; got {}",
            transform.translation.z
        );
    }

    #[test]
    fn orbit_camera_added_from_schema_default_converts_to_runtime() {
        use engine::builtin_registry;
        use engine_authoring::id::ComponentTypeId;
        let registry = builtin_registry();
        let orbit_def = registry
            .get(&ComponentTypeId::new("engine.orbit_camera"))
            .expect("orbit_camera must be registered");
        let default_value = orbit_def.schema.default_value();
        let scene_json = format!(
            r#"{{
              "schema_version": 1,
              "entities": [{{
                "id": "entity_01JP0000000000000000000001",
                "name": "cam",
                "components": {{
                  "engine.orbit_camera": {}
                }}
              }}]
            }}"#,
            serde_json::to_string(&default_value).unwrap()
        );
        let scene = load_scene_fixture(&scene_json).expect("fixture must load");
        let start = RuntimePlayState::start(&scene, None).expect("play must start");
        let world = start.state.app.world();
        let cam_entity = world
            .entities()
            .find(|e| world.has_component::<engine::OrbitCamera>(*e))
            .expect("orbit camera entity must exist");
        let orbit = world
            .get_component::<engine::OrbitCamera>(cam_entity)
            .expect("entity must have OrbitCamera");
        assert!(orbit.distance > 0.0, "default distance must be positive");
    }

    #[test]
    fn play_scene_switch_replaces_the_initial_scene_on_the_next_tick() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectRoot::create(
            dir.path(),
            engine_authoring::ProjectConfig {
                name: "PlaySceneSwitchTest".into(),
                schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
            },
        )
        .expect("project create must succeed");
        // Scene A's entity carries a PlayerMarker so the test can identify it
        // by component instead of relying on `World::entities()` iteration
        // order, which is explicitly documented as unstable.
        std::fs::write(
            project.scenes_dir().join("a.scene.json"),
            complete_scene_document(
                r#"{"entities":[
                  {"id":"entity_01JP0000000000000000000001","name":"a","components":{
                    "engine.player_marker": {}
                  }}
                ]}"#,
            )
            .expect("scene A fixture must be valid JSON"),
        )
        .expect("scene A write must succeed");
        std::fs::write(
            project.scenes_dir().join("b.scene.json"),
            complete_scene_document(
                r#"{"entities":[
                  {"id":"entity_01JP0000000000000000000002","name":"b","components":{}}
                ]}"#,
            )
            .expect("scene B fixture must be valid JSON"),
        )
        .expect("scene B write must succeed");
        let scene_a = load_scene_from_json(
            &std::fs::read_to_string(project.scenes_dir().join("a.scene.json")).unwrap(),
        )
        .expect("scene A fixture must parse");

        let start = RuntimePlayState::start_with_scene_path(
            &scene_a,
            Some(&project),
            "scenes/a.scene.json",
        )
        .expect("play must start with a registered initial scene");
        let mut state = start.state;

        assert!(
            state
                .app
                .world()
                .entities()
                .any(|entity| state.app.world().has_component::<PlayerMarker>(entity)),
            "scene A's marked entity must exist right after start"
        );

        state
            .app
            .world_mut()
            .get_resource_mut::<engine::SceneManager>()
            .expect("SceneManager must be installed by start_with_scene_path")
            .request_switch("scenes/b.scene.json");

        state.tick().expect("tick must succeed");

        let world = state.app.world();
        assert!(
            !world
                .entities()
                .any(|entity| world.has_component::<PlayerMarker>(entity)),
            "scene A's entity must be despawned once the switch to scene B runs"
        );
        assert_eq!(
            world
                .get_resource::<engine::SceneManager>()
                .unwrap()
                .generation(),
            1
        );
        assert_eq!(
            world
                .get_resource::<engine::SceneManager>()
                .unwrap()
                .current_scene_path(),
            Some("scenes/b.scene.json")
        );
        assert!(matches!(
            world.get_resource::<engine::SceneSwitchState>().unwrap(),
            engine::SceneSwitchState::Idle
        ));
    }
}
