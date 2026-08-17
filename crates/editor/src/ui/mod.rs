//! egui and eframe user interface shell.

use crate::animation_set_editor::AnimationSetEditorState;
use crate::asset_browser::{AssetBrowser, AssetKind};
use crate::asset_import::{AssetImportManager, AssetImportResult};
use crate::build::{
    find_player_binary, package_project, package_project_with_game_module, BuildConfig,
};
use crate::canvas::{show_graph_canvas, GraphCanvasAction, GraphCanvasState};
use crate::component_source_index::ComponentSourceIndex;
use crate::component_source_viewer::ReadOnlySourceDocument;
use crate::console::ConsolePanel;
use crate::drag_drop::DragPayload;
use crate::filesystem_sync::{FileSyncArea, FileSyncEvent, FileSyncKind, ProjectFileWatcher};
use crate::game_build::{
    engine_sdk_root, latest_shadow_module, prepare_cargo_sdk_config, GameBuildKind,
    GameBuildManager, GameBuildResult, GameBuildState,
};
use crate::material_editor::{show_material_editor_panel, MaterialEditorPanel};
use crate::preferences::{EditorPreferences, PlayModeView};
use crate::problems::ProblemsPanel;
use crate::project_settings_panel::{show_project_settings_panel, ProjectSettingsPanel};
use crate::runtime::{
    EditorMode, FrameCapture, PlayError, PlayTickError, RuntimeAnimationDebugSnapshot,
    RuntimeDiagnosticKind, RuntimeInputDebugSnapshot, RuntimePlayState,
};
use crate::scene_view::{
    AudioDistanceGizmoEdit, GizmoEdit, GizmoMode, GizmoSpace, SceneComponentPreview,
    SceneUiNodeSelection, SceneView,
};
use crate::session::{EditorPersistError, EditorSession, GraphNodeInsertKind};
use crate::session::{SceneAlignment, SceneAxis};
use crate::systems_panel::{show_systems_panel, SystemsPanel};
use crate::ui_builder::{show_ui_builder, show_ui_builder_inspector, UiBuilderState};
use crate::view_aspect::ViewAspect;
use crate::workspace::{DocumentWorkspace, WorkspaceDocumentKind, WorkspaceTabId};
use eframe::{egui, egui_wgpu};
use engine::{InputCommand, InputSource, KeyCode};
use engine_authoring::id::{AssetId, StableId};
use engine_authoring::{
    replace_file_contents, AuthoringCommand, AuthoringEntity, AuthoringScene, ComponentTypeId,
    EdgeId, EntityId, NodeId, ProjectRoot, ProjectSettings, PropertyPathSegment, RustScriptKind,
    RustScriptSchedule, Transaction, Value,
};
use std::sync::Arc;
use std::{
    fs,
    path::{Path, PathBuf},
};
mod animation_graph_parameters;
mod audio_authoring;
mod behavior_debug;
mod animation_preview;
mod asset_inspector;
mod assets;
mod chrome;
mod data_assets;
mod documents;
mod game_tools;
mod hierarchy;
mod inspector;
mod mcp;
mod navigation_workspace;
mod play;
mod presentation;
mod viewport;

pub use mcp::EditorMcpCallFailure;

const SCENE_FILESYSTEM_VALIDATION_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(200);

struct SceneFilesystemValidationJob {
    generation: u64,
    receiver: std::sync::mpsc::Receiver<Vec<engine_authoring::Diagnostic>>,
}

/// Latest unsaved value for one Material path during a continuous UI edit.
struct PendingMaterialSave {
    material: engine_authoring::MaterialAsset,
    deadline: std::time::Instant,
}

/// Revision-keyed choices used by the Material Editor's texture pickers.
struct MaterialTextureChoicesCache {
    manifest_revision: u64,
    assets_root: Option<PathBuf>,
    choices: Arc<Vec<(AssetId, String)>>,
}

use animation_preview::*;
use audio_authoring::*;
use behavior_debug::*;
use asset_inspector::*;
use assets::*;
use chrome::*;
use documents::*;
use game_tools::*;
use hierarchy::*;
use inspector::*;
use navigation_workspace::*;
use play::*;
use presentation::*;

