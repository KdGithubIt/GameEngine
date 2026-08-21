//! Authoritative GUI-free headless authoring host (ADR 0151).

use engine_assets::asset::AssetManifest;
use engine_authoring::{
    AnimationSet, AuthoringGraphDomain, AuthoringPermission, AuthoringPermissions,
    AuthoringSession, ComponentSchemaRegistry, Graph, GraphDomain, GraphView, MaterialAsset,
    ProjectRoot, ProjectSettings, SpriteAnimationDocument, SpriteAtlasDocument, TileMapDocument,
    TileSetDocument, TypedAuthoringDocument, TypedDocumentAuthoringError,
    TypedDocumentAuthoringService, TypedDocumentAuthoringState, UiAuthoringSession, UiDocument,
    load_scene_from_json, replace_file_contents,
};
use engine_mcp::{
    AssetInspectInput, AssetMcpTools, AssetSearchInput, AuthoringCapabilityMcpTools, AuthoringVerb,
    BehaviorTreeApplyInput, BehaviorTreeGraphInput, BehaviorTreeMcpTools, CapabilityDescribeInput,
    CapabilityInvokeInput, EntityFindInput, EntityInspectInput, GenericAuthoringMcpError,
    GenericAuthoringMcpTools, GraphMutationInput, GraphViewMutationInput, McpToolError,
    PrefabCreateInput, PrefabInstantiateInput, PrefabMcpTools, SceneMcpTools, SceneMutationInput,
    TypedDocumentMutationInput, UiMutationInput, VfxEffectInput, VfxMcpTools, VfxMutationInput,
    VfxTemplateInput,
};
use engine_project_lifecycle::{
    HeadlessProjectLease, LifecycleError, acquire_headless_project, inspect_project,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Headless host authority mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessAccessMode {
    /// Saved-file snapshot access that never acquires project write authority.
    ReadOnlySavedFiles,
    /// Authoritative saved-project writer protected by the project OS lease.
    Writer,
}

/// Startup selection for document-scoped MCP tools.
///
/// Paths are relative to the project's `assets/` directory. Scene defaults to
/// `ProjectSettings::start_scene` when omitted; every other document class is
/// loaded only when explicitly selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadlessProjectSelection {
    /// Active Scene path.
    pub scene: Option<String>,
    /// Active semantic Graph path.
    pub graph: Option<String>,
    /// Active GraphView path associated with `graph`.
    pub graph_view: Option<String>,
    /// Active declarative UI document path.
    pub ui: Option<String>,
    /// Active Material asset path.
    pub material: Option<String>,
    /// Active Animation Set asset path.
    pub animation_set: Option<String>,
    /// Active Native 2D Sprite Atlas asset path.
    pub sprite_atlas: Option<String>,
    /// Active Native 2D Sprite Animation asset path.
    pub sprite_animation: Option<String>,
    /// Active Native 2D Tile Set asset path.
    pub tile_set: Option<String>,
    /// Active Native 2D Tile Map asset path.
    pub tile_map: Option<String>,
}

/// Explicit visibility/authority description returned by the headless host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeadlessViewDescriptor {
    /// Always `saved_file_snapshot` for ADR 0151 headless hosts.
    pub source: &'static str,
    /// Whether this process owns the authoritative project-writer lease.
    pub writable: bool,
    /// Headless state never contains a live Editor's dirty in-memory copy.
    pub live_editor_unsaved_state_visible: bool,
    /// Document classes loaded into this process at startup.
    pub loaded_documents: Vec<String>,
}

/// Failure to open or initialize a headless authoring host.
#[derive(Debug)]
pub enum HeadlessHostError {
    /// Project lifecycle inspection or writer acquisition failed.
    Lifecycle(LifecycleError),
    /// A selected saved document could not be loaded.
    Load {
        /// Document path involved in the failure.
        path: PathBuf,
        /// Underlying structured load message.
        message: String,
    },
}

impl fmt::Display for HeadlessHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lifecycle(error) => error.fmt(formatter),
            Self::Load { path, message } => {
                write!(formatter, "could not load `{}`: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for HeadlessHostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lifecycle(error) => Some(error),
            Self::Load { .. } => None,
        }
    }
}

impl From<LifecycleError> for HeadlessHostError {
    fn from(error: LifecycleError) -> Self {
        Self::Lifecycle(error)
    }
}

/// Structured failure returned from one headless MCP tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessMcpCallFailure {
    code: String,
    message: String,
}

impl HeadlessMcpCallFailure {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic-style code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable failure message.
    pub fn message(&self) -> &str {
        &self.message
    }

    fn no_document(kind: &str) -> Self {
        Self::new(
            "mcp.headless_document_unavailable",
            format!("no {kind} document is loaded in this headless host; select one at startup"),
        )
    }

    fn persist(path: &Path, message: impl fmt::Display) -> Self {
        Self::new(
            "mcp.headless_persist_failed",
            format!(
                "could not persist canonical saved copy `{}`: {message}",
                path.display()
            ),
        )
    }
}

impl fmt::Display for HeadlessMcpCallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HeadlessMcpCallFailure {}

struct LoadedScene {
    path: PathBuf,
    relative: String,
    session: AuthoringSession,
}
struct LoadedGraph {
    path: PathBuf,
    relative: String,
    document: Graph,
}
struct LoadedGraphView {
    path: PathBuf,
    relative: String,
    document: GraphView,
}
struct LoadedUi {
    path: PathBuf,
    relative: String,
    session: UiAuthoringSession,
}
struct LoadedTyped<T> {
    path: Option<PathBuf>,
    relative: String,
    document: T,
    state: TypedDocumentAuthoringState,
}

