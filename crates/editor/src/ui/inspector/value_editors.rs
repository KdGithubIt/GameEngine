//! The concrete controls a schema field or a bare value is drawn with.

use crate::ui::*;

/// Numeric `_r`/`_g`/`_b` field triples that render as one color swatch.
pub(in crate::ui) struct ColorTripleGroups {
    /// Group description keyed by the red field's name.
    pub(in crate::ui) by_red: std::collections::BTreeMap<String, ColorTriple>,
    /// Green/blue member field names suppressed from per-field rows.
    pub(in crate::ui) members: std::collections::BTreeSet<String>,
}

pub(in crate::ui) struct ColorTriple {
    pub(in crate::ui) label: String,
    green: String,
    blue: String,
}

/// Detects `<prefix>_r/_g/_b` triples whose current values are plain floats
/// inside [0, 1]. HDR values above 1 keep their numeric rows because the
/// color picker would silently clamp them.
pub(in crate::ui) fn color_triple_groups(
    schema_fields: &[engine_authoring::FieldSchema],
    values: &std::collections::BTreeMap<String, Value>,
) -> ColorTripleGroups {
    let mut groups = ColorTripleGroups {
        by_red: std::collections::BTreeMap::new(),
        members: std::collections::BTreeSet::new(),
    };
    let names: std::collections::BTreeSet<&str> = schema_fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    let in_unit_range = |name: &str| matches!(values.get(name), Some(Value::F64(value)) if (0.0..=1.0).contains(value));
    for field in schema_fields {
        let Some(prefix) = field.name.strip_suffix("_r") else {
            continue;
        };
        let green = format!("{prefix}_g");
        let blue = format!("{prefix}_b");
        if !names.contains(green.as_str()) || !names.contains(blue.as_str()) {
            continue;
        }
        if !in_unit_range(&field.name) || !in_unit_range(&green) || !in_unit_range(&blue) {
            continue;
        }
        let label = field
            .display_name
            .strip_suffix(" R")
            .unwrap_or(&field.display_name)
            .to_owned();
        groups.members.insert(green.clone());
        groups.members.insert(blue.clone());
        groups
            .by_red
            .insert(field.name.clone(), ColorTriple { label, green, blue });
    }
    groups
}

/// One swatch that edits an `_r`/`_g`/`_b` triple as a whole-component
/// update.
///
/// The draft is buffered in egui memory while the pointer is held so a
/// slider gesture inside the picker popup commits one undo step on release
/// instead of one step per tick.
pub(in crate::ui) fn show_color_triple_editor(
    ui: &mut egui::Ui,
    fields: &std::collections::BTreeMap<String, Value>,
    red: &str,
    triple: &ColorTriple,
) -> Option<ComponentEdit> {
    let value_of = |name: &str| match fields.get(name) {
        Some(Value::F64(value)) => *value as f32,
        _ => 0.0,
    };
    let committed = [
        value_of(red),
        value_of(&triple.green),
        value_of(&triple.blue),
    ];
    let id = ui.id().with((red, "color_draft"));
    let mut rgb = ui
        .data_mut(|data| data.get_temp::<[f32; 3]>(id))
        .unwrap_or(committed);
    let response = ui.color_edit_button_rgb(&mut rgb);
    let pointer_down = ui.input(|input| input.pointer.any_down());
    let has_draft = ui.data_mut(|data| data.get_temp::<[f32; 3]>(id)).is_some();
    if (response.changed() || has_draft) && pointer_down {
        ui.data_mut(|data| data.insert_temp(id, rgb));
        return None;
    }
    if response.changed() || has_draft {
        ui.data_mut(|data| data.remove::<[f32; 3]>(id));
        if rgb != committed {
            let mut updated = fields.clone();
            updated.insert(red.to_owned(), Value::F64(rgb[0] as f64));
            updated.insert(triple.green.clone(), Value::F64(rgb[1] as f64));
            updated.insert(triple.blue.clone(), Value::F64(rgb[2] as f64));
            return Some(ComponentEdit::Whole(Value::Object(updated)));
        }
    }
    None
}