/// Editor application shell (Phase 8-C / 9-B / 9-C).
pub struct EditorApp {
    session: DocumentWorkspace,
    canvas: GraphCanvasState,
    pending_connect_source: Option<NodeId>,
    property_node: Option<NodeId>,
    property_text: String,
    /// Editable display name for the selected Animation State.
    ///
    /// Buffered because text is edited one keystroke at a time; the Motion Slot
    /// beside it needs no buffer and is read straight from the graph.
    state_name_text: String,
    /// Transition whose typed Inspector values are currently buffered.
    transition_edge: Option<EdgeId>,
    /// Editable boolean parameter name for the selected Animation transition.
    transition_condition_text: String,
    /// Editable cross-fade duration in seconds for an explicit edge override.
    transition_fade_duration: f64,
    /// Whether the selected edge delegates fade duration to the Controller.
    transition_uses_default_fade: bool,
    new_node_behavior: String,
    /// Asset browser state.
    asset_browser: AssetBrowser,
    /// Asset and imported sub-asset selection/display-name state.
    asset_inspector: AssetInspectorState,
    /// Search text used by the Unity-style asset grid.
    asset_search: String,
    /// In-memory thumbnails keyed by asset-relative path.
    asset_thumbnails: std::collections::BTreeMap<PathBuf, TexturePreview>,
    /// Requests that only the right-hand asset grid return to its top edge.
    asset_content_scroll_reset: bool,
    /// Active full-height view inside the Assets utility dock.
    project_browser_tab: ProjectBrowserTab,
    /// Concrete root for a normal Editor workspace.
    /// `None` exists only for project-less test/support construction.
    project_root: Option<ProjectRoot>,
    /// Asset manifest loaded from the open project root.
    asset_manifest: engine::AssetManifest,
    /// Open material documents use their dedicated typed editor rather than
    /// exposing raw JSON or free-form asset identifiers.
    material_editor: MaterialEditorPanel,
    /// Whether the floating material editor is visible.
    show_material_editor: bool,
    /// Quiet-period deadline that coalesces continuous Material controls into
    /// one Scene View rebuild after the drag pauses.
    material_scene_preview_deadline: Option<std::time::Instant>,
    /// Coalesced Material file writes keyed by project-relative path.
    pending_material_saves: std::collections::BTreeMap<PathBuf, PendingMaterialSave>,
    material_texture_choices_cache: Option<MaterialTextureChoicesCache>,
    /// Open Animation Set document edited through the dedicated typed window.
    animation_set_editor: Option<AnimationSetEditorState>,
    /// Graph selection waiting for stale-binding removal confirmation.
    pending_animation_set_graph: Option<AssetId>,
    /// Whether the two-choice Animation Set Clear confirmation is visible.
    pending_animation_set_clear: bool,
    /// Uncommitted edit of the one Animation Set event row being changed.
    ///
    /// Only one row holds the keyboard focus or the pointer at a time, so a
    /// single draft keeps a drag or a partially typed name visible without
    /// writing an undo entry per frame (ADR 0116).
    animation_set_event_draft: Option<AnimationSetEventDraft>,
    texture_preview: Option<TexturePreview>,
    material_preview_asset: Option<AssetId>,
    material_texture_preview: Option<TexturePreview>,
    /// Cached project layer names used by schema-driven mask controls.
    project_layers: Vec<engine_authoring::project_settings::Layer>,
    /// Editable project settings loaded from `project_settings.json`.
    project_settings_panel: Option<ProjectSettingsPanel>,
    /// Whether the Project Settings window is visible.
    show_project_settings: bool,
    /// Selected scene entity for the hierarchy and inspector.
    selected_entity: Option<EntityId>,
    /// Complete scene selection; `selected_entity` is the primary item.
    selected_entities: std::collections::BTreeSet<EntityId>,
    /// Copied component values for Paste Component Values.
    component_clipboard: Option<(ComponentTypeId, Value)>,
    /// Last hierarchy row used as the Shift-range selection anchor.
    hierarchy_selection_anchor: Option<EntityId>,
    /// Configurable position offset applied by batch duplicate.
    duplicate_offset: [f64; 3],
    /// Selection returned by the previous duplicate for repeat placement.
    last_duplicate_selection: Vec<EntityId>,
    /// Editor-only hierarchy visibility flags.
    hidden_entities: std::collections::BTreeSet<EntityId>,
    /// Editor-only hierarchy edit locks.
    locked_entities: std::collections::BTreeSet<EntityId>,
    /// Entities whose children are folded away in the hierarchy.
    collapsed_entities: std::collections::BTreeSet<EntityId>,
    /// Text filter for the scene hierarchy.
    hierarchy_filter: String,
    /// Current editor play mode.
    editor_mode: EditorMode,
    /// Transient spatial-audio audition backend; never serialized into project data.
    audio_audition: AudioAuditionState,
    /// Runtime world owned while Play is active.
    runtime_state: Option<RuntimePlayState>,
    /// Transient read-only Behavior Tree live-debug presentation.
    behavior_debug: BehaviorTreeDebugState,
    /// Most recently recorded or loaded deterministic input artifact.
    last_replay: Option<engine::InputReplay>,
    /// Desktop controller adapter created lazily when Play first starts.
    #[cfg(not(target_arch = "wasm32"))]
    gamepad_context: Option<engine::gamepad::GilrsContext>,
    /// Prevents an unavailable controller backend from retrying every frame.
    #[cfg(not(target_arch = "wasm32"))]
    gamepad_initialization_attempted: bool,
    /// Action waiting on the unsaved-changes dialog.
    pending_unsaved_action: Option<PendingUnsavedAction>,
    /// Game View texture whose egui registration outlived its render state.
    ///
    /// One slot is enough: a registration can only exist when a wgpu render
    /// state existed, so at most one release can be deferred at a time.
    orphaned_game_view_textures: Vec<egui::TextureId>,
    /// Scene component numeric edit currently being dragged in the inspector.
    pending_component_drag: Option<PendingComponentDrag>,
    /// Whether the Game View panel currently receives keyboard input.
    ///
    /// Set to `true` when the user clicks inside the Game View image. Cleared
    /// when Escape is pressed or the OS window loses focus.
    game_view_focused: bool,
    /// Keys currently forwarded to the runtime so they can be released on
    /// focus loss.
    forwarded_keys: std::collections::HashSet<engine::KeyCode>,
    /// Mouse buttons forwarded while the Game View owns input focus.
    forwarded_mouse_buttons: std::collections::HashSet<engine::MouseButton>,
    /// Whether debug lines are drawn in the runtime world.
    show_debug_lines: bool,
    /// Project-scoped editor workspace preferences.
    preferences: EditorPreferences,
    /// Stable-ID project component lookup rebuilt after source changes.
    component_source_index: ComponentSourceIndex,
    /// Built-in source currently displayed in an internal read-only window.
    source_viewer: Option<ReadOnlySourceDocument>,
    /// Prefab source repeatedly instantiated by primary Scene View clicks.
    prefab_placement_source: Option<PathBuf>,
    /// Whether editor command preferences are visible.
    show_editor_preferences: bool,
    /// Scene view offscreen renderer and editor orbit camera.
    scene_view: SceneView,
    /// Current Scene View conversion or rendering problem.
    ///
    /// This is retained separately from Console history because Problems
    /// represents current state and removes the entry when the preview heals.
    scene_view_problem: Option<engine_authoring::Diagnostic>,
    /// Dedicated clip, transition, and graph inspection surface.
    animation_preview: AnimationPreviewWindow,
    /// Active transform gizmo mode (Translate / Rotate / Scale).
    gizmo_mode: GizmoMode,
    /// Coordinate space for translate/scale handles (Global / Local).
    gizmo_space: GizmoSpace,
    /// Entity templates stored by Ctrl+C for one transactional Ctrl+V paste.
    entity_clipboard: Option<Vec<AuthoringEntity>>,
    /// Console panel with per-severity filter.
    console_panel: ConsolePanel,
    /// Problems panel showing project-level validation diagnostics.
    problems_panel: ProblemsPanel,
    /// Pure in-memory diagnostics refreshed synchronously after an edit.
    inline_scene_problems: Vec<engine_authoring::Diagnostic>,
    /// Filesystem-backed diagnostics published by the debounced worker.
    filesystem_scene_problems: Vec<engine_authoring::Diagnostic>,
    /// Generation used to discard a worker result superseded by a newer edit.
    scene_validation_generation: u64,
    /// Quiet-period deadline before starting filesystem-backed validation.
    scene_validation_deadline: Option<std::time::Instant>,
    /// Current filesystem validation worker, if one is running.
    scene_validation_job: Option<SceneFilesystemValidationJob>,
    /// Problems already mirrored into the Console, keyed by code + message.
    ///
    /// Keeps each problem logged once per appearance instead of on every
    /// refresh; cleared when Play resets the Console so the problems that are
    /// still present re-log into the fresh Play log.
    mirrored_problem_keys: std::collections::BTreeSet<String>,
    /// Cancellable glTF/GLB parse and catalog job.
    asset_import: AssetImportManager,
    /// Import warnings/errors retained in Problems across scene revalidation.
    asset_import_problems: Vec<engine_authoring::Diagnostic>,
    /// Latest project Rust compiler diagnostics retained in Problems.
    game_build_problems: Vec<engine_authoring::Diagnostic>,
    /// Background Cargo process and completion channel for project Rust code.
    game_build: GameBuildManager,
    /// Background production navigation bake and cancellation channel.
    navigation_bake: NavigationBakeManager,
    /// Dedicated production navigation bake, profile, status, and path-test UI.
    navigation_workspace: NavigationWorkspaceUi,
    /// Latest successfully loaded project game-module generation.
    game_module: Option<Arc<engine::game_module::GameModule>>,
    /// Starts Play automatically after a requested prerequisite build succeeds.
    play_after_game_build: bool,
    /// Requests another development build after a script edit made while Cargo
    /// or Play prevented the build from starting immediately.
    game_build_requested_after_edit: bool,
    /// Monotonic source generation dirtied by internal or external edits.
    game_code_generation: u64,
    /// Generation attached to the currently running Cargo process.
    running_game_code_generation: Option<u64>,
    /// Newest generation successfully built and loaded.
    built_game_code_generation: u64,
    /// Quiet-period deadline for coalesced automatic builds.
    game_build_quiet_deadline: Option<std::time::Instant>,
    /// Whether the Rust script creation modal is open.
    show_new_rust_script: bool,
    /// Whether the Rhai script creation modal is open.
    show_new_rhai_script: bool,
    /// Rust type name entered in the creation modal.
    new_rust_script_name: String,
    /// Rhai file stem entered in the creation modal.
    new_rhai_script_name: String,
    /// Script kind selected in the creation modal.
    new_rust_script_kind: RustScriptKind,
    /// Runtime schedule selected for a new System.
    new_rust_script_schedule: RustScriptSchedule,
    /// Search text for Add Component.
    component_search: String,
    /// Whether the Add Component choice list is expanded in the Inspector.
    add_component_picker_open: bool,
    /// Revision-keyed Inspector catalogs and imported skeleton choices.
    inspector_cache: InspectorDerivedCache,
    pending_game_package: Option<PendingGamePackage>,
    /// Active tab in the main left-side navigation dock.
    left_panel_tab: LeftPanelTab,
    /// New Motion Slot display name entered in the Animation Graph left dock.
    new_motion_slot_name: String,
    /// Per-slot rename buffers keyed by stable MotionSlotId.
    motion_slot_name_buffers: std::collections::BTreeMap<engine_authoring::MotionSlotId, String>,
    /// Slot awaiting delete confirmation after its State usages were shown.
    pending_motion_slot_delete: Option<engine_authoring::MotionSlotId>,
    /// Active tab in the shared bottom dock.
    bottom_panel_tab: BottomPanelTab,
    /// Whether the bottom dock is expanded beyond its tab strip.
    bottom_panel_open: bool,
    /// Project-wide ECS schedule catalog and persisted settings editor.
    systems_panel: SystemsPanel,
    /// Selection and preview presentation state for UI documents.
    ui_builder: UiBuilderState,
    /// Runtime entity selected in the Play debugger as `(id, generation)`.
    selected_runtime_entity: Option<(u32, u32)>,
    /// Aspect constraint for the Game View image.
    game_view_aspect: ViewAspect,
    /// Applies Inspector property edits to every selected entity that has
    /// the edited component, not only the primary one.
    multi_edit_all: bool,
    /// Last time the editor attempted a crash-recovery snapshot.
    last_recovery_autosave: std::time::Instant,
    pending_asset_mutation: Option<PendingAssetMutation>,
    /// Debounced observer for external assets, documents, manifest, and Rust.
    file_watcher: Option<ProjectFileWatcher>,
    /// Dirty open document whose disk version changed externally.
    external_document_conflict: Option<ExternalDocumentConflict>,
    /// Short-lived, always-visible results for asset registration operations.
    notifications: Vec<EditorNotification>,
    /// Model sources waiting for the single import worker (ADR 0075).
    ///
    /// Import starts on its own whenever a glTF/GLB appears or changes under
    /// `assets/`, so a burst from a branch switch would otherwise be dropped
    /// by the one-job-at-a-time worker.
    pending_model_imports: std::collections::VecDeque<(AssetId, PathBuf)>,
    /// Per-source skeleton bind reports from the latest import (ADR 0077 §6,
    /// AP-5), keyed by source `AssetId` string. Rebuilt on every import
    /// result; never persisted.
    skeleton_rebind_reports:
        std::collections::BTreeMap<String, Vec<crate::anim_ux::SkeletonBindReport>>,
    /// Source `AssetId` string whose bind report detail view (opened from a
    /// Problems panel `anim.skeleton_rebind` entry) is currently visible.
    show_skeleton_bind_report: Option<String>,
    /// Per-source detected contact interval summaries from the latest import
    /// (ADR 0080 §1, AP-5), keyed by source `AssetId` string. Rebuilt on
    /// every import result; never persisted.
    clip_contact_summaries:
        std::collections::BTreeMap<String, Vec<crate::asset_import::ClipContactSummary>>,
    /// Open RetargetMap inspector window (AP-5), if any.
    retarget_map_editor: Option<RetargetMapEditorState>,
    /// Open multi-skin retarget-map creation picker (AP-6 scope (b)), if any.
    retarget_map_creation_picker: Option<RetargetMapCreationPickerState>,
    /// Open glTF/GLB source Import Settings window (AP-5), if any.
    import_settings_editor: Option<ImportSettingsEditorState>,
    /// Selection and canvas state of every open tab except the active one.
    ///
    /// The active tab's state is the live state on this shell; it is moved
    /// into this map only while another tab is being drawn.
    document_presentations: std::collections::BTreeMap<WorkspaceTabId, DocumentPresentation>,
}

