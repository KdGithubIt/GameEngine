//! Entity reference fields: the catalog of assignable entities, the picker
//! filtered by required component types, and the reference editors.

use crate::ui::*;

/// One selectable target for an entity-reference field.
///
/// Carrying the component types alongside the label is what lets a reference
/// field offer only entities it can actually point at (ADR 0087 4); a label
/// alone cannot express "entities that own a rig".
#[derive(Clone)]
pub(in crate::ui) struct EntityChoice {
    pub(in crate::ui) id: EntityId,
    pub(in crate::ui) label: String,
    /// Human-readable path from a scene root to this entity.
    pub(in crate::ui) hierarchy_path: String,
    pub(in crate::ui) components: Vec<ComponentTypeId>,
}

impl EntityChoice {
    fn has_every(&self, required: &[&str]) -> bool {
        required.iter().all(|component| {
            self.components
                .iter()
                .any(|present| present.as_str() == *component)
        })
    }
}

/// Returns the label used for an entity inside a reference picker.
pub(in crate::ui) fn entity_reference_display_label(entity: &AuthoringEntity) -> String {
    if !entity.display_name.trim().is_empty() {
        entity.display_name.clone()
    } else if !entity.name.trim().is_empty() {
        entity.name.clone()
    } else {
        entity.id.as_str().to_owned()
    }
}

/// Builds a readable root-to-entity path without using names for identity.
///
/// Invalid direct JSON edits can leave a missing parent or cycle. The visited
/// set bounds traversal so the Inspector remains usable while scene
/// validation reports the structural problem.
pub(in crate::ui) fn entity_reference_hierarchy_path(
    scene: &AuthoringScene,
    entity: &EntityId,
) -> String {
    let mut labels = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut current = Some(entity.clone());
    while let Some(current_id) = current {
        if !visited.insert(current_id.clone()) {
            labels.push("[cycle]".to_owned());
            break;
        }
        let Some(current_entity) = scene.entity(&current_id) else {
            labels.push(format!("[missing {}]", current_id.as_str()));
            break;
        };
        labels.push(entity_reference_display_label(current_entity));
        current = current_entity.parent.clone();
    }
    labels.reverse();
    labels.join(" / ")
}

/// Draws a searchable entity picker filtered by required component types.
fn show_entity_choice_picker(
    ui: &mut egui::Ui,
    selector_response: &egui::Response,
    current: Option<&EntityId>,
    choices: &[EntityChoice],
    required: &[&str],
    allow_none: bool,
) -> Option<ReferencePickerAction<EntityId>> {
    let picker_id = selector_response.id.with("entity_reference_picker");
    let search_id = picker_id.with("search");
    let mut search = ui
        .data_mut(|data| data.get_temp::<String>(search_id))
        .unwrap_or_default();
    let mut action = None;
    egui::Popup::menu(selector_response)
        .id(picker_id)
        .width(380.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(360.0);

            if allow_none {
                if ui
                    .selectable_label(current.is_none(), "—  None")
                    .on_hover_text("Leave this reference unassigned")
                    .clicked()
                {
                    action = Some(ReferencePickerAction::Clear);
                    ui.close();
                }
                ui.separator();
            }

            let search_response = ui.add(
                egui::TextEdit::singleline(&mut search)
                    .hint_text("Search name, hierarchy, or ID...")
                    .desired_width(f32::INFINITY),
            );
            if selector_response.clicked() {
                search_response.request_focus();
            }
            ui.separator();
            let normalized_search = search.trim().to_ascii_lowercase();
            let mut visible_count = 0_usize;
            egui::ScrollArea::vertical()
                .id_salt(picker_id.with("scroll"))
                .max_height(280.0)
                .show(ui, |ui| {
                    for choice in choices {
                        if !choice.has_every(required) && current != Some(&choice.id) {
                            continue;
                        }
                        let matches = normalized_search.is_empty()
                            || choice
                                .label
                                .to_ascii_lowercase()
                                .contains(&normalized_search)
                            || choice
                                .hierarchy_path
                                .to_ascii_lowercase()
                                .contains(&normalized_search)
                            || choice
                                .id
                                .as_str()
                                .to_ascii_lowercase()
                                .contains(&normalized_search);
                        if !matches {
                            continue;
                        }
                        visible_count += 1;
                        let row = ui.horizontal(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(90, 175, 235),
                                egui::RichText::new("●").size(14.0),
                            );
                            let clicked = ui
                                .selectable_label(current == Some(&choice.id), &choice.label)
                                .clicked();
                            ui.weak(&choice.hierarchy_path);
                            clicked
                        });
                        row.response.on_hover_text(format!(
                            "{}\nStable ID: {}",
                            choice.hierarchy_path,
                            choice.id.as_str()
                        ));
                        if row.inner {
                            action = Some(ReferencePickerAction::Assign(choice.id.clone()));
                            ui.close();
                        }
                    }
                });
            if visible_count == 0 {
                ui.weak("No compatible entities match the search.");
            }
        });
    ui.data_mut(|data| data.insert_temp(search_id, search));
    action
}

