//! Native 2D document and Tile Map authoring workspace (ADR 0127).
//! Persisted mutation semantics stay in GUI-free engine-authoring services.

use eframe::egui;
use engine_authoring::{
    AuthoringPermission, AuthoringPermissions, ProjectRoot, SpriteRef, TypedAuthoringDocument,
    TypedDocumentAuthoringService, TypedDocumentAuthoringState, replace_file_contents,
};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

mod tile_map_pointer;

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
        let relative_path = relative.to_path_buf();
        let relative = relative
            .to_str()
            .ok_or_else(|| "Native 2D document path contains non-UTF-8 characters".to_owned())?;
        let path = project
            .resolve_asset(relative)
            .map_err(|error| error.to_string())?;
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let document: T = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        Ok(Self {
            relative: relative_path,
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
        let json = replacement
            .canonical_json()
            .map_err(|error| error.to_string())?;
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
        let relative_path = relative.to_path_buf();
        let relative = relative
            .to_str()
            .ok_or_else(|| "Tile Map path contains non-UTF-8 characters".to_owned())?;
        let path = project
            .resolve_asset(relative)
            .map_err(|error| error.to_string())?;
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
            relative: relative_path,
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
        let commit = self
            .service
            .commit_gesture()
            .map_err(|error| error.to_string())?;
        self.gesture_active = false;
        self.gesture_start = None;
        self.affected_chunks = commit.affected_chunks;
        self.dirty = true;
        Ok(())
    }

    fn cancel_gesture(&mut self) -> Result<(), String> {
        if self.gesture_active {
            self.service
                .cancel_gesture()
                .map_err(|error| error.to_string())?;
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
    animation_preview: engine::native_2d::SpriteAnimationState2d,
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
                self.animation_preview = engine::native_2d::SpriteAnimationState2d::default();
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
        .filter(|(_, entry)| entry.path.to_ascii_lowercase().ends_with(suffix))
        .map(|(_, entry)| PathBuf::from(&entry.path))
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
                if ui
                    .selectable_label(current == Some(path.as_path()), path.display().to_string())
                    .clicked()
                {
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
        if ui
            .add_enabled(document.is_dirty(), egui::Button::new("Apply"))
            .clicked()
        {
            status = Some(match document.apply_draft() {
                Ok(()) => "Applied validated canonical document".to_owned(),
                Err(error) => format!("Apply failed: {error}"),
            });
        }
        if ui
            .add_enabled(document.is_dirty(), egui::Button::new("Revert"))
            .clicked()
        {
            document.revert_draft();
        }
        if ui
            .add_enabled(!document.undo.is_empty(), egui::Button::new("Undo"))
            .clicked()
        {
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
                        let response = ui.selectable_label(
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
                ui.add(
                    egui::DragValue::new(&mut region.pivot[0])
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
                ui.add(
                    egui::DragValue::new(&mut region.pivot[1])
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
            });
            let mut use_project_ppu = matches!(
                region.pixels_per_unit,
                engine_authoring::PixelsPerUnit::ProjectDefault
            );
            if columns[1]
                .checkbox(&mut use_project_ppu, "Use project pixels/unit")
                .changed()
            {
                region.pixels_per_unit = if use_project_ppu {
                    engine_authoring::PixelsPerUnit::ProjectDefault
                } else {
                    engine_authoring::PixelsPerUnit::Override(100.0)
                };
            }
            if let engine_authoring::PixelsPerUnit::Override(value) = &mut region.pixels_per_unit {
                columns[1].add(
                    egui::DragValue::new(value)
                        .range(0.001..=100_000.0)
                        .prefix("Pixels/unit "),
                );
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
            columns[1].small(
                "Drag a region row into 2D Scene View to create Transform + SpriteRenderer2D.",
            );
        });
    }
}

impl Native2dEditorState {
    fn show_animation(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
    ) {
        let paths = manifest_paths(manifest, ".spriteanim.json");
        let current = self
            .animation
            .as_ref()
            .map(|loaded| loaded.relative.as_path());
        if let Some(path) = document_picker(ui, "native2d_animation_document", current, &paths) {
            self.open_animation(project, &path);
        }
        let Some(loaded) = self.animation.as_mut() else {
            ui.label(
                "Register and select a *.spriteanim.json asset to author deterministic frames.",
            );
            return;
        };
        if let Some(status) = typed_document_toolbar(ui, loaded) {
            self.status = Some(status);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut loaded.draft.ticks_per_second)
                    .range(1..=10_000)
                    .prefix("Ticks/s "),
            );
            ui.checkbox(&mut loaded.draft.looping, "Looping");
            ui.add(
                egui::DragValue::new(&mut loaded.draft.default_speed)
                    .range(0.0..=100.0)
                    .speed(0.05)
                    .prefix("Default speed "),
            );
        });

        ui.columns(2, |columns| {
            columns[0].strong("Frames");
            for (index, frame) in loaded.draft.frames.iter().enumerate() {
                if columns[0]
                    .selectable_label(
                        self.animation_frame == index,
                        format!(
                            "#{index} · {} ticks · {}",
                            frame.duration_ticks,
                            frame.sprite.sprite.as_str()
                        ),
                    )
                    .clicked()
                {
                    self.animation_frame = index;
                }
            }
            if !loaded.draft.frames.is_empty() && columns[0].button("Duplicate frame").clicked() {
                let index = self.animation_frame.min(loaded.draft.frames.len() - 1);
                let frame = loaded.draft.frames[index].clone();
                loaded.draft.frames.insert(index + 1, frame);
                self.animation_frame = index + 1;
            }
            if loaded.draft.frames.len() > 1 && columns[0].button("Remove selected frame").clicked()
            {
                let index = self.animation_frame.min(loaded.draft.frames.len() - 1);
                loaded.draft.frames.remove(index);
                self.animation_frame = self.animation_frame.min(loaded.draft.frames.len() - 1);
                if self.animation_preview.frame_index >= loaded.draft.frames.len() {
                    self.animation_preview.stop();
                }
            }

            if let Some(frame) = loaded.draft.frames.get_mut(self.animation_frame) {
                columns[1].strong(format!("Frame {}", self.animation_frame));
                columns[1].label(format!("Atlas: {}", frame.sprite.atlas.as_str()));
                let sprite_drop =
                    columns[1].label(format!("Sprite: {}", frame.sprite.sprite.as_str()));
                if let Some(payload) = sprite_drop.dnd_release_payload::<SpriteRegionDragPayload>()
                {
                    frame.sprite = payload.sprite.clone();
                }
                columns[1].add(
                    egui::DragValue::new(&mut frame.duration_ticks)
                        .range(1..=u32::MAX)
                        .prefix("Duration ticks "),
                );
                let mut event = frame.event.clone().unwrap_or_default();
                if columns[1].text_edit_singleline(&mut event).changed() {
                    frame.event = (!event.trim().is_empty()).then_some(event);
                }
                columns[1].small(
                    "Drop a Sprite Atlas region on the Sprite row to replace the stable SpriteRef.",
                );
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Play").clicked() {
                self.animation_preview.play();
            }
            if ui.button("Pause").clicked() {
                self.animation_preview.pause();
            }
            if ui.button("Stop").clicked() {
                self.animation_preview.stop();
            }
            if ui.button("Step tick").clicked()
                && self.animation_preview.frame_index < loaded.draft.frames.len()
            {
                self.animation_preview.advance_ticks(&loaded.draft, 1, None);
            }
            ui.label(format!(
                "Runtime preview: frame {} · tick {}",
                self.animation_preview.frame_index, self.animation_preview.tick_in_frame
            ));
        });
        if self.animation_preview.playing
            && self.animation_preview.frame_index < loaded.draft.frames.len()
        {
            self.animation_preview
                .advance_fixed_seconds(&loaded.draft, 1.0 / 60.0, None);
            ui.ctx().request_repaint();
        }
        if let Some(sprite) = self.animation_preview.current_sprite(&loaded.draft) {
            ui.small(format!(
                "Current SpriteRef: {} / {}",
                sprite.atlas.as_str(),
                sprite.sprite.as_str()
            ));
        }
    }
}

