//! Schema-driven component editing: the context every control reads, the
//! dispatch from a component or field schema to its control, and the shared
//! field validation helpers.

use crate::ui::*;

pub(in crate::ui) struct InspectorEditContext<'a> {
    pub(in crate::ui) manifest: &'a engine::AssetManifest,
    pub(in crate::ui) assets_root: Option<&'a Path>,
    pub(in crate::ui) entity_choices: &'a [EntityChoice],
    /// Bones of the rig the edited component references, as `(BoneId, name)`.
    ///
    /// Empty when the component references no rig or the rig's skeleton
    /// cannot be read; the control then shows the stored ID so a binding
    /// stays visible and repairable (ADR 0088 1).
    pub(in crate::ui) bone_choices: &'a [(u32, String)],
    pub(in crate::ui) project_layers: &'a [engine_authoring::project_settings::Layer],
    /// Presentation-only navigation emitted by a reference unit.
    ///
    /// Interior mutability lets deeply nested schema controls request
    /// navigation while retaining an immutable shared Inspector context.
    pub(in crate::ui) reference_navigation: &'a std::cell::RefCell<Option<InspectorReferenceNavigation>>,
}

/// Shows the editor for one component value.
///
/// Asset reference components with known built-in assets get a whole-value
/// picker; every other value falls through to field-level property editing.
pub(in crate::ui) fn show_component_value_editor(
    ui: &mut egui::Ui,
    component_type: &ComponentTypeId,
    context: &InspectorEditContext<'_>,
    value: &mut Value,
    animation_asset_action: &mut Option<AnimationControllerAssetAction>,
) -> Option<ComponentEdit> {
    let registry = engine::builtin_registry();
    let Some(definition) = registry.get(component_type) else {
        return show_property_value_editor(ui, value);
    };
    if let engine::InspectorHint::AssetRef { kind } = definition.inspector {
        // Bare AssetRef components use the same visual reference unit as
        // schema fields. Convert its property-shaped result back into the
        // whole-component edit expected by the session command boundary.
        let mut ignored_clear = false;
        return show_asset_reference_editor(ui, value, kind, context, false, &mut ignored_clear)
            .and_then(|edit| match edit {
                ComponentEdit::Property { value, .. } => Some(ComponentEdit::Whole(value)),
                _ => None,
            });
    }
    let hints = match definition.inspector {
        engine::InspectorHint::Fields { fields } => fields,
        _ => &[],
    };
    match value {
        Value::Object(fields) => show_schema_object_editor(
            ui,
            fields,
            &definition.schema,
            hints,
            context,
            animation_asset_action,
        ),
        _ => show_property_value_editor(ui, value),
    }
}

fn show_schema_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
    schema: &engine_authoring::ComponentSchema,
    hints: &[engine::FieldDef],
    context: &InspectorEditContext<'_>,
    animation_asset_action: &mut Option<AnimationControllerAssetAction>,
) -> Option<ComponentEdit> {
    if schema.type_id.as_str() == "engine.transform" {
        return show_transform_object_editor(ui, fields);
    }
    if schema.type_id.as_str() == engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT {
        return show_animation_controller_object_editor(
            ui,
            fields,
            schema,
            hints,
            context,
            animation_asset_action,
        );
    }
    if schema.type_id.as_str() == engine::scene_bridge::BONE_ATTACHMENT_COMPONENT {
        return show_bone_attachment_object_editor(ui, fields, schema, hints, context);
    }
    show_schema_fields_editor(ui, fields, schema, hints, context, None)
}

