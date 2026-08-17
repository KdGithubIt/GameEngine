//! Human visual editor frontend shell.
//!
//! This crate owns GUI toolkit dependencies for Phase 8-A. Authoring data,
//! commands, validation, and graph view persistence remain in
//! `engine-authoring`.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

mod agent_host;
mod native_agent;
pub mod ai_studio;
pub mod anim_ux;
mod animation_set_editor;
pub mod asset_browser;
pub mod asset_import;
pub mod asset_management;
pub mod authoring_tools;
pub mod authoring_windows;
pub mod build;
pub mod canvas;
pub mod component_source_index;
pub mod component_source_viewer;
pub mod console;
pub mod document;
pub mod drag_drop;
mod editor_fonts;
pub mod environment;
pub mod filesystem_sync;
pub mod game_build;
pub mod geometry;
pub mod gizmo;
pub mod material_editor;
#[path = "navmesh_shared.rs"]
pub mod navmesh_bake;
pub mod prefab_workflow;
pub mod preferences;
mod preview_residency;
pub mod problems;
pub mod project_settings_panel;
pub mod runtime;
pub mod scene_view;
pub mod session;
pub mod skinned_model_bake;
pub mod systems_panel;
pub mod ui;
pub mod ui_builder;
mod vfx_builder;
pub mod view_aspect;
mod view_resolution;
mod workspace;

pub use ai_studio::{AiStudioConnection, AiStudioPanel};
pub use anim_ux::{
    build_retarget_map_inspector_model, build_skeleton_bind_report, find_skeleton_display_name,
    find_skeleton_record, merge_bone_pairs, rerun_name_matching, resolve_bone_name,
    retarget_map_file_name, show_contact_bones_editor, show_retarget_map_inspector,
    show_skeleton_bind_report, synthetic_skeleton_asset, ResolvedBonePair, ResolvedChainPair,
    RetargetMapInspectorAction, RetargetMapInspectorModel, SkeletonBindReport,
    SkeletonBindReportRow, SkeletonBoneStatus, UnresolvedTargetBone,
};
pub use asset_browser::{AssetBrowser, AssetEntry, AssetFolder, AssetKind};
pub use asset_import::{
    AssetImportManager, AssetImportProgress, AssetImportResult, AssetImportStartError,
};
pub use asset_management::{
    create_asset_folder, import_external_asset_files, is_registerable_asset_path, move_asset,
    move_asset_batch, move_asset_path, move_asset_paths_to_trash, move_asset_to_trash,
    AssetManagementError, AssetMoveReport, BatchAssetMoveReport, ExternalAssetImportFailure,
    ExternalAssetImportFailureKind, ExternalAssetImportReport, ImportedExternalAsset,
};
pub use authoring_tools::AuthoringTool;
pub use authoring_windows::AuthoringWindows;
pub use build::{
    analyze_build, find_player_binary, package_project, package_project_with_game_module,
    plan_package, BuildConfig, BuildDiagnostic, BuildDiagnosticKind, BuildReport, PackageCopy,
    PackageError, PackagePlan,
};
pub use component_source_index::{ComponentSourceIndex, ComponentSourceLocation};
pub use component_source_viewer::ReadOnlySourceDocument;
pub use document::{CurrentDocument, OpenDocumentError};
pub use drag_drop::{DragPayload, DropTarget};
pub use editor_fonts::install_editor_fonts;
pub use environment::EnvironmentSettings;
pub use filesystem_sync::{FileSyncArea, FileSyncEvent, FileSyncKind, ProjectFileWatcher};
pub use game_build::{
    latest_shadow_module, GameBuildDiagnostic, GameBuildKind, GameBuildManager, GameBuildResult,
    GameBuildStartError, GameBuildState,
};
pub use material_editor::MaterialEditorPanel;
pub use navmesh_bake::{
    bake_scene_navmesh, NavMeshBakeDocument, NavMeshBakeError, NavMeshBakeResult,
    NavMeshBakeSettings,
};
pub use prefab_workflow::{
    apply_prefab_overrides, create_prefab_from_selection, inspect_prefab_instance,
    instantiate_prefab, prefab_dependencies, revert_prefab_instance, unpack_prefab_instance,
    PrefabInstanceInfo, PrefabWorkflowError, EDITOR_PREFAB_INSTANCE_COMPONENT,
};
pub use project_settings_panel::ProjectSettingsPanel;
pub use runtime::{
    EditorMode, FrameCapture, GameViewError, PlayError, PlayTickError, RuntimePlayState,
};
pub use session::{
    BehaviorNodeInsertKind, EditorLoadError, EditorPersistError, EditorSession, SceneAlignment,
    SceneAxis,
};
pub use skinned_model_bake::{
    bake_skinned_model, SkinnedModelBakeError, SkinnedModelBakeResult, BONE_ATTACHMENT_DIAGNOSTIC,
    CONFIGURED_CONTROLLER_DIAGNOSTIC,
};
pub use systems_panel::{SystemsPanel, SystemsSaveState};
pub use ui::EditorApp;
pub use ui_builder::UiBuilderState;
pub use view_aspect::ViewAspect;
