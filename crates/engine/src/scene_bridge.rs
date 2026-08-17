//! Temporary authoring-to-runtime scene bridge for Phase 2.5.
//!
//! Converts an [`AuthoringScene`] into runtime ECS entities. This module is
//! a thin convenience layer. Full build-pipeline ownership remains an open
//! decision in the specification.
//!
//! [`AuthoringScene`]: engine_authoring::scene::AuthoringScene

use crate::anim_graph::{
    load_animation_graph_document, load_animation_graph_document_json, AnimGraphPlayer,
    AnimationGraphDebugSource, AnimationMotionDebugBinding,
};
use crate::animation::{
    compose_animation_clips, AnimEvent, AnimationClip, Animator, RootMotionMode, RootMotionRequest,
};
use crate::asset::{
    AssetLoadError, AssetManifest, Assets, Handle, ImportedSubAssetKind, RuntimeAssetId,
};
use crate::authoring_overlay::AuthoringDocumentOverlay;
use crate::audio::{AudioAsset, AudioEmitter, AudioListener, AudioRolloffMode, MusicController};
use crate::behavior_tree::BehaviorTreeRunner;
use crate::camera::{Camera3D, FollowCamera, LockOnCamera, OrbitCamera};
use crate::character_controller::KinematicCharacterController;
use crate::collision::{Collider, CollisionLayers, PhysicsBody, TriggerVolume};
use crate::combat::DamageReceiver;
use crate::components::{builtin_registry, ComponentRegistry, ComponentSpawnError, SpawnContext};
use crate::foot_ik::FootIk;
use crate::game_module::{GameModule, GameModuleResource, GameModuleRunError};
use crate::light::{AmbientLight, DirectionalLight, PointLight, SpotLight};
use crate::lock_on::LockOnTarget;
use crate::lod::{LodGroup, LodLevel};
#[cfg(test)]
use crate::material::{AlphaMode, CullMode, ShadingModel};
use crate::material::{DecodedTexture, Material, MaterialSlots};
use crate::mesh::Mesh;
use crate::navmesh::NavMeshAgent;
use crate::player::{MovePlane, PlayerController, PlayerMarker};
use crate::postprocess::{
    BloomSettings, ColorGradingSettings, PostProcessSettings, ToneMapOperator,
};
use crate::morph::{MorphBaseColor, MorphDirtyVertices, MorphTargets, MorphWeights};
use crate::secondary_motion::{
    SecondaryMotion, SecondaryMotionRigAsset, SecondaryMotionRigRegistry,
};
use crate::runtime_metadata::RuntimeMetadata;
use crate::script_api::RuntimeEntityIdentity;
use crate::shadow::{EnvironmentLighting, ShadowSettings};
use crate::skeleton_asset::{BoneId, SkeletonAsset, SkeletonAssetRegistry};
use crate::skinning::{
    spawn_rig, BoneAttachment, JointPalette, RigSpawnError, Skeleton, SkinnedMesh,
};
use crate::transform::{Children, GlobalTransform, Parent, Transform};
use crate::ui_document::{load_ui_document, UiDocumentRef};
use crate::vfx::{VfxPlayer, VfxRenderBinding, VfxRenderBindings, VfxRestartPolicy};
use engine_authoring::id::{AssetId, ComponentTypeId, EntityId, StableId};
use engine_authoring::scene::AuthoringScene;
use engine_authoring::ui::{UiDocument, UiNode, UiNodeKind, UiString};
use engine_authoring::value::Value;
use engine_authoring::{
    AnimationSet, AuthoringEntity, BehaviorTreeAuthoringService, Diagnostic, DiagnosticTarget,
    Graph, VfxAuthoringService, VfxModuleOperation,
};
use engine_ecs::{Entity, World};
use glam::{EulerRot, Mat4, Quat, Vec3};
use hashbrown::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// The `"engine.player_marker"` component type string recognised by the bridge.
///
/// Authoring entities receive a [`PlayerMarker`] at runtime only when this
/// component has an empty [`Value::Object`] value. Use this constant instead
/// of a raw string literal.
pub const PLAYER_MARKER_COMPONENT: &str = "engine.player_marker";

/// The `"engine.transform"` component type string recognised by the bridge.
///
/// The bridge reads `x`, `y`, `z` numeric fields from this component value to
/// set the initial [`Transform`] translation. Use this constant instead of a
/// raw string literal.
pub const TRANSFORM_COMPONENT: &str = "engine.transform";

/// The unified authoring component for one static mesh render instance.
pub const STATIC_MESH_RENDERER_COMPONENT: &str = "engine.static_mesh_renderer";

/// The unified authoring component for one skeleton-deformed mesh render instance.
pub const SKINNED_MESH_RENDERER_COMPONENT: &str = "engine.skinned_mesh_renderer";

/// The authoring component that owns one character rig and its render parts
/// (ADR 0087).
pub const SKINNED_MODEL_COMPONENT: &str = "engine.skinned_model";

/// The authoring component that mounts an entity on a rig bone (ADR 0088).
pub const BONE_ATTACHMENT_COMPONENT: &str = "engine.bone_attachment";

/// The authoring component that opts an entity into engine-native secondary
/// motion (ADR 0112).
pub const SECONDARY_MOTION_COMPONENT: &str = "engine.secondary_motion";

/// The authorable distance-based mesh LOD group recognised by the bridge.
pub const LOD_GROUP_COMPONENT: &str = "engine.lod_group";

/// The `"engine.camera"` component type string recognised by the bridge.
pub const CAMERA_COMPONENT: &str = "engine.camera";

/// The `"engine.directional_light"` component type string recognised by the bridge.
pub const DIRECTIONAL_LIGHT_COMPONENT: &str = "engine.directional_light";

/// The stable authoring component for a transform-positioned point light.
pub const POINT_LIGHT_COMPONENT: &str = "engine.point_light";

/// The stable authoring component for a transform-oriented spot light.
pub const SPOT_LIGHT_COMPONENT: &str = "engine.spot_light";

/// The `"engine.ambient_light"` component type string recognised by the bridge.
pub const AMBIENT_LIGHT_COMPONENT: &str = "engine.ambient_light";

/// The scene-owned directional shadow settings recognised by the bridge.
pub const SHADOW_SETTINGS_COMPONENT: &str = "engine.shadow_settings";

/// The scene-owned diffuse environment-lighting settings recognised by the bridge.
pub const ENVIRONMENT_LIGHTING_COMPONENT: &str = "engine.environment_lighting";

/// The scene-owned HDR post-processing settings recognised by the bridge.
pub const POST_PROCESS_COMPONENT: &str = "engine.post_process";

/// The `"engine.player_controller"` component type string recognised by the bridge.
pub const PLAYER_CONTROLLER_COMPONENT: &str = "engine.player_controller";

/// The `"engine.orbit_camera"` component type string recognised by the bridge.
pub const ORBIT_CAMERA_COMPONENT: &str = "engine.orbit_camera";

/// The `"engine.follow_camera"` component type string recognised by the bridge.
pub const FOLLOW_CAMERA_COMPONENT: &str = "engine.follow_camera";

/// The `"engine.particle_emitter"` component type string recognised by the bridge.
pub const PARTICLE_EMITTER_COMPONENT: &str = "engine.particle_emitter";

/// The typed scene VFX player component defined by ADR 0125.
pub const VFX_PLAYER_COMPONENT: &str = "engine.vfx_player";