impl EditorApp {
    /// Creates an editor app around an existing session.
    pub fn new(session: EditorSession) -> Self {
        Self {
            session: DocumentWorkspace::new(session),
            canvas: GraphCanvasState::default(),
            pending_connect_source: None,
            property_node: None,
            property_text: String::new(),
            state_name_text: String::new(),
            transition_edge: None,
            transition_condition_text: String::new(),
            transition_fade_duration: DEFAULT_TRANSITION_FADE_DURATION,
            transition_uses_default_fade: true,
            new_node_behavior: "new_behavior".into(),
            asset_browser: AssetBrowser::new(),
            asset_inspector: AssetInspectorState::new(),
            asset_search: String::new(),
            asset_thumbnails: std::collections::BTreeMap::new(),
            asset_content_scroll_reset: false,
            project_browser_tab: ProjectBrowserTab::Assets,
            project_root: None,
            asset_manifest: engine::AssetManifest::default(),
            material_editor: MaterialEditorPanel::new(),
            show_material_editor: false,
            material_scene_preview_deadline: None,
            pending_material_saves: std::collections::BTreeMap::new(),
            material_texture_choices_cache: None,
            animation_set_editor: None,
            pending_animation_set_graph: None,
            pending_animation_set_clear: false,
            animation_set_event_draft: None,
            texture_preview: None,
            material_preview_asset: None,
            material_texture_preview: None,
            project_layers: ProjectSettings::default().layers,
            project_settings_panel: None,
            show_project_settings: false,
            selected_entity: None,
            selected_entities: std::collections::BTreeSet::new(),
            component_clipboard: None,
            hierarchy_selection_anchor: None,
            duplicate_offset: [1.0, 0.0, 0.0],
            last_duplicate_selection: Vec::new(),
            hidden_entities: std::collections::BTreeSet::new(),
            collapsed_entities: std::collections::BTreeSet::new(),
            locked_entities: std::collections::BTreeSet::new(),
            hierarchy_filter: String::new(),
            editor_mode: EditorMode::Edit,
            audio_audition: AudioAuditionState::default(),
            runtime_state: None,
            behavior_debug: BehaviorTreeDebugState::default(),
            last_replay: None,
            #[cfg(not(target_arch = "wasm32"))]
            gamepad_context: None,
            #[cfg(not(target_arch = "wasm32"))]
            gamepad_initialization_attempted: false,
            pending_unsaved_action: None,
            orphaned_game_view_textures: Vec::new(),
            pending_component_drag: None,
            game_view_focused: false,
            forwarded_keys: std::collections::HashSet::new(),
            forwarded_mouse_buttons: std::collections::HashSet::new(),
            show_debug_lines: true,
            preferences: EditorPreferences::default(),
            component_source_index: ComponentSourceIndex::default(),
            source_viewer: None,
            prefab_placement_source: None,
            show_editor_preferences: false,
            scene_view: SceneView::new(),
            scene_view_problem: None,
            animation_preview: AnimationPreviewWindow::default(),
            gizmo_mode: GizmoMode::Translate,
            gizmo_space: GizmoSpace::Global,
            entity_clipboard: None,
            console_panel: ConsolePanel::default(),
            problems_panel: ProblemsPanel::default(),
            inline_scene_problems: Vec::new(),
            filesystem_scene_problems: Vec::new(),
            scene_validation_generation: 0,
            scene_validation_deadline: None,
            scene_validation_job: None,
            mirrored_problem_keys: std::collections::BTreeSet::new(),
            asset_import: AssetImportManager::default(),
            asset_import_problems: Vec::new(),
            game_build_problems: Vec::new(),
            game_build: GameBuildManager::default(),
            navigation_bake: NavigationBakeManager::default(),
            navigation_workspace: NavigationWorkspaceUi::default(),
            game_module: None,
            play_after_game_build: false,
            game_build_requested_after_edit: false,
            game_code_generation: 0,
            running_game_code_generation: None,
            built_game_code_generation: 0,
            game_build_quiet_deadline: None,
            show_new_rust_script: false,
            show_new_rhai_script: false,
            new_rust_script_name: "NewComponent".into(),
            new_rhai_script_name: "new_script".into(),
            new_rust_script_kind: RustScriptKind::Component,
            new_rust_script_schedule: RustScriptSchedule::Update,
            component_search: String::new(),
            add_component_picker_open: false,
            inspector_cache: InspectorDerivedCache::default(),
            pending_game_package: None,
            left_panel_tab: LeftPanelTab::Hierarchy,
            new_motion_slot_name: String::new(),
            motion_slot_name_buffers: std::collections::BTreeMap::new(),
            pending_motion_slot_delete: None,
            bottom_panel_tab: BottomPanelTab::Assets,
            bottom_panel_open: true,
            systems_panel: SystemsPanel::default(),
            ui_builder: UiBuilderState::new(),
            selected_runtime_entity: None,
            game_view_aspect: ViewAspect::default(),
            multi_edit_all: false,
            last_recovery_autosave: std::time::Instant::now(),
            pending_asset_mutation: None,
            file_watcher: None,
            external_document_conflict: None,
            notifications: Vec::new(),
            pending_model_imports: std::collections::VecDeque::new(),
            skeleton_rebind_reports: std::collections::BTreeMap::new(),
            show_skeleton_bind_report: None,
            clip_contact_summaries: std::collections::BTreeMap::new(),
            retarget_map_editor: None,
            retarget_map_creation_picker: None,
            import_settings_editor: None,
            document_presentations: std::collections::BTreeMap::new(),
        }
    }

