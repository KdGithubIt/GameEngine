//! Entity inspector panel and schema-driven component value editors.

use super::*;

/// Describes an action requested by the Animation Controller asset controls.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AnimationControllerAssetAction {
    OpenGraph(AssetId),
    CreateGraph,
    OpenSet(AssetId),
    CreateSet { graph: AssetId },
    OpenPreview,
}

/// Describes editor-only navigation requested by an Inspector reference field.
///
/// Reference navigation never mutates authoring data. Asset references reveal
/// their source in the Asset Browser, while entity references synchronize the
/// Hierarchy selection and may additionally frame the target in Scene View.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InspectorReferenceNavigation {
    RevealAsset(AssetId),
    SelectEntity(EntityId),
    FocusEntity(EntityId),
}

/// Distinguishes assigning a concrete reference from choosing the explicit
/// unassigned row in a searchable reference picker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReferencePickerAction<T> {
    Assign(T),
    Clear,
}

/// Revision-keyed data derived for the entity Inspector.
///
/// The Inspector is drawn every frame, but the scene reference catalog,
/// component catalog, and imported skeleton bone list only change when their
/// owning document, manifest, selection, or game module changes. Keeping the
/// derived values here removes repeated hierarchy walks, schema cloning, and
/// glTF parsing from steady-state UI frames (ADR 0104).
#[derive(Default)]
pub(super) struct InspectorDerivedCache {
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

fn animation_state_playback_mode(
    node: &engine_authoring::Node,
) -> engine_authoring::AnimationStatePlaybackMode {
    let Value::Object(properties) = &node.properties else {
        return engine_authoring::AnimationStatePlaybackMode::Loop;
    };
    properties
        .get(engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY)
        .and_then(|value| match value {
            Value::String(value) => {
                engine_authoring::AnimationStatePlaybackMode::from_persisted_name(value)
            }
            _ => None,
        })
        .unwrap_or_default()
}

impl EditorApp {
    pub(super) fn show_inspector(&mut self, ui: &mut egui::Ui) {
        if self.session.ui_document().is_some() {
            if let Err(error) =
                show_ui_builder_inspector(ui, &mut self.session, &mut self.ui_builder)
            {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.ui_builder_inspector_failed",
                        error.to_string(),
                    ));
            }
            return;
        }
        if self.session.scene().is_some() {
            if self.is_playing() {
                self.show_runtime_entity_inspector(ui);
                return;
            }
            self.show_entity_inspector(ui);
            return;
        }

        ui.heading("Inspector");
        if self.session.is_animation_graph() {
            self.show_animation_graph_preview_action(ui);
        }
        if let Some(selected_edge) = self.session.selected_edge().cloned() {
            self.show_graph_edge_inspector(ui, selected_edge);
            return;
        }
        let Some(selected) = self.session.selected_node().cloned() else {
            ui.label("Select a node or transition to edit it.");
            ui.separator();
            self.show_summary(ui);
            self.property_node = None;
            self.property_text.clear();
            self.state_name_text.clear();
            self.transition_edge = None;
            return;
        };
        let Some(node) = self.session.graph().nodes.get(&selected).cloned() else {
            ui.label("Selected node is missing");
            return;
        };

        if self.property_node.as_ref() != Some(&selected) {
            self.property_node = Some(selected.clone());
            self.property_text = graph_node_property_buffer(&node.node_type, &node.properties);
            self.state_name_text = node.name.clone().unwrap_or_default();
        }