/// The `"engine.ui_document"` component type string recognised by the bridge
/// (Phase 54).
pub const UI_DOCUMENT_COMPONENT: &str = "engine.ui_document";

/// The `"engine.collider"` component type string recognised by the bridge
/// (Phase 57).
pub const COLLIDER_COMPONENT: &str = "engine.collider";

/// The `"engine.physics_body"` component type string recognised by the
/// bridge (Phase 57).
pub const PHYSICS_BODY_COMPONENT: &str = "engine.physics_body";

/// The `"engine.character_controller"` component type string recognised by
/// the bridge (Phase 57).
pub const CHARACTER_CONTROLLER_COMPONENT: &str = "engine.character_controller";

/// The `"engine.damage_receiver"` component type recognized by the bridge.
pub const DAMAGE_RECEIVER_COMPONENT: &str = "engine.damage_receiver";

/// The `"engine.lock_on_target"` component type string recognised by the
/// bridge (Phase 58).
pub const LOCK_ON_TARGET_COMPONENT: &str = "engine.lock_on_target";

/// The `"engine.lock_on_camera"` component type string recognised by the
/// bridge (Phase 58).
pub const LOCK_ON_CAMERA_COMPONENT: &str = "engine.lock_on_camera";
/// The `"engine.nav_mesh_agent"` component type recognized by the bridge.
pub const NAV_MESH_AGENT_COMPONENT: &str = "engine.nav_mesh_agent";
/// The `"engine.nav_mesh_surface"` component type recognized by the bridge.
pub const NAV_MESH_SURFACE_COMPONENT: &str = "engine.nav_mesh_surface";
/// The `"engine.runtime_metadata"` component type recognized by the bridge.
pub const RUNTIME_METADATA_COMPONENT: &str = "engine.runtime_metadata";
/// The unified public animation authoring component recognized by the bridge.
///
/// Conversion expands this component into the separate runtime skeleton,
/// animator, and graph-player components defined by ADR 0082.
pub const ANIMATION_CONTROLLER_COMPONENT: &str = "engine.animation_controller";
/// The `"engine.behavior_tree_runner"` component type recognized by the bridge.
pub const BEHAVIOR_TREE_RUNNER_COMPONENT: &str = "engine.behavior_tree_runner";
/// The `"engine.audio_emitter"` component type recognized by the bridge.
pub const AUDIO_EMITTER_COMPONENT: &str = "engine.audio_emitter";
/// The `"engine.audio_listener"` component type recognized by the bridge.
pub const AUDIO_LISTENER_COMPONENT: &str = "engine.audio_listener";
/// The `"engine.music_controller"` component type recognized by the bridge.
pub const MUSIC_CONTROLLER_COMPONENT: &str = "engine.music_controller";
/// The `"engine.foot_ik"` component type recognized by the bridge (ADR 0080).
pub const FOOT_IK_COMPONENT: &str = "engine.foot_ik";

/// The built-in triangle mesh asset used by the Phase 2.5 bridge.
pub const BUILTIN_TRIANGLE_ASSET_ID: &str = "asset_01JP0000000000000000000101";

/// The built-in quad mesh asset used by the Phase 2.5 bridge.
pub const BUILTIN_QUAD_ASSET_ID: &str = "asset_01JP0000000000000000000102";

/// The built-in blue material asset used by the Phase 2.5 bridge.
pub const BUILTIN_BLUE_MATERIAL_ASSET_ID: &str = "asset_01JP0000000000000000000201";

/// The built-in orange material asset used by the Phase 2.5 bridge.
pub const BUILTIN_ORANGE_MATERIAL_ASSET_ID: &str = "asset_01JP0000000000000000000202";

/// The built-in white material used as the neutral renderer fallback.
pub const BUILTIN_WHITE_MATERIAL_ASSET_ID: &str = "asset_01JP0000000000000000000203";

/// The built-in UI document asset used by the Phase 54 bridge: a minimal
/// document with one "New UI" text label, resolved without a manifest entry.
pub const BUILTIN_UI_DOCUMENT_ASSET_ID: &str = "asset_01JP0000000000000000000501";

/// Maps authoring [`EntityId`] values to their spawned runtime [`Entity`] handles.
///
/// Returned by [`spawn_from_authoring_scene`]. Use [`get`] to look up the
/// runtime entity for an authoring entity so that extra components—meshes,
/// materials, or game-logic tags—can be added after the initial spawn.
///
/// [`get`]: AuthoringToRuntimeMap::get
#[derive(Debug)]
pub struct AuthoringToRuntimeMap {
    entities: HashMap<EntityId, Entity>,
    assets: HashMap<AssetId, RuntimeAssetId>,
    /// Non-fatal diagnostics produced during scene conversion.
    ///
    /// These include recoverable asset issues and additive runtime warnings
    /// such as multiple authorable lights where conversion can still proceed.
    pub asset_diagnostics: Vec<Diagnostic>,
}

impl AuthoringToRuntimeMap {
    /// Returns the runtime [`Entity`] spawned from the authoring entity with
    /// `id`, or `None` when `id` is not present in the source scene.
    pub fn get(&self, id: &EntityId) -> Option<Entity> {
        self.entities.get(id).copied()
    }

    /// Returns the runtime asset ID resolved from `id`, or `None` when the
    /// source scene did not reference that authoring asset.
    pub fn asset(&self, id: &AssetId) -> Option<RuntimeAssetId> {
        self.assets.get(id).copied()
    }

    /// Returns every runtime [`Entity`] this scene spawned, in unspecified
    /// order.
    ///
    /// Hosts that track scene membership across a runtime scene switch (see
    /// [`crate::scene_manager::SceneManager`], Phase 55 / ADR 0047) use this
    /// to record which entities belong to a spawned scene without
    /// duplicating the authoring-to-runtime key set.
    pub fn spawned_entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.entities.values().copied()
    }

    /// Iterates authoring IDs and their corresponding runtime entities.
    ///
    /// Editor observation tools use this read-only view to resolve runtime
    /// transforms back to stable authoring selection without persisting or
    /// exposing runtime entity IDs as project data.
    pub fn entities(&self) -> impl Iterator<Item = (&EntityId, Entity)> + '_ {
        self.entities
            .iter()
            .map(|(authoring, runtime)| (authoring, *runtime))
    }
}

/// Reports why authoring scene conversion could not complete.
#[derive(Debug)]
pub enum SceneBridgeError {
    /// The authoring scene failed semantic validation.
    InvalidScene {
        /// Diagnostics produced by authoring validation.
        diagnostics: Vec<Diagnostic>,
    },
    /// A bridge component had a value with the wrong shape or type.
    InvalidComponentValue {
        /// The entity carrying the invalid component.
        entity: EntityId,
        /// The invalid component type.
        component_type: ComponentTypeId,
        /// A description of the expected value.
        expected: &'static str,
    },
    /// A persisted game component is unavailable in the loaded module.
    MissingGameComponent {
        /// Entity carrying the unresolved component.
        entity: EntityId,
        /// Persisted component type ID.
        component_type: ComponentTypeId,
    },
    /// A loaded game module rejected component conversion.
    GameModule {
        /// Underlying module callback error.
        source: GameModuleRunError,
    },
    /// A referenced asset is not a built-in and is not registered in the manifest.
    UnknownAsset {
        /// The unresolved authoring asset ID.
        asset: AssetId,
    },
    /// A manifest-registered asset file could not be loaded.
    AssetLoad {
        /// The authoring asset ID that failed to load.
        asset: AssetId,
        /// The underlying load error.
        source: AssetLoadError,
    },
    /// Runtime ECS mutation failed after authoring conversion was prepared.
    WorldMutation {
        /// The mutation failure that interrupted bridge application.
        source: engine_ecs::WorldError,
        /// Cleanup failures encountered while rolling back bridge-owned state.
        cleanup_errors: Vec<engine_ecs::WorldError>,
    },
}