    /// Initializes project-scoped services for the concrete workspace root.
    ///
    /// This stays private because ADR 0117 forbids rebinding one Editor
    /// process from one project to another after workspace construction.
    fn initialize_project_root(&mut self, root: ProjectRoot) {
        self.flush_all_pending_material_saves();
        self.audio_audition.reset_project();
        self.scene_view.clear_project_caches();
        self.material_scene_preview_deadline = None;
        self.pending_material_saves.clear();
        self.material_texture_choices_cache = None;
        let _ = self.asset_import.cancel();
        self.navigation_bake.clear();
        self.asset_import_problems.clear();
        self.game_build_problems.clear();
        self.scene_view_problem = None;
        if let Err(error) = self.asset_inspector.open_project(&root) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.sub_asset_names_load_failed",
                    error,
                ));
        }
        let (manifest, diagnostic) = load_asset_manifest(&root);
        let settings = ProjectSettings::load(root.path()).unwrap_or_default();
        self.project_layers = settings.layers.clone();
        self.project_settings_panel = Some(ProjectSettingsPanel::new(settings));
        // Project-local derived state starts empty for this concrete root.
        self.asset_thumbnails.clear();
        // Entity and node identifiers are meaningful only inside this project.
        self.document_presentations.clear();
        self.asset_content_scroll_reset = true;
        self.project_browser_tab = ProjectBrowserTab::Assets;
        self.asset_search.clear();
        let game_project_error = if root.game_dir().join("Cargo.toml").is_file() {
            engine_authoring::initialize_game_project(&root).err()
        } else {
            None
        };
        self.asset_browser.refresh(&root.assets_root());
        self.asset_browser
            .set_selected_folder(self.preferences.selected_asset_folder.clone());
        self.component_source_index = ComponentSourceIndex::build(&root.rust_scripts_dir());
        if let Some(error) = game_project_error {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.game_project_initialize_failed",
                    error.to_string(),
                ));
        }
        let mut game_module_issue = None;
        self.game_module = latest_shadow_module(&root).and_then(|path| {
            match engine::game_module::GameModule::load(&path) {
                Ok(module) => Some(Arc::new(module)),
                Err(error) => {
                    game_module_issue = Some(format!("could not load {}: {error}", path.display()));
                    // A project can still be edited when its last native module is stale or
                    // incompatible. Surface the reason instead of silently hiding game types.
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.game_module_load_failed",
                            format!("could not load {}: {error}", path.display()),
                        ));
                    None
                }
            }
        });
        if self.game_module.is_none()
            && root.game_dir().join("Cargo.toml").is_file()
            && game_module_issue.is_none()
        {
            game_module_issue =
                Some("build the project Game module to inspect Game systems".into());
        }
        self.systems_panel.open_project(
            &root,
            self.game_module.as_ref().map(Arc::clone),
            game_module_issue,
        );
        self.project_root = Some(root);
        self.file_watcher = self
            .project_root
            .as_ref()
            .map(|project| ProjectFileWatcher::new(project.path().to_path_buf()));
        self.asset_manifest = manifest;
        self.reconcile_sub_asset_display_names();
        // Project initialization should immediately expose its authoring
        // content instead of leaving a diagnostic utility tab in front.
        self.left_panel_tab = LeftPanelTab::Hierarchy;
        self.bottom_panel_tab = BottomPanelTab::Assets;
        self.bottom_panel_open = true;
        if let Some(diagnostic) = diagnostic {
            self.session.push_diagnostic(diagnostic);
        }
        self.pending_model_imports.clear();
        self.import_models_missing_catalogs();
    }

    /// Initializes project-scoped services in tests that exercise one
    /// subsystem without constructing a full project-first workspace.
    ///
    /// This shim is never compiled into the shipping Editor and therefore
    /// cannot reintroduce an in-process project switch path.
    #[cfg(test)]
    fn set_project_root(&mut self, root: ProjectRoot) {
        self.initialize_project_root(root);
    }
}

