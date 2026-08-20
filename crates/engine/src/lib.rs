//! High-level game engine APIs built on the runtime ECS and renderer.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// Data-driven startup, active, recovery, and cooldown ability timing helpers.
pub mod ability;
/// Multi-layer navigation and static triangle-mesh query helpers.
pub mod advanced_geometry;
/// Animation state machine runtime built on the compiled graph from
/// `engine_authoring::animation_graph` (Phase 59, ADR 0033).
pub mod anim_graph;
/// Keyframe-based animation clip asset and runtime Animator component.
pub mod animation;
/// Typed animation parameters and one-dimensional blend sampling.
#[rustfmt::skip]
pub mod animation_parameters;
/// Application construction and event-loop integration.
pub mod app;
/// Runtime asset storage and loading.
pub mod asset;
/// Runtime audio assets and playback control.
pub mod audio;
/// Immutable editor working-copy snapshots supplied at runtime composition boundaries.
pub mod authoring_overlay;
/// Runtime executor for compiled Behavior Tree artifacts.
pub mod behavior_tree;
/// Camera components and built-in camera systems.
pub mod camera;
/// Kinematic character controller for action-game movement (Phase 57).
pub mod character_controller;
/// AABB, sphere, and capsule collision detection for the fixed-timestep update loop.
pub mod collision;
/// Engine-owned hit validation, damage, and knockback services.
pub mod combat;
/// Authorable component definitions owned by the engine.
pub mod components;
/// Ground-contact interval detection over an animation clip (ADR 0080 §1).
pub mod contact_detect;
/// Generic author-owned data assets and component references.
pub mod data_asset;
/// Immediate-mode debug line drawing.
pub mod debug_draw;
/// Content-addressed cache for derived (rebuildable) assets, generic across
/// domains (ADR 0079 §3).
pub mod derived_cache;
/// Bounded combat and animation event timeline for runtime inspection.
pub mod event_debug;
/// FBX static mesh / skin / animation import pipeline via `ufbx` (ADR 0081).
///
/// Desktop-only: FBX import is an authoring-time operation, and compiling
/// `ufbx`'s C source to `wasm32-unknown-unknown` is unsupported (ADR 0081
/// §2), so this module is absent from wasm32 builds regardless of the
/// `fbx-import` feature flag.
#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
pub mod fbx_import;
/// Runtime two-bone foot IK correction against detected ground contacts
/// (ADR 0080 §2).
pub mod foot_ik;
/// Typed gameplay-facing System, Query, Resource, Input, Event, View, and Command API.
pub mod game_api;
/// Host-validated processors for deferred project Rust commands.
#[doc(hidden)]
pub mod game_commands;
/// Intent-level query, transform, collision, command, asset, and physics helpers.
pub mod game_convenience;
/// Per-entity filters and copied values used by `#[game_system(each)]`.
pub mod game_each;
/// Host-side compiler for query-scoped project Rust callback input.
#[doc(hidden)]
pub mod game_host;
/// Query-scoped input and deferred-output contracts for project Rust systems.
#[doc(hidden)]
pub mod game_io;
/// Native project-local Rust component and system modules (ADR 0050).
#[doc(hidden)]
pub mod game_module;
mod game_prefab;
mod game_service_events;
mod game_timer;
/// Gamepad input via gilrs (desktop) or Web Gamepad API (WASM, Phase 43-C).
pub mod gamepad;
/// glTF / GLB static mesh import pipeline.
pub mod gltf_import;
/// Prefab generation from an imported glTF/GLB source (ADR 0074).
pub mod gltf_prefab;
/// Runtime attack-hitbox metadata and activation state.
pub mod hitbox;
/// Humanoid profile validation and conservative import-time detection (ADR 0110).
pub mod humanoid;
/// Humanoid profiles and portable motion variants derived during model import (ADR 0110).
pub mod humanoid_import;
/// Skeleton-independent humanoid motion conversion and target baking (ADR 0110).
pub mod humanoid_motion;
/// Keyboard and mouse input resources.
pub mod input;
/// Ambient, directional, point, and spot light resources.
pub mod light;
/// Lock-on targeting: target selection, persistence, and validation (Phase 58).
pub mod lock_on;
/// Level-of-detail selection and GPU instancing statistics (Phase 47).
pub mod lod;
/// Materials and texture resources.
pub mod material;
/// CPU and GPU mesh types.
pub mod mesh;
/// Format-independent asset builder consuming [`model_ir`] (ADR 0078):
/// sub-asset IDs, skeleton identity/dedupe/rebind, and clip BoneId
/// resolution.
pub mod model_import;
/// Format-independent model intermediate representation shared by every
/// format parser (ADR 0078).
pub mod model_ir;
/// Morph targets: named vertex and material deformations blended on top of a
/// mesh (ADR 0097 §5).
pub mod morph;
/// Target-aware Animation Set motion candidate routing (ADR 0154).
pub mod motion_binding;
/// Cross-domain Native 2D physics composition and public facade (ADR 0127).
pub mod native_2d;
/// GUI-free production navigation bake service shared by authoring adapters.
pub mod navigation_bake;
/// Production tiled polygon navigation and runtime query facade.
pub mod navmesh;
/// CPU particle simulation rendered through GPU instancing (ADR 0044).
pub mod particles;
/// Velocity, gravity, and restitution for dynamic physics bodies.
pub mod physics;
/// Player-controlled entity marker component.
pub mod player;
/// PMX model import pipeline for mesh, skin, material, morph, and best-effort
/// engine-native Secondary Motion hints (ADR 0097, ADR 0112).
///
/// Desktop-only, same rationale as [`fbx_import`]: PMX import is an
/// authoring-time operation, so this module is absent from wasm32 builds
/// regardless of the `mmd-import` feature flag.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub mod pmx_import;
/// Pure skeletal clip sampling and pose blending.
pub mod pose_graph;
/// Post-processing pipeline settings: HDR, tone mapping, bloom (Phase 45).
pub mod postprocess;
/// Offscreen rendering helpers for editor previews.
pub mod preview;
mod render;
/// Deterministic renderer budgets validated during authoring.
pub mod render_limits;
/// Deterministic input recording and playback.
pub mod replay;
/// Retarget maps, the pure FK retarget function, and the baked-clip cache
/// wiring (ADR 0079).
pub mod retarget;
/// Layered rig pose buffers and deterministic world-space pose evaluation.
pub mod rig_pose;
/// Generic rigid-body definitions shared by the engine-native Secondary Motion
/// facade and low-level rig tooling (ADR 0111, ADR 0112).
pub mod rigid_body_rig;
pub mod runtime_metadata;
/// Shared system registration profiles used by runtime hosts and editor catalogs.
pub mod runtime_systems;
/// Save data model and slot storage (Phase 56 / ADR 0048).
pub mod save;
/// Temporary bridge from authoring scene data to runtime ECS entities.
pub mod scene_bridge;
/// Loads authoring scenes from the project asset directory.
pub mod scene_loader;
/// Runtime scene switching resources and the frame-boundary switch algorithm
/// (Phase 55 / ADR 0047).
pub mod scene_manager;
/// Command-oriented Rhai Script API v2 integration (Phase 60 / ADR 0049).
pub mod script_api;
/// Rhai scripting contracts for scene-specific behavior.
pub mod scripting;
/// Engine-native secondary-motion facade (ADR 0112).
pub mod secondary_motion;
/// Shadow mapping and environment-lighting runtime settings.
pub mod shadow;
/// Skeleton asset identity: stable per-skeleton bone IDs and the canonical
/// structure hash used to dedupe and rebind skeletons across imports
/// (ADR 0077).
pub mod skeleton_asset;
/// Skinned mesh component and joint palette computation (ADR 0043).
pub mod skinning;
/// Frame timing resources.
pub mod time;
/// Local and global transform components.
pub mod timeline;
pub mod transform;
/// Runtime HUD and in-game menu UI integration.
pub mod ui;
/// Declarative UI document interpreter (Phase 53 / ADR 0046).
pub mod ui_document;
/// Typed VFX effect playback and deterministic runtime simulation (ADR 0125).
pub mod vfx;
/// VMD (MMD motion) import: bakes MMD's FK -> IK -> appended-parent pipeline
/// into plain [`animation::AnimationClip`] curves (ADR 0097 §3).
///
/// Desktop-only, same rationale as [`pmx_import`]: baking is an
/// authoring-time operation, so this module is absent from wasm32 builds
/// regardless of the `mmd-import` feature flag.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub mod vmd_import;