impl fmt::Display for SceneBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScene { diagnostics } => write!(
                formatter,
                "authoring scene validation failed with {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::InvalidComponentValue {
                entity,
                component_type,
                expected,
            } => write!(
                formatter,
                "component `{}` on entity `{}` must be {expected}",
                component_type.as_str(),
                entity.as_str()
            ),
            Self::MissingGameComponent {
                entity,
                component_type,
            } => write!(
                formatter,
                "game component `{}` on entity `{}` is not available; build the project game module",
                component_type.as_str(),
                entity.as_str()
            ),
            Self::GameModule { source } => write!(formatter, "game module conversion failed: {source}"),
            Self::UnknownAsset { asset } => {
                write!(
                    formatter,
                    "authoring asset `{}` is not available",
                    asset.as_str()
                )
            }
            Self::AssetLoad { asset, source } => {
                write!(
                    formatter,
                    "failed to load asset `{}`: {source}",
                    asset.as_str()
                )
            }
            Self::WorldMutation {
                source,
                cleanup_errors,
            } => {
                write!(formatter, "runtime world mutation failed: {source}")?;
                if !cleanup_errors.is_empty() {
                    write!(
                        formatter,
                        " (bridge rollback also produced {} error(s))",
                        cleanup_errors.len()
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SceneBridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::WorldMutation { source, .. } => Some(source),
            Self::AssetLoad { source, .. } => Some(source),
            Self::GameModule { source } => Some(source),
            Self::InvalidScene { .. }
            | Self::InvalidComponentValue { .. }
            | Self::MissingGameComponent { .. }
            | Self::UnknownAsset { .. } => None,
        }
    }
}

struct PreparedEntity {
    id: EntityId,
    authoring_entity: AuthoringEntity,
    components: Vec<PreparedComponent>,
}

struct PreparedComponent {
    component_type: ComponentTypeId,
    value: Value,
    is_game: bool,
}

struct ConversionPlan {
    entities: Vec<PreparedEntity>,
}

/// Conversion-local asset caches and ownership needed for atomic rollback.
///
/// Keeping every asset family in one state prevents a new loader from being
/// cached but accidentally omitted from the failure cleanup path.
#[derive(Default)]
pub(crate) struct BridgeAssetState {
    pub(crate) runtime_ids: HashMap<AssetId, RuntimeAssetId>,
    pub(crate) mesh_handles: HashMap<AssetId, Handle<Mesh>>,
    pub(crate) material_handles: HashMap<AssetId, Handle<Material>>,
    pub(crate) animation_clip_handles: HashMap<AssetId, BTreeMap<String, Handle<AnimationClip>>>,
    /// Source asset ID -> the `(sub-asset ID, runtime clip key)` pairs that
    /// source contributed, recorded when its clips were first loaded.
    ///
    /// Exists so a second reference to the same source can resolve *which*
    /// clip a sub-asset ID selects without reaching back into the format
    /// -specific import result: a `.vmd` motion source (ADR 0097 §3) is baked
    /// through `crate::vmd_import`, not `gltf_imports`, so the selector
    /// lookup has to live somewhere both loaders can fill.
    pub(crate) animation_clip_selectors: HashMap<AssetId, Vec<(AssetId, String)>>,
    /// Runtime clip handle -> (glTF source asset ID, clip sub-asset ID),
    /// recorded for every clip loaded while resolving an `engine.animator`
    /// clip source, so the cross-skeleton retarget pass can compose a cache
    /// key from a bare `Handle<AnimationClip>` (ADR 0079 §4).
    pub(crate) animation_clip_sources: HashMap<RuntimeAssetId, (AssetId, AssetId)>,
    /// Runtime composed clip handle -> cache identity assembled from every
    /// ordered source layer. Cross-skeleton retargeting uses this instead of
    /// pretending a composite belongs to only its primary source.
    pub(crate) composed_animation_cache_sources:
        HashMap<RuntimeAssetId, (String, String, AssetId)>,
    pub(crate) audio_handles: HashMap<AssetId, Handle<AudioAsset>>,
    /// Parsed glTF documents shared by mesh, material, skin, and clip loaders
    /// during one atomic scene conversion.
    pub(crate) gltf_imports: HashMap<AssetId, Arc<crate::model_import::GltfImportResult>>,
    /// CPU-decoded glTF images shared by material slots and materials.
    pub(crate) gltf_textures: HashMap<AssetId, Arc<DecodedTexture>>,
    /// Optional cross-conversion cache consulted before parsing or decoding
    /// (ADR 0071). Present when the host world carries a
    /// [`SharedGltfImportCache`] resource.
    pub(crate) shared_gltf_cache: Option<SharedGltfImportCache>,
    /// Optional project-derived cache used to reuse baked VMD clips across
    /// otherwise independent scene conversions (ADR 0079, ADR 0097).
    ///
    /// Conversion-local runtime handles remain owned by this state; only the
    /// immutable serialized bake is shared through [`crate::DerivedCache`].
    pub(crate) derived_cache: Option<crate::DerivedCache>,
    /// Immutable authoring working copies captured by the host for this conversion.
    ///
    /// The bridge never mutates this overlay and never knows about Editor session
    /// types. When an entry exists, loaders must use it instead of rereading disk.
    pub(crate) authoring_overlay: AuthoringDocumentOverlay,
    pub(crate) added_mesh_handles: Vec<Handle<Mesh>>,
    pub(crate) added_material_handles: Vec<Handle<Material>>,
    pub(crate) added_animation_clip_handles: Vec<Handle<AnimationClip>>,
    pub(crate) added_audio_handles: Vec<Handle<AudioAsset>>,
    /// Skeleton asset IDs registered into [`SkeletonAssetRegistry`] while
    /// spawning `engine.skeleton` this conversion (ADR 0080 §2), tracked for
    /// the same atomic-rollback reason as the `added_*_handles` fields above.
    pub(crate) added_skeleton_asset_ids: Vec<AssetId>,
    /// Secondary Motion rig definitions replaced while converting this scene.
    ///
    /// Each entry keeps the previous value, if any, so rollback restores the
    /// registry exactly instead of only removing newly inserted IDs.
    pub(crate) secondary_motion_rig_rollbacks:
        Vec<(AssetId, Option<SecondaryMotionRigAsset>)>,
    /// Skeleton entities are runtime-only and must be removed if any later
    /// component makes the otherwise atomic scene conversion fail.
    pub(crate) auxiliary_entities: Vec<Entity>,
    remove_mesh_store: bool,
    remove_material_store: bool,
    remove_animation_store: bool,
    remove_audio_store: bool,
    remove_skeleton_registry_store: bool,
    remove_secondary_motion_registry_store: bool,
}