impl Default for EditorApp {
    fn default() -> Self {
        let preferences = EditorPreferences::default();
        let mut app = Self::new(EditorSession::empty_behavior_tree());
        app.ui_builder.preview_preset = preferences.ui_preview_preset;
        app.bottom_panel_open = preferences.bottom_panel_open;
        app.bottom_panel_tab = match preferences.bottom_panel_tab.as_str() {
            "console" => BottomPanelTab::Console,
            "problems" => BottomPanelTab::Problems,
            "input" => BottomPanelTab::Input,
            "runtime" => BottomPanelTab::Runtime,
            _ => BottomPanelTab::Assets,
        };
        // Problem suppressions are editor-local and apply before project-scoped
        // state is loaded, so a recurring import notice stays hidden on startup.
        app.problems_panel.set_suppressed_codes(
            preferences.suppressed_problem_codes.iter().cloned(),
        );
        app.preferences = preferences;
        app
    }
}

impl eframe::App for EditorApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.audio_audition.poll();
        self.update_audio_audition();
        // The UI Builder resolves image sources and offers texture choices
        // from transient state; refreshing here keeps it in sync with the
        // open project and manifest without threading them through every
        // builder function.
        self.ui_builder.preview_base_path = self
            .project_root
            .as_ref()
            .map(|project| project.path().to_path_buf());
        self.ui_builder.texture_source_choices = self
            .asset_manifest
            .iter()
            .filter(|(_, entry)| {
                engine::asset_path_matches_kind(engine::AssetKind::Texture, Path::new(&entry.path))
            })
            .map(|(_, entry)| entry.path.clone())
            .collect();
        if self.last_recovery_autosave.elapsed() >= std::time::Duration::from_secs(30) {
            self.last_recovery_autosave = std::time::Instant::now();
            self.persist_editor_local_state();
            for error in self.session.autosave_recovery_all() {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.recovery_autosave_failed",
                        format!("could not write crash-recovery snapshot: {error}"),
                    ));
            }
        }
        if self.asset_import.is_running() {
            ctx.request_repaint();
        }
        if let Some(result) = self.asset_import.poll() {
            self.handle_asset_import_result(result);
        }
        self.start_next_model_import();
        if self.game_build.state() != GameBuildState::Idle {
            ctx.request_repaint();
        }
        if let Some(result) = self.game_build.poll() {
            self.handle_game_build_result(result);
        }
        if self.navigation_bake.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(completion) = self.navigation_bake.poll() {
            self.handle_navigation_bake_completion(completion);
        }
        self.poll_coalesced_game_build(ctx);
        self.poll_project_filesystem(ctx);
        self.poll_scene_filesystem_validation(ctx);
        self.reconcile_sub_asset_display_names();
        #[cfg(not(target_arch = "wasm32"))]
        if self.runtime_state.is_some() {
            if !self.gamepad_initialization_attempted {
                self.gamepad_initialization_attempted = true;
                self.gamepad_context = engine::gamepad::GilrsContext::try_new();
                if self.gamepad_context.is_none() {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::warning(
                            "editor.runtime.gamepad_unavailable",
                            "the OS gamepad backend could not start; keyboard and virtual input remain available",
                        ));
                }
            }
            if let (Some(runtime), Some(gamepad)) =
                (&mut self.runtime_state, &mut self.gamepad_context)
            {
                runtime.poll_gamepads(gamepad);
            }
        }
        let tick_error = if let Some(runtime) = &mut self.runtime_state {
            match runtime.tick() {
                Ok(()) => {
                    ctx.request_repaint();
                    None
                }
                Err(error) => Some(error),
            }
        } else {
            None
        };

        if let Some(error) = tick_error {
            let kind = match error {
                PlayTickError::Panicked => RuntimeDiagnosticKind::Panicked,
                PlayTickError::Schedule { .. }
                | PlayTickError::GameModule(_)
                | PlayTickError::Replay(_) => RuntimeDiagnosticKind::TickFailed,
            };
            self.stop_play(frame.wgpu_render_state());
            self.session
                .push_diagnostic(kind.to_diagnostic(format!("Play stopped: {error}")));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        apply_editor_style(ui.ctx());
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
        if let Some(render_state) = frame.wgpu_render_state() {
            for texture_id in self.orphaned_game_view_textures.drain(..) {
                render_state.renderer.write().free_texture(&texture_id);
            }
        }
        self.handle_keyboard_shortcuts(ui, frame);

        egui::Panel::top("editor_menu_bar")
            .exact_size(28.0)
            .show_inside(ui, |ui| self.show_menu_bar(ui, frame));
        // Continuous commands and document tabs share one fixed-height row.
        // The shell draws Authoring Tools over the reserved right edge, so
        // removing the former full-width tab panel gives that height back to
        // Hierarchy, Scene View, and Inspector.
        egui::Panel::top("editor_main_toolbar")
            .exact_size(40.0)
            .show_inside(ui, |ui| self.show_main_toolbar(ui, frame));
        egui::Panel::bottom("editor_status_bar")
            .exact_size(24.0)
            .show_inside(ui, |ui| self.show_status_bar(ui));
        if self.bottom_panel_open {
            let maximum_height = bottom_dock_max_height(ui.available_height());
            egui::Panel::bottom("editor_bottom_dock_expanded")
                .resizable(true)
                .default_size(210.0)
                .min_size(BOTTOM_DOCK_MIN_HEIGHT)
                .max_size(maximum_height)
                .show_inside(ui, |ui| {
                    self.show_bottom_dock(ui);
                    if self.bottom_panel_tab == BottomPanelTab::Assets
                        && panel_received_primary_click(ui)
                    {
                        self.activate_asset_inspector();
                    }
                });
        } else {
            // A separate ID prevents the collapsed strip from overwriting the
            // height remembered by egui for the resizable expanded dock.
            egui::Panel::bottom("editor_bottom_dock_collapsed")
                .exact_size(32.0)
                .show_inside(ui, |ui| {
                    self.show_bottom_dock(ui);
                    if self.bottom_panel_tab == BottomPanelTab::Assets
                        && panel_received_primary_click(ui)
                    {
                        self.activate_asset_inspector();
                    }
                });
        }

        // UI Builder provides its own palette and hierarchy, so it receives
        // the width that would otherwise be reserved for project Systems.
        let show_left_dock = should_show_left_dock(
            self.project_root.is_some(),
            self.session.scene().is_some(),
            self.session.ui_document().is_some(),
        );
        // Every dock surface below is scoped by document tab so its scroll
        // offset belongs to the document that produced it.
        let active_tab = self.session.active_tab_id();
        if show_left_dock {
            let maximum_width = left_dock_max_width(ui.available_width());
            // The shared wrapper ensures long hierarchy and system text cannot
            // create an invisible reserved strip outside the dock clip rect.
            show_primary_left_dock_panel(ui, maximum_width, |ui| {
                ui.push_id(dock_surface_id("left_dock", active_tab), |ui| {
                    self.show_left_dock(ui)
                });
                if panel_received_primary_click(ui) {
                    self.deactivate_asset_inspector();
                }
            });
        }

        // Behavior Tree live debug already owns a runtime-specific details pane.
        // Hiding the generic Inspector here preserves enough horizontal graph
        // space for node labels and badges instead of showing two inspectors.
        if !self.behavior_debug.visible {
            let inspector_maximum_width = inspector_max_width(ui.available_width());
            show_inspector_panel(ui, inspector_maximum_width, |ui| {
                self.show_data_asset_tools(ui);
                ui.separator();
                // The three inspector surfaces replace one another inside this
                // panel, so each needs its own scope rather than the panel's.
                let showed_asset_inspector = ui
                    .push_id(dock_surface_id("asset_inspector", active_tab), |ui| {
                        self.show_asset_inspector(ui)
                    })
                    .inner;
                if !showed_asset_inspector {
                    if self.session.is_animation_graph() {
                        ui.push_id(
                            dock_surface_id("animation_graph_inspector", active_tab),
                            |ui| self.show_animation_graph_parameter_inspector(ui),
                        );
                    } else {
                        ui.push_id(dock_surface_id("entity_inspector", active_tab), |ui| {
                            self.show_inspector(ui)
                        });
                    }
                }
            });
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.show_workspace_header(ui);
            if self.is_playing() {
                self.show_runtime_workspace(ui, frame);
            } else if self.session.scene().is_some() {
                self.show_scene_workspace(ui, frame);
            } else if self.session.ui_document().is_some() {
                if let Err(error) = show_ui_builder(ui, &mut self.session, &mut self.ui_builder) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.ui_builder_command_failed",
                            error.to_string(),
                        ));
                }
                self.sync_scene_view_ui_selection();
            } else if matches!(
                self.session.current_document(),
                crate::document::CurrentDocument::None
            ) {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.heading("No document open");
                    ui.label("Open or create an authoring document inside this project.");
                });
            } else {
                let actions = show_graph_canvas(
                    ui,
                    &self.session,
                    &mut self.canvas,
                    self.pending_connect_source.as_ref(),
                );
                self.handle_canvas_actions(actions);
            }
            if panel_received_primary_click(ui) {
                self.deactivate_asset_inspector();
            }
        });

        // Draw floating editor surfaces after every dock and viewport. Scene View
        // HUD Areas share the egui context, so emitting editor windows last keeps
        // independent inspection surfaces above viewport-owned content.
        self.show_unsaved_changes_modal(ui.ctx());
        self.show_new_rhai_script_modal(ui.ctx());
        self.show_new_rust_script_modal(ui.ctx());
        self.show_material_editor_window(ui.ctx());
        self.show_animation_set_editor_window(ui.ctx());
        self.show_project_settings_window(ui.ctx());
        self.show_navigation_window(ui.ctx());
        self.show_texture_preview_window(ui.ctx());
        self.show_skeleton_bind_report_window(ui.ctx());
        self.show_retarget_map_editor_window(ui.ctx());
        self.show_retarget_map_creation_picker_window(ui.ctx());
        self.show_import_settings_editor_window(ui.ctx());
        self.show_asset_mutation_window(ui.ctx());
        self.show_editor_preferences_window(ui.ctx());
        self.show_source_viewer_window(ui.ctx());
        self.show_animation_preview_window(ui.ctx(), frame);
        self.show_external_document_conflict(ui.ctx());

        // 全パネルの描画後にTooltipレイヤーへ出すことで、
        // Asset Browserから別パネルへ移動してもプレビューを常に最前面へ表示する。
        show_asset_drag_preview(ui.ctx());
        self.show_notifications(ui.ctx());
        self.show_play_build_overlay(ui.ctx());
    }
}

