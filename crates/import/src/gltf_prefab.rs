//! Builds a ready-to-place prefab from an imported glTF/GLB source
//! (ADR 0074, revised by ADR 0075 and ADR 0076).
//!
//! Older model placement resolved a source through a standalone `engine.mesh`
//! component and could omit its materials. This module turns one
//! `GltfImportResult`
//! into the entity subtree the source actually describes: a skeleton per
//! skin, one entity per drawn node, and the materials each node's submeshes
//! declare.
//!
//! The result is import output, not authoring data: it lives under
//! `.engine/` and every successful reimport regenerates it (ADR 0075).

use crate::model_import::GltfImportResult;
use crate::scene_bridge::{
    BUILTIN_WHITE_MATERIAL_ASSET_ID, SKINNED_MESH_RENDERER_COMPONENT, SKINNED_MODEL_COMPONENT,
    STATIC_MESH_RENDERER_COMPONENT, TRANSFORM_COMPONENT,
};
use engine_authoring::id::{AssetId, ComponentTypeId, EntityId};
use engine_authoring::prefab::PrefabAsset;
use engine_authoring::value::Value;
use engine_authoring::AuthoringEntity;
use glam::{EulerRot, Quat, Vec3};
use std::collections::BTreeMap;

/// Builds the prefab describing `imported`, or `None` when the source draws
/// nothing.
///
/// `source` is the manifest ID of the glTF/GLB document; sub-asset IDs
/// already carried by `imported` are written into the component values, so
/// the generated prefab is only valid once the same import catalog is stored
/// in the manifest.
///
/// `name` names the root entity and should be the source file stem.
///
/// A source with no meshes has nothing to place, so it yields `None` instead
/// of an empty prefab that would litter the asset browser.
pub fn build_gltf_prefab(
    _source: &AssetId,
    imported: &GltfImportResult,
    name: &str,
) -> Option<PrefabAsset> {
    if imported.meshes.is_empty() {
        return None;
    }

    let root_id = EntityId::generate();
    let mut entities: BTreeMap<EntityId, AuthoringEntity> = BTreeMap::new();
    let mut root = AuthoringEntity::new(root_id.clone(), name);
    // The generated root is the model instance's author-facing placement
    // handle. Giving it an authored transform lets the Scene View gizmo move,
    // rotate, and scale the complete imported subtree as one object.
    root.components.insert(
        ComponentTypeId::new(TRANSFORM_COMPONENT),
        identity_transform_value(),
    );
    entities.insert(root_id.clone(), root);

    let model_ids = build_skinned_models(imported, &root_id, &mut entities);
    build_mesh_nodes(imported, &root_id, &model_ids, &mut entities);

    PrefabAsset::from_selection(entities.iter()).ok()
}

/// Creates one Skinned Model entity per skeleton asset in the source
/// (ADR 0087 5).
///
/// Returns the model entity that owns each skin, indexed the same way as
/// [`GltfImportResult::skins`]; skins sharing a skeleton share one model, so
/// body, clothes, and hair end up over one rig.
///
/// A source with exactly one skeleton puts the model on the prefab root, so
/// an ordinary character is one entity in the Hierarchy. Additional skeletons
/// each get their own child entity, because one entity carries one rig.
fn build_skinned_models(
    imported: &GltfImportResult,
    root_id: &EntityId,
    entities: &mut BTreeMap<EntityId, AuthoringEntity>,
) -> Vec<EntityId> {
    let mut skeletons: Vec<&crate::model_import::GltfSkinData> = Vec::new();
    for skin in &imported.skins {
        if !skeletons
            .iter()
            .any(|existing| existing.skeleton.id == skin.skeleton.id)
        {
            skeletons.push(skin);
        }
    }
    let on_root = skeletons.len() == 1;

    let mut model_by_skeleton: Vec<(AssetId, EntityId)> = Vec::new();
    for skin in &skeletons {
        let model_id = if on_root {
            root_id.clone()
        } else {
            let model_id = EntityId::generate();
            let mut entity = AuthoringEntity::new(model_id.clone(), &skin.skeleton.name);
            entity.parent = Some(root_id.clone());
            entity.components.insert(
                ComponentTypeId::new(TRANSFORM_COMPONENT),
                identity_transform_value(),
            );
            entities.insert(model_id.clone(), entity);
            model_id
        };
        entities
            .get_mut(&model_id)
            .expect("model entity was just inserted or is the root")
            .components
            .insert(
                ComponentTypeId::new(SKINNED_MODEL_COMPONENT),
                // The authoring value names the catalog entry an author can
                // pick, which is the per-skin derived Skeleton sub-asset ID.
                // `skin.skeleton.id` may instead be an ID adopted from
                // another source by the dedupe rule (ADR 0077 4) and is not
                // in this source's catalog.
                skinned_model_value(&skin.skeleton_id),
            );
        model_by_skeleton.push((skin.skeleton.id.clone(), model_id));
    }

    imported
        .skins
        .iter()
        .map(|skin| {
            model_by_skeleton
                .iter()
                .find(|(skeleton, _)| skeleton == &skin.skeleton.id)
                .map(|(_, model)| model.clone())
                .expect("every skin's skeleton has a model entity")
        })
        .collect()
}