impl Native2dEditorState {
    fn show_tile_set(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
    ) {
        let paths = manifest_paths(manifest, ".tileset.json");
        let current = self
            .tile_set
            .as_ref()
            .map(|loaded| loaded.relative.as_path());
        if let Some(path) = document_picker(ui, "native2d_tileset_document", current, &paths) {
            self.open_tile_set(project, &path);
        }
        let Some(loaded) = self.tile_set.as_mut() else {
            ui.label("Register and select a *.tileset.json asset to edit palette and collision metadata.");
            return;
        };
        if let Some(status) = typed_document_toolbar(ui, loaded) {
            self.status = Some(status);
        }
        ui.separator();
        ui.columns(2, |columns| {
            columns[0].strong("Tile palette");
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(&mut columns[0], |ui| {
                    for (index, tile) in loaded.draft.tiles.iter().enumerate() {
                        if ui
                            .selectable_label(
                                self.tile_index == index,
                                format!("{}  {}", tile.name, tile.id.as_str()),
                            )
                            .clicked()
                        {
                            self.tile_index = index;
                        }
                    }
                });
            if !loaded.draft.tiles.is_empty() && columns[0].button("Duplicate tile").clicked() {
                let index = self.tile_index.min(loaded.draft.tiles.len() - 1);
                let mut tile = loaded.draft.tiles[index].clone();
                tile.id = engine_authoring::TileId::generate();
                tile.name = format!("{} Copy", tile.name);
                loaded.draft.tiles.push(tile);
                self.tile_index = loaded.draft.tiles.len() - 1;
            }

            let Some(tile) = loaded.draft.tiles.get_mut(self.tile_index) else {
                columns[1].label("Select a tile to edit its stable palette entry.");
                return;
            };
            columns[1].strong("Tile definition");
            columns[1].label(format!("Stable ID: {}", tile.id.as_str()));
            columns[1].horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut tile.name);
            });
            let sprite_drop = columns[1].label(format!(
                "Sprite: {} / {}",
                tile.sprite.atlas.as_str(),
                tile.sprite.sprite.as_str()
            ));
            if let Some(payload) = sprite_drop.dnd_release_payload::<SpriteRegionDragPayload>() {
                tile.sprite = payload.sprite.clone();
            }
            columns[1].checkbox(&mut tile.one_way, "One-way platform surface");
            let mut tags = tile.tags.join(", ");
            columns[1].horizontal(|ui| {
                ui.label("Tags");
                if ui.text_edit_singleline(&mut tags).changed() {
                    tile.tags = tags
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
            });
            columns[1].separator();
            columns[1].strong("Collision shapes");
            let mut remove_shape = None;
            for (index, shape) in tile.collision.iter_mut().enumerate() {
                columns[1].group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("Shape {}", index + 1));
                        if ui.small_button("Remove").clicked() {
                            remove_shape = Some(index);
                        }
                    });
                    match shape {
                        engine_authoring::TileCollisionShape::Box { half_extents } => {
                            ui.label("Box");
                            ui.add(
                                egui::DragValue::new(&mut half_extents[0])
                                    .range(0.001..=1000.0)
                                    .prefix("Half X "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut half_extents[1])
                                    .range(0.001..=1000.0)
                                    .prefix("Half Y "),
                            );
                        }
                        engine_authoring::TileCollisionShape::Circle { radius } => {
                            ui.label("Circle");
                            ui.add(
                                egui::DragValue::new(radius)
                                    .range(0.001..=1000.0)
                                    .prefix("Radius "),
                            );
                        }
                        engine_authoring::TileCollisionShape::Polygon { points } => {
                            ui.label(format!("Polygon · {} vertices", points.len()));
                            for (point_index, point) in points.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(format!("P{point_index}"));
                                    ui.add(egui::DragValue::new(&mut point[0]).speed(0.05));
                                    ui.add(egui::DragValue::new(&mut point[1]).speed(0.05));
                                });
                            }
                        }
                    }
                });
            }
            if let Some(index) = remove_shape {
                tile.collision.remove(index);
            }
            columns[1].horizontal_wrapped(|ui| {
                if ui.button("Add Box").clicked() {
                    tile.collision
                        .push(engine_authoring::TileCollisionShape::Box {
                            half_extents: [0.5, 0.5],
                        });
                }
                if ui.button("Add Circle").clicked() {
                    tile.collision
                        .push(engine_authoring::TileCollisionShape::Circle { radius: 0.5 });
                }
                if ui.button("Add Polygon").clicked() {
                    tile.collision
                        .push(engine_authoring::TileCollisionShape::Polygon {
                            points: vec![[-0.5, -0.5], [0.5, -0.5], [0.0, 0.5]],
                        });
                }
            });
        });
    }
}