/// GUI-free headless authoring host backed by the same `engine-mcp` services as Editor MCP.
pub struct HeadlessAuthoringHost {
    project: ProjectRoot,
    mode: HeadlessAccessMode,
    _writer_lease: Option<HeadlessProjectLease>,
    permissions: AuthoringPermissions,
    manifest: AssetManifest,
    scene: Option<LoadedScene>,
    graph: Option<LoadedGraph>,
    graph_view: Option<LoadedGraphView>,
    ui: Option<LoadedUi>,
    material: Option<LoadedTyped<MaterialAsset>>,
    project_settings: LoadedTyped<ProjectSettings>,
    animation_set: Option<LoadedTyped<AnimationSet>>,
    sprite_atlas: Option<LoadedTyped<SpriteAtlasDocument>>,
    sprite_animation: Option<LoadedTyped<SpriteAnimationDocument>>,
    tile_set: Option<LoadedTyped<TileSetDocument>>,
    tile_map: Option<LoadedTyped<TileMapDocument>>,
    game_module: Option<engine::game_module::GameModule>,
}

impl HeadlessAuthoringHost {
    /// Opens a saved-file snapshot host that may coexist with an Editor writer.
    ///
    /// # Errors
    ///
    /// Returns [`HeadlessHostError`] when project or selected documents cannot be loaded.
    pub fn open_read_only(
        project_path: impl AsRef<Path>,
        selection: HeadlessProjectSelection,
    ) -> Result<Self, HeadlessHostError> {
        let project = inspect_project(project_path.as_ref())?;
        Self::open(
            project,
            HeadlessAccessMode::ReadOnlySavedFiles,
            None,
            selection,
        )
    }

    /// Opens an authoritative saved-project writer after acquiring ADR 0151 ownership.
    ///
    /// # Errors
    ///
    /// Returns [`HeadlessHostError`] when another writer owns the canonical project
    /// or when project/selected documents cannot be loaded.
    pub fn open_writer(
        project_path: impl AsRef<Path>,
        selection: HeadlessProjectSelection,
    ) -> Result<Self, HeadlessHostError> {
        let lease = acquire_headless_project(project_path.as_ref())?;
        let project = lease.project_root().clone();
        Self::open(project, HeadlessAccessMode::Writer, Some(lease), selection)
    }

    fn open(
        project: ProjectRoot,
        mode: HeadlessAccessMode,
        writer_lease: Option<HeadlessProjectLease>,
        selection: HeadlessProjectSelection,
    ) -> Result<Self, HeadlessHostError> {
        let settings =
            ProjectSettings::load(project.path()).map_err(|error| HeadlessHostError::Load {
                path: project.path().join("project_settings.json"),
                message: error.to_string(),
            })?;
        let scene_relative = selection
            .scene
            .clone()
            .or_else(|| settings.start_scene.clone());
        let manifest = load_manifest(&project)?;
        let scene = scene_relative
            .map(|relative| load_scene(&project, relative))
            .transpose()?;
        let graph = selection
            .graph
            .map(|relative| load_graph(&project, relative))
            .transpose()?;
        let graph_view = selection
            .graph_view
            .map(|relative| load_graph_view(&project, relative))
            .transpose()?;
        if graph_view.is_some() && graph.is_none() {
            return Err(HeadlessHostError::Load {
                path: project.path().to_path_buf(),
                message: "a GraphView selection requires an active semantic Graph".into(),
            });
        }
        let ui = selection
            .ui
            .map(|relative| load_ui(&project, relative))
            .transpose()?;
        let material = selection
            .material
            .map(|relative| load_material(&project, relative))
            .transpose()?;
        let animation_set = selection
            .animation_set
            .map(|relative| load_animation_set(&project, relative))
            .transpose()?;
        let sprite_atlas = selection
            .sprite_atlas
            .map(|relative| load_native_2d::<SpriteAtlasDocument>(&project, relative))
            .transpose()?;
        let sprite_animation = selection
            .sprite_animation
            .map(|relative| load_native_2d::<SpriteAnimationDocument>(&project, relative))
            .transpose()?;
        let tile_set = selection
            .tile_set
            .map(|relative| load_native_2d::<TileSetDocument>(&project, relative))
            .transpose()?;
        let tile_map = selection
            .tile_map
            .map(|relative| load_native_2d::<TileMapDocument>(&project, relative))
            .transpose()?;
        let game_module = latest_shadow_module(&project)
            .and_then(|path| engine::game_module::GameModule::load(&path).ok());
        let permissions = match mode {
            HeadlessAccessMode::ReadOnlySavedFiles => {
                AuthoringPermissions::read_only().with(AuthoringPermission::Preview)
            }
            HeadlessAccessMode::Writer => AuthoringPermissions::read_only()
                .with(AuthoringPermission::Preview)
                .with(AuthoringPermission::ProjectDataWrite)
                .with(AuthoringPermission::AssetWrite),
        };
        Ok(Self {
            project,
            mode,
            _writer_lease: writer_lease,
            permissions,
            manifest,
            scene,
            graph,
            graph_view,
            ui,
            material,
            project_settings: LoadedTyped {
                path: None,
                relative: "project_settings.json".into(),
                document: settings,
                state: TypedDocumentAuthoringState::new(),
            },
            animation_set,
            sprite_atlas,
            sprite_animation,
            tile_set,
            tile_map,
            game_module,
        })
    }

    /// Returns the host authority mode.
    pub fn access_mode(&self) -> HeadlessAccessMode {
        self.mode
    }

