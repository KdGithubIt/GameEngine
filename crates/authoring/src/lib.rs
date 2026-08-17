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
#[allow(dead_code)]
#[path = "behavior_tree.rs"]
mod behavior_tree_legacy;
#[path = "behavior_tree_stateful.rs"]
pub mod behavior_tree;
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
pub mod timeline_authoring;
pub mod transaction;
pub mod typed_document_authoring;
pub mod ui;
pub mod ui_contract;
pub mod ui_contract_runtime;
pub mod ui_edit;
pub mod validation;
pub mod value;
pub mod vfx;

pub use access::{
    AuthoringPermission, AuthoringPermissionError, AuthoringPermissions,
};
pub use animation_graph::{
    animation_graph_motion_slots, compile_animation_graph, motion_slots_annotation_value,
    AnimState, AnimTransition, AnimationGraphDomain, AnimationStatePlaybackMode, CompiledAnimGraph,
    MotionSlot, ANIMATION_STATE_PLAYBACK_MODE_PROPERTY, MOTION_SLOTS_ANNOTATION,
};
pub use animation_set::{
    AnimationBinding, AnimationSet, AnimationSetError, AnimationSetEvent, MotionSourceRef,
    MotionSourceVariant,
    ANIMATION_SET_FILE_SUFFIX, ANIMATION_SET_SCHEMA_VERSION,
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
    AuthoringCapabilityRegistry, AuthoringDocumentKind, AuthoringDomain, AuthoringSchemaRef,
    AuthoringTransactionRequirement, RESERVED_CAPABILITY_NAMESPACE,
};
pub use command::{AuthoringCommand, Change, CommandResult, PropertyPath, PropertyPathSegment};
pub use component_metadata::{
    component_metadata_path, load_component_metadata, validate_project_component_id,
    write_component_metadata, ComponentMetadata, ComponentMetadataError,
    COMPONENT_METADATA_SCHEMA_VERSION,
};
pub use diagnostic::{Diagnostic, DiagnosticTarget, Severity};
pub use entity::AuthoringEntity;
pub use explicit_game_fields::{create_rust_script, create_rust_script_in};
pub use game_project::{
    create_rhai_script_in, duplicate_rust_component, initialize_game_project, move_rust_script,
    refresh_game_module_indexes, rust_declarations, GameProjectError, RustDeclaration,
    RustDeclarationKind, RustScriptKind, RustScriptSchedule, RECOMMENDED_RUST_SCRIPT_FOLDERS,
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
    apply_graph_commands_with_domain, validate_graph_with_domain, GraphCommandApplication,
    GraphDomain, TestGraphDomain,
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
pub use load::{load_scene_from_json, SceneLoadError};
pub use material_asset::{
    LinearRgba, MaterialAlphaMode, MaterialAsset, MaterialAssetError, MaterialCullMode,
    MaterialOutline, MaterialShadingModel, MaterialSphereBlendMode,
    MaterialSphereCoordinateSource, ToonLitProperties, MATERIAL_SCHEMA_VERSION,
};
pub use persist::{replace_file_contents, PersistError, PersistOperation};
pub use prefab::{PrefabAsset, PrefabError, PrefabInstantiation, PREFAB_SCHEMA_VERSION};
pub use prefab_authoring::{
    prefab_instance_marker, prefab_instance_source, PrefabAuthoringError, PrefabAuthoringService,
    PrefabInstanceSource, PrefabInstantiationMutation, PrefabInstantiationRequest,
    PrefabSourceError, PrefabSourcePath, PREFAB_INSTANCE_COMPONENT, PREFAB_INSTANCE_SOURCE_FIELD,
};
pub use project::{ProjectConfig, ProjectError, ProjectRoot, PROJECT_SCHEMA_VERSION};
pub use project_settings::{
    AxisBinding, InputAction, KeyAxisBinding, Layer, ProjectSettings, ProjectSettingsError,
    PROJECT_SETTINGS_SCHEMA_VERSION,
};
pub use scene::{AuthoringScene, SceneSaveError};
pub use scene_authoring::{
    SceneAuthoringError, SceneAuthoringMutation, SceneAuthoringService, SceneAuthoringSnapshot,
    SceneAuthoringValidation,
};
pub use schema::{ComponentSchema, ComponentSchemaRegistry, FieldSchema, FieldType};
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
    UiAnchor, UiDocument, UiDocumentError, UiElementConstraints, UiLayout, UiNode, UiNodeKind,
    UiNumber, UiScaleMatch, UiScalePolicy, UiString, UI_SCHEMA_VERSION,
};
pub use ui_contract::{
    UiAuthoringContract, UiBindingDeclaration, UiBindingKind, UiContractError, UiEventDeclaration,
    UiFocusDirection, UiFocusLink,
};
pub use ui_contract_runtime::{UiContractDocumentError, UiFocusNavigator, UI_CONTRACT_FILE_SUFFIX};
pub use ui_edit::{
    find_ui_node, UiDocumentChange, UiDocumentCommand, UiDocumentCommandResult,
    UiDocumentCommitError, UiDocumentEditError, UiDocumentTransaction,
};
pub use validation::{
    validate_asset_manifest, validate_scene, validate_scene_asset_refs, validate_start_scene,
};
pub use value::Value;
pub use vfx::{
    CompiledVfxEffect, CompiledVfxEmitter, CompiledVfxOperation, VfxApply, VfxAttributeLayout,
    VfxAuthoringService, VfxCapabilityRequirements, VfxCommand, VfxCompilation, VfxCurve,
    VfxCurveInterpolation, VfxCurveKey, VfxCurveKeyId, VfxDiagnostic, VfxDiagnosticSeverity,
    VfxDocumentError, VfxEffect, VfxEmitter, VfxEmitterId, VfxGradient, VfxGradientKey,
    VfxGradientKeyId, VfxIdError, VfxModule, VfxModuleId, VfxModuleOperation, VfxModuleSchema,
    VfxPhase, VfxRandomChannel, VfxScalarValue, VfxSchemaCatalog, VfxShape, VfxTemplate,
    VfxTextureSheet, VfxValidation, VfxVectorValue, VFX_FILE_SUFFIX, VFX_SCHEMA_VERSION,
};
