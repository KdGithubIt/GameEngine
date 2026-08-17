use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use egui_winit::State as EguiWinitState;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes},
};

use crate::asset::{AssetServer, Assets};
use crate::audio::{
    game_audio_effect_system, AudioAsset, GameAudioCommandQueue, SpatialAudioRuntime,
};
use crate::camera::{camera_aspect_system, ViewportSize};
use crate::game_module::{
    GameComponentDefaults, GameModule, GameModuleResource, GameModuleRunError, GameSystemSchedule,
};
use crate::game_prefab::{process_game_prefab_requests, GamePrefabEvents, GamePrefabSpawnQueue};
use crate::game_timer::{game_timer_update_system, GameTimerEvents, GameTimers};
use crate::input::{
    GamepadAxisState, GamepadButton, GamepadConnectionState, Input, KeyCode, MouseButton,
    MouseInput, VirtualInputQueue,
};
use crate::light::{light_resource_mirror_system, AmbientLight, DirectionalLight};
use crate::lock_on::TargetLock;
use crate::lod::{lod_selection_system, InstanceStats};
use crate::material::{Material, Texture};
use crate::mesh::{GpuMeshCache, Mesh};
use crate::particles::particle_update_system;
use crate::vfx::vfx_update_system;
use crate::physics::{GameplayPhysicsWorld, Gravity};
use crate::player::PlayerMovementIntents;
use crate::postprocess::PostProcessSettings;
use crate::render::{
    RenderFrameError, RenderStateError, ToneMapPass, WorldRenderer, MAIN_PASS_SAMPLE_COUNT,
};
use crate::save::{game_save_effect_system, GameSaveCommandQueue, SaveData};
use crate::scene_manager::{process_scene_switch, SceneManager, SceneSwitchState};
use crate::script_api::{
    process_script_world_commands, script_animation_effect_system, script_api_collect_system,
    script_api_command_dispatch_system, script_api_snapshot_system, script_audio_effect_system,
    script_core_effect_system, PendingScriptEffects, ScriptCommandQueue, ScriptEventBus,
    ScriptTimers, ScriptWorldCommandQueue,
};
use crate::shadow::{presentation_resource_mirror_system, EnvironmentLighting, ShadowSettings};
use crate::morph::{material_morph_system, morph_blend_system};
use crate::skinning::joint_palette_system;
use crate::time::{FixedTime, Time};
use crate::transform::transform_propagation_system;
use crate::ui::{UiContext, UiSystem, UiViewport};
use crate::ui_document::{
    ui_event_relay_system, ui_script_event_system, UiBindings, UiDocumentDrawOptions,
    UiDocumentInstanceDrawReport, UiDocumentRef, UiEventFrame, UiEvents, UiReloadTimer,
    UiRuntimeDiagnostics,
};
use engine_authoring::project_settings::{
    ProjectSystemSchedule, SystemScheduleSettings, SystemSettings,
};
use engine_ecs::App as EcsApp;
use engine_ecs::{
    IntoSystem, ScheduleConfiguration, ScheduleDiagnostic, ScheduleEditError, ScheduleEntryInfo,
    SystemDescriptor, SystemId, SystemOrigin, SystemRegistrationError,
};
use engine_renderer::{
    GpuContext, GpuContextError, UnconfiguredWindowSurface, WindowSurface, WindowSurfaceError,
};

/// Builds and runs a windowed game application.
pub struct App {
    ecs: EcsApp,
    title: String,
    width: u32,
    height: u32,
    ui_systems: Vec<Box<dyn UiSystem>>,
    pending_ui_fonts: Vec<(String, Vec<u8>)>,
    ui_fonts_installed: bool,
}

/// Diagnostics produced while applying project system settings.
#[derive(Debug, Clone)]
pub struct SystemSettingsApplyReport {
    /// Non-fatal merge diagnostics from the per-frame schedule.
    pub update: Vec<ScheduleDiagnostic>,
    /// Non-fatal merge diagnostics from the fixed-timestep schedule.
    pub fixed_update: Vec<ScheduleDiagnostic>,
    /// Malformed persisted IDs ignored before schedule merging.
    pub invalid_ids: Vec<String>,
}