impl TileTool2d {
    fn label(self) -> &'static str {
        match self {
            Self::Paint => "Paint",
            Self::Erase => "Erase",
            Self::Rectangle => "Rectangle",
            Self::Line => "Line",
            Self::Fill => "Fill",
            Self::Eyedropper => "Eyedropper",
            Self::SelectStamp => "Select/Stamp",
        }
    }
}

impl Native2dEditorState {
    fn show_tile_map(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        manifest: &engine::AssetManifest,
    ) {
        let paths = manifest_paths(manifest, ".tilemap.json");
        let current = self
            .tile_map
            .as_ref()
            .map(|loaded| loaded.relative.as_path());
        if let Some(path) = document_picker(ui, "native2d_tilemap_document", current, &paths) {
            self.open_tile_map(project, manifest, &path);
        }
        let Some(loaded) = self.tile_map.as_mut() else {
            ui.label("Register and select a *.tilemap.json asset to paint sparse chunks.");
            return;
        };

        let layer_count = loaded.service.preview().layers.len();
        if layer_count == 0 {
            ui.colored_label(
                egui::Color32::YELLOW,
                "This Tile Map has no layer. Add a layer through a typed-document client first.",
            );
            return;
        }
        loaded.selected_layer = loaded.selected_layer.min(layer_count - 1);
        ui.horizontal_wrapped(|ui| {
            ui.label(loaded.relative.display().to_string());
            if loaded.dirty {
                ui.colored_label(egui::Color32::YELLOW, "Modified");
            }
            if ui
                .add_enabled(loaded.dirty, egui::Button::new("Save"))
                .clicked()
            {
                self.status = Some(match loaded.save() {
                    Ok(()) => "Saved canonical Tile Map".to_owned(),
                    Err(error) => format!("Tile Map save failed: {error}"),
                });
            }
            if ui
                .add_enabled(!loaded.gesture_active, egui::Button::new("Undo stroke"))
                .clicked()
                && loaded.service.undo()
            {
                loaded.dirty = true;
                loaded.affected_chunks.clear();
            }
            if ui
                .add_enabled(loaded.gesture_active, egui::Button::new("Cancel stroke"))
                .clicked()
            {
                self.status = loaded.cancel_gesture().err();
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.strong("Layer");
            let layers = &loaded.service.preview().layers;
            for (index, layer) in layers.iter().enumerate() {
                let label = if layer.locked {
                    format!("{} 🔒", layer.name)
                } else {
                    layer.name.clone()
                };
                ui.selectable_value(&mut loaded.selected_layer, index, label);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.strong("Palette");
            for tile in &loaded.tiles.tiles {
                ui.selectable_value(&mut loaded.selected_tile, Some(tile.id.clone()), &tile.name);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for tool in [
                TileTool2d::Paint,
                TileTool2d::Erase,
                TileTool2d::Rectangle,
                TileTool2d::Line,
                TileTool2d::Fill,
                TileTool2d::Eyedropper,
                TileTool2d::SelectStamp,
            ] {
                ui.selectable_value(&mut self.tile_tool, tool, tool.label());
            }
            ui.separator();
            ui.checkbox(&mut self.show_grid, "Grid");
            ui.checkbox(&mut self.show_chunks, "Chunks");
            ui.checkbox(&mut self.show_collisions, "Collision");
            if !loaded.stamp.cells.is_empty() && ui.button("Clear Stamp").clicked() {
                loaded.stamp = engine_authoring::TileStamp::default();
            }
        });

        ui.separator();
        if let Err(error) = show_tile_map_canvas(
            ui,
            loaded,
            self.tile_tool,
            self.show_grid,
            self.show_chunks,
            self.show_collisions,
        ) {
            self.status = Some(error);
        }
        if !loaded.affected_chunks.is_empty() {
            ui.small(format!(
                "Last gesture invalidated {} chunk(s): {}",
                loaded.affected_chunks.len(),
                loaded
                    .affected_chunks
                    .iter()
                    .map(|key| format!("{}:{},{}", key.layer.as_str(), key.chunk.x, key.chunk.y))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        ui.small("One pointer gesture commits one semantic undo entry; Escape/Cancel restores the exact pre-stroke cells.");
    }
}

fn show_tile_map_canvas(
    ui: &mut egui::Ui,
    loaded: &mut LoadedTileMap,
    tool: TileTool2d,
    show_grid: bool,
    show_chunks: bool,
    show_collisions: bool,
) -> Result<(), String> {
    const MIN_X: i32 = -12;
    const MAX_X: i32 = 12;
    const MIN_Y: i32 = -7;
    const MAX_Y: i32 = 7;
    const CELL_PX: f32 = 30.0;
    const WORK_BUDGET: usize = 16_384;

    let layer_id = loaded
        .service
        .preview()
        .layers
        .get(loaded.selected_layer)
        .map(|layer| layer.id.clone())
        .ok_or_else(|| "selected Tile Map layer disappeared".to_owned())?;
    let desired = egui::vec2(
        ((MAX_X - MIN_X + 1) as f32 * CELL_PX).min(ui.available_width()),
        (MAX_Y - MIN_Y + 1) as f32 * CELL_PX,
    );
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(28, 31, 36));

    let visible_width = MAX_X - MIN_X + 1;
    let visible_height = MAX_Y - MIN_Y + 1;
    let cell_size = egui::vec2(
        rect.width() / visible_width as f32,
        rect.height() / visible_height as f32,
    );
    {
        let preview = loaded.service.preview();
        for y in MIN_Y..=MAX_Y {
            for x in MIN_X..=MAX_X {
                let cell = engine_authoring::TileCell { x, y };
                let cell_rect = tile_cell_rect(rect, cell_size, cell, MIN_X, MAX_Y);
                if let Some(tile_id) = preview.tile_at(&layer_id, cell) {
                    painter.rect_filled(cell_rect.shrink(1.0), 1.0, tile_color(tile_id));
                    if show_collisions
                        && let Some(tile) = loaded.tiles.tile(tile_id)
                        && (!tile.collision.is_empty() || tile.one_way)
                    {
                        let color = if tile.one_way {
                            egui::Color32::from_rgb(245, 180, 75)
                        } else {
                            egui::Color32::from_rgb(80, 220, 225)
                        };
                        painter.rect_stroke(
                            cell_rect.shrink(3.0),
                            1.0,
                            egui::Stroke::new(2.0_f32, color),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                if show_grid {
                    painter.rect_stroke(
                        cell_rect,
                        0.0,
                        egui::Stroke::new(0.5_f32, egui::Color32::from_gray(64)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }
    if show_chunks {
        let chunk = i32::from(loaded.service.preview().chunk_size.max(1));
        for x in MIN_X..=MAX_X + 1 {
            if x.rem_euclid(chunk) == 0 {
                let px = rect.left() + (x - MIN_X) as f32 * cell_size.x;
                painter.line_segment(
                    [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(190, 125, 55)),
                );
            }
        }
        for y in MIN_Y..=MAX_Y + 1 {
            if y.rem_euclid(chunk) == 0 {
                let py = rect.top() + (MAX_Y - y + 1) as f32 * cell_size.y;
                painter.line_segment(
                    [egui::pos2(rect.left(), py), egui::pos2(rect.right(), py)],
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(190, 125, 55)),
                );
            }
        }
    }
    let origin_x = rect.left() + (-MIN_X) as f32 * cell_size.x;
    let origin_y = rect.top() + (MAX_Y + 1) as f32 * cell_size.y;
    painter.line_segment(
        [
            egui::pos2(origin_x, rect.top()),
            egui::pos2(origin_x, rect.bottom()),
        ],
        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(190, 80, 80)),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), origin_y),
            egui::pos2(rect.right(), origin_y),
        ],
        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(80, 190, 100)),
    );

    let pointer_cell = response
        .interact_pointer_pos()
        .and_then(|position| tile_cell_from_pointer(rect, cell_size, position, MIN_X, MAX_Y));
    if let Some(cell) = pointer_cell {
        let hover = tile_cell_rect(rect, cell_size, cell, MIN_X, MAX_Y);
        painter.rect_stroke(
            hover.shrink(1.0),
            1.0,
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    tile_map_pointer::handle(
        tile_map_pointer::PointerContext {
            ui,
            response: &response,
            layer: &layer_id,
            visible_bounds: engine_authoring::TileRect::from_corners(
                engine_authoring::TileCell { x: MIN_X, y: MIN_Y },
                engine_authoring::TileCell { x: MAX_X, y: MAX_Y },
            ),
            work_budget: WORK_BUDGET,
        },
        loaded,
        pointer_cell,
        tool,
    )
}

fn tile_cell_rect(
    rect: egui::Rect,
    cell_size: egui::Vec2,
    cell: engine_authoring::TileCell,
    min_x: i32,
    max_y: i32,
) -> egui::Rect {
    let left = rect.left() + (cell.x - min_x) as f32 * cell_size.x;
    let top = rect.top() + (max_y - cell.y) as f32 * cell_size.y;
    egui::Rect::from_min_size(egui::pos2(left, top), cell_size)
}

fn tile_cell_from_pointer(
    rect: egui::Rect,
    cell_size: egui::Vec2,
    position: egui::Pos2,
    min_x: i32,
    max_y: i32,
) -> Option<engine_authoring::TileCell> {
    if !rect.contains(position) {
        return None;
    }
    Some(engine_authoring::TileCell {
        x: min_x + ((position.x - rect.left()) / cell_size.x).floor() as i32,
        y: max_y - ((position.y - rect.top()) / cell_size.y).floor() as i32,
    })
}

fn tile_color(tile: &engine_authoring::TileId) -> egui::Color32 {
    let hash = tile.as_str().bytes().fold(0_u32, |state, byte| {
        state.wrapping_mul(33).wrapping_add(u32::from(byte))
    });
    egui::Color32::from_rgb(
        70 + (hash & 63) as u8,
        90 + ((hash >> 6) & 63) as u8,
        105 + ((hash >> 12) & 63) as u8,
    )
}

// __ADR0127_NATIVE2D_EDITOR_CONTINUE__
