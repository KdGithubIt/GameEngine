//! Inspector panel entry point and the Animation Graph document Inspector.
//!
//! The panel dispatcher lives here because every branch it does not delegate
//! to another submodule 窶・raw node properties, Animation States, and
//! transitions 窶・is Animation Graph document editing.

use crate::ui::*;

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
    pub(in crate::ui) fn show_inspector(&mut self, ui: &mut egui::Ui) {
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

    pub(in crate::ui) fn sync_property_buffer(&mut self) {
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