        ui.label(format!("ID: {}", selected.as_str()));
        ui.label(format!("Type: {}", node.node_type.as_str()));
        if let Some(name) = &node.name {
            ui.label(format!("Name: {name}"));
        }
        if node.node_type.as_str() == "anim.entry" {
            ui.separator();
            ui.label("Entry points to the State that starts when the Controller begins playing.");
            ui.label("Use Connect From, then click a State to choose the initial state.");
            return;
        }
        if node.node_type.as_str() == "anim.state" {
            ui.separator();
            self.show_animation_state_inspector(ui, &selected, &node);
            return;
        }
        ui.label("Properties JSON");
        ui.text_edit_multiline(&mut self.property_text);
        if ui.button("Apply properties").clicked() {
            match serde_json::from_str::<Value>(&self.property_text) {
                Ok(value) => {
                    let result = self.session.set_node_property(selected, value);
                    match result {
                        Ok(()) => {
                            self.refresh_property_buffer();
                        }
                        Err(error) => {
                            self.apply_ui_result::<(), _>(Err(error));
                        }
                    }
                }
                Err(error) => {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.invalid_property_json",
                            format!("invalid property JSON: {error}"),
                        ));
                }
            }
        }
    }

    /// Draws the Animation Graph workspace's entry point into the dedicated
    /// preview window.
    ///
    /// The State and transition controls below can only preview what they are
    /// already bound to, and both disable themselves until that binding
    /// exists. Without an unconditional entry point the graph workspace would
    /// have no way of its own to reach the window, leaving only the View menu.
    fn show_animation_graph_preview_action(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("Open Animation Preview")
            .on_hover_text("Inspect one clip, one transition, or the full Animation Graph")
            .clicked()
        {
            self.open_animation_preview_window();
        }
        ui.separator();
    }

    /// Draws the typed controls for one selected Animation State.
    ///
    /// Each control commits its own accepted edit, as required by ADR 0016.
    /// The name field buffers keystrokes and commits when it loses focus, so a
    /// partially typed label never reaches the graph and one rename is one undo
    /// step. Motion Slot and Playback Mode selections have no partial state, so
    /// they commit immediately and need no buffers.
    fn show_animation_state_inspector(
        &mut self,
        ui: &mut egui::Ui,
        selected: &NodeId,
        node: &engine_authoring::Node,
    ) {
        ui.label("State Name");
        let name_response = ui
            .text_edit_singleline(&mut self.state_name_text)
            .on_hover_text("Press Enter or click away to rename the graph node");
        if name_response.lost_focus() {
            self.commit_animation_state_name(selected, node);
        }

        let slots = self.session.motion_slots().unwrap_or_default();
        let mut committed_slot = animation_state_motion_slot(node);
        let selected_text = committed_slot
            .as_ref()
            .and_then(|current| slots.iter().find(|slot| &slot.id == current))
            .map(|slot| slot.display_name.as_str())
            .unwrap_or("(Unassigned)");
        ui.label("Motion Slot");
        let mut picked_slot = None;
        egui::ComboBox::from_id_salt(("animation_state_motion_slot", selected.as_str()))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(committed_slot.is_none(), "(Unassigned)")
                    .clicked()
                {
                    picked_slot = Some(None);
                }
                for slot in &slots {
                    let is_current = committed_slot.as_ref() == Some(&slot.id);
                    if ui
                        .selectable_label(is_current, &slot.display_name)
                        .clicked()
                    {
                        picked_slot = Some(Some(slot.id.clone()));
                    }
                }
            });
        if let Some(picked) = picked_slot.filter(|picked| picked != &committed_slot) {
            let result = self
                .session
                .set_animation_state_motion_slot(selected.clone(), picked.clone());
            match result {
                Ok(()) => {
                    committed_slot = picked;
                    self.refresh_property_buffer();
                }
                Err(error) => self.apply_ui_result::<(), _>(Err(error)),
            }
        }

        let committed_playback_mode = animation_state_playback_mode(node);
        let selected_playback_text = match committed_playback_mode {
            engine_authoring::AnimationStatePlaybackMode::Loop => "Loop",
            engine_authoring::AnimationStatePlaybackMode::Once => "Once",
        };
        ui.label("Playback Mode");
        let mut picked_playback_mode = None;
        egui::ComboBox::from_id_salt(("animation_state_playback_mode", selected.as_str()))
            .selected_text(selected_playback_text)
            .show_ui(ui, |ui| {
                for (mode, label) in [
                    (engine_authoring::AnimationStatePlaybackMode::Loop, "Loop"),
                    (engine_authoring::AnimationStatePlaybackMode::Once, "Once"),
                ] {
                    if ui
                        .selectable_label(committed_playback_mode == mode, label)
                        .clicked()
                    {
                        picked_playback_mode = Some(mode);
                    }
                }
            });
        if let Some(picked) =
            picked_playback_mode.filter(|picked| *picked != committed_playback_mode)
        {
            let result = self
                .session
                .set_animation_state_playback_mode(selected.clone(), picked);
            match result {
                Ok(()) => {
                    self.refresh_property_buffer();
                }
                Err(error) => self.apply_ui_result::<(), _>(Err(error)),
            }
        }

        if let Some(slot) = &committed_slot {
            ui.small(format!("Slot ID: {}", slot.as_str()));
        } else if slots.is_empty() {
            ui.small("Add a Motion Slot in the left dock before assigning this State.");
        }
        if ui
            .add_enabled(
                committed_slot.is_some(),
                egui::Button::new("Preview Bound Motion"),
            )
            .on_disabled_hover_text("This State has no motion slot assigned yet")
            .clicked()
            && let Some(slot) = &committed_slot {
                self.preview_animation_clip(slot.as_str().to_owned());
            }
        ui.small("Bind this stable slot to an imported clip in an Animation Set.");
    }

    /// Commits the buffered Animation State name when it differs from the graph.
    ///
    /// Called on focus loss rather than per keystroke so that one accepted
    /// rename produces exactly one undo step.
    fn commit_animation_state_name(&mut self, selected: &NodeId, node: &engine_authoring::Node) {
        if self.state_name_text.trim() == node.name.as_deref().unwrap_or_default() {
            return;
        }
        let result = self
            .session
            .set_node_name(selected.clone(), self.state_name_text.clone());
        match result {
            Ok(()) => self.refresh_property_buffer(),
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    /// Draws typed controls for a selected graph edge.
    ///
    /// Animation State-to-State edges expose the two runtime annotations
    /// understood by the Animation Graph evaluator. Entry edges are kept
    /// deliberately simple because they only select the initial state.
    fn show_graph_edge_inspector(&mut self, ui: &mut egui::Ui, selected: EdgeId) {
        let Some(edge) = self.session.graph().edges.get(&selected).cloned() else {
            ui.label("Selected transition is missing");
            self.transition_edge = None;
            return;
        };
        let source = self.session.graph().nodes.get(&edge.from.node).cloned();
        let target = self.session.graph().nodes.get(&edge.to.node).cloned();
        let source_label = source
            .as_ref()
            .map(graph_node_display_name)
            .unwrap_or_else(|| edge.from.node.as_str().to_owned());
        let target_label = target
            .as_ref()
            .map(graph_node_display_name)
            .unwrap_or_else(|| edge.to.node.as_str().to_owned());

        ui.strong("Transition");
        ui.label(format!("{source_label}  ->  {target_label}"));
        ui.monospace(selected.as_str());
        ui.separator();

        let source_is_entry = source
            .as_ref()
            .is_some_and(|node| node.node_type.as_str() == "anim.entry");
        if !self.session.is_animation_graph() || source_is_entry {
            if source_is_entry {
                ui.label("This Entry connection selects the initial animation State.");
            } else {
                ui.label("This edge has no typed Inspector for the active graph domain.");
            }
            if ui.button("Delete Transition").clicked() {
                let result = self.session.delete_edge(selected);
                self.apply_ui_result(result);
                self.sync_property_buffer();
            }
            return;
        }

        if self.transition_edge.as_ref() != Some(&selected) {
            self.load_transition_buffer(&selected);
        }

        ui.label("Condition Parameter");
        ui.text_edit_singleline(&mut self.transition_condition_text)
            .on_hover_text(
                "Boolean Animation Controller parameter required to take this transition",
            );
        if self.transition_condition_text.trim().is_empty() {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Blank means unconditional: this transition is taken on the next graph tick.",
            );
        }
        ui.checkbox(
            &mut self.transition_uses_default_fade,
            "Use Controller default fade",
        );
        if !self.transition_uses_default_fade {
            ui.horizontal(|ui| {
                ui.label("Fade Duration");
                ui.add(
                    egui::DragValue::new(&mut self.transition_fade_duration)
                        .range(0.0..=60.0)
                        .speed(0.01)
                        .suffix(" s"),
                );
            });
        }
        control_row(ui, |ui| {
            if ui.button("Apply Transition").clicked() {
                let fade =
                    (!self.transition_uses_default_fade).then_some(self.transition_fade_duration);
                let result = self.session.set_animation_transition(
                    selected.clone(),
                    self.transition_condition_text.clone(),
                    fade,
                );
                match result {
                    Ok(()) => {
                        self.transition_edge = None;
                        self.load_transition_buffer(&selected);
                    }
                    Err(error) => self.apply_ui_result::<(), _>(Err(error)),
                }
            }
            if ui.button("Delete Transition").clicked() {
                let result = self.session.delete_edge(selected);
                self.apply_ui_result(result);
                self.sync_property_buffer();
            }
        });
        let transition_clips = source
            .as_ref()
            .and_then(animation_state_clip_name)
            .zip(target.as_ref().and_then(animation_state_clip_name));
        if let Some((from_clip, to_clip)) = transition_clips {
            if ui.button("Preview Transition").clicked() {
                let fade = if self.transition_uses_default_fade {
                    0.2
                } else {
                    self.transition_fade_duration as f32
                };
                self.preview_animation_transition(from_clip, to_clip, fade);
            }
        } else {
            ui.add_enabled(false, egui::Button::new("Preview Transition"))
                .on_disabled_hover_text("Both States need non-empty Clip Names");
        }
    }

    fn show_runtime_entity_inspector(&mut self, ui: &mut egui::Ui) {
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

    fn show_entity_inspector(&mut self, ui: &mut egui::Ui) {
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
    pub(super) fn show_add_component_choices(
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
    pub(super) fn apply_add_component_choice(
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
    fn paste_component_values(&mut self, component_type: &ComponentTypeId) {
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
    fn reset_component_values(&mut self, component_type: &ComponentTypeId) {
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

    /// Draws lifecycle actions for the two author-owned assets referenced by
    /// an Animation Controller. The controls are rendered inside the
    /// controller's `Rig & Clips` section beside the schema-driven pickers.
    fn show_animation_controller_asset_actions(
        ui: &mut egui::Ui,
        fields: &std::collections::BTreeMap<String, Value>,
    ) -> Option<AnimationControllerAssetAction> {
        let graph = fields.get("graph").and_then(|value| match value {
            Value::AssetRef(asset) => Some(asset.clone()),
            _ => None,
        });
        let animation_set = fields.get("animation_set").and_then(|value| match value {
            Value::AssetRef(asset) => Some(asset.clone()),
            _ => None,
        });

        let mut action = None;
        control_row(ui, |ui| {
            ui.small("Graph");
            action = match graph.clone() {
                Some(graph) if ui.button("Open Graph").clicked() => {
                    Some(AnimationControllerAssetAction::OpenGraph(graph))
                }
                None if ui.button("Create Graph").clicked() => {
                    Some(AnimationControllerAssetAction::CreateGraph)
                }
                _ => None,
            };

            ui.small("Set");
            if action.is_none() {
                action = match animation_set {
                    Some(animation_set) if ui.button("Open Set").clicked() => {
                        Some(AnimationControllerAssetAction::OpenSet(animation_set))
                    }
                    None => {
                        let response = ui
                            .add_enabled(graph.is_some(), egui::Button::new("Create Set"))
                            .on_disabled_hover_text(
                                "Create or assign an Animation Graph before creating its Set",
                            );
                        response
                            .clicked()
                            .then(|| AnimationControllerAssetAction::CreateSet {
                                graph: graph.clone().expect("enabled Create Set requires a Graph"),
                            })
                    }
                    _ => None,
                };
            }
        });

        // The preview samples this controller's rig without entering Play, so
        // it stays reachable even while the Graph or Set reference is still
        // missing; the window itself reports what it cannot resolve.
        if ui
            .button("Open Animation Preview")
            .on_hover_text("Inspect one clip, one transition, or the full Animation Graph")
            .clicked()
        {
            action = Some(AnimationControllerAssetAction::OpenPreview);
        }
        action
    }

    /// Executes one Animation Controller asset action after the Inspector has
    /// released its immutable scene and manifest borrows.
    fn perform_animation_controller_asset_action(
        &mut self,
        entity: EntityId,
        action: AnimationControllerAssetAction,
    ) {
        match action {
            AnimationControllerAssetAction::OpenGraph(graph) => {
                self.open_animation_graph_asset(&graph);
            }
            AnimationControllerAssetAction::CreateGraph => {
                self.create_animation_graph_for_controller(entity);
            }
            AnimationControllerAssetAction::OpenSet(animation_set) => {
                self.open_animation_set_asset(&animation_set);
            }
            AnimationControllerAssetAction::CreateSet { graph } => {
                self.create_animation_set_for_controller(entity, graph);
            }
            AnimationControllerAssetAction::OpenPreview => {
                self.open_animation_preview_for(Some(entity));
            }
        }
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

    /// Assigns a created asset through the same command-backed component edit
    /// path as the ordinary AssetRef picker.
    pub(super) fn assign_animation_controller_asset_reference(
        &mut self,
        entity: EntityId,
        field: &'static str,
        asset: AssetId,
    ) {
        let component_type =
            ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
        let Some(current) = self
            .session
            .scene_entity(&entity)
            .and_then(|entity| entity.components.get(&component_type))
            .cloned()
        else {
            self.report_error(
                "editor.animation_controller_missing",
                "the selected entity no longer has an Animation Controller",
            );
            return;
        };

        self.apply_component_edit(
            entity,
            component_type,
            &current,
            ComponentEdit::Property {
                path: vec![PropertyPathSegment::Field {
                    name: field.to_owned(),
                }],
                value: Value::AssetRef(asset),
            },
        );
    }

    pub(super) fn apply_component_edit(
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

    fn commit_pending_component_drag_for_entity(
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

    pub(super) fn sync_property_buffer(&mut self) {
        if let Some(selected) = self.session.selected_edge().cloned() {
            self.property_node = None;
            self.property_text.clear();
            if self.transition_edge.as_ref() != Some(&selected) {
                self.load_transition_buffer(&selected);
            }
            return;
        }
        self.transition_edge = None;
        let Some(selected) = self.session.selected_node().cloned() else {
            self.property_node = None;
            self.property_text.clear();
            self.state_name_text.clear();
            return;
        };
        if self.property_node.as_ref() == Some(&selected) {
            return;
        }
        let Some(node) = self.session.graph().nodes.get(&selected) else {
            self.property_node = None;
            self.property_text.clear();
            self.state_name_text.clear();
            return;
        };
        self.property_node = Some(selected);
        self.property_text = graph_node_property_buffer(&node.node_type, &node.properties);
        self.state_name_text = node.name.clone().unwrap_or_default();
    }

    /// Loads persisted transition annotations into typed Inspector controls.
    fn load_transition_buffer(&mut self, selected: &EdgeId) {
        let Some(edge) = self.session.graph().edges.get(selected) else {
            self.transition_edge = None;
            self.transition_condition_text.clear();
            self.transition_fade_duration = 0.2;
            self.transition_uses_default_fade = true;
            return;
        };
        self.transition_edge = Some(selected.clone());
        self.transition_condition_text = match edge.annotations.get("condition") {
            Some(Value::String(condition)) => condition.clone(),
            _ => String::new(),
        };
        let fade = edge
            .annotations
            .get("fade_duration")
            .and_then(|value| match value {
                Value::F64(value) => Some(*value),
                Value::I64(value) => Some(*value as f64),
                Value::U64(value) => Some(*value as f64),
                _ => None,
            });
        self.transition_uses_default_fade = fade.is_none();
        self.transition_fade_duration = fade.unwrap_or(0.2);
    }

    fn refresh_property_buffer(&mut self) {
        self.property_node = None;
        self.sync_property_buffer();
    }
}

/// Returns the runtime motion key of one Animation Graph State.
///
/// The key is the State's stable Animation Set slot ID; a State without one
/// has no runtime motion.
fn animation_state_clip_name(node: &engine_authoring::Node) -> Option<String> {
    if node.node_type.as_str() != "anim.state" {
        return None;
    }
    let Value::Object(fields) = &node.properties else {
        return None;
    };
    match fields.get("motion_slot") {
        Some(Value::String(clip)) if !clip.trim().is_empty() => Some(clip.trim().to_owned()),
        _ => None,
    }
}

fn animation_state_motion_slot(
    node: &engine_authoring::Node,
) -> Option<engine_authoring::MotionSlotId> {
    if node.node_type.as_str() != "anim.state" {
        return None;
    }
    let Value::Object(fields) = &node.properties else {
        return None;
    };
    let Some(Value::String(slot)) = fields.get("motion_slot") else {
        return None;
    };
    engine_authoring::MotionSlotId::from_stable_id(engine_authoring::StableId::new(slot)).ok()
}

/// Returns the Inspector buffer text appropriate for one graph node schema.
fn graph_node_property_buffer(
    node_type: &engine_authoring::NodeTypeId,
    properties: &Value,
) -> String {
    if node_type.as_str() == "anim.state" {
        if let Value::Object(properties) = properties
            && let Some(Value::String(name)) = properties.get("motion_name")
        {
            return name.clone();
        }
        return String::new();
    }
    serde_json::to_string_pretty(properties).unwrap_or_else(|_| "null".into())
}

/// Returns a concise graph-canvas/Inspector label for an edge endpoint.
fn graph_node_display_name(node: &engine_authoring::Node) -> String {
    if let Some(name) = node.name.as_deref().filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    match node.node_type.as_str() {
        "anim.entry" => "Entry".to_owned(),
        "anim.state" => graph_node_property_buffer(&node.node_type, &node.properties)
            .trim()
            .to_owned()
            .or_else_non_empty("State"),
        _ => node.node_type.as_str().to_owned(),
    }
}

/// Small local helper for replacing an empty display string with a fallback.
trait NonEmptyString {
    fn or_else_non_empty(self, fallback: &str) -> String;
}

impl NonEmptyString for String {
    fn or_else_non_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

/// Builds the component registry offered by the Add Component picker.
impl super::EditorApp {
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

/// Draws one Inspector component section as a separated card.
///
/// A bare run of collapsing headers reads as a single continuous field list,
/// so the boundary between two components is only visible while every one of
/// them is collapsed. The card gives each component its own background, border,
/// and trailing gap, which keeps the boundary readable when they are expanded.
/// Height at which an in-card list starts scrolling instead of growing.
///
/// Roughly eight rows: enough to read a list at a glance, short enough that
/// the components below the card stay reachable without scrolling past it.
const BOUNDED_LIST_HEIGHT: f32 = 150.0;

/// Draws a list whose length comes from project data inside a fixed-height
/// scroll area.
///
/// The surrounding Inspector is one long column, so an unbounded list pushes
/// every later component out of view; the list scrolls on its own instead.
fn bounded_list_scroll_area(
    ui: &mut egui::Ui,
    salt: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .max_height(BOUNDED_LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, add_contents);
}

/// Returns whether one component's Inspector card is currently open.
///
/// The declared default (ADR 0102) applies until the author toggles that
/// component, after which their choice is what persists.
pub(super) fn component_card_is_open(
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

/// Fixed height of the Add Component choice list.
///
/// Taller than [`BOUNDED_LIST_HEIGHT`] because the grouped list spends two
/// rows on the owner and category headings before the first component, but
/// still short enough to read the panel below it without scrolling far.
pub(super) const ADD_COMPONENT_LIST_HEIGHT: f32 = 260.0;

/// Draws one component choice as a full-width row and reports a click.
///
/// Left-aligned full-width rows keep long component names readable in a narrow
/// Inspector, where a centered button label would be truncated on both sides.
fn add_component_choice_button(ui: &mut egui::Ui, label: &str) -> bool {
    let width = ui.available_width();
    ui.add(
        egui::Button::new(label)
            .truncate()
            .min_size(egui::vec2(width, ui.spacing().interact_size.y)),
    )
    .clicked()
}

fn inspector_component_card<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let fill = ui.visuals().faint_bg_color;
    let stroke = egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color);
    let inner = egui::Frame::group(ui.style())
        .fill(fill)
        .stroke(stroke)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, add_contents)
        .inner;
    ui.add_space(2.0);
    inner
}

/// Returns the Inspector header for one component.
///
/// Engine-owned components intentionally keep their familiar schema names.
/// Project components use the human-facing name exported by their compiled
/// schema; their opaque stable IDs are not part of the ordinary Inspector UI.
pub(super) fn inspector_component_header(
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
pub(super) fn inspector_lists_component(component_type: &ComponentTypeId) -> bool {
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

/// Single-line or multi-line text field that buffers while focused and
/// returns the new text once when focus leaves (Enter included).
///
/// Committing per keystroke floods the undo stack with one step per typed
/// character; Escape discards the buffered draft instead of committing it.
fn draft_text_value(
    ui: &mut egui::Ui,
    salt: &str,
    current: &str,
    multiline: bool,
) -> Option<String> {
    let id = ui.id().with((salt, "text_draft"));
    let mut draft = ui
        .data_mut(|data| data.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_owned());
    let response = if multiline {
        ui.text_edit_multiline(&mut draft)
    } else {
        ui.text_edit_singleline(&mut draft)
    };
    if response.lost_focus() {
        ui.data_mut(|data| data.remove::<String>(id));
        let discard = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if !discard && draft != current {
            return Some(draft);
        }
        return None;
    }
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, draft));
    }
    None
}

pub(super) fn orphan_game_component_diagnostics(
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

pub(super) fn builtin_asset_id(id: &str) -> AssetId {
    AssetId::from_stable_id(engine_authoring::id::StableId::new(id))
        .expect("built-in asset id constants are valid asset ids")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AssetChoice {
    pub(super) label: String,
    pub(super) id: AssetId,
}

/// Returns the physical Asset Browser row that owns an asset reference.
///
/// Imported sub-assets do not have files of their own, so they reveal their
/// model source. Built-in assets return `None` because they intentionally do
/// not appear in the project Asset Browser.
pub(super) fn asset_reference_source_path(
    manifest: &engine::AssetManifest,
    asset: &AssetId,
) -> Option<PathBuf> {
    manifest
        .get(asset)
        .map(|entry| PathBuf::from(&entry.path))
        .or_else(|| {
            manifest
                .iter()
                .find(|(_, entry)| {
                    entry
                        .import_settings
                        .sub_assets
                        .iter()
                        .any(|sub_asset| sub_asset.id == asset.as_str())
                })
                .map(|(_, entry)| PathBuf::from(&entry.path))
        })
}

/// Resolves the best human-readable label for an existing asset reference.
///
/// Picker choices are preferred because they already contain category-aware
/// labels. Manifest lookup is retained as a repair path for missing files,
/// whose entries are deliberately excluded from new picker choices.
fn asset_reference_display_label(
    manifest: &engine::AssetManifest,
    choices: &[AssetChoice],
    asset: &AssetId,
) -> String {
    if let Some(choice) = choices.iter().find(|choice| &choice.id == asset) {
        return choice.label.clone();
    }
    if let Some(entry) = manifest.get(asset) {
        return entry.name.clone().unwrap_or_else(|| entry.path.clone());
    }
    for (_, source) in manifest.iter() {
        if let Some(sub_asset) = source
            .import_settings
            .sub_assets
            .iter()
            .find(|sub_asset| sub_asset.id == asset.as_str())
        {
            let source_label = source.name.as_deref().unwrap_or(&source.path);
            return format!("{source_label} / {}", sub_asset.name);
        }
    }
    asset.as_str().to_owned()
}

/// Maps authoring asset categories onto the same visual family used by the
/// Asset Browser. Imported model parts deliberately inherit the source model's
/// mesh icon because that is the row authors can reveal in the browser.
pub(super) fn asset_reference_browser_kind(
    kind: engine::AssetKind,
) -> crate::asset_browser::AssetKind {
    use crate::asset_browser::AssetKind as BrowserKind;

    match kind {
        engine::AssetKind::Mesh
        | engine::AssetKind::GltfSource
        | engine::AssetKind::Skin
        | engine::AssetKind::Skeleton
        // Morphs and rigid-body rigs have no file of their own; they are
        // revealed on their owning model's row, so they inherit its icon.
        | engine::AssetKind::Morph
        | engine::AssetKind::RigidBodyRig => BrowserKind::Mesh,
        engine::AssetKind::Material => BrowserKind::Material,
        engine::AssetKind::Texture => BrowserKind::Texture,
        engine::AssetKind::AnimationClip => BrowserKind::AnimationClip,
        engine::AssetKind::MotionSource => BrowserKind::MotionSource,
        engine::AssetKind::AnimationGraph | engine::AssetKind::BehaviorTree => BrowserKind::Graph,
        engine::AssetKind::AnimationSet => BrowserKind::AnimationSet,
        engine::AssetKind::Audio => BrowserKind::Audio,
        engine::AssetKind::NavMesh => BrowserKind::NavMesh,
        engine::AssetKind::UiDocument => BrowserKind::UiDocument,
        engine::AssetKind::Prefab => BrowserKind::Prefab,
    }
}

/// Returns the exact symbol and accent color used by the Asset Browser tile
/// for the equivalent authoring asset family.
fn asset_reference_visual(kind: engine::AssetKind) -> (&'static str, egui::Color32) {
    let browser_kind = asset_reference_browser_kind(kind);
    (
        super::assets::asset_kind_icon(browser_kind),
        super::assets::asset_kind_color(browser_kind),
    )
}

/// Width of the object-picker selector at the right edge of a reference row.
pub(super) const REFERENCE_SELECTOR_WIDTH: f32 = 24.0;

/// Width reserved for the trailing remove button of a reference list row.
const REFERENCE_LIST_REMOVE_WIDTH: f32 = 34.0;

/// Height of one compact reference row.
fn reference_row_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y.max(22.0)
}

/// Returns the width a reference field may occupy once `trailing` points are
/// reserved for widgets placed after it on the same row.
pub(super) fn remaining_reference_row_width(
    available_width: f32,
    trailing: f32,
    item_spacing: f32,
) -> f32 {
    (available_width - trailing - item_spacing).max(REFERENCE_SELECTOR_WIDTH)
}

/// Splits one reserved reference row into its body and selector cells.
///
/// The Inspector panel sizes itself from its contents, so a row that allocates
/// more than the width it was offered widens the panel again on every frame.
/// Deriving both cells from a single reserved row keeps that impossible.
pub(super) fn reference_row_rects(row_rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    const CELL_GAP: f32 = 2.0;

    let selector_left = (row_rect.right() - REFERENCE_SELECTOR_WIDTH).max(row_rect.left());
    let body = egui::Rect::from_min_max(
        row_rect.min,
        egui::pos2(
            (selector_left - CELL_GAP).max(row_rect.left()),
            row_rect.bottom(),
        ),
    );
    let selector = egui::Rect::from_min_max(
        egui::pos2(selector_left, row_rect.top()),
        egui::pos2(row_rect.right(), row_rect.bottom()),
    );
    (body, selector)
}

/// Draws a compact single-line object field body and its Unity-style selector
/// button. The body remains a drag-and-drop target while the selector owns the
/// searchable popup.
///
/// Both cells are painted into one reserved row instead of being added as
/// sequential widgets: a button whose text plus padding exceeds its cell would
/// otherwise report a wider desired size and grow the Inspector panel.
pub(super) fn show_compact_reference_field(
    ui: &mut egui::Ui,
    icon: &str,
    icon_color: egui::Color32,
    label: &str,
    tooltip: &str,
) -> (egui::Response, egui::Response) {
    let height = reference_row_height(ui);
    let row_width = ui.available_width().max(REFERENCE_SELECTOR_WIDTH);
    let (row_rect, row_response) =
        ui.allocate_exact_size(egui::vec2(row_width, height), egui::Sense::hover());
    let (field_rect, selector_rect) = reference_row_rects(row_rect);

    let field_response = ui.interact(
        field_rect,
        row_response.id.with("field"),
        egui::Sense::click_and_drag(),
    );
    let visuals = ui.style().interact(&field_response);
    ui.painter().rect(
        field_rect,
        2.0,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        egui::pos2(field_rect.left() + 13.0, field_rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(16.0),
        icon_color,
    );
    ui.painter()
        .with_clip_rect(field_rect.shrink2(egui::vec2(28.0, 2.0)))
        .text(
            egui::pos2(field_rect.left() + 27.0, field_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            visuals.text_color(),
        );

    let selector_response = ui.interact(
        selector_rect,
        row_response.id.with("selector"),
        egui::Sense::click(),
    );
    let selector_visuals = ui.style().interact(&selector_response);
    ui.painter().rect(
        selector_rect,
        2.0,
        selector_visuals.bg_fill,
        selector_visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        selector_rect.center(),
        egui::Align2::CENTER_CENTER,
        "⊙",
        egui::FontId::proportional(14.0),
        selector_visuals.text_color(),
    );

    (
        field_response.on_hover_text(tooltip),
        selector_response.on_hover_text("Open object picker"),
    )
}

/// Draws a searchable object-picker popup anchored to an AssetRef selector.
///
/// `CloseOnClickOutside` is essential here: a normal ComboBox closes on every
/// internal click, including the click needed to focus its search field.
fn show_asset_choice_picker(
    ui: &mut egui::Ui,
    selector_response: &egui::Response,
    current: Option<&AssetId>,
    kind: engine::AssetKind,
    choices: &[AssetChoice],
    manifest: &engine::AssetManifest,
    allow_none: bool,
) -> Option<ReferencePickerAction<AssetId>> {
    let picker_id = selector_response.id.with("asset_reference_picker");
    let search_id = picker_id.with("search");
    let mut search = ui
        .data_mut(|data| data.get_temp::<String>(search_id))
        .unwrap_or_default();
    let mut action = None;
    let (icon, icon_color) = asset_reference_visual(kind);

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
                    .hint_text("Search name, path, or ID...")
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
                .max_height(300.0)
                .show(ui, |ui| {
                    for choice in choices {
                        let source_path = asset_reference_source_path(manifest, &choice.id);
                        let path_text = source_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Built-in engine asset".to_owned());
                        let matches = normalized_search.is_empty()
                            || choice
                                .label
                                .to_ascii_lowercase()
                                .contains(&normalized_search)
                            || path_text.to_ascii_lowercase().contains(&normalized_search)
                            || choice
                                .id
                                .as_str()
                                .to_ascii_lowercase()
                                .contains(&normalized_search);
                        if !matches {
                            continue;
                        }
                        visible_count += 1;
                        let selected = current == Some(&choice.id);
                        let row = ui.horizontal(|ui| {
                            ui.colored_label(icon_color, egui::RichText::new(icon).size(16.0));
                            let clicked = ui.selectable_label(selected, &choice.label).clicked();
                            ui.weak(path_text.as_str());
                            clicked
                        });
                        row.response.on_hover_text(format!(
                            "{}\nStable ID: {}",
                            path_text,
                            choice.id.as_str()
                        ));
                        if row.inner {
                            action = Some(ReferencePickerAction::Assign(choice.id.clone()));
                            ui.close();
                        }
                    }
                });
            if visible_count == 0 {
                ui.weak("No compatible assets match the search.");
            }
        });
    ui.data_mut(|data| data.insert_temp(search_id, search));
    action
}

#[cfg(test)]
pub(super) fn asset_choices_from_manifest(
    component_type: &ComponentTypeId,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AssetChoice> {
    let registry = engine::builtin_registry();
    let Some(engine::InspectorHint::AssetRef { kind }) = registry
        .get(component_type)
        .map(|definition| definition.inspector)
    else {
        return Vec::new();
    };
    asset_choices_for_kind(kind, manifest, assets_root)
}

pub(super) fn asset_choices_for_kind(
    kind: engine::AssetKind,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AssetChoice> {
    let mut choices = match kind {
        engine::AssetKind::Mesh => vec![
            AssetChoice {
                label: "Built-in Triangle".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_TRIANGLE_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Quad".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_QUAD_ASSET_ID),
            },
        ],
        engine::AssetKind::Material => vec![
            AssetChoice {
                label: "Built-in White".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Blue".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Orange".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID),
            },
        ],
        engine::AssetKind::UiDocument => vec![AssetChoice {
            label: "Built-in UI".into(),
            id: builtin_asset_id(engine::scene_bridge::BUILTIN_UI_DOCUMENT_ASSET_ID),
        }],
        _ => Vec::new(),
    };
    choices.extend(
        manifest
            .iter()
            .filter(|(_, entry)| {
                manifest_path_matches_asset_kind(kind, Path::new(&entry.path), assets_root)
                    && asset_file_exists(&entry.path, assets_root)
                    && manifest_source_backs_kind_directly(kind, entry)
            })
            .map(|(id, entry)| AssetChoice {
                label: entry.name.clone().unwrap_or_else(|| entry.path.clone()),
                id: id.clone(),
            }),
    );
    for (source_id, source) in manifest.iter() {
        if !asset_file_exists(&source.path, assets_root) {
            continue;
        }
        let source_label = source.name.as_deref().unwrap_or(&source.path);
        for sub_asset in &source.import_settings.sub_assets {
            if !imported_sub_asset_matches_picker_kind(sub_asset.kind, kind) {
                continue;
            }
            if is_legacy_motion_clip_alias(source_id, sub_asset) {
                continue;
            }
            let stable = engine_authoring::StableId::new(&sub_asset.id);
            let Ok(id) = AssetId::from_stable_id(stable) else {
                // Malformed persisted IDs are surfaced by manifest/import
                // validation; the picker must not manufacture an unusable ref.
                continue;
            };
            let target_label = sub_asset
                .target_model_source
                .as_deref()
                .and_then(|target| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
                })
                .and_then(|target| manifest.get(&target))
                .map(|entry| entry.name.as_deref().unwrap_or(&entry.path));
            let label = match target_label {
                Some(target) => format!("{source_label} / {} — {target}", sub_asset.name),
                None => format!("{source_label} / {}", sub_asset.name),
            };
            choices.push(AssetChoice { label, id });
        }
    }
    choices
}

/// Returns whether a catalog row is the hidden compatibility alias retained
/// for an Animation Set authored before target-specific VMD clip IDs existed.
pub(super) fn is_legacy_motion_clip_alias(
    source_id: &AssetId,
    sub_asset: &engine::ImportedSubAsset,
) -> bool {
    if sub_asset.kind != engine::ImportedSubAssetKind::Animation {
        return false;
    }
    let Some(target) = sub_asset.target_model_source.as_deref() else {
        return false;
    };
    let Ok(target) = AssetId::from_stable_id(engine_authoring::StableId::new(target)) else {
        return false;
    };
    sub_asset.id
        != engine::imported_motion_sub_asset_id(source_id, &target, sub_asset.index as usize)
            .as_str()
}

/// Returns whether a model source file may be referenced directly as `kind`,
/// rather than only through the sub-assets its import produced.
///
/// `asset_path_matches_kind` accepts a model source for every kind its
/// extension can carry, so this narrows that answer to the references the
/// engine actually resolves.
fn manifest_source_backs_kind_directly(
    kind: engine::AssetKind,
    entry: &engine::ManifestEntry,
) -> bool {
    let path = Path::new(&entry.path);
    if kind == engine::AssetKind::AnimationClip {
        // Model and motion sources only produce clips through import. Exposing
        // either top-level source here lets Animation Sets persist a reference
        // that runtime validation must reject instead of the imported clip.
        return !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path)
            && !engine::asset_path_matches_kind(engine::AssetKind::MotionSource, path);
    }
    if !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path) {
        return true;
    }
    match kind {
        // An imported model hands its geometry to the sub-asset rows, but a
        // source that was never imported stays selectable so a plain mesh
        // file keeps working.
        engine::AssetKind::Mesh => entry.import_settings.sub_assets.is_empty(),
        _ => true,
    }
}

pub(super) fn imported_sub_asset_matches_picker_kind(
    imported: engine::ImportedSubAssetKind,
    requested: engine::AssetKind,
) -> bool {
    matches!(
        (imported, requested),
        (engine::ImportedSubAssetKind::Mesh, engine::AssetKind::Mesh)
            | (
                engine::ImportedSubAssetKind::Material,
                engine::AssetKind::Material
            )
            | (
                engine::ImportedSubAssetKind::Texture,
                engine::AssetKind::Texture
            )
            | (
                engine::ImportedSubAssetKind::Skeleton,
                engine::AssetKind::Skeleton
            )
            | (
                // Validation already accepts animation sub-asset references
                // for clip fields; without this arm the picker could not
                // offer them at all.
                engine::ImportedSubAssetKind::Animation,
                engine::AssetKind::AnimationClip
            )
            | (engine::ImportedSubAssetKind::Skin, engine::AssetKind::Skin)
            | (
                engine::ImportedSubAssetKind::RigidBodyRig,
                engine::AssetKind::RigidBodyRig
            )
    )
}

/// Whether an Asset Browser drag payload can be assigned to a field of the
/// given engine asset kind.
fn payload_matches_engine_kind(
    payload: &crate::drag_drop::DragPayload,
    kind: engine::AssetKind,
) -> bool {
    use crate::asset_browser::AssetKind as BrowserKind;
    matches!(
        (payload.kind, kind),
        (BrowserKind::Mesh, engine::AssetKind::Mesh)
            | (BrowserKind::Material, engine::AssetKind::Material)
            | (BrowserKind::Texture, engine::AssetKind::Texture)
            | (BrowserKind::AnimationSet, engine::AssetKind::AnimationSet)
            | (BrowserKind::AnimationClip, engine::AssetKind::AnimationClip)
    )
}

/// Accepts an Asset Browser drop on an AssetRef widget.
///
/// Returns the dropped asset when the payload kind matches; a hover with a
/// compatible payload highlights the widget so the drop target is obvious.
fn asset_drop_on_response(
    ui: &egui::Ui,
    response: &egui::Response,
    kind: engine::AssetKind,
) -> Option<AssetId> {
    if let Some(payload) = response.dnd_hover_payload::<crate::drag_drop::DragPayload>()
        && payload_matches_engine_kind(&payload, kind) {
            ui.painter().rect_stroke(
                response.rect,
                2.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 180, 255)),
                egui::StrokeKind::Outside,
            );
        }
    response
        .dnd_release_payload::<crate::drag_drop::DragPayload>()
        .filter(|payload| payload_matches_engine_kind(payload, kind))
        .map(|payload| payload.asset_id.clone())
}

/// Returns whether a registered asset still has its backing file.
///
/// A file deleted outside the editor leaves its manifest entry behind, and
/// offering that entry would let the author create a reference that is broken
/// the moment it is made. Existing references keep resolving to the entry and
/// are reported by scene validation instead.
///
/// Without an assets root there is nothing to check against, so entries are
/// kept; the picker must stay usable in tests and in previews that run
/// without a project on disk.
fn asset_file_exists(relative_path: &str, assets_root: Option<&Path>) -> bool {
    match assets_root {
        Some(root) => root.join(relative_path).is_file(),
        None => true,
    }
}

pub(super) fn manifest_path_matches_asset_kind(
    kind: engine::AssetKind,
    relative_path: &Path,
    assets_root: Option<&Path>,
) -> bool {
    if !engine::asset_path_matches_kind(kind, relative_path) {
        return false;
    }
    match kind {
        engine::AssetKind::AnimationGraph => {
            graph_asset_has_kind(relative_path, assets_root, "anim.graph")
        }
        engine::AssetKind::BehaviorTree => {
            graph_asset_has_kind(relative_path, assets_root, "behavior_tree.graph")
        }
        _ => true,
    }
}

fn graph_asset_has_kind(
    relative_path: &Path,
    assets_root: Option<&Path>,
    expected_kind: &str,
) -> bool {
    let is_graph = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".graph.json"));
    if !is_graph {
        return false;
    }
    let Some(assets_root) = assets_root else {
        // Tests and unopened-project fallback can still use conventional,
        // unambiguous suffixes without touching the filesystem.
        let name = relative_path.to_string_lossy().to_ascii_lowercase();
        return match expected_kind {
            "anim.graph" => name.ends_with(".anim.graph.json"),
            "behavior_tree.graph" => {
                name.ends_with(".behavior.graph.json") || name.ends_with(".bt.graph.json")
            }
            _ => false,
        };
    };
    std::fs::read_to_string(assets_root.join(relative_path))
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|document| document.get("kind")?.as_str().map(str::to_owned))
        .is_some_and(|kind| kind == expected_kind)
}

fn show_asset_reference_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
    allow_none: bool,
    clear_requested: &mut bool,
) -> Option<ComponentEdit> {
    let Value::AssetRef(current) = value else {
        ui.colored_label(egui::Color32::RED, "invalid asset reference");
        return None;
    };
    let choices = asset_choices_for_kind(kind, context.manifest, context.assets_root);
    let source_path = asset_reference_source_path(context.manifest, current);
    let label = asset_reference_display_label(context.manifest, &choices, current);
    let known = choices.iter().any(|choice| choice.id == *current) || source_path.is_some();
    let backing_file_missing = source_path.as_ref().is_some_and(|path| {
        context
            .assets_root
            .is_some_and(|assets_root| !assets_root.join(path).is_file())
    });
    let (icon, mut icon_color) = asset_reference_visual(kind);
    if !known {
        icon_color = egui::Color32::from_rgb(230, 92, 92);
    }
    let field_label = if known {
        label
    } else {
        format!("Missing: {}", current.as_str())
    };
    let location = match &source_path {
        Some(path) if backing_file_missing => format!("{} (file missing)", path.display()),
        Some(path) => path.display().to_string(),
        None if known => "Built-in engine asset".to_owned(),
        None => "The referenced asset is not registered.".to_owned(),
    };
    let tooltip = format!("{location}\nStable ID: {}", current.as_str());
    let (field_response, selector_response) =
        show_compact_reference_field(ui, icon, icon_color, &field_label, &tooltip);

    if field_response.clicked() && source_path.is_some() {
        context
            .reference_navigation
            .replace(Some(InspectorReferenceNavigation::RevealAsset(
                current.clone(),
            )));
    }

    let mut action = show_asset_choice_picker(
        ui,
        &selector_response,
        Some(current),
        kind,
        &choices,
        context.manifest,
        allow_none,
    );
    if let Some(dropped) = asset_drop_on_response(ui, &field_response, kind)
        .or_else(|| asset_drop_on_response(ui, &selector_response, kind))
    {
        action = Some(ReferencePickerAction::Assign(dropped));
    }

    match action {
        Some(ReferencePickerAction::Assign(selected)) if selected != *current => {
            Some(ComponentEdit::Property {
                path: Vec::new(),
                value: Value::AssetRef(selected),
            })
        }
        Some(ReferencePickerAction::Clear) if allow_none => {
            *clear_requested = true;
            None
        }
        _ => None,
    }
}

pub(super) enum ComponentEdit {
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
pub(super) struct PendingComponentDrag {
    pub(super) entity: EntityId,
    pub(super) component_type: ComponentTypeId,
    pub(super) path: Vec<PropertyPathSegment>,
    pub(super) value: Value,
}

impl PendingComponentDrag {
    fn matches_component(&self, entity: &EntityId, component_type: &ComponentTypeId) -> bool {
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
    pub(super) fn scene_preview(&self, current_component: &Value) -> Option<SceneComponentPreview> {
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

/// Shows the editor for one component value.
///
/// Asset reference components with known built-in assets get a whole-value
/// picker; every other value falls through to field-level property editing.
fn show_component_value_editor(
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

/// Committed edit shape mirrored onto the rest of the selection.
enum PropagatedEdit {
    Whole(Value),
    Property {
        path: Vec<PropertyPathSegment>,
        value: Value,
    },
}

/// Numeric `_r`/`_g`/`_b` field triples that render as one color swatch.
struct ColorTripleGroups {
    /// Group description keyed by the red field's name.
    by_red: std::collections::BTreeMap<String, ColorTriple>,
    /// Green/blue member field names suppressed from per-field rows.
    members: std::collections::BTreeSet<String>,
}

struct ColorTriple {
    label: String,
    green: String,
    blue: String,
}

/// Detects `<prefix>_r/_g/_b` triples whose current values are plain floats
/// inside [0, 1]. HDR values above 1 keep their numeric rows because the
/// color picker would silently clamp them.
fn color_triple_groups(
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
fn show_color_triple_editor(
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

struct InspectorEditContext<'a> {
    manifest: &'a engine::AssetManifest,
    assets_root: Option<&'a Path>,
    entity_choices: &'a [EntityChoice],
    /// Bones of the rig the edited component references, as `(BoneId, name)`.
    ///
    /// Empty when the component references no rig or the rig's skeleton
    /// cannot be read; the control then shows the stored ID so a binding
    /// stays visible and repairable (ADR 0088 1).
    bone_choices: &'a [(u32, String)],
    project_layers: &'a [engine_authoring::project_settings::Layer],
    /// Presentation-only navigation emitted by a reference unit.
    ///
    /// Interior mutability lets deeply nested schema controls request
    /// navigation while retaining an immutable shared Inspector context.
    reference_navigation: &'a std::cell::RefCell<Option<InspectorReferenceNavigation>>,
}

/// One selectable target for an entity-reference field.
///
/// Carrying the component types alongside the label is what lets a reference
/// field offer only entities it can actually point at (ADR 0087 4); a label
/// alone cannot express "entities that own a rig".
#[derive(Clone)]
struct EntityChoice {
    id: EntityId,
    label: String,
    /// Human-readable path from a scene root to this entity.
    hierarchy_path: String,
    components: Vec<ComponentTypeId>,
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
fn entity_reference_display_label(entity: &AuthoringEntity) -> String {
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
pub(super) fn entity_reference_hierarchy_path(scene: &AuthoringScene, entity: &EntityId) -> String {
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

/// Organizes the Animation Controller's long field list by authoring task.
///
/// Rig and playback settings stay open because they are the common path.
/// Variable-length event and parameter editors start folded so one controller
/// does not consume the entire Inspector before the author needs those lists.
fn show_animation_controller_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
    schema: &engine_authoring::ComponentSchema,
    hints: &[engine::FieldDef],
    context: &InspectorEditContext<'_>,
    animation_asset_action: &mut Option<AnimationControllerAssetAction>,
) -> Option<ComponentEdit> {
    const RIG_AND_CLIPS: &[&str] = &["enabled", "skeleton", "animation_set", "graph"];
    const PLAYBACK: &[&str] = &[
        "looping",
        "playback_speed",
        "completion_event",
        "root_motion_mode",
        "fade_duration",
    ];
    const PARAMETERS: &[&str] = &["parameters"];
    const GROUPED: &[&str] = &[
        "enabled",
        "skeleton",
        "animation_set",
        "graph",
        "looping",
        "playback_speed",
        "completion_event",
        "root_motion_mode",
        "fade_duration",
        "parameters",
    ];

    for (title, names, default_open) in [
        ("Rig & Clips", RIG_AND_CLIPS, true),
        ("Playback", PLAYBACK, true),
        ("Parameters", PARAMETERS, false),
    ] {
        let mut section_edit = None;
        egui::CollapsingHeader::new(title)
            .id_salt((schema.type_id.as_str(), title))
            .default_open(default_open)
            .show(ui, |ui| {
                section_edit =
                    show_schema_fields_editor(ui, fields, schema, hints, context, Some(names));
                if title == "Rig & Clips" {
                    show_animation_controller_assignment_notice(ui, fields);
                    if animation_asset_action.is_none() {
                        *animation_asset_action =
                            EditorApp::show_animation_controller_asset_actions(ui, fields);
                    }
                }
            });
        if section_edit.is_some() {
            return section_edit;
        }
    }

    let additional = schema
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .filter(|name| !GROUPED.contains(name))
        .collect::<Vec<_>>();
    if additional.is_empty() {
        return None;
    }

    let mut section_edit = None;
    egui::CollapsingHeader::new("Additional Settings")
        .id_salt((schema.type_id.as_str(), "additional"))
        .default_open(true)
        .show(ui, |ui| {
            section_edit =
                show_schema_fields_editor(ui, fields, schema, hints, context, Some(&additional));
        });
    section_edit
}

/// Explains whether an Animation Controller will animate or stay in its rest pose.
///
/// Animation Graph and Animation Set references are a pair under ADR 0085.
/// Keeping this notice beside their pickers makes the inactive or invalid
/// state visible without requiring the author to switch to the Problems panel.
fn show_animation_controller_assignment_notice(
    ui: &mut egui::Ui,
    fields: &std::collections::BTreeMap<String, Value>,
) {
    let has_animation_set = matches!(fields.get("animation_set"), Some(Value::AssetRef(_)));
    let has_graph = matches!(fields.get("graph"), Some(Value::AssetRef(_)));
    match (has_animation_set, has_graph) {
        (false, false) => {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Rest pose only. Assign both an Animation Set and Animation Graph to enable playback.",
            );
        }
        (false, true) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 92, 92),
                "Animation Set is required when an Animation Graph is assigned.",
            );
        }
        (true, false) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 92, 92),
                "Animation Graph is required when an Animation Set is assigned.",
            );
        }
        (true, true) => {}
    }
}

/// Draws either every schema field or one named subset.
///
/// Keeping the field renderer shared means grouped components retain the same
/// validation, conditional visibility, drag buffering, and command output as
/// ordinary generated Inspectors.
fn show_schema_fields_editor(
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

/// Returns whether the Inspector may remove a scalar reference field.
///
/// Optional entity references already use an absent field as their
/// unassigned state. Asset references also permit that state when they have
/// no schema default: optional references remain unassigned, while required
/// references become inactive with the non-blocking ADR 0069 diagnostic.
/// Asset references with a default are intentionally excluded because
/// removing them means "use the default asset", not "use no asset".
pub(super) fn field_reference_can_be_unassigned(field: &engine_authoring::FieldSchema) -> bool {
    match field.field_type {
        engine_authoring::FieldType::EntityRef => !field.required,
        engine_authoring::FieldType::AssetRef => field.default_value.is_none(),
        _ => false,
    }
}

/// Width at which a generated Inspector row can keep both columns useful.
const INSPECTOR_INLINE_FIELD_MIN_WIDTH: f32 = 260.0;

/// Lays out a generated Inspector field without squeezing either column.
///
/// Wide docks retain the familiar label-and-editor row. Narrow docks place the
/// editor below its label, giving reference lists and other compound controls
/// the complete viewport width instead of compressing text into vertical runs.
pub(super) fn inspector_field_row<R>(
    ui: &mut egui::Ui,
    display_name: &str,
    description: &str,
    add_editor: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if ui.available_width() >= INSPECTOR_INLINE_FIELD_MIN_WIDTH {
        ui.horizontal(|ui| {
            show_inspector_field_label(ui, display_name, description);
            add_editor(ui)
        })
        .inner
    } else {
        ui.vertical(|ui| {
            let label_width = ui.available_width();
            ui.add_sized(
                [label_width, 20.0],
                egui::Label::new(display_name).truncate(),
            )
            .on_hover_text(description);
            add_editor(ui)
        })
        .inner
    }
}

/// Allocates a stable label column for a wide generated Inspector row.
fn show_inspector_field_label(
    ui: &mut egui::Ui,
    display_name: &str,
    description: &str,
) -> egui::Response {
    let width = (ui.available_width() * 0.34).clamp(88.0, 132.0);
    ui.add_sized([width, 20.0], egui::Label::new(display_name).truncate())
        .on_hover_text(description)
}

/// Presents the complete built-in transform as familiar grouped vectors.
/// Missing v2 fields use their compatibility defaults until the user edits
/// them, so simply inspecting a schema-v1 scene does not rewrite the document.
fn show_transform_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
) -> Option<ComponentEdit> {
    let groups = [
        (
            "Position",
            " m",
            [("x", "X", 0.0), ("y", "Y", 0.0), ("z", "Z", 0.0)],
        ),
        (
            "Rotation",
            "°",
            [
                ("rotation_x_degrees", "X", 0.0),
                ("rotation_y_degrees", "Y", 0.0),
                ("rotation_z_degrees", "Z", 0.0),
            ],
        ),
        (
            "Scale",
            "×",
            [
                ("scale_x", "X", 1.0),
                ("scale_y", "Y", 1.0),
                ("scale_z", "Z", 1.0),
            ],
        ),
    ];

    for (title, suffix, axes) in groups {
        let mut group_edit = None;
        // Reserve the complete row before placing any controls. A normal
        // horizontal layout grows downward as taller widgets are encountered,
        // which can give later DragValue controls a different vertical origin.
        let row_width = ui.available_width();
        let axis_spacing = ui.spacing().item_spacing.x;
        let row_height = transform_row_height(ui);
        let (row_rect, _) =
            ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());
        let cell_rects = transform_row_rects(row_rect, axis_spacing);

        // Every cell uses the same precomputed vertical bounds. `place` paints
        // inside those bounds without advancing or resizing the parent row.
        let _ = ui.place(
            cell_rects[0],
            egui::Label::new(egui::RichText::new(title).strong()).truncate(),
        );

        for ((key, axis, default), cell_rect) in
            axes.into_iter().zip(cell_rects.into_iter().skip(1))
        {
            // Compatibility defaults remain transient until the author edits
            // an axis, preserving older scene documents exactly as before.
            let mut numeric = fields
                .get(key)
                .and_then(numeric_value_as_f64)
                .unwrap_or(default);

            // Position the control inside its fixed cell so X, Y, and Z share
            // an identical top edge, center line, and bottom edge.
            let response = ui.place(
                cell_rect,
                egui::DragValue::new(&mut numeric)
                    .speed(if title == "Rotation" { 0.5 } else { 0.05 })
                    .prefix(format!("{axis} "))
                    .suffix(suffix),
            );

            // Keep the existing drag buffering and property-command behavior.
            // Only the visual allocation changes; authoring state flow does not.
            if group_edit.is_none() {
                group_edit = numeric_drag_response(response, &numeric, |value| Value::F64(*value))
                    .map(|edit| {
                        prepend_property_segment(
                            edit,
                            PropertyPathSegment::Field { name: key.into() },
                        )
                    });
            }
        }
        if group_edit.is_some() {
            return group_edit;
        }
    }
    None
}

/// Returns enough height for both the configured DragValue text and padding.
///
/// Computing this before allocating the row prevents an individual control
/// from enlarging the row after earlier controls have already been positioned.
fn transform_row_height(ui: &egui::Ui) -> f32 {
    const MINIMUM_ROW_HEIGHT: f32 = 22.0;

    // DragValue switches between a button and a text editor. Both modes must
    // fit the same row so focusing a value cannot move the surrounding axes.
    let text_height = ui.text_style_height(&ui.style().drag_value_text_style);
    let padded_text_height = text_height + ui.spacing().button_padding.y * 2.0;

    MINIMUM_ROW_HEIGHT
        .max(ui.spacing().interact_size.y)
        .max(padded_text_height)
}

/// Returns the label and per-axis widths for one compact Transform row.
pub(super) fn transform_row_widths(row_width: f32, item_spacing: f32) -> (f32, f32) {
    let title_width = (row_width * 0.20).clamp(58.0, 76.0);
    // Keep vector controls compact when the Inspector is wide. The minimum
    // preserves usability in a narrow dock, while the maximum prevents X, Y,
    // and Z from stretching merely to consume otherwise unused space.
    let axis_width = ((row_width - title_width - item_spacing * 3.0) / 3.0).clamp(44.0, 92.0);
    (title_width, axis_width)
}

/// Divides one reserved Transform row into a title cell and three axis cells.
///
/// All returned rectangles deliberately preserve the row's vertical bounds.
/// This invariant prevents sequential egui layout growth from creating a
/// staircase across the X, Y, and Z controls.
pub(super) fn transform_row_rects(row_rect: egui::Rect, item_spacing: f32) -> [egui::Rect; 4] {
    let (title_width, axis_width) = transform_row_widths(row_rect.width(), item_spacing);
    let widths = [title_width, axis_width, axis_width, axis_width];
    let mut next_left = row_rect.left();

    std::array::from_fn(|index| {
        // Every cell starts at the row's top and uses the full row height.
        // Only the horizontal origin and width differ between cells.
        let cell_rect = egui::Rect::from_min_size(
            egui::pos2(next_left, row_rect.top()),
            egui::vec2(widths[index], row_rect.height()),
        );

        // Advance to the next column while retaining the editor-wide spacing.
        next_left = cell_rect.right() + item_spacing;
        cell_rect
    })
}

fn show_unassigned_asset_reference_editor(
    ui: &mut egui::Ui,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
) -> Option<AssetId> {
    let choices = asset_choices_for_kind(kind, context.manifest, context.assets_root);
    let (icon, icon_color) = asset_reference_visual(kind);
    let (field_response, selector_response) =
        show_compact_reference_field(ui, icon, icon_color, "None", "No asset is assigned.");
    let mut action = show_asset_choice_picker(
        ui,
        &selector_response,
        None,
        kind,
        &choices,
        context.manifest,
        true,
    );
    if let Some(dropped) = asset_drop_on_response(ui, &field_response, kind)
        .or_else(|| asset_drop_on_response(ui, &selector_response, kind))
    {
        action = Some(ReferencePickerAction::Assign(dropped));
    }
    match action {
        Some(ReferencePickerAction::Assign(asset)) => Some(asset),
        Some(ReferencePickerAction::Clear) | None => None,
    }
}

pub(super) fn value_matches_field_type(
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

pub(super) fn inspector_condition_matches(
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

fn show_schema_field_editor(
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

/// Edits a deterministic ordered list of same-kind asset references.
fn show_asset_reference_list_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
) -> Option<ComponentEdit> {
    let Value::Array(values) = value else {
        ui.colored_label(egui::Color32::RED, "invalid asset reference list");
        return None;
    };
    let mut replacement = values.clone();
    let mut changed = false;
    let mut remove = None;
    // An imported model contributes one slot per submesh, so this list is as
    // long as the source material set and must not grow without bound.
    bounded_list_scroll_area(ui, "asset_reference_list", |ui| {
        for (index, item) in replacement.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("#{index}"));
                let mut ignored_clear = false;
                // The reference field fills the row it is given, so the remove
                // button's own space is reserved before the field is drawn.
                let height = reference_row_height(ui);
                let reference_width = remaining_reference_row_width(
                    ui.available_width(),
                    REFERENCE_LIST_REMOVE_WIDTH,
                    ui.spacing().item_spacing.x,
                );
                let reference_edit = ui
                    .allocate_ui(egui::vec2(reference_width, height), |ui| {
                        show_asset_reference_editor(
                            ui,
                            item,
                            kind,
                            context,
                            false,
                            &mut ignored_clear,
                        )
                    })
                    .inner;
                if let Some(ComponentEdit::Property { value, .. }) = reference_edit {
                    *item = value;
                    changed = true;
                }
                if ui
                    .add_sized(
                        [REFERENCE_LIST_REMOVE_WIDTH, height],
                        egui::Button::new("−").small(),
                    )
                    .clicked()
                {
                    remove = Some(index);
                }
            });
        }
    });
    if let Some(index) = remove {
        replacement.remove(index);
        changed = true;
    }
    if ui.small_button("+ Add Material Slot").clicked() {
        let default = asset_choices_for_kind(kind, context.manifest, context.assets_root)
            .into_iter()
            .next()
            .map(|choice| Value::AssetRef(choice.id));
        if let Some(default) = default {
            replacement.push(default);
            changed = true;
        }
    }
    changed.then(|| ComponentEdit::Property {
        path: Vec::new(),
        value: Value::Array(replacement),
    })
}