/// Creates one entity per node that instantiates a mesh.
///
/// A mesh with several primitives stays one entity and carries material slots
/// inside its unified renderer, one entry per submesh (ADR 0076 and ADR 0083).
///
fn build_mesh_nodes(
    imported: &GltfImportResult,
    root_id: &EntityId,
    model_ids: &[EntityId],
    entities: &mut BTreeMap<EntityId, AuthoringEntity>,
) {
    // Joint nodes are spawned at runtime by the Skinned Model, so only nodes
    // that draw something get an authoring entity. Their transforms are
    // accumulated to the prefab root to keep placement identical without
    // recreating intermediate nodes that hold nothing.
    for (node_index, binding) in imported.node_bindings.iter().enumerate() {
        let Some(gltf_mesh_index) = binding.gltf_mesh_index else {
            continue;
        };
        let Some(mesh) = imported
            .meshes
            .iter()
            .find(|mesh| mesh.source_index == gltf_mesh_index)
        else {
            continue;
        };
        let model = binding
            .skin_index
            .and_then(|index| model_ids.get(index).cloned());

        let node_id = EntityId::generate();
        let node_name = imported
            .nodes
            .get(node_index)
            .map(|node| node.name.as_str())
            .unwrap_or("node");
        let mut node = AuthoringEntity::new(node_id.clone(), node_name);
        // A render part is a child of the model that owns it, so moving the
        // model moves everything it draws (ADR 0087 2).
        node.parent = Some(model.clone().unwrap_or_else(|| root_id.clone()));
        // A skinned draw's world position is already fully determined by the
        // joint palette (`joint_world * inverse_bind` per joint, matching
        // `ufbx_skin_cluster::geometry_to_world`'s documented
        // `bone->node_to_world * geometry_to_bone`, and glTF's identical
        // skinning contract). The mesh-owning node's own local transform is
        // not part of that computation, so baking it into `engine.transform`
        // here would apply it a second time on top of the skin. It is
        // usually identity for glTF sources (authoring tools already treat
        // it as ignored), which is why this stayed unnoticed; FBX sources
        // routinely leave a real scale on that node (unit conversion, or a
        // Maya "segment scale compensate" residue), where the double
        // application collapses the whole mesh toward a point.
        let transform = if model.is_some() {
            identity_transform_value()
        } else {
            transform_value(imported, node_index)
        };
        node.components
            .insert(ComponentTypeId::new(TRANSFORM_COMPONENT), transform);
        insert_drawn_components(&mut node, &mesh.id, &mesh.materials, model.as_ref());
        entities.insert(node_id, node);
    }
}

/// The difference between a placed Skinned Model's render parts and the
/// meshes its source currently declares (ADR 0087 §5).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ModelPartSync {
    /// Skinned meshes of the source that no render part draws, paired with
    /// the materials each declares, in source order.
    pub missing: Vec<(AssetId, Vec<Option<AssetId>>)>,
    /// Render parts whose mesh the source no longer declares.
    ///
    /// Reported, never deleted: an author may have retargeted the part at a
    /// mesh of their own, and losing their work to a background reimport is
    /// exactly what ADR 0087 §5 refuses to do.
    pub stale: Vec<EntityId>,
}

