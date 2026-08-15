//! Entity Inspector panel: the component list, the per-frame caches it reads,
//! and the editor-only navigation an Inspector reference field can request.
//!
//! The panel decides what to draw and in which order; the controls themselves
//! come from the schema, reference, and value editor submodules.

use crate::ui::*;

/// Revision-keyed data derived for the entity Inspector.
///
/// The Inspector is drawn every frame, but the scene reference catalog,
/// component catalog, and imported skeleton bone list only change when their
/// owning document, manifest, selection, or game module changes. Keeping the
/// derived values here removes repeated hierarchy walks, schema cloning, and
/// glTF parsing from steady-state UI frames (ADR 0104).
#[derive(Default)]
pub(in crate::ui) struct InspectorDerivedCache {
    scene_revision: Option<u64>,
    entity_choices: Arc<Vec<EntityChoice>>,
    game_module: Option<Arc<engine::game_module::GameModule>>,
    builtins: Option<Arc<engine::ComponentRegistry>>,
    component_catalog: Arc<Vec<(engine_authoring::ComponentSchema, &'static str)>>,
    bone_key: Option<BoneChoicesCacheKey>,
    bone_choices: Arc<Vec<(u32, String)>>,
}

/// Inputs that determine the imported bone names shown for one selection.
#[derive(Clone, PartialEq, Eq)]
struct BoneChoicesCacheKey {
    document_revision: u64,
    manifest_revision: u64,
    selected_entity: Option<EntityId>,
}

impl EditorApp {
    pub(in crate::ui) fn show_runtime_entity_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Runtime Inspector");
        let Some(runtime) = self.runtime_state.as_ref() else {
            ui.label("Runtime is unavailable.");
            return;
        };
        let entities = runtime.entity_debug_snapshot();
        let selected = self.selected_runtime_entity.or_else(|| {
            self.selected_entity.as_ref().and_then(|authoring_id| {
                entities.iter().find_map(|entity| {
                    (entity.authoring_id.as_deref() == Some(authoring_id.as_str()))
                        .then_some((entity.entity.id(), entity.entity.generation()))
                })
            })
        });
        let Some(selected) = selected else {
            ui.label("Select an entity in the Runtime debugger.");
            return;
        };
        self.selected_runtime_entity = Some(selected);
        let Some(entity) = entities
            .into_iter()
            .find(|entity| (entity.entity.id(), entity.entity.generation()) == selected)
        else {
            ui.label("The selected runtime entity no longer exists.");
            self.selected_runtime_entity = None;
            return;
        };
        ui.label(format!("Runtime ID: {}", entity.entity));
        ui.label(format!("Name: {}", entity.name));
        if let Some(authoring_id) = entity.authoring_id {
            ui.monospace(format!("Authoring: {authoring_id}"));
        }
        ui.separator();
        ui.strong("Components");
        for component in entity.components {
            ui.label(component);
        }
        if let Some((translation, rotation, scale)) = entity.transform {
            ui.separator();
            ui.strong("Transform (read-only while playing)");
            ui.monospace(format!(
                "Position  {:.3}, {:.3}, {:.3}",
                translation[0], translation[1], translation[2]
            ));
            ui.monospace(format!(
                "Rotation  {:.2}°, {:.2}°, {:.2}°",
                rotation[0], rotation[1], rotation[2]
            ));
            ui.monospace(format!(
                "Scale     {:.3}, {:.3}, {:.3}",
                scale[0], scale[1], scale[2]
            ));
        }
    }