pub use ability::{
    AbilityActivationError, AbilityDefinition, AbilityDefinitionError, AbilityEvent,
    AbilityMachine, AbilityPhase,
};
pub use advanced_geometry::{
    LayeredNavMesh, LayeredNavMeshError, NavMeshLayer, NavMeshLayerLink, StaticTriangleMesh,
    StaticTriangleMeshError, TriangleMeshRayHit,
};
pub use anim_graph::{
    AnimGraphLoadError, AnimGraphPlayer, anim_graph_system, load_animation_graph,
};
pub use animation::{
    AnimChannel, AnimEvent, AnimProperty, AnimationClip, AnimationEventRecord, AnimationEvents,
    Animator, AnimatorState, ClipCompositionError, Keyframe, MorphChannel, animation_system,
    compose_animation_clips, lerp_channel,
};
pub use animation_parameters::{
    ANIMATION_MOTION_SCHEMA_VERSION, AnimationMotionLibrary, AnimationMotionLibraryError,
    AnimationParameterDeclaration, AnimationParameterError, AnimationParameterKind,
    AnimationParameterValue, AnimationParameters, Blend1d, Blend1dDefinition, Blend1dError,
    Blend1dPoint, Blend1dSample,
};
pub use app::App;
pub use asset::{
    AssetLoadError, AssetManifest, AssetManifestError, AssetPathError, AssetServer, Assets, Handle,
    ImportSettings, ImportedSubAsset, ImportedSubAssetKind, ManifestEntry, RuntimeAssetId,
    SkeletonBoneRecord, SkeletonRecord, SourceFileStamp, SourceStamp,
    imported_logical_humanoid_motion_sub_asset_id, imported_motion_sub_asset_id,
    imported_sub_asset_id,
};
pub use audio::{
    AudioAsset, AudioEmitter, AudioError, AudioListener, AudioSystem, AuthoredAudioState,
    MusicController, authored_audio_system,
};
pub use behavior_tree::{
    BehaviorStatus, BehaviorTreeBehaviorRegistry, BehaviorTreeContext, BehaviorTreeDispatchRecord,
    BehaviorTreeExecutor, BehaviorTreeRegistryError, BehaviorTreeRunner, BehaviorTreeRuntimeError,
    behavior_tree_tick_system, register_behavior_tree_system,
};
pub use camera::{
    Camera3D, FollowCamera, LockOnCamera, OrbitCamera, ViewportSize, follow_camera_system,
    lock_on_camera_system, orbit_camera_system,
};
pub use character_controller::{KinematicCharacterController, character_controller_system};
pub use collision::{
    Collider, CollisionEvent, CollisionEvents, CollisionInfo, CollisionLayers, CollisionPhase,
    CollisionStats, CollisionTransition, PhysicsBody, TriggerVolume, WorldAabb, WorldCapsule,
    WorldShape, WorldSphere, collider_debug_draw_system, collision_detection_system,
    collisions_by_entity, segment_blocked_by_static, should_collide, static_obstacle_aabbs,
    world_shapes_overlap,
};
pub use combat::{
    DamageReceiver, HitResult, HitResults, KnockbackRequest, KnockbackRequests,
    apply_knockback_system, combat_contact_system, combat_debug_draw_system,
};
pub use components::{
    AssetKind, ComponentDefinition, ComponentRegistry, ComponentSpawnError, FieldDef,
    InspectorFieldCondition, InspectorFieldControl, InspectorHint, NumericRange, SpawnContext,
    SpawnFn, asset_path_matches_kind, builtin_registry, validate_builtin_component_asset_files,
    validate_builtin_component_asset_files_with_overlay,
    validate_builtin_component_asset_references, validate_builtin_component_assets,
    validate_builtin_component_values,
};
pub use contact_detect::{ContactInterval, detect_contact_intervals};
pub use debug_draw::{DebugLine, DebugLines};
pub use derived_cache::{CacheKey, DerivedCache};
pub use engine_ecs as ecs;
pub use engine_ecs::{Commands, Query, Res, ResMut, With, Without};
pub use engine_game_component_macros::GameComponent;
#[doc(hidden)]
pub use engine_game_macros::GameComponent as __LegacyGameComponent;
pub use engine_game_macros::{GameQuerySpec, GameResource, InputAction, SaveKey};
pub use engine_game_system_macros::game_system;
pub use event_debug::{
    RUNTIME_EVENT_TRACE_PATH_ENV, RUNTIME_EVENT_TRACE_SCHEMA_VERSION, RuntimeEventDebugEntry,
    RuntimeEventDebugKind, RuntimeEventTimeline, RuntimeEventTrace, RuntimeEventTraceEntity,
    RuntimeEventTraceEntry, RuntimeEventTraceError, RuntimeEventTraceKind,
    runtime_event_timeline_system,
};
#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
pub use fbx_import::{
    FbxImportError, fbx_source_dependencies, fingerprint_fbx_source, import_fbx_bytes,
    import_fbx_path, import_fbx_path_with_contact_bones, parse_fbx, parse_fbx_path,
};
pub use foot_ik::{FootIk, FootIkDiagnostics, foot_ik_system};
pub use game_prefab::{MAX_GAME_PREFAB_EVENTS, MAX_GAME_PREFAB_REQUESTS};
pub use game_timer::{MAX_GAME_TIMER_EVENTS, MAX_GAME_TIMERS};
pub use glam;
pub use gltf_import::{
    GltfImportError, fingerprint_gltf_source, gltf_source_dependencies, import_gltf_bytes,
    import_gltf_path, import_gltf_path_with_contact_bones, parse_gltf, parse_gltf_path,
};
pub use gltf_prefab::{
    ModelPartSync, build_gltf_prefab, model_part_sync, skinned_render_part_value,
};
pub use hitbox::AttackHitbox;
pub use input::{
    GamepadAxis, GamepadAxisState, GamepadButton, GamepadConnectionState, GamepadId, Input,
    InputCommand, InputSource, KeyCode, MouseButton, MouseInput, VirtualInputQueue,
    clear_input_transitions, drain_virtual_input, prepare_mouse_frame, release_all_input,
};
pub use inventory;
pub use light::{
    AmbientLight, DirectionalLight, PointLight, SkySettings, SpotLight,
    light_resource_mirror_system,
};
pub use lock_on::{LockOnTarget, TargetLock, lock_on_system};
pub use lod::{InstanceStats, LodGroup, LodLevel, lod_selection_system};
pub use material::{
    AlphaMode, CullMode, DecodedTexture, Material, MaterialSlots, ShadingModel, Texture,
    TextureUploadError,
};
pub use mesh::{
    GpuMesh, GpuMeshCache, InstanceData, Mesh, MeshValidationError, SharedGpuMeshCache,
    SkinningVertexData, Submesh, Vertex, extract_baked_submesh, mesh_to_obj,
};
pub use model_import::{
    GltfAnimationData, GltfImportResult, GltfMaterialData, GltfMeshData, GltfNodeBinding,
    GltfSkinData, GltfTextureData, ModelImportError, SKELETON_REBIND_DIAGNOSTIC,
    fingerprint_model_source, import_model_bytes, import_model_path,
    import_model_path_with_contact_bones, model_source_dependencies,
};
pub use model_ir::{
    IrClip, IrClipChannel, IrMaterial, IrMesh, IrNode, IrSkin, IrTexture, ModelDocument,
};
pub use morph::{
    MaterialMorphOffset, MaterialMorphOperation, MorphAsset, MorphBaseColor, MorphDirtyVertices,
    MorphTargets, MorphWeights, apply_morph_blend, blended_base_color, material_morph_system,
    morph_blend_system,
};
pub use navmesh::{
    NavMesh, NavMeshAgent, NavMeshAgentStatus, NavMeshError, NavMeshQuery, NavMeshSettings,
    NavMeshSurface, bake_from_obstacles, nav_mesh_agent_system, nav_mesh_debug_draw_system,
};
#[cfg(not(target_arch = "wasm32"))]
pub use navmesh::{bake_navmesh, load_navmesh, save_navmesh};
pub use particles::{ParticleEmitter, particle_update_system};
pub use physics::{
    GameplayPhysicsWorld, Gravity, GravityScale, Velocity, gravity_system, restitution_system,
    velocity_system,
};
pub use player::{
    InputActionMap, InputActionMapDiagnostic, MovePlane, PlayerController, PlayerMarker,
    PlayerMovementIntent, PlayerMovementIntents, player_character_motor_system,
    player_controller_system,
};
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub use pmx_import::{
    PMX_TO_METERS, PmxImportError, fingerprint_pmx_source, import_pmx_bytes, import_pmx_path,
    import_pmx_path_with_contact_bones, parse_pmx, parse_pmx_path, pmx_source_dependencies,
};
pub use pose_graph::{BoneMask, EntityPose, PoseArena, PoseGraphOutput};
pub use postprocess::{
    BloomSettings, ColorGradingSettings, HdrRenderTargetFormat, PostProcessSettings,
    ToneMapOperator,
};
pub use preview::{
    PREVIEW_COLOR_FORMAT, PREVIEW_DEPTH_FORMAT, PREVIEW_MSAA_SAMPLE_COUNT, PREVIEW_RENDER_FORMAT,
    PreviewRenderer, PreviewRendererError,
};
pub use render_limits::{
    MATERIAL_TEXTURE_SLOTS, MAX_AMBIENT_LIGHTS, MAX_DIRECTIONAL_LIGHTS, MAX_PARTICLES_PER_EMITTER,
    MAX_POINT_LIGHTS, MAX_RENDER_INSTANCES, MAX_SPOT_LIGHTS, MAX_TEXTURE_DIMENSION,
    validate_scene_render_limits,
};
pub use replay::{
    InputReplay, REPLAY_FORMAT_VERSION, ReplayCheckpoint, ReplayCommand, ReplayError, ReplayPlayer,
    ReplayRecorder, ReplayTick,
};
pub use retarget::{
    BAKED_CLIP_FILE_EXTENSION, BAKED_CLIP_SCHEMA_VERSION, BakedClipError, BonePair, ChainPair,
    PackagedBakedClips, RETARGET_ALGORITHM_VERSION, RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC,
    RETARGET_CACHE_DOMAIN, RETARGET_MAP_FILE_SUFFIX, RETARGET_MAP_MISSING_DIAGNOSTIC,
    RETARGET_MAP_SCHEMA_VERSION, RETARGET_MAP_STALE_DIAGNOSTIC,
    RETARGET_SOURCE_UNFINGERPRINTED_DIAGNOSTIC, RetargetError, RetargetMap, RetargetMapError,
    RetargetResolveError, TranslationMode, TranslationPolicy, TranslationScale,
    cache_key_for_retargeted_clip, deserialize_baked_clip, find_retarget_map_for_pair,
    generate_retarget_map, load_registered_retarget_maps, resolve_or_bake_retargeted_clip,
    retarget_clip, serialize_baked_clip,
};
pub use rig_pose::{
    PoseBlend, PoseBuffer, PoseChannels, PoseLayer, PoseStage, RigPose,
    publish_final_rig_pose_system, rig_pose_clear_transient_system,
};
pub use runtime_metadata::RuntimeMetadata;
pub use runtime_systems::register_runtime_systems;
pub use save::{
    MAX_GAME_SAVE_COMMANDS, SAVE_SCHEMA_VERSION, SaveData, SaveDataError, SaveSlotMetadata,
    SaveStore, SaveStoreError, SaveValue,
};
#[cfg(not(target_arch = "wasm32"))]
pub use save::{distributed_log_root, distributed_save_root};
pub use scene_loader::{SceneLoadError, SceneLoader};
pub use scene_manager::{SceneManager, SceneSwitchState};
pub use script_api::{
    MAX_SCRIPT_COMMANDS, QueuedScriptCommand, RuntimeEntityIdentity, ScriptApiCommand,
    ScriptCommandQueue, ScriptEvent, ScriptEventBus, ScriptLockCommand, ScriptTimers,
    ScriptWorldCommandQueue, build_entity_snapshot, dynamic_to_ui_binding,
    process_script_world_commands,
};
pub use scripting::{
    ComponentSetCommand, FutureScriptLifecycleHook, SCRIPT_COMPONENT, SavePersistCommand,
    SaveSetCommand, ScriptAsset, ScriptCallResult, ScriptComponent, ScriptContextCall,
    ScriptContextCallCount, ScriptEngine, ScriptEngineConfig, ScriptError, ScriptExecutionMetrics,
    ScriptInstance, ScriptLifecycleHook, ScriptProfilerFrame, ScriptState, ScriptValue,
    apply_save_commands, build_input_snapshot, scripting_update_system,
};
pub use secondary_motion::{
    JointDef, RigidBodyDef, RigidBodyMode, RigidBodyShape, SECONDARY_MOTION_RIG_SCHEMA_VERSION,
    SecondaryMotion, SecondaryMotionRigAsset, SecondaryMotionRigRegistry, SecondaryMotionWorlds,
    secondary_motion_presentation_system, secondary_motion_system,
};
pub use shadow::{
    EnvironmentLighting, SHADOW_CASCADE_COUNT, ShadowCascade, ShadowMapDescriptor, ShadowMapFormat,
    ShadowSettings, presentation_resource_mirror_system,
};
pub use skeleton_asset::{
    BoneDef, BoneId, SkeletonAsset, SkeletonAssetRegistry, SkeletonIdentity,
    compute_skeleton_identity,
};
pub use skinning::{
    BoneAttachment, JointPalette, MAX_JOINTS, RigSpawnError, Skeleton, SkeletonNodeDesc,
    SkinnedMesh, SpawnedRig, joint_palette_system, spawn_rig,
};
pub use time::FixedTime;
pub use transform::{Children, GlobalTransform, Parent, Transform, transform_propagation_system};
pub use ui::{UiContext, UiSystem, UiViewport};
#[cfg(not(target_arch = "wasm32"))]
pub use ui_document::ui_document_reload_system;
pub use ui_document::{
    MAX_QUEUED_UI_EVENTS, UI_RELOAD_INTERVAL_SECONDS, UiBindingValue, UiBindings,
    UiDocumentDrawOptions, UiDocumentDrawReport, UiDocumentInstanceDrawReport, UiDocumentLoadError,
    UiDocumentOverlay, UiDocumentRef, UiDocumentVisibility, UiDrawFrame, UiEventFrame, UiEvents,
    UiNodeDrawRecord, UiReloadTimer, UiRuntimeDiagnostics, anchor_position, draw_ui_document,
    draw_ui_document_with_frame, draw_ui_document_with_options, format_binding_value,
    load_ui_document, resolve_ui_string, ui_document_scale, ui_event_relay_system,
    ui_script_event_system,
};
pub use vfx::{
    VFX_PREVIEW_STEP_SECONDS, VfxInstance, VfxPlayer, VfxRenderBinding, VfxRenderBindings,
    VfxRenderParticle, VfxRestartPolicy, VfxRuntimeBackend, VfxRuntimeStats, vfx_update_system,
};
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub use vmd_import::{
    DEFAULT_VMD_SAMPLE_RATE, MAX_BAKED_FRAME, MMD_FRAME_RATE, VMD_BAKE_ALGORITHM_VERSION,
    VMD_COMPATIBILITY_NEUTRAL_EPSILON, VmdBakeOptions, VmdBakeRig, VmdBakedClip, VmdContentKind,
    VmdImportError, VmdImportResult, VmdPmxCompatibilityIssue, VmdPmxCompatibilityIssueKind,
    VmdPmxCompatibilityReport, VmdPmxCompatibilitySummary, cache_key_for_baked_vmd,
    check_vmd_pmx_compatibility_bytes, check_vmd_pmx_compatibility_path, classify_vmd_bytes,
    classify_vmd_path, classify_vmd_summary, fingerprint_motion_source, fingerprint_motion_sources,
    import_motion_path, import_vmd_bytes, import_vmd_path, is_motion_source_path,
    motion_source_dependencies, motion_source_dependencies_for_models, resolve_or_bake_vmd_bytes,
    resolve_or_bake_vmd_path, vmd_recorded_model_name_path,
};

