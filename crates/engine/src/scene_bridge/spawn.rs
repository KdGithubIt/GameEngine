//! Per-component spawn callbacks that convert authoring values to runtime components.

use super::*;

pub(crate) fn spawn_transform_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(TRANSFORM_COMPONENT);
    let authored = extract_transform_value(context.authoring_entity, &component_type, value)?;
    let transform = context
        .world
        .get_component_mut::<Transform>(entity)
        .expect("bridge entities must be spawned with Transform before component dispatch");
    *transform = authored;
    Ok(())
}

pub(crate) fn spawn_player_marker_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(PLAYER_MARKER_COMPONENT);
    validate_player_marker_value(context.authoring_entity, &component_type, value)?;
    context.world.add_component(entity, PlayerMarker)?;
    Ok(())
}

/// Expands one unified static-mesh renderer authoring value into the runtime
/// mesh, base material, and optional per-submesh material components.
pub(crate) fn spawn_static_mesh_renderer_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(STATIC_MESH_RENDERER_COMPONENT);
    const EXPECTED: &str = "an object with mesh, material, and material_slots fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    ensure_renderer_component_is_exclusive(context.authoring_entity, &component_type)?;

    let mesh_asset = fields.asset_ref("mesh")?.clone();
    let material_asset = fields.asset_ref("material")?.clone();
    let slot_assets = material_slot_asset_ids(&fields, "material_slots")?;

    let mesh = resolve_mesh_handle(&mesh_asset, context);
    let material = resolve_material_value(&material_asset, context);
    let slots = slot_assets
        .iter()
        .map(|asset| resolve_material_value(asset, context))
        .collect();

    install_mesh_handle(entity, mesh, context.world)?;
    install_material(entity, material, context.world)?;
    install_material_slots(entity, slots, context.world)?;
    Ok(())
}

/// Resolves every authored LOD mesh through the same conversion-local cache
/// used by the runtime mesh handle, then attaches the runtime selector. The
/// first LOD mesh is also installed as the initial render handle when no
/// renderer has selected a mesh yet; the frame system may replace it after
/// camera distance is known.
pub(crate) fn spawn_lod_group_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(LOD_GROUP_COMPONENT);
    const EXPECTED: &str =
        "an object with a non-empty `levels` array of increasing positive distance/mesh rows";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let Some(Value::Array(authored_levels)) = fields.get("levels") else {
        return Err(fields.invalid(EXPECTED).into());
    };
    if authored_levels.is_empty() {
        return Err(fields.invalid(EXPECTED).into());
    }

    let mut levels = Vec::with_capacity(authored_levels.len());
    let mut previous_distance = 0.0_f32;
    for level in authored_levels {
        let Value::Object(level_object) = level else {
            return Err(fields.invalid(EXPECTED).into());
        };
        let distance = level_object
            .get("distance")
            .and_then(|value| match value {
                Value::F64(value) => Some(*value as f32),
                Value::I64(value) => Some(*value as f32),
                Value::U64(value) => Some(*value as f32),
                _ => None,
            })
            .filter(|distance| distance.is_finite() && *distance > previous_distance)
            .ok_or_else(|| fields.invalid(EXPECTED))?;
        let Some(Value::AssetRef(asset)) = level_object.get("mesh") else {
            return Err(fields.invalid(EXPECTED).into());
        };
        let mesh = resolve_mesh_handle(asset, context);
        levels.push(LodLevel {
            max_distance: distance,
            mesh,
        });
        previous_distance = distance;
    }

    let initial_mesh = levels[0].mesh;
    context.world.add_component(entity, LodGroup { levels })?;
    install_mesh_handle(entity, initial_mesh, context.world)?;
    Ok(())
}

/// Creates the character rig owned by one `engine.skinned_model` (ADR 0087 §1).
///
/// Idempotent: the Animation Controller on the same entity ensures the rig
/// exists before its own playback setup, because components dispatch in
/// `ComponentTypeId` order and `engine.animation_controller` sorts first. A
/// second call for the same entity therefore finds the rig already present
/// and does nothing.
pub(crate) fn spawn_skinned_model_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(SKINNED_MODEL_COMPONENT);
    const EXPECTED: &str = "an object with a skeleton AssetRef";
    if context.world.get_component::<Skeleton>(entity).is_some() {
        return Ok(());
    }
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let Some(skeleton_asset) = fields.assignable_asset_ref("skeleton")?.cloned() else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "skeleton",
            ));
        return Ok(());
    };
    let Some(source) = context
        .manifest
        .imported_sub_asset(&skeleton_asset)
        .and_then(|(source, _, sub_asset)| {
            (sub_asset.kind == ImportedSubAssetKind::Skeleton).then(|| source.clone())
        })
    else {
        return Err(fields
            .invalid("a skeleton reference to an imported Skeleton sub-asset")
            .into());
    };
    spawn_rig_from_source(
        entity,
        &source,
        |skin| skin.skeleton_id == skeleton_asset,
        context,
    )
}

fn spawn_rig_from_source(
    entity: Entity,
    source: &AssetId,
    selector: impl Fn(&crate::model_import::GltfSkinData) -> bool,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let imported = match import_source_cached(source, context) {
        Ok(imported) => imported,
        Err(error) => {
            push_skeleton_diagnostic(source, error, context);
            return Ok(());
        }
    };
    context.asset_diagnostics.extend(
        imported.diagnostics.iter().cloned().map(|diagnostic| {
            diagnostic.with_target(DiagnosticTarget::Asset { id: source.clone() })
        }),
    );
    let Some(skin) = imported.skins.iter().find(|skin| selector(skin)).cloned() else {
        push_skeleton_diagnostic(
            source,
            "the referenced skeleton is not part of this source; reimport it".to_owned(),
            context,
        );
        return Ok(());
    };

    let before = context.world.entities().collect::<HashSet<_>>();
    let rig = match spawn_rig(context.world, &skin.skeleton) {
        Ok(rig) => rig,
        Err(RigSpawnError::World(error)) => return Err(error.into()),
        Err(error @ RigSpawnError::BoneParentOutOfOrder { .. }) => {
            push_skeleton_diagnostic(source, error.to_string(), context);
            return Ok(());
        }
    };
    context.asset_state.auxiliary_entities.extend(
        context
            .world
            .entities()
            .filter(|spawned| !before.contains(spawned)),
    );
    if let Some(registry) = context.world.get_resource_mut::<SkeletonAssetRegistry>() {
        registry.insert(skin.skeleton.clone());
        context
            .asset_state
            .added_skeleton_asset_ids
            .push(skin.skeleton.id.clone());
    }
    context.world.add_component(entity, rig.skeleton)?;
    context.world.add_component(entity, rig.pose)?;
    Ok(())
}

pub(crate) fn spawn_skinned_mesh_renderer_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT);
    const EXPECTED: &str = "an object with mesh, model, material, and material_slots fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    ensure_renderer_component_is_exclusive(context.authoring_entity, &component_type)?;

    let Some(mesh_asset) = fields.assignable_asset_ref("mesh")?.cloned() else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "mesh",
            ));
        return Ok(());
    };
    let material_asset = fields.asset_ref("material")?.clone();
    let slot_assets = material_slot_asset_ids(&fields, "material_slots")?;

    let handle = resolve_mesh_handle(&mesh_asset, context);
    let is_skinned = mesh_has_skinning_attributes(handle, context);
    if !is_skinned {
        push_mesh_without_skinning_diagnostic(&mesh_asset, context);
    }

    let material = resolve_material_value(&material_asset, context);
    let slots = slot_assets
        .iter()
        .map(|asset| resolve_material_value(asset, context))
        .collect();

    let morphs_material = install_morph_binding(entity, &mesh_asset, handle, context)?;
    if !morphs_material.bound {
        install_mesh_handle(entity, handle, context.world)?;
    }
    if morphs_material.changes_material {
        context
            .world
            .add_component(entity, MorphBaseColor(material.color))?;
    }
    install_material(entity, material, context.world)?;
    install_material_slots(entity, slots, context.world)?;
    if let Some(rig) = resolve_rig_entity(&fields, &component_type, context)? {
        let binding = resolve_skin_binding(&mesh_asset, is_skinned, context);
        context
            .world
            .add_component(entity, binding.into_component(rig))?;
        context
            .world
            .add_component(entity, JointPalette::default())?;
    }
    Ok(())
}

fn import_source_cached(
    source: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Result<Arc<crate::model_import::GltfImportResult>, String> {
    let source_path = context
        .manifest
        .get(source)
        .filter(|entry| {
            crate::components::asset_path_matches_kind(
                crate::components::AssetKind::GltfSource,
                Path::new(&entry.path),
            )
        })
        .map(|entry| {
            context
                .asset_root
                .unwrap_or_else(|| Path::new("."))
                .join(&entry.path)
        })
        .ok_or_else(|| "source is not a registered glTF/GLB asset".to_owned())?;
    let existing_skeletons = context
        .manifest
        .get(source)
        .map(|entry| entry.import_settings.skeleton_records.clone())
        .unwrap_or_default();
    let contact_bones = context
        .manifest
        .get(source)
        .map(|entry| entry.import_settings.contact_bones.clone())
        .unwrap_or_default();
    import_gltf_cached(
        source,
        &source_path,
        &existing_skeletons,
        &contact_bones,
        context.asset_state,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn spawn_rigid_body_physics_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(RIGID_BODY_PHYSICS_COMPONENT);
    const EXPECTED: &str = "an object with a rig AssetRef field";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let Some(rig_asset) = fields.assignable_asset_ref("rig")?.cloned() else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "rig",
            ));
        return Ok(());
    };

    register_rigid_body_rig(&rig_asset, context);
    context
        .world
        .add_component(entity, RigidBodyPhysics::new(rig_asset))?;
    Ok(())
}

fn register_rigid_body_rig(rig_asset: &AssetId, context: &mut SpawnContext<'_>) {
    let Some((source_id, _, sub_asset)) = context.manifest.imported_sub_asset(rig_asset) else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.secondary_motion_rig_unresolved",
            format!(
                "secondary-motion rig `{}` is not an imported sub-asset of any registered model source",
                rig_asset.as_str()
            ),
        ));
        return;
    };
    if sub_asset.kind != ImportedSubAssetKind::SecondaryMotionRig {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.secondary_motion_rig_unresolved",
            format!(
                "asset `{}` is {:?}, not a secondary-motion rig",
                rig_asset.as_str(),
                sub_asset.kind
            ),
        ));
        return;
    }
    let source_id = source_id.clone();
    let imported = match import_source_cached(&source_id, context) {
        Ok(imported) => imported,
        Err(error) => {
            context.asset_diagnostics.push(Diagnostic::warning(
                "scene_bridge.secondary_motion_rig_unresolved",
                format!(
                    "could not import the model source owning secondary-motion rig `{}`: {error}",
                    rig_asset.as_str()
                ),
            ));
            return;
        }
    };
    let Some(rig) = imported
        .rigid_body_rig
        .as_ref()
        .filter(|rig| &rig.id == rig_asset)
    else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.secondary_motion_rig_unresolved",
            format!(
                "the model source no longer contains secondary-motion rig `{}`; reimport it",
                rig_asset.as_str()
            ),
        ));
        return;
    };
    let registry = context
        .world
        .get_resource_mut::<RigidBodyRigRegistry>()
        .is_none();
    if registry {
        context.world.insert_resource(RigidBodyRigRegistry::new());
    }
    if let Some(registry) = context.world.get_resource_mut::<RigidBodyRigRegistry>() {
        registry.insert(rig.clone());
        context
            .asset_state
            .added_rigid_body_rig_ids
            .push(rig.id.clone());
    }
}

