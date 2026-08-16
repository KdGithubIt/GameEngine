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
/// Humanoid profile validation and conservative import-time detection (ADR 0110).
pub mod humanoid;
/// Skeleton-independent humanoid motion conversion and target baking (ADR 0110).
pub mod humanoid_motion;
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
/// Humanoid profiles and portable motion variants derived during model import (ADR 0110).
pub mod humanoid_import;
/// Runtime attack-hitbox metadata and activation state.
pub mod hitbox;
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
/// Engine-native secondary-motion facade (ADR 0112).
pub mod secondary_motion;
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
/// Pure skeletal clip sampling and pose blending.
pub mod pose_graph;
/// PMX model import pipeline for mesh, skin, material, morph, and best-effort
/// engine-native Secondary Motion hints (ADR 0097, ADR 0112).
///
/// Desktop-only, same rationale as [`fbx_import`]: PMX import is an
/// authoring-time operation, so this module is absent from wasm32 builds
/// regardless of the `mmd-import` feature flag.
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub mod pmx_import;
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
/// Generic rigid-body definitions shared by the engine-native Secondary Motion
/// facade and low-level rig tooling (ADR 0111, ADR 0112).
pub mod rigid_body_rig;
/// Layered rig pose buffers and deterministic world-space pose evaluation.
pub mod rig_pose;
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
pub mod transform;
/// Runtime HUD and in-game menu UI integration.
pub mod ui;
/// Declarative UI document interpreter (Phase 53 / ADR 0046).
pub mod ui_document;
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
    anim_graph_system, load_animation_graph, AnimGraphLoadError, AnimGraphPlayer,
};
pub use animation::{
    animation_system, compose_animation_clips, lerp_channel, AnimChannel, AnimEvent, AnimProperty,
    AnimationClip, AnimationEventRecord, AnimationEvents, Animator, AnimatorState,
    ClipCompositionError, Keyframe, MorphChannel,
};
pub use animation_parameters::{
    AnimationMotionLibrary, AnimationMotionLibraryError, AnimationParameterDeclaration,
    AnimationParameterError, AnimationParameterKind, AnimationParameterValue, AnimationParameters,
    Blend1d, Blend1dDefinition, Blend1dError, Blend1dPoint, Blend1dSample,
    ANIMATION_MOTION_SCHEMA_VERSION,
};
pub use app::App;
pub use asset::{
    imported_motion_sub_asset_id, imported_sub_asset_id, AssetLoadError, AssetManifest,
    AssetManifestError, AssetPathError, AssetServer, Assets, Handle, ImportSettings,
    ImportedSubAsset, ImportedSubAssetKind, ManifestEntry, RuntimeAssetId, SkeletonBoneRecord,
    SkeletonRecord, SourceFileStamp, SourceStamp,
};
pub use audio::{
    authored_audio_system, AudioAsset, AudioEmitter, AudioError, AudioListener, AudioSystem,
    AuthoredAudioState, MusicController,
};
pub use behavior_tree::{
    behavior_tree_tick_system, register_behavior_tree_system, BehaviorStatus,
    BehaviorTreeBehaviorRegistry, BehaviorTreeContext, BehaviorTreeDispatchRecord,
    BehaviorTreeExecutor, BehaviorTreeRegistryError, BehaviorTreeRunner, BehaviorTreeRuntimeError,
};
pub use camera::{
    follow_camera_system, lock_on_camera_system, orbit_camera_system, Camera3D, FollowCamera,
    LockOnCamera, OrbitCamera, ViewportSize,
};
pub use character_controller::{character_controller_system, KinematicCharacterController};
pub use collision::{
    collider_debug_draw_system, collision_detection_system, collisions_by_entity,
    segment_blocked_by_static, should_collide, static_obstacle_aabbs, world_shapes_overlap,
    Collider, CollisionEvent, CollisionEvents, CollisionInfo, CollisionLayers, CollisionPhase,
    CollisionStats, CollisionTransition, PhysicsBody, TriggerVolume, WorldAabb, WorldCapsule,
    WorldShape, WorldSphere,
};
pub use combat::{
    apply_knockback_system, combat_contact_system, combat_debug_draw_system, DamageReceiver,
    HitResult, HitResults, KnockbackRequest, KnockbackRequests,
};
pub use components::{
    asset_path_matches_kind, builtin_registry, validate_builtin_component_asset_files,
    validate_builtin_component_asset_references, validate_builtin_component_assets,
    validate_builtin_component_values, AssetKind, ComponentDefinition, ComponentRegistry,
    ComponentSpawnError, FieldDef, InspectorFieldCondition, InspectorFieldControl, InspectorHint,
    NumericRange, SpawnContext, SpawnFn,
};
pub use contact_detect::{detect_contact_intervals, ContactInterval};
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
    runtime_event_timeline_system, RuntimeEventDebugEntry, RuntimeEventDebugKind,
    RuntimeEventTimeline, RuntimeEventTrace, RuntimeEventTraceEntity, RuntimeEventTraceEntry,
    RuntimeEventTraceError, RuntimeEventTraceKind, RUNTIME_EVENT_TRACE_PATH_ENV,
    RUNTIME_EVENT_TRACE_SCHEMA_VERSION,
};
#[cfg(all(feature = "fbx-import", not(target_arch = "wasm32")))]
pub use fbx_import::{
    fbx_source_dependencies, fingerprint_fbx_source, import_fbx_bytes, import_fbx_path,
    import_fbx_path_with_contact_bones, parse_fbx, parse_fbx_path, FbxImportError,
};
pub use foot_ik::{foot_ik_system, FootIk, FootIkDiagnostics};
pub use game_prefab::{MAX_GAME_PREFAB_EVENTS, MAX_GAME_PREFAB_REQUESTS};
pub use game_timer::{MAX_GAME_TIMERS, MAX_GAME_TIMER_EVENTS};
pub use glam;
pub use gltf_import::{
    fingerprint_gltf_source, gltf_source_dependencies, import_gltf_bytes, import_gltf_path,
    import_gltf_path_with_contact_bones, parse_gltf, parse_gltf_path, GltfImportError,
};
pub use gltf_prefab::{
    build_gltf_prefab, model_part_sync, skinned_render_part_value, ModelPartSync,
};
pub use hitbox::AttackHitbox;
pub use input::{
    clear_input_transitions, drain_virtual_input, prepare_mouse_frame, release_all_input,
    GamepadAxis, GamepadAxisState, GamepadButton, GamepadConnectionState, GamepadId, Input,
    InputCommand, InputSource, KeyCode, MouseButton, MouseInput, VirtualInputQueue,
};
pub use inventory;
pub use light::{
    light_resource_mirror_system, AmbientLight, DirectionalLight, PointLight, SkySettings, SpotLight,
};
pub use lock_on::{lock_on_system, LockOnTarget, TargetLock};
pub use lod::{lod_selection_system, InstanceStats, LodGroup, LodLevel};
pub use material::{
    AlphaMode, CullMode, DecodedTexture, Material, MaterialSlots, ShadingModel, Texture,
    TextureUploadError,
};
pub use mesh::{
    extract_baked_submesh, mesh_to_obj, GpuMesh, GpuMeshCache, InstanceData, Mesh,
    MeshValidationError, SharedGpuMeshCache, SkinningVertexData, Submesh, Vertex,
};
pub use secondary_motion::{
    secondary_motion_presentation_system, secondary_motion_system, JointDef, RigidBodyDef,
    RigidBodyMode, RigidBodyShape, SecondaryMotion, SecondaryMotionRigAsset,
    SecondaryMotionRigRegistry, SecondaryMotionWorlds, SECONDARY_MOTION_RIG_SCHEMA_VERSION,
};
pub use model_import::{
    fingerprint_model_source, import_model_bytes, import_model_path,
    import_model_path_with_contact_bones, model_source_dependencies, GltfAnimationData,
    GltfImportResult, GltfMaterialData, GltfMeshData, GltfNodeBinding, GltfSkinData,
    GltfTextureData, ModelImportError, SKELETON_REBIND_DIAGNOSTIC,
};
pub use model_ir::{
    IrClip, IrClipChannel, IrMaterial, IrMesh, IrNode, IrSkin, IrTexture, ModelDocument,
};
pub use navmesh::{
    bake_from_obstacles, nav_mesh_agent_system, nav_mesh_debug_draw_system, NavMesh, NavMeshAgent,
    NavMeshAgentStatus, NavMeshError, NavMeshQuery, NavMeshSettings, NavMeshSurface,
};
#[cfg(not(target_arch = "wasm32"))]
pub use navmesh::{bake_navmesh, load_navmesh, save_navmesh};
pub use particles::{particle_update_system, ParticleEmitter};
pub use physics::{
    gravity_system, restitution_system, velocity_system, GameplayPhysicsWorld, Gravity,
    GravityScale, Velocity,
};
pub use player::{
    player_character_motor_system, player_controller_system, InputActionMap,
    InputActionMapDiagnostic, MovePlane, PlayerController, PlayerMarker, PlayerMovementIntent,
    PlayerMovementIntents,
};
pub use pose_graph::{BoneMask, EntityPose, PoseArena, PoseGraphOutput};
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub use pmx_import::{
    fingerprint_pmx_source, import_pmx_bytes, import_pmx_path, import_pmx_path_with_contact_bones,
    parse_pmx, parse_pmx_path, pmx_source_dependencies, PmxImportError, PMX_TO_METERS,
};
pub use postprocess::{
    BloomSettings, ColorGradingSettings, HdrRenderTargetFormat, PostProcessSettings,
    ToneMapOperator,
};
pub use morph::{
    apply_morph_blend, blended_base_color, material_morph_system, morph_blend_system,
    MaterialMorphOffset, MaterialMorphOperation, MorphAsset, MorphBaseColor, MorphDirtyVertices,
    MorphTargets, MorphWeights,
};
pub use rig_pose::{
    publish_final_rig_pose_system, rig_pose_clear_transient_system, PoseBlend, PoseBuffer,
    PoseChannels, PoseLayer, PoseStage, RigPose,
};
pub use preview::{
    PreviewRenderer, PreviewRendererError, PREVIEW_COLOR_FORMAT, PREVIEW_DEPTH_FORMAT,
    PREVIEW_MSAA_SAMPLE_COUNT, PREVIEW_RENDER_FORMAT,
};
pub use render_limits::{
    validate_scene_render_limits, MATERIAL_TEXTURE_SLOTS, MAX_AMBIENT_LIGHTS,
    MAX_DIRECTIONAL_LIGHTS, MAX_PARTICLES_PER_EMITTER, MAX_POINT_LIGHTS, MAX_RENDER_INSTANCES,
    MAX_SPOT_LIGHTS, MAX_TEXTURE_DIMENSION,
};
pub use replay::{
    InputReplay, ReplayCheckpoint, ReplayCommand, ReplayError, ReplayPlayer, ReplayRecorder,
    ReplayTick, REPLAY_FORMAT_VERSION,
};
pub use retarget::{
    cache_key_for_retargeted_clip, deserialize_baked_clip, find_retarget_map_for_pair,
    generate_retarget_map, load_registered_retarget_maps, resolve_or_bake_retargeted_clip,
    retarget_clip, serialize_baked_clip, BakedClipError, BonePair, ChainPair, PackagedBakedClips,
    RetargetError, RetargetMap, RetargetMapError, RetargetResolveError, TranslationMode,
    TranslationPolicy, TranslationScale, BAKED_CLIP_FILE_EXTENSION, BAKED_CLIP_SCHEMA_VERSION,
    RETARGET_ALGORITHM_VERSION, RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC,
    RETARGET_CACHE_DOMAIN, RETARGET_MAP_FILE_SUFFIX, RETARGET_MAP_MISSING_DIAGNOSTIC,
    RETARGET_MAP_SCHEMA_VERSION, RETARGET_MAP_STALE_DIAGNOSTIC,
    RETARGET_SOURCE_UNFINGERPRINTED_DIAGNOSTIC,
};
pub use runtime_metadata::RuntimeMetadata;
pub use runtime_systems::register_runtime_systems;
#[cfg(not(target_arch = "wasm32"))]
pub use save::{distributed_log_root, distributed_save_root};
pub use save::{
    SaveData, SaveDataError, SaveSlotMetadata, SaveStore, SaveStoreError, SaveValue,
    MAX_GAME_SAVE_COMMANDS, SAVE_SCHEMA_VERSION,
};
pub use scene_loader::{SceneLoadError, SceneLoader};
pub use scene_manager::{SceneManager, SceneSwitchState};
pub use script_api::{
    build_entity_snapshot, dynamic_to_ui_binding, process_script_world_commands,
    QueuedScriptCommand, RuntimeEntityIdentity, ScriptApiCommand, ScriptCommandQueue, ScriptEvent,
    ScriptEventBus, ScriptLockCommand, ScriptTimers, ScriptWorldCommandQueue, MAX_SCRIPT_COMMANDS,
};
pub use scripting::{
    apply_save_commands, build_input_snapshot, scripting_update_system, ComponentSetCommand,
    FutureScriptLifecycleHook, SavePersistCommand, SaveSetCommand, ScriptAsset, ScriptCallResult,
    ScriptComponent, ScriptContextCall, ScriptContextCallCount, ScriptEngine, ScriptEngineConfig,
    ScriptError, ScriptExecutionMetrics, ScriptInstance, ScriptLifecycleHook, ScriptProfilerFrame,
    ScriptState, ScriptValue, SCRIPT_COMPONENT,
};
pub use shadow::{
    presentation_resource_mirror_system, EnvironmentLighting, ShadowCascade, ShadowMapDescriptor,
    ShadowMapFormat, ShadowSettings, SHADOW_CASCADE_COUNT,
};
pub use skeleton_asset::{
    compute_skeleton_identity, BoneDef, BoneId, SkeletonAsset, SkeletonAssetRegistry,
    SkeletonIdentity,
};
pub use skinning::{
    joint_palette_system, spawn_rig, BoneAttachment, JointPalette, RigSpawnError, Skeleton,
    SkeletonNodeDesc, SkinnedMesh, SpawnedRig, MAX_JOINTS,
};
pub use time::FixedTime;
pub use transform::{transform_propagation_system, Children, GlobalTransform, Parent, Transform};
pub use ui::{UiContext, UiSystem, UiViewport};
#[cfg(not(target_arch = "wasm32"))]
pub use ui_document::ui_document_reload_system;
pub use ui_document::{
    anchor_position, draw_ui_document, draw_ui_document_with_frame, draw_ui_document_with_options,
    format_binding_value, load_ui_document, resolve_ui_string, ui_document_scale,
    ui_event_relay_system, ui_script_event_system, UiBindingValue, UiBindings,
    UiDocumentDrawOptions, UiDocumentDrawReport, UiDocumentInstanceDrawReport, UiDocumentLoadError,
    UiDocumentOverlay, UiDocumentRef, UiDocumentVisibility, UiDrawFrame, UiEventFrame, UiEvents,
    UiNodeDrawRecord, UiReloadTimer, UiRuntimeDiagnostics, MAX_QUEUED_UI_EVENTS,
    UI_RELOAD_INTERVAL_SECONDS,
};
#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
pub use vmd_import::{
    cache_key_for_baked_vmd, check_vmd_pmx_compatibility_bytes,
    check_vmd_pmx_compatibility_path, classify_vmd_bytes, classify_vmd_path,
    classify_vmd_summary, fingerprint_motion_source, fingerprint_motion_sources,
    import_motion_path, import_vmd_bytes, import_vmd_path, is_motion_source_path,
    motion_source_dependencies, motion_source_dependencies_for_models,
    resolve_or_bake_vmd_bytes, resolve_or_bake_vmd_path, vmd_recorded_model_name_path,
    VmdBakeOptions, VmdBakeRig, VmdBakedClip, VmdContentKind, VmdImportError, VmdImportResult,
    VmdPmxCompatibilityIssue, VmdPmxCompatibilityIssueKind, VmdPmxCompatibilityReport,
    VmdPmxCompatibilitySummary, DEFAULT_VMD_SAMPLE_RATE, MAX_BAKED_FRAME, MMD_FRAME_RATE,
    VMD_BAKE_ALGORITHM_VERSION, VMD_COMPATIBILITY_NEUTRAL_EPSILON,
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
        AnimationMotionLibrary, AnimationMotionLibraryError, AnimationParameterDeclaration,
        AnimationParameterError, AnimationParameterKind, AnimationParameterValue,
        AnimationParameters, Blend1d, Blend1dDefinition, Blend1dError, Blend1dPoint, Blend1dSample,
        ANIMATION_MOTION_SCHEMA_VERSION,
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
        game_system, GameComponent, GameQuerySpec, GameResource, InputAction, SaveKey,
    };
    pub use glam::{Quat, Vec2, Vec3};
}