/// Spawns one runtime entity for every [`AuthoringEntity`] in `scene`.
///
/// Each spawned entity receives a [`Transform`] and a default
/// [`GlobalTransform`]. The [`Transform`] translation is extracted from the
/// [`TRANSFORM_COMPONENT`] value when present and well-formed. Entities without
/// a transform component use [`Vec3::ZERO`].
///
/// Entities whose authoring data contains a [`PLAYER_MARKER_COMPONENT`]
/// component with an empty object value also receive a [`PlayerMarker`].
///
/// Entities whose authoring data declares a parent receive a [`Parent`]
/// pointing at the runtime parent entity, and every parent receives a
/// [`Children`] list ordered by stable authoring identifier, so
/// [`crate::transform::transform_propagation_system`] can resolve the
/// hierarchy during Play.
///
/// Returns an [`AuthoringToRuntimeMap`] so callers can look up spawned
/// entities by their authoring [`EntityId`] to attach additional components.
/// Authoring validation and conversion planning complete before this function
/// mutates `world`. If runtime ECS mutation fails during application, the
/// bridge attempts to remove every entity and asset it added.
///
/// # Errors
///
/// Returns an error when the scene is invalid, a recognised component has an
/// invalid value, a referenced built-in asset is unknown, or runtime ECS
/// mutation fails.
pub fn spawn_from_authoring_scene(
    world: &mut World,
    scene: &AuthoringScene,
) -> Result<AuthoringToRuntimeMap, SceneBridgeError> {
    spawn_with_policy(world, scene, ComponentFailurePolicy::Abort)
}

/// Diagnostic code reported for every component skipped by
/// [`spawn_from_authoring_scene_best_effort`].
pub const COMPONENT_SKIPPED_DIAGNOSTIC: &str = "scene_bridge.component_skipped";

/// Diagnostic code reported when a component converts to nothing because a
/// schema-required asset reference has not been assigned yet (ADR 0069).
///
/// This is a normal editing state, not an error: the component becomes
/// active as soon as the reference is assigned. Emitted under every
/// conversion policy, including strict Play/runtime conversion.
pub const COMPONENT_INACTIVE_DIAGNOSTIC: &str = "scene_bridge.component_inactive";

/// Converts an authoring scene for editing previews (ADR 0068).
///
/// Unlike [`spawn_from_authoring_scene`], a component whose value fails
/// conversion does not abort the scene. The component is skipped, the rest of
/// the entity and scene keep converting, and each skip is reported as a
/// [`COMPONENT_SKIPPED_DIAGNOSTIC`] warning in
/// [`AuthoringToRuntimeMap::asset_diagnostics`]. This keeps an editing
/// preview visible while a newly added component is still incomplete.
///
/// Play mode, the player, and packaging must keep using the strict
/// [`spawn_from_authoring_scene`] so invalid content cannot run silently.
///
/// # Errors
///
/// Still returns an error when scene-level validation is blocking or when
/// runtime ECS mutation itself fails.
pub fn spawn_from_authoring_scene_best_effort(
    world: &mut World,
    scene: &AuthoringScene,
) -> Result<AuthoringToRuntimeMap, SceneBridgeError> {
    spawn_with_policy(world, scene, ComponentFailurePolicy::SkipAndReport)
}

/// How a component-level conversion failure is handled (ADR 0068).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ComponentFailurePolicy {
    /// Roll back the whole conversion on the first failing component.
    Abort,
    /// Skip the failing component and record a warning diagnostic.
    SkipAndReport,
}

fn spawn_with_policy(
    world: &mut World,
    scene: &AuthoringScene,
    policy: ComponentFailurePolicy,
) -> Result<AuthoringToRuntimeMap, SceneBridgeError> {
    let asset_root = world
        .get_resource::<crate::asset::AssetServer>()
        .map(|s| s.root().to_path_buf());
    let manifest = world
        .get_resource::<AssetManifest>()
        .cloned()
        .unwrap_or_default();
    let registry = builtin_registry();
    let game_module = world
        .get_resource::<GameModuleResource>()
        .map(|resource| Arc::clone(&resource.module));
    let (plan, asset_diagnostics) = prepare_conversion_plan(
        scene,
        asset_root.as_deref(),
        &manifest,
        &registry,
        game_module.as_deref(),
    )?;
    let mut result = apply_conversion_plan(
        world,
        plan,
        asset_root.as_deref(),
        &manifest,
        &registry,
        game_module.as_deref(),
        policy,
    )?;
    result.asset_diagnostics.extend(asset_diagnostics);
    Ok(result)
}

fn prepare_conversion_plan(
    scene: &AuthoringScene,
    _asset_root: Option<&Path>,
    _manifest: &AssetManifest,
    registry: &ComponentRegistry,
    game_module: Option<&GameModule>,
) -> Result<(ConversionPlan, Vec<Diagnostic>), SceneBridgeError> {
    let mut scene_diagnostics = scene.validate();
    scene_diagnostics.extend(crate::render_limits::validate_scene_render_limits(scene));
    if scene_diagnostics.iter().any(Diagnostic::is_blocking) {
        return Err(SceneBridgeError::InvalidScene {
            diagnostics: scene_diagnostics,
        });
    }

    let mut entities = Vec::new();
    let mut asset_diagnostics: Vec<Diagnostic> = Vec::new();
    collect_singleton_component_diagnostics(scene, &mut asset_diagnostics);

    for (id, authoring_entity) in scene.entities() {
        if entity_or_ancestor_disabled(scene, id) {
            continue;
        }
        let mut components = Vec::new();
        for (component_type, value) in &authoring_entity.components {
            if registry.get(component_type).is_some() {
                components.push(PreparedComponent {
                    component_type: component_type.clone(),
                    value: value.clone(),
                    is_game: false,
                });
            } else if game_module
                .and_then(|module| module.component_schema(component_type))
                .is_some()
            {
                components.push(PreparedComponent {
                    component_type: component_type.clone(),
                    value: value.clone(),
                    is_game: true,
                });
            } else if component_type.as_str().starts_with("game.") {
                return Err(SceneBridgeError::MissingGameComponent {
                    entity: id.clone(),
                    component_type: component_type.clone(),
                });
            }
        }

        entities.push(PreparedEntity {
            id: id.clone(),
            authoring_entity: authoring_entity.clone(),
            components,
        });
    }

    Ok((ConversionPlan { entities }, asset_diagnostics))
}

/// Whether an entity or any of its ancestors is disabled (ADR 0070).
///
/// Disabling cascades: a child of a disabled entity never converts even
/// when its own flag is still `true`, matching the SetActive mental model.
fn entity_or_ancestor_disabled(scene: &AuthoringScene, id: &EntityId) -> bool {
    let mut current = Some(id.clone());
    let mut depth = 0;
    while let Some(entity_id) = current {
        // Parent cycles are rejected by scene validation; the guard only
        // keeps conversion terminating if one slips through.
        depth += 1;
        if depth > 512 {
            return false;
        }
        let Some(entity) = scene.entity(&entity_id) else {
            return false;
        };
        if !entity.enabled {
            return true;
        }
        current = entity.parent.clone();
    }
    false
}

