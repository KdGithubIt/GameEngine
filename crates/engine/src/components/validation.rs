//! Value-range, relation, and asset-reference validation for builtin components.

use super::*;

/// Validates registered built-in component shapes for editor Problems output.
///
/// Runtime conversion remains the semantic authority for range and dependency
/// checks. This lightweight pass catches missing fields, wrong value kinds,
/// and invalid enum strings before Play without constructing a runtime world.
pub fn validate_builtin_component_values(scene: &AuthoringScene) -> Vec<Diagnostic> {
    let registry = builtin_registry();
    let mut diagnostics = Vec::new();
    for (entity_id, entity) in scene.entities() {
        for (component_type, value) in &entity.components {
            let Some(definition) = registry.get(component_type) else {
                continue;
            };
            let target = DiagnosticTarget::Component {
                entity: entity_id.clone(),
                component_type: component_type.clone(),
            };
            if component_type.as_str() == ANIMATION_CONTROLLER_COMPONENT
                && matches!(
                    value,
                    Value::Object(fields)
                        if fields.get("root_motion_mode")
                            == Some(&Value::String("applied_to_motor".to_owned()))
                )
                && !entity
                    .components
                    .contains_key(&ComponentTypeId::new(CHARACTER_CONTROLLER_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.animation_controller` with applied root motion requires `engine.character_controller` on the same entity",
                    )
                    .with_target(target.clone()),
                );
            }
            // A controller drives the rig owned by the Skinned Model on its
            // own entity (ADR 0087 §3).
            if component_type.as_str() == ANIMATION_CONTROLLER_COMPONENT
                && !entity
                    .components
                    .contains_key(&ComponentTypeId::new(SKINNED_MODEL_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.animation_controller` requires `engine.skinned_model` on the same entity to own its rig",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == ANIMATION_CONTROLLER_COMPONENT
                && matches!(
                    value,
                    Value::Object(fields)
                        if matches!(fields.get("animation_set"), Some(Value::AssetRef(_)))
                            && !matches!(fields.get("graph"), Some(Value::AssetRef(_)))
                )
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.animation_controller.animation_set` requires `graph`; direct single-clip playback is not supported",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == STATIC_MESH_RENDERER_COMPONENT
                && entity
                    .components
                    .contains_key(&ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.mesh_renderer_conflict",
                        "Static Mesh Renderer and Skinned Mesh Renderer cannot coexist on one entity.",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == ANIMATION_CONTROLLER_COMPONENT
                && matches!(
                        value,
                        Value::Object(fields)
                        if matches!(fields.get("graph"), Some(Value::AssetRef(_)))
                            && !matches!(fields.get("animation_set"), Some(Value::AssetRef(_)))
                )
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.animation_controller.graph` requires `animation_set` so graph motion slots can be resolved",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == AUDIO_EMITTER_COMPONENT
                && !entity
                    .components
                    .contains_key(&ComponentTypeId::new(TRANSFORM_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.audio_emitter` requires `engine.transform` on the same entity",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == AUDIO_LISTENER_COMPONENT
                && !entity
                    .components
                    .contains_key(&ComponentTypeId::new(TRANSFORM_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.audio_listener` requires `engine.transform` on the same entity",
                    )
                    .with_target(target.clone()),
                );
            }
            if component_type.as_str() == FOOT_IK_COMPONENT
                && !entity
                    .components
                    .contains_key(&ComponentTypeId::new(ANIMATION_CONTROLLER_COMPONENT))
            {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_dependency_missing",
                        "`engine.foot_ik` requires `engine.animation_controller` on the same entity",
                    )
                    .with_target(target.clone()),
                );
            }
            if definition.schema.component_default.is_some() {
                if !matches!(value, Value::AssetRef(_)) {
                    diagnostics.push(
                        Diagnostic::error(
                            "scene.component_value_type",
                            format!("`{component_type}` expects an asset reference"),
                        )
                        .with_target(target),
                    );
                }
                continue;
            }
            let Value::Object(fields) = value else {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_value_type",
                        format!("`{component_type}` expects an object"),
                    )
                    .with_target(target),
                );
                continue;
            };
            if component_type.as_str() == SKINNED_MESH_RENDERER_COMPONENT
                && !matches!(
                    fields
                        .get("model")
                        .or_else(|| fields.get("rig_source"))
                        .or_else(|| fields.get("skeleton")),
                    Some(Value::EntityRef(_))
                )
            {
                diagnostics.push(
                    Diagnostic::warning(
                        "scene.component_reference_unassigned",
                        "`engine.skinned_mesh_renderer.model` is not assigned; the mesh stays in its bind pose until a Skinned Model is selected",
                    )
                    .with_target(target.clone()),
                );
            }
            for field in &definition.schema.fields {
                let Some(field_value) = fields.get(&field.name) else {
                    if field.required && field.default_value.is_none() {
                        // An unassigned asset reference is a normal editing
                        // state: the component converts to nothing until the
                        // user assigns it (ADR 0069). Other missing required
                        // fields cannot be produced by the editor and stay
                        // errors.
                        if matches!(field.field_type, FieldType::AssetRef) {
                            diagnostics.push(
                                Diagnostic::warning(
                                    "scene.component_reference_unassigned",
                                    format!(
                                        "`{component_type}.{}` is not assigned; the component stays inactive until it is",
                                        field.name
                                    ),
                                )
                                .with_target(target.clone()),
                            );
                        } else {
                            diagnostics.push(
                                Diagnostic::error(
                                    "scene.component_field_missing",
                                    format!(
                                        "`{component_type}.{}` is required but missing",
                                        field.name
                                    ),
                                )
                                .with_target(target.clone()),
                            );
                        }
                    }
                    continue;
                };
                if !schema_value_matches(field_value, &field.field_type) {
                    diagnostics.push(
                        Diagnostic::error(
                            "scene.component_field_type",
                            format!(
                                "`{component_type}.{}` has the wrong value type; expected {:?}",
                                field.name, field.field_type
                            ),
                        )
                        .with_target(target.clone()),
                    );
                }
            }
            if let InspectorHint::Fields { fields: hints } = definition.inspector {
                for hint in hints {
                    if hint
                        .visible_when
                        .is_some_and(|condition| !field_condition_matches(condition, fields))
                    {
                        continue;
                    }
                    match hint.control {
                        Some(InspectorFieldControl::Enum(options)) => {
                            let Some(Value::String(value)) = fields.get(hint.name) else {
                                continue;
                            };
                            if !options.contains(&value.as_str()) {
                                diagnostics.push(
                                    Diagnostic::error(
                                        "scene.component_enum_value",
                                        format!(
                                            "`{component_type}.{}` value `{value}` is not one of {}",
                                            hint.name,
                                            options.join(", ")
                                        ),
                                    )
                                    .with_target(target.clone()),
                                );
                            }
                        }
                        Some(InspectorFieldControl::Number(range)) => {
                            let Some(value) = fields.get(hint.name).and_then(value_as_f64) else {
                                continue;
                            };
                            if !range.contains(value) {
                                diagnostics.push(
                                    Diagnostic::error(
                                        "scene.component_number_range",
                                        format!(
                                            "`{component_type}.{}` {}; found {value}",
                                            hint.name,
                                            range.expectation()
                                        ),
                                    )
                                    .with_target(target.clone()),
                                );
                            }
                        }
                        Some(InspectorFieldControl::EntityRef(required)) => {
                            validate_entity_reference_target(
                                scene,
                                component_type,
                                hint.name,
                                fields.get(hint.name),
                                required,
                                &target,
                                &mut diagnostics,
                            );
                        }
                        Some(InspectorFieldControl::StringBoolMap) => {
                            validate_string_bool_map(
                                component_type,
                                hint.name,
                                fields.get(hint.name),
                                &target,
                                &mut diagnostics,
                            );
                        }
                        _ => {}
                    }
                }
            }
            validate_component_relations(component_type, fields, &target, &mut diagnostics);
        }
    }
    diagnostics.extend(validate_skinned_models_without_renderers(scene));
    diagnostics.extend(validate_spatial_audio_scene(scene));
    diagnostics.extend(crate::render_limits::validate_scene_render_limits(scene));
    diagnostics
}