/// Edits one entity reference, offering only entities the field accepts.
///
/// `required` names the component types a target must carry (ADR 0087 4).
/// The entity currently stored stays visible even when it does not qualify,
/// so an out-of-band edit is visible and repairable rather than silently
/// replaced; scene validation reports it as
/// `scene.entity_reference_wrong_target`.
pub(in crate::ui) fn show_entity_reference_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    context: &InspectorEditContext<'_>,
    required: &[&str],
    allow_none: bool,
    clear_requested: &mut bool,
) -> Option<ComponentEdit> {
    let Value::EntityRef(current) = value else {
        ui.colored_label(egui::Color32::RED, "invalid entity reference");
        return None;
    };
    let choice = context
        .entity_choices
        .iter()
        .find(|choice| choice.id == *current);
    let (label, icon_color, tooltip) = match choice {
        Some(choice) => (
            choice.label.clone(),
            egui::Color32::from_rgb(90, 175, 235),
            format!("{}\nStable ID: {}", choice.hierarchy_path, current.as_str()),
        ),
        None => (
            format!("Missing: {}", current.as_str()),
            egui::Color32::from_rgb(230, 92, 92),
            format!(
                "The referenced entity is missing.\nStable ID: {}",
                current.as_str()
            ),
        ),
    };
    let (field_response, selector_response) =
        show_compact_reference_field(ui, "●", icon_color, &label, &tooltip);

    if field_response.double_clicked() {
        context
            .reference_navigation
            .replace(Some(InspectorReferenceNavigation::FocusEntity(
                current.clone(),
            )));
    } else if field_response.clicked() {
        context
            .reference_navigation
            .replace(Some(InspectorReferenceNavigation::SelectEntity(
                current.clone(),
            )));
    }

    match show_entity_choice_picker(
        ui,
        &selector_response,
        Some(current),
        context.entity_choices,
        required,
        allow_none,
    ) {
        Some(ReferencePickerAction::Assign(selected)) if selected != *current => {
            Some(ComponentEdit::Property {
                path: Vec::new(),
                value: Value::EntityRef(selected),
            })
        }
        Some(ReferencePickerAction::Clear) if allow_none => {
            *clear_requested = true;
            None
        }
        _ => None,
    }
}

/// Offers a filtered target picker for an optional EntityRef that is not
/// currently serialized. No placeholder ID is written until the author
/// selects an actual compatible entity.
pub(in crate::ui) fn show_unassigned_entity_reference_editor(
    ui: &mut egui::Ui,
    context: &InspectorEditContext<'_>,
    required: &[&str],
) -> Option<EntityId> {
    let (field_response, selector_response) = show_compact_reference_field(
        ui,
        "●",
        egui::Color32::from_gray(140),
        "None",
        "No entity is assigned.",
    );
    let _ = field_response;
    match show_entity_choice_picker(
        ui,
        &selector_response,
        None,
        context.entity_choices,
        required,
        true,
    ) {
        Some(ReferencePickerAction::Assign(entity)) => Some(entity),
        Some(ReferencePickerAction::Clear) | None => None,
    }
}
