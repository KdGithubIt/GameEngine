//! Native 2D document and Tile Map authoring workspace (ADR 0127).
//! Persisted mutation semantics stay in GUI-free engine-authoring services.

use eframe::egui;
use engine_authoring::{
    replace_file_contents, AuthoringPermission, AuthoringPermissions, ProjectRoot, SpriteRef,
    TypedAuthoringDocument, TypedDocumentAuthoringService, TypedDocumentAuthoringState,
};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SpriteRegionDragPayload {
    pub sprite: SpriteRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Native2dWorkspace {
    #[default]
    SpriteAtlas,
    SpriteAnimation,
    TileSet,
    TileMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TileTool2d {
    #[default]
    Paint,
    Erase,
    Rectangle,
    Line,
    Fill,
    Eyedropper,
    SelectStamp,
}

trait Native2dDocument: TypedAuthoringDocument + DeserializeOwned {
    fn canonical_json(&self) -> Result<String, serde_json::Error>;
}

macro_rules! native_document {
    ($ty:ty) => {
        impl Native2dDocument for $ty {
            fn canonical_json(&self) -> Result<String, serde_json::Error> {
                self.to_canonical_json()
            }
        }
    };
}

native_document!(engine_authoring::SpriteAtlasDocument);
native_document!(engine_authoring::SpriteAnimationDocument);
native_document!(engine_authoring::TileSetDocument);

struct LoadedTyped<T> {
    relative: PathBuf,
    path: PathBuf,
    document: T,
    draft: T,
    state: TypedDocumentAuthoringState,
    undo: Vec<T>,
}

impl<T: Native2dDocument> LoadedTyped<T> {
    fn open(project: &ProjectRoot, relative: &Path) -> Result<Self, String> {
        let path = project.resolve_asset(relative).map_err(|error| error.to_string())?;
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let document: T = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(Self {
            relative: relative.to_path_buf(),
            path,
            draft: document.clone(),
            document,
            state: TypedDocumentAuthoringState::new(),
            undo: Vec::new(),
        })
    }

    fn is_dirty(&self) -> bool {
        self.draft != self.document
    }

    fn revert_draft(&mut self) {
        self.draft = self.document.clone();
    }

    fn apply_draft(&mut self) -> Result<(), String> {
        self.apply_replacement(self.draft.clone(), true)
    }

    fn undo(&mut self) -> Result<bool, String> {
        let Some(previous) = self.undo.pop() else {
            return Ok(false);
        };
        self.apply_replacement(previous, false)?;
        Ok(true)
    }

    fn apply_replacement(&mut self, replacement: T, record_undo: bool) -> Result<(), String> {
        let service = TypedDocumentAuthoringService::new();
        let permissions = writable_permissions();
        let base = service
            .inspect(&self.document, &self.state, &permissions)
            .map_err(|error| error.to_string())?;
        let preview = service
            .preview(
                &self.document,
                &self.state,
                &permissions,
                base.revision,
                base.generation,
                replacement.clone(),
            )
            .map_err(|error| error.to_string())?;
        if !preview.success {
            return Err(preview
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "));
        }
        if preview.diff.is_empty() {
            self.draft = self.document.clone();
            return Ok(());
        }
        let json = replacement.canonical_json().map_err(|error| error.to_string())?;
        replace_file_contents(&self.path, &json).map_err(|error| error.to_string())?;
        let before = self.document.clone();
        let applied = service
            .apply(
                &mut self.document,
                &mut self.state,
                &permissions,
                base.revision,
                base.generation,
                replacement,
            )
            .map_err(|error| error.to_string())?;
        if !applied.success {
            return Err("typed-document apply diverged from successful preview".into());
        }
        if record_undo {
            self.undo.push(before);
        }
        self.draft = self.document.clone();
        Ok(())
    }
}

fn writable_permissions() -> AuthoringPermissions {
    AuthoringPermissions::read_only()
        .with(AuthoringPermission::Preview)
        .with(AuthoringPermission::ProjectDataWrite)
        .with(AuthoringPermission::AssetWrite)
}

struct LoadedTileMap {
    relative: PathBuf,
    path: PathBuf,
    service: engine_authoring::TileMapAuthoringService,
    tiles: engine_authoring::TileSetDocument,
    selected_layer: usize,
    selected_tile: Option<engine_authoring::TileId>,
    gesture_start: Option<engine_authoring::TileCell>,
    gesture_active: bool,
    stamp: engine_authoring::TileStamp,
    dirty: bool,
    affected_chunks: Vec<engine_authoring::TileMapChunkKey>,
}

impl LoadedTileMap {
    fn open(
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
        relative: &Path,
    ) -> Result<Self, String> {
        let path = project.resolve_asset(relative).map_err(|error| error.to_string())?;
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let document = engine_authoring::TileMapDocument::from_json(&text)
            .map_err(|error| error.to_string())?;
        let tile_set_path = manifest
            .iter()
            .find(|(id, _)| *id == &document.tile_set)
            .map(|(_, entry)| entry.path.clone())
            .ok_or_else(|| {
                format!(
                    "Tile Map references unregistered Tile Set `{}`",
                    document.tile_set.as_str()
                )
            })?;
        let tile_set_path = project
            .resolve_asset(&tile_set_path)
            .map_err(|error| error.to_string())?;
        let tiles = engine_authoring::TileSetDocument::from_json(
            &std::fs::read_to_string(tile_set_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let selected_tile = tiles.tiles.first().map(|tile| tile.id.clone());
        Ok(Self {
            relative: relative.to_path_buf(),
            path,
            service: engine_authoring::TileMapAuthoringService::new(document),
            tiles,
            selected_layer: 0,
            selected_tile,
            gesture_start: None,
            gesture_active: false,
            stamp: engine_authoring::TileStamp::default(),
            dirty: false,
            affected_chunks: Vec::new(),
        })
    }

    fn save(&mut self) -> Result<(), String> {
        let json = self
            .service
            .document()
            .to_canonical_json()
            .map_err(|error| error.to_string())?;
        replace_file_contents(&self.path, &json).map_err(|error| error.to_string())?;
        self.dirty = false;
        Ok(())
    }

    fn commit_gesture(&mut self) -> Result<(), String> {
        let commit = self.service.commit_gesture().map_err(|error| error.to_string())?;
        self.gesture_active = false;
        self.gesture_start = None;
        self.affected_chunks = commit.affected_chunks;
        self.dirty = true;
        Ok(())
    }

    fn cancel_gesture(&mut self) -> Result<(), String> {
        if self.gesture_active {
            self.service.cancel_gesture().map_err(|error| error.to_string())?;
        }
        self.gesture_active = false;
        self.gesture_start = None;
        Ok(())
    }
}

/// Persistent UI state for the modeless Native 2D authoring workspace.
#[derive(Default)]
pub struct Native2dEditorState {
    workspace: Native2dWorkspace,
    atlas: Option<LoadedTyped<engine_authoring::SpriteAtlasDocument>>,
    animation: Option<LoadedTyped<engine_authoring::SpriteAnimationDocument>>,
    tile_set: Option<LoadedTyped<engine_authoring::TileSetDocument>>,
    tile_map: Option<LoadedTileMap>,
    atlas_region: usize,
    animation_frame: usize,
    animation_preview: engine::SpriteAnimationState2d,
    tile_index: usize,
    tile_tool: TileTool2d,
    show_grid: bool,
    show_chunks: bool,
    show_collisions: bool,
    status: Option<String>,
}

impl Native2dEditorState {
    /// Draws the real service-backed Native 2D authoring surface.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(
                &mut self.workspace,
                Native2dWorkspace::SpriteAtlas,
                "Sprite Atlas",
            );
            ui.selectable_value(
                &mut self.workspace,
                Native2dWorkspace::SpriteAnimation,
                "Sprite Animation",
            );
            ui.selectable_value(&mut self.workspace, Native2dWorkspace::TileSet, "Tile Set");
            ui.selectable_value(&mut self.workspace, Native2dWorkspace::TileMap, "Tile Map");
        });
        ui.separator();

        match self.workspace {
            Native2dWorkspace::SpriteAtlas => self.show_atlas(ui, project, manifest),
            Native2dWorkspace::SpriteAnimation => self.show_animation(ui, project, manifest),
            Native2dWorkspace::TileSet => self.show_tile_set(ui, project, manifest),
            Native2dWorkspace::TileMap => self.show_tile_map(ui, project, manifest),
        }
        if let Some(status) = &self.status {
            ui.separator();
            ui.label(status);
        }
    }

    fn open_atlas(&mut self, project: &ProjectRoot, relative: &Path) {
        match LoadedTyped::open(project, relative) {
            Ok(document) => {
                self.atlas = Some(document);
                self.atlas_region = 0;
                self.status = None;
            }
            Err(error) => self.status = Some(error),
        }
    }

    fn open_animation(&mut self, project: &ProjectRoot, relative: &Path) {
        match LoadedTyped::open(project, relative) {
            Ok(document) => {
                self.animation = Some(document);
                self.animation_frame = 0;
                self.animation_preview = engine::SpriteAnimationState2d::default();
                self.status = None;
            }
            Err(error) => self.status = Some(error),
        }
    }

    fn open_tile_set(&mut self, project: &ProjectRoot, relative: &Path) {
        match LoadedTyped::open(project, relative) {
            Ok(document) => {
                self.tile_set = Some(document);
                self.tile_index = 0;
                self.status = None;
            }
            Err(error) => self.status = Some(error),
        }
    }

    fn open_tile_map(
        &mut self,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
        relative: &Path,
    ) {
        match LoadedTileMap::open(project, manifest, relative) {
            Ok(document) => {
                self.tile_map = Some(document);
                self.status = None;
            }
            Err(error) => self.status = Some(error),
        }
    }
}

fn manifest_paths(manifest: &engine::AssetManifest, suffix: &str) -> Vec<PathBuf> {
    let mut paths = manifest
        .iter()
        .filter_map(|(_, entry)| {
            entry
                .path
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(suffix)
                .then(|| entry.path.clone())
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn document_picker(
    ui: &mut egui::Ui,
    id: &'static str,
    current: Option<&Path>,
    paths: &[PathBuf],
) -> Option<PathBuf> {
    let mut selected = None;
    let label = current
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Select document...".to_owned());
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(ui.available_width().min(420.0))
        .show_ui(ui, |ui| {
            for path in paths {
                if ui.selectable_label(current == Some(path.as_path()), path.display().to_string()).clicked() {
                    selected = Some(path.clone());
                }
            }
        });
    selected
}

fn typed_document_toolbar<T: Native2dDocument>(
    ui: &mut egui::Ui,
    document: &mut LoadedTyped<T>,
) -> Option<String> {
    let mut status = None;
    ui.horizontal(|ui| {
        ui.label(document.relative.display().to_string());
        if document.is_dirty() {
            ui.colored_label(egui::Color32::YELLOW, "Modified");
        }
        if ui.add_enabled(document.is_dirty(), egui::Button::new("Apply")).clicked() {
            status = Some(match document.apply_draft() {
                Ok(()) => "Applied validated canonical document".to_owned(),
                Err(error) => format!("Apply failed: {error}"),
            });
        }
        if ui.add_enabled(document.is_dirty(), egui::Button::new("Revert")).clicked() {
            document.revert_draft();
        }
        if ui.add_enabled(!document.undo.is_empty(), egui::Button::new("Undo")).clicked() {
            status = Some(match document.undo() {
                Ok(true) => "Undid one typed-document transaction".to_owned(),
                Ok(false) => "Nothing to undo".to_owned(),
                Err(error) => format!("Undo failed: {error}"),
            });
        }
    });
    status
}

impl Native2dEditorState {
    fn show_atlas(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
    ) {
        let paths = manifest_paths(manifest, ".spriteatlas.json");
        let current = self.atlas.as_ref().map(|loaded| loaded.relative.as_path());
        if let Some(path) = document_picker(ui, "native2d_atlas_document", current, &paths) {
            self.open_atlas(project, &path);
        }
        let Some(loaded) = self.atlas.as_mut() else {
            ui.label("Register and select a *.spriteatlas.json asset to edit stable regions.");
            return;
        };
        if let Some(status) = typed_document_toolbar(ui, loaded) {
            self.status = Some(status);
        }
        ui.separator();

        let atlas_id = manifest
            .iter()
            .find(|(_, entry)| entry.path == loaded.relative)
            .map(|(id, _)| id.clone());
        ui.columns(2, |columns| {
            columns[0].strong("Regions / slices");
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(&mut columns[0], |ui| {
                    for (index, region) in loaded.draft.regions.iter().enumerate() {
                        let response = ui
                            .selectable_label(
                                self.atlas_region == index,
                                format!("{}  {}", region.name, region.id.as_str()),
                            );
                        if response.clicked() {
                            self.atlas_region = index;
                        }
                        if let Some(atlas) = atlas_id.clone() {
                            response.dnd_set_drag_payload(SpriteRegionDragPayload {
                                sprite: SpriteRef {
                                    atlas,
                                    sprite: region.id.clone(),
                                },
                            });
                        }
                    }
                });
            if !loaded.draft.regions.is_empty()
                && columns[0].button("Duplicate as new slice").clicked()
            {
                let source = loaded.draft.regions
                    [self.atlas_region.min(loaded.draft.regions.len() - 1)]
                    .clone();
                let mut slice = source;
                slice.id = engine_authoring::SpriteId::generate();
                slice.name = format!("{} Copy", slice.name);
                slice.rect.x = slice.rect.x.saturating_add(slice.rect.width);
                loaded.draft.regions.push(slice);
                self.atlas_region = loaded.draft.regions.len() - 1;
            }

            let Some(region) = loaded.draft.regions.get_mut(self.atlas_region) else {
                columns[1].label("Create or select a region to edit its slicing metadata.");
                return;
            };
            columns[1].strong("Selected region");
            columns[1].label(format!("Stable ID: {}", region.id.as_str()));
            columns[1].horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut region.name);
            });
            columns[1].horizontal(|ui| {
                ui.label("Rect x/y/w/h");
                ui.add(egui::DragValue::new(&mut region.rect.x));
                ui.add(egui::DragValue::new(&mut region.rect.y));
                ui.add(egui::DragValue::new(&mut region.rect.width).range(1..=u32::MAX));
                ui.add(egui::DragValue::new(&mut region.rect.height).range(1..=u32::MAX));
            });
            columns[1].horizontal(|ui| {
                ui.label("Pivot");
                ui.add(egui::DragValue::new(&mut region.pivot[0]).range(0.0..=1.0).speed(0.01));
                ui.add(egui::DragValue::new(&mut region.pivot[1]).range(0.0..=1.0).speed(0.01));
            });
            let mut use_project_ppu = matches!(region.pixels_per_unit, engine_authoring::PixelsPerUnit::ProjectDefault);
            if columns[1].checkbox(&mut use_project_ppu, "Use project pixels/unit").changed() {
                region.pixels_per_unit = if use_project_ppu {
                    engine_authoring::PixelsPerUnit::ProjectDefault
                } else {
                    engine_authoring::PixelsPerUnit::Override(100.0)
                };
            }
            if let engine_authoring::PixelsPerUnit::Override(value) = &mut region.pixels_per_unit {
                columns[1].add(egui::DragValue::new(value).range(0.001..=100_000.0).prefix("Pixels/unit "));
            }
            columns[1].horizontal(|ui| {
                ui.label("Filtering");
                ui.selectable_value(&mut region.filtering, None, "Project");
                ui.selectable_value(
                    &mut region.filtering,
                    Some(engine_authoring::SpriteFiltering::Nearest),
                    "Nearest",
                );
                ui.selectable_value(
                    &mut region.filtering,
                    Some(engine_authoring::SpriteFiltering::Linear),
                    "Linear",
                );
            });
            columns[1].add(
                egui::DragValue::new(&mut region.extrusion_pixels)
                    .range(0..=32)
                    .prefix("Extrusion "),
            );
            columns[1].small("Drag a region row into 2D Scene View to create Transform + SpriteRenderer2D.");
        });
    }
}

// __ADR0127_NATIVE2D_EDITOR_CONTINUE__