/// Keeps `bone_name` in step with the `bone` binding an author just picked.
///
/// The bone control only knows the ID it wrote; the readable name lives in a
/// sibling field, which is in scope here and not there. Writing both as one
/// whole-value edit keeps them from drifting apart, and the name stays what
/// ADR 0088 says it is: a label for humans, never a resolution input.
fn show_bone_attachment_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
    schema: &engine_authoring::ComponentSchema,
    hints: &[engine::FieldDef],
    context: &InspectorEditContext<'_>,
) -> Option<ComponentEdit> {
    let bone_choices = context.bone_choices.to_vec();
    let edit = show_schema_fields_editor(ui, fields, schema, hints, context, None)?;
    let ComponentEdit::Property { path, value } = &edit else {
        return Some(edit);
    };
    let picked_bone = matches!(
        path.first(),
        Some(PropertyPathSegment::Field { name }) if name == "bone"
    );
    let Value::I64(bone) = value else {
        return Some(edit);
    };
    if !picked_bone {
        return Some(edit);
    }
    let name = bone_choices
        .iter()
        .find(|(candidate, _)| i64::from(*candidate) == *bone)
        .map(|(_, name)| name.clone())
        .unwrap_or_default();
    let mut updated = fields.clone();
    updated.insert("bone".to_owned(), Value::I64(*bone));
    updated.insert("bone_name".to_owned(), Value::String(name));
    Some(ComponentEdit::Whole(Value::Object(updated)))
}

/// Draws either every schema field or one named subset.
///
/// Keeping the field renderer shared means grouped components retain the same
/// validation, conditional visibility, drag buffering, and command output as
/// ordinary generated Inspectors.
pub(in crate::ui) fn show_schema_fields_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
    schema: &engine_authoring::ComponentSchema,
    hints: &[engine::FieldDef],
    context: &InspectorEditContext<'_>,
    included_fields: Option<&[&str]>,
) -> Option<ComponentEdit> {
    let condition_values = fields.clone();
    let color_groups = color_triple_groups(&schema.fields, fields);
    for field_schema in &schema.fields {
        if included_fields.is_some_and(|names| !names.contains(&field_schema.name.as_str())) {
            continue;
        }
        let hint = hints.iter().find(|hint| hint.name == field_schema.name);
        if color_groups.members.contains(field_schema.name.as_str()) {
            continue;
        }
        if let Some(triple) = color_groups.by_red.get(field_schema.name.as_str()) {
            if hint
                .and_then(|hint| hint.visible_when)
                .is_some_and(|condition| !inspector_condition_matches(condition, &condition_values))
            {
                continue;
            }
            let edit = inspector_field_row(
                ui,
                &triple.label,
                &field_schema.description,
                |ui| show_color_triple_editor(ui, fields, &field_schema.name, triple),
            );
            if let Some(edit) = edit {
                return Some(edit);
            }
            continue;
        }
        let Some(field_value) = fields.get_mut(&field_schema.name) else {
            // Older scenes may omit fields that were added later with a schema
            // default. Keep those scenes editable and let the author opt into
            // materialising the default without changing data merely by opening
            // the Inspector.
            if let Some(engine::InspectorFieldControl::AssetRef(kind)) =
                hint.and_then(|hint| hint.control)
            {
                let selected = inspector_field_row(
                    ui,
                    &field_schema.display_name,
                    &field_schema.description,
                    |ui| show_unassigned_asset_reference_editor(ui, kind, context),
                );
                if let Some(asset) = selected {
                    fields.insert(field_schema.name.clone(), Value::AssetRef(asset));
                    return Some(ComponentEdit::Property {
                        path: Vec::new(),
                        value: Value::Object(fields.clone()),
                    });
                }
            } else if let Some(engine::InspectorFieldControl::EntityRef(required)) =
                hint.and_then(|hint| hint.control)
            {
                let selected = inspector_field_row(
                    ui,
                    &field_schema.display_name,
                    &field_schema.description,
                    |ui| show_unassigned_entity_reference_editor(ui, context, required),
                );
                if let Some(entity) = selected {
                    fields.insert(field_schema.name.clone(), Value::EntityRef(entity));
                    return Some(ComponentEdit::Property {
                        path: Vec::new(),
                        value: Value::Object(fields.clone()),
                    });
                }
            } else if let Some(default) = field_schema.default_value.clone() {
                let use_default = inspector_field_row(
                    ui,
                    &field_schema.display_name,
                    &field_schema.description,
                    |ui| ui.small_button("Use default").clicked(),
                );
                if use_default {
                    fields.insert(field_schema.name.clone(), default);
                    return Some(ComponentEdit::Property {
                        path: Vec::new(),
                        value: Value::Object(fields.clone()),
                    });
                }
            } else {
                inspector_field_row(
                    ui,
                    &field_schema.display_name,
                    &field_schema.description,
                    |ui| {
                        ui.colored_label(egui::Color32::from_rgb(230, 92, 92), "missing");
                    },
                );
            }
            continue;
        };
        if hint
            .and_then(|hint| hint.visible_when)
            .is_some_and(|condition| !inspector_condition_matches(condition, &condition_values))
        {
            continue;
        }
        if !value_matches_field_type(field_value, &field_schema.field_type) {
            inspector_field_row(
                ui,
                &field_schema.display_name,
                &field_schema.description,
                |ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 92, 92),
                        format!("expected {:?}", field_schema.field_type),
                    );
                },
            );
            continue;
        }
        let mut clear_reference = false;
        let allow_none = field_reference_can_be_unassigned(field_schema);
        let edit = inspector_field_row(
            ui,
            &field_schema.display_name,
            &field_schema.description,
            |ui| {
                show_schema_field_editor(
                ui,
                field_value,
                &field_schema.field_type,
                hint.and_then(|hint| hint.control),
                context,
                allow_none,
                &mut clear_reference,
                )
            },
        );
        if clear_reference {
            // Authoring references use an absent object field for the
            // unassigned state. Removing the field preserves ADR 0069's
            // inactive-component behavior and avoids serializing a null value
            // that would fail the field's declared reference type.
            fields.remove(&field_schema.name);
            return Some(ComponentEdit::Whole(Value::Object(fields.clone())));
        }
        if let Some(edit) = edit {
            return Some(prepend_property_segment(
                edit,
                PropertyPathSegment::Field {
                    name: field_schema.name.clone(),
                },
            ));
        }
    }
    None
}