fn collect_singleton_component_diagnostics(
    scene: &AuthoringScene,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Renderer-facing resources have exactly one value. Choosing the first
    // component in stable authoring order prevents a later duplicate from
    // silently replacing an already-authored scene contract.
    for (component_id, display_name, diagnostic_code) in [
        (
            DIRECTIONAL_LIGHT_COMPONENT,
            "directional light",
            "scene_bridge.multiple_directional_lights",
        ),
        (
            AMBIENT_LIGHT_COMPONENT,
            "ambient light",
            "scene_bridge.multiple_ambient_lights",
        ),
        (
            SHADOW_SETTINGS_COMPONENT,
            "shadow settings",
            "scene_bridge.multiple_shadow_settings",
        ),
        (
            ENVIRONMENT_LIGHTING_COMPONENT,
            "environment lighting",
            "scene_bridge.multiple_environment_lighting",
        ),
        (
            POST_PROCESS_COMPONENT,
            "post-process settings",
            "scene_bridge.multiple_post_process",
        ),
    ] {
        let component_type = ComponentTypeId::new(component_id);
        let mut matches = scene
            .entities()
            .filter(|(_, entity)| entity.components.contains_key(&component_type));
        let Some((first_id, _)) = matches.next() else {
            continue;
        };
        for (entity_id, _) in matches {
            diagnostics.push(
                Diagnostic::warning(
                    diagnostic_code,
                    format!(
                        "multiple {display_name} components are present; entity `{}` is ignored and entity `{}` drives the renderer resource",
                        entity_id.as_str(),
                        first_id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Component {
                    entity: entity_id.clone(),
                    component_type: component_type.clone(),
                }),
            );
        }
    }
}

fn apply_conversion_plan(
    world: &mut World,
    plan: ConversionPlan,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    registry: &ComponentRegistry,
    game_module: Option<&GameModule>,
    policy: ComponentFailurePolicy,
) -> Result<AuthoringToRuntimeMap, SceneBridgeError> {
    let remove_mesh_store = world.get_resource::<Assets<Mesh>>().is_none();
    let remove_material_store = world.get_resource::<Assets<Material>>().is_none();
    let remove_animation_store = world.get_resource::<Assets<AnimationClip>>().is_none();
    let remove_audio_store = world.get_resource::<Assets<AudioAsset>>().is_none();
    let remove_skeleton_registry_store = world.get_resource::<SkeletonAssetRegistry>().is_none();
    if remove_mesh_store {
        world.insert_resource(Assets::<Mesh>::default());
    }
    if remove_material_store {
        world.insert_resource(Assets::<Material>::default());
    }
    if remove_animation_store {
        world.insert_resource(Assets::<AnimationClip>::default());
    }
    if remove_audio_store {
        world.insert_resource(Assets::<AudioAsset>::default());
    }
    if remove_skeleton_registry_store {
        world.insert_resource(SkeletonAssetRegistry::default());
    }

    let mut asset_state = BridgeAssetState {
        remove_mesh_store,
        remove_material_store,
        remove_animation_store,
        remove_audio_store,
        remove_skeleton_registry_store,
        // Hosts that convert repeatedly (Scene View preview) opt into
        // cross-conversion glTF reuse by inserting this resource (ADR 0071).
        shared_gltf_cache: world.get_resource::<SharedGltfImportCache>().cloned(),
        // VMD baking is substantially more expensive than deserializing its
        // engine-native cached clip, so editor hosts can provide the project
        // cache without changing strict conversion or rollback ownership.
        derived_cache: world.get_resource::<crate::DerivedCache>().cloned(),
        // Editor Play/preview inserts an immutable snapshot before conversion.
        // Absence means this is a normal runtime/player conversion and disk remains the source.
        authoring_overlay: world
            .get_resource::<AuthoringDocumentOverlay>()
            .cloned()
            .unwrap_or_default(),
        ..BridgeAssetState::default()
    };
    let mut asset_diagnostics = Vec::new();
    let mut spawned = Vec::new();

    // Phase 1: allocate all runtime entities so that entity references (e.g.
    // FollowCamera.target) can be resolved in any component order.
    let mut entity_map: HashMap<EntityId, Entity> = HashMap::new();
    for prepared in &plan.entities {
        let entity = match world.spawn_with(Transform::default()) {
            Ok(e) => e,
            Err(source) => {
                rollback_bridge_changes(world, &mut spawned, &asset_state);
                return Err(SceneBridgeError::WorldMutation {
                    source,
                    cleanup_errors: Vec::new(),
                });
            }
        };
        spawned.push(entity);
        if let Err(source) = world.add_component(entity, GlobalTransform::default()) {
            rollback_bridge_changes(world, &mut spawned, &asset_state);
            return Err(SceneBridgeError::WorldMutation {
                source,
                cleanup_errors: Vec::new(),
            });
        }
        if let Err(source) = world.add_component(
            entity,
            RuntimeEntityIdentity {
                authoring_id: prepared.id.clone(),
                name: prepared.authoring_entity.name.clone(),
            },
        ) {
            rollback_bridge_changes(world, &mut spawned, &asset_state);
            return Err(SceneBridgeError::WorldMutation {
                source,
                cleanup_errors: Vec::new(),
            });
        }
        entity_map.insert(prepared.id.clone(), entity);
    }

    // Phase 1.5: attach hierarchy components so transform propagation can
    // follow authoring parent-child relationships at runtime.
    let mut children_by_parent: HashMap<Entity, Vec<Entity>> = HashMap::new();
    for prepared in &plan.entities {
        let Some(parent_id) = &prepared.authoring_entity.parent else {
            continue;
        };
        let entity = *entity_map
            .get(&prepared.id)
            .expect("entity must be in map after phase 1");
        // Blocking `scene.missing_parent` validation ran during planning, so
        // every declared parent has a runtime entity.
        let parent_entity = *entity_map
            .get(parent_id)
            .expect("validated scene parent must be in the entity map");
        if let Err(source) = world.add_component(entity, Parent(parent_entity)) {
            let cleanup_errors = rollback_bridge_changes(world, &mut spawned, &asset_state);
            return Err(SceneBridgeError::WorldMutation {
                source,
                cleanup_errors,
            });
        }
        children_by_parent
            .entry(parent_entity)
            .or_default()
            .push(entity);
    }
    // Child vectors already follow the deterministic stable-identifier order
    // of the plan; this pass only attaches them to their parents.
    for prepared in &plan.entities {
        let entity = *entity_map
            .get(&prepared.id)
            .expect("entity must be in map after phase 1");
        let Some(children) = children_by_parent.remove(&entity) else {
            continue;
        };
        if let Err(source) = world.add_component(entity, Children(children)) {
            let cleanup_errors = rollback_bridge_changes(world, &mut spawned, &asset_state);
            return Err(SceneBridgeError::WorldMutation {
                source,
                cleanup_errors,
            });
        }
    }

    // Phase 2: apply authoring components with the complete entity map.
    for prepared in &plan.entities {
        let entity = *entity_map
            .get(&prepared.id)
            .expect("entity must be in map after phase 1");
        for component in &prepared.components {
            if component.is_game {
                let module = game_module.expect("prepared game component requires loaded module");
                if let Err(source) = module.spawn_component(
                    world,
                    entity,
                    &component.component_type,
                    &component.value,
                ) {
                    if policy == ComponentFailurePolicy::SkipAndReport {
                        asset_diagnostics.push(component_skipped_diagnostic(
                            &prepared.id,
                            &component.component_type,
                            &SceneBridgeError::GameModule { source },
                        ));
                        continue;
                    }
                    rollback_bridge_changes(world, &mut spawned, &asset_state);
                    return Err(SceneBridgeError::GameModule { source });
                }
                continue;
            }
            let definition = registry
                .get(&component.component_type)
                .expect("prepared component type must be registered");
            let mut context = SpawnContext {
                world: &mut *world,
                authoring_entity: &prepared.authoring_entity,
                asset_root,
                manifest,
                entity_map: &entity_map,
                asset_diagnostics: &mut asset_diagnostics,
                asset_state: &mut asset_state,
            };
            match (definition.spawn)(entity, &component.value, &mut context) {
                Ok(()) => {}
                // World mutation failures abort under every policy: the ECS
                // state is no longer trustworthy, unlike an authoring value
                // that merely failed validation.
                Err(ComponentSpawnError::World(source)) => {
                    let cleanup_errors = rollback_bridge_changes(world, &mut spawned, &asset_state);
                    return Err(SceneBridgeError::WorldMutation {
                        source,
                        cleanup_errors,
                    });
                }
                Err(ComponentSpawnError::Bridge(error)) => {
                    if policy == ComponentFailurePolicy::SkipAndReport {
                        asset_diagnostics.push(component_skipped_diagnostic(
                            &prepared.id,
                            &component.component_type,
                            &error,
                        ));
                        continue;
                    }
                    rollback_bridge_changes(world, &mut spawned, &asset_state);
                    return Err(error);
                }
            }
        }
    }

    // Phase 2.5: mount bone attachments (ADR 0088 §2). Deferred to here for
    // the same reason as Phase 3: the referenced rig entity's `Skeleton` is
    // not guaranteed to exist while the attachment's own component is being
    // dispatched.
    resolve_bone_attachments(world, &mut asset_diagnostics);

    // Phase 3: cross-skeleton animator clip resolution (ADR 0079 §4). This
    // runs after every component on every entity has been dispatched because
    // an entity's `engine.skeleton` component is not guaranteed to exist yet
    // during `engine.animator`'s own dispatch (components apply in
    // `ComponentTypeId` order, and `"engine.animator"` sorts before
    // `"engine.skeleton"`).
    resolve_cross_skeleton_animator_clips(
        world,
        &entity_map,
        asset_root,
        manifest,
        &mut asset_state,
        &mut asset_diagnostics,
    );

    Ok(AuthoringToRuntimeMap {
        entities: entity_map,
        assets: asset_state.runtime_ids,
        asset_diagnostics,
    })
}

/// Resolves every entity carrying both a [`Skeleton`] and an [`Animator`]
/// whose clip was authored against a *different* skeleton (ADR 0079 §4).
///
/// For each such entity: a cache hit loads the baked retargeted clip; a miss
/// calls [`crate::retarget::retarget_clip`] synchronously and caches the
/// result via [`crate::derived_cache::DerivedCache`], then the entity's
/// [`Animator::clip`] is repointed at the (possibly new) runtime handle. When
/// resolution cannot proceed — no [`crate::retarget::RetargetMap`] registered
/// for the pair, a stale map (ADR 0079 §1), or the skeleton data could not be
/// located in this conversion pass — a [`crate::retarget::RETARGET_MAP_MISSING_DIAGNOSTIC`]
/// (or `anim.retarget_map_stale`) diagnostic is recorded and the entity's
/// [`Animator`] is removed outright: the clip is never applied
/// cross-skeleton silently.
///
/// This pass runs in two modes, selected by whether
/// [`crate::retarget::PackagedBakedClips`] is present in `world` (ADR 0079
/// §4): absent (editor Play, tests, tools) resolves through the bake-or-cache
/// path, identical to before AP-8; present (the shipped `player` binary)
/// loads a pre-baked clip from the package's `baked_anim/` directory and
/// never bakes or writes to the derived cache, reporting
/// [`crate::retarget::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC`] on a
/// miss instead. See [`resolve_cross_skeleton_clip`] for the branch itself.
/// Reparents every `BoneAttachment` onto the joint entity it names
/// (ADR 0088 §2).
///
/// Reparenting rather than per-frame copying means transform propagation
/// moves the attached entity, and everything under it, with the bone at no
/// recurring cost. A rig that no longer exists or a bone the rig does not
/// carry leaves the entity exactly where it was, with a diagnostic: an
/// attachment that cannot resolve must not move anything.
fn resolve_bone_attachments(world: &mut World, asset_diagnostics: &mut Vec<Diagnostic>) {
    let attachments: Vec<(Entity, Entity, BoneId)> = world
        .entities()
        .filter_map(|entity| {
            world
                .get_component::<BoneAttachment>(entity)
                .map(|attachment| (entity, attachment.rig, attachment.bone))
        })
        .collect();

    for (entity, rig, bone) in attachments {
        let Some(joint) = world
            .get_component::<Skeleton>(rig)
            .and_then(|skeleton| skeleton.joint_of(bone))
        else {
            asset_diagnostics.push(Diagnostic::warning(
                "scene_bridge.bone_attachment_unresolved_bone",
                format!(
                    "bone {} is not part of the referenced rig; the attached entity keeps its own placement",
                    bone.0
                ),
            ));
            continue;
        };
        if let Err(error) = reparent_onto_joint(world, entity, joint) {
            asset_diagnostics.push(Diagnostic::warning(
                "scene_bridge.bone_attachment_unresolved_bone",
                format!("bone attachment could not be mounted: {error}"),
            ));
        }
    }
}

/// Moves `entity` under `joint`, detaching it from any previous parent.
fn reparent_onto_joint(
    world: &mut World,
    entity: Entity,
    joint: Entity,
) -> Result<(), engine_ecs::WorldError> {
    if let Some(previous) = world.get_component::<Parent>(entity).map(|parent| parent.0) {
        if previous == joint {
            return Ok(());
        }
        if let Some(children) = world.get_component_mut::<Children>(previous) {
            children.0.retain(|child| *child != entity);
        }
    }
    if let Some(parent) = world.get_component_mut::<Parent>(entity) {
        parent.0 = joint;
    } else {
        world.add_component(entity, Parent(joint))?;
    }
    if let Some(children) = world.get_component_mut::<Children>(joint) {
        if !children.0.contains(&entity) {
            children.0.push(entity);
        }
    } else {
        world.add_component(joint, Children(vec![entity]))?;
    }
    Ok(())
}

fn resolve_cross_skeleton_animator_clips(
    world: &mut World,
    entity_map: &HashMap<EntityId, Entity>,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
    asset_diagnostics: &mut Vec<Diagnostic>,
) {
    // Read once and clone (a single `PathBuf`) rather than holding a borrow
    // of `world` across the loop below, which also needs `world` mutably to
    // repoint or remove each entity's `Animator`.
    let packaged_baked_clips = world
        .get_resource::<crate::retarget::PackagedBakedClips>()
        .cloned();

    for &entity in entity_map.values() {
        let Some(target_skeleton_id) = world
            .get_component::<Skeleton>(entity)
            .and_then(|skeleton| skeleton.asset.clone())
        else {
            continue;
        };
        let Some(active_clip) = world
            .get_component::<Animator>(entity)
            .map(|animator| animator.clip)
        else {
            continue;
        };
        let mut clip_handles = vec![active_clip];
        if let Some(player) = world.get_component::<AnimGraphPlayer>(entity) {
            for (_, handle) in player.clip_bindings() {
                if !clip_handles.contains(&handle) {
                    clip_handles.push(handle);
                }
            }
        }

        let mut replacements = Vec::new();
        let mut failed = false;
        for clip_handle in clip_handles {
            let Some(clip) = world
                .get_resource::<Assets<AnimationClip>>()
                .and_then(|clips| clips.get(&clip_handle))
                .cloned()
            else {
                continue;
            };
            let Some(source_skeleton_id) = clip.skeleton.clone() else {
                continue;
            };
            if source_skeleton_id == target_skeleton_id {
                continue;
            }

            match resolve_cross_skeleton_clip(
                &clip,
                &clip_handle,
                &source_skeleton_id,
                &target_skeleton_id,
                asset_root,
                manifest,
                asset_state,
                packaged_baked_clips.as_ref(),
            ) {
                Ok(retargeted) => {
                    let handle = world
                        .get_resource_mut::<Assets<AnimationClip>>()
                        .expect(
                            "animation asset store must exist during the retarget resolution pass",
                        )
                        .add(retargeted);
                    asset_state.added_animation_clip_handles.push(handle);
                    replacements.push((clip_handle, handle));
                }
                Err(diagnostic) => {
                    asset_diagnostics.push(*diagnostic);
                    failed = true;
                    break;
                }
            }
        }

        if failed {
            // No graph motion may play on a mismatched skeleton silently. A
            // failed binding invalidates the controller as a whole, even when
            // its current entry motion happened to be compatible.
            let _ = world.remove_component::<Animator>(entity);
            let _ = world.remove_component::<AnimGraphPlayer>(entity);
            continue;
        }
        for (old, new) in replacements {
            if let Some(animator) = world.get_component_mut::<Animator>(entity) {
                animator.replace_clip_handle(old, new);
            }
            if let Some(player) = world.get_component_mut::<AnimGraphPlayer>(entity) {
                player.replace_clip_handle(old, new);
            }
        }
    }
}

/// Looks up a registered [`crate::retarget::RetargetMap`] for
/// `(source_skeleton_id, target_skeleton_id)` and resolves the retargeted
/// clip, or returns the [`Diagnostic`] explaining why it could not (see
/// [`resolve_cross_skeleton_animator_clips`]).
///
/// `packaged_baked_clips` selects the resolution mode (ADR 0079 §4): `None`
/// resolves through the bake-or-cache path
/// ([`crate::retarget::resolve_or_bake_retargeted_clip`]), unchanged from
/// before AP-8. `Some` (the shipped player) computes the identical cache key
/// and looks the baked clip up under
/// [`crate::retarget::PackagedBakedClips::root`] instead: a hit deserializes
/// and returns it, a miss (file absent, or fails to deserialize) reports
/// [`crate::retarget::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC`] — the
/// player never bakes at runtime and never writes to the derived cache.
// Every parameter is an independently required input (the cache-key inputs
// mirror `cache_key_for_retargeted_clip`'s own audited argument list, ADR
// 0079 §3); bundling them would hide which one a future caller forgot.
#[allow(clippy::too_many_arguments)]
fn resolve_cross_skeleton_clip(
    clip: &AnimationClip,
    clip_handle: &Handle<AnimationClip>,
    source_skeleton_id: &AssetId,
    target_skeleton_id: &AssetId,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
    packaged_baked_clips: Option<&crate::retarget::PackagedBakedClips>,
) -> Result<AnimationClip, Box<Diagnostic>> {
    let missing_diagnostic = |message: String| {
        Box::new(
            Diagnostic::error(crate::retarget::RETARGET_MAP_MISSING_DIAGNOSTIC, message)
                .with_target(DiagnosticTarget::Asset {
                    id: target_skeleton_id.clone(),
                }),
        )
    };
    let target_skeleton = resolve_retarget_skeleton_asset(
        target_skeleton_id,
        asset_root,
        manifest,
        asset_state,
    )
    .ok_or_else(|| {
        missing_diagnostic(format!(
            "entity's skeleton `{}` data could not be loaded for this conversion; clip not applied",
            target_skeleton_id.as_str()
        ))
    })?;

    let source_skeleton = resolve_retarget_skeleton_asset(
        source_skeleton_id,
        asset_root,
        manifest,
        asset_state,
    );

    // A cache can retain an older skeleton asset ID after a model reimport.
    // Only use the structural-identity fallback when that old source ID no
    // longer resolves at all. If both skeleton assets still exist, preserve
    // the explicit cross-skeleton path even when their structures happen to
    // be identical, because a registered retarget map may still be required.
    if source_skeleton.is_none()
        && clip.skeleton_identity == Some(target_skeleton.identity)
    {
        let mut rebound = clip.clone();
        rebound.skeleton = Some(target_skeleton_id.clone());
        rebound.skeleton_identity = Some(target_skeleton.identity);
        return Ok(rebound);
    }

    let source_skeleton = source_skeleton.ok_or_else(|| {
        missing_diagnostic(format!(
            "animator clip targets skeleton `{}` but its skeleton data could not be loaded for this conversion; clip not applied",
            source_skeleton_id.as_str()
        ))
    })?;

    let assets_root = asset_root.unwrap_or_else(|| Path::new("."));
    let maps = crate::retarget::load_registered_retarget_maps(assets_root, manifest);
    let map = crate::retarget::find_retarget_map_for_pair(&maps, source_skeleton_id, target_skeleton_id)
        .ok_or_else(|| {
            missing_diagnostic(format!(
                "animator clip targets skeleton `{}` but entity uses skeleton `{}` and no retarget map resolves this pair; clip not applied",
                source_skeleton_id.as_str(),
                target_skeleton_id.as_str()
            ))
        })?;

    let mut stale_diagnostics = map.validate(source_skeleton.identity, target_skeleton.identity);
    if let Some(diagnostic) = stale_diagnostics.pop() {
        return Err(Box::new(diagnostic));
    }

    let (source_id, clip_sub_asset_id) = asset_state
        .animation_clip_sources
        .get(&clip_handle.id())
        .cloned()
        .unwrap_or_else(|| (source_skeleton_id.clone(), source_skeleton_id.clone()));
    let composed_source = asset_state
        .composed_animation_cache_sources
        .get(&clip_handle.id())
        .cloned();
    // A missing fingerprint must block resolution rather than fall back to
    // the clip sub-asset ID: that fallback is only unique within this
    // session and would poison the on-disk derived-clip cache across
    // sessions (AP-6; ADR 0079 §3 cache key requires the source fingerprint).
    let source_fingerprint = if let Some((fingerprint, _, _)) = &composed_source {
        fingerprint.clone()
    } else {
        manifest
            .get(&source_id)
            .and_then(|entry| entry.import_settings.source_fingerprint.clone())
            .ok_or_else(|| {
                Box::new(
                    Diagnostic::error(
                        crate::retarget::RETARGET_SOURCE_UNFINGERPRINTED_DIAGNOSTIC,
                        format!(
                            "animator clip's source `{}` has no recorded fingerprint; reimport the source to record one before cross-skeleton retargeting can resolve `{}` -> `{}`; clip not applied",
                            source_id.as_str(),
                            source_skeleton_id.as_str(),
                            target_skeleton_id.as_str()
                        ),
                    )
                    .with_target(DiagnosticTarget::Asset {
                        id: source_id.clone(),
                    }),
                )
            })?
    };
    let clip_cache_selector = composed_source
        .as_ref()
        .map(|(_, selector, _)| selector.as_str())
        .unwrap_or_else(|| clip_sub_asset_id.as_str());

    // Baked contact intervals are re-detected against the target skeleton
    // (ADR 0080 §1), so the override that must apply is the *target's own*
    // source's `contact_bones`, not the clip's source. Found via the
    // manifest entry whose recorded skeleton ledger contains this skeleton
    // (the dedupe rule in ADR 0077 §4 means that is not always the entry the
    // skeleton is "conceptually about").
    let target_contact_bones = manifest
        .iter()
        .find(|(_, entry)| {
            entry
                .import_settings
                .skeleton_records
                .iter()
                .any(|record| record.id == target_skeleton_id.as_str())
        })
        .map(|(_, entry)| entry.import_settings.contact_bones.clone())
        .unwrap_or_default();

    if let Some(packaged) = packaged_baked_clips {
        // Same audited key fn as the bake-or-cache path: this is what makes
        // the file packaging staged under `baked_anim/` findable here without
        // recomputing or duplicating any of its inputs.
        let key = crate::retarget::cache_key_for_retargeted_clip(
            &source_fingerprint,
            clip_cache_selector,
            source_skeleton.identity,
            target_skeleton.identity,
            map,
            &target_contact_bones,
        )
        .map_err(|error| {
            missing_diagnostic(format!(
                "failed to compute the retarget cache key for a packaged clip lookup: {error}"
            ))
        })?;
        let file_name = format!(
            "{}.{}",
            key.file_stem(),
            crate::retarget::BAKED_CLIP_FILE_EXTENSION
        );
        let path = packaged.root.join(&file_name);
        // No runtime bake and no cache write here: a miss means AP-7's
        // reachability trace or the map's `always_package` flag is wrong,
        // which must surface loudly rather than self-heal by baking into a
        // (possibly read-only) install directory.
        return std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::retarget::deserialize_baked_clip(&bytes).ok())
            .ok_or_else(|| {
                Box::new(
                    Diagnostic::error(
                        crate::retarget::RETARGET_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC,
                        format!(
                            "packaged baked clip `{file_name}` for skeleton `{}` -> `{}` was not found under `{}`; re-package the project or set `always_package` on the retarget map",
                            source_skeleton_id.as_str(),
                            target_skeleton_id.as_str(),
                            packaged.root.display()
                        ),
                    )
                    .with_target(DiagnosticTarget::Asset {
                        id: target_skeleton_id.clone(),
                    }),
                )
            });
    }

    let project_root = assets_root.parent().unwrap_or(assets_root);
    let cache = crate::derived_cache::DerivedCache::new(project_root);
    crate::retarget::resolve_or_bake_retargeted_clip(
        &cache,
        clip,
        &source_fingerprint,
        clip_cache_selector,
        &source_skeleton,
        &target_skeleton,
        map,
        &target_contact_bones,
    )
    .map_err(|error| {
        missing_diagnostic(format!(
            "failed to retarget animator clip from skeleton `{}` to `{}`: {error}; clip not applied",
            source_skeleton_id.as_str(),
            target_skeleton_id.as_str()
        ))
    })
}