    /// Returns an explicit description of saved-state visibility and writer authority.
    pub fn view_descriptor(&self) -> HeadlessViewDescriptor {
        let mut loaded = vec![self.project_settings.relative.clone()];
        if let Some(value) = &self.scene {
            loaded.push(format!("scene:{}", value.relative));
        }
        if let Some(value) = &self.graph {
            loaded.push(format!("graph:{}", value.relative));
        }
        if let Some(value) = &self.graph_view {
            loaded.push(format!("graph_view:{}", value.relative));
        }
        if let Some(value) = &self.ui {
            loaded.push(format!("ui:{}", value.relative));
        }
        if let Some(value) = &self.material {
            loaded.push(format!("material:{}", value.relative));
        }
        if let Some(value) = &self.animation_set {
            loaded.push(format!("animation_set:{}", value.relative));
        }
        if let Some(value) = &self.sprite_atlas {
            loaded.push(format!("sprite_atlas:{}", value.relative));
        }
        if let Some(value) = &self.sprite_animation {
            loaded.push(format!("sprite_animation:{}", value.relative));
        }
        if let Some(value) = &self.tile_set {
            loaded.push(format!("tile_set:{}", value.relative));
        }
        if let Some(value) = &self.tile_map {
            loaded.push(format!("tile_map:{}", value.relative));
        }
        HeadlessViewDescriptor {
            source: "saved_file_snapshot",
            writable: self.mode == HeadlessAccessMode::Writer,
            live_editor_unsaved_state_visible: false,
            loaded_documents: loaded,
        }
    }

