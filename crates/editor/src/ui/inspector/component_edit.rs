//! Turning one Inspector interaction into an authoring session command.
//!
//! Controls describe what changed as a [`ComponentEdit`] instead of touching
//! the session themselves, so undo granularity, drag buffering, and the
//! "Edit all selected" fan-out are decided in exactly one place.

use crate::ui::*;

pub(in crate::ui) enum ComponentEdit {
    Whole(Value),
    Property {
        path: Vec<PropertyPathSegment>,
        value: Value,
    },
    DraftProperty {
        path: Vec<PropertyPathSegment>,
        value: Value,
    },
    CommitDraft {
        path: Vec<PropertyPathSegment>,
    },
}

#[derive(Clone)]
pub(in crate::ui) struct PendingComponentDrag {
    pub(in crate::ui) entity: EntityId,
    pub(in crate::ui) component_type: ComponentTypeId,
    pub(in crate::ui) path: Vec<PropertyPathSegment>,
    pub(in crate::ui) value: Value,
}

impl PendingComponentDrag {
    pub(in crate::ui) fn matches_component(
        &self,
        entity: &EntityId,
        component_type: &ComponentTypeId,
    ) -> bool {
        self.entity == *entity && self.component_type == *component_type
    }

    fn matches_path(
        &self,
        entity: &EntityId,
        component_type: &ComponentTypeId,
        path: &[PropertyPathSegment],
    ) -> bool {
        self.matches_component(entity, component_type) && self.path == path
    }

    /// Builds the complete component value rendered during an Inspector drag.
    ///
    /// This value remains transient until pointer release. Keeping the preview
    /// separate from the authoring session preserves one undo entry while the
    /// Scene View can rebuild its runtime world from the latest pointer value.
    pub(in crate::ui) fn scene_preview(
        &self,
        current_component: &Value,
    ) -> Option<SceneComponentPreview> {
        let mut value = current_component.clone();
        if !upsert_property_value(&mut value, &self.path, self.value.clone()) {
            return None;
        }
        Some(SceneComponentPreview {
            entity: self.entity.clone(),
            component_type: self.component_type.clone(),
            value,
        })
    }
}

/// Committed edit shape mirrored onto the rest of the selection.
enum PropagatedEdit {
    Whole(Value),
    Property {
        path: Vec<PropertyPathSegment>,
        value: Value,
    },
}