impl App {
    /// Creates an application with built-in timing, input, asset, and camera systems.
    pub fn new() -> Self {
        let mut ecs = EcsApp::new();
        ecs.insert_resource(Time::default());
        ecs.insert_resource(ViewportSize::default());
        ecs.insert_resource(Assets::<Mesh>::default());
        ecs.insert_resource(Assets::<Material>::default());
        ecs.insert_resource(Assets::<AudioAsset>::default());
        ecs.insert_resource(GameAudioCommandQueue::default());
        ecs.insert_resource(SpatialAudioRuntime::default());
        ecs.insert_resource(Assets::<std::sync::Arc<Texture>>::default());
        ecs.insert_resource(AssetServer::default());
        ecs.insert_resource(AmbientLight::default());
        ecs.insert_resource(DirectionalLight::default());
        ecs.insert_resource(ShadowSettings::default());
        ecs.insert_resource(EnvironmentLighting::default());
        ecs.insert_resource(Input::<KeyCode>::default());
        ecs.insert_resource(Input::<MouseButton>::default());
        ecs.insert_resource(MouseInput::default());
        ecs.insert_resource(VirtualInputQueue::default());
        ecs.insert_resource(Input::<GamepadButton>::default());
        ecs.insert_resource(GamepadAxisState::default());
        ecs.insert_resource(GamepadConnectionState::default());
        ecs.insert_resource(PlayerMovementIntents::default());
        ecs.insert_resource(PostProcessSettings::default());
        ecs.insert_resource(GpuMeshCache::default());
        ecs.insert_resource(InstanceStats::default());
        ecs.insert_resource(FixedTime::default());
        ecs.insert_resource(Gravity::default());
        ecs.insert_resource(GameplayPhysicsWorld::default());
        ecs.insert_resource(UiBindings::default());
        ecs.insert_resource(UiEvents::default());
        ecs.insert_resource(UiEventFrame::default());
        ecs.insert_resource(UiRuntimeDiagnostics::default());
        ecs.insert_resource(UiReloadTimer::default());
        ecs.insert_resource(SceneManager::new());
        ecs.insert_resource(SceneSwitchState::default());
        // `SaveStore` is host-provided (player.rs / editor Play insert it
        // once the package or project root is known); `SaveData` is always
        // present so `ctx.save_get`/`ctx.save_set` never see a missing
        // resource, even before the first `save_load` (Phase 56 / ADR 0048).
        ecs.insert_resource(SaveData::default());
        ecs.insert_resource(GameSaveCommandQueue::default());
        ecs.insert_resource(GameTimers::default());
        ecs.insert_resource(GameTimerEvents::default());
        ecs.insert_resource(GamePrefabSpawnQueue::default());
        ecs.insert_resource(GamePrefabEvents::default());
        // Phase 58: always present so `TargetLock::request_*` never targets a
        // missing resource, mirroring `SaveData`'s always-inserted pattern.
        ecs.insert_resource(TargetLock::default());
        ecs.insert_resource(ScriptCommandQueue::default());
        ecs.insert_resource(ScriptWorldCommandQueue::default());
        ecs.insert_resource(ScriptTimers::default());
        ecs.insert_resource(ScriptEventBus::default());
        ecs.insert_resource(PendingScriptEffects::default());
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.transform_propagation",
                "Transform Propagation",
                "Resolves local parent transforms into world-space transforms.",
            ),
            transform_propagation_system,
        );
        // Joint palettes and LOD selection read the world matrices written
        // by transform propagation, so registration order matters here.
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.joint_palette",
                "Joint Palette",
                "Builds skinning palettes from propagated joint transforms.",
            )
            .try_after("engine.transform_propagation")
            .expect("built-in system IDs are valid"),
            joint_palette_system,
        );
        // Morph blending rewrites vertex positions, so it must land before
        // the palette that skins them: morph and skin then compose the way
        // an author expects, with no shader change (ADR 0097 §5).
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.morph_blend",
                "Morph Blend",
                "Applies per-entity morph weights to morphable meshes.",
            )
            .try_before("engine.joint_palette")
            .expect("built-in system IDs are valid"),
            morph_blend_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.material_morph",
                "Material Morph",
                "Applies per-entity morph weights to material parameters.",
            )
            .try_after("engine.morph_blend")
            .expect("built-in system IDs are valid"),
            material_morph_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.lod_selection",
                "LOD Selection",
                "Selects mesh detail levels from propagated world transforms.",
            )
            .try_after("engine.transform_propagation")
            .expect("built-in system IDs are valid"),
            lod_selection_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.particle_update",
                "Particle Update",
                "Advances engine-owned CPU particle emitters.",
            ),
            particle_update_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.vfx_update",
                "VFX Update",
                "Advances scene VFX players from the frame clock.",
            )
            .try_after("engine.transform_propagation")
            .expect("built-in system IDs are valid"),
            vfx_update_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.camera_aspect",
                "Camera Aspect",
                "Synchronizes runtime camera aspect ratios with the viewport.",
            ),
            camera_aspect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.light_resource_mirror",
                "Light Resource Mirror",
                "Copies scene light components into renderer resources.",
            ),
            light_resource_mirror_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.presentation_resource_mirror",
                "Presentation Resource Mirror",
                "Copies scene-owned shadow, environment, and post-process settings into renderer resources.",
            ),
            presentation_resource_mirror_system,
        );
        ecs.add_fixed_system_with_descriptor(
            builtin_system(
                "engine.script_api_snapshot",
                "Script API Snapshot",
                "Captures fixed-step script state before script dispatch.",
            ),
            script_api_snapshot_system,
        );
        ecs.add_fixed_system_with_descriptor(
            builtin_system(
                "engine.game_timer_update",
                "GameModule Timer Update",
                "Advances bounded project Rust timers and publishes completions.",
            ),
            game_timer_update_system,
        );
        // The UI event relay must run before ui_script_event_system (or any
        // game system) reads `UiEventFrame`, so every consumer observes the
        // same frame's events (Phase 54 §1).
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.ui_event_relay",
                "UI Event Relay",
                "Moves pending UI events into the frame-visible event resource.",
            ),
            ui_event_relay_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.ui_script_event",
                "UI Script Event",
                "Dispatches the current UI event frame to scripts.",
            )
            .try_after("engine.ui_event_relay")
            .expect("built-in system IDs are valid"),
            ui_script_event_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.script_api_collect",
                "Script API Collect",
                "Collects deferred commands emitted by script contexts.",
            ),
            script_api_collect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.script_api_dispatch",
                "Script API Dispatch",
                "Validates and dispatches collected script commands.",
            )
            .try_after("engine.script_api_collect")
            .expect("built-in system IDs are valid"),
            script_api_command_dispatch_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.script_core_effect",
                "Script Core Effects",
                "Applies validated core world effects from scripts.",
            )
            .try_after("engine.script_api_dispatch")
            .expect("built-in system IDs are valid"),
            script_core_effect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.script_animation_effect",
                "Script Animation Effects",
                "Applies validated animation effects from scripts.",
            )
            .try_after("engine.script_api_dispatch")
            .expect("built-in system IDs are valid"),
            script_animation_effect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.script_audio_effect",
                "Script Audio Effects",
                "Applies validated audio effects from scripts.",
            )
            .try_after("engine.script_api_dispatch")
            .expect("built-in system IDs are valid"),
            script_audio_effect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.game_audio_effect",
                "GameModule Audio Effects",
                "Resolves and applies bounded project Rust audio requests.",
            )
            .try_after("engine.script_audio_effect")
            .expect("built-in system IDs are valid"),
            game_audio_effect_system,
        );
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.game_save_effect",
                "GameModule Save Effects",
                "Applies bounded project Rust save-slot requests.",
            )
            .try_after("engine.game_audio_effect")
            .expect("built-in system IDs are valid"),
            game_save_effect_system,
        );
        // Hot reload polls the filesystem, which is unavailable on wasm32.
        #[cfg(not(target_arch = "wasm32"))]
        ecs.add_system_with_descriptor(
            builtin_system(
                "engine.ui_document_reload",
                "UI Document Reload",
                "Polls project UI documents for development-time changes.",
            ),
            crate::ui_document::ui_document_reload_system,
        );
        Self {
            ecs,
            title: "Game Engine".to_string(),
            width: 1280,
            height: 720,
            ui_systems: Vec::new(),
            pending_ui_fonts: Vec::new(),
            ui_fonts_installed: false,
        }
    }

    /// Sets the initial window title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the initial window size in physical pixels.
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Adds an ECS system after validating its parameter access.
    ///
    /// # Panics
    ///
    /// Panics when the system requests conflicting component or resource
    /// access. Use [`App::try_add_system`] to handle that failure.
    pub fn add_system<P, M>(&mut self, system: impl IntoSystem<P, M>) -> &mut Self {
        self.ecs.add_system(system);
        self
    }

    /// Tries to add an ECS system after validating its parameter access.
    pub fn try_add_system<P, M>(
        &mut self,
        system: impl IntoSystem<P, M>,
    ) -> Result<&mut Self, engine_ecs::SystemBuildError> {
        self.ecs.try_add_system(system)?;
        Ok(self)
    }

    /// Tries to add a persistently identified per-frame ECS system.
    pub fn try_add_system_with_descriptor<P, M>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<P, M>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        self.ecs
            .try_add_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Adds an ECS system to the fixed-timestep schedule.
    ///
    /// # Panics
    ///
    /// Panics when the system requests conflicting component or resource
    /// access.
    pub fn add_fixed_system<P, M>(&mut self, system: impl IntoSystem<P, M>) -> &mut Self {
        self.ecs.add_fixed_system(system);
        self
    }

    /// Tries to add a fixed-timestep ECS system through the compatibility API.
    pub fn try_add_fixed_system<P, M>(
        &mut self,
        system: impl IntoSystem<P, M>,
    ) -> Result<&mut Self, engine_ecs::SystemBuildError> {
        self.ecs.try_add_fixed_system(system)?;
        Ok(self)
    }

    /// Tries to add a persistently identified fixed-timestep ECS system.
    pub fn try_add_fixed_system_with_descriptor<P, M>(
        &mut self,
        descriptor: SystemDescriptor,
        system: impl IntoSystem<P, M>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        self.ecs
            .try_add_fixed_system_with_descriptor(descriptor, system)?;
        Ok(self)
    }

    /// Inserts or replaces an ECS resource.
    pub fn insert_resource<T: 'static + Send + Sync>(&mut self, resource: T) -> &mut Self {
        self.ecs.insert_resource(resource);
        self
    }

    /// Registers a UI system that draws HUD or overlay elements each frame.
    ///
    /// UI systems run after the ECS update and before the wgpu scene is
    /// presented.  The host calls [`App::run_ui_systems`] with the active
    /// egui context and game-viewport rectangle.
    pub fn add_ui_system(&mut self, system: impl UiSystem) -> &mut Self {
        self.ui_systems.push(Box::new(system));
        self
    }

    /// Runs all registered UI systems, then draws every scene-placed
    /// [`UiDocumentRef`] found in the world (Phase 54 §4).
    ///
    /// Call this once per frame after [`App::ecs_mut`]`().update()` returns
    /// and before the egui frame is submitted so that HUD widgets appear on top
    /// of the game scene.  Build `viewport` with [`UiViewport::direct`] when the
    /// game texture is presented at its own resolution, or
    /// [`UiViewport::scaled`] when a host shrinks the target screen into a
    /// smaller rectangle (ADR 0090).
    pub fn run_ui_systems(&mut self, ctx: &egui::Context, viewport: UiViewport) {
        let _ = self.run_ui_systems_with_options(ctx, viewport, UiDocumentDrawOptions::runtime());
    }

    /// Runs UI systems with explicit scene-document interaction behavior.
    ///
    /// Runtime callers normally use [`App::run_ui_systems`]. Editor surfaces
    /// use this method with [`UiDocumentDrawOptions::editor_preview`] to obtain
    /// hit-test rectangles while suppressing gameplay events. Custom Rust
    /// [`UiSystem`] callbacks still run because their internals are opaque;
    /// editor preview hosts should not register gameplay UI callbacks.
    pub fn run_ui_systems_with_options(
        &mut self,
        ctx: &egui::Context,
        viewport: UiViewport,
        options: UiDocumentDrawOptions,
    ) -> Vec<UiDocumentInstanceDrawReport> {
        self.ecs
            .world_mut()
            .insert_resource(UiContext::new(ctx.clone(), viewport));
        let world = self.ecs.world();
        for system in &mut self.ui_systems {
            system.run(ctx, world);
        }
        let mut reports = Vec::new();
        for entity in world.entities() {
            if let Some(document_ref) = world.get_component::<UiDocumentRef>(entity) {
                if !crate::ui_document::ui_document_is_visible(world, entity) {
                    continue;
                }
                let report = crate::ui_document::draw_ui_document_with_options(
                    ctx,
                    &document_ref.document,
                    world,
                    options,
                );
                reports.push(UiDocumentInstanceDrawReport {
                    entity,
                    asset: document_ref.asset.clone(),
                    document_order: reports.len() as u64,
                    nodes: report.nodes,
                });
            }
        }
        reports
    }

    /// Returns `true` when the standalone runner should drive an egui frame
    /// this session (Phase 54 §4).
    ///
    /// True when a Rust [`UiSystem`] is registered via [`App::add_ui_system`],
    /// or the world contains at least one [`UiDocumentRef`], so scene-placed
    /// UI documents draw without requiring a Rust `UiSystem` registration.
    /// Used by the standalone runner's egui event routing and frame gate
    /// (previously keyed only on `ui_systems.is_empty()`).
    pub fn wants_ui(&self) -> bool {
        !self.ui_systems.is_empty()
            || self
                .ecs
                .world()
                .entities()
                .any(|entity| self.ecs.world().has_component::<UiDocumentRef>(entity))
    }

    /// Registers a font to be installed into the egui context used for UI
    /// systems (ADR 0046 §6).
    ///
    /// egui's built-in fonts cannot render Japanese or other CJK glyphs.
    /// `name` becomes the font's key in egui's `FontDefinitions` and is
    /// inserted at the front of the `Proportional` and `Monospace` family
    /// lists, so it is preferred over the built-in fonts once installed by
    /// [`App::install_ui_fonts`]. Registering a font here has no effect
    /// until `install_ui_fonts` runs; the engine does not bundle font bytes
    /// itself, so the host or project supplies them.
    pub fn add_ui_font(&mut self, name: impl Into<String>, bytes: Vec<u8>) -> &mut Self {
        self.pending_ui_fonts.push((name.into(), bytes));
        self
    }

    /// Installs all fonts registered via [`App::add_ui_font`] into `ctx`.
    ///
    /// This method is idempotent: only the first call per `App` instance
    /// rebuilds and installs the egui font atlas. Calling `ctx.set_fonts`
    /// every frame is expensive because it rebuilds the font atlas texture,
    /// so hosts (the standalone runner, the editor Play host) MAY call this
    /// once per frame; the internal flag makes every call after the first a
    /// cheap no-op.
    pub fn install_ui_fonts(&mut self, ctx: &egui::Context) {
        if self.ui_fonts_installed {
            return;
        }
        self.ui_fonts_installed = true;

        // A host such as the editor may already have installed a system CJK
        // and symbol fallback. Replacing the context with egui defaults when a
        // game registered no fonts would silently discard those host fonts and
        // turn otherwise valid labels into missing-glyph boxes.
        if self.pending_ui_fonts.is_empty() {
            return;
        }

        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in &self.pending_ui_fonts {
            fonts.font_data.insert(
                name.clone(),
                Arc::new(egui::FontData::from_owned(bytes.clone())),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.insert(0, name.clone());
                }
            }
        }
        ctx.set_fonts(fonts);
    }

    /// Returns a shared reference to the underlying ECS application.
    pub fn ecs(&self) -> &EcsApp {
        &self.ecs
    }

    /// Returns an exclusive reference to the underlying ECS application.
    pub fn ecs_mut(&mut self) -> &mut EcsApp {
        &mut self.ecs
    }

    /// Returns a shared reference to the ECS world.
    pub fn world(&self) -> &engine_ecs::World {
        self.ecs.world()
    }

    /// Returns an exclusive reference to the ECS world.
    pub fn world_mut(&mut self) -> &mut engine_ecs::World {
        self.ecs.world_mut()
    }

    /// Returns current system metadata for one runtime schedule.
    pub fn system_infos(&self, schedule: ProjectSystemSchedule) -> Vec<ScheduleEntryInfo> {
        match schedule {
            ProjectSystemSchedule::Update => self.ecs.update_system_infos(),
            ProjectSystemSchedule::FixedUpdate => self.ecs.fixed_system_infos(),
        }
    }

    /// Applies project order and enabled preferences to both schedules.
    ///
    /// Call this after every target system is registered and before the first
    /// update. Unknown and malformed saved IDs are diagnostics; cycles in the
    /// current descriptor graph are returned as blocking errors.
    pub fn apply_system_settings(
        &mut self,
        settings: &SystemSettings,
    ) -> Result<SystemSettingsApplyReport, ScheduleEditError> {
        let mut invalid_ids = Vec::new();
        let update = schedule_configuration(&settings.update, &mut invalid_ids);
        let fixed_update = schedule_configuration(&settings.fixed_update, &mut invalid_ids);
        Ok(SystemSettingsApplyReport {
            update: self.ecs.apply_update_configuration(&update)?,
            fixed_update: self.ecs.apply_fixed_configuration(&fixed_update)?,
            invalid_ids,
        })
    }

    /// Installs a project-local Rust game module into this runtime world.
    ///
    /// The resource retains the native library until all concrete game
    /// component values stored in this world have been dropped.
    pub fn install_game_module(&mut self, module: Arc<GameModule>) -> &mut Self {
        self.try_install_game_module(module)
            .expect("game module system IDs and parameter access must be valid")
    }

    /// Installs a project module and registers each callback as one ECS entry.
    ///
    /// Game callbacks retain a validated data access manifest. The host bridge
    /// receives exclusive world access only long enough to copy declared views
    /// and atomically apply validated output; the module never receives it.
    pub fn try_install_game_module(
        &mut self,
        module: Arc<GameModule>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        self.try_register_game_module_systems(Arc::clone(&module))?;
        self.retain_game_module(module);
        Ok(self)
    }

    /// Retains a project module for component spawning without registering callbacks yet.
    ///
    /// Runtime hosts use this before scene conversion, then register callbacks
    /// after conditionally discovered Engine systems have been added.
    pub fn retain_game_module(&mut self, module: Arc<GameModule>) -> &mut Self {
        let defaults = GameComponentDefaults::from_module(&module);
        let resource_defaults = module
            .resource_schemas()
            .map(|schema| (schema.id.clone(), schema.default_value.clone()))
            .collect();
        let world = self.ecs.world_mut();
        let mut runtime = world
            .remove_resource::<crate::game_host::GameHostRuntime>()
            .unwrap_or_default();
        runtime.replace_module_resources(resource_defaults);
        world.insert_resource(runtime);
        world.insert_resource(defaults);
        world.insert_resource(GameModuleResource { module });
        self
    }

    /// Registers every loaded project callback as an individual ECS entry.
    pub fn try_register_game_module_systems(
        &mut self,
        module: Arc<GameModule>,
    ) -> Result<&mut Self, SystemRegistrationError> {
        let entries: Vec<_> = module
            .system_entries()
            .map(|(index, metadata, descriptor)| (index, metadata.schedule, descriptor.clone()))
            .collect();

        // Validate the complete identity set before mutating either schedule. This
        // keeps a module with an Engine-ID collision from being registered only
        // partially when the conflicting callback is not its first entry.
        let mut known_ids: BTreeSet<SystemId> = [
            self.ecs.update_system_infos(),
            self.ecs.fixed_system_infos(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|info| {
            std::iter::once(info.descriptor.id().clone())
                .chain(info.descriptor.aliases().iter().cloned())
                .collect::<Vec<_>>()
        })
        .collect();
        for (_, _, descriptor) in &entries {
            for id in std::iter::once(descriptor.id()).chain(descriptor.aliases()) {
                if !known_ids.insert(id.clone()) {
                    return Err(ScheduleEditError::DuplicateId(id.clone()).into());
                }
            }
        }

        for (index, schedule, descriptor) in entries {
            let callback_module = Arc::clone(&module);
            let callback = move |world: &mut engine_ecs::World| {
                callback_module.run_system_scoped(index, world)
            };
            match schedule {
                GameSystemSchedule::Update => {
                    self.ecs
                        .try_add_exclusive_system_with_descriptor(descriptor, callback)?;
                }
                GameSystemSchedule::FixedUpdate => {
                    self.ecs
                        .try_add_exclusive_fixed_system_with_descriptor(descriptor, callback)?;
                }
            }
        }
        Ok(self)
    }

    /// Runs project-local systems registered for one existing schedule.
    pub fn run_game_systems(
        &mut self,
        schedule: GameSystemSchedule,
    ) -> Result<(), GameModuleRunError> {
        let module = self
            .ecs
            .world()
            .get_resource::<GameModuleResource>()
            .map(|resource| Arc::clone(&resource.module));
        if let Some(module) = module {
            module.run_systems(schedule, self.ecs.world_mut())?;
        }
        Ok(())
    }

    /// Executes any scene switch requested this frame via
    /// [`crate::scene_manager::SceneManager::request_switch`] (Phase 55,
    /// ADR 0047).
    ///
    /// Hosts (the standalone winit runner started by [`App::run`], the
    /// editor's Play host) call this once per frame, immediately before
    /// running the fixed-timestep and per-frame ECS schedules, while they
    /// still have the moment of exclusive `&mut World` access a frame
    /// boundary provides.
    ///
    /// **Never call this from inside an ECS system.** Systems only ever see
    /// partitioned world access (queries and individual resources); a scene
    /// switch needs to despawn and spawn entities with exclusive world
    /// access, which no system can obtain while the schedule is running.
    /// [`crate::scene_manager::SceneManager::request_switch`] is the
    /// system-safe way to ask for a switch — it just records the request for
    /// this method to pick up on the next call.
    ///
    /// A frame with no pending request returns immediately after one
    /// resource lookup, so hosts can call this unconditionally every frame
    /// without a cost concern. This method never panics: load, validation,
    /// and spawn failures are recorded in the world's
    /// [`crate::scene_manager::SceneSwitchState`] resource instead.
    pub fn process_scene_requests(&mut self) {
        process_script_world_commands(self.ecs.world_mut());
        process_game_prefab_requests(self.ecs.world_mut());
        process_scene_switch(self.ecs.world_mut());
    }

    /// Starts the winit event loop.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform cannot create or run the event loop.
    pub fn run(self) -> Result<(), winit::error::EventLoopError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // A host application may have already installed a global logger.
            let _ = env_logger::try_init();
            let event_loop = EventLoop::new()?;
            let mut runner = EngineRunner::new(self);
            event_loop.run_app(&mut runner)
        }

        #[cfg(target_arch = "wasm32")]
        {
            console_error_panic_hook::set_once();
            // A host page may have already installed a console logger.
            let _ = console_log::init_with_level(log::Level::Debug);
            let event_loop = EventLoop::new()?;
            let mut runner = EngineRunner::new(self);
            event_loop.run_app(&mut runner)
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

fn builtin_system(id: &str, display_name: &str, description: &str) -> SystemDescriptor {
    SystemDescriptor::new(id, display_name, SystemOrigin::Engine)
        .expect("built-in system IDs are valid")
        .with_description(description)
}

fn schedule_configuration(
    settings: &SystemScheduleSettings,
    invalid_ids: &mut Vec<String>,
) -> ScheduleConfiguration {
    let mut parse = |ids: &[String]| {
        ids.iter()
            .filter_map(|id| match SystemId::try_new(id.clone()) {
                Ok(id) => Some(id),
                Err(_) => {
                    invalid_ids.push(id.clone());
                    None
                }
            })
            .collect()
    };
    ScheduleConfiguration {
        order: parse(&settings.order),
        disabled: parse(&settings.disabled),
    }
}

struct EngineRunner {
    app: App,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    last_frame: Option<Instant>,
    egui_context: egui::Context,
    egui_state: Option<EguiWinitState>,
    #[cfg(not(target_arch = "wasm32"))]
    gilrs_context: Option<crate::gamepad::GilrsContext>,
}

impl EngineRunner {
    fn new(app: App) -> Self {
        Self {
            app,
            window: None,
            gpu: None,
            last_frame: None,
            egui_context: egui::Context::default(),
            egui_state: None,
            #[cfg(not(target_arch = "wasm32"))]
            gilrs_context: crate::gamepad::GilrsContext::try_new(),
        }
    }

    fn route_to_egui(&mut self, event: &WindowEvent) -> egui_winit::EventResponse {
        if !self.app.wants_ui() {
            return egui_winit::EventResponse::default();
        }
        let Some(window) = &self.window else {
            return egui_winit::EventResponse::default();
        };
        let Some(egui_state) = &mut self.egui_state else {
            return egui_winit::EventResponse::default();
        };
        egui_state.on_window_event(window, event)
    }

    fn run_egui_frame(&mut self, window: &Window) -> Option<EguiFrameOutput> {
        if !self.app.wants_ui() {
            return None;
        }
        let egui_state = self.egui_state.as_mut()?;
        let raw_input = egui_state.take_egui_input(window);
        let pixels_per_point = egui_winit::pixels_per_point(&self.egui_context, window);
        let size = window.inner_size();
        // The game owns the whole window here, so the presented rectangle *is*
        // the target screen: authored logical units are window points.
        let viewport = UiViewport::direct(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(
                size.width as f32 / pixels_per_point,
                size.height as f32 / pixels_per_point,
            ),
        ));

        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            viewport_output: _,
        } = self.egui_context.run_ui(raw_input, |ui| {
            self.app.run_ui_systems(ui.ctx(), viewport);
        });

        egui_state.handle_platform_output(window, platform_output);

        Some(EguiFrameOutput {
            textures_delta,
            paint_jobs: self.egui_context.tessellate(shapes, pixels_per_point),
            pixels_per_point,
        })
    }
}