/// Resolves skeleton data needed by cross-skeleton animation conversion.
///
/// Normal scene conversion usually imports both skeletons while expanding
/// scene components. Isolated editor previews can intentionally omit the
/// source model entity, so fall back to the manifest's persistent skeleton
/// ledger and import that owning model source on demand.
fn resolve_retarget_skeleton_asset(
    wanted: &AssetId,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    asset_state: &mut BridgeAssetState,
) -> Option<SkeletonAsset> {
    if let Some(skeleton) = asset_state.gltf_imports.values().find_map(|imported| {
        imported
            .skins
            .iter()
            .find(|skin| &skin.skeleton.id == wanted)
            .map(|skin| skin.skeleton.clone())
    }) {
        return Some(skeleton);
    }

    let (source_id, entry) = manifest.iter().find(|(_, entry)| {
        entry
            .import_settings
            .skeleton_records
            .iter()
            .any(|record| record.id == wanted.as_str())
    })?;
    let source_path = asset_root
        .unwrap_or_else(|| Path::new("."))
        .join(&entry.path);
    let imported = import_gltf_cached(
        source_id,
        &source_path,
        &entry.import_settings.skeleton_records,
        &entry.import_settings.contact_bones,
        asset_state,
    )
    .ok()?;

    imported
        .skins
        .iter()
        .find(|skin| &skin.skeleton.id == wanted)
        .map(|skin| skin.skeleton.clone())
}