fn validate_spatial_audio_scene(scene: &AuthoringScene) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let listener_type = ComponentTypeId::new(AUDIO_LISTENER_COMPONENT);
    let emitter_type = ComponentTypeId::new(AUDIO_EMITTER_COMPONENT);
    let mut enabled = Vec::new();

    for (entity_id, entity) in scene.entities() {
        let Some(Value::Object(fields)) = entity.components.get(&listener_type) else {
            continue;
        };
        if fields.get("enabled") != Some(&Value::Bool(true)) {
            continue;
        }
        let Some(priority) = fields.get("priority").and_then(|value| match value {
            Value::I64(value) => Some(*value),
            Value::U64(value) => i64::try_from(*value).ok(),
            _ => None,
        }) else {
            continue;
        };
        enabled.push((entity_id.clone(), priority));
    }

    let highest = enabled.iter().map(|(_, priority)| *priority).max();
    let tied_highest = highest
        .map(|priority| enabled.iter().filter(|(_, value)| *value == priority).count())
        .unwrap_or(0);
    if tied_highest > 1 {
        for (entity_id, priority) in &enabled {
            if Some(*priority) != highest {
                continue;
            }
            diagnostics.push(
                Diagnostic::warning(
                    "scene.audio_listener_priority_ambiguous",
                    "Multiple enabled Audio Listeners share the highest priority; runtime selection falls back to deterministic entity order.",
                )
                .with_target(DiagnosticTarget::Component {
                    entity: entity_id.clone(),
                    component_type: listener_type.clone(),
                }),
            );
        }
    }

    if enabled.is_empty() {
        for (entity_id, entity) in scene.entities() {
            let Some(Value::Object(fields)) = entity.components.get(&emitter_type) else {
                continue;
            };
            let spatial_blend = fields
                .get("spatial_blend")
                .and_then(value_as_f64)
                .unwrap_or(0.0);
            if spatial_blend <= 0.0 {
                continue;
            }
            diagnostics.push(
                Diagnostic::warning(
                    "scene.spatial_audio_listener_missing",
                    "This spatial Audio Emitter has no enabled Audio Listener; only its non-spatial mix is audible.",
                )
                .with_target(DiagnosticTarget::Component {
                    entity: entity_id.clone(),
                    component_type: emitter_type.clone(),
                }),
            );
        }
    }

    diagnostics
}