impl ModelPartSync {
    /// Whether the placed model already matches its source.
    pub fn is_in_sync(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty()
    }
}

/// Compares one model's render parts against the skinned meshes `imported`
/// declares for `skeleton`.
///
/// `skeleton` is the authored Skeleton sub-asset reference, which names a
/// catalog entry; it is resolved to the rig it identifies so that every skin
/// sharing that rig contributes, not just the one skin the reference was
/// derived from.
///
/// `parts` maps each existing render part to the mesh it draws. Meshes bound
/// to a different rig in the same source belong to a different model and are
/// ignored.
pub fn model_part_sync(
    imported: &GltfImportResult,
    skeleton: &AssetId,
    parts: &[(EntityId, AssetId)],
) -> ModelPartSync {
    let Some(rig) = imported
        .skins
        .iter()
        .find(|skin| &skin.skeleton_id == skeleton)
        .map(|skin| skin.skeleton.id.clone())
    else {
        return ModelPartSync::default();
    };
    let expected: Vec<&crate::model_import::GltfMeshData> = imported
        .meshes
        .iter()
        .filter(|mesh| {
            mesh.skin_index
                .and_then(|index| imported.skins.get(index))
                .is_some_and(|skin| skin.skeleton.id == rig)
        })
        .collect();
    ModelPartSync {
        missing: expected
            .iter()
            .filter(|mesh| !parts.iter().any(|(_, drawn)| drawn == &mesh.id))
            .map(|mesh| (mesh.id.clone(), mesh.materials.clone()))
            .collect(),
        stale: parts
            .iter()
            .filter(|(_, drawn)| !expected.iter().any(|mesh| &mesh.id == drawn))
            .map(|(part, _)| part.clone())
            .collect(),
    }
}

/// Builds the authoring value for one skinned render part.
///
/// Shared by prefab generation and by Resync Model Parts so a part added
/// later is indistinguishable from one created at placement.
pub fn skinned_render_part_value(
    mesh: &AssetId,
    materials: &[Option<AssetId>],
    model: &EntityId,
) -> Value {
    let mut entity = AuthoringEntity::new(EntityId::generate(), "part");
    insert_drawn_components(&mut entity, mesh, materials, Some(model));
    entity
        .components
        .remove(&ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT))
        .expect("a skinned draw always inserts the skinned renderer")
}

/// Adds the one unified renderer component a drawn mesh node needs.
fn insert_drawn_components(
    entity: &mut AuthoringEntity,
    mesh: &AssetId,
    materials: &[Option<AssetId>],
    model: Option<&EntityId>,
) {
    let white = builtin_white_material_id();
    let base_material = materials
        .first()
        .and_then(Clone::clone)
        .unwrap_or_else(|| white.clone());
    let material_slots = if materials.len() > 1 {
        materials
            .iter()
            .map(|material| Value::AssetRef(material.clone().unwrap_or_else(|| white.clone())))
            .collect()
    } else {
        Vec::new()
    };
    let mut fields = BTreeMap::from([
        ("mesh".to_owned(), Value::AssetRef(mesh.clone())),
        ("material".to_owned(), Value::AssetRef(base_material)),
        ("material_slots".to_owned(), Value::Array(material_slots)),
    ]);
    let component_type = if let Some(model) = model {
        fields.insert("model".to_owned(), Value::EntityRef(model.clone()));
        SKINNED_MESH_RENDERER_COMPONENT
    } else {
        STATIC_MESH_RENDERER_COMPONENT
    };
    entity
        .components
        .insert(ComponentTypeId::new(component_type), Value::Object(fields));
}

/// Returns the stable built-in white material used for unmaterialed imported
/// surfaces without manufacturing a project manifest entry.
fn builtin_white_material_id() -> AssetId {
    AssetId::from_stable_id(engine_authoring::StableId::new(
        BUILTIN_WHITE_MATERIAL_ASSET_ID,
    ))
    .expect("built-in white material ID must be valid")
}