    /// Executes one advertised `engine-mcp` tool against this process-owned state.
    ///
    /// Successful writer mutations are persisted through the same canonical
    /// serializers used by CLI/Editor persistence. Persistence failure restores
    /// the prior in-memory document before returning an error.
    pub fn handle_tool_call(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, HeadlessMcpCallFailure> {
        let permissions = self.permissions.clone();
        let capability_tools = AuthoringCapabilityMcpTools::new();
        let scene_tools = SceneMcpTools::new();
        let generic_tools = GenericAuthoringMcpTools::new();
        let asset_tools = AssetMcpTools::new();
        let prefab_tools = PrefabMcpTools::new();
        let vfx_tools = VfxMcpTools::new();
        let behavior_tools = BehaviorTreeMcpTools::new();

        match name {
            "authoring.list" => {
                require_empty(arguments)?;
                to_value(
                    capability_tools
                        .list(&permissions)
                        .map_err(capability_failure)?,
                )
            }
            "authoring.capabilities" => {
                require_empty(arguments)?;
                to_value(
                    capability_tools
                        .capabilities(&permissions)
                        .map_err(capability_failure)?,
                )
            }
            "authoring.describe" => {
                let input = decode::<CapabilityDescribeInput>(arguments)?;
                to_value(
                    capability_tools
                        .describe(&permissions, input)
                        .map_err(capability_failure)?,
                )
            }
            "authoring.inspect" => {
                self.invoke_generic(&capability_tools, AuthoringVerb::Inspect, arguments)
            }
            "authoring.validate" => {
                self.invoke_generic(&capability_tools, AuthoringVerb::Validate, arguments)
            }
            "authoring.preview" => {
                self.invoke_generic(&capability_tools, AuthoringVerb::Preview, arguments)
            }
            "authoring.apply" => {
                self.invoke_generic(&capability_tools, AuthoringVerb::Apply, arguments)
            }
            "project.describe" => {
                require_empty(arguments)?;
                let mut value =
                    to_value(scene_tools.project_describe(&self.project, &permissions)?)?;
                if let Value::Object(object) = &mut value {
                    object.insert("headless_view".into(), to_value(self.view_descriptor())?);
                }
                Ok(value)
            }
            "scene.inspect" => {
                require_empty(arguments)?;
                let session = self.scene_ref()?;
                to_value(scene_tools.scene_inspect(session, &permissions)?)
            }
            "scene.validate" => {
                require_empty(arguments)?;
                let session = self.scene_ref()?;
                to_value(scene_tools.scene_validate(session, &permissions)?)
            }
            "scene.preview" => {
                let input = decode::<SceneMutationInput>(arguments)?;
                let session = self.scene_ref()?;
                to_value(scene_tools.scene_preview(session, &permissions, input)?)
            }
            "scene.apply" => {
                let input = decode::<SceneMutationInput>(arguments)?;
                let loaded = self
                    .scene
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Scene"))?;
                let saved = fs::read_to_string(&loaded.path)
                    .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
                let mutation = scene_tools.scene_apply(&mut loaded.session, &permissions, input)?;
                if mutation.success
                    && !mutation.diff.is_empty()
                    && let Err(error) = persist_scene(loaded)
                {
                    loaded.session =
                        AuthoringSession::new(load_scene_from_json(&saved).map_err(|reload| {
                            HeadlessMcpCallFailure::new(
                                "mcp.headless_reload_failed",
                                reload.to_string(),
                            )
                        })?);
                    return Err(error);
                }
                to_value(mutation)
            }
            "entity.find" => {
                let input = decode::<EntityFindInput>(arguments)?;
                let session = self.scene_ref()?;
                to_value(scene_tools.entity_find(session, &permissions, input)?)
            }
            "entity.inspect" => {
                let input = decode::<EntityInspectInput>(arguments)?;
                let session = self.scene_ref()?;
                to_value(scene_tools.entity_inspect(session, &permissions, input)?)
            }
            "component.schemas" => {
                require_empty(arguments)?;
                let mut registry = ComponentSchemaRegistry::builtin();
                let builtins = engine::builtin_registry();
                for definition in builtins.definitions() {
                    registry.register(definition.schema.clone());
                }
                if let Some(module) = &self.game_module {
                    for schema in module.component_schemas() {
                        registry.register(schema.clone());
                    }
                }
                to_value(scene_tools.component_schemas(&registry, &permissions)?)
            }
            "asset.search" => {
                let input = decode::<AssetSearchInput>(arguments)?;
                to_value(asset_tools.asset_search(
                    &self.project,
                    &self.manifest,
                    &permissions,
                    input,
                )?)
            }
            "asset.inspect" => {
                let input = decode::<AssetInspectInput>(arguments)?;
                to_value(asset_tools.asset_inspect(
                    &self.project,
                    &self.manifest,
                    &permissions,
                    input,
                )?)
            }
            "prefab.create" => {
                let input = decode::<PrefabCreateInput>(arguments)?;
                let scene = self.scene_ref()?.scene().clone();
                to_value(prefab_tools.prefab_create(
                    &self.project,
                    &mut self.manifest,
                    &permissions,
                    &scene,
                    input,
                )?)
            }
            "prefab.preview" => {
                let input = decode::<PrefabInstantiateInput>(arguments)?;
                let session = self.scene_ref()?;
                to_value(prefab_tools.prefab_preview(
                    &self.project,
                    session,
                    &permissions,
                    input,
                )?)
            }
            "prefab.instantiate" => {
                let input = decode::<PrefabInstantiateInput>(arguments)?;
                let loaded = self
                    .scene
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Scene"))?;
                let saved = fs::read_to_string(&loaded.path)
                    .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
                let mutation = prefab_tools.prefab_instantiate(
                    &self.project,
                    &mut loaded.session,
                    &permissions,
                    input,
                )?;
                if !mutation.mutation.diff.is_empty()
                    && let Err(error) = persist_scene(loaded)
                {
                    loaded.session =
                        AuthoringSession::new(load_scene_from_json(&saved).map_err(|reload| {
                            HeadlessMcpCallFailure::new(
                                "mcp.headless_reload_failed",
                                reload.to_string(),
                            )
                        })?);
                    return Err(error);
                }
                to_value(mutation)
            }
            "graph.inspect" => {
                require_empty(arguments)?;
                let graph = self.graph_ref()?;
                to_value(
                    generic_tools
                        .graph_inspect(graph, &permissions)
                        .map_err(generic_failure)?,
                )
            }
            "graph.validate" => {
                require_empty(arguments)?;
                let graph = self.graph_ref()?;
                to_value(
                    generic_tools
                        .graph_validate(graph, &permissions)
                        .map_err(generic_failure)?,
                )
            }
            "graph.preview" => {
                let input = decode::<GraphMutationInput>(arguments)?;
                let graph = self.graph_ref()?;
                to_value(
                    generic_tools
                        .graph_preview(graph, &permissions, input)
                        .map_err(generic_failure)?,
                )
            }
            "graph.apply" => {
                let input = decode::<GraphMutationInput>(arguments)?;
                let loaded = self
                    .graph
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Graph"))?;
                let saved = fs::read_to_string(&loaded.path)
                    .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
                let mutation = generic_tools
                    .graph_apply(&mut loaded.document, &permissions, input)
                    .map_err(generic_failure)?;
                if mutation.success
                    && !mutation.diff.is_empty()
                    && let Err(error) = persist_graph(loaded)
                {
                    loaded.document = serde_json::from_str(&saved).map_err(|reload| {
                        HeadlessMcpCallFailure::new(
                            "mcp.headless_reload_failed",
                            reload.to_string(),
                        )
                    })?;
                    return Err(error);
                }
                to_value(mutation)
            }
            "graph.layout.inspect" => {
                require_empty(arguments)?;
                let view = self.graph_view_ref()?;
                to_value(
                    generic_tools
                        .graph_view_inspect(view, &permissions)
                        .map_err(generic_failure)?,
                )
            }
            "graph.layout.validate" => {
                require_empty(arguments)?;
                to_value(
                    generic_tools
                        .graph_view_validate(
                            self.graph_ref()?,
                            self.graph_view_ref()?,
                            &permissions,
                        )
                        .map_err(generic_failure)?,
                )
            }
            "graph.layout.preview" => {
                let input = decode::<GraphViewMutationInput>(arguments)?;
                to_value(
                    generic_tools
                        .graph_view_preview(
                            self.graph_ref()?,
                            self.graph_view_ref()?,
                            &permissions,
                            input,
                        )
                        .map_err(generic_failure)?,
                )
            }
            "graph.layout.apply" => {
                let input = decode::<GraphViewMutationInput>(arguments)?;
                let graph = self
                    .graph
                    .as_ref()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Graph"))?;
                let loaded = self
                    .graph_view
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("GraphView"))?;
                let saved = fs::read_to_string(&loaded.path)
                    .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
                let mutation = generic_tools
                    .graph_view_apply(&graph.document, &mut loaded.document, &permissions, input)
                    .map_err(generic_failure)?;
                if mutation.success
                    && !mutation.diff.is_empty()
                    && let Err(error) = persist_graph_view(&graph.document, loaded)
                {
                    loaded.document = serde_json::from_str(&saved).map_err(|reload| {
                        HeadlessMcpCallFailure::new(
                            "mcp.headless_reload_failed",
                            reload.to_string(),
                        )
                    })?;
                    return Err(error);
                }
                to_value(mutation)
            }
            "ui.inspect" => {
                require_empty(arguments)?;
                let ui = self.ui_ref()?;
                to_value(
                    generic_tools
                        .ui_inspect(ui, &permissions)
                        .map_err(generic_failure)?,
                )
            }
            "ui.validate" => {
                require_empty(arguments)?;
                let ui = self.ui_ref()?;
                to_value(
                    generic_tools
                        .ui_validate(ui, &permissions)
                        .map_err(generic_failure)?,
                )
            }
            "ui.preview" => {
                let input = decode::<UiMutationInput>(arguments)?;
                let ui = self.ui_ref()?;
                to_value(
                    generic_tools
                        .ui_preview(ui, &permissions, input)
                        .map_err(generic_failure)?,
                )
            }
            "ui.apply" => {
                let input = decode::<UiMutationInput>(arguments)?;
                let loaded = self
                    .ui
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("UI"))?;
                let before = loaded
                    .session
                    .document()
                    .to_json_string()
                    .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
                let mutation = generic_tools
                    .ui_apply(&mut loaded.session, &permissions, input)
                    .map_err(generic_failure)?;
                if mutation.success
                    && !mutation.diff.is_empty()
                    && let Err(error) = persist_ui(loaded)
                {
                    let document = UiDocument::from_json_str(&before).map_err(|reload| {
                        HeadlessMcpCallFailure::new(
                            "mcp.headless_reload_failed",
                            reload.to_string(),
                        )
                    })?;
                    loaded.session = UiAuthoringSession::new(document);
                    return Err(error);
                }
                to_value(mutation)
            }
            "material.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.material
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Material"))?,
                    &permissions,
                )
            }
            "material.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.material
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Material"))?,
                    &permissions,
                )
            }
            "material.preview" => {
                let input = decode::<TypedDocumentMutationInput<MaterialAsset>>(arguments)?;
                typed_preview(
                    self.material
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Material"))?,
                    &permissions,
                    input,
                )
            }
            "material.apply" => {
                let input = decode::<TypedDocumentMutationInput<MaterialAsset>>(arguments)?;
                let loaded = self
                    .material
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Material"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    let json = document.to_json().map_err(|error| error.to_string())?;
                    replace_file_contents(path.expect("material path"), &json)
                        .map_err(|error| error.to_string())
                })
            }
            "project_settings.inspect" => {
                require_empty(arguments)?;
                typed_inspect(&self.project_settings, &permissions)
            }
            "project_settings.validate" => {
                require_empty(arguments)?;
                typed_validate(&self.project_settings, &permissions)
            }
            "project_settings.preview" => {
                let input = decode::<TypedDocumentMutationInput<ProjectSettings>>(arguments)?;
                typed_preview(&self.project_settings, &permissions, input)
            }
            "project_settings.apply" => {
                let input = decode::<TypedDocumentMutationInput<ProjectSettings>>(arguments)?;
                let project_path = self.project.path().to_path_buf();
                typed_apply(
                    &mut self.project_settings,
                    &permissions,
                    input,
                    move |document, _| {
                        document
                            .save(&project_path)
                            .map_err(|error| error.to_string())
                    },
                )
            }
            "animation_set.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.animation_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Animation Set"))?,
                    &permissions,
                )
            }
            "animation_set.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.animation_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Animation Set"))?,
                    &permissions,
                )
            }
            "animation_set.preview" => {
                let input = decode::<TypedDocumentMutationInput<AnimationSet>>(arguments)?;
                typed_preview(
                    self.animation_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Animation Set"))?,
                    &permissions,
                    input,
                )
            }
            "animation_set.apply" => {
                let input = decode::<TypedDocumentMutationInput<AnimationSet>>(arguments)?;
                let loaded = self
                    .animation_set
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Animation Set"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    let json = document
                        .to_canonical_json()
                        .map_err(|error| error.to_string())?;
                    replace_file_contents(path.expect("animation set path"), &json)
                        .map_err(|error| error.to_string())
                })
            }
            "sprite_atlas.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.sprite_atlas
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Atlas"))?,
                    &permissions,
                )
            }
            "sprite_atlas.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.sprite_atlas
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Atlas"))?,
                    &permissions,
                )
            }
            "sprite_atlas.preview" => {
                let input = decode::<TypedDocumentMutationInput<SpriteAtlasDocument>>(arguments)?;
                typed_preview(
                    self.sprite_atlas
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Atlas"))?,
                    &permissions,
                    input,
                )
            }
            "sprite_atlas.apply" => {
                let input = decode::<TypedDocumentMutationInput<SpriteAtlasDocument>>(arguments)?;
                let loaded = self
                    .sprite_atlas
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Atlas"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    persist_native_2d(document.to_canonical_json(), path, "Sprite Atlas")
                })
            }
            "sprite_animation.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.sprite_animation
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Animation"))?,
                    &permissions,
                )
            }
            "sprite_animation.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.sprite_animation
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Animation"))?,
                    &permissions,
                )
            }
            "sprite_animation.preview" => {
                let input =
                    decode::<TypedDocumentMutationInput<SpriteAnimationDocument>>(arguments)?;
                typed_preview(
                    self.sprite_animation
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Animation"))?,
                    &permissions,
                    input,
                )
            }
            "sprite_animation.apply" => {
                let input =
                    decode::<TypedDocumentMutationInput<SpriteAnimationDocument>>(arguments)?;
                let loaded = self
                    .sprite_animation
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Sprite Animation"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    persist_native_2d(document.to_canonical_json(), path, "Sprite Animation")
                })
            }
            "tile_set.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.tile_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Set"))?,
                    &permissions,
                )
            }
            "tile_set.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.tile_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Set"))?,
                    &permissions,
                )
            }
            "tile_set.preview" => {
                let input = decode::<TypedDocumentMutationInput<TileSetDocument>>(arguments)?;
                typed_preview(
                    self.tile_set
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Set"))?,
                    &permissions,
                    input,
                )
            }
            "tile_set.apply" => {
                let input = decode::<TypedDocumentMutationInput<TileSetDocument>>(arguments)?;
                let loaded = self
                    .tile_set
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Set"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    persist_native_2d(document.to_canonical_json(), path, "Tile Set")
                })
            }
            "tile_map.inspect" => {
                require_empty(arguments)?;
                typed_inspect(
                    self.tile_map
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Map"))?,
                    &permissions,
                )
            }
            "tile_map.validate" => {
                require_empty(arguments)?;
                typed_validate(
                    self.tile_map
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Map"))?,
                    &permissions,
                )
            }
            "tile_map.preview" => {
                let input = decode::<TypedDocumentMutationInput<TileMapDocument>>(arguments)?;
                typed_preview(
                    self.tile_map
                        .as_ref()
                        .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Map"))?,
                    &permissions,
                    input,
                )
            }
            "tile_map.apply" => {
                let input = decode::<TypedDocumentMutationInput<TileMapDocument>>(arguments)?;
                let loaded = self
                    .tile_map
                    .as_mut()
                    .ok_or_else(|| HeadlessMcpCallFailure::no_document("Tile Map"))?;
                typed_apply(loaded, &permissions, input, |document, path| {
                    persist_native_2d(document.to_canonical_json(), path, "Tile Map")
                })
            }
            "vfx.schemas" => {
                require_empty(arguments)?;
                to_value(vfx_tools.schemas(&permissions)?)
            }
            "vfx.inspect" => {
                let input = decode::<VfxEffectInput>(arguments)?;
                to_value(vfx_tools.inspect(&permissions, input)?)
            }
            "vfx.validate" => {
                let input = decode::<VfxEffectInput>(arguments)?;
                to_value(vfx_tools.validate(&permissions, input)?)
            }
            "vfx.preview" => {
                let input = decode::<VfxMutationInput>(arguments)?;
                to_value(vfx_tools.preview(&permissions, input)?)
            }
            "vfx.apply" => {
                let input = decode::<VfxMutationInput>(arguments)?;
                to_value(vfx_tools.apply(&permissions, input)?)
            }
            "vfx.template" => {
                let input = decode::<VfxTemplateInput>(arguments)?;
                to_value(vfx_tools.template(&permissions, input)?)
            }
            "behavior_tree.schemas" => {
                require_empty(arguments)?;
                to_value(behavior_tools.behavior_tree_schemas(&permissions)?)
            }
            "behavior_tree.validate" => {
                let input = decode::<BehaviorTreeGraphInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_validate(&permissions, input)?)
            }
            "behavior_tree.compile" => {
                let input = decode::<BehaviorTreeGraphInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_compile(&permissions, input)?)
            }
            "behavior_tree.layout" => {
                let input = decode::<BehaviorTreeGraphInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_layout(&permissions, input)?)
            }
            "behavior_tree.nodes" => {
                let input = decode::<BehaviorTreeGraphInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_nodes(&permissions, input)?)
            }
            "behavior_tree.edges" => {
                let input = decode::<BehaviorTreeGraphInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_edges(&permissions, input)?)
            }
            "behavior_tree.apply" => {
                let input = decode::<BehaviorTreeApplyInput>(arguments)?;
                to_value(behavior_tools.behavior_tree_apply(&permissions, input)?)
            }
            _ => Err(HeadlessMcpCallFailure::new(
                "mcp.unknown_tool",
                format!("unknown MCP tool `{name}`"),
            )),
        }
    }

    fn invoke_generic(
        &mut self,
        tools: &AuthoringCapabilityMcpTools,
        verb: AuthoringVerb,
        arguments: Value,
    ) -> Result<Value, HeadlessMcpCallFailure> {
        let input = decode::<CapabilityInvokeInput>(arguments)?;
        let plan = tools
            .plan(verb, &self.permissions, input)
            .map_err(capability_failure)?;
        self.handle_tool_call(&plan.tool, plan.arguments)
    }

    fn scene_ref(&self) -> Result<&AuthoringSession, HeadlessMcpCallFailure> {
        self.scene
            .as_ref()
            .map(|loaded| &loaded.session)
            .ok_or_else(|| HeadlessMcpCallFailure::no_document("Scene"))
    }
    fn graph_ref(&self) -> Result<&Graph, HeadlessMcpCallFailure> {
        self.graph
            .as_ref()
            .map(|loaded| &loaded.document)
            .ok_or_else(|| HeadlessMcpCallFailure::no_document("Graph"))
    }
    fn graph_view_ref(&self) -> Result<&GraphView, HeadlessMcpCallFailure> {
        self.graph_view
            .as_ref()
            .map(|loaded| &loaded.document)
            .ok_or_else(|| HeadlessMcpCallFailure::no_document("GraphView"))
    }
    fn ui_ref(&self) -> Result<&UiAuthoringSession, HeadlessMcpCallFailure> {
        self.ui
            .as_ref()
            .map(|loaded| &loaded.session)
            .ok_or_else(|| HeadlessMcpCallFailure::no_document("UI"))
    }
}