/// Reports a reference that points at an entity lacking the components the
/// field requires (ADR 0087 §4).
///
/// An unassigned reference is not reported here: ADR 0069 makes that an inert
/// editing state, reported as a warning elsewhere. Only a reference to a
/// wrong target is an error, because nothing downstream can act on it.
fn validate_entity_reference_target(
    scene: &AuthoringScene,
    component_type: &ComponentTypeId,
    field: &str,
    value: Option<&Value>,
    required: &[&str],
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(Value::EntityRef(referenced)) = value else {
        return;
    };
    let Some(entity) = scene.entity(referenced) else {
        // `scene.bad_entity_ref` already covers a reference to an entity that
        // is not in the document at all.
        return;
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|component| {
            !entity
                .components
                .contains_key(&ComponentTypeId::new(*component))
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    diagnostics.push(
        Diagnostic::error(
            "scene.entity_reference_wrong_target",
            format!(
                "`{}.{field}` points at entity `{}`, which has no {}",
                component_type.as_str(),
                referenced.as_str(),
                missing.join(", ")
            ),
        )
        .with_target(target.clone()),
    );
}

/// Reports Skinned Model ownership problems across the whole scene
/// (ADR 0087 §1, §6).
///
/// Ownership is a scene-wide relation, so it cannot be checked from one
/// component value: a part claimed twice and a part that does not exist are
/// both only visible with every model in hand.
fn validate_skinned_models_without_renderers(scene: &AuthoringScene) -> Vec<Diagnostic> {
    let model_type = ComponentTypeId::new(SKINNED_MODEL_COMPONENT);
    let renderer_type = ComponentTypeId::new(SKINNED_MESH_RENDERER_COMPONENT);
    let mut diagnostics = Vec::new();
    for (entity_id, entity) in scene.entities() {
        if !matches!(entity.components.get(&model_type), Some(Value::Object(_))) {
            continue;
        }
        let target = DiagnosticTarget::Component {
            entity: entity_id.clone(),
            component_type: model_type.clone(),
        };
        let has_renderer = scene.entities().any(|(_, candidate)| {
            let Some(Value::Object(fields)) = candidate.components.get(&renderer_type) else {
                return false;
            };
            matches!(
                fields
                    .get("model")
                    .or_else(|| fields.get("rig_source"))
                    .or_else(|| fields.get("skeleton")),
                Some(Value::EntityRef(model)) if model == entity_id
            )
        });
        if !has_renderer {
            diagnostics.push(
                Diagnostic::warning(
                    "scene.model_without_renderers",
                    "This Skinned Model draws nothing: no Skinned Mesh Renderer references it.",
                )
                .with_target(target),
            );
        }
    }
    diagnostics
}

fn field_condition_matches(
    condition: InspectorFieldCondition,
    fields: &BTreeMap<String, Value>,
) -> bool {
    match condition {
        InspectorFieldCondition::Bool { field, equals } => {
            fields.get(field) == Some(&Value::Bool(equals))
        }
        InspectorFieldCondition::String { field, equals } => {
            fields.get(field) == Some(&Value::String(equals.to_owned()))
        }
        InspectorFieldCondition::Assigned { field } => {
            !matches!(fields.get(field), None | Some(Value::Null))
        }
        InspectorFieldCondition::StringAny { field, values } => fields
            .get(field)
            .and_then(|value| match value {
                Value::String(value) => Some(value.as_str()),
                _ => None,
            })
            .is_some_and(|value| values.contains(&value)),
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::F64(value) => Some(*value),
        Value::I64(value) => Some(*value as f64),
        Value::U64(value) => Some(*value as f64),
        _ => None,
    }
}

fn validate_string_bool_map(
    component_type: &ComponentTypeId,
    field: &str,
    value: Option<&Value>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(Value::Object(entries)) = value else {
        return;
    };
    for (name, value) in entries {
        if name.trim().is_empty() || !matches!(value, Value::Bool(_)) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.animation_parameter_invalid",
                    format!(
                        "`{component_type}.{field}` parameter names must be non-empty and every value must be boolean"
                    ),
                )
                .with_target(target.clone()),
            );
            break;
        }
    }
}

fn validate_component_relations(
    component_type: &ComponentTypeId,
    fields: &BTreeMap<String, Value>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match component_type.as_str() {
        CAMERA_COMPONENT => push_order_diagnostic(
            component_type,
            fields,
            "near",
            "far",
            true,
            "the far plane must be greater than the near plane",
            target,
            diagnostics,
        ),
        PARTICLE_EMITTER_COMPONENT => {
            push_order_diagnostic(
                component_type,
                fields,
                "lifetime_min",
                "lifetime_max",
                false,
                "lifetime_max must be at least lifetime_min",
                target,
                diagnostics,
            );
            push_order_diagnostic(
                component_type,
                fields,
                "initial_speed_min",
                "initial_speed_max",
                false,
                "initial_speed_max must be at least initial_speed_min",
                target,
                diagnostics,
            );
        }
        AUDIO_EMITTER_COMPONENT => push_order_diagnostic(
            component_type,
            fields,
            "min_distance",
            "max_distance",
            false,
            "max_distance must be at least min_distance",
            target,
            diagnostics,
        ),
        SHADOW_SETTINGS_COMPONENT => push_order_diagnostic(
            component_type,
            fields,
            "cascade_near_split",
            "cascade_far_split",
            true,
            "cascade_far_split must be greater than cascade_near_split",
            target,
            diagnostics,
        ),
        RUNTIME_METADATA_COMPONENT => {
            if fields.get("tags").is_some_and(|tags| match tags {
                Value::Array(tags) => tags.iter().any(|tag| match tag {
                    Value::String(tag) => tag.trim().is_empty(),
                    _ => false,
                }),
                _ => false,
            }) {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.component_empty_tag",
                        "`engine.runtime_metadata.tags` cannot contain blank tags",
                    )
                    .with_target(target.clone()),
                );
            }
        }
        LOD_GROUP_COMPONENT => validate_lod_levels(fields, target, diagnostics),
        _ => {}
    }
}