/// Common imports for project-local native Rust gameplay.
///
/// This prelude deliberately selects the typed game-module API rather than the
/// host ECS types with similar names. Engine runtime systems may continue to
/// import [`crate::ecs`] directly.
pub mod prelude {
    pub use crate::ability::{
        AbilityActivationError, AbilityDefinition, AbilityDefinitionError, AbilityEvent,
        AbilityMachine, AbilityPhase,
    };
    pub use crate::animation_parameters::{
        ANIMATION_MOTION_SCHEMA_VERSION, AnimationMotionLibrary, AnimationMotionLibraryError,
        AnimationParameterDeclaration, AnimationParameterError, AnimationParameterKind,
        AnimationParameterValue, AnimationParameters, Blend1d, Blend1dDefinition, Blend1dError,
        Blend1dPoint, Blend1dSample,
    };
    pub use crate::game_api::{
        Action, AnimationEvent, AnimationPlaybackState, AnimationStateView, AuthoringIdentityView,
        BehaviorTreeStateView, CharacterStateView, CollisionEvent, CollisionEventPhase, Commands,
        DamageReceiverView, EngineView, Event, Events, GameApiError, GameSystemParam,
        GlobalTransformView, HitEvent, HitboxStateView, HostEvent, HostView, InputAction,
        LocalTransformView, LockOnStateView, NavigationStateView, NavigationStatus, ProjectEvent,
        ProjectEventRecord, ProjectEvents, Query, QueryAccessBuilder, QueryRow, QuerySpec, Res,
        ResMut, SaveKey, SaveValue, SceneEvent, SceneStateView, SpawnResultEvent, Time, TimerEvent,
        TransformView, UiBindingsView, UiEvent, View,
    };
    pub use crate::game_convenience::*;
    pub use crate::game_each::{AnyOf, Entity, Transform, With, Without};
    pub use crate::game_io::{GameBehaviorStatus, GameEntityHandle, GameHitboxShape};
    pub use crate::{
        GameComponent, GameQuerySpec, GameResource, InputAction, SaveKey, game_system,
    };
    pub use glam::{Quat, Vec2, Vec3};
}