impl ApplicationHandler for EngineRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = WindowAttributes::default()
            .with_title(&self.app.title)
            .with_inner_size(winit::dpi::PhysicalSize::new(
                self.app.width,
                self.app.height,
            ));

        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                log::error!("Failed to create window: {error}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(Arc::clone(&window));

        #[cfg(not(target_arch = "wasm32"))]
        {
            match pollster::block_on(GpuState::new(Arc::clone(&window))) {
                Ok(gpu) => {
                    let max_texture_side = gpu.max_texture_side();
                    self.egui_state = Some(EguiWinitState::new(
                        self.egui_context.clone(),
                        egui::ViewportId::ROOT,
                        event_loop,
                        Some(window.scale_factor() as f32),
                        event_loop.system_theme(),
                        Some(max_texture_side),
                    ));
                    self.app.install_ui_fonts(&self.egui_context);
                    self.gpu = Some(gpu);
                }
                Err(error) => {
                    log::error!("Failed to initialize GPU: {error}");
                    event_loop.exit();
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            // Attach the canvas to the document body.
            use winit::platform::web::WindowExtWebSys;
            if let Some(canvas) = window.canvas() {
                web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.body())
                    .and_then(|body| body.append_child(&canvas).ok());
            } else {
                log::error!("WASM window did not provide a canvas");
            }

            let window_clone = Arc::clone(&window);
            wasm_bindgen_futures::spawn_local(async move {
                // GPU initialization for WASM is not wired into GpuState yet.
                log::info!("WASM GPU init: {:?}", window_clone.inner_size());
            });
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let egui_event = self.route_to_egui(&event);
        let world = self.app.ecs.world_mut();

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu
                    && let Err(error) = gpu.resize(size.width, size.height) {
                        log::error!("Failed to resize rendering surface: {error}");
                    }
                if let Some(vp) = world.get_resource_mut::<ViewportSize>() {
                    vp.width = size.width;
                    vp.height = size.height;
                }
            }

            WindowEvent::Focused(false) => {
                // Platform backends do not guarantee matching release events
                // after Alt+Tab, so clear every held device at this boundary.
                crate::input::release_all_input(world);
            }

            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::PhysicalKey;
                if !egui_event.consumed
                    && let PhysicalKey::Code(code) = event.physical_key
                        && let Some(input) = world.get_resource_mut::<Input<KeyCode>>() {
                            match event.state {
                                winit::event::ElementState::Pressed => input.press(code),
                                winit::event::ElementState::Released => input.release(code),
                            }
                        }
                if let winit::keyboard::PhysicalKey::Code(KeyCode::Escape) = event.physical_key {
                    event_loop.exit();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if !egui_event.consumed
                    && let Some(input) = world.get_resource_mut::<Input<MouseButton>>() {
                        match state {
                            winit::event::ElementState::Pressed => input.press(button),
                            winit::event::ElementState::Released => input.release(button),
                        }
                    }
            }

            WindowEvent::CursorMoved { position, .. } => {
                if !egui_event.consumed
                    && let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                        mouse.set_position(position.x as f32, position.y as f32);
                    }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if !egui_event.consumed {
                    let y = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / 20.0,
                    };
                    if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                        mouse.accumulate_scroll(y);
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(ctx) = &mut self.gilrs_context
                    && let Some(queue) = world.get_resource_mut::<VirtualInputQueue>() {
                        ctx.poll(queue);
                    }

                crate::input::drain_virtual_input(world);
                // Publish mouse movement accumulated since the previous frame.
                crate::input::prepare_mouse_frame(world);

                let now = Instant::now();
                let delta = self
                    .last_frame
                    .map(|last| now.duration_since(last).as_secs_f32())
                    .unwrap_or(0.0);
                self.last_frame = Some(now);

                if let Some(time) = world.get_resource_mut::<Time>() {
                    time.advance(delta);
                }

                let fixed_steps = world
                    .get_resource_mut::<FixedTime>()
                    .map(|ft| ft.step(delta))
                    .unwrap_or(0);

                // Runs before schedule execution so a switch always has
                // exclusive world access (ADR 0047 §1); see
                // `App::process_scene_requests` for the full contract.
                self.app.process_scene_requests();

                for _ in 0..fixed_steps {
                    if let Some(time) = self.app.world_mut().get_resource_mut::<FixedTime>() {
                        time.begin_step();
                    }
                    if let Err(error) = self.app.ecs.run_fixed_update() {
                        log::error!("Fixed update failed: {error}");
                    }
                }

                if let Err(error) = self.app.ecs.update() {
                    log::error!("ECS update failed: {error}");
                }

                let egui_frame = self
                    .window
                    .as_ref()
                    .map(Arc::clone)
                    .and_then(|window| self.run_egui_frame(&window));

                if let Some(gpu) = &mut self.gpu
                    && let Err(error) = gpu.render(self.app.ecs.world_mut(), egui_frame) {
                        log::error!("Rendering cannot continue: {error}");
                        event_loop.exit();
                    }

                let world = self.app.ecs.world_mut();
                crate::input::clear_input_transitions(world);

                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event {
            let world = self.app.ecs.world_mut();
            if let Some(mouse) = world.get_resource_mut::<MouseInput>() {
                mouse.accumulate_delta(delta.0, delta.1);
            }
        }
    }
}