fn latest_shadow_module(project: &ProjectRoot) -> Option<PathBuf> {
    let root = project.game_dir().join(".iroha").join("modules");
    let mut candidates = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .path()
                .join(engine::game_module::packaged_game_module_file_name())
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop()
}

fn load_manifest(project: &ProjectRoot) -> Result<AssetManifest, HeadlessHostError> {
    let path = project.path().join("asset_manifest.json");
    match fs::read_to_string(&path) {
        Ok(json) => AssetManifest::from_json(&json).map_err(|error| HeadlessHostError::Load {
            path,
            message: error.to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AssetManifest::default()),
        Err(error) => Err(HeadlessHostError::Load {
            path,
            message: error.to_string(),
        }),
    }
}

fn asset_path(project: &ProjectRoot, relative: &str) -> Result<PathBuf, HeadlessHostError> {
    project
        .resolve_asset(relative)
        .map_err(|error| HeadlessHostError::Load {
            path: PathBuf::from(relative),
            message: error.to_string(),
        })
}

fn read_text(path: &Path) -> Result<String, HeadlessHostError> {
    fs::read_to_string(path).map_err(|error| HeadlessHostError::Load {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn load_scene(project: &ProjectRoot, relative: String) -> Result<LoadedScene, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let json = read_text(&path)?;
    let scene = load_scene_from_json(&json).map_err(|error| HeadlessHostError::Load {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Ok(LoadedScene {
        path,
        relative,
        session: AuthoringSession::new(scene),
    })
}

fn load_graph(project: &ProjectRoot, relative: String) -> Result<LoadedGraph, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        serde_json::from_str(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedGraph {
        path,
        relative,
        document,
    })
}

fn load_graph_view(
    project: &ProjectRoot,
    relative: String,
) -> Result<LoadedGraphView, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        serde_json::from_str(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedGraphView {
        path,
        relative,
        document,
    })
}

fn load_ui(project: &ProjectRoot, relative: String) -> Result<LoadedUi, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        UiDocument::from_json_str(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedUi {
        path,
        relative,
        session: UiAuthoringSession::new(document),
    })
}

fn load_material(
    project: &ProjectRoot,
    relative: String,
) -> Result<LoadedTyped<MaterialAsset>, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        MaterialAsset::from_json(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedTyped {
        path: Some(path),
        relative,
        document,
        state: TypedDocumentAuthoringState::new(),
    })
}

fn load_animation_set(
    project: &ProjectRoot,
    relative: String,
) -> Result<LoadedTyped<AnimationSet>, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        AnimationSet::from_json(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedTyped {
        path: Some(path),
        relative,
        document,
        state: TypedDocumentAuthoringState::new(),
    })
}