pub(crate) fn spawn_bone_attachment_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(BONE_ATTACHMENT_COMPONENT);
    const EXPECTED: &str = "an object with a rig EntityRef and a bone BoneId";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let rig_authoring_id = match fields.get("rig") {
        Some(Value::EntityRef(id)) => id.clone(),
        Some(Value::Null) | None => {
            context
                .asset_diagnostics
                .push(component_inactive_diagnostic(
                    context.authoring_entity,
                    &component_type,
                    "rig",
                ));
            return Ok(());
        }
        _ => return Err(fields.invalid(EXPECTED).into()),
    };
    let bone = match fields.get("bone") {
        Some(Value::I64(bone)) if *bone >= 0 => BoneId(*bone as u32),
        Some(Value::I64(_)) | None => {
            context
                .asset_diagnostics
                .push(component_inactive_diagnostic(
                    context.authoring_entity,
                    &component_type,
                    "bone",
                ));
            return Ok(());
        }
        _ => return Err(fields.invalid(EXPECTED).into()),
    };
    let Some(rig) = context.entity_map.get(&rig_authoring_id).copied() else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.bone_attachment_unresolved_rig",
            format!(
                "bone attachment on entity `{}` references unknown entity `{}`",
                context.authoring_entity.id.as_str(),
                rig_authoring_id.as_str()
            ),
        ));
        return Ok(());
    };
    context
        .world
        .add_component(entity, BoneAttachment { rig, bone })?;
    Ok(())
}

/// Ensures the Animation Controller uses the rig owned by the same entity's
/// current `engine.skinned_model` component.
fn ensure_rig_for_controller(
    entity: Entity,
    component_type: &ComponentTypeId,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    if context.world.get_component::<Skeleton>(entity).is_some() {
        return Ok(());
    }
    let model_type = ComponentTypeId::new(SKINNED_MODEL_COMPONENT);
    if let Some(model) = context
        .authoring_entity
        .components
        .get(&model_type)
        .cloned()
    {
        return spawn_skinned_model_component(entity, &model, context);
    }
    context.asset_diagnostics.push(
        Diagnostic::error(
            "scene.component_dependency_missing",
            format!(
                "`{}` on entity `{}` needs an `{SKINNED_MODEL_COMPONENT}` on the same entity to own its rig",
                component_type.as_str(),
                context.authoring_entity.id.as_str()
            ),
        )
        .with_target(DiagnosticTarget::Component {
            entity: context.authoring_entity.id.clone(),
            component_type: component_type.clone(),
        }),
    );
    Ok(())
}

fn resolve_rig_entity(
    fields: &ComponentFields<'_>,
    component_type: &ComponentTypeId,
    context: &mut SpawnContext<'_>,
) -> Result<Option<Entity>, ComponentSpawnError> {
    let authored = match fields.get("model") {
        Some(Value::EntityRef(id)) => Some(id.clone()),
        None | Some(Value::Null) => None,
        Some(_) => return Err(fields.invalid("an entity reference or null").into()),
    };
    let Some(model) = authored else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                component_type,
                "model",
            ));
        return Ok(None);
    };
    let Some(rig) = context.entity_map.get(&model).copied() else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.skinned_mesh_unresolved_skeleton",
            format!(
                "skinned renderer on entity `{}` references unknown entity `{}`; mesh skipped",
                context.authoring_entity.id.as_str(),
                model.as_str()
            ),
        ));
        return Ok(None);
    };
    Ok(Some(rig))
}

#[derive(Default)]
struct SkinBinding {
    joint_bones: Vec<BoneId>,
    inverse_bind_matrices: Vec<Mat4>,
    skin: Option<AssetId>,
}

impl SkinBinding {
    fn into_component(self, rig: Entity) -> SkinnedMesh {
        SkinnedMesh {
            rig,
            joint_bones: self.joint_bones,
            inverse_bind_matrices: self.inverse_bind_matrices,
            skin: self.skin,
        }
    }
}

fn resolve_skin_binding(
    mesh_asset: &AssetId,
    is_skinned: bool,
    context: &mut SpawnContext<'_>,
) -> SkinBinding {
    let Some(binding) = lookup_skin_binding(mesh_asset, context) else {
        if is_skinned {
            context.asset_diagnostics.push(
                Diagnostic::warning(
                    "scene_bridge.skin_binding_unresolved",
                    format!(
                        "mesh `{}` has skinning attributes but no resolvable skin; it will render in its bind pose",
                        mesh_asset.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: mesh_asset.clone(),
                }),
            );
        }
        return SkinBinding::default();
    };
    binding
}

fn lookup_skin_binding(
    mesh_asset: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Option<SkinBinding> {
    let source = context
        .manifest
        .imported_sub_asset(mesh_asset)
        .and_then(|(source, _, sub_asset)| {
            (sub_asset.kind == ImportedSubAssetKind::Mesh).then(|| source.clone())
        })?;
    let imported = import_source_cached(&source, context).ok()?;
    let skin_index = imported
        .meshes
        .iter()
        .find(|mesh| &mesh.id == mesh_asset)
        .and_then(|mesh| mesh.skin_index)?;
    let skin = imported.skins.get(skin_index)?;
    Some(SkinBinding {
        joint_bones: skin.joint_bone_ids.clone(),
        inverse_bind_matrices: skin.inverse_bind_matrices.clone(),
        skin: Some(skin.id.clone()),
    })
}

fn mesh_has_skinning_attributes(handle: Handle<Mesh>, context: &SpawnContext<'_>) -> bool {
    context
        .world
        .get_resource::<Assets<Mesh>>()
        .and_then(|meshes| meshes.get(&handle))
        .is_some_and(|mesh| mesh.skinning.is_some())
}

fn push_mesh_without_skinning_diagnostic(mesh_asset: &AssetId, context: &mut SpawnContext<'_>) {
    context.asset_diagnostics.push(
        Diagnostic::warning(
            "asset.mesh_without_skinning",
            format!(
                "mesh `{}` has no joint or weight attributes; it will render in its bind pose",
                mesh_asset.as_str()
            ),
        )
        .with_target(DiagnosticTarget::Asset {
            id: mesh_asset.clone(),
        }),
    );
}

fn push_skeleton_diagnostic(source: &AssetId, reason: String, context: &mut SpawnContext<'_>) {
    context.asset_diagnostics.push(
        Diagnostic::error(
            "asset.skeleton_source_invalid",
            format!(
                "skeleton source `{}` could not be instantiated: {reason}; bound meshes keep their bind pose",
                source.as_str()
            ),
        )
        .with_target(DiagnosticTarget::Asset { id: source.clone() }),
    );
}

fn install_morph_binding(
    entity: Entity,
    mesh_asset: &AssetId,
    handle: Handle<Mesh>,
    context: &mut SpawnContext<'_>,
) -> Result<MorphBinding, ComponentSpawnError> {
    let Some(morphs) = resolve_mesh_morphs(mesh_asset, context) else {
        return Ok(MorphBinding::default());
    };
    let changes_material = morphs
        .iter()
        .any(|morph| !morph.material_offsets.is_empty());
    let Some(mesh) = context
        .world
        .get_resource::<Assets<Mesh>>()
        .and_then(|assets| assets.get(&handle))
        .cloned()
    else {
        return Ok(MorphBinding::default());
    };
    let targets = MorphTargets::new(morphs, crate::morph::rest_positions(&mesh));
    let weights = MorphWeights::for_targets(&targets);

    let _ = context.world.remove_component::<Handle<Mesh>>(entity);
    context.world.add_component(entity, mesh)?;
    context.world.add_component(entity, targets)?;
    context.world.add_component(entity, weights)?;
    context
        .world
        .add_component(entity, MorphDirtyVertices::default())?;
    Ok(MorphBinding {
        bound: true,
        changes_material,
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct MorphBinding {
    bound: bool,
    changes_material: bool,
}

fn resolve_mesh_morphs(
    mesh_asset: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Option<Vec<crate::morph::MorphAsset>> {
    let (source_id, _, sub_asset) = context.manifest.imported_sub_asset(mesh_asset)?;
    if sub_asset.kind != ImportedSubAssetKind::Mesh {
        return None;
    }
    let source_id = source_id.clone();
    let imported = import_source_cached(&source_id, context).ok()?;
    let morphs = imported
        .meshes
        .iter()
        .find(|mesh| &mesh.id == mesh_asset)
        .map(|mesh| mesh.morphs.clone())?;
    (!morphs.is_empty()).then_some(morphs)
}

fn install_mesh_handle(
    entity: Entity,
    handle: Handle<Mesh>,
    world: &mut World,
) -> Result<(), ComponentSpawnError> {
    if let Some(existing) = world.get_component_mut::<Handle<Mesh>>(entity) {
        *existing = handle;
        Ok(())
    } else {
        world.add_component(entity, handle).map_err(Into::into)
    }
}

fn resolve_mesh_handle(asset: &AssetId, context: &mut SpawnContext<'_>) -> Handle<Mesh> {
    if let Some(handle) = context.asset_state.mesh_handles.get(asset) {
        return *handle;
    }
    let (loaded, diag) = load_mesh_asset(
        asset,
        context.asset_root,
        context.manifest,
        context.asset_state,
    );
    if let Some(diagnostic) = diag {
        context.asset_diagnostics.push(diagnostic);
    }
    let handle = context
        .world
        .get_resource_mut::<Assets<Mesh>>()
        .expect("mesh asset store must exist before mesh component dispatch")
        .add(loaded);
    context
        .asset_state
        .runtime_ids
        .insert(asset.clone(), handle.id());
    context
        .asset_state
        .mesh_handles
        .insert(asset.clone(), handle);
    context.asset_state.added_mesh_handles.push(handle);
    handle
}

fn resolve_material_value(asset: &AssetId, context: &mut SpawnContext<'_>) -> Material {
    let handle = if let Some(handle) = context.asset_state.material_handles.get(asset) {
        *handle
    } else {
        let (loaded, diagnostics) = load_material_asset(
            asset,
            context.asset_root,
            context.manifest,
            context.asset_state,
        );
        context.asset_diagnostics.extend(diagnostics);
        let handle = context
            .world
            .get_resource_mut::<Assets<Material>>()
            .expect("material asset store must exist before material component dispatch")
            .add(loaded);
        context
            .asset_state
            .runtime_ids
            .insert(asset.clone(), handle.id());
        context
            .asset_state
            .material_handles
            .insert(asset.clone(), handle);
        context.asset_state.added_material_handles.push(handle);
        handle
    };
    context
        .world
        .get_resource::<Assets<Material>>()
        .and_then(|materials| materials.get(&handle))
        .cloned()
        .expect("prepared material asset must have a runtime value")
}

fn install_material(
    entity: Entity,
    material: Material,
    world: &mut World,
) -> Result<(), ComponentSpawnError> {
    if let Some(existing) = world.get_component_mut::<Material>(entity) {
        *existing = material;
        Ok(())
    } else {
        world.add_component(entity, material).map_err(Into::into)
    }
}

fn install_material_slots(
    entity: Entity,
    materials: Vec<Material>,
    world: &mut World,
) -> Result<(), ComponentSpawnError> {
    if materials.is_empty() {
        return Ok(());
    }
    if let Some(existing) = world.get_component_mut::<MaterialSlots>(entity) {
        existing.materials = materials;
        Ok(())
    } else {
        world
            .add_component(entity, MaterialSlots { materials })
            .map_err(Into::into)
    }
}

fn material_slot_asset_ids(
    fields: &ComponentFields<'_>,
    field: &str,
) -> Result<Vec<AssetId>, SceneBridgeError> {
    let Some(Value::Array(values)) = fields.get(field) else {
        return Err(fields.invalid("an array of material AssetRefs"));
    };
    values
        .iter()
        .map(|value| match value {
            Value::AssetRef(asset) => Ok(asset.clone()),
            _ => Err(fields.invalid("an array containing only material AssetRefs")),
        })
        .collect()
}

fn ensure_renderer_component_is_exclusive(
    entity: &AuthoringEntity,
    active_component: &ComponentTypeId,
) -> Result<(), SceneBridgeError> {
    let renderer_components = [
        STATIC_MESH_RENDERER_COMPONENT,
        SKINNED_MESH_RENDERER_COMPONENT,
    ];
    if renderer_components.iter().any(|candidate| {
        *candidate != active_component.as_str()
            && entity
                .components
                .contains_key(&ComponentTypeId::new(*candidate))
    }) {
        return Err(invalid_component(
            entity,
            active_component,
            "exactly one of Static Mesh Renderer or Skinned Mesh Renderer",
        ));
    }
    Ok(())
}

pub(crate) fn spawn_animation_controller_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(ANIMATION_CONTROLLER_COMPONENT);
    const EXPECTED: &str =
        "an Animation Controller object with animation_set, graph, and playback fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    ensure_rig_for_controller(entity, &component_type, context)?;
    if context.world.get_component::<Skeleton>(entity).is_none() {
        return Ok(());
    }
    if !fields.bool_or("enabled", true)? {
        return Ok(());
    }

    let animation_set_asset = fields.assignable_asset_ref("animation_set")?.cloned();
    let graph_asset = match fields.get("graph") {
        None | Some(Value::Null) => None,
        Some(Value::AssetRef(asset)) => Some(asset.clone()),
        Some(_) => return Err(fields.invalid(EXPECTED).into()),
    };
    match (animation_set_asset.is_some(), graph_asset.is_some()) {
        (true, false) => {
            return Err(fields
                .invalid("an Animation Graph when Animation Set is assigned")
                .into());
        }
        (false, true) => {
            return Err(fields
                .invalid("an Animation Set when Animation Graph is assigned")
                .into());
        }
        _ => {}
    }
    let Some(animation_set_asset) = animation_set_asset else {
        return Ok(());
    };

    let looping = fields.bool_or("looping", true)?;
    let playback_speed = fields.f32_or("playback_speed", 1.0)?;
    let fade_duration = fields.f32_or("fade_duration", 0.2)?;
    if playback_speed < 0.0 || fade_duration < 0.0 {
        return Err(fields
            .invalid("finite non-negative playback_speed and fade_duration values")
            .into());
    }
    let completion_event = match fields.get("completion_event") {
        Some(Value::String(name)) if name.trim().is_empty() => None,
        Some(Value::String(name)) => Some(name.clone()),
        None => Some("animation.completed".to_owned()),
        Some(_) => return Err(fields.invalid(EXPECTED).into()),
    };
    let root_motion_mode = match fields.get("root_motion_mode") {
        None => RootMotionMode::Disabled,
        Some(Value::String(mode)) if mode == "disabled" => RootMotionMode::Disabled,
        Some(Value::String(mode)) if mode == "extracted_only" => RootMotionMode::ExtractedOnly,
        Some(Value::String(mode)) if mode == "applied_to_motor" => RootMotionMode::AppliedToMotor,
        _ => return Err(fields.invalid(EXPECTED).into()),
    };
    if root_motion_mode == RootMotionMode::AppliedToMotor
        && !context
            .authoring_entity
            .components
            .contains_key(&ComponentTypeId::new(CHARACTER_CONTROLLER_COMPONENT))
    {
        return Err(fields
            .invalid("an engine.character_controller when root_motion_mode is applied_to_motor")
            .into());
    }

    let Some(graph_asset) = graph_asset else {
        return Ok(());
    };
    let graph_path = manifest_asset_path(&graph_asset, context)?;
    let compiled_graph =
        load_animation_graph(&graph_path).map_err(|source| SceneBridgeError::AssetLoad {
            asset: graph_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: graph_path,
                message: source.to_string(),
            },
        })?;
    let resolved_set = resolve_animation_set(
        &animation_set_asset,
        &graph_asset,
        entity,
        context,
    )?;
    let ResolvedAnimationSet {
        clips: resolved_clips,
        events: animation_set_events,
        unresolved_motion_slots,
    } = resolved_set;
    if let Some(motion_key) = compiled_graph
        .states
        .iter()
        .filter_map(|state| state.motion_key())
        .find(|motion_key| !resolved_clips.contains_key(*motion_key))
    {
        if unresolved_motion_slots.contains(motion_key) {
            return Ok(());
        }
        return Err(fields
            .invalid("an Animation Set binding for every graph motion slot")
            .into());
    }
    let active_motion_key = compiled_graph
        .states
        .get(compiled_graph.entry_state)
        .and_then(|state| state.motion_key())
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| fields.invalid("an Animation Graph entry state with a motion slot"))?
        .to_owned();
    let active_clip = resolved_clips
        .get(&active_motion_key)
        .copied()
        .ok_or_else(|| fields.invalid("every graph motion slot bound in Animation Set"))?;

    let mut animator = Animator::playing(active_clip);
    animator.set_looping(looping);
    let _ = animator.set_playback_speed(playback_speed);
    for (clip, events) in animation_set_events {
        animator.set_clip_events(clip, events);
    }
    animator.completion_event = completion_event;
    animator.root_motion_mode = root_motion_mode;
    context.world.add_component(entity, animator)?;
    if root_motion_mode == RootMotionMode::AppliedToMotor {
        context
            .world
            .add_component(entity, RootMotionRequest::default())?;
    }

    let mut player = AnimGraphPlayer::new(compiled_graph, resolved_clips);
    player.fade_duration = fade_duration;
    let parameters = match fields.get("parameters") {
        None => BTreeMap::new(),
        Some(Value::Object(parameters)) => parameters.clone(),
        Some(_) => return Err(fields.invalid(EXPECTED).into()),
    };
    for (name, value) in parameters {
        let Value::Bool(value) = value else {
            return Err(fields
                .invalid("parameter defaults whose values are boolean")
                .into());
        };
        player
            .set_bool_parameter(name, value)
            .map_err(|_| fields.invalid("named boolean parameter defaults"))?;
    }
    context.world.add_component(entity, player)?;
    Ok(())
}

pub(crate) fn spawn_behavior_tree_runner_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(BEHAVIOR_TREE_RUNNER_COMPONENT);
    let expected = "an object with graph, blackboard, and enabled fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let Some(assigned) = fields.assignable_asset_ref("graph")? else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "graph",
            ));
        return Ok(());
    };
    let graph_asset = assigned;
    let blackboard = match fields.get("blackboard") {
        Some(Value::Object(values)) => values.clone(),
        _ => return Err(fields.invalid(expected).into()),
    };
    let enabled = fields.bool_or("enabled", true)?;
    let graph_path = manifest_asset_path(graph_asset, context)?;
    let json =
        std::fs::read_to_string(&graph_path).map_err(|source| SceneBridgeError::AssetLoad {
            asset: graph_asset.clone(),
            source: AssetLoadError::Io {
                path: graph_path.clone(),
                source,
            },
        })?;
    let graph: Graph =
        serde_json::from_str(&json).map_err(|source| SceneBridgeError::AssetLoad {
            asset: graph_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: graph_path.clone(),
                message: format!("Behavior Tree graph JSON could not be parsed: {source}"),
            },
        })?;
    let service = BehaviorTreeAuthoringService::new();
    service
        .ensure_behavior_tree_graph(&graph)
        .map_err(|source| SceneBridgeError::AssetLoad {
            asset: graph_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: graph_path.clone(),
                message: source.to_string(),
            },
        })?;
    let compilation = service.compile(&graph);
    let tree = compilation.compiled_tree.ok_or_else(|| {
        let details = compilation
            .diagnostics
            .iter()
            .map(|diagnostic| format!("[{}] {}", diagnostic.code, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ");
        SceneBridgeError::AssetLoad {
            asset: graph_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: graph_path,
                message: format!("Behavior Tree graph did not compile: {details}"),
            },
        }
    })?;
    let mut runner = BehaviorTreeRunner::with_blackboard(tree, blackboard);
    runner.set_enabled(enabled);
    context.world.add_component(entity, runner)?;
    Ok(())
}