/// Edits animation graph condition defaults without exposing raw JSON keys.
pub(in crate::ui) fn show_string_bool_map_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
) -> Option<ComponentEdit> {
    let Value::Object(parameters) = value else {
        ui.colored_label(egui::Color32::RED, "invalid parameter map");
        return None;
    };
    let mut rows = parameters
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    let mut changed = false;
    let mut remove = None;

    ui.vertical(|ui| {
        for (index, (name, value)) in rows.iter_mut().enumerate() {
            ui.push_id(("animation_parameter", index), |ui| {
                ui.horizontal(|ui| {
                    changed |= ui.text_edit_singleline(name).changed();
                    match value {
                        Value::Bool(enabled) => changed |= ui.checkbox(enabled, "").changed(),
                        _ => {
                            ui.colored_label(egui::Color32::RED, "boolean required");
                        }
                    }
                    if ui.small_button("−").clicked() {
                        remove = Some(index);
                    }
                });
            });
        }
        if ui.small_button("+ Add Parameter").clicked() {
            let existing = rows
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let mut suffix = 1_u32;
            let name = loop {
                let candidate = if suffix == 1 {
                    "parameter".to_owned()
                } else {
                    format!("parameter_{suffix}")
                };
                if !existing.contains(candidate.as_str()) {
                    break candidate;
                }
                suffix = suffix.saturating_add(1);
            };
            rows.push((name, Value::Bool(false)));
            changed = true;
        }
    });

    if let Some(index) = remove {
        rows.remove(index);
        changed = true;
    }
    let names = rows.iter().map(|(name, _)| name.trim()).collect::<Vec<_>>();
    let unique_names = names
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let valid = names.iter().all(|name| !name.is_empty())
        && unique_names.len() == names.len()
        && rows
            .iter()
            .all(|(_, value)| matches!(value, Value::Bool(_)));
    if !valid {
        ui.colored_label(
            egui::Color32::RED,
            "parameter names must be unique and non-empty; values must be boolean",
        );
    }
    (changed && valid).then(|| ComponentEdit::Property {
        path: Vec::new(),
        value: Value::Object(rows.into_iter().collect()),
    })
}

/// Dedicated editor for the structured LOD array. Generic `Value::Object`
/// editing cannot infer that `distance` is ordered or that `mesh` needs a
/// filtered picker, so this control keeps invalid free-form JSON out of the
/// normal authoring path.
pub(in crate::ui) fn show_lod_levels_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    context: &InspectorEditContext<'_>,
) -> Option<ComponentEdit> {
    let Value::Array(levels) = value else {
        ui.colored_label(egui::Color32::RED, "invalid LOD levels");
        return None;
    };
    let choices = asset_choices_for_kind(
        engine::AssetKind::Mesh,
        context.manifest,
        context.assets_root,
    );
    let mut replacement = levels.clone();
    let mut changed = false;
    let mut remove = None;
    let mut previous = 0.0_f64;

    ui.vertical(|ui| {
        for (index, level) in replacement.iter_mut().enumerate() {
            let Value::Object(fields) = level else {
                ui.colored_label(egui::Color32::RED, format!("LOD {index}: invalid row"));
                continue;
            };
            ui.horizontal(|ui| {
                ui.label(format!("LOD {index}"));
                let distance = fields
                    .entry("distance".into())
                    .or_insert(Value::F64((previous + 25.0).max(1.0)));
                if let Value::F64(distance) = distance {
                    changed |= ui
                        .add(
                            egui::DragValue::new(distance)
                                .range(f64::MIN_POSITIVE..=f64::MAX)
                                .speed(0.25)
                                .suffix(" m"),
                        )
                        .changed();
                    if !distance.is_finite() || *distance <= previous {
                        ui.colored_label(egui::Color32::from_rgb(230, 92, 92), "must increase");
                    }
                    previous = *distance;
                } else {
                    ui.colored_label(egui::Color32::RED, "invalid distance");
                }

                let mesh = fields.get("mesh").and_then(|value| match value {
                    Value::AssetRef(asset) => Some(asset.clone()),
                    _ => None,
                });
                let selected_text = mesh
                    .as_ref()
                    .and_then(|mesh| choices.iter().find(|choice| &choice.id == mesh))
                    .map_or("Select mesh…", |choice| choice.label.as_str());
                egui::ComboBox::from_id_salt(("lod_mesh", index))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for choice in &choices {
                            if ui
                                .selectable_label(mesh.as_ref() == Some(&choice.id), &choice.label)
                                .clicked()
                            {
                                fields.insert("mesh".into(), Value::AssetRef(choice.id.clone()));
                                changed = true;
                            }
                        }
                    });
                if ui.small_button("−").clicked() {
                    remove = Some(index);
                }
            });
        }
        if ui.small_button("+ Add LOD").clicked() {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert("distance".into(), Value::F64((previous + 25.0).max(1.0)));
            if let Some(choice) = choices.first() {
                fields.insert("mesh".into(), Value::AssetRef(choice.id.clone()));
            }
            replacement.push(Value::Object(fields));
            changed = true;
        }
    });

    if let Some(index) = remove {
        replacement.remove(index);
        changed = true;
    }
    changed.then(|| ComponentEdit::Property {
        path: Vec::new(),
        value: Value::Array(replacement),
    })
}