pub(in crate::ui) fn show_schema_field_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    field_type: &engine_authoring::FieldType,
    control: Option<engine::InspectorFieldControl>,
    context: &InspectorEditContext<'_>,
    allow_none: bool,
    clear_requested: &mut bool,
) -> Option<ComponentEdit> {
    if let Some(engine::InspectorFieldControl::Enum(options)) = control {
        let Value::String(current) = value else {
            ui.colored_label(egui::Color32::RED, "invalid enum value");
            return None;
        };
        let mut selected = None;
        egui::ComboBox::from_id_salt(ui.next_auto_id())
            .selected_text(current.as_str())
            .show_ui(ui, |ui| {
                for option in options {
                    if ui.selectable_label(current == option, *option).clicked() {
                        selected = Some((*option).to_owned());
                    }
                }
            });
        return selected
            .filter(|selected| selected != current)
            .map(|selected| ComponentEdit::Property {
                path: Vec::new(),
                value: Value::String(selected),
            });
    }
    if matches!(control, Some(engine::InspectorFieldControl::LayerMask)) {
        return show_layer_mask_editor(ui, value, context.project_layers);
    }
    if let Some(engine::InspectorFieldControl::AssetRefList(kind)) = control {
        return show_asset_reference_list_editor(ui, value, kind, context);
    }
    if let Some(engine::InspectorFieldControl::AssetRef(kind)) = control {
        return show_asset_reference_editor(ui, value, kind, context, allow_none, clear_requested);
    }
    if let Some(engine::InspectorFieldControl::Number(range)) = control {
        let current = numeric_value_as_f64(value);
        let edit = match value {
            Value::I64(value) => {
                drag_value_edit_in_range(ui, value, range, |value| Value::I64(*value))
            }
            Value::U64(value) => {
                drag_value_edit_in_range(ui, value, range, |value| Value::U64(*value))
            }
            Value::F64(value) => {
                drag_value_edit_in_range(ui, value, range, |value| Value::F64(*value))
            }
            _ => None,
        };
        if current.is_none_or(|value| !range.contains(value)) {
            ui.colored_label(egui::Color32::from_rgb(230, 92, 92), range.expectation());
        }
        return edit;
    }
    if matches!(control, Some(engine::InspectorFieldControl::StringBoolMap)) {
        return show_string_bool_map_editor(ui, value);
    }
    if matches!(control, Some(engine::InspectorFieldControl::LodLevels)) {
        return show_lod_levels_editor(ui, value, context);
    }
    if let Some(engine::InspectorFieldControl::BoneRef { .. }) = control {
        return show_bone_reference_editor(ui, value, context.bone_choices);
    }
    if matches!(field_type, engine_authoring::FieldType::EntityRef) {
        let required = match control {
            Some(engine::InspectorFieldControl::EntityRef(required)) => required,
            _ => &[][..],
        };
        return show_entity_reference_editor(
            ui,
            value,
            context,
            required,
            allow_none,
            clear_requested,
        );
    }
    if let (Value::Array(values), engine_authoring::FieldType::Array(element_type)) =
        (&mut *value, field_type)
    {
        return show_typed_array_editor(ui, values, element_type, context);
    }
    show_property_value_editor(ui, value)
}