impl EditorApp {
    fn refresh_scene_problems(&mut self) {
        let Some(scene) = self.session.scene() else {
            self.inline_scene_problems.clear();
            self.filesystem_scene_problems.clear();
            self.scene_validation_deadline = None;
            self.scene_validation_generation = self.scene_validation_generation.wrapping_add(1);
            self.publish_scene_problems();
            return;
        };
        let mut problems = engine_authoring::validate_scene(scene);
        problems.extend(engine::validate_builtin_component_values(scene));
        problems.extend(engine::validate_builtin_component_asset_references(
            scene,
            &self.asset_manifest,
        ));
        problems.extend(orphan_game_component_diagnostics(
            scene,
            &self.component_source_index,
            |component_type| {
                self.game_module
                    .as_ref()
                    .and_then(|module| module.component_schema(component_type))
                    .is_some()
            },
        ));
        self.inline_scene_problems = problems;
        self.scene_validation_generation = self.scene_validation_generation.wrapping_add(1);
        self.scene_validation_deadline = self
            .project_root
            .as_ref()
            .map(|_| std::time::Instant::now() + SCENE_FILESYSTEM_VALIDATION_DEBOUNCE);
        self.publish_scene_problems();
    }

    fn publish_scene_problems(&mut self) {
        let mut problems = self.inline_scene_problems.clone();
        problems.extend(self.filesystem_scene_problems.iter().cloned());
        problems.extend(self.asset_import_problems.iter().cloned());
        problems.extend(self.game_build_problems.iter().cloned());
        problems.extend(self.component_source_index.diagnostics().iter().cloned());
        problems.extend(self.scene_view_problem.iter().cloned());
        self.mirror_new_problems_to_console(&problems);
        self.problems_panel.set_problems(problems);
    }