trait Native2dHeadlessDocument: TypedAuthoringDocument + DeserializeOwned {}
impl Native2dHeadlessDocument for SpriteAtlasDocument {}
impl Native2dHeadlessDocument for SpriteAnimationDocument {}
impl Native2dHeadlessDocument for TileSetDocument {}
impl Native2dHeadlessDocument for TileMapDocument {}

fn load_native_2d<T: Native2dHeadlessDocument>(
    project: &ProjectRoot,
    relative: String,
) -> Result<LoadedTyped<T>, HeadlessHostError> {
    let path = asset_path(project, &relative)?;
    let document =
        serde_json::from_str(&read_text(&path)?).map_err(|error| HeadlessHostError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
    Ok(LoadedTyped {
        path: Some(path),
        relative,
        document,
        state: TypedDocumentAuthoringState::new(),
    })
}

fn persist_native_2d(
    json: Result<String, serde_json::Error>,
    path: Option<&Path>,
    label: &str,
) -> Result<(), String> {
    let json = json.map_err(|error| error.to_string())?;
    let path = path.ok_or_else(|| format!("{label} path is unavailable"))?;
    replace_file_contents(path, &json).map_err(|error| error.to_string())
}

fn persist_scene(loaded: &LoadedScene) -> Result<(), HeadlessMcpCallFailure> {
    let json = loaded
        .session
        .scene()
        .to_canonical_json()
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
    replace_file_contents(&loaded.path, &json)
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))
}