/// Edits one bone binding, offering bone names and storing the `BoneId`.
///
/// An author never sees or types the ID. When the rig's bones cannot be read
/// the stored value is still shown, because hiding an unresolvable binding
/// would make it unrepairable (ADR 0088 1).
pub(in crate::ui) fn show_bone_reference_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    bone_choices: &[(u32, String)],
) -> Option<ComponentEdit> {
    let Value::I64(current) = value else {
        ui.colored_label(egui::Color32::RED, "invalid bone binding");
        return None;
    };
    let current = *current;
    if bone_choices.is_empty() {
        ui.label(if current < 0 {
            "assign a Rig to choose a bone".to_owned()
        } else {
            format!("bone {current} (rig unavailable)")
        });
        return None;
    }
    let selected_text = bone_choices
        .iter()
        .find(|(bone, _)| i64::from(*bone) == current)
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| {
            if current < 0 {
                "(none)".to_owned()
            } else {
                format!("bone {current} (not in this rig)")
            }
        });
    let mut selected = None;
    egui::ComboBox::from_id_salt(ui.next_auto_id())
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for (bone, name) in bone_choices {
                if ui
                    .selectable_label(i64::from(*bone) == current, name)
                    .clicked()
                {
                    selected = Some((*bone, name.clone()));
                }
            }
        });
    selected
        .filter(|(bone, _)| i64::from(*bone) != current)
        .map(|(bone, _)| ComponentEdit::Property {
            path: Vec::new(),
            value: Value::I64(i64::from(bone)),
        })
}

pub(in crate::ui) fn show_layer_mask_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    project_layers: &[engine_authoring::project_settings::Layer],
) -> Option<ComponentEdit> {
    let (mut mask, signed) = match value {
        Value::I64(mask) => ((*mask as u64) & u64::from(u32::MAX), true),
        Value::U64(mask) => (*mask & u64::from(u32::MAX), false),
        _ => {
            ui.colored_label(egui::Color32::RED, "invalid layer mask");
            return None;
        }
    };
    let original = mask;
    ui.menu_button(format!("0x{mask:08X}"), |ui| {
        for layer in project_layers {
            let bit = 1_u64 << layer.index;
            let mut enabled = mask & bit != 0;
            if ui.checkbox(&mut enabled, &layer.name).changed() {
                if enabled {
                    mask |= bit;
                } else {
                    mask &= !bit;
                }
            }
        }
    });
    (mask != original).then(|| ComponentEdit::Property {
        path: Vec::new(),
        value: if signed {
            Value::I64(mask as i64)
        } else {
            Value::U64(mask)
        },
    })
}

pub(in crate::ui) fn show_typed_array_editor(
    ui: &mut egui::Ui,
    values: &mut Vec<Value>,
    element_type: &engine_authoring::FieldType,
    context: &InspectorEditContext<'_>,
) -> Option<ComponentEdit> {
    let mut remove = None;
    for (index, item) in values.iter_mut().enumerate() {
        let mut item_edit = None;
        ui.horizontal(|ui| {
            let mut ignored_clear = false;
            item_edit = show_schema_field_editor(
                ui,
                item,
                element_type,
                None,
                context,
                false,
                &mut ignored_clear,
            );
            if ui.small_button("−").clicked() {
                remove = Some(index);
            }
        });
        if let Some(edit) = item_edit {
            return Some(prepend_property_segment(
                edit,
                PropertyPathSegment::Index { index },
            ));
        }
    }
    if let Some(index) = remove {
        values.remove(index);
        return Some(ComponentEdit::Property {
            path: Vec::new(),
            value: Value::Array(values.clone()),
        });
    }
    if let Some(default) = default_value_for_field_type(element_type)
        && ui.small_button("+ Add").clicked() {
            values.push(default);
            return Some(ComponentEdit::Property {
                path: Vec::new(),
                value: Value::Array(values.clone()),
            });
        }
    None
}