    /// Refreshes expensive Inspector-derived collections only after an input
    /// revision changes, then lets the frame clone cheap `Arc` handles.
    fn refresh_inspector_derived_cache(&mut self) {
        let scene_revision = self.session.scene().map(AuthoringScene::revision);
        if self.inspector_cache.scene_revision != scene_revision {
            let entity_choices = self
                .session
                .scene()
                .map(|scene| {
                    scene
                        .entities()
                        .map(|(id, entity)| EntityChoice {
                            id: id.clone(),
                            label: entity_reference_display_label(entity),
                            hierarchy_path: entity_reference_hierarchy_path(scene, id),
                            components: entity.components.keys().cloned().collect(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.inspector_cache.scene_revision = scene_revision;
            self.inspector_cache.entity_choices = Arc::new(entity_choices);
        }

        let game_module_changed = match (&self.inspector_cache.game_module, &self.game_module) {
            (Some(cached), Some(current)) => !Arc::ptr_eq(cached, current),
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
        };
        if self.inspector_cache.builtins.is_none()
            || game_module_changed
        {
            let builtins = Arc::new(engine::builtin_registry());
            let mut component_catalog = builtins
                .definitions()
                .map(|definition| (definition.schema.clone(), "Engine"))
                .collect::<Vec<_>>();
            if let Some(module) = &self.game_module {
                component_catalog.extend(
                    module
                        .component_schemas()
                        .cloned()
                        .map(|schema| (schema, "Game")),
                );
            }
            self.inspector_cache.game_module = self.game_module.as_ref().map(Arc::clone);
            self.inspector_cache.builtins = Some(builtins);
            self.inspector_cache.component_catalog = Arc::new(component_catalog);
        }

        let bone_key = BoneChoicesCacheKey {
            document_revision: self.session.document_revision(),
            manifest_revision: self.asset_manifest.revision(),
            selected_entity: self.selected_entity.clone(),
        };
        if self.inspector_cache.bone_key.as_ref() != Some(&bone_key) {
            let bone_choices = self.compute_bone_choices_for_selected_entity();
            self.inspector_cache.bone_key = Some(bone_key);
            self.inspector_cache.bone_choices = Arc::new(bone_choices);
        }
    }

    pub(in crate::ui) fn show_entity_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Entity Inspector");
        let Some(selected) = self.selected_entity.clone() else {
            ui.label("Select an entity in the Hierarchy or Scene View.");
            ui.separator();
            self.show_summary(ui);
            return;
        };
        let Some(entity) = self.session.scene_entity(&selected).cloned() else {
            ui.label("Selected entity is missing");
            self.select_single_entity(None);
            return;
        };

        let selection_count = self.selected_scene_ids().len();
        if selection_count > 1 {
            ui.colored_label(
                egui::Color32::from_rgb(120, 180, 235),
                format!("{selection_count} entities selected."),
            );
            ui.checkbox(&mut self.multi_edit_all, "Edit all selected")
                .on_hover_text(
                    "Apply component edits below to every selected entity that has the component; \
                     off limits them to the primary entity",
                );
            control_row(ui, |ui| {
                ui.label("Duplicate Offset");
                for (index, label) in ["X", "Y", "Z"].iter().enumerate() {
                    ui.label(*label);
                    ui.add(egui::DragValue::new(&mut self.duplicate_offset[index]).speed(0.1));
                }
                if ui.button("Duplicate Set").clicked() {
                    self.duplicate_selected_entity();
                }
            });
            let selected_ids = self.selected_scene_ids();
            match self.session.scene_entity_positions(&selected_ids) {
                Ok(positions) => {
                    ui.label("Common Transform Position (absolute)");
                    ui.horizontal(|ui| {
                        for (axis, label) in ["X", "Y", "Z"].iter().enumerate() {
                            let first = positions.first().map(|(_, position)| position[axis]);
                            let common = first.filter(|first| {
                                positions.iter().all(|(_, position)| {
                                    (position[axis] - *first).abs() <= f64::EPSILON
                                })
                            });
                            ui.label(*label);
                            let mut value = common.unwrap_or(0.0);
                            let response = ui
                                .add(
                                    egui::DragValue::new(&mut value)
                                        .speed(0.1)
                                        .prefix(if common.is_some() { "" } else { "Mixed → " }),
                                )
                                .on_hover_text(if common.is_some() {
                                    "All selected entities share this value"
                                } else {
                                    "Values differ; editing assigns one absolute value to every selected entity"
                                });
                            if response.changed() {
                                let mut axes = [None; 3];
                                axes[axis] = Some(value);
                                let result = self
                                    .session
                                    .set_scene_entity_positions(selected_ids.clone(), axes);
                                self.apply_ui_result(result);
                            }
                        }
                    });
                    ui.small("Use Duplicate Offset for relative placement; absolute edits replace only the changed axis.");
                }
                Err(error) => {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("Common Transform unavailable: {error}"),
                    );
                }
            }
            ui.small("Delete, duplicate, component add/remove, alignment, and distribution are committed as one undo step.");
            ui.separator();
        }

        ui.label(format!("ID: {}", selected.as_str()));
        if let Some(snapshot) = self
            .runtime_state
            .as_mut()
            .and_then(|runtime| runtime.animation_debug_snapshot(&selected))
        {
            show_runtime_animation_debug(ui, &snapshot);
        }

        let rename = inspector_field_row(
            ui,
            "Name",
            "Short lowercase slug used by tools and search",
            |ui| draft_text_value(ui, "entity_name", &entity.name, false),
        );
        if let Some(name) = rename {
            let result = self.session.set_scene_entity_name(selected.clone(), name);
            self.apply_ui_result(result);
        }

        let rename_display = inspector_field_row(
            ui,
            "Display Name",
            "Human-readable label shown in the editor",
            |ui| draft_text_value(ui, "entity_display_name", &entity.display_name, false),
        );
        if let Some(display_name) = rename_display {
            let result = self
                .session
                .set_scene_entity_display_name(selected.clone(), display_name);
            self.apply_ui_result(result);
        }

        ui.label("Description")
            .on_hover_text("Documentation and AI context for this entity");
        if let Some(description) =
            draft_text_value(ui, "entity_description", &entity.description, true)
        {
            let result = self
                .session
                .set_scene_entity_description(selected.clone(), description);
            self.apply_ui_result(result);
        }

        if let Some(info) = self.session.scene().and_then(|scene| {
            crate::prefab_workflow::inspect_prefab_instance(scene, &selected).ok()
        }) {
            ui.separator();
            ui.heading("Prefab Instance");
            // A prefab path has no break opportunities, so the dock's wrap mode
            // would slice it mid-segment across several lines. Truncating keeps
            // the row readable and moves the complete path to the tooltip.
            let source_text = info.source.display().to_string();
            ui.add(egui::Label::new(egui::RichText::new(&source_text).monospace()).truncate())
                .on_hover_text(&source_text);
            if let Some(diagnostic) = info.diagnostic {
                ui.colored_label(egui::Color32::RED, diagnostic);
            }
            let mut apply = false;
            let mut revert = false;
            let mut unpack = false;
            let mut open_prefab = false;
            let mut placement_mode = false;
            control_row(ui, |ui| {
                open_prefab = ui
                    .add_enabled(info.source_valid, egui::Button::new("Open Prefab"))
                    .on_disabled_hover_text("The prefab source is missing or invalid")
                    .clicked();
                apply = ui
                    .add_enabled(info.source_valid, egui::Button::new("Apply"))
                    .clicked();
                revert = ui
                    .add_enabled(info.source_valid, egui::Button::new("Revert"))
                    .clicked();
                unpack = ui.button("Unpack").clicked();
                placement_mode = ui
                    .add_enabled(info.source_valid, egui::Button::new("Place Repeatedly"))
                    .on_disabled_hover_text("The prefab source is missing or invalid")
                    .clicked();
            });
            if placement_mode {
                self.prefab_placement_source = Some(info.source.clone());
            }
            if open_prefab
                && let Some(project) = &self.project_root {
                    let relative = info
                        .source
                        .strip_prefix(project.assets_root())
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|_| info.source.clone());
                    self.reveal_asset_in_browser(&relative);
                }
            if apply {
                let result = self
                    .session
                    .scene()
                    .ok_or(crate::prefab_workflow::PrefabWorkflowError::Session(
                        crate::session::EditorSessionError::NoSceneDocument,
                    ))
                    .and_then(|scene| {
                        crate::prefab_workflow::apply_prefab_overrides(scene, &selected)
                    });
                if let Err(error) = result {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.prefab_apply_failed",
                            error.to_string(),
                        ));
                }
            }
            if revert {
                match crate::prefab_workflow::revert_prefab_instance(&mut self.session, &selected) {
                    Ok(root) => self.select_single_entity(Some(root)),
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.prefab_revert_failed",
                                error.to_string(),
                            ))
                    }
                }
            }
            if unpack
                && let Err(error) =
                    crate::prefab_workflow::unpack_prefab_instance(&mut self.session, &selected)
                {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.prefab_unpack_failed",
                            error.to_string(),
                        ));
                }
        }

        ui.separator();
        let mut set_all_cards = None;
        control_row(ui, |ui| {
            ui.heading("Components");
            if ui
                .small_button("Expand All")
                .on_hover_text("Open every component card on this entity")
                .clicked()
            {
                set_all_cards = Some(true);
            }
            if ui
                .small_button("Collapse All")
                .on_hover_text("Close every component card on this entity")
                .clicked()
            {
                set_all_cards = Some(false);
            }
        });
        if let Some(open) = set_all_cards {
            self.set_all_component_cards(&entity, open);
        }
        self.refresh_inspector_derived_cache();
        let builtins = Arc::clone(
            self.inspector_cache
                .builtins
                .as_ref()
                .expect("Inspector cache refresh must initialize built-ins"),
        );
        let component_catalog = Arc::clone(&self.inspector_cache.component_catalog);
        let entity_choices = Arc::clone(&self.inspector_cache.entity_choices);
        let bone_choices = Arc::clone(&self.inspector_cache.bone_choices);
        let assets_root = self.project_root.as_ref().map(ProjectRoot::assets_root);
        let mut remove_component = None;
        let mut source_action = None;
        let mut copy_component: Option<(ComponentTypeId, Value)> = None;
        let mut paste_component: Option<ComponentTypeId> = None;
        let mut reset_component: Option<ComponentTypeId> = None;
        let mut animation_asset_action = None;
        let mut bake_to_static_mesh = false;
        // Component editors are pure with respect to EditorApp so immutable
        // scene and manifest borrows can remain active while the Inspector is
        // drawn. Store one presentation-only navigation request and apply it
        // after all component edits have released those borrows.
        let reference_navigation = std::cell::RefCell::new(None);
        // The serialized BTreeMap iterates alphabetically, which buries
        // `engine.transform` at the bottom; rank it first like every major
        // editor and keep the rest grouped by category.
        // Prefab linkage is editor metadata with its own dedicated panel
        // above. Showing the same marker as an ordinary component would make
        // it look like a broken user component because it intentionally has
        // no runtime or authoring schema.
        let mut ordered: Vec<(&ComponentTypeId, &Value)> = entity
            .components
            .iter()
            .filter(|(component_type, _)| inspector_lists_component(component_type))
            .collect();
        ordered.sort_by_cached_key(|(component_type, _)| {
            component_display_rank(component_type, &builtins)
        });
        // Whether each card is open is decided before drawing so a collapsed
        // component pays for neither its editors nor the reverse lookups its
        // body would need.
        let open_cards: Vec<bool> = ordered
            .iter()
            .map(|(component_type, _)| {
                component_card_is_open(&self.preferences, component_type, &builtins)
            })
            .collect();
        let mut toggle_card = None;
        for ((component_type, value), card_open) in ordered.into_iter().zip(open_cards) {
            let model_renderers = (card_open
                && component_type.as_str() == engine::scene_bridge::SKINNED_MODEL_COMPONENT)
                .then(|| self.session.model_render_parts(&selected));
            let mut edited_value = value.clone();
            if let Some(pending) = &self.pending_component_drag
                && pending.matches_component(&selected, component_type) {
                    upsert_property_value(&mut edited_value, &pending.path, pending.value.clone());
                }
            let mut edit = None;
            let game_display_name = self
                .game_module
                .as_ref()
                .and_then(|module| module.component_schema(component_type))
                .map(|schema| schema.display_name.clone());
            let is_game_component = game_display_name.is_some();
            let is_known = builtins.get(component_type).is_some() || game_display_name.is_some();
            let header = if is_known {
                inspector_component_header(component_type, game_display_name.as_deref())
            } else {
                "Missing Component Definition".to_owned()
            };
            let header_text = if is_known {
                egui::RichText::new(header).strong()
            } else {
                egui::RichText::new(header)
                    .strong()
                    .color(ui.visuals().warn_fg_color)
            };
            let component_response = inspector_component_card(ui, |ui| {
                egui::CollapsingHeader::new(header_text)
                    // Two unknown components share the same header text, so the
                    // component type must be what separates their open states.
                    .id_salt(component_type.as_str())
                    // The editor owns this state rather than egui: it must
                    // start from the component's declared default and survive
                    // a restart through editor preferences.
                    .open(Some(card_open))
                    .show(ui, |ui| {
                        edit = show_component_value_editor(
                            ui,
                            component_type,
                            &InspectorEditContext {
                                manifest: &self.asset_manifest,
                                assets_root: assets_root.as_deref(),
                                entity_choices: &entity_choices,
                                bone_choices: &bone_choices,
                                project_layers: &self.project_layers,
                                reference_navigation: &reference_navigation,
                            },
                            &mut edited_value,
                            &mut animation_asset_action,
                        );
                        if component_type.as_str()
                            == engine::scene_bridge::SKINNED_MODEL_COMPONENT
                        {
                            ui.separator();
                            let renderers = model_renderers.as_deref().unwrap_or_default();
                            ui.strong(format!("Renderers (read-only): {}", renderers.len()));
                            if renderers.is_empty() {
                                ui.weak("No Skinned Mesh Renderer references this model.");
                            } else {
                                // An imported character binds one renderer per
                                // submesh, so this reverse lookup is as long as
                                // the model is complex. Cap the list instead of
                                // letting it push every other component of the
                                // entity off screen.
                                bounded_list_scroll_area(ui, "skinned_model_renderers", |ui| {
                                    for (renderer, _) in renderers {
                                        let label = entity_choices
                                            .iter()
                                            .find(|choice| &choice.id == renderer)
                                            .map(|choice| choice.label.as_str())
                                            .unwrap_or(renderer.as_str());
                                        ui.label(label).on_hover_text(renderer.as_str());
                                    }
                                });
                            }
                            ui.separator();
                            ui.strong("Conversion");
                            bake_to_static_mesh = ui
                                .button("Bake to Static Mesh")
                                .on_hover_text(
                                    "Create author-owned OBJ meshes in assets/baked_meshes and remove this rig. The scene edit can be undone, but the created mesh assets remain.",
                                )
                                .clicked();
                        }
                    })
            });
            let header_response = component_response.header_response;
            if header_response.clicked() {
                toggle_card = Some(component_type.clone());
            }
            if is_game_component && header_response.double_clicked() {
                source_action = Some(ComponentSourceAction::OpenProject(
                    component_type.as_str().to_owned(),
                ));
            }
            header_response.context_menu(|ui| {
                if ui.button("Copy Component Values").clicked() {
                    copy_component = Some((component_type.clone(), value.clone()));
                    ui.close();
                }
                let paste_matches = self
                    .component_clipboard
                    .as_ref()
                    .is_some_and(|(clipboard_type, _)| clipboard_type == component_type);
                if ui
                    .add_enabled(paste_matches, egui::Button::new("Paste Component Values"))
                    .on_disabled_hover_text("Copy values from a component of the same type first")
                    .clicked()
                {
                    paste_component = Some(component_type.clone());
                    ui.close();
                }
                if is_known
                    && ui
                        .button("Reset to Defaults")
                        .on_hover_text("Replace every field with the schema default value")
                        .clicked()
                {
                    reset_component = Some(component_type.clone());
                    ui.close();
                }
                if ui.button("Remove Component").clicked() {
                    remove_component = Some(component_type.clone());
                    ui.close();
                }
                ui.separator();
                if is_game_component {
                    let location = self.component_source_index.resolve(component_type.as_str());
                    let disabled_reason = if self
                        .component_source_index
                        .is_ambiguous(component_type.as_str())
                    {
                        "Duplicate stable component IDs must be resolved first"
                    } else {
                        "No exact sidecar-backed GameComponent declaration is indexed"
                    };
                    if ui
                        .add_enabled(location.is_some(), egui::Button::new("Open Script"))
                        .on_disabled_hover_text(disabled_reason)
                        .clicked()
                    {
                        source_action = Some(ComponentSourceAction::OpenProject(
                            component_type.as_str().to_owned(),
                        ));
                        ui.close();
                    }
                    if ui
                        .add_enabled(location.is_some(), egui::Button::new("Reveal in Game Code"))
                        .on_disabled_hover_text(disabled_reason)
                        .clicked()
                    {
                        source_action = Some(ComponentSourceAction::RevealProject(
                            component_type.as_str().to_owned(),
                        ));
                        ui.close();
                    }
                } else if builtins.get(component_type).is_some() {
                    if ui.button("View Built-in Source (Read-only)").clicked() {
                        source_action = Some(ComponentSourceAction::ViewBuiltin(
                            component_type.as_str().to_owned(),
                        ));
                        ui.close();
                    }
                    if ui.button("Show Documentation").clicked() {
                        source_action = Some(ComponentSourceAction::ViewBuiltin(
                            component_type.as_str().to_owned(),
                        ));
                        ui.close();
                    }
                }
            });
            if let Some(edit) = edit {
                self.apply_component_edit(selected.clone(), component_type.clone(), value, edit);
            }
        }
        if let Some(component_type) = toggle_card {
            self.toggle_component_card(&component_type);
        }
        if let Some(action) = animation_asset_action {
            self.perform_animation_controller_asset_action(selected.clone(), action);
        }
        if bake_to_static_mesh {
            self.perform_skinned_model_bake(selected.clone());
        }
        if let Some((component_type, value)) = copy_component {
            self.component_clipboard = Some((component_type, value));
        }
        if let Some(component_type) = paste_component {
            self.paste_component_values(&component_type);
        }
        if let Some(component_type) = reset_component {
            self.reset_component_values(&component_type);
        }
        if let Some(action) = source_action {
            self.perform_component_source_action(action);
        }
        if let Some(component_type) = remove_component {
            let targets = self
                .selected_scene_ids()
                .into_iter()
                .filter(|entity_id| {
                    self.session
                        .scene_entity(entity_id)
                        .is_some_and(|item| item.components.contains_key(&component_type))
                })
                .collect::<Vec<_>>();
            let result = self
                .session
                .remove_scene_component_from_entities(targets, component_type);
            self.apply_ui_result(result);
            self.refresh_scene_problems();
        }
        if !ui.input(|input| input.pointer.any_down()) {
            self.commit_pending_component_drag_for_entity(&selected, &entity);
        }

        self.show_game_component_availability_hint(ui);

        let mut available = component_catalog
            .iter()
            .filter(|(schema, _)| !entity.components.contains_key(&schema.type_id))
            .map(|(schema, source)| (schema, *source))
            .collect::<Vec<_>>();
        let has_any_available = !available.is_empty();
        let search = self.component_search.trim().to_ascii_lowercase();
        available.retain(|(schema, source)| {
            search.is_empty()
                || schema.display_name.to_ascii_lowercase().contains(&search)
                || schema
                    .type_id
                    .as_str()
                    .to_ascii_lowercase()
                    .contains(&search)
                || schema.category.to_ascii_lowercase().contains(&search)
                || source.to_ascii_lowercase().contains(&search)
        });
        available.sort_by(|(left_schema, left_source), (right_schema, right_source)| {
            left_source
                .cmp(right_source)
                .then_with(|| left_schema.category.cmp(&right_schema.category))
                .then_with(|| left_schema.display_name.cmp(&right_schema.display_name))
        });

        if has_any_available {
            let toggle = ui.add_sized(
                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                egui::Button::new(if self.add_component_picker_open {
                    "Add Component ⏶"
                } else {
                    "Add Component ⏷"
                }),
            );
            if toggle.clicked() {
                self.add_component_picker_open = !self.add_component_picker_open;
                if !self.add_component_picker_open {
                    self.component_search.clear();
                }
            }
            if self.add_component_picker_open {
                let picker = ui.scope(|ui| self.show_add_component_choices(ui, &search, &available));
                if toggle.clicked() {
                    // The button sits at the end of a long Inspector column, so
                    // the list it just revealed usually starts below the visible
                    // area of the panel.
                    picker.response.scroll_to_me(Some(egui::Align::BOTTOM));
                }
                if let Some(schema) = picker.inner {
                    self.apply_add_component_choice(&schema);
                }
            }
            // Keep the last control clear of the panel edge so it never reads
            // as touching the Asset Browser below.
            ui.add_space(12.0);
        }
        if let Some(navigation) = reference_navigation.into_inner() {
            self.apply_inspector_reference_navigation(navigation);
        }
    }

    /// Flips one component's Inspector card between open and collapsed and
    /// persists the choice.
    ///
    /// Writing the preference file here is acceptable because it happens once
    /// per deliberate click, not per frame.
    fn toggle_component_card(&mut self, component_type: &ComponentTypeId) {
        let builtins = engine::builtin_registry();
        let open = component_card_is_open(&self.preferences, component_type, &builtins);
        self.preferences
            .component_card_open
            .insert(component_type.as_str().to_owned(), !open);
        self.preferences.save();
    }

    /// Opens or closes every component card on one entity at once.
    fn set_all_component_cards(&mut self, entity: &AuthoringEntity, open: bool) {
        for component_type in entity.components.keys() {
            if !inspector_lists_component(component_type) {
                continue;
            }
            self.preferences
                .component_card_open
                .insert(component_type.as_str().to_owned(), open);
        }
        self.preferences.save();
    }

    /// Applies editor-only navigation emitted by an Inspector reference field.
    ///
    /// This runs after component editing for the frame so selecting a linked
    /// entity cannot accidentally redirect a pending edit, paste, or removal
    /// from the entity whose Inspector produced the request.
    fn apply_inspector_reference_navigation(&mut self, navigation: InspectorReferenceNavigation) {
        match navigation {
            InspectorReferenceNavigation::RevealAsset(asset) => {
                let Some(relative_path) = asset_reference_source_path(&self.asset_manifest, &asset)
                else {
                    // Built-in assets intentionally have no physical source
                    // row, so their reference unit remains informative without
                    // pretending the Asset Browser can reveal them.
                    return;
                };
                self.reveal_asset_in_browser(&relative_path);
            }
            InspectorReferenceNavigation::SelectEntity(entity) => {
                if self.session.scene_entity(&entity).is_some() {
                    self.select_single_entity(Some(entity));
                }
            }
            InspectorReferenceNavigation::FocusEntity(entity) => {
                if self.session.scene_entity(&entity).is_none() {
                    return;
                }
                self.select_single_entity(Some(entity.clone()));
                if let Some(scene) = self.session.scene() {
                    let _ = self.scene_view.focus_entity(scene, &entity);
                }
            }
        }
    }

    /// Explains why Game Components may be absent without putting a
    /// project-wide build command inside an entity-specific Inspector.
    fn show_game_component_availability_hint(&self, ui: &mut egui::Ui) {
        let has_game_project = self
            .project_root
            .as_ref()
            .is_some_and(|project| project.game_dir().join("Cargo.toml").is_file());
        if !has_game_project
            || self.game_build.state() != GameBuildState::Idle
            || !self.game_code_is_stale()
        {
            return;
        }

        let message = if self.game_module.is_some() {
            "Game components may be outdated. Use Build next to Play to refresh them."
        } else {
            "Game components are unavailable until Game Code is built. Use Build next to Play."
        };
        ui.colored_label(egui::Color32::YELLOW, message);
    }

    /// Executes the one-way authoring conversion requested from the Skinned
    /// Model Inspector after all immutable scene borrows have been released.
    fn perform_skinned_model_bake(&mut self, entity: EntityId) {
        let Some(project) = self.project_root.clone() else {
            self.report_error(
                "editor.skinned_model_bake_without_project",
                "open a project before baking a Skinned Model",
            );
            return;
        };

        match crate::skinned_model_bake::bake_skinned_model(
            &project,
            &mut self.asset_manifest,
            &mut self.session,
            &entity,
        ) {
            Ok(result) => {
                self.asset_browser.refresh(&project.assets_root());
                self.refresh_scene_problems();
                self.push_notification(
                    EditorNotificationLevel::Success,
                    format!(
                        "Baked {} render part(s) into {} Static Mesh asset(s)",
                        result.render_parts, result.baked_meshes
                    ),
                );
            }
            Err(error) => {
                let code = match &error {
                    crate::skinned_model_bake::SkinnedModelBakeError::ConfiguredController(_) => {
                        crate::skinned_model_bake::CONFIGURED_CONTROLLER_DIAGNOSTIC
                    }
                    crate::skinned_model_bake::SkinnedModelBakeError::BoneAttachment { .. } => {
                        crate::skinned_model_bake::BONE_ATTACHMENT_DIAGNOSTIC
                    }
                    _ => "editor.skinned_model_bake_failed",
                };
                self.report_error(code, error.to_string());
            }
        }
    }

    /// Bones of the rig this entity's Bone Attachment references, as
    /// `(BoneId, name)` in the skeleton's own bone order (ADR 0088 §1).
    ///
    /// Empty when the entity has no attachment, the attachment names no rig,
    /// or the rig's skeleton cannot be read; the bone control then falls back
    /// to showing the stored ID instead of hiding an unresolvable binding.
    fn compute_bone_choices_for_selected_entity(&self) -> Vec<(u32, String)> {
        let Some(project) = self.project_root.as_ref() else {
            return Vec::new();
        };
        let Some(entity) = self
            .selected_entity
            .as_ref()
            .and_then(|id| self.session.scene()?.entity(id))
        else {
            return Vec::new();
        };
        let Some(Value::Object(fields)) = entity.components.get(&ComponentTypeId::new(
            engine::scene_bridge::BONE_ATTACHMENT_COMPONENT,
        )) else {
            return Vec::new();
        };
        let Some(Value::EntityRef(rig)) = fields.get("rig") else {
            return Vec::new();
        };
        let Some(skeleton) = self.session.model_skeleton(rig) else {
            return Vec::new();
        };
        let Some((source, entry)) = self
            .asset_manifest
            .imported_sub_asset(&skeleton)
            .map(|(source, entry, _)| (source.clone(), entry.clone()))
        else {
            return Vec::new();
        };
        let Ok(imported) = engine::import_gltf_path(
            &source,
            &project.assets_root().join(&entry.path),
            &entry.import_settings.skeleton_records,
        ) else {
            return Vec::new();
        };
        imported
            .skins
            .iter()
            .find(|skin| skin.skeleton_id == skeleton)
            .map(|skin| {
                skin.skeleton
                    .bones
                    .iter()
                    .map(|bone| (bone.id.0, bone.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Returns whether one component's Inspector card is currently open.
///
/// The declared default (ADR 0102) applies until the author toggles that
/// component, after which their choice is what persists.
pub(in crate::ui) fn component_card_is_open(
    preferences: &EditorPreferences,
    component_type: &ComponentTypeId,
    builtins: &engine::ComponentRegistry,
) -> bool {
    if let Some(open) = preferences.component_card_open.get(component_type.as_str()) {
        return *open;
    }
    !builtins
        .get(component_type)
        .is_some_and(|definition| definition.default_collapsed)
}

/// Returns the Inspector header for one component.
///
/// Engine-owned components intentionally keep their familiar schema names.
/// Project components use the human-facing name exported by their compiled
/// schema; their opaque stable IDs are not part of the ordinary Inspector UI.
pub(in crate::ui) fn inspector_component_header(
    component_type: &ComponentTypeId,
    game_display_name: Option<&str>,
) -> String {
    game_display_name
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| component_type.as_str().to_owned())
}

/// Returns whether an authored component belongs in the ordinary component list.
///
/// The prefab-instance marker is editor-owned metadata rendered by the
/// dedicated Prefab Instance panel. Unknown user or future components remain
/// visible so a genuinely missing definition can still be diagnosed and
/// recovered without losing its serialized value.
pub(in crate::ui) fn inspector_lists_component(component_type: &ComponentTypeId) -> bool {
    component_type.as_str() != crate::prefab_workflow::EDITOR_PREFAB_INSTANCE_COMPONENT
}

/// Sort key for the Inspector's component list: transform first, then
/// builtins grouped by category, then game/unknown components by type id.
fn component_display_rank(
    component_type: &ComponentTypeId,
    builtins: &engine::ComponentRegistry,
) -> (u8, String, String) {
    if component_type.as_str() == "engine.transform" {
        return (0, String::new(), String::new());
    }
    match builtins.get(component_type) {
        Some(definition) => (
            1,
            definition.schema.category.clone(),
            definition.schema.display_name.clone(),
        ),
        None => (2, String::new(), component_type.as_str().to_owned()),
    }
}

pub(in crate::ui) fn orphan_game_component_diagnostics(
    scene: &AuthoringScene,
    source_index: &ComponentSourceIndex,
    is_compiled_component: impl Fn(&ComponentTypeId) -> bool,
) -> Vec<engine_authoring::Diagnostic> {
    let mut diagnostics = Vec::new();
    for (entity_id, entity) in scene.entities() {
        for component_type in entity.components.keys() {
            if !component_type.as_str().starts_with("game.")
                || is_compiled_component(component_type)
                || source_index.resolve(component_type.as_str()).is_some()
                || source_index.is_ambiguous(component_type.as_str())
            {
                continue;
            }
            diagnostics.push(
                engine_authoring::Diagnostic::error(
                    "editor.scene.orphan_game_component",
                    format!(
                        "entity `{}` references a project component whose source metadata and compiled schema are unavailable",
                        entity.display_name
                    ),
                )
                .with_target(engine_authoring::DiagnosticTarget::Component {
                    entity: entity_id.clone(),
                    component_type: component_type.clone(),
                }),
            );
        }
    }
    diagnostics
}