struct EguiFrameOutput {
    textures_delta: egui::TexturesDelta,
    paint_jobs: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
}

#[derive(Debug)]
enum GpuInitError {
    Surface(WindowSurfaceError),
    Context(GpuContextError),
    Render(RenderStateError),
}

impl fmt::Display for GpuInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(error) => error.fmt(formatter),
            Self::Context(error) => error.fmt(formatter),
            Self::Render(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for GpuInitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Surface(error) => Some(error),
            Self::Context(error) => Some(error),
            Self::Render(error) => Some(error),
        }
    }
}

struct GpuState {
    surface: WindowSurface,
    context: GpuContext,
    hdr_texture: wgpu::Texture,
    hdr_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    render: WorldRenderer,
    tonemap: ToneMapPass,
    egui_renderer: EguiRenderer,
}

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

impl GpuState {
    async fn new(window: Arc<Window>) -> Result<Self, GpuInitError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface =
            UnconfiguredWindowSurface::new(&instance, window).map_err(GpuInitError::Surface)?;
        let context = GpuContext::new(&instance, Some(surface.surface()))
            .await
            .map_err(GpuInitError::Context)?;
        let surface = surface.configure(&context).map_err(GpuInitError::Surface)?;

        let w = surface.config().width.max(1);
        let h = surface.config().height.max(1);
        let depth_view = Self::create_depth_view(context.device(), w, h);
        let hdr_texture = Self::create_hdr_texture(context.device(), w, h);
        let hdr_view = hdr_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Scene renders to HDR; tonemap converts HDR 驕ｶ鄙ｫ繝ｻswapchain format.
        let render = WorldRenderer::new(context.device(), context.queue(), HDR_FORMAT)
            .await
            .map_err(GpuInitError::Render)?;
        let tonemap = ToneMapPass::new(context.device(), surface.format(), &hdr_view)
            .await
            .map_err(GpuInitError::Render)?;
        let egui_renderer = EguiRenderer::new(
            context.device(),
            surface.format(),
            RendererOptions::default(),
        );