/// Returns whether the Inspector may remove a scalar reference field.
///
/// Optional entity references already use an absent field as their
/// unassigned state. Asset references also permit that state when they have
/// no schema default: optional references remain unassigned, while required
/// references become inactive with the non-blocking ADR 0069 diagnostic.
/// Asset references with a default are intentionally excluded because
/// removing them means "use the default asset", not "use no asset".
pub(in crate::ui) fn field_reference_can_be_unassigned(
    field: &engine_authoring::FieldSchema,
) -> bool {
    match field.field_type {
        engine_authoring::FieldType::EntityRef => !field.required,
        engine_authoring::FieldType::AssetRef => field.default_value.is_none(),
        _ => false,
    }
}

pub(in crate::ui) fn value_matches_field_type(
    value: &Value,
    field_type: &engine_authoring::FieldType,
) -> bool {
    match (value, field_type) {
        (Value::Bool(_), engine_authoring::FieldType::Bool)
        | (Value::I64(_), engine_authoring::FieldType::I64)
        | (Value::U64(_), engine_authoring::FieldType::U64)
        | (Value::String(_), engine_authoring::FieldType::String)
        | (Value::EntityRef(_), engine_authoring::FieldType::EntityRef)
        | (Value::AssetRef(_), engine_authoring::FieldType::AssetRef)
        | (Value::Object(_), engine_authoring::FieldType::Object)
        | (Value::Object(_), engine_authoring::FieldType::Vec2)
        | (Value::Object(_), engine_authoring::FieldType::Vec3) => true,
        (Value::F64(_) | Value::I64(_) | Value::U64(_), engine_authoring::FieldType::F64) => true,
        (Value::Array(values), engine_authoring::FieldType::Array(element)) => values
            .iter()
            .all(|value| value_matches_field_type(value, element)),
        _ => false,
    }
}

pub(in crate::ui) fn inspector_condition_matches(
    condition: engine::InspectorFieldCondition,
    fields: &std::collections::BTreeMap<String, Value>,
) -> bool {
    match condition {
        engine::InspectorFieldCondition::Bool { field, equals } => {
            fields.get(field) == Some(&Value::Bool(equals))
        }
        engine::InspectorFieldCondition::String { field, equals } => {
            fields.get(field) == Some(&Value::String(equals.to_owned()))
        }
        engine::InspectorFieldCondition::Assigned { field } => {
            !matches!(fields.get(field), None | Some(Value::Null))
        }
        engine::InspectorFieldCondition::StringAny { field, values } => fields
            .get(field)
            .and_then(|value| match value {
                Value::String(value) => Some(value.as_str()),
                _ => None,
            })
            .is_some_and(|value| values.contains(&value)),
    }
}

pub(in crate::ui) fn default_value_for_field_type(
    field_type: &engine_authoring::FieldType,
) -> Option<Value> {
    Some(match field_type {
        engine_authoring::FieldType::Bool => Value::Bool(false),
        engine_authoring::FieldType::I64 => Value::I64(0),
        engine_authoring::FieldType::U64 => Value::U64(0),
        engine_authoring::FieldType::F64 => Value::F64(0.0),
        engine_authoring::FieldType::String => Value::String(String::new()),
        engine_authoring::FieldType::Array(_) => Value::Array(Vec::new()),
        engine_authoring::FieldType::Object => Value::Object(Default::default()),
        engine_authoring::FieldType::Vec2
        | engine_authoring::FieldType::Vec3
        | engine_authoring::FieldType::EntityRef
        | engine_authoring::FieldType::AssetRef => return None,
    })
}
