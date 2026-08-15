//! Inspector panel: the Animation Graph and entity Inspectors, and the
//! schema-driven controls that edit one component value.
//!
//! The submodules are layered rather than parallel. [`graph`] and
//! [`entity_panel`] decide what the panel shows; [`schema_editor`] turns one
//! component schema into controls; [`asset_reference`], [`entity_reference`],
//! [`transform`], and [`value_editors`] implement those controls; and
//! [`component_edit`] is the single place where an interaction becomes an
//! authoring command. Working on one layer does not require reading the
//! others.

mod animation_controller;
mod asset_reference;
mod component_edit;
mod entity_panel;
mod entity_reference;
mod graph;
mod layout;
mod property_path;
mod reference_field;
mod schema_editor;
mod transform;
mod value_editors;

// `graph` is absent below because it only extends `EditorApp` with inherent
// methods, which are reached through the type rather than through this module.
pub(in crate::ui) use animation_controller::*;
pub(in crate::ui) use asset_reference::*;
pub(in crate::ui) use component_edit::*;
pub(in crate::ui) use entity_panel::*;
pub(in crate::ui) use entity_reference::*;
pub(in crate::ui) use layout::*;
pub(in crate::ui) use property_path::*;
pub(in crate::ui) use reference_field::*;
pub(in crate::ui) use schema_editor::*;
pub(in crate::ui) use transform::*;
pub(in crate::ui) use value_editors::*;