fn validate_lod_levels(
    fields: &BTreeMap<String, Value>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(Value::Array(levels)) = fields.get("levels") else {
        return;
    };
    if levels.is_empty() {
        diagnostics.push(
            Diagnostic::error(
                "scene.lod_levels_empty",
                "`engine.lod_group.levels` must contain at least one level",
            )
            .with_target(target.clone()),
        );
        return;
    }
    let mut previous = None;
    for (index, level) in levels.iter().enumerate() {
        let Value::Object(object) = level else {
            continue;
        };
        let Some(distance) = object.get("distance").and_then(value_as_f64) else {
            diagnostics.push(
                Diagnostic::error(
                    "scene.lod_distance_missing",
                    format!("LOD level {index} requires a numeric `distance`"),
                )
                .with_target(target.clone()),
            );
            continue;
        };
        if !distance.is_finite() || distance <= 0.0 {
            diagnostics.push(
                Diagnostic::error(
                    "scene.lod_distance_invalid",
                    format!("LOD level {index} distance must be finite and greater than zero"),
                )
                .with_target(target.clone()),
            );
        }
        if previous.is_some_and(|previous| distance <= previous) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.lod_distance_order",
                    format!("LOD level {index} distance must be greater than the preceding level"),
                )
                .with_target(target.clone()),
            );
        }
        previous = Some(distance);
        if !matches!(object.get("mesh"), Some(Value::AssetRef(_))) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.lod_mesh_missing",
                    format!("LOD level {index} requires a mesh asset reference"),
                )
                .with_target(target.clone()),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_order_diagnostic(
    component_type: &ComponentTypeId,
    fields: &BTreeMap<String, Value>,
    lower: &str,
    upper: &str,
    strict: bool,
    message: &str,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if violates_order(fields, lower, upper, strict) {
        diagnostics.push(
            Diagnostic::error(
                "scene.component_field_relation",
                format!("`{component_type}.{lower}` and `{upper}` are invalid: {message}"),
            )
            .with_target(target.clone()),
        );
    }
}

/// Returns `true` only when two present numeric fields violate ascending order.
fn violates_order(
    fields: &BTreeMap<String, Value>,
    lower: &str,
    upper: &str,
    strict: bool,
) -> bool {
    fields
        .get(lower)
        .and_then(value_as_f64)
        .zip(fields.get(upper).and_then(value_as_f64))
        .is_some_and(|(lower, upper)| lower > upper || (strict && lower == upper))
}

/// Validates built-in component asset references against the open project.
///
/// This pass is intended for editor Problems output before runtime conversion.
/// It checks manifest membership, category compatibility, source existence,
/// and the persisted graph domain where a suffix cannot distinguish graph types.
pub fn validate_builtin_component_assets(
    scene: &AuthoringScene,
    manifest: &AssetManifest,
    asset_root: Option<&Path>,
) -> Vec<Diagnostic> {
    let registry = builtin_registry();
    let mut diagnostics = Vec::new();
    for (entity_id, entity) in scene.entities() {
        for (component_type, value) in &entity.components {
            let Some(definition) = registry.get(component_type) else {
                continue;
            };
            let target = DiagnosticTarget::Component {
                entity: entity_id.clone(),
                component_type: component_type.clone(),
            };
            if let InspectorHint::AssetRef { kind } = definition.inspector
                && let Value::AssetRef(asset) = value {
                    validate_component_asset(
                        asset,
                        kind,
                        manifest,
                        asset_root,
                        &target,
                        &mut diagnostics,
                    );
                }
            let (InspectorHint::Fields { fields: hints }, Value::Object(fields)) =
                (definition.inspector, value)
            else {
                continue;
            };
            for hint in hints {
                if matches!(hint.control, Some(InspectorFieldControl::LodLevels)) {
                    if let Some(Value::Array(levels)) = fields.get(hint.name) {
                        for level in levels {
                            if let Value::Object(level) = level
                                && let Some(Value::AssetRef(asset)) = level.get("mesh") {
                                    validate_component_asset(
                                        asset,
                                        AssetKind::Mesh,
                                        manifest,
                                        asset_root,
                                        &target,
                                        &mut diagnostics,
                                    );
                                }
                        }
                    }
                    continue;
                }
                if let Some(InspectorFieldControl::AssetRefList(kind)) = hint.control {
                    if let Some(Value::Array(assets)) = fields.get(hint.name) {
                        for asset in assets {
                            if let Value::AssetRef(asset) = asset {
                                validate_component_asset(
                                    asset,
                                    kind,
                                    manifest,
                                    asset_root,
                                    &target,
                                    &mut diagnostics,
                                );
                            }
                        }
                    }
                    continue;
                }
                let Some(InspectorFieldControl::AssetRef(kind)) = hint.control else {
                    continue;
                };
                let Some(Value::AssetRef(asset)) = fields.get(hint.name) else {
                    continue;
                };
                validate_component_asset(
                    asset,
                    kind,
                    manifest,
                    asset_root,
                    &target,
                    &mut diagnostics,
                );
            }
            if component_type.as_str() == ANIMATION_CONTROLLER_COMPONENT {
                validate_animation_controller_bindings(
                    fields,
                    manifest,
                    asset_root,
                    &target,
                    &mut diagnostics,
                );
            }
        }
    }
    diagnostics
}

/// Validates built-in asset references without touching the filesystem.
///
/// This pass is safe for the synchronous Inspector edit path. It reports
/// manifest membership and category errors immediately while leaving source
/// files, material documents, and imported dependency stamps to
/// [`validate_builtin_component_asset_files`].
pub fn validate_builtin_component_asset_references(
    scene: &AuthoringScene,
    manifest: &AssetManifest,
) -> Vec<Diagnostic> {
    validate_builtin_component_assets(scene, manifest, None)
}

