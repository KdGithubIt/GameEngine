//! Authoring data model for the game engine.
//!
//! This crate defines the editable source of truth for scenes, entities,
//! components, and graphs. It is intentionally independent of the runtime ECS,
//! GUI frameworks, CLI transport, and MCP transport.
//!
//! Runtime identifiers such as `Entity` and `RuntimeAssetId` are not part of
//! this crate's data model. See `AI_FRIENDLY_AUTHORING_SPEC.md` §5.2 for the
//! build pipeline boundary.
//!
//! ## Phase 2: Commands and Transactions
//!
//! Mutations to [`AuthoringScene`] MUST go through a [`Transaction`]:
//!
//! ```
//! use engine_authoring::scene::AuthoringScene;
//! use engine_authoring::transaction::Transaction;
//! use engine_authoring::command::AuthoringCommand;
//! use engine_authoring::id::EntityId;
//!
//! let mut scene = AuthoringScene::new();
//! let id = EntityId::generate();
//!
//! let mut tx = Transaction::begin(&scene);
//! tx.apply(AuthoringCommand::CreateEntity {
//!     id: id.clone(),
//!     name: "player".into(),
//!     parent: None,
//! });
//! tx.commit(&mut scene).expect("valid transaction must commit");
//! assert!(scene.entity(&id).is_some());
//! ```

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod access;
pub mod animation_graph;
pub mod animation_set;
// The stateful wrapper delegates only part of this compatibility surface.
#[path = "behavior_tree_stateful.rs"]
pub mod behavior_tree;
#[allow(dead_code)]
#[path = "behavior_tree.rs"]
mod behavior_tree_legacy;
pub mod capability;
pub mod command;
pub mod component_metadata;
pub mod diagnostic;
pub mod entity;
mod explicit_game_fields;
pub mod game_project;
pub mod graph;
pub mod graph_domain;
pub mod graph_routing;
pub mod graph_view;
pub mod id;
pub mod load;
pub mod material_asset;
/// Native 2D project settings and stable sorting-layer contracts (ADR 0127).
pub mod native_2d;
/// Native 2D sprite, animation, and tile document contracts (ADR 0127).
pub mod native_2d_assets;
/// GUI-free Native 2D authoring services shared by every client.
pub mod native_2d_services;
pub mod persist;
pub mod prefab;
pub mod prefab_authoring;
pub mod project;
pub mod project_settings;
pub mod scene;
pub mod scene_authoring;
pub mod schema;
pub mod test_fixtures;
pub mod timeline;
pub mod transaction;
pub mod typed_document_authoring;
pub mod ui;
pub mod ui_contract;
pub mod ui_contract_runtime;
pub mod ui_edit;
pub mod validation;
pub mod value;
pub mod vfx;