fn persist_graph(loaded: &LoadedGraph) -> Result<(), HeadlessMcpCallFailure> {
    let domain = AuthoringGraphDomain::for_graph(&loaded.document)
        .map_err(|error| HeadlessMcpCallFailure::new(error.code(), error.to_string()))?;
    let json = loaded
        .document
        .to_canonical_json(domain.schema_registry())
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
    replace_file_contents(&loaded.path, &json)
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))
}

fn persist_graph_view(
    graph: &Graph,
    loaded: &LoadedGraphView,
) -> Result<(), HeadlessMcpCallFailure> {
    let json = loaded
        .document
        .to_canonical_json(graph)
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
    replace_file_contents(&loaded.path, &json)
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))
}

fn persist_ui(loaded: &LoadedUi) -> Result<(), HeadlessMcpCallFailure> {
    let json = loaded
        .session
        .document()
        .to_json_string()
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))?;
    replace_file_contents(&loaded.path, &json)
        .map_err(|error| HeadlessMcpCallFailure::persist(&loaded.path, error))
}

fn typed_inspect<T: TypedAuthoringDocument>(
    loaded: &LoadedTyped<T>,
    permissions: &AuthoringPermissions,
) -> Result<Value, HeadlessMcpCallFailure> {
    to_value(
        TypedDocumentAuthoringService::new()
            .inspect(&loaded.document, &loaded.state, permissions)
            .map_err(typed_failure)?,
    )
}

fn typed_validate<T: TypedAuthoringDocument>(
    loaded: &LoadedTyped<T>,
    permissions: &AuthoringPermissions,
) -> Result<Value, HeadlessMcpCallFailure> {
    to_value(
        TypedDocumentAuthoringService::new()
            .validate(&loaded.document, &loaded.state, permissions)
            .map_err(typed_failure)?,
    )
}

fn typed_preview<T: TypedAuthoringDocument>(
    loaded: &LoadedTyped<T>,
    permissions: &AuthoringPermissions,
    input: TypedDocumentMutationInput<T>,
) -> Result<Value, HeadlessMcpCallFailure> {
    to_value(
        TypedDocumentAuthoringService::new()
            .preview(
                &loaded.document,
                &loaded.state,
                permissions,
                input.expected_revision,
                input.expected_generation,
                input.replacement,
            )
            .map_err(typed_failure)?,
    )
}

fn typed_apply<T, F>(
    loaded: &mut LoadedTyped<T>,
    permissions: &AuthoringPermissions,
    input: TypedDocumentMutationInput<T>,
    persist: F,
) -> Result<Value, HeadlessMcpCallFailure>
where
    T: TypedAuthoringDocument,
    F: FnOnce(&T, Option<&Path>) -> Result<(), String>,
{
    let before_document = loaded.document.clone();
    let before_state = loaded.state;
    let mutation = TypedDocumentAuthoringService::new()
        .apply(
            &mut loaded.document,
            &mut loaded.state,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.replacement,
        )
        .map_err(typed_failure)?;
    if mutation.success
        && !mutation.diff.is_empty()
        && let Err(message) = persist(&loaded.document, loaded.path.as_deref())
    {
        loaded.document = before_document;
        loaded.state = before_state;
        let path = loaded
            .path
            .as_deref()
            .unwrap_or_else(|| Path::new("project_settings.json"));
        return Err(HeadlessMcpCallFailure::persist(path, message));
    }
    to_value(mutation)
}

