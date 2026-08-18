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

// __ADR0127_NATIVE2D_EDITOR_CONTINUE__