/// Runs only the filesystem-backed half of built-in asset validation.
///
/// The returned diagnostics exclude the pure in-memory diagnostics produced
/// by [`validate_builtin_component_asset_references`], allowing an editor to
/// publish the two passes at different times without duplicate rows.
pub fn validate_builtin_component_asset_files(
    scene: &AuthoringScene,
    manifest: &AssetManifest,
    asset_root: &Path,
) -> Vec<Diagnostic> {
    let inline = validate_builtin_component_asset_references(scene, manifest);
    let mut complete = validate_builtin_component_assets(scene, manifest, Some(asset_root));
    complete.retain(|diagnostic| !inline.contains(diagnostic));
    complete
}

fn validate_animation_controller_bindings(
    fields: &BTreeMap<String, Value>,
    manifest: &AssetManifest,
    asset_root: Option<&Path>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (
        Some(Value::AssetRef(graph_id)),
        Some(Value::AssetRef(animation_set_id)),
        Some(asset_root),
    ) = (fields.get("graph"), fields.get("animation_set"), asset_root)
    else {
        return;
    };
    let (Some(graph_entry), Some(animation_set_entry)) =
        (manifest.get(graph_id), manifest.get(animation_set_id))
    else {
        return;
    };
    let animation_set_path = asset_root.join(&animation_set_entry.path);
    let animation_set = std::fs::read_to_string(&animation_set_path)
        .ok()
        .and_then(|json| engine_authoring::AnimationSet::from_json(&json).ok());
    let Some(animation_set) = animation_set else {
        return;
    };
    if animation_set.graph.as_ref() != Some(graph_id) {
        let target_graph = animation_set
            .graph
            .as_ref()
            .map(AssetId::as_str)
            .unwrap_or("(unassigned)");
        diagnostics.push(
            Diagnostic::error(
                "scene.animation_set_graph_mismatch",
                format!(
                    "Animation Set `{}` targets graph `{}`, but the controller selects `{}`",
                    animation_set_id.as_str(),
                    target_graph,
                    graph_id.as_str()
                ),
            )
            .with_target(target.clone()),
        );
        return;
    }

    let graph_path = asset_root.join(&graph_entry.path);
    let Ok(graph) = crate::anim_graph::load_animation_graph(&graph_path) else {
        return;
    };
    for state in &graph.states {
        let Some(slot) = &state.motion_slot else {
            continue;
        };
        if !animation_set.bindings.contains_key(slot) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.animation_set_slot_unbound",
                    format!(
                        "Animation Set `{}` does not bind graph motion slot `{}`",
                        animation_set_id.as_str(),
                        slot.as_str()
                    ),
                )
                .with_target(target.clone()),
            );
        }
    }
}