fn require_empty(arguments: Value) -> Result<(), HeadlessMcpCallFailure> {
    match arguments {
        Value::Object(values) if values.is_empty() => Ok(()),
        other => Err(HeadlessMcpCallFailure::new(
            "mcp.invalid_arguments",
            format!("tool expects an empty argument object, received {other}"),
        )),
    }
}

fn decode<T: DeserializeOwned>(arguments: Value) -> Result<T, HeadlessMcpCallFailure> {
    serde_json::from_value(arguments)
        .map_err(|error| HeadlessMcpCallFailure::new("mcp.invalid_arguments", error.to_string()))
}

fn to_value<T: Serialize>(value: T) -> Result<Value, HeadlessMcpCallFailure> {
    serde_json::to_value(value).map_err(|error| {
        HeadlessMcpCallFailure::new("mcp.result_serialization_failed", error.to_string())
    })
}

fn generic_failure(error: GenericAuthoringMcpError) -> HeadlessMcpCallFailure {
    HeadlessMcpCallFailure::new(error.code(), error.to_string())
}
fn capability_failure(error: engine_mcp::CapabilityMcpError) -> HeadlessMcpCallFailure {
    HeadlessMcpCallFailure::new(error.code(), error.to_string())
}
fn typed_failure(error: TypedDocumentAuthoringError) -> HeadlessMcpCallFailure {
    HeadlessMcpCallFailure::new(error.code(), error.to_string())
}

impl From<McpToolError> for HeadlessMcpCallFailure {
    fn from(error: McpToolError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AuthoringCommand, EntityId};
    use engine_mcp::authoring_tool_descriptors;
    use serde_json::json;

    fn project() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary project parent");
        let path = directory.path().join("HeadlessMcp");
        engine_project_lifecycle::create_standard_project(&path, "HeadlessMcp")
            .expect("project scaffold");
        (directory, path)
    }

    #[test]
    fn read_only_saved_snapshot_can_coexist_with_editor_writer() {
        let (_directory, path) = project();
        let editor = engine_project_lifecycle::acquire_editor_project(&path).expect("Editor lease");
        let host =
            HeadlessAuthoringHost::open_read_only(&path, HeadlessProjectSelection::default())
                .expect("read-only host");
        assert_eq!(host.access_mode(), HeadlessAccessMode::ReadOnlySavedFiles);
        assert!(!host.view_descriptor().live_editor_unsaved_state_visible);
        assert!(
            HeadlessAuthoringHost::open_writer(&path, HeadlessProjectSelection::default(),)
                .is_err()
        );
        drop(host);
        drop(editor);
    }

    #[test]
    fn read_only_host_reports_saved_snapshot_and_rejects_apply() {
        let (_directory, path) = project();
        let mut host =
            HeadlessAuthoringHost::open_read_only(&path, HeadlessProjectSelection::default())
                .expect("read-only host");
        let project = host
            .handle_tool_call("project.describe", json!({}))
            .expect("describe");
        assert_eq!(project["headless_view"]["source"], "saved_file_snapshot");
        assert_eq!(project["headless_view"]["writable"], false);
        let scene = host
            .handle_tool_call("scene.inspect", json!({}))
            .expect("scene inspect");
        let error = host
            .handle_tool_call(
                "scene.apply",
                json!({
                    "expected_revision": scene["revision"],
                    "expected_generation": scene["generation"],
                    "commands": []
                }),
            )
            .expect_err("read-only mutation must be denied");
        assert_eq!(error.code(), "authoring.permission_denied");
    }

    #[test]
    fn writer_scene_apply_persists_canonical_saved_state() {
        let (_directory, path) = project();
        let mut host =
            HeadlessAuthoringHost::open_writer(&path, HeadlessProjectSelection::default())
                .expect("writer host");
        let scene = host
            .handle_tool_call("scene.inspect", json!({}))
            .expect("scene inspect");
        let entity = EntityId::generate();
        host.handle_tool_call("scene.apply", json!({
            "expected_revision": scene["revision"],
            "expected_generation": scene["generation"],
            "commands": [AuthoringCommand::CreateEntity { id: entity.clone(), name: "headless".into(), parent: None }]
        })).expect("scene apply");
        drop(host);
        let host =
            HeadlessAuthoringHost::open_read_only(&path, HeadlessProjectSelection::default())
                .expect("reopen saved state");
        assert!(
            host.scene_ref()
                .expect("scene")
                .scene()
                .entity(&entity)
                .is_some()
        );
    }

    #[test]
    fn compact_authoring_discovery_is_executable_through_headless_host() {
        let (_directory, path) = project();
        let mut host =
            HeadlessAuthoringHost::open_read_only(&path, HeadlessProjectSelection::default())
                .expect("read-only host");

        let listed = host
            .handle_tool_call("authoring.list", json!({}))
            .expect("compact discovery must execute through the advertised headless tool");
        let capabilities = listed["capabilities"]
            .as_array()
            .expect("compact discovery capabilities");

        assert!(!capabilities.is_empty());
        assert!(capabilities.iter().any(|capability| {
            capability["id"] == "scene.apply" && capability.get("input").is_none()
        }));
    }

    #[test]
    fn host_inventory_is_exactly_engine_mcp_inventory() {
        let names = authoring_tool_descriptors()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"authoring.list".into()));
        assert!(names.contains(&"scene.apply".into()));
        assert!(names.contains(&"material.apply".into()));
        assert!(names.contains(&"behavior_tree.apply".into()));
        assert!(names.contains(&"vfx.apply".into()));
    }
}