/// An identity `engine.transform` value.
///
/// Used for skinned mesh nodes, whose world placement comes entirely from
/// the joint palette rather than their own local transform (see the call
/// site in [`build_mesh_nodes`]).
fn identity_transform_value() -> Value {
    Value::Object(BTreeMap::from([
        ("x".to_owned(), Value::F64(0.0)),
        ("y".to_owned(), Value::F64(0.0)),
        ("z".to_owned(), Value::F64(0.0)),
        ("rotation_x_degrees".to_owned(), Value::F64(0.0)),
        ("rotation_y_degrees".to_owned(), Value::F64(0.0)),
        ("rotation_z_degrees".to_owned(), Value::F64(0.0)),
        ("scale_x".to_owned(), Value::F64(1.0)),
        ("scale_y".to_owned(), Value::F64(1.0)),
        ("scale_z".to_owned(), Value::F64(1.0)),
    ]))
}

/// Accumulates a node's transform up to the document root.
///
/// Intermediate nodes that draw nothing get no authoring entity, so their
/// contribution has to be folded into the entity that does exist.
fn transform_value(imported: &GltfImportResult, node_index: usize) -> Value {
    let mut translation = Vec3::ZERO;
    let mut rotation = Quat::IDENTITY;
    let mut scale = Vec3::ONE;
    let mut current = Some(node_index);
    // The walk is bounded so a corrupt parent link with a cycle cannot hang.
    let mut steps = 0;
    while let Some(index) = current {
        let Some(node) = imported.nodes.get(index) else {
            break;
        };
        if steps > imported.nodes.len() {
            break;
        }
        steps += 1;
        translation = node.translation + node.rotation * (node.scale * translation);
        rotation = node.rotation * rotation;
        scale *= node.scale;
        current = node.parent;
    }

    let (rotation_x, rotation_y, rotation_z) = rotation.to_euler(EulerRot::XYZ);
    Value::Object(BTreeMap::from([
        ("x".to_owned(), Value::F64(translation.x as f64)),
        ("y".to_owned(), Value::F64(translation.y as f64)),
        ("z".to_owned(), Value::F64(translation.z as f64)),
        (
            "rotation_x_degrees".to_owned(),
            Value::F64(rotation_x.to_degrees() as f64),
        ),
        (
            "rotation_y_degrees".to_owned(),
            Value::F64(rotation_y.to_degrees() as f64),
        ),
        (
            "rotation_z_degrees".to_owned(),
            Value::F64(rotation_z.to_degrees() as f64),
        ),
        ("scale_x".to_owned(), Value::F64(scale.x as f64)),
        ("scale_y".to_owned(), Value::F64(scale.y as f64)),
        ("scale_z".to_owned(), Value::F64(scale.z as f64)),
    ]))
}