impl EditorApp {
    /// Draws the Add Component search box and choice list, and returns the
    /// schema the author picked in this frame.
    ///
    /// The list is drawn inline in the Inspector column instead of in a
    /// floating popup. The Add Component button sits at the end of a long
    /// column, so a popup could not open downwards there and flipped above the
    /// button; an inline section always grows below it and scrolls with the
    /// panel. Its own height stays bounded so the choices never push the rest
    /// of the Inspector out of reach.
    ///
    /// `search` is the already lowercased filter that produced `available`.
    pub(in crate::ui) fn show_add_component_choices(
        &mut self,
        ui: &mut egui::Ui,
        search: &str,
        available: &[(&engine_authoring::ComponentSchema, &'static str)],
    ) -> Option<engine_authoring::ComponentSchema> {
        let mut clicked = None;
        ui.add(
            egui::TextEdit::singleline(&mut self.component_search)
                .desired_width(ui.available_width())
                .hint_text("Search components..."),
        );
        ui.small("To show a model: add Static Mesh Renderer or Skinned Mesh Renderer.");
        if available.is_empty() {
            ui.label("No matching components");
            return None;
        }
        egui::ScrollArea::vertical()
            .id_salt("add_component_choices")
            .max_height(ADD_COMPONENT_LIST_HEIGHT)
            // The section is the last thing in a long Inspector column, so the
            // space left below it is usually a few pixels. Without this floor
            // egui sizes the list from that remainder and it collapses to one
            // row; the surrounding panel scrolls down to it instead.
            .min_scrolled_height(ADD_COMPONENT_LIST_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                if search.is_empty() {
                    // Keep the complete list hierarchical: ownership
                    // (Engine/Game), then schema category, then component.
                    let mut groups: std::collections::BTreeMap<
                        String,
                        std::collections::BTreeMap<String, Vec<_>>,
                    > = std::collections::BTreeMap::new();
                    for (schema, source) in available.iter().copied() {
                        groups
                            .entry(source.to_owned())
                            .or_default()
                            .entry(schema.category.clone())
                            .or_default()
                            .push(schema);
                    }
                    for (source, categories) in groups {
                        // The owner is a plain heading rather than a third
                        // collapsing level: the Inspector column is narrow, and
                        // every extra level of indentation is width the
                        // component names no longer have.
                        ui.strong(&source);
                        for (category, mut schemas) in categories {
                            schemas
                                .sort_by(|left, right| left.display_name.cmp(&right.display_name));
                            let category_salt = category.clone();
                            egui::CollapsingHeader::new(&category)
                                .id_salt((
                                    "add_component_category",
                                    source.as_str(),
                                    category_salt,
                                ))
                                .default_open(true)
                                .show(ui, |ui| {
                                    for schema in schemas {
                                        if add_component_choice_button(ui, &schema.display_name) {
                                            clicked = Some(schema.clone());
                                        }
                                    }
                                });
                        }
                    }
                } else {
                    for (schema, source) in available.iter().copied() {
                        let label =
                            format!("{source} / {} / {}", schema.category, schema.display_name);
                        if add_component_choice_button(ui, &label) {
                            clicked = Some(schema.clone());
                        }
                    }
                }
            });
        clicked
    }

    /// Adds the picked component and dismisses the Add Component section.
    ///
    /// One choice completes the interaction, so the list closes and the filter
    /// is reset for the next component instead of staying open over the
    /// Inspector.
    pub(in crate::ui) fn apply_add_component_choice(
        &mut self,
        schema: &engine_authoring::ComponentSchema,
    ) {
        self.add_component_picker_open = false;
        self.component_search.clear();
        self.add_component_to_selection(schema);
    }

    /// Adds the schema's default component to every selected entity that does
    /// not already have it.
    ///
    /// Targets are pre-filtered like the Remove path: without the filter a
    /// mixed multi-selection makes the atomic command fail with
    /// `component.already_exists` and nothing is added at all.
    fn add_component_to_selection(&mut self, schema: &engine_authoring::ComponentSchema) {
        let component_type: ComponentTypeId = schema.type_id.clone();
        let targets: Vec<_> = self
            .selected_scene_ids()
            .into_iter()
            .filter(|entity_id| {
                self.session
                    .scene_entity(entity_id)
                    .is_some_and(|item| !item.components.contains_key(&component_type))
            })
            .collect();
        if targets.is_empty() {
            self.push_notification(
                EditorNotificationLevel::Info,
                format!("Every selected entity already has {}", schema.display_name),
            );
            return;
        }
        let result = self.session.add_scene_component_to_entities(
            targets,
            component_type,
            schema.default_value(),
        );
        if let Err(error) = &result {
            self.push_notification(
                EditorNotificationLevel::Error,
                format!("Add Component failed: {error}"),
            );
        }
        self.apply_ui_result(result);
        self.refresh_scene_problems();
    }

    /// Applies the copied component value to every selected entity that
    /// already has a component of the same type.
    pub(in crate::ui) fn paste_component_values(&mut self, component_type: &ComponentTypeId) {
        let Some((clipboard_type, value)) = self.component_clipboard.clone() else {
            return;
        };
        if clipboard_type != *component_type {
            return;
        }
        let targets: Vec<_> = self
            .selected_scene_ids()
            .into_iter()
            .filter(|entity_id| {
                self.session
                    .scene_entity(entity_id)
                    .is_some_and(|item| item.components.contains_key(component_type))
            })
            .collect();
        for entity in targets {
            let result = self.session.set_scene_component_value(
                entity,
                component_type.clone(),
                value.clone(),
            );
            self.apply_ui_result(result);
        }
        self.refresh_scene_problems();
    }

    /// Replaces the component on every selected entity with its schema default.
    pub(in crate::ui) fn reset_component_values(&mut self, component_type: &ComponentTypeId) {
        let default_value = engine::builtin_registry()
            .get(component_type)
            .map(|definition| definition.schema.default_value())
            .or_else(|| {
                self.game_module
                    .as_ref()
                    .and_then(|module| module.component_schema(component_type))
                    .map(|schema| schema.default_value())
            });
        let Some(default_value) = default_value else {
            return;
        };
        let targets: Vec<_> = self
            .selected_scene_ids()
            .into_iter()
            .filter(|entity_id| {
                self.session
                    .scene_entity(entity_id)
                    .is_some_and(|item| item.components.contains_key(component_type))
            })
            .collect();
        for entity in targets {
            let result = self.session.set_scene_component_value(
                entity,
                component_type.clone(),
                default_value.clone(),
            );
            self.apply_ui_result(result);
        }
        self.refresh_scene_problems();
    }

    pub(in crate::ui) fn apply_component_edit(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        current_component: &Value,
        edit: ComponentEdit,
    ) {
        let refresh_problems = !matches!(&edit, ComponentEdit::DraftProperty { .. });
        // Committed edits optionally mirror onto the rest of the selection;
        // transient drafts stay primary-only so previews never fan out.
        let propagate = if self.multi_edit_all {
            match &edit {
                ComponentEdit::Whole(value) => Some(PropagatedEdit::Whole(value.clone())),
                ComponentEdit::Property { path, value } => Some(PropagatedEdit::Property {
                    path: path.clone(),
                    value: value.clone(),
                }),
                _ => None,
            }
        } else {
            None
        };
        let primary = entity.clone();
        let propagate_type = component_type.clone();
        match edit {
            ComponentEdit::Whole(new_value) => {
                self.clear_pending_component_drag_for_component(&entity, &component_type);
                if new_value != *current_component {
                    let result =
                        self.session
                            .set_scene_component_value(entity, component_type, new_value);
                    self.apply_ui_result(result);
                }
            }
            ComponentEdit::Property { path, value } => {
                self.clear_pending_component_drag_for_path(&entity, &component_type, &path);
                // Schema defaults can expose fields which are absent from an
                // older serialized component. Materialize such a field by
                // replacing the component atomically; existing paths retain
                // the narrower property command and its precise undo record.
                let result = if property_value(current_component, &path).is_some() {
                    self.session
                        .set_scene_component_property(entity, component_type, path, value)
                } else {
                    let mut upgraded = current_component.clone();
                    if upsert_property_value(&mut upgraded, &path, value.clone()) {
                        self.session
                            .set_scene_component_value(entity, component_type, upgraded)
                    } else {
                        self.session.set_scene_component_property(
                            entity,
                            component_type,
                            path,
                            value,
                        )
                    }
                };
                self.apply_ui_result(result);
            }
            ComponentEdit::DraftProperty { path, value } => {
                self.pending_component_drag = Some(PendingComponentDrag {
                    entity,
                    component_type,
                    path,
                    value,
                });
            }
            ComponentEdit::CommitDraft { path } => {
                self.commit_pending_component_drag(
                    &entity,
                    &component_type,
                    current_component,
                    &path,
                );
            }
        }
        if let Some(edit) = propagate {
            self.propagate_edit_to_selection(&primary, &propagate_type, edit);
        }
        if refresh_problems {
            self.refresh_scene_problems();
        }
    }

    /// Mirrors one committed edit onto every other selected entity that has
    /// the same component (the "Edit all selected" toggle).
    fn propagate_edit_to_selection(
        &mut self,
        primary: &EntityId,
        component_type: &ComponentTypeId,
        edit: PropagatedEdit,
    ) {
        let targets: Vec<_> = self
            .selected_scene_ids()
            .into_iter()
            .filter(|candidate| candidate != primary)
            .filter(|candidate| {
                self.session
                    .scene_entity(candidate)
                    .is_some_and(|item| item.components.contains_key(component_type))
            })
            .collect();
        for target in targets {
            let Some(current) = self
                .session
                .scene_entity(&target)
                .and_then(|item| item.components.get(component_type))
                .cloned()
            else {
                continue;
            };
            let updated = match &edit {
                PropagatedEdit::Whole(value) => value.clone(),
                PropagatedEdit::Property { path, value } => {
                    let mut updated = current.clone();
                    if !upsert_property_value(&mut updated, path, value.clone()) {
                        continue;
                    }
                    updated
                }
            };
            if updated == current {
                continue;
            }
            let result =
                self.session
                    .set_scene_component_value(target, component_type.clone(), updated);
            self.apply_ui_result(result);
        }
    }

    fn clear_pending_component_drag_for_component(
        &mut self,
        entity: &EntityId,
        component_type: &ComponentTypeId,
    ) {
        if self
            .pending_component_drag
            .as_ref()
            .is_some_and(|pending| pending.matches_component(entity, component_type))
        {
            self.pending_component_drag = None;
        }
    }

    fn clear_pending_component_drag_for_path(
        &mut self,
        entity: &EntityId,
        component_type: &ComponentTypeId,
        path: &[PropertyPathSegment],
    ) {
        if self
            .pending_component_drag
            .as_ref()
            .is_some_and(|pending| pending.matches_path(entity, component_type, path))
        {
            self.pending_component_drag = None;
        }
    }

    pub(in crate::ui) fn commit_pending_component_drag_for_entity(
        &mut self,
        entity_id: &EntityId,
        entity: &engine_authoring::AuthoringEntity,
    ) {
        let Some(pending) = self.pending_component_drag.clone() else {
            return;
        };
        if pending.entity != *entity_id {
            return;
        }
        if let Some(component) = entity.components.get(&pending.component_type) {
            self.commit_pending_component_drag(
                &pending.entity,
                &pending.component_type,
                component,
                &pending.path,
            );
        }
    }

    fn commit_pending_component_drag(
        &mut self,
        entity: &EntityId,
        component_type: &ComponentTypeId,
        current_component: &Value,
        path: &[PropertyPathSegment],
    ) {
        let Some(pending) = self.pending_component_drag.take() else {
            return;
        };
        if !pending.matches_path(entity, component_type, path) {
            self.pending_component_drag = Some(pending);
            return;
        }
        if property_value(current_component, path).is_some_and(|current| current == &pending.value)
        {
            return;
        }
        let result = if property_value(current_component, &pending.path).is_some() {
            self.session.set_scene_component_property(
                pending.entity,
                pending.component_type,
                pending.path,
                pending.value,
            )
        } else {
            let mut upgraded = current_component.clone();
            if upsert_property_value(&mut upgraded, &pending.path, pending.value.clone()) {
                self.session.set_scene_component_value(
                    pending.entity,
                    pending.component_type,
                    upgraded,
                )
            } else {
                self.session.set_scene_component_property(
                    pending.entity,
                    pending.component_type,
                    pending.path,
                    pending.value,
                )
            }
        };
        self.apply_ui_result(result);
    }
}