/// Edits animation graph condition defaults without exposing raw JSON keys.
fn show_string_bool_map_editor(ui: &mut egui::Ui, value: &mut Value) -> Option<ComponentEdit> {
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
fn show_lod_levels_editor(
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
fn show_bone_reference_editor(
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

/// Edits one entity reference, offering only entities the field accepts.
///
/// `required` names the component types a target must carry (ADR 0087 4).
/// The entity currently stored stays visible even when it does not qualify,
/// so an out-of-band edit is visible and repairable rather than silently
/// replaced; scene validation reports it as
/// `scene.entity_reference_wrong_target`.
fn show_entity_reference_editor(
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
fn show_unassigned_entity_reference_editor(
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

fn show_layer_mask_editor(
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

fn show_typed_array_editor(
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

pub(super) fn default_value_for_field_type(
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

fn show_property_value_editor(ui: &mut egui::Ui, value: &mut Value) -> Option<ComponentEdit> {
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

fn numeric_value_as_f64(value: &Value) -> Option<f64> {
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

fn drag_value_edit_in_range<T>(
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

fn numeric_drag_response<T>(
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

fn prepend_property_segment(edit: ComponentEdit, segment: PropertyPathSegment) -> ComponentEdit {
    match edit {
        ComponentEdit::Property { mut path, value } => {
            path.insert(0, segment);
            ComponentEdit::Property { path, value }
        }
        ComponentEdit::DraftProperty { mut path, value } => {
            path.insert(0, segment);
            ComponentEdit::DraftProperty { path, value }
        }
        ComponentEdit::CommitDraft { mut path } => {
            path.insert(0, segment);
            ComponentEdit::CommitDraft { path }
        }
        ComponentEdit::Whole(value) => ComponentEdit::Whole(value),
    }
}

pub(super) fn upsert_property_value(
    value: &mut Value,
    path: &[PropertyPathSegment],
    replacement: Value,
) -> bool {
    let Some((first, rest)) = path.split_first() else {
        *value = replacement;
        return true;
    };

    match first {
        PropertyPathSegment::Field { name } => match value {
            Value::Object(fields) if rest.is_empty() => {
                fields.insert(name.clone(), replacement);
                true
            }
            Value::Object(fields) => {
                let Some(child) = fields.get_mut(name) else {
                    return false;
                };
                upsert_property_value(child, rest, replacement)
            }
            _ => false,
        },
        PropertyPathSegment::Index { index } => match value {
            Value::Array(values) => {
                let Some(child) = values.get_mut(*index) else {
                    return false;
                };
                upsert_property_value(child, rest, replacement)
            }
            _ => false,
        },
    }
}

pub(super) fn property_value<'a>(
    value: &'a Value,
    path: &[PropertyPathSegment],
) -> Option<&'a Value> {
    let Some((first, rest)) = path.split_first() else {
        return Some(value);
    };

    match first {
        PropertyPathSegment::Field { name } => match value {
            Value::Object(fields) => property_value(fields.get(name)?, rest),
            _ => None,
        },
        PropertyPathSegment::Index { index } => match value {
            Value::Array(values) => property_value(values.get(*index)?, rest),
            _ => None,
        },
    }
}