    fn poll_scene_filesystem_validation(&mut self, context: &egui::Context) {
        if let Some(job) = &self.scene_validation_job {
            match job.receiver.try_recv() {
                Ok(diagnostics) => {
                    let generation = job.generation;
                    self.scene_validation_job = None;
                    if generation == self.scene_validation_generation {
                        self.filesystem_scene_problems = diagnostics;
                        self.publish_scene_problems();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.scene_validation_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    context.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
        }
        if self.scene_validation_job.is_some() {
            return;
        }
        let Some(deadline) = self.scene_validation_deadline else {
            return;
        };
        let now = std::time::Instant::now();
        if now < deadline {
            context.request_repaint_after(deadline.saturating_duration_since(now));
            return;
        }
        self.scene_validation_deadline = None;
        let (Some(scene), Some(project)) = (self.session.scene(), self.project_root.as_ref()) else {
            return;
        };
        let scene = scene.clone();
        let manifest = self.asset_manifest.clone();
        let project = project.clone();
        let scene_path = self.session.current_document_path().map(Path::to_path_buf);
        let generation = self.scene_validation_generation;
        let (sender, receiver) = std::sync::mpsc::channel();
        match std::thread::Builder::new()
            .name("scene-filesystem-validation".to_owned())
            .spawn(move || {
                let mut diagnostics = engine::validate_builtin_component_asset_files(
                    &scene,
                    &manifest,
                    &project.assets_root(),
                );
                diagnostics.extend(navigation_artifact_diagnostics(
                    &scene,
                    &project,
                    &manifest,
                    scene_path.as_deref(),
                ));
                let _ = sender.send(diagnostics);
            })
        {
            Ok(_) => {
                self.scene_validation_job = Some(SceneFilesystemValidationJob {
                    generation,
                    receiver,
                });
                context.request_repaint_after(std::time::Duration::from_millis(50));
            }
            Err(error) => {
                self.filesystem_scene_problems = vec![engine_authoring::Diagnostic::warning(
                    "editor.scene_validation_worker_failed",
                    format!("could not start filesystem validation: {error}"),
                )];
                self.publish_scene_problems();
            }
        }
    }

    /// Logs newly appeared warning/error problems into the Console.
    ///
    /// The Problems panel always shows the complete current set; the Console
    /// additionally records when each problem appeared, alongside runtime
    /// output. Info-severity problems stay panel-only to keep the log quiet.
    fn mirror_new_problems_to_console(&mut self, problems: &[engine_authoring::Diagnostic]) {
        let mut current_keys = std::collections::BTreeSet::new();
        for problem in problems {
            if matches!(problem.severity, engine_authoring::Severity::Info) {
                continue;
            }
            let key = format!("{}|{}", problem.code, problem.message);
            if !self.mirrored_problem_keys.contains(&key) {
                self.session.push_diagnostic(problem.clone());
            }
            current_keys.insert(key);
        }
        self.mirrored_problem_keys = current_keys;
    }

    /// Brings one asset-relative file into view in the Asset Browser.
    ///
    /// Every surface that navigates to an asset goes through this method:
    /// revealing a row is only useful when the dock that shows it is open, on
    /// the Assets tab, and scrolled to the row, and each caller that
    /// reimplemented part of that sequence used to leave a different step out.
    ///
    /// Returns `false` when no browser row exists for `relative_path`, which
    /// is the case for built-in assets with no project file.
    pub(super) fn reveal_asset_in_browser(&mut self, relative_path: &Path) -> bool {
        if !self.asset_browser.select_relative_path(relative_path) {
            return false;
        }
        self.bottom_panel_open = true;
        self.bottom_panel_tab = BottomPanelTab::Assets;
        self.project_browser_tab = ProjectBrowserTab::Assets;
        self.asset_content_scroll_reset = true;
        true
    }

    /// Opens one asset folder in the Asset Browser, revealing its tree row.
    ///
    /// Returns `false` when `folder` is not a discovered asset folder.
    pub(super) fn reveal_asset_folder_in_browser(&mut self, folder: &Path) -> bool {
        if !self.asset_browser.set_selected_folder(folder.to_path_buf()) {
            return false;
        }
        self.bottom_panel_open = true;
        self.bottom_panel_tab = BottomPanelTab::Assets;
        self.project_browser_tab = ProjectBrowserTab::Assets;
        self.asset_content_scroll_reset = true;
        true
    }

    fn apply_ui_result<T, E: std::fmt::Display>(&mut self, result: Result<T, E>) {
        if let Err(error) = result {
            self.report_error("editor.operation_failed", error.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
