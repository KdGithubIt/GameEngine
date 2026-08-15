//! Asset browser panels, asset registration and import, and asset editing
//! windows.
//!
//! Each submodule owns one asset-facing area of the editor and extends
//! `EditorApp` with the methods that drive it. The split follows the panel or
//! window a reader is looking for, so working on one area does not require
//! reading the others.

mod animation_set_window;
mod browser;
mod document_creation;
mod import_settings_window;
mod instantiate;
mod manifest;
mod material_window;
mod model_import;
mod motion_pairing;
mod mutation;
mod retarget_window;

pub(in crate::ui) use animation_set_window::*;
pub(in crate::ui) use browser::*;
pub(in crate::ui) use document_creation::*;
pub(in crate::ui) use import_settings_window::*;
pub(in crate::ui) use manifest::*;
pub(in crate::ui) use material_window::*;
pub(in crate::ui) use motion_pairing::*;
pub(in crate::ui) use mutation::*;
pub(in crate::ui) use retarget_window::*;

/// One of the editor-wide document shortcuts a modeless editor window may claim
/// while it is the frontmost window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum DocumentShortcut {
    Save,
    Undo,
    Redo,
}
