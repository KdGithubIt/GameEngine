//! Lookups shared by the built-in component declaration table.
//!
//! Component fields themselves are declared once in
//! [`super::builtins`]; only the two lookups that reach outside that table
//! live here.

use engine_authoring::id::{AssetId, ComponentTypeId};
use engine_authoring::schema::{ComponentSchema, ComponentSchemaRegistry};

/// Returns the authoring crate's schema for an engine built-in component.
///
/// `engine.transform` and `engine.player_marker` are owned by the authoring
/// built-in registry, which is the format authority for them. Restating their
/// fields in the engine declaration table would create a second source of
/// truth for a persisted format.
///
/// # Panics
///
/// Panics when `component_type` is not an engine built-in registered by the
/// authoring crate. Callers pass only the `engine.*` constants declared in
/// `crate::scene_bridge`, so a failure here is a build-time mistake in the
/// declaration table rather than a recoverable runtime condition.
pub(super) fn authoring_builtin_schema(component_type: &str) -> ComponentSchema {
    ComponentSchemaRegistry::builtin()
        .get(&ComponentTypeId::new(component_type))
        .cloned()
        .expect("engine built-in component schema must exist in authoring registry")
}

/// Converts a built-in asset ID constant into an [`AssetId`].
///
/// # Panics
///
/// Panics when `id` is not a valid stable ID. Callers pass only the
/// `BUILTIN_*_ASSET_ID` constants declared in `crate::scene_bridge`.
pub(super) fn builtin_asset_id(id: &str) -> AssetId {
    AssetId::from_stable_id(engine_authoring::id::StableId::new(id))
        .expect("built-in asset id constants must be valid asset ids")
}