pub(in crate::ui) fn show_property_value_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
) -> Option<ComponentEdit> {
    match value {
        Value::Null => {
            ui.label("null");
            None
        }
        Value::Bool(value) => ui
            .checkbox(value, "")
            .changed()
            .then(|| ComponentEdit::Property {
                path: Vec::new(),
                value: Value::Bool(*value),
            }),
        Value::I64(value) => drag_value_edit(ui, value, |value| Value::I64(*value)),
        Value::U64(value) => drag_value_edit(ui, value, |value| Value::U64(*value)),
        Value::F64(value) => drag_value_edit(ui, value, |value| Value::F64(*value)),
        Value::String(value) => {
            draft_text_value(ui, "component_string", value, false).map(|text| {
                *value = text;
                ComponentEdit::Property {
                    path: Vec::new(),
                    value: Value::String(value.clone()),
                }
            })
        }
        Value::Array(values) => {
            for (index, item) in values.iter_mut().enumerate() {
                let mut edit = None;
                ui.horizontal(|ui| {
                    ui.label(format!("[{index}]"));
                    edit = show_property_value_editor(ui, item);
                });
                if let Some(edit) = edit {
                    return Some(prepend_property_segment(
                        edit,
                        PropertyPathSegment::Index { index },
                    ));
                }
            }
            None
        }
        Value::Object(fields) => {
            for (name, field_value) in fields.iter_mut() {
                let mut edit = None;
                ui.horizontal(|ui| {
                    ui.label(name);
                    edit = show_property_value_editor(ui, field_value);
                });
                if let Some(edit) = edit {
                    return Some(prepend_property_segment(
                        edit,
                        PropertyPathSegment::Field { name: name.clone() },
                    ));
                }
            }
            None
        }
        Value::EntityRef(id) => {
            ui.label(format!("entity_ref: {}", id.as_str()));
            None
        }
        Value::AssetRef(id) => {
            ui.label(format!("asset_ref: {}", id.as_str()));
            None
        }
    }
}

pub(in crate::ui) fn numeric_value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::I64(value) => Some(*value as f64),
        Value::U64(value) => Some(*value as f64),
        Value::F64(value) => Some(*value),
        _ => None,
    }
}

fn drag_value_edit<T>(
    ui: &mut egui::Ui,
    value: &mut T,
    make_value: impl FnOnce(&T) -> Value,
) -> Option<ComponentEdit>
where
    T: egui::emath::Numeric,
{
    let response = ui.add(egui::DragValue::new(value));
    numeric_drag_response(response, value, make_value)
}

pub(in crate::ui) fn drag_value_edit_in_range<T>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: engine::NumericRange,
    make_value: impl FnOnce(&T) -> Value,
) -> Option<ComponentEdit>
where
    T: egui::emath::Numeric,
{
    let mut drag = egui::DragValue::new(value);
    let lower = range
        .min
        .filter(|_| range.min_inclusive)
        .unwrap_or(f64::NEG_INFINITY);
    let upper = range
        .max
        .filter(|_| range.max_inclusive)
        .unwrap_or(f64::INFINITY);
    drag = drag.range(lower..=upper);
    let response = ui.add(drag);
    numeric_drag_response(response, value, make_value)
}

pub(in crate::ui) fn numeric_drag_response<T>(
    response: egui::Response,
    value: &T,
    make_value: impl FnOnce(&T) -> Value,
) -> Option<ComponentEdit> {
    let changed_value = response.changed().then(|| make_value(value));

    if response.drag_stopped() {
        return Some(match changed_value {
            Some(value) => ComponentEdit::Property {
                path: Vec::new(),
                value,
            },
            None => ComponentEdit::CommitDraft { path: Vec::new() },
        });
    }
    if response.dragged() {
        return changed_value.map(|value| ComponentEdit::DraftProperty {
            path: Vec::new(),
            value,
        });
    }
    changed_value.map(|value| ComponentEdit::Property {
        path: Vec::new(),
        value,
    })
}