        log::info!("GPU initialized: {:?}", context.adapter().get_info().name);

        Ok(Self {
            surface,
            context,
            hdr_texture,
            hdr_view,
            depth_view,
            render,
            tonemap,
            egui_renderer,
        })
    }

    fn max_texture_side(&self) -> usize {
        self.context.device().limits().max_texture_dimension_2d as usize
    }

    fn create_hdr_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("HDR color texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Depth texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MAIN_PASS_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        depth_tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), WindowSurfaceError> {
        if width > 0 && height > 0 {
            self.surface.resize(&self.context, width, height)?;
            self.depth_view = Self::create_depth_view(self.context.device(), width, height);
            self.hdr_texture = Self::create_hdr_texture(self.context.device(), width, height);
            self.hdr_view = self
                .hdr_texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.tonemap
                .update_bind_group(self.context.device(), &self.hdr_view);
        }
        Ok(())
    }

    fn render(
        &mut self,
        world: &mut engine_ecs::World,
        egui_frame: Option<EguiFrameOutput>,
    ) -> Result<(), RenderFrameError> {
        let output = match self.surface.surface().get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.context);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                log::warn!("Surface temporarily unavailable: skipping frame");
                return Ok(());
            }
        };

        let swapchain_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Scene 驕ｶ鄙ｫ繝ｻHDR offscreen texture.
        self.render.render_to_view(
            world,
            self.context.device(),
            self.context.queue(),
            &self.hdr_view,
            &self.depth_view,
        )?;

        // HDR 驕ｶ鄙ｫ繝ｻswapchain with tone mapping and optional bloom.
        let post_settings = world
            .get_resource::<PostProcessSettings>()
            .cloned()
            .unwrap_or_default();
        self.tonemap.execute(
            self.context.device(),
            self.context.queue(),
            &swapchain_view,
            &post_settings,
        );

        if let Some(egui_frame) = egui_frame {
            self.render_egui(&swapchain_view, egui_frame);
        }

        output.present();
        Ok(())
    }

    fn render_egui(&mut self, swapchain_view: &wgpu::TextureView, frame: EguiFrameOutput) {
        let EguiFrameOutput {
            textures_delta,
            paint_jobs,
            pixels_per_point,
        } = frame;

        let size = self.surface.config();
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point,
        };
        let mut encoder =
            self.context
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("egui overlay encoder"),
                });

        for (id, image_delta) in &textures_delta.set {
            self.egui_renderer.update_texture(
                self.context.device(),
                self.context.queue(),
                *id,
                image_delta,
            );
        }

        let callback_commands = self.egui_renderer.update_buffers(
            self.context.device(),
            self.context.queue(),
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui overlay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: swapchain_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.egui_renderer.render(
                &mut render_pass.forget_lifetime(),
                &paint_jobs,
                &screen_descriptor,
            );
        }

        self.context
            .queue()
            .submit(callback_commands.into_iter().chain([encoder.finish()]));

        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_document::UiDocumentRef;
    use engine_authoring::id::AssetId;
    use engine_authoring::ui::UiDocument;

    #[test]
    fn new_app_updates_without_a_script_engine_or_scene_ui() {
        // Phase 54 auto-registers `ui_event_relay_system` and
        // `ui_script_event_system` for every `App`, but most games never
        // insert a `ScriptEngine` resource; the update loop must still run.
        let mut app = App::new();
        assert!(app.ecs_mut().update().is_ok());
    }

    #[test]
    fn wants_ui_is_false_for_a_fresh_app() {
        let app = App::new();
        assert!(!app.wants_ui());
    }

    #[test]
    fn wants_ui_is_true_once_add_ui_system_is_registered() {
        struct NoopUiSystem;
        impl UiSystem for NoopUiSystem {
            fn run(&mut self, _ctx: &egui::Context, _world: &engine_ecs::World) {}
        }

        let mut app = App::new();
        app.add_ui_system(NoopUiSystem);
        assert!(app.wants_ui());
    }

    #[test]
    fn wants_ui_is_true_once_a_ui_document_ref_is_spawned() {
        let mut app = App::new();
        assert!(!app.wants_ui());

        app.world_mut()
            .spawn_with(UiDocumentRef {
                asset: AssetId::generate(),
                document: UiDocument::default(),
                source_path: None,
                modified: None,
            })
            .expect("must spawn UiDocumentRef entity");

        assert!(app.wants_ui());
    }
}