fn validate_component_asset(
    asset: &AssetId,
    kind: AssetKind,
    manifest: &AssetManifest,
    asset_root: Option<&Path>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if builtin_asset_matches_kind(asset, kind) {
        return;
    }
    let derived = manifest.imported_sub_asset(asset);
    let entry = manifest
        .get(asset)
        .or_else(|| derived.map(|(_, entry, _)| entry));
    let Some(entry) = entry else {
        diagnostics.push(
            Diagnostic::error(
                "scene.asset_unregistered",
                format!(
                    "asset `{}` is not registered in the project manifest",
                    asset.as_str()
                ),
            )
            .with_target(target.clone()),
        );
        return;
    };
    if let Some((_, source_entry, sub_asset)) = derived {
        if !imported_sub_asset_matches_kind(sub_asset.kind, kind) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.asset_category_mismatch",
                    format!(
                        "imported asset `{}` is {:?}, which is not compatible with {kind:?}",
                        asset.as_str(),
                        sub_asset.kind
                    ),
                )
                .with_target(target.clone()),
            );
            return;
        }
        validate_import_source_files(asset, source_entry, asset_root, target, diagnostics);
        return;
    }
    let relative_path = Path::new(&entry.path);
    if !asset_path_matches_kind(kind, relative_path) {
        diagnostics.push(
            Diagnostic::error(
                "scene.asset_category_mismatch",
                format!(
                    "asset `{}` path `{}` is not compatible with {kind:?}",
                    asset.as_str(),
                    entry.path
                ),
            )
            .with_target(target.clone()),
        );
        return;
    }
    let Some(asset_root) = asset_root else {
        return;
    };
    let full_path = asset_root.join(relative_path);
    if !full_path.is_file() {
        diagnostics.push(
            Diagnostic::error(
                "scene.asset_missing_file",
                format!(
                    "asset `{}` source file `{}` does not exist",
                    asset.as_str(),
                    full_path.display()
                ),
            )
            .with_target(target.clone()),
        );
        return;
    }
    if kind == AssetKind::Material {
        validate_material_dependencies(
            asset,
            &full_path,
            manifest,
            asset_root,
            target,
            diagnostics,
        );
        return;
    }
    if kind == AssetKind::AnimationSet {
        validate_animation_set_dependencies(
            asset,
            &full_path,
            manifest,
            asset_root,
            target,
            diagnostics,
        );
        return;
    }
    if kind == AssetKind::GltfSource {
        validate_import_source_files(asset, entry, Some(asset_root), target, diagnostics);
        if entry.import_settings.source_fingerprint.is_none() {
            diagnostics.push(
                Diagnostic::error(
                    "scene.gltf_source_not_imported",
                    format!(
                        "glTF/GLB source `{}` has no successful import catalog; use Reimport before Play",
                        full_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
        }
        return;
    }
    if kind == AssetKind::MotionSource {
        validate_motion_source(entry, &full_path, manifest, target, diagnostics);
        return;
    }
    let Some(expected_graph_kind) = expected_graph_kind(kind) else {
        return;
    };
    let actual_kind = std::fs::read_to_string(&full_path)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|document| document.get("kind")?.as_str().map(str::to_owned));
    if actual_kind.as_deref() != Some(expected_graph_kind) {
        diagnostics.push(
            Diagnostic::error(
                "scene.asset_graph_kind_mismatch",
                format!(
                    "asset `{}` must contain graph kind `{expected_graph_kind}`, found {}",
                    asset.as_str(),
                    actual_kind
                        .as_deref()
                        .unwrap_or("unreadable or missing kind")
                ),
            )
            .with_target(target.clone()),
        );
    }
}

fn imported_sub_asset_matches_kind(kind: ImportedSubAssetKind, expected: AssetKind) -> bool {
    matches!(
        (kind, expected),
        (ImportedSubAssetKind::Mesh, AssetKind::Mesh)
            | (ImportedSubAssetKind::Material, AssetKind::Material)
            | (ImportedSubAssetKind::Texture, AssetKind::Texture)
            | (ImportedSubAssetKind::Animation, AssetKind::AnimationClip)
            | (ImportedSubAssetKind::Skin, AssetKind::Skin)
            // Skinned ModelはSkinではなくSkeletonサブアセットを参照する。
            // インポート結果とInspector要求型がともにSkeletonである場合は、
            // 正常な同種参照なのでカテゴリー互換として受け入れる。
            | (ImportedSubAssetKind::Skeleton, AssetKind::Skeleton)
            | (ImportedSubAssetKind::Morph, AssetKind::Morph)
            | (ImportedSubAssetKind::SecondaryMotionRig, AssetKind::SecondaryMotionRig)
    )
}

fn validate_animation_set_dependencies(
    animation_set_id: &AssetId,
    animation_set_path: &Path,
    manifest: &AssetManifest,
    asset_root: &Path,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let animation_set = std::fs::read_to_string(animation_set_path)
        .map_err(|error| error.to_string())
        .and_then(|json| {
            engine_authoring::AnimationSet::from_json(&json).map_err(|error| error.to_string())
        });
    let animation_set = match animation_set {
        Ok(animation_set) => animation_set,
        Err(error) => {
            diagnostics.push(
                Diagnostic::error(
                    "scene.animation_set_invalid",
                    format!(
                        "animation set `{}` could not be loaded from `{}`: {error}",
                        animation_set_id.as_str(),
                        animation_set_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
            return;
        }
    };

    if let Some(graph) = &animation_set.graph {
        validate_component_asset(
            graph,
            AssetKind::AnimationGraph,
            manifest,
            Some(asset_root),
            target,
            diagnostics,
        );
    }
    for binding in animation_set.bindings.values() {
        for clip in std::iter::once(&binding.clip).chain(&binding.overlays) {
            if !matches!(
                manifest.imported_sub_asset(clip),
                Some((_, _, sub_asset)) if sub_asset.kind == ImportedSubAssetKind::Animation
            ) {
                diagnostics.push(
                    Diagnostic::error(
                        "scene.animation_set_clip_not_sub_asset",
                        format!(
                            "animation set `{}` binding `{}` must reference imported Animation Clip sub-assets for its primary clip and overlays",
                            animation_set_id.as_str(),
                            binding.name
                        ),
                    )
                    .with_target(target.clone()),
                );
                continue;
            }
            validate_component_asset(
                clip,
                AssetKind::AnimationClip,
                manifest,
                Some(asset_root),
                target,
                diagnostics,
            );
        }
    }
}

fn validate_import_source_files(
    asset: &AssetId,
    source: &ManifestEntry,
    asset_root: Option<&Path>,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(asset_root) = asset_root else {
        return;
    };
    let source_path = asset_root.join(&source.path);
    let dependency_paths = source
        .import_settings
        .source_dependencies
        .iter()
        .map(|relative| asset_root.join(relative))
        .collect::<Vec<_>>();
    let mut all_present = true;
    for full_path in std::iter::once(&source_path).chain(dependency_paths.iter()) {
        if !full_path.is_file() {
            all_present = false;
            diagnostics.push(
                Diagnostic::error(
                    "scene.import_dependency_missing",
                    format!(
                        "imported asset `{}` requires missing source file `{}`",
                        asset.as_str(),
                        full_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
        }
    }
    let Some(expected_stamp) = &source.import_settings.source_stamp else {
        return;
    };
    if !all_present {
        return;
    }
    match crate::asset::SourceStamp::capture(&source_path, &dependency_paths) {
        Ok(actual) if &actual != expected_stamp => diagnostics.push(
            Diagnostic::warning(
                "scene.import_source_changed",
                format!(
                    "source for imported asset `{}` changed after its last successful import; use Reimport before Play or Package",
                    asset.as_str()
                ),
            )
            .with_target(target.clone()),
        ),
        Ok(_) => {}
        Err(error) => diagnostics.push(
            Diagnostic::error(
                "scene.import_fingerprint_failed",
                format!(
                    "could not inspect source metadata for imported asset `{}`: {error}",
                    asset.as_str()
                ),
            )
            .with_target(target.clone()),
        ),
    }
}

/// Validates a `*.vmd` motion source's pairing with the PMX model it is baked
/// against (ADR 0097 §3).
///
/// Unlike every other source kind, a motion cannot be imported from its own
/// file alone, so an unset or dangling pairing is the failure authors will
/// hit most and is reported specifically rather than as a generic
/// "not imported" error.
fn validate_motion_source(
    entry: &ManifestEntry,
    full_path: &Path,
    manifest: &AssetManifest,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Inspecting raw VMD contents belongs to the desktop authoring importer.
    // Packaged and wasm32 builds still validate the persisted pairing and
    // import catalog below without linking the desktop-only parser.
    #[cfg(all(feature = "mmd-import", not(target_arch = "wasm32")))]
    {
        match crate::vmd_import::classify_vmd_path(full_path) {
            Ok(crate::vmd_import::VmdContentKind::Scene) => {
                diagnostics.push(
                    Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        format!(
                            "VMD `{}` contains scene-level camera, light, or self-shadow motion; it is not paired with a PMX model and scene motion playback is not implemented",
                            full_path.display()
                        ),
                    )
                    .with_target(target.clone()),
                );
                return;
            }
            Ok(crate::vmd_import::VmdContentKind::Empty) => {
                diagnostics.push(
                    Diagnostic::error(
                        "vmd.empty_motion",
                        format!("VMD `{}` contains no animation keys", full_path.display()),
                    )
                    .with_target(target.clone()),
                );
                return;
            }
            Ok(
                crate::vmd_import::VmdContentKind::Model
                | crate::vmd_import::VmdContentKind::Mixed,
            ) => {}
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect VMD `{}`: {error}", full_path.display()),
                    )
                    .with_target(target.clone()),
                );
                return;
            }
        }
    }
    let model_sources = entry.import_settings.resolved_motion_model_sources();
    if model_sources.is_empty() {
        diagnostics.push(
            Diagnostic::error(
                "scene.motion_source_unpaired",
                format!(
                    "motion source `{}` is not paired with a PMX model; choose one in Import Settings so its clips can be baked",
                    full_path.display()
                ),
            )
            .with_target(target.clone()),
        );
        return;
    }
    if let Some(original_source) = entry
        .import_settings
        .motion_original_model_source
        .as_deref()
    {
        let original_entry =
            AssetId::from_stable_id(engine_authoring::StableId::new(original_source))
                .ok()
                .and_then(|id| manifest.get(&id));
        let Some(original_entry) = original_entry else {
            diagnostics.push(
                Diagnostic::error(
                    "scene.motion_original_model_unregistered",
                    format!(
                        "motion source `{}` selects original model `{original_source}`, which is not registered in the project manifest",
                        full_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
            return;
        };
        if !Path::new(&original_entry.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        {
            diagnostics.push(
                Diagnostic::error(
                    "scene.motion_original_model_not_pmx",
                    format!(
                        "motion source `{}` selects original model `{}`, which is not a .pmx model and carries no MMD rig",
                        full_path.display(),
                        original_entry.path
                    ),
                )
                .with_target(target.clone()),
            );
        }
    }
    for model_source in model_sources {
        let model_entry = AssetId::from_stable_id(engine_authoring::StableId::new(model_source))
            .ok()
            .and_then(|id| manifest.get(&id));
        let Some(model_entry) = model_entry else {
            diagnostics.push(
                Diagnostic::error(
                    "scene.motion_model_unregistered",
                    format!(
                        "motion source `{}` targets model `{model_source}`, which is not registered in the project manifest",
                        full_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
            continue;
        };
        if !asset_path_matches_kind(AssetKind::GltfSource, Path::new(&model_entry.path))
            || !Path::new(&model_entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        {
            diagnostics.push(
                Diagnostic::error(
                    "scene.motion_model_not_pmx",
                    format!(
                        "motion source `{}` targets `{}`, which is not a .pmx model and carries no MMD rig",
                        full_path.display(),
                        model_entry.path
                    ),
                )
                .with_target(target.clone()),
            );
        }
    }
    if entry.import_settings.source_fingerprint.is_none() {
        diagnostics.push(
            Diagnostic::error(
                "scene.motion_source_not_imported",
                format!(
                    "motion source `{}` has no successful import catalog; use Reimport before Play",
                    full_path.display()
                ),
            )
            .with_target(target.clone()),
        );
    }
}

fn validate_material_dependencies(
    material_id: &AssetId,
    material_path: &Path,
    manifest: &AssetManifest,
    asset_root: &Path,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let material = std::fs::read_to_string(material_path)
        .map_err(|error| error.to_string())
        .and_then(|json| {
            engine_authoring::MaterialAsset::from_json(&json).map_err(|error| error.to_string())
        });
    let material = match material {
        Ok(material) => material,
        Err(error) => {
            diagnostics.push(
                Diagnostic::error(
                    "scene.material_invalid",
                    format!(
                        "material `{}` could not be loaded from `{}`: {error}",
                        material_id.as_str(),
                        material_path.display()
                    ),
                )
                .with_target(target.clone()),
            );
            return;
        }
    };

    for (slot, texture_id) in [
        ("base_color_texture", material.base_color_texture.as_ref()),
        ("normal_texture", material.normal_texture.as_ref()),
        (
            "metallic_roughness_texture",
            material.metallic_roughness_texture.as_ref(),
        ),
        (
            "occlusion_texture",
            material.occlusion_texture.as_ref(),
        ),
        ("emissive_texture", material.emissive_texture.as_ref()),
    ] {
        let Some(texture_id) = texture_id else {
            continue;
        };
        let Some(entry) = manifest.get(texture_id) else {
            diagnostics.push(
                Diagnostic::error(
                    "scene.material_texture_unregistered",
                    format!(
                        "material `{}` slot `{slot}` references unregistered texture `{}`",
                        material_id.as_str(),
                        texture_id.as_str()
                    ),
                )
                .with_target(target.clone()),
            );
            continue;
        };
        let relative_path = Path::new(&entry.path);
        if !asset_path_matches_kind(AssetKind::Texture, relative_path) {
            diagnostics.push(
                Diagnostic::error(
                    "scene.material_texture_category_mismatch",
                    format!(
                        "material `{}` slot `{slot}` asset `{}` is not a PNG, JPEG, WebP, or BMP texture",
                        material_id.as_str(),
                        texture_id.as_str()
                    ),
                )
                .with_target(target.clone()),
            );
            continue;
        }
        let texture_path = asset_root.join(relative_path);
        let decoded = std::fs::read(&texture_path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| image::load_from_memory(&bytes).map_err(|error| error.to_string()));
        match decoded {
            Ok(image)
                if image.width() > crate::render_limits::MAX_TEXTURE_DIMENSION
                    || image.height() > crate::render_limits::MAX_TEXTURE_DIMENSION =>
            {
                diagnostics.push(
                    Diagnostic::error(
                        "renderer.texture_dimension_limit",
                        format!(
                            "material `{}` slot `{slot}` texture `{}` is {}x{}; the supported maximum dimension is {}",
                            material_id.as_str(),
                            texture_path.display(),
                            image.width(),
                            image.height(),
                            crate::render_limits::MAX_TEXTURE_DIMENSION
                        ),
                    )
                    .with_target(target.clone()),
                );
            }
            Err(error) => diagnostics.push(
                Diagnostic::error(
                    "scene.material_texture_invalid",
                    format!(
                        "material `{}` slot `{slot}` texture `{}` failed to decode: {error}",
                        material_id.as_str(),
                        texture_path.display()
                    ),
                )
                .with_target(target.clone()),
            ),
            Ok(_) => {}
        }
    }
}

fn builtin_asset_matches_kind(asset: &AssetId, kind: AssetKind) -> bool {
    matches!(
        (asset.as_str(), kind),
        (
            BUILTIN_TRIANGLE_ASSET_ID | BUILTIN_QUAD_ASSET_ID,
            AssetKind::Mesh
        ) | (
            BUILTIN_WHITE_MATERIAL_ASSET_ID
                | BUILTIN_BLUE_MATERIAL_ASSET_ID
                | BUILTIN_ORANGE_MATERIAL_ASSET_ID,
            AssetKind::Material
        ) | (BUILTIN_UI_DOCUMENT_ASSET_ID, AssetKind::UiDocument)
    )
}

fn expected_graph_kind(kind: AssetKind) -> Option<&'static str> {
    match kind {
        AssetKind::AnimationGraph => Some("anim.graph"),
        AssetKind::BehaviorTree => Some("behavior_tree.graph"),
        _ => None,
    }
}

fn schema_value_matches(value: &Value, field_type: &FieldType) -> bool {
    match (value, field_type) {
        (Value::Bool(_), FieldType::Bool)
        | (Value::I64(_), FieldType::I64)
        | (Value::U64(_), FieldType::U64)
        | (Value::String(_), FieldType::String)
        | (Value::EntityRef(_), FieldType::EntityRef)
        | (Value::AssetRef(_), FieldType::AssetRef)
        | (Value::Object(_), FieldType::Object)
        | (Value::Object(_), FieldType::Vec2)
        | (Value::Object(_), FieldType::Vec3) => true,
        (Value::F64(_) | Value::I64(_) | Value::U64(_), FieldType::F64) => true,
        (Value::Array(values), FieldType::Array(element)) => values
            .iter()
            .all(|value| schema_value_matches(value, element)),
        _ => false,
    }
}

#[cfg(test)]
mod spatial_audio_tests {
    use super::*;
    use engine_authoring::{AuthoringCommand, Transaction};

    fn add_component(
        scene: &mut AuthoringScene,
        component_type: ComponentTypeId,
        value: Value,
    ) -> EntityId {
        let entity = EntityId::generate();
        let mut transaction = Transaction::begin(scene);
        transaction.apply(AuthoringCommand::CreateEntity {
            id: entity.clone(),
            name: "audio".to_owned(),
            parent: None,
        });
        transaction.apply(AuthoringCommand::AddComponent {
            entity: entity.clone(),
            component_type,
            value,
        });
        transaction
            .commit(scene)
            .expect("spatial audio validation fixture must commit");
        entity
    }

    #[test]
    fn equal_highest_listener_priorities_are_reported_as_ambiguous() {
        let mut scene = AuthoringScene::new();
        let listener = ComponentTypeId::new(AUDIO_LISTENER_COMPONENT);
        let value = || Value::Object(BTreeMap::from([
            ("enabled".to_owned(), Value::Bool(true)),
            ("priority".to_owned(), Value::I64(5)),
        ]));
        add_component(&mut scene, listener.clone(), value());
        add_component(&mut scene, listener, value());

        let diagnostics = validate_spatial_audio_scene(&scene);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "scene.audio_listener_priority_ambiguous")
                .count(),
            2
        );
    }

    #[test]
    fn spatial_emitter_without_enabled_listener_is_reported() {
        let mut scene = AuthoringScene::new();
        add_component(
            &mut scene,
            ComponentTypeId::new(AUDIO_EMITTER_COMPONENT),
            Value::Object(BTreeMap::from([
                ("spatial_blend".to_owned(), Value::F64(1.0)),
            ])),
        );

        let diagnostics = validate_spatial_audio_scene(&scene);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "scene.spatial_audio_listener_missing"
        }));
    }
}
