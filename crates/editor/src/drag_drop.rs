//! Drag-and-drop payload and target types for the asset browser workflow
//! (Phase 32).
//!
//! All drop operations route through `AuthoringCommand` and are undoable.
//! Prefab drops are gated on Phase 33 completion; they must not be performed
//! before the prefab module is available.

use crate::asset_browser::AssetKind;
use engine_authoring::id::{AssetId, ComponentTypeId, EntityId};
use std::path::PathBuf;

/// The data carried by a drag operation that started in the Asset Browser.
///
/// egui keeps exactly one drag payload at a time, so this type carries both
/// things an asset drag can mean: dropping a *reference* on the Scene View or
/// an Inspector field, and relocating the dragged *files* into a folder. Two
/// separate payload types would silently overwrite each other.
#[derive(Debug, Clone)]
pub struct DragPayload {
    /// The stable authoring identifier for the dragged asset.
    pub asset_id: AssetId,
    /// Path relative to `assets/` for this asset.
    pub relative_path: PathBuf,
    /// Semantic category of the dragged asset.
    pub kind: AssetKind,
    /// Every asset-relative path in the drag, which is the whole selection
    /// when the dragged row was part of it. Folder drop targets move these.
    pub paths: Vec<PathBuf>,
}

/// Where a [`DragPayload`] was dropped.
#[derive(Debug, Clone)]
pub enum DropTarget {
    /// Dropped onto an `AssetRef` field in the Inspector.
    ///
    /// The assignment is performed by replacing the named component field with
    /// the asset ID string value.
    InspectorField {
        /// The entity whose component field receives the asset.
        entity: EntityId,
        /// The component type that owns the field.
        component_type: ComponentTypeId,
        /// The field name inside the component value object.
        field: String,
    },
    /// Dropped onto the Scene View canvas.
    ///
    /// For a mesh asset this creates a new root entity with a `Transform` and
    /// `Mesh` component.
    SceneView,
    /// Dropped onto the Hierarchy panel, optionally onto a specific entity.
    ///
    /// For a mesh asset this creates a new entity as a child of `parent`.
    /// When `parent` is `None` the entity is created at the scene root.
    Hierarchy {
        /// The entity under the cursor at drop time, or `None` for root.
        parent: Option<EntityId>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::StableId;

    fn make_asset_id() -> AssetId {
        AssetId::from_stable_id(StableId::new("asset_01JP0000000000000000000DDD"))
            .expect("id must be valid")
    }

    #[test]
    fn drag_payload_carries_asset_kind_and_path() {
        let id = make_asset_id();
        let payload = DragPayload {
            asset_id: id.clone(),
            relative_path: PathBuf::from("meshes/cube.obj"),
            kind: AssetKind::Mesh,
            paths: vec![PathBuf::from("meshes/cube.obj")],
        };
        assert_eq!(payload.kind, AssetKind::Mesh);
        assert_eq!(payload.asset_id, id);
        assert_eq!(payload.paths.len(), 1);
    }

    #[test]
    fn one_payload_serves_both_reference_drops_and_file_moves() {
        // A second `dnd_set_drag_payload` call would replace the first, so a
        // registered asset must advertise its reference and its paths in the
        // same payload or one of the two drop targets silently stops working.
        let payload = DragPayload {
            asset_id: make_asset_id(),
            relative_path: PathBuf::from("meshes/cube.obj"),
            kind: AssetKind::Mesh,
            paths: vec![
                PathBuf::from("meshes/cube.obj"),
                PathBuf::from("meshes/ramp.obj"),
            ],
        };

        assert_eq!(payload.kind, AssetKind::Mesh, "Scene View reads the kind");
        assert_eq!(payload.paths.len(), 2, "folder targets read every path");
    }

    #[test]
    fn drop_target_inspector_field_holds_component_context() {
        let entity = EntityId::generate();
        let component_type = ComponentTypeId::new("engine.static_mesh_renderer");
        let target = DropTarget::InspectorField {
            entity: entity.clone(),
            component_type: component_type.clone(),
            field: "texture".into(),
        };
        if let DropTarget::InspectorField {
            entity: e,
            component_type: ct,
            field,
        } = target
        {
            assert_eq!(e, entity);
            assert_eq!(ct, component_type);
            assert_eq!(field, "texture");
        } else {
            panic!("expected InspectorField");
        }
    }
}