/// Builds the authoring value for one Skinned Model.
///
/// No Animation Controller is generated: an imported model creates its rig
/// and rests in its bind pose until an author adds playback (ADR 0084).
fn skinned_model_value(skeleton: &AssetId) -> Value {
    Value::Object(BTreeMap::from([(
        "skeleton".to_owned(),
        Value::AssetRef(skeleton.clone()),
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_bridge::ANIMATION_CONTROLLER_COMPONENT;

    fn imported_fixture(source: &AssetId, bytes: &[u8], name: &str) -> GltfImportResult {
        let directory = tempfile::tempdir().expect("temporary source directory");
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).expect("source fixture");
        crate::gltf_import::import_gltf_path(source, &path, &[]).expect("fixture import")
    }

    fn component<'a>(
        prefab: &'a PrefabAsset,
        component_type: &str,
    ) -> Vec<(&'a EntityId, &'a Value)> {
        let wanted = ComponentTypeId::new(component_type);
        prefab
            .entities
            .iter()
            .filter_map(|(id, entity)| entity.components.get(&wanted).map(|value| (id, value)))
            .collect()
    }

    #[test]
    fn skinned_source_produces_renderers_that_reference_one_model() {
        let source = AssetId::generate();
        let imported = imported_fixture(
            &source,
            crate::gltf_import::tests::SKINNED_GLTF.as_bytes(),
            "character.gltf",
        );

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        let models = component(&prefab, SKINNED_MODEL_COMPONENT);
        assert_eq!(models.len(), 1);
        let (model_entity, value) = &models[0];
        let Value::Object(fields) = value else {
            panic!("skinned model must be an object");
        };
        assert_eq!(
            fields.get("skeleton"),
            Some(&Value::AssetRef(imported.skins[0].skeleton_id.clone())),
            "the model owns a Skeleton sub-asset, not a Skin"
        );

        let skinned = component(&prefab, SKINNED_MESH_RENDERER_COMPONENT);
        assert!(!skinned.is_empty());
        for (part_id, value) in &skinned {
            let Value::Object(fields) = value else {
                panic!("skinned mesh must be an object");
            };
            assert_eq!(
                fields.get("model"),
                Some(&Value::EntityRef((*model_entity).clone())),
                "every generated renderer must explicitly select its Skinned Model"
            );
            assert_eq!(
                prefab.entities[*part_id].parent.as_ref(),
                Some(*model_entity),
                "a render part is a child of the model that owns it"
            );
        }
    }

    #[test]
    fn a_single_skeleton_model_lives_on_the_prefab_root() {
        let source = AssetId::generate();
        let imported = imported_fixture(
            &source,
            crate::gltf_import::tests::SKINNED_GLTF.as_bytes(),
            "character.gltf",
        );

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        let models = component(&prefab, SKINNED_MODEL_COMPONENT);
        assert_eq!(models.len(), 1);
        assert!(
            prefab.entities[models[0].0].parent.is_none(),
            "an ordinary character is one entity in the Hierarchy"
        );
    }

    #[test]
    fn generated_model_root_has_an_identity_transform() {
        let source = AssetId::generate();
        let imported = imported_fixture(
            &source,
            crate::gltf_import::tests::SKINNED_GLTF.as_bytes(),
            "character.gltf",
        );

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");
        let root = prefab
            .entities
            .get(&prefab.root)
            .expect("generated prefab root must exist");

        assert_eq!(
            root.components
                .get(&ComponentTypeId::new(TRANSFORM_COMPONENT)),
            Some(&identity_transform_value()),
            "the model root must be a movable authoring handle"
        );
    }

    #[test]
    fn every_primitive_keeps_the_material_it_declares() {
        let source = AssetId::generate();
        let imported = imported_fixture(
            &source,
            &crate::gltf_import::tests::three_clip_character_glb(),
            "character.glb",
        );

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        let expected: Vec<_> = imported
            .meshes
            .iter()
            .filter_map(|mesh| mesh.materials.first().cloned().flatten())
            .map(Value::AssetRef)
            .collect();
        assert!(!expected.is_empty(), "fixture must declare a material");
        let materials: Vec<_> = component(&prefab, SKINNED_MESH_RENDERER_COMPONENT)
            .into_iter()
            .filter_map(|(_, value)| match value {
                Value::Object(fields) => fields.get("material").cloned(),
                _ => None,
            })
            .collect();
        assert_eq!(materials, expected);
    }

    #[test]
    fn a_source_with_clips_stays_in_rest_pose_until_a_graph_is_assigned() {
        let source = AssetId::generate();
        let imported = imported_fixture(
            &source,
            &crate::gltf_import::tests::three_clip_character_glb(),
            "character.glb",
        );

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        assert!(
            component(&prefab, ANIMATION_CONTROLLER_COMPONENT).is_empty(),
            "an imported model creates its rig but no playback state (ADR 0084)"
        );
        assert_eq!(component(&prefab, SKINNED_MODEL_COMPONENT).len(), 1);
    }

    #[test]
    fn a_skin_without_animation_still_gets_its_rig() {
        let source = AssetId::generate();
        let mut imported = imported_fixture(
            &source,
            crate::gltf_import::tests::SKINNED_GLTF.as_bytes(),
            "character.gltf",
        );
        imported.animations.clear();

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        let models = component(&prefab, SKINNED_MODEL_COMPONENT);
        assert_eq!(models.len(), 1);
        let Value::Object(fields) = models[0].1 else {
            panic!("skinned model must be an object");
        };
        assert_eq!(
            fields.get("skeleton"),
            Some(&Value::AssetRef(imported.skins[0].skeleton_id.clone()))
        );
        assert!(
            component(&prefab, ANIMATION_CONTROLLER_COMPONENT).is_empty(),
            "a model with no clips instantiates its rig without inventing playback"
        );
    }

    #[test]
    fn a_skinned_mesh_node_gets_an_identity_transform_even_when_its_source_node_does_not() {
        // Same shape as `gltf_import::tests::SKINNED_GLTF`, but the mesh
        // node ("character") also declares a non-identity scale — the way a
        // direct FBX import routinely does (unit conversion, or a Maya
        // "segment scale compensate" residue left on the mesh-owning node).
        // A skinned draw's world position already comes entirely from the
        // joint palette, so baking this node's own scale into
        // `engine.transform` would apply it a second time.
        let source = AssetId::generate();
        let scaled_gltf = crate::gltf_import::tests::SKINNED_GLTF
            .replacen(r#"{"name": "character", "mesh": 0, "skin": 0}"#, r#"{"name": "character", "mesh": 0, "skin": 0, "scale": [0.01, 0.01, 0.01], "translation": [5.0, 0.0, 0.0]}"#, 1);
        assert_ne!(
            scaled_gltf,
            crate::gltf_import::tests::SKINNED_GLTF,
            "the fixture edit must actually add a node transform"
        );
        let imported = imported_fixture(&source, scaled_gltf.as_bytes(), "character.gltf");

        let prefab = build_gltf_prefab(&source, &imported, "character").expect("prefab");

        let skinned_meshes = component(&prefab, SKINNED_MESH_RENDERER_COMPONENT);
        assert_eq!(skinned_meshes.len(), 1, "the fixture has one skinned mesh");
        let mesh_entity = prefab
            .entities
            .get(skinned_meshes[0].0)
            .expect("the skinned mesh entity must exist");
        assert_eq!(
            mesh_entity
                .components
                .get(&ComponentTypeId::new(TRANSFORM_COMPONENT)),
            Some(&identity_transform_value()),
            "a skinned mesh node's own transform must stay identity"
        );
    }

    #[test]
    fn a_multi_primitive_mesh_becomes_one_entity_with_material_slots() {
        let source = AssetId::generate();
        let directory = tempfile::tempdir().expect("temporary source directory");
        let path = directory.path().join("prop.gltf");
        // Two primitives of one mesh, each declaring its own material.
        let mut positions = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            positions.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(directory.path().join("prop.bin"), positions).expect("buffer fixture");
        std::fs::write(
            &path,
            r#"{
                "asset":{"version":"2.0"},
                "buffers":[{"uri":"prop.bin","byteLength":36}],
                "bufferViews":[{"buffer":0,"byteLength":36}],
                "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
                "materials":[{"name":"body"},{"name":"trim"}],
                "meshes":[{"name":"prop","primitives":[
                    {"attributes":{"POSITION":0},"material":0},
                    {"attributes":{"POSITION":0},"material":1}
                ]}],
                "nodes":[{"name":"prop","mesh":0}],
                "scenes":[{"nodes":[0]}]
            }"#,
        )
        .expect("source fixture");
        let imported =
            crate::gltf_import::import_gltf_path(&source, &path, &[]).expect("fixture import");

        assert_eq!(imported.meshes.len(), 1, "primitives merge into one mesh");
        assert_eq!(imported.meshes[0].mesh.submeshes.len(), 2);

        let prefab = build_gltf_prefab(&source, &imported, "prop").expect("prefab");

        let renderers = component(&prefab, STATIC_MESH_RENDERER_COMPONENT);
        assert_eq!(renderers.len(), 1, "the mesh node stays a single entity");
        let Value::Object(fields) = renderers[0].1 else {
            panic!("static mesh renderer must be an object");
        };
        assert_eq!(
            fields.get("material_slots"),
            Some(&Value::Array(
                imported.meshes[0]
                    .materials
                    .iter()
                    .map(|material| Value::AssetRef(
                        material.clone().expect("fixture declares both materials")
                    ))
                    .collect()
            ))
        );
    }

    #[test]
    fn a_source_without_meshes_produces_no_prefab() {
        let source = AssetId::generate();
        let imported = GltfImportResult {
            meshes: Vec::new(),
            nodes: Vec::new(),
            node_bindings: Vec::new(),
            skins: Vec::new(),
            animations: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            rigid_body_rig: None,
            diagnostics: Vec::new(),
            skeleton_records: Vec::new(),
        };

        assert!(build_gltf_prefab(&source, &imported, "empty").is_none());
    }
}