pub(crate) fn spawn_audio_emitter_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(AUDIO_EMITTER_COMPONENT);
    let expected = "an object with clip, volume, distance, spatial blend, and autoplay fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    if !context
        .authoring_entity
        .components
        .contains_key(&ComponentTypeId::new(TRANSFORM_COMPONENT))
    {
        return Err(fields
            .invalid("an engine.transform component on the same entity")
            .into());
    }
    let Some(assigned) = fields.assignable_asset_ref("clip")? else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "clip",
            ));
        return Ok(());
    };
    let clip = assigned.clone();
    let volume = fields.f32("volume")?;
    let spatial_blend = fields.f32("spatial_blend")?;
    let min_distance = fields.f32("min_distance")?;
    let max_distance = fields.f32("max_distance")?;
    let autoplay = fields.bool_or("autoplay", false)?;
    if !(0.0..=1.0).contains(&volume)
        || !(0.0..=1.0).contains(&spatial_blend)
        || min_distance <= 0.0
        || max_distance < min_distance
    {
        return Err(fields
            .invalid("volume/spatial_blend in 0..=1 and 0 < min_distance <= max_distance")
            .into());
    }
    let handle = resolve_audio_asset(&clip, context)?;
    context.world.add_component(
        entity,
        AudioEmitter::new(
            handle,
            volume,
            spatial_blend,
            min_distance,
            max_distance,
            autoplay,
        ),
    )?;
    Ok(())
}

pub(crate) fn spawn_audio_listener_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(AUDIO_LISTENER_COMPONENT);
    let expected = "an object with an enabled boolean field";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let enabled = fields.bool_or("enabled", true)?;
    context
        .world
        .add_component(entity, AudioListener { enabled })?;
    Ok(())
}

pub(crate) fn spawn_music_controller_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(MUSIC_CONTROLLER_COMPONENT);
    let expected = "an object with clip, volume, fade_in_seconds, and autoplay fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let Some(assigned) = fields.assignable_asset_ref("clip")? else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "clip",
            ));
        return Ok(());
    };
    let clip = assigned.clone();
    let volume = fields.f32("volume")?;
    let fade_in_seconds = fields.f32("fade_in_seconds")?;
    let autoplay = fields.bool_or("autoplay", false)?;
    if !(0.0..=1.0).contains(&volume) || fade_in_seconds < 0.0 {
        return Err(fields
            .invalid("volume in 0..=1 and a non-negative fade_in_seconds")
            .into());
    }
    let handle = resolve_audio_asset(&clip, context)?;
    context.world.add_component(
        entity,
        MusicController::new(handle, volume, fade_in_seconds, autoplay),
    )?;
    Ok(())
}

pub(crate) fn spawn_foot_ik_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(FOOT_IK_COMPONENT);
    let expected = "an object with max_correction and enabled fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let default = FootIk::default();
    let max_correction = fields.f32_or("max_correction", default.max_correction)?;
    let enabled = fields.bool_or("enabled", default.enabled)?;
    if !max_correction.is_finite() || max_correction < 0.0 {
        return Err(fields
            .invalid("a finite, non-negative max_correction")
            .into());
    }
    context.world.add_component(
        entity,
        FootIk {
            max_correction,
            enabled,
        },
    )?;
    Ok(())
}