/// Builds the warning recorded when a component stays inactive because a
/// required asset reference is not assigned yet (ADR 0069).
pub(crate) fn component_inactive_diagnostic(
    entity: &AuthoringEntity,
    component_type: &ComponentTypeId,
    field: &str,
) -> Diagnostic {
    Diagnostic::warning(
        COMPONENT_INACTIVE_DIAGNOSTIC,
        format!(
            "component `{}` on entity `{}` is inactive: `{field}` is not assigned",
            component_type.as_str(),
            entity.id.as_str()
        ),
    )
}

/// Builds the warning recorded when best-effort conversion skips a component.
fn component_skipped_diagnostic(
    entity: &EntityId,
    component_type: &ComponentTypeId,
    error: &SceneBridgeError,
) -> Diagnostic {
    Diagnostic::warning(
        COMPONENT_SKIPPED_DIAGNOSTIC,
        format!(
            "component `{}` on entity `{}` was skipped: {error}",
            component_type.as_str(),
            entity.as_str()
        ),
    )
}

fn rollback_bridge_changes(
    world: &mut World,
    spawned: &mut Vec<Entity>,
    assets: &BridgeAssetState,
) -> Vec<engine_ecs::WorldError> {
    let mut errors = Vec::new();
    for entity in assets.auxiliary_entities.iter().rev() {
        if let Err(error) = world.despawn(*entity) {
            errors.push(error);
        }
    }
    while let Some(entity) = spawned.pop() {
        if let Err(error) = world.despawn(entity) {
            errors.push(error);
        }
    }
    if let Some(meshes) = world.get_resource_mut::<Assets<Mesh>>() {
        for handle in &assets.added_mesh_handles {
            meshes.remove(handle);
        }
    }
    if let Some(materials) = world.get_resource_mut::<Assets<Material>>() {
        for handle in &assets.added_material_handles {
            materials.remove(handle);
        }
    }
    if let Some(clips) = world.get_resource_mut::<Assets<AnimationClip>>() {
        for handle in &assets.added_animation_clip_handles {
            clips.remove(handle);
        }
    }
    if let Some(audio) = world.get_resource_mut::<Assets<AudioAsset>>() {
        for handle in &assets.added_audio_handles {
            audio.remove(handle);
        }
    }
    if let Some(skeletons) = world.get_resource_mut::<SkeletonAssetRegistry>() {
        for id in &assets.added_skeleton_asset_ids {
            skeletons.remove(id);
        }
    }
    if let Some(rigs) = world.get_resource_mut::<SecondaryMotionRigRegistry>() {
        for (id, previous) in assets.secondary_motion_rig_rollbacks.iter().rev() {
            if let Some(previous) = previous {
                rigs.insert(previous.clone());
            } else {
                rigs.remove(id);
            }
        }
    }
    if assets.remove_mesh_store {
        world.remove_resource::<Assets<Mesh>>();
    }
    if assets.remove_material_store {
        world.remove_resource::<Assets<Material>>();
    }
    if assets.remove_animation_store {
        world.remove_resource::<Assets<AnimationClip>>();
    }
    if assets.remove_audio_store {
        world.remove_resource::<Assets<AudioAsset>>();
    }
    if assets.remove_skeleton_registry_store {
        world.remove_resource::<SkeletonAssetRegistry>();
    }
    if assets.remove_secondary_motion_registry_store {
        world.remove_resource::<SecondaryMotionRigRegistry>();
    }
    errors
}

mod asset_load;
mod fields;
mod gltf_cache;
mod spawn;
#[cfg(test)]
mod tests;

use asset_load::*;
use fields::*;
pub use gltf_cache::SharedGltfImportCache;
pub(crate) use spawn::*;