pub use access::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
pub use animation_graph::{
    ANIMATION_STATE_PLAYBACK_MODE_PROPERTY, AnimState, AnimTransition, AnimationGraphDomain,
    AnimationStatePlaybackMode, CompiledAnimGraph, MOTION_SLOTS_ANNOTATION, MotionSlot,
    animation_graph_motion_slots, compile_animation_graph, motion_slots_annotation_value,
};
pub use animation_set::{
    ANIMATION_SET_FILE_SUFFIX, ANIMATION_SET_SCHEMA_VERSION, AnimationBinding, AnimationSet,
    AnimationSetError, AnimationSetEvent, MotionSourceRef,
};
pub use behavior_tree::{
    BehaviorTreeApply, BehaviorTreeAuthoringService, BehaviorTreeCompilation, BehaviorTreeDomain,
    BehaviorTreeEdgeSummary, BehaviorTreeExample, BehaviorTreeLayout, BehaviorTreeNodeKind,
    BehaviorTreeNodeSummary, BehaviorTreeSchemaCatalog, BehaviorTreeServiceError,
    BehaviorTreeValidation, CompiledBehaviorNode, CompiledBehaviorTree,
};
pub use capability::{
    AuthoringCapability, AuthoringCapabilityError, AuthoringCapabilityExposure,
    AuthoringCapabilityId, AuthoringCapabilityIdError, AuthoringCapabilityKind,
    AuthoringCapabilityRegistry, AuthoringCapabilitySummary, AuthoringDocumentKind, AuthoringDomain,
    AuthoringSchemaRef, AuthoringTransactionRequirement, RESERVED_CAPABILITY_NAMESPACE,
};
pub use command::{AuthoringCommand, Change, CommandResult, PropertyPath, PropertyPathSegment};
pub use component_metadata::{
    COMPONENT_METADATA_SCHEMA_VERSION, ComponentMetadata, ComponentMetadataError,
    component_metadata_path, load_component_metadata, validate_project_component_id,
    write_component_metadata,
};
pub use diagnostic::{Diagnostic, DiagnosticTarget, Severity};
pub use entity::AuthoringEntity;
pub use explicit_game_fields::{create_rust_script, create_rust_script_in};
pub use game_project::{
    GameProjectError, RECOMMENDED_RUST_SCRIPT_FOLDERS, RustDeclaration, RustDeclarationKind,
    RustScriptKind, RustScriptSchedule, create_rhai_script_in, duplicate_rust_component,
    initialize_game_project, move_rust_script, refresh_game_module_indexes, rust_declarations,
};
pub use graph::authoring_service::{
    GraphAuthoringError, GraphAuthoringMutation, GraphAuthoringService, GraphAuthoringSnapshot,
    GraphAuthoringValidation,
};
pub use graph::{
    Annotations, DottedIdError, Edge, Graph, GraphChange, GraphCommand, GraphCommandResult,
    GraphKind, GraphSaveError, GraphSchemaRegistry, GraphTransaction, GraphTransactionCommit,
    GraphTransactionError, GraphTransactionPrivateCommit, GraphTransactionValidationError, Group,
    Node, NodeSchema, NodeTypeId, PortArity, PortDirection, PortRef, PortSchema, PortValueTypeId,
};
pub use graph_domain::{
    GraphCommandApplication, GraphDomain, TestGraphDomain, apply_graph_commands_with_domain,
    validate_graph_with_domain,
};
pub use graph_routing::{AuthoringGraphDomain, UnsupportedGraphKind};
pub use graph_view::authoring_service::{
    GraphViewAuthoringError, GraphViewAuthoringMutation, GraphViewAuthoringService,
    GraphViewAuthoringSnapshot, GraphViewAuthoringValidation,
};
pub use graph_view::{
    GraphView, GraphViewChange, GraphViewCommand, GraphViewCommandResult, GraphViewSaveError,
    GraphViewTransaction, GraphViewTransactionError, GroupLayout, LayoutPolicyId, NodeLayout, Rect,
    Selection, Vec2, Viewport,
};
pub use id::{
    AssetId, ComponentTypeId, ComponentTypeIdError, EdgeId, EntityId, GraphId, GroupId, IdError,
    MotionSlotId, NodeId, PortId, ProjectId, StableId,
};
pub use load::{SceneLoadError, load_scene_from_json};
pub use material_asset::{
    LinearRgba, MATERIAL_SCHEMA_VERSION, MaterialAlphaMode, MaterialAsset, MaterialAssetError,
    MaterialCullMode, MaterialOutline, MaterialShadingModel, MaterialSphereBlendMode,
    MaterialSphereCoordinateSource, ToonLitProperties,
};
pub use native_2d::{
    PixelPreviewPolicy, Project2dSettings, SortingLayer, SortingLayerId, SortingLayerIdError,
    SpriteFiltering,
};
pub use native_2d_assets::{
    Native2dIdError, PixelRect, PixelsPerUnit, SPRITE_ANIMATION_SCHEMA_VERSION,
    SPRITE_ATLAS_SCHEMA_VERSION, SpriteAnimationDocument, SpriteAnimationFrame, SpriteAnimator2d,
    SpriteAtlasDocument, SpriteBlendMode, SpriteId, SpriteRef, SpriteRegion, SpriteRenderer2d,
    TILE_MAP_SCHEMA_VERSION, TILE_SET_SCHEMA_VERSION, TileCell, TileCellEntry, TileChunk,
    TileChunkCoord, TileCollisionMaterial, TileCollisionShape, TileDefinition, TileId, TileLayerId,
    TileMapDocument, TileMapLayer, TileSetDocument,
};
pub use native_2d_services::{
    Native2dAuthoringError, SpriteAtlasAuthoringService, TileMapAuthoringService, TileMapChunkKey,
    TileMapGestureCommit, TileRect, TileStamp,
};
pub use persist::{PersistError, PersistOperation, replace_file_contents};
pub use prefab::{PREFAB_SCHEMA_VERSION, PrefabAsset, PrefabError, PrefabInstantiation};
pub use prefab_authoring::{
    PREFAB_INSTANCE_COMPONENT, PREFAB_INSTANCE_SOURCE_FIELD, PrefabAuthoringError,
    PrefabAuthoringService, PrefabInstanceSource, PrefabInstantiationMutation,
    PrefabInstantiationRequest, PrefabSourceError, PrefabSourcePath, prefab_instance_marker,
    prefab_instance_source,
};
pub use project::{PROJECT_SCHEMA_VERSION, ProjectConfig, ProjectError, ProjectRoot};
pub use project_settings::{
    AxisBinding, InputAction, KeyAxisBinding, Layer, PROJECT_SETTINGS_SCHEMA_VERSION,
    ProjectSettings, ProjectSettingsError,
};
pub use scene::{AuthoringScene, SceneSaveError};
pub use scene_authoring::{
    SceneAuthoringError, SceneAuthoringMutation, SceneAuthoringService, SceneAuthoringSnapshot,
    SceneAuthoringValidation,
};
pub use schema::{ComponentSchema, ComponentSchemaRegistry, FieldSchema, FieldType};
pub use timeline::{
    DisplayFrameRate, TIMELINE_SCHEMA_VERSION, TIMELINE_TICKS_PER_SECOND, TimelineAudioAction,
    TimelineBinding, TimelineClip, TimelineClipId, TimelineClipPayload, TimelineDocument,
    TimelineId, TimelineIdError, TimelineInterpolation, TimelineKey, TimelineMarker,
    TimelineMarkerId, TimelineProperty, TimelineTick, TimelineTrack, TimelineTrackId,
    TimelineTrackKind, TimelineVfxAction,
};
pub use transaction::{AuthoringSession, Transaction, TransactionError};
pub use typed_document_authoring::{
    TypedAuthoringDocument, TypedDocumentAuthoringError, TypedDocumentAuthoringMutation,
    TypedDocumentAuthoringService, TypedDocumentAuthoringSnapshot, TypedDocumentAuthoringState,
    TypedDocumentAuthoringValidation, TypedDocumentChange,
};
pub use ui::authoring_service::{
    UiAuthoringError, UiAuthoringMutation, UiAuthoringService, UiAuthoringSession,
    UiAuthoringSnapshot, UiAuthoringValidation,
};
pub use ui::{
    UI_SCHEMA_VERSION, UiAnchor, UiDocument, UiDocumentError, UiElementConstraints, UiLayout,
    UiNode, UiNodeKind, UiNumber, UiScaleMatch, UiScalePolicy, UiString,
};
pub use ui_contract::{
    UiAuthoringContract, UiBindingDeclaration, UiBindingKind, UiContractError, UiEventDeclaration,
    UiFocusDirection, UiFocusLink,
};
pub use ui_contract_runtime::{UI_CONTRACT_FILE_SUFFIX, UiContractDocumentError, UiFocusNavigator};
pub use ui_edit::{
    UiDocumentChange, UiDocumentCommand, UiDocumentCommandResult, UiDocumentCommitError,
    UiDocumentEditError, UiDocumentTransaction, find_ui_node,
};
pub use validation::{
    validate_asset_manifest, validate_scene, validate_scene_asset_refs, validate_start_scene,
};
pub use value::Value;
pub use vfx::{
    CompiledVfxEffect, CompiledVfxEmitter, CompiledVfxOperation, VFX_FILE_SUFFIX,
    VFX_SCHEMA_VERSION, VfxApply, VfxAttributeLayout, VfxAuthoringService,
    VfxCapabilityRequirements, VfxCommand, VfxCompilation, VfxCurve, VfxCurveInterpolation,
    VfxCurveKey, VfxCurveKeyId, VfxDiagnostic, VfxDiagnosticSeverity, VfxDocumentError, VfxEffect,
    VfxEmitter, VfxEmitterId, VfxGradient, VfxGradientKey, VfxGradientKeyId, VfxIdError, VfxModule,
    VfxModuleId, VfxModuleOperation, VfxModuleSchema, VfxPhase, VfxRandomChannel, VfxScalarValue,
    VfxSchemaCatalog, VfxShape, VfxTemplate, VfxTextureSheet, VfxValidation, VfxVectorValue,
};