fn resolve_audio_asset(
    asset: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Result<Handle<AudioAsset>, SceneBridgeError> {
    if let Some(handle) = context.asset_state.audio_handles.get(asset) {
        return Ok(*handle);
    }
    let path = manifest_asset_path(asset, context)?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("wav") && !extension.eq_ignore_ascii_case("ogg") {
        return Err(SceneBridgeError::AssetLoad {
            asset: asset.clone(),
            source: AssetLoadError::UnsupportedAudioFormat { path },
        });
    }
    let bytes = std::fs::read(&path).map_err(|source| SceneBridgeError::AssetLoad {
        asset: asset.clone(),
        source: AssetLoadError::Io {
            path: path.clone(),
            source,
        },
    })?;
    let decoded = AudioAsset::from_bytes(bytes).map_err(|source| SceneBridgeError::AssetLoad {
        asset: asset.clone(),
        source: AssetLoadError::AudioDecode {
            path,
            message: source.to_string(),
        },
    })?;
    let handle = context
        .world
        .get_resource_mut::<Assets<AudioAsset>>()
        .expect("audio asset store must exist before audio component dispatch")
        .add(decoded);
    context
        .asset_state
        .runtime_ids
        .insert(asset.clone(), handle.id());
    context
        .asset_state
        .audio_handles
        .insert(asset.clone(), handle);
    context.asset_state.added_audio_handles.push(handle);
    Ok(handle)
}

struct ResolvedAnimationClipSource {
    clips: BTreeMap<String, Handle<AnimationClip>>,
    selected_clip: Option<String>,
}

struct ResolvedAnimationSet {
    clips: BTreeMap<String, Handle<AnimationClip>>,
    events: Vec<(Handle<AnimationClip>, Vec<AnimEvent>)>,
    unresolved_motion_slots: HashSet<String>,
}

fn resolve_animation_set(
    animation_set_asset: &AssetId,
    graph_asset: &AssetId,
    entity: Entity,
    context: &mut SpawnContext<'_>,
) -> Result<ResolvedAnimationSet, SceneBridgeError> {
    let path = manifest_asset_path(animation_set_asset, context)?;
    let target_skeleton_id = context
        .world
        .get_component::<Skeleton>(entity)
        .and_then(|skeleton| skeleton.asset.clone());
    let Some(target_skeleton_id) = target_skeleton_id else {
        return Err(SceneBridgeError::AssetLoad {
            asset: animation_set_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: path.clone(),
                message: "Animation Controller rig has no target SkeletonAsset ID".to_owned(),
            },
        });
    };
    let target_skeleton = context
        .world
        .get_resource::<SkeletonAssetRegistry>()
        .and_then(|registry| registry.get(&target_skeleton_id))
        .cloned();
    let target_skeleton = if let Some(target_skeleton) = target_skeleton {
        target_skeleton
    } else {
        resolve_retarget_skeleton_asset(
            &target_skeleton_id,
            context.asset_root,
            context.manifest,
            context.asset_state,
        )
        .ok_or_else(|| SceneBridgeError::AssetLoad {
            asset: animation_set_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: path.clone(),
                message: format!(
                    "Animation Controller target skeleton `{}` could not be loaded",
                    target_skeleton_id.as_str()
                ),
            },
        })?
    };
    let json = std::fs::read_to_string(&path).map_err(|source| SceneBridgeError::AssetLoad {
        asset: animation_set_asset.clone(),
        source: AssetLoadError::Io {
            path: path.clone(),
            source,
        },
    })?;
    let animation_set =
        AnimationSet::from_json(&json).map_err(|source| SceneBridgeError::AssetLoad {
            asset: animation_set_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path: path.clone(),
                message: source.to_string(),
            },
        })?;
    if animation_set.graph.as_ref() != Some(graph_asset) {
        let target_graph = animation_set
            .graph
            .as_ref()
            .map(AssetId::as_str)
            .unwrap_or("(unassigned)");
        return Err(SceneBridgeError::AssetLoad {
            asset: animation_set_asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path,
                message: format!(
                    "animation set targets graph `{}`, but the controller selected `{}`",
                    target_graph,
                    graph_asset.as_str()
                ),
            },
        });
    }

    let mut clips = BTreeMap::new();
    let mut events = Vec::<(Handle<AnimationClip>, Vec<AnimEvent>)>::new();
    let mut unresolved_motion_slots = HashSet::new();
    for (motion_slot, binding) in animation_set.bindings {
        let Some(primary_handle) = resolve_animation_binding_clip(
            animation_set_asset,
            &binding.clip,
            &target_skeleton,
            context,
        )?
        else {
            unresolved_motion_slots.insert(motion_slot.as_str().to_owned());
            continue;
        };
        let mut layer_handles = Vec::with_capacity(binding.overlays.len() + 1);
        layer_handles.push(primary_handle);
        for overlay in &binding.overlays {
            if let Some(overlay_handle) = resolve_animation_binding_clip(
                animation_set_asset,
                overlay,
                &target_skeleton,
                context,
            )? {
                layer_handles.push(overlay_handle);
            }
        }

        let handle = if layer_handles.len() == 1 {
            primary_handle
        } else {
            let layer_clips = {
                let assets = context
                    .world
                    .get_resource::<Assets<AnimationClip>>()
                    .expect("animation asset store must exist before Animation Set resolution");
                layer_handles
                    .iter()
                    .map(|handle| {
                        assets
                            .get(handle)
                            .cloned()
                            .ok_or_else(|| SceneBridgeError::UnknownAsset {
                                asset: binding.clip.asset.clone(),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let composite = compose_animation_clips(&layer_clips[0], &layer_clips[1..]).map_err(
                |source| SceneBridgeError::AssetLoad {
                    asset: animation_set_asset.clone(),
                    source: AssetLoadError::InvalidAsset {
                        path: path.clone(),
                        message: format!(
                            "animation-set binding `{}` could not compose its layers: {source}",
                            binding.name
                        ),
                    },
                },
            )?;
            let mut fingerprint = String::from("animation-composite-v1");
            for layer_handle in &layer_handles {
                let (source_id, sub_asset_id) = context
                    .asset_state
                    .animation_clip_sources
                    .get(&layer_handle.id())
                    .ok_or_else(|| SceneBridgeError::UnknownAsset {
                        asset: binding.clip.asset.clone(),
                    })?;
                let source_fingerprint = context
                    .manifest
                    .get(source_id)
                    .and_then(|entry| entry.import_settings.source_fingerprint.as_deref())
                    .ok_or_else(|| SceneBridgeError::AssetLoad {
                        asset: source_id.clone(),
                        source: AssetLoadError::InvalidAsset {
                            path: manifest_asset_path(source_id, context).unwrap_or_default(),
                            message: "animation source has no fingerprint; reimport it before composing this binding".to_owned(),
                        },
                    })?;
                fingerprint.push_str(&format!(
                    ":{}:{}:{}:{}",
                    source_fingerprint.len(),
                    source_fingerprint,
                    sub_asset_id.as_str().len(),
                    sub_asset_id.as_str()
                ));
            }
            let composite_handle = context
                .world
                .get_resource_mut::<Assets<AnimationClip>>()
                .expect("animation asset store must exist before Animation Set resolution")
                .add(composite);
            context
                .asset_state
                .added_animation_clip_handles
                .push(composite_handle);
            context.asset_state.composed_animation_cache_sources.insert(
                composite_handle.id(),
                (
                    fingerprint,
                    format!("{}:{}", animation_set_asset.as_str(), motion_slot.as_str()),
                    animation_set_asset.clone(),
                ),
            );
            composite_handle
        };
        clips.insert(motion_slot.as_str().to_owned(), handle);
        if !binding.events.is_empty() {
            let binding_events = binding
                .events
                .into_iter()
                .map(|event| AnimEvent {
                    time: event.time,
                    name: event.name,
                })
                .collect::<Vec<_>>();
            if let Some((_, existing)) = events.iter_mut().find(|(clip, _)| *clip == handle) {
                existing.extend(binding_events);
                existing.sort_by(|left, right| {
                    left.time
                        .total_cmp(&right.time)
                        .then(left.name.cmp(&right.name))
                });
            } else {
                events.push((handle, binding_events));
            }
        }
    }
    Ok(ResolvedAnimationSet {
        clips,
        events,
        unresolved_motion_slots,
    })
}

fn resolve_animation_binding_clip(
    animation_set_asset: &AssetId,
    source: &engine_authoring::MotionSourceRef,
    target_skeleton: &SkeletonAsset,
    context: &mut SpawnContext<'_>,
) -> Result<Option<Handle<AnimationClip>>, SceneBridgeError> {
    if source.variant == engine_authoring::MotionSourceVariant::Humanoid {
        return resolve_humanoid_animation_binding_clip(
            animation_set_asset,
            &source.asset,
            target_skeleton,
            context,
        )
        .map(Some);
    }

    let resolved = resolve_animation_clip_source(&source.asset, context)?;
    let selected_name = resolved.selected_clip.ok_or_else(|| {
        animation_binding_error(
            animation_set_asset,
            &source.asset,
            context,
            "Animation Set Native and Auto sources must reference imported Animation sub-assets",
        )
    })?;
    let native_handle = resolved
        .clips
        .get(&selected_name)
        .copied()
        .ok_or_else(|| SceneBridgeError::UnknownAsset {
            asset: source.asset.clone(),
        })?;
    let native_clip = context
        .world
        .get_resource::<Assets<AnimationClip>>()
        .and_then(|assets| assets.get(&native_handle))
        .cloned()
        .ok_or_else(|| SceneBridgeError::UnknownAsset {
            asset: source.asset.clone(),
        })?;
    let source_skeleton_id = native_clip.skeleton.clone().ok_or_else(|| {
        animation_binding_error(
            animation_set_asset,
            &source.asset,
            context,
            "Animation Set Native and Auto sources must be bound to an imported skeleton",
        )
    })?;

    if source_skeleton_id == target_skeleton.id {
        return Ok(Some(native_handle));
    }

    if source.variant == engine_authoring::MotionSourceVariant::Auto {
        let assets_root = context.asset_root.unwrap_or_else(|| Path::new("."));
        let maps = crate::retarget::load_registered_retarget_maps(assets_root, context.manifest);
        if crate::retarget::find_retarget_map_for_pair(
            &maps,
            &source_skeleton_id,
            &target_skeleton.id,
        )
        .is_none()
        {
            let humanoid_asset =
                crate::asset::imported_humanoid_motion_sub_asset_id(&source.asset);
            return resolve_humanoid_animation_binding_clip(
                animation_set_asset,
                &humanoid_asset,
                target_skeleton,
                context,
            )
            .map(Some);
        }
    }

    resolve_retargeted_animation_binding_clip(
        animation_set_asset,
        &source.asset,
        native_handle,
        &native_clip,
        &source_skeleton_id,
        target_skeleton,
        context,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_retargeted_animation_binding_clip(
    _animation_set_asset: &AssetId,
    source_asset: &AssetId,
    native_handle: Handle<AnimationClip>,
    native_clip: &AnimationClip,
    source_skeleton_id: &AssetId,
    target_skeleton: &SkeletonAsset,
    context: &mut SpawnContext<'_>,
) -> Result<Option<Handle<AnimationClip>>, SceneBridgeError> {
    let provenance = context
        .asset_state
        .animation_clip_sources
        .get(&native_handle.id())
        .cloned()
        .ok_or_else(|| SceneBridgeError::UnknownAsset {
            asset: source_asset.clone(),
        })?;
    let packaged = context
        .world
        .get_resource::<crate::retarget::PackagedBakedClips>()
        .cloned();
    let adapted = match resolve_cross_skeleton_clip(
        native_clip,
        &native_handle,
        source_skeleton_id,
        &target_skeleton.id,
        context.asset_root,
        context.manifest,
        context.asset_state,
        packaged.as_ref(),
    ) {
        Ok(adapted) => adapted,
        Err(diagnostic) => {
            context.asset_diagnostics.push(*diagnostic);
            return Ok(None);
        }
    };
    Ok(Some(store_animation_binding_clip(
        adapted, provenance, context,
    )))
}

fn resolve_humanoid_animation_binding_clip(
    animation_set_asset: &AssetId,
    motion_asset: &AssetId,
    target_skeleton: &SkeletonAsset,
    context: &mut SpawnContext<'_>,
) -> Result<Handle<AnimationClip>, SceneBridgeError> {
    let (source_id, existing_profiles) = {
        let Some((source_id, entry, sub_asset)) = context.manifest.imported_sub_asset(motion_asset)
        else {
            return Err(animation_binding_error(
                animation_set_asset,
                motion_asset,
                context,
                "Humanoid source is not a registered imported sub-asset",
            ));
        };
        if sub_asset.kind != ImportedSubAssetKind::HumanoidMotion {
            return Err(animation_binding_error(
                animation_set_asset,
                motion_asset,
                context,
                "Humanoid source must reference an imported HumanoidMotion sub-asset",
            ));
        }
        (
            source_id.clone(),
            entry.import_settings.humanoid_profiles.clone(),
        )
    };

    if let Some(packaged_root) = context
        .world
        .get_resource::<crate::retarget::PackagedBakedClips>()
        .map(|packaged| packaged.root.clone())
    {
        let file_name = crate::humanoid_motion::humanoid_packaged_bake_file_name(
            motion_asset,
            &target_skeleton.id,
        );
        let path = packaged_root.join("humanoid").join(&file_name);
        let clip = std::fs::read(&path)
            .ok()
            .and_then(|bytes| crate::humanoid_motion::deserialize_humanoid_baked_clip(&bytes).ok())
            .filter(|clip| {
                clip.skeleton.as_ref() == Some(&target_skeleton.id)
                    && clip.skeleton_identity == Some(target_skeleton.identity)
            })
            .ok_or_else(|| {
                let diagnostic = Diagnostic::error(
                    crate::humanoid_motion::HUMANOID_BAKE_MISSING_FROM_PACKAGE_DIAGNOSTIC,
                    format!(
                        "packaged Humanoid bake `{file_name}` for motion `{}` and target skeleton `{}` was not found or did not match the target; re-package the project",
                        motion_asset.as_str(),
                        target_skeleton.id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Asset {
                    id: motion_asset.clone(),
                });
                let message = diagnostic.message.clone();
                context.asset_diagnostics.push(diagnostic);
                animation_binding_error(animation_set_asset, motion_asset, context, message)
            })?;
        return Ok(store_animation_binding_clip(
            clip,
            (source_id, motion_asset.clone()),
            context,
        ));
    }

    let imported = import_source_cached(&source_id, context).map_err(|error| {
        animation_binding_error(
            animation_set_asset,
            motion_asset,
            context,
            format!(
                "could not import the model source owning Humanoid motion `{}`: {error}",
                motion_asset.as_str()
            ),
        )
    })?;
    let catalog =
        crate::humanoid_import::build_humanoid_import_catalog(&imported, &existing_profiles);
    context.asset_diagnostics.extend(
        catalog
            .diagnostics
            .iter()
            .cloned()
            .map(|diagnostic| diagnostic.with_target(DiagnosticTarget::Asset {
                id: source_id.clone(),
            })),
    );
    let portable = catalog
        .motions
        .iter()
        .find(|motion| &motion.id == motion_asset)
        .ok_or_else(|| {
            animation_binding_error(
                animation_set_asset,
                motion_asset,
                context,
                "the source no longer exposes this Humanoid motion; reimport it",
            )
        })?;
    let target_profile = context
        .manifest
        .iter()
        .flat_map(|(_, entry)| entry.import_settings.humanoid_profiles.iter())
        .find(|profile| profile.skeleton == target_skeleton.id.as_str())
        .cloned()
        .ok_or_else(|| {
            animation_binding_error(
                animation_set_asset,
                motion_asset,
                context,
                format!(
                    "target skeleton `{}` has no persisted HumanoidProfile",
                    target_skeleton.id.as_str()
                ),
            )
        })?;

    let mut baked = if let Some(cache) = context.asset_state.derived_cache.as_ref() {
        crate::humanoid_motion::resolve_or_bake_humanoid_motion(
            cache,
            &portable.motion,
            target_skeleton,
            &target_profile,
        )
    } else {
        crate::humanoid_motion::bake_humanoid_motion(
            &portable.motion,
            target_skeleton,
            &target_profile,
        )
    }
    .map_err(|error| {
        animation_binding_error(
            animation_set_asset,
            motion_asset,
            context,
            format!(
                "Humanoid motion `{}` could not be baked for target skeleton `{}`: {error}",
                motion_asset.as_str(),
                target_skeleton.id.as_str()
            ),
        )
    })?;
    context.asset_diagnostics.append(&mut baked.diagnostics);
    Ok(store_animation_binding_clip(
        baked.clip,
        (source_id, motion_asset.clone()),
        context,
    ))
}

fn store_animation_binding_clip(
    clip: AnimationClip,
    provenance: (AssetId, AssetId),
    context: &mut SpawnContext<'_>,
) -> Handle<AnimationClip> {
    let handle = context
        .world
        .get_resource_mut::<Assets<AnimationClip>>()
        .expect("animation asset store must exist before Animation Set resolution")
        .add(clip);
    context
        .asset_state
        .added_animation_clip_handles
        .push(handle);
    context
        .asset_state
        .animation_clip_sources
        .insert(handle.id(), provenance);
    handle
}

fn animation_binding_error(
    animation_set_asset: &AssetId,
    source_asset: &AssetId,
    context: &SpawnContext<'_>,
    message: impl Into<String>,
) -> SceneBridgeError {
    SceneBridgeError::AssetLoad {
        asset: source_asset.clone(),
        source: AssetLoadError::InvalidAsset {
            path: manifest_asset_path(animation_set_asset, context)
                .unwrap_or_else(|_| std::path::PathBuf::from("<animation-set>")),
            message: message.into(),
        },
    }
}

struct SourceAnimation {
    id: AssetId,
    runtime_key: String,
    clip: AnimationClip,
}

fn selected_clip_name(selectors: &[(AssetId, String)], asset: &AssetId) -> Option<String> {
    selectors
        .iter()
        .find(|(selector, _)| selector == asset)
        .map(|(_, name)| name.clone())
}

fn load_source_animations(
    source_id: &AssetId,
    path: &Path,
    context: &mut SpawnContext<'_>,
) -> Result<Vec<SourceAnimation>, SceneBridgeError> {
    if crate::components::asset_path_matches_kind(
        crate::components::AssetKind::MotionSource,
        path,
    ) {
        return load_motion_source_animations(source_id, path, context);
    }
    let existing_skeletons = context
        .manifest
        .get(source_id)
        .map(|entry| entry.import_settings.skeleton_records.clone())
        .unwrap_or_default();
    let contact_bones = context
        .manifest
        .get(source_id)
        .map(|entry| entry.import_settings.contact_bones.clone())
        .unwrap_or_default();
    let imported = import_gltf_cached(
        source_id,
        path,
        &existing_skeletons,
        &contact_bones,
        context.asset_state,
    )
    .map_err(|source| SceneBridgeError::AssetLoad {
        asset: source_id.clone(),
        source: AssetLoadError::InvalidAsset {
            path: path.to_path_buf(),
            message: source.to_string(),
        },
    })?;
    for diagnostic in imported.diagnostics.iter().cloned() {
        context
            .asset_diagnostics
            .push(diagnostic.with_target(DiagnosticTarget::Asset {
                id: source_id.clone(),
            }));
    }
    Ok(imported
        .animations
        .iter()
        .cloned()
        .map(|animation| SourceAnimation {
            id: animation.id,
            runtime_key: animation.name,
            clip: animation.clip,
        })
        .collect())
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn load_motion_source_animations(
    source_id: &AssetId,
    path: &Path,
    context: &mut SpawnContext<'_>,
) -> Result<Vec<SourceAnimation>, SceneBridgeError> {
    let invalid = |path: &Path, message: String| SceneBridgeError::AssetLoad {
        asset: source_id.clone(),
        source: AssetLoadError::InvalidAsset {
            path: path.to_path_buf(),
            message,
        },
    };

    let settings = context
        .manifest
        .get(source_id)
        .map(|entry| entry.import_settings.clone())
        .ok_or_else(|| invalid(path, "motion source is not registered".to_owned()))?;
    let model_sources = settings
        .resolved_motion_model_sources()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if model_sources.is_empty() {
        return Err(invalid(
            path,
            "motion source has no output PMX model; select at least one output in Import Settings before Play".to_owned(),
        ));
    }
    let original_model_source = settings
        .motion_original_model_source
        .as_deref()
        .map(|original| {
            AssetId::from_stable_id(engine_authoring::StableId::new(original)).map_err(|error| {
                invalid(
                    path,
                    format!("original model source `{original}` is not a valid asset ID: {error}"),
                )
            })
        })
        .transpose()?;
    let options = crate::vmd_import::VmdBakeOptions {
        contact_bone_names: context
            .manifest
            .get(source_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default(),
        ..crate::vmd_import::VmdBakeOptions::default()
    };
    let mut animations = Vec::new();
    if let Some(original_model_source) = original_model_source {
        let (_original_path, original_rig) =
            load_vmd_model_rig(source_id, &original_model_source, context)?;
        let mut original_bake = resolve_or_bake_vmd_for_scene(
            source_id,
            path,
            &original_rig,
            &options,
            context,
        )
        .map_err(|error| invalid(path, error.to_string()))?;
        for diagnostic in std::mem::take(&mut original_bake.diagnostics) {
            context
                .asset_diagnostics
                .push(diagnostic.with_target(DiagnosticTarget::Asset {
                    id: source_id.clone(),
                }));
        }
        let assets_root = context.asset_root.unwrap_or_else(|| Path::new("."));
        let retarget_maps =
            crate::retarget::load_registered_retarget_maps(assets_root, context.manifest);

        for model_source in model_sources {
            let model_source_id = AssetId::from_stable_id(engine_authoring::StableId::new(
                &model_source,
            ))
            .map_err(|error| {
                invalid(
                    path,
                    format!("output model source `{model_source}` is not a valid asset ID: {error}"),
                )
            })?;
            let mut baked = original_bake.clone();
            if model_source_id == original_model_source {
                baked.bind_model_source(source_id, &model_source_id);
            } else {
                let (target_path, target_rig) =
                    load_vmd_model_rig(source_id, &model_source_id, context)?;
                let map = crate::retarget::find_retarget_map_for_pair(
                    &retarget_maps,
                    &original_rig.skeleton().id,
                    &target_rig.skeleton().id,
                )
                .ok_or_else(|| {
                    invalid(
                        &target_path,
                        format!(
                            "no Retarget Map resolves original skeleton `{}` to output skeleton `{}`",
                            original_rig.skeleton().id.as_str(),
                            target_rig.skeleton().id.as_str()
                        ),
                    )
                })?;
                if let Some(stale) = map
                    .validate(
                        original_rig.skeleton().identity,
                        target_rig.skeleton().identity,
                    )
                    .into_iter()
                    .next()
                {
                    return Err(invalid(&target_path, stale.message));
                }
                let target_contact_bones = context
                    .manifest
                    .get(&model_source_id)
                    .map(|entry| entry.import_settings.contact_bones.clone())
                    .unwrap_or_default();
                baked
                    .retarget_to_model_source(
                        source_id,
                        &model_source_id,
                        original_rig.skeleton(),
                        target_rig.skeleton(),
                        map,
                        &target_contact_bones,
                    )
                    .map_err(|error| invalid(&target_path, error.to_string()))?;
            }
            animations.extend(baked.clips.into_iter().map(|baked| SourceAnimation {
                runtime_key: baked.id.as_str().to_owned(),
                id: baked.id,
                clip: baked.clip,
            }));
        }
        return Ok(animations);
    }

    for model_source in model_sources {
        let model_source_id = AssetId::from_stable_id(engine_authoring::StableId::new(
            &model_source,
        ))
        .map_err(|error| {
            invalid(
                path,
                format!("target model source `{model_source}` is not a valid asset ID: {error}"),
            )
        })?;
        let model_path = manifest_asset_path(&model_source_id, context)?;
        let existing_skeletons = context
            .manifest
            .get(&model_source_id)
            .map(|entry| entry.import_settings.skeleton_records.clone())
            .unwrap_or_default();
        let contact_bones = context
            .manifest
            .get(&model_source_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();
        let imported = import_gltf_cached(
            &model_source_id,
            &model_path,
            &existing_skeletons,
            &contact_bones,
            context.asset_state,
        )
        .map_err(|source| invalid(&model_path, source.to_string()))?;
        let model_bytes =
            std::fs::read(&model_path).map_err(|error| invalid(&model_path, error.to_string()))?;
        let rig =
            crate::vmd_import::VmdBakeRig::from_model_import(&model_path, &model_bytes, &imported)
                .map_err(|error| invalid(&model_path, error.to_string()))?;
        let mut baked =
            resolve_or_bake_vmd_for_scene(source_id, path, &rig, &options, context)
                .map_err(|error| invalid(path, error.to_string()))?;
        baked.bind_model_source(source_id, &model_source_id);
        for diagnostic in baked.diagnostics {
            context
                .asset_diagnostics
                .push(diagnostic.with_target(DiagnosticTarget::Asset {
                    id: source_id.clone(),
                }));
        }
        animations.extend(baked.clips.into_iter().map(|baked| SourceAnimation {
            runtime_key: baked.id.as_str().to_owned(),
            id: baked.id,
            clip: baked.clip,
        }));
    }
    Ok(animations)
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn resolve_or_bake_vmd_for_scene(
    source_id: &AssetId,
    path: &Path,
    rig: &crate::vmd_import::VmdBakeRig,
    options: &crate::vmd_import::VmdBakeOptions,
    context: &SpawnContext<'_>,
) -> Result<crate::vmd_import::VmdImportResult, crate::vmd_import::VmdImportError> {
    match &context.asset_state.derived_cache {
        Some(cache) => crate::vmd_import::resolve_or_bake_vmd_path(
            cache, source_id, path, rig, options,
        ),
        None => crate::vmd_import::import_vmd_path(source_id, path, rig, options),
    }
}

#[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
fn load_vmd_model_rig(
    motion_source_id: &AssetId,
    model_source_id: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Result<(std::path::PathBuf, crate::vmd_import::VmdBakeRig), SceneBridgeError> {
    let invalid = |path: &Path, message: String| SceneBridgeError::AssetLoad {
        asset: motion_source_id.clone(),
        source: AssetLoadError::InvalidAsset {
            path: path.to_path_buf(),
            message,
        },
    };
    let model_path = manifest_asset_path(model_source_id, context)?;
    let existing_skeletons = context
        .manifest
        .get(model_source_id)
        .map(|entry| entry.import_settings.skeleton_records.clone())
        .unwrap_or_default();
    let contact_bones = context
        .manifest
        .get(model_source_id)
        .map(|entry| entry.import_settings.contact_bones.clone())
        .unwrap_or_default();
    let imported = import_gltf_cached(
        model_source_id,
        &model_path,
        &existing_skeletons,
        &contact_bones,
        context.asset_state,
    )
    .map_err(|source| invalid(&model_path, source.to_string()))?;
    let model_bytes =
        std::fs::read(&model_path).map_err(|error| invalid(&model_path, error.to_string()))?;
    let rig = crate::vmd_import::VmdBakeRig::from_model_import(
        &model_path,
        &model_bytes,
        &imported,
    )
    .map_err(|error| invalid(&model_path, error.to_string()))?;
    Ok((model_path, rig))
}

#[cfg(not(all(feature = "mmd-import", not(target_arch = "wasm32"))))]
fn load_motion_source_animations(
    source_id: &AssetId,
    path: &Path,
    _context: &mut SpawnContext<'_>,
) -> Result<Vec<SourceAnimation>, SceneBridgeError> {
    Err(SceneBridgeError::AssetLoad {
        asset: source_id.clone(),
        source: AssetLoadError::InvalidAsset {
            path: path.to_path_buf(),
            message: "this build has no MMD motion importer; rebuild with the `mmd-import` feature or package the baked clips instead".to_owned(),
        },
    })
}

fn resolve_animation_clip_source(
    asset: &AssetId,
    context: &mut SpawnContext<'_>,
) -> Result<ResolvedAnimationClipSource, SceneBridgeError> {
    let (source_id, selected_asset) =
        if let Some((source_id, _, sub_asset)) = context.manifest.imported_sub_asset(asset) {
            if sub_asset.kind != ImportedSubAssetKind::Animation {
                return Err(SceneBridgeError::UnknownAsset {
                    asset: asset.clone(),
                });
            }
            (source_id.clone(), Some(asset.clone()))
        } else {
            (asset.clone(), None)
        };
    if let Some(clips) = context.asset_state.animation_clip_handles.get(&source_id) {
        let selected_clip = selected_asset.as_ref().and_then(|selected| {
            selected_clip_name(
                context.asset_state.animation_clip_selectors.get(&source_id)?,
                selected,
            )
        });
        return Ok(ResolvedAnimationClipSource {
            clips: clips.clone(),
            selected_clip,
        });
    }
    let path = manifest_asset_path(&source_id, context)?;
    let loaded = load_source_animations(&source_id, &path, context)?;
    if loaded.is_empty() {
        return Err(SceneBridgeError::AssetLoad {
            asset: source_id.clone(),
            source: AssetLoadError::InvalidAsset {
                path,
                message: "source contains no importable animation clips".into(),
            },
        });
    }
    let mut clips = BTreeMap::new();
    let mut selectors = Vec::with_capacity(loaded.len());
    for animation in loaded {
        if clips.contains_key(&animation.runtime_key) {
            return Err(SceneBridgeError::AssetLoad {
                asset: source_id.clone(),
                source: AssetLoadError::InvalidAsset {
                    path: manifest_asset_path(&source_id, context)?,
                    message: format!(
                        "animation runtime key `{}` occurs more than once",
                        animation.runtime_key
                    ),
                },
            });
        }
        let handle = context
            .world
            .get_resource_mut::<Assets<AnimationClip>>()
            .expect("animation asset store must exist before Animator dispatch")
            .add(animation.clip);
        context
            .asset_state
            .added_animation_clip_handles
            .push(handle);
        context
            .asset_state
            .animation_clip_sources
            .insert(handle.id(), (source_id.clone(), animation.id.clone()));
        selectors.push((animation.id, animation.runtime_key.clone()));
        clips.insert(animation.runtime_key, handle);
    }
    if let Some(handle) = clips.values().next() {
        context
            .asset_state
            .runtime_ids
            .insert(source_id.clone(), handle.id());
    }
    context
        .asset_state
        .animation_clip_handles
        .insert(source_id.clone(), clips.clone());
    let selected_clip = selected_asset
        .as_ref()
        .and_then(|selected| selected_clip_name(&selectors, selected));
    context
        .asset_state
        .animation_clip_selectors
        .insert(source_id.clone(), selectors);
    if selected_asset.is_some() && selected_clip.is_none() {
        return Err(SceneBridgeError::AssetLoad {
            asset: asset.clone(),
            source: AssetLoadError::InvalidAsset {
                path,
                message: "the selected imported animation no longer exists; reimport the source"
                    .to_owned(),
            },
        });
    }
    if let Some(name) = &selected_clip
        && let Some(handle) = clips.get(name)
    {
        context
            .asset_state
            .runtime_ids
            .insert(asset.clone(), handle.id());
    }
    Ok(ResolvedAnimationClipSource {
        clips,
        selected_clip,
    })
}

fn manifest_asset_path(
    asset: &AssetId,
    context: &SpawnContext<'_>,
) -> Result<std::path::PathBuf, SceneBridgeError> {
    let entry = context
        .manifest
        .get(asset)
        .ok_or_else(|| SceneBridgeError::UnknownAsset {
            asset: asset.clone(),
        })?;
    Ok(context
        .asset_root
        .unwrap_or_else(|| Path::new("."))
        .join(&entry.path))
}

pub(crate) fn spawn_camera_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(CAMERA_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with boolean enabled, integer priority, and numeric fov_y_degrees, near, and far fields",
    )?;
    let enabled = fields.bool("enabled")?;
    let authored_priority = fields.i64("priority")?;
    let priority = i32::try_from(authored_priority)
        .map_err(|_| fields.invalid("a camera priority between i32::MIN and i32::MAX"))?;
    let fov_y_degrees = fields.f32("fov_y_degrees")?;
    let near = fields.f32("near")?;
    let far = fields.f32("far")?;
    if !fov_y_degrees.is_finite() || !near.is_finite() || !far.is_finite() {
        return Err(fields
            .invalid("finite numeric fov_y_degrees, near, and far fields")
            .into());
    }
    if fov_y_degrees <= 0.0 || near <= 0.0 || far <= near {
        return Err(fields
            .invalid("positive fov_y_degrees and clipping planes with far greater than near")
            .into());
    }
    let aspect = context
        .world
        .get_resource::<crate::camera::ViewportSize>()
        .map(crate::camera::ViewportSize::aspect)
        .unwrap_or(Camera3D::default().aspect);
    let mut camera = Camera3D::new(fov_y_degrees, aspect, near, far);
    camera.enabled = enabled;
    camera.priority = priority;
    context.world.add_component(entity, camera)?;
    Ok(())
}

pub(crate) fn spawn_directional_light_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(DIRECTIONAL_LIGHT_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with numeric direction, color, and intensity fields",
    )?;
    let direction = Vec3::new(
        fields.f32("direction_x")?,
        fields.f32("direction_y")?,
        fields.f32("direction_z")?,
    );
    if !direction.is_finite() || direction.length_squared() <= f32::EPSILON {
        return Err(fields.invalid("a finite non-zero direction vector").into());
    }
    let intensity = fields.f32("intensity")?;
    if intensity < 0.0 {
        return Err(fields.invalid("a non-negative finite intensity").into());
    }
    let light = DirectionalLight {
        direction: direction.normalize(),
        color: fields.color()?,
        intensity,
    };
    context.world.add_component(entity, light)?;
    Ok(())
}

pub(crate) fn spawn_point_light_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    const EXPECTED: &str = "an object with unit-range color, non-negative intensity, and positive range";
    let component_type = ComponentTypeId::new(POINT_LIGHT_COMPONENT);
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let color = fields.color()?;
    let intensity = fields.f32("intensity")?;
    let range = fields.f32("range")?;
    if color.min_element() < 0.0
        || color.max_element() > 1.0
        || !intensity.is_finite()
        || intensity < 0.0
        || !range.is_finite()
        || range <= 0.0
    {
        return Err(fields.invalid(EXPECTED).into());
    }
    context.world.add_component(
        entity,
        PointLight {
            color,
            intensity,
            range,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_spot_light_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    const EXPECTED: &str =
        "an object with unit-range color, non-negative intensity, positive range, and 0 <= inner_angle_degrees < outer_angle_degrees < 90";
    let component_type = ComponentTypeId::new(SPOT_LIGHT_COMPONENT);
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let color = fields.color()?;
    let intensity = fields.f32("intensity")?;
    let range = fields.f32("range")?;
    let inner_angle_degrees = fields.f32("inner_angle_degrees")?;
    let outer_angle_degrees = fields.f32("outer_angle_degrees")?;
    if color.min_element() < 0.0
        || color.max_element() > 1.0
        || !intensity.is_finite()
        || intensity < 0.0
        || !range.is_finite()
        || range <= 0.0
        || !inner_angle_degrees.is_finite()
        || !outer_angle_degrees.is_finite()
        || inner_angle_degrees < 0.0
        || inner_angle_degrees >= outer_angle_degrees
        || outer_angle_degrees >= 90.0
    {
        return Err(fields.invalid(EXPECTED).into());
    }
    context.world.add_component(
        entity,
        SpotLight {
            color,
            intensity,
            range,
            inner_angle_radians: inner_angle_degrees.to_radians(),
            outer_angle_radians: outer_angle_degrees.to_radians(),
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_ambient_light_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(AMBIENT_LIGHT_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with numeric color and intensity fields",
    )?;
    let intensity = fields.f32("intensity")?;
    if intensity < 0.0 {
        return Err(fields.invalid("a non-negative finite intensity").into());
    }
    let light = AmbientLight {
        color: fields.color()?,
        intensity,
    };
    context.world.add_component(entity, light)?;
    Ok(())
}

pub(crate) fn spawn_shadow_settings_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    const EXPECTED: &str =
        "an object with enabled, ordered cascade splits, and non-negative shadow biases";
    let component_type = ComponentTypeId::new(SHADOW_SETTINGS_COMPONENT);
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let enabled = fields.bool("enabled")?;
    let near_split = fields.f32("cascade_near_split")?;
    let far_split = fields.f32("cascade_far_split")?;
    let depth_bias = fields.f32("depth_bias")?;
    let normal_bias = fields.f32("normal_bias")?;
    if !(0.0..=1.0).contains(&near_split)
        || !(0.0..=1.0).contains(&far_split)
        || near_split >= far_split
        || depth_bias < 0.0
        || normal_bias < 0.0
    {
        return Err(fields.invalid(EXPECTED).into());
    }

    let defaults = ShadowSettings::default();
    context.world.add_component(
        entity,
        ShadowSettings {
            enabled,
            cascade_splits: [near_split, far_split],
            depth_bias,
            normal_bias,
            resolution: defaults.resolution,
            format: defaults.format,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_environment_lighting_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    const EXPECTED: &str =
        "an object with diffuse_ibl_enabled, unit-range color, and non-negative intensity";
    let component_type = ComponentTypeId::new(ENVIRONMENT_LIGHTING_COMPONENT);
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let diffuse_ibl_enabled = fields.bool("diffuse_ibl_enabled")?;
    let diffuse_color = fields.color()?;
    let intensity = fields.f32("intensity")?;
    if diffuse_color.min_element() < 0.0 || diffuse_color.max_element() > 1.0 || intensity < 0.0 {
        return Err(fields.invalid(EXPECTED).into());
    }

    context.world.add_component(
        entity,
        EnvironmentLighting {
            skybox: None,
            diffuse_irradiance: None,
            diffuse_color,
            intensity,
            diffuse_ibl_enabled,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_post_process_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    const EXPECTED: &str =
        "an object with enabled, non-negative exposure and bloom fields, and a supported tone_map";
    let component_type = ComponentTypeId::new(POST_PROCESS_COMPONENT);
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, EXPECTED)?;
    let enabled = fields.bool("enabled")?;
    let exposure = fields.f32("exposure")?;
    let tone_map = match fields.string("tone_map")? {
        "aces_fitted" => ToneMapOperator::AcesFitted,
        "reinhard" => ToneMapOperator::Reinhard,
        _ => return Err(fields.invalid(EXPECTED).into()),
    };
    let bloom = BloomSettings {
        enabled: fields.bool("bloom_enabled")?,
        threshold: fields.f32("bloom_threshold")?,
        intensity: fields.f32("bloom_intensity")?,
        radius: fields.f32("bloom_radius")?,
    };
    let grading_default = ColorGradingSettings::default();
    let color_grading = ColorGradingSettings {
        enabled: fields.bool_or("color_grading_enabled", grading_default.enabled)?,
        tint: [
            fields.f32_or("grading_tint_r", grading_default.tint[0])?,
            fields.f32_or("grading_tint_g", grading_default.tint[1])?,
            fields.f32_or("grading_tint_b", grading_default.tint[2])?,
        ],
        saturation: fields.f32_or("grading_saturation", grading_default.saturation)?,
        contrast: fields.f32_or("grading_contrast", grading_default.contrast)?,
        gamma: fields.f32_or("grading_gamma", grading_default.gamma)?,
    };
    if exposure < 0.0
        || bloom.threshold < 0.0
        || bloom.intensity < 0.0
        || bloom.radius < 0.0
        || color_grading.tint.iter().any(|value| *value < 0.0)
        || color_grading.saturation < 0.0
        || color_grading.contrast < 0.0
        || color_grading.gamma <= 0.0
    {
        return Err(fields.invalid(EXPECTED).into());
    }

    context.world.add_component(
        entity,
        PostProcessSettings {
            enabled,
            exposure,
            tone_map,
            hdr_format: PostProcessSettings::default().hdr_format,
            bloom,
            color_grading,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_player_controller_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(PLAYER_CONTROLLER_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with numeric move_speed and string move_plane fields",
    )?;
    let move_speed = fields.f32("move_speed")?;
    let move_plane = match fields.get("move_plane") {
        Some(Value::String(value)) if value == "xz" => MovePlane::Xz,
        Some(Value::String(value)) if value == "xy" => MovePlane::Xy,
        _ => {
            return Err(fields
                .invalid("an object with numeric move_speed and string move_plane fields")
                .into())
        }
    };
    let defaults = PlayerController::default();
    let acceleration = fields.f32_or("acceleration", defaults.acceleration)?;
    let deceleration = fields.f32_or("deceleration", defaults.deceleration)?;
    let sprint_multiplier = fields.f32_or("sprint_multiplier", defaults.sprint_multiplier)?;
    if move_speed < 0.0 || acceleration < 0.0 || deceleration < 0.0 || sprint_multiplier < 0.0 {
        return Err(fields
            .invalid("finite non-negative player motor values")
            .into());
    }
    context.world.add_component(
        entity,
        PlayerController {
            move_speed,
            move_plane,
            acceleration,
            deceleration,
            sprint_multiplier,
            camera_relative: fields.bool_or("camera_relative", defaults.camera_relative)?,
            face_movement: fields.bool_or("face_movement", defaults.face_movement)?,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_nav_mesh_agent_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(NAV_MESH_AGENT_COMPONENT);
    let expected = "an object with finite non-negative navigation agent fields";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let speed = fields.f32("speed")?;
    let stopping_distance = fields.f32("stopping_distance")?;
    let defaults = NavMeshAgent::default();
    let repath_interval = fields.f32_or("repath_interval", defaults.repath_interval)?;
    let avoidance_radius = fields.f32_or("avoidance_radius", defaults.avoidance_radius)?;
    if speed < 0.0 || stopping_distance < 0.0 || repath_interval < 0.0 || avoidance_radius < 0.0 {
        return Err(fields.invalid(expected).into());
    }
    let has_target = fields.bool("has_target")?;
    let mut agent = NavMeshAgent::new(speed);
    agent.stopping_distance = stopping_distance;
    agent.repath_interval = repath_interval;
    agent.avoidance_radius = avoidance_radius;
    if has_target {
        agent.target = Some(Vec3::new(
            fields.f32("target_x")?,
            fields.f32("target_y")?,
            fields.f32("target_z")?,
        ));
    }
    context.world.add_component(entity, agent)?;
    Ok(())
}

pub(crate) fn spawn_nav_mesh_surface_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(NAV_MESH_SURFACE_COMPONENT);
    let expected = "an object with a registered NavMesh asset in source";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let Some(assigned) = fields.assignable_asset_ref("source")? else {
        context
            .asset_diagnostics
            .push(component_inactive_diagnostic(
                context.authoring_entity,
                &component_type,
                "source",
            ));
        return Ok(());
    };
    let source = assigned;
    if context
        .world
        .get_resource::<crate::navmesh::NavMeshQuery>()
        .is_some()
    {
        return Err(fields
            .invalid("only one NavMesh Surface may be active in a scene")
            .into());
    }
    let path = manifest_asset_path(source, context)?;
    let nav_mesh =
        crate::navmesh::load_navmesh(&path).map_err(|error| SceneBridgeError::AssetLoad {
            asset: source.clone(),
            source: AssetLoadError::InvalidAsset {
                path: path.clone(),
                message: error.to_string(),
            },
        })?;
    context
        .world
        .insert_resource(crate::navmesh::NavMeshQuery::new(nav_mesh));
    context
        .world
        .add_component(entity, crate::navmesh::NavMeshSurface { source: path })?;
    Ok(())
}

pub(crate) fn spawn_runtime_metadata_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(RUNTIME_METADATA_COMPONENT);
    let expected = "an object with string name/team fields and a string tags array";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let name = match fields.get("name") {
        Some(Value::String(name)) if !name.trim().is_empty() => name.clone(),
        Some(Value::String(_)) => context.authoring_entity.name.clone(),
        _ => return Err(fields.invalid(expected).into()),
    };
    let team = match fields.get("team") {
        Some(Value::String(team)) => team.clone(),
        _ => return Err(fields.invalid(expected).into()),
    };
    let tags = match fields.get("tags") {
        Some(Value::Array(tags)) => tags
            .iter()
            .map(|tag| match tag {
                Value::String(tag) if !tag.trim().is_empty() => Ok(tag.clone()),
                _ => Err(fields.invalid(expected)),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(fields.invalid(expected).into()),
    };
    context
        .world
        .add_component(entity, RuntimeMetadata { name, tags, team })?;
    Ok(())
}

pub(crate) fn spawn_orbit_camera_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(ORBIT_CAMERA_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with orbit camera fields",
    )?;
    let orbit = OrbitCamera {
        target: Vec3::new(
            fields.f32("target_x")?,
            fields.f32("target_y")?,
            fields.f32("target_z")?,
        ),
        distance: fields.f32("distance")?,
        yaw: fields.f32("yaw")?,
        pitch: fields.f32("pitch")?,
        orbit_speed: fields.f32("orbit_speed")?,
        zoom_speed: fields.f32("zoom_speed")?,
        ..OrbitCamera::default()
    };
    if orbit.distance <= 0.0 || orbit.orbit_speed < 0.0 || orbit.zoom_speed < 0.0 {
        return Err(fields
            .invalid("a positive distance and non-negative orbit/zoom speeds")
            .into());
    }
    context.world.add_component(entity, orbit)?;
    Ok(())
}

pub(crate) fn spawn_follow_camera_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(FOLLOW_CAMERA_COMPONENT);
    let fields = ComponentFields::new(
        context.authoring_entity,
        &component_type,
        value,
        "an object with follow camera fields including a target entity reference",
    )?;
    let target_authoring_id = match fields.get("target") {
        Some(Value::EntityRef(id)) => id.clone(),
        _ => return Err(fields.invalid("a target entity reference").into()),
    };
    let Some(target_runtime) = context.entity_map.get(&target_authoring_id).copied() else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.follow_camera_unresolved_target",
            format!(
                "follow_camera on entity `{}` references unknown entity `{}`; camera skipped",
                context.authoring_entity.id.as_str(),
                target_authoring_id.as_str()
            ),
        ));
        return Ok(());
    };
    let offset = Vec3::new(
        fields.f32("offset_x")?,
        fields.f32("offset_y")?,
        fields.f32("offset_z")?,
    );
    let spring_strength = fields.f32("spring_strength")?;
    if !(0.0..=1.0).contains(&spring_strength) {
        return Err(fields
            .invalid("a spring_strength between 0.0 and 1.0")
            .into());
    }
    context.world.add_component(
        entity,
        FollowCamera::new(target_runtime, offset, spring_strength),
    )?;
    Ok(())
}

const PARTICLE_EMITTER_EXPECTED: &str = "an object with particle emitter fields (mesh, material, spawn_rate, lifetime_min/max, initial_speed_min/max, direction_x/y/z, spread, gravity_x/y/z, start_color_r/g/b/a, end_color_r/g/b/a, start_size, end_size, max_particles, seed)";

pub(crate) fn spawn_particle_emitter_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(PARTICLE_EMITTER_COMPONENT);
    let expected = PARTICLE_EMITTER_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;

    let spawn_rate = fields.f32("spawn_rate")?;
    let lifetime_min = fields.f32("lifetime_min")?;
    let lifetime_max = fields.f32("lifetime_max")?;
    let initial_speed_min = fields.f32("initial_speed_min")?;
    let initial_speed_max = fields.f32("initial_speed_max")?;
    let direction = Vec3::new(
        fields.f32("direction_x")?,
        fields.f32("direction_y")?,
        fields.f32("direction_z")?,
    );
    let spread = fields.f32("spread")?;
    let gravity = Vec3::new(
        fields.f32("gravity_x")?,
        fields.f32("gravity_y")?,
        fields.f32("gravity_z")?,
    );
    let start_color = [
        fields.f32("start_color_r")?,
        fields.f32("start_color_g")?,
        fields.f32("start_color_b")?,
        fields.f32("start_color_a")?,
    ];
    let end_color = [
        fields.f32("end_color_r")?,
        fields.f32("end_color_g")?,
        fields.f32("end_color_b")?,
        fields.f32("end_color_a")?,
    ];
    let start_size = fields.f32("start_size")?;
    let end_size = fields.f32("end_size")?;
    let max_particles = fields.i64("max_particles")?;
    let seed = fields.i64("seed")?;
    let mesh_asset = fields.asset_ref("mesh")?.clone();
    let material_asset = fields.asset_ref("material")?.clone();

    if spawn_rate < 0.0 {
        return Err(fields.invalid("a spawn_rate that is >= 0.0").into());
    }
    if lifetime_min <= 0.0 || lifetime_min > lifetime_max {
        return Err(fields
            .invalid("a lifetime_min that is > 0.0 and <= lifetime_max")
            .into());
    }
    if initial_speed_min > initial_speed_max {
        return Err(fields
            .invalid("an initial_speed_min that is <= initial_speed_max")
            .into());
    }
    if spread < 0.0 {
        return Err(fields.invalid("a spread that is >= 0.0").into());
    }
    if max_particles < 0
        || usize::try_from(max_particles)
            .is_ok_and(|count| count > crate::render_limits::MAX_PARTICLES_PER_EMITTER)
    {
        return Err(fields
            .invalid("a max_particles value within the supported renderer limit")
            .into());
    }
    if !(0..=i64::from(u32::MAX)).contains(&seed) {
        return Err(fields
            .invalid("a seed value between 0 and u32::MAX inclusive")
            .into());
    }

    let mesh_handle = resolve_mesh_handle(&mesh_asset, context);

    let mut emitter = crate::particles::ParticleEmitter::new(mesh_handle);
    emitter.spawn_rate = spawn_rate;
    emitter.lifetime = (lifetime_min, lifetime_max);
    emitter.initial_speed = (initial_speed_min, initial_speed_max);
    emitter.direction = direction;
    emitter.spread = spread;
    emitter.gravity = gravity;
    emitter.start_color = start_color;
    emitter.end_color = end_color;
    emitter.start_size = start_size;
    emitter.end_size = end_size;
    emitter.max_particles = max_particles as usize;
    emitter.seed = seed as u32;
    emitter.reset();

    let material = resolve_material_value(&material_asset, context);
    install_material(entity, material, context.world)?;
    context.world.add_component(entity, emitter)?;
    Ok(())
}

pub(crate) fn spawn_ui_document_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(UI_DOCUMENT_COMPONENT);
    let asset = component_asset_ref(context.authoring_entity, &component_type, value)?.clone();
    let document_ref = load_ui_document_ref(
        &asset,
        context.asset_root,
        context.manifest,
        context.asset_diagnostics,
    );
    context.world.add_component(entity, document_ref)?;
    Ok(())
}

fn load_ui_document_ref(
    asset: &AssetId,
    asset_root: Option<&Path>,
    manifest: &AssetManifest,
    diagnostics: &mut Vec<Diagnostic>,
) -> UiDocumentRef {
    if asset.as_str() == BUILTIN_UI_DOCUMENT_ASSET_ID {
        if let Some(diagnostic) = builtin_conflict_diagnostic(asset, manifest) {
            diagnostics.push(diagnostic);
        }
        return UiDocumentRef {
            asset: asset.clone(),
            document: builtin_ui_document(),
            source_path: None,
            modified: None,
        };
    }

    let Some(entry) = manifest.get(asset) else {
        diagnostics.push(
            Diagnostic::warning(
                "asset.unregistered_file",
                format!(
                    "asset `{}` is not registered in the manifest; using an empty UI document",
                    asset.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Asset { id: asset.clone() }),
        );
        return UiDocumentRef {
            asset: asset.clone(),
            document: UiDocument::default(),
            source_path: None,
            modified: None,
        };
    };

    let root = asset_root.unwrap_or_else(|| Path::new("."));
    let full_path = root.join(&entry.path);
    match load_ui_document(&full_path) {
        Ok(document) => {
            let modified = std::fs::metadata(&full_path)
                .and_then(|metadata| metadata.modified())
                .ok();
            UiDocumentRef {
                asset: asset.clone(),
                document,
                source_path: Some(full_path),
                modified,
            }
        }
        Err(source) => {
            diagnostics.push(
                Diagnostic::error(
                    "asset.missing_file",
                    format!("failed to load asset `{}`: {source}", asset.as_str()),
                )
                .with_target(DiagnosticTarget::Asset { id: asset.clone() }),
            );
            UiDocumentRef {
                asset: asset.clone(),
                document: UiDocument::default(),
                source_path: None,
                modified: None,
            }
        }
    }
}

fn builtin_ui_document() -> UiDocument {
    let mut document = UiDocument::default();
    document.root.children.push(UiNode {
        id: "text".to_string(),
        kind: UiNodeKind::Text {
            content: UiString::Literal("New UI".to_string()),
            size: 16.0,
            color: [1.0, 1.0, 1.0, 1.0],
        },
        children: Vec::new(),
    });
    document
}

const COLLIDER_EXPECTED: &str = "an object with a shape field (\"aabb\"|\"sphere\"|\"capsule_y\"), numeric half_extent_x/y/z, radius, half_height fields, a bool is_trigger field, and numeric membership/mask fields within 0..=u32::MAX";

pub(crate) fn spawn_collider_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(COLLIDER_COMPONENT);
    let expected = COLLIDER_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;

    let shape = fields.string("shape")?;
    let half_extent_x = fields.f32("half_extent_x")?;
    let half_extent_y = fields.f32("half_extent_y")?;
    let half_extent_z = fields.f32("half_extent_z")?;
    let radius = fields.f32("radius")?;
    let half_height = fields.f32("half_height")?;
    let is_trigger = fields.bool("is_trigger")?;
    let membership = fields.i64("membership")?;
    let mask = fields.i64("mask")?;

    let collider = match shape {
        "aabb" => {
            let extents = Vec3::new(half_extent_x, half_extent_y, half_extent_z);
            if !extents.is_finite() || extents.x <= 0.0 || extents.y <= 0.0 || extents.z <= 0.0 {
                return Err(fields
                    .invalid("positive finite half_extent_x/y/z for shape \"aabb\"")
                    .into());
            }
            Collider::aabb(extents)
        }
        "sphere" => {
            if !radius.is_finite() || radius <= 0.0 {
                return Err(fields
                    .invalid("a positive finite radius for shape \"sphere\"")
                    .into());
            }
            Collider::sphere(radius)
        }
        "capsule_y" => {
            if !radius.is_finite()
                || radius <= 0.0
                || !half_height.is_finite()
                || half_height <= 0.0
            {
                return Err(fields
                    .invalid("a positive finite radius and half_height for shape \"capsule_y\"")
                    .into());
            }
            Collider::capsule_y(half_height, radius)
        }
        _ => {
            return Err(fields
                .invalid("a shape of \"aabb\", \"sphere\", or \"capsule_y\"")
                .into())
        }
    };

    if !(0..=i64::from(u32::MAX)).contains(&membership)
        || !(0..=i64::from(u32::MAX)).contains(&mask)
    {
        return Err(fields
            .invalid("membership and mask values between 0 and u32::MAX inclusive")
            .into());
    }

    context.world.add_component(entity, collider)?;
    context.world.add_component(
        entity,
        CollisionLayers {
            membership: membership as u32,
            mask: mask as u32,
        },
    )?;
    if is_trigger {
        context.world.add_component(entity, TriggerVolume)?;
    }
    Ok(())
}

const PHYSICS_BODY_EXPECTED: &str =
    "an object with a kind field of \"static\", \"kinematic\", or \"dynamic\"";

pub(crate) fn spawn_physics_body_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(PHYSICS_BODY_COMPONENT);
    let expected = PHYSICS_BODY_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let kind = fields.string("kind")?;
    let body = match kind {
        "static" => PhysicsBody::Static,
        "kinematic" => PhysicsBody::Kinematic,
        "dynamic" => PhysicsBody::Dynamic,
        _ => return Err(fields.invalid(expected).into()),
    };
    context.world.add_component(entity, body)?;
    Ok(())
}

const CHARACTER_CONTROLLER_EXPECTED: &str =
    "an object with finite character motor settings, max_resolve_iterations between 1 and 16, and slope_limit_degrees between 0 and 89";

pub(crate) fn spawn_character_controller_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(CHARACTER_CONTROLLER_COMPONENT);
    let expected = CHARACTER_CONTROLLER_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let gravity_scale = fields.f32("gravity_scale")?;
    let max_resolve_iterations = fields.i64("max_resolve_iterations")?;
    let defaults = KinematicCharacterController::default();
    let slope_limit_degrees = fields.f32_or("slope_limit_degrees", defaults.slope_limit_degrees)?;
    let step_offset = fields.f32_or("step_offset", defaults.step_offset)?;
    let ground_snap_distance =
        fields.f32_or("ground_snap_distance", defaults.ground_snap_distance)?;
    let skin_width = fields.f32_or("skin_width", defaults.skin_width)?;

    if !gravity_scale.is_finite() {
        return Err(fields.invalid(expected).into());
    }
    if !(1..=16).contains(&max_resolve_iterations) {
        return Err(fields.invalid(expected).into());
    }
    if !(0.0..=89.0).contains(&slope_limit_degrees)
        || step_offset < 0.0
        || ground_snap_distance < 0.0
        || skin_width < 0.0
    {
        return Err(fields.invalid(expected).into());
    }

    context.world.add_component(
        entity,
        KinematicCharacterController {
            velocity: Vec3::ZERO,
            gravity_scale,
            grounded: false,
            max_resolve_iterations: max_resolve_iterations as u32,
            slope_limit_degrees,
            step_offset,
            ground_snap_distance,
            skin_width,
        },
    )?;
    Ok(())
}

pub(crate) fn spawn_damage_receiver_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(DAMAGE_RECEIVER_COMPONENT);
    let expected = "an object with finite max_health, health, invulnerability_seconds, and a signed 32-bit team";
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let max_health = fields.f32("max_health")?;
    let health = fields.f32("health")?;
    let invulnerability_seconds = fields.f32("invulnerability_seconds")?;
    let team = fields.i64("team")?;
    if max_health <= 0.0
        || health < 0.0
        || health > max_health
        || invulnerability_seconds < 0.0
        || i32::try_from(team).is_err()
    {
        return Err(fields.invalid(expected).into());
    }
    context.world.add_component(
        entity,
        DamageReceiver {
            team: team as i32,
            health,
            max_health,
            invulnerability_seconds,
            invulnerability_remaining: 0.0,
        },
    )?;
    Ok(())
}

const LOCK_ON_TARGET_EXPECTED: &str = "an object with a team field between 0 and u32::MAX";

pub(crate) fn spawn_lock_on_target_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(LOCK_ON_TARGET_COMPONENT);
    let expected = LOCK_ON_TARGET_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;
    let team = fields.i64("team")?;
    if !(0..=i64::from(u32::MAX)).contains(&team) {
        return Err(fields.invalid(expected).into());
    }
    context
        .world
        .add_component(entity, LockOnTarget { team: team as u32 })?;
    Ok(())
}

const LOCK_ON_CAMERA_EXPECTED: &str =
    "an object with lock-on camera fields including a source entity reference";

pub(crate) fn spawn_lock_on_camera_component(
    entity: Entity,
    value: &Value,
    context: &mut SpawnContext<'_>,
) -> Result<(), ComponentSpawnError> {
    let component_type = ComponentTypeId::new(LOCK_ON_CAMERA_COMPONENT);
    let expected = LOCK_ON_CAMERA_EXPECTED;
    let fields = ComponentFields::new(context.authoring_entity, &component_type, value, expected)?;

    let source_authoring_id = match fields.get("source") {
        Some(Value::EntityRef(id)) => id.clone(),
        _ => return Err(fields.invalid("a source entity reference").into()),
    };
    let Some(source_runtime) = context.entity_map.get(&source_authoring_id).copied() else {
        context.asset_diagnostics.push(Diagnostic::warning(
            "scene_bridge.lock_on_camera_unresolved_source",
            format!(
                "lock_on_camera on entity `{}` references unknown entity `{}`; camera skipped",
                context.authoring_entity.id.as_str(),
                source_authoring_id.as_str()
            ),
        ));
        return Ok(());
    };

    let distance = fields.f32("distance")?;
    let height = fields.f32("height")?;
    let spring_strength = fields.f32("spring_strength")?;
    let max_target_distance = fields.f32("max_target_distance")?;
    let require_line_of_sight = fields.bool("require_line_of_sight")?;
    let team_filter = fields.i64("team_filter")?;

    if !distance.is_finite() || distance <= 0.0 {
        return Err(fields.invalid("a positive finite distance").into());
    }
    if !height.is_finite() {
        return Err(fields.invalid("a finite height").into());
    }
    if !spring_strength.is_finite() || !(0.0..=1.0).contains(&spring_strength) {
        return Err(fields
            .invalid("a spring_strength between 0.0 and 1.0")
            .into());
    }
    if !max_target_distance.is_finite() || max_target_distance <= 0.0 {
        return Err(fields
            .invalid("a positive finite max_target_distance")
            .into());
    }
    if !(-1..=i64::from(u32::MAX)).contains(&team_filter) {
        return Err(fields
            .invalid("a team_filter between -1 and u32::MAX inclusive")
            .into());
    }

    context.world.add_component(
        entity,
        LockOnCamera::new(
            source_runtime,
            distance,
            height,
            spring_strength,
            max_target_distance,
            require_line_of_sight,
            team_filter,
        ),
    )?;
    Ok(())
}

pub(super) fn extract_transform_value(
    entity: &AuthoringEntity,
    transform_type: &ComponentTypeId,
    value: &Value,
) -> Result<Transform, SceneBridgeError> {
    let fields = ComponentFields::new(
        entity,
        transform_type,
        value,
        "an object with finite numeric position, optional Euler rotation, and optional scale fields",
    )?;
    let translation = Vec3::new(fields.f32("x")?, fields.f32("y")?, fields.f32("z")?);
    let rotation_degrees = Vec3::new(
        fields.f32_or("rotation_x_degrees", 0.0)?,
        fields.f32_or("rotation_y_degrees", 0.0)?,
        fields.f32_or("rotation_z_degrees", 0.0)?,
    );
    let scale = Vec3::new(
        fields.f32_or("scale_x", 1.0)?,
        fields.f32_or("scale_y", 1.0)?,
        fields.f32_or("scale_z", 1.0)?,
    );
    let radians = Vec3::new(
        rotation_degrees.x.to_radians(),
        rotation_degrees.y.to_radians(),
        rotation_degrees.z.to_radians(),
    );
    Ok(Transform {
        translation,
        rotation: Quat::from_euler(EulerRot::XYZ, radians.x, radians.y, radians.z),
        scale,
    })
}
