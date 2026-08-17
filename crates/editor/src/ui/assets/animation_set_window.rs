//! Animation Set editor window.
//!
//! Owns the modeless window that binds motion slots to imported Animation
//! Clip sub-assets, together with the clip reference validation that keeps an
//! invalid binding from being written back to disk.

use crate::ui::*;

struct AnimationSetGraphModel {
    slots: Vec<engine_authoring::MotionSlot>,
    states: std::collections::BTreeMap<engine_authoring::MotionSlotId, Vec<String>>,
}

#[derive(Clone)]
struct MotionSourceChoice {
    label: String,
    source: engine_authoring::MotionSourceRef,
}

fn humanoid_motion_choices(
    manifest: &engine::AssetManifest,
    assets_root: Option<&std::path::Path>,
) -> Vec<AssetChoice> {
    let mut choices = Vec::new();
    for (_, source) in manifest.iter() {
        if assets_root.is_some_and(|root| !root.join(&source.path).is_file()) {
            continue;
        }
        let source_label = source.name.as_deref().unwrap_or(&source.path);
        for sub_asset in &source.import_settings.sub_assets {
            if sub_asset.kind != engine::ImportedSubAssetKind::HumanoidMotion {
                continue;
            }
            let Ok(id) =
                AssetId::from_stable_id(engine_authoring::StableId::new(&sub_asset.id))
            else {
                continue;
            };
            choices.push(AssetChoice {
                label: format!("{source_label} / {}", sub_asset.name),
                id,
            });
        }
    }
    choices
}

fn animation_set_motion_source_choices(
    native: &[AssetChoice],
    humanoid: &[AssetChoice],
) -> Vec<MotionSourceChoice> {
    let mut choices = Vec::with_capacity(native.len() * 2 + humanoid.len());
    for choice in native {
        choices.push(MotionSourceChoice {
            label: format!("[Auto] {}", choice.label),
            source: engine_authoring::MotionSourceRef::auto(choice.id.clone()),
        });
        choices.push(MotionSourceChoice {
            label: format!("[Native] {}", choice.label),
            source: engine_authoring::MotionSourceRef::native(choice.id.clone()),
        });
    }
    for choice in humanoid {
        choices.push(MotionSourceChoice {
            label: format!("[Humanoid] {}", choice.label),
            source: engine_authoring::MotionSourceRef::humanoid(choice.id.clone()),
        });
    }
    choices
}

fn motion_source_display_label(
    source: &engine_authoring::MotionSourceRef,
    choices: &[MotionSourceChoice],
) -> String {
    choices
        .iter()
        .find(|choice| choice.source == *source)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| {
            let variant = match source.variant {
                engine_authoring::MotionSourceVariant::Auto => "Auto",
                engine_authoring::MotionSourceVariant::Native => "Native",
                engine_authoring::MotionSourceVariant::Humanoid => "Humanoid",
            };
            format!("Missing [{variant}] ({})", source.asset.as_str())
        })
}

/// Stable identity for the Animation Set editor window.
///
/// `egui::Window` derives its area ID from its title, and this window's title
/// carries the document path plus a dirty marker. Without an explicit ID every
/// save or edit renamed the window, so egui saw a window it had never laid out
/// and dropped it back to the default position and size.
pub(in crate::ui) fn animation_set_window_id() -> egui::Id {
    egui::Id::new("animation_set_editor_window")
}

/// One Animation Set event row being edited before the change is committed.
///
/// A drag or a partially typed name is held here so the widget can show it
/// while the document, its undo history, and its dirty flag stay untouched
/// until the edit finishes (ADR 0116).
pub(in crate::ui) struct AnimationSetEventDraft {
    slot: engine_authoring::MotionSlotId,
    index: usize,
    time: f32,
    name: String,
}

/// Draws one editable event row and returns the edit once it is finished.
///
/// While the pointer or the keyboard focus is on the row, the value lives in
/// `draft` so the widget can follow the user. The row commits when the drag
/// stops or the focus leaves, which keeps one undo entry per completed edit.
fn show_animation_set_event_row(
    ui: &mut egui::Ui,
    slot: &engine_authoring::MotionSlotId,
    index: usize,
    event: &engine_authoring::AnimationSetEvent,
    draft: &mut Option<AnimationSetEventDraft>,
) -> Option<AnimationSetUiAction> {
    let editing = draft
        .as_ref()
        .is_some_and(|draft| draft.slot == *slot && draft.index == index);
    let mut time = if editing {
        draft.as_ref().expect("draft matched above").time
    } else {
        event.time
    };
    let mut name = if editing {
        draft.as_ref().expect("draft matched above").name.clone()
    } else {
        event.name.clone()
    };
    let mut removed = false;
    let mut finished = false;
    let mut touched = false;

    ui.push_id(("animation_set_event", slot.as_str(), index), |ui| {
        ui.horizontal(|ui| {
            let time_response = ui.add(
                egui::DragValue::new(&mut time)
                    .range(0.0..=f32::MAX)
                    .speed(0.01)
                    .suffix(" s"),
            );
            touched |= time_response.changed();
            finished |= time_response.drag_stopped() || time_response.lost_focus();

            let name_response = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .desired_width(140.0)
                    .hint_text("event name"),
            );
            touched |= name_response.changed();
            finished |= name_response.lost_focus();

            removed = ui.small_button("−").clicked();
        });
    });

    if removed {
        *draft = None;
        return Some(AnimationSetUiAction::RemoveEvent {
            slot: slot.clone(),
            index,
        });
    }
    if finished {
        *draft = None;
        if time != event.time || name != event.name {
            return Some(AnimationSetUiAction::SetEvent {
                slot: slot.clone(),
                index,
                time,
                name,
            });
        }
        return None;
    }
    if touched {
        *draft = Some(AnimationSetEventDraft {
            slot: slot.clone(),
            index,
            time,
            name,
        });
    }
    None
}

enum AnimationSetUiAction {
    Undo,
    Redo,
    Save,
    ChooseGraph(AssetId),
    ConfirmGraphChange(AssetId),
    CancelGraphChange,
    BeginClear,
    ClearGraph(bool),
    CancelClear,
    SetBinding {
        slot: engine_authoring::MotionSlot,
        motion: Option<engine_authoring::MotionSourceRef>,
    },
    AddOverlay {
        slot: engine_authoring::MotionSlotId,
        motion: engine_authoring::MotionSourceRef,
    },
    RemoveOverlay {
        slot: engine_authoring::MotionSlotId,
        index: usize,
    },
    MoveOverlay {
        slot: engine_authoring::MotionSlotId,
        index: usize,
        new_index: usize,
    },
    AddEvent {
        slot: engine_authoring::MotionSlotId,
    },
    RemoveEvent {
        slot: engine_authoring::MotionSlotId,
        index: usize,
    },
    SetEvent {
        slot: engine_authoring::MotionSlotId,
        index: usize,
        time: f32,
        name: String,
    },
}

/// Checks that a motion source about to be stored in an Animation Set points at
/// the imported sub-asset kind required by its explicit variant.
///
/// Auto and Native are rooted at an imported `Animation` sub-asset. Humanoid is
/// rooted at an imported `HumanoidMotion` sub-asset. The variant is persisted
/// and is never inferred from a display name or a parent source.
pub(in crate::ui) fn validate_imported_animation_motion_source_reference(
    manifest: &engine::AssetManifest,
    source: &engine_authoring::MotionSourceRef,
) -> Result<(), String> {
    let (expected_kind, expected_label) = match source.variant {
        engine_authoring::MotionSourceVariant::Auto
        | engine_authoring::MotionSourceVariant::Native => {
            (engine::ImportedSubAssetKind::Animation, "Animation Clip")
        }
        engine_authoring::MotionSourceVariant::Humanoid => (
            engine::ImportedSubAssetKind::HumanoidMotion,
            "HumanoidMotion",
        ),
    };

    match manifest.imported_sub_asset(&source.asset) {
        Some((_, _, sub_asset)) if sub_asset.kind == expected_kind => Ok(()),
        Some((_, _, _)) => Err(format!(
            "asset `{}` is an imported sub-asset, but {:?} requires an imported {expected_label}",
            source.asset.as_str(),
            source.variant
        )),
        None if manifest.get(&source.asset).is_some() => Err(format!(
            "asset `{}` is a source asset; {:?} requires its imported {expected_label} sub-asset instead",
            source.asset.as_str(),
            source.variant
        )),
        None => Err(format!(
            "asset `{}` is not a registered imported {expected_label} sub-asset",
            source.asset.as_str()
        )),
    }
}

/// Validates the primary motion and every overlay of an Animation Set document
/// before it is saved.
///
/// Existing invalid data remains openable for repair. This guard prevents the
/// editor from writing a variant/sub-asset mismatch back as canonical data.
pub(in crate::ui) fn validate_animation_set_clip_references(
    document: &engine_authoring::AnimationSet,
    manifest: &engine::AssetManifest,
) -> Result<(), String> {
    for binding in document.bindings.values() {
        validate_imported_animation_motion_source_reference(manifest, &binding.clip)
            .map_err(|error| format!("binding `{}` primary clip: {error}", binding.name))?;

        for (index, overlay) in binding.overlays.iter().enumerate() {
            validate_imported_animation_motion_source_reference(manifest, overlay).map_err(
                |error| {
                    format!(
                        "binding `{}` overlay {}: {error}",
                        binding.name,
                        index + 1
                    )
                },
            )?;
        }
    }

    Ok(())
}

impl EditorApp {
    pub(in crate::ui) fn open_animation_set_editor(
        &mut self,
        relative_path: PathBuf,
        absolute_path: PathBuf,
    ) {
        let result = fs::read_to_string(&absolute_path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                engine_authoring::AnimationSet::from_json(&json).map_err(|error| error.to_string())
            });
        match result {
            Ok(document) => {
                self.animation_set_editor = Some(AnimationSetEditorState::new(
                    relative_path,
                    absolute_path,
                    document,
                ));
                self.pending_animation_set_graph = None;
                self.pending_animation_set_clear = false;
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_open_failed",
                    format!("failed to open {}: {error}", absolute_path.display()),
                )),
        }
    }

    pub(in crate::ui) fn show_animation_set_editor_window(&mut self, context: &egui::Context) {
        let Some(read_only_state) = self.animation_set_editor.as_ref() else {
            return;
        };
        let assets_root = self.project_root.as_ref().map(ProjectRoot::assets_root);
        let graph_choices = asset_choices_for_kind(
            engine::AssetKind::AnimationGraph,
            &self.asset_manifest,
            assets_root.as_deref(),
        );
        let native_clip_choices = asset_choices_for_kind(
            engine::AssetKind::AnimationClip,
            &self.asset_manifest,
            assets_root.as_deref(),
        );
        let humanoid_clip_choices =
            humanoid_motion_choices(&self.asset_manifest, assets_root.as_deref());
        let motion_source_choices =
            animation_set_motion_source_choices(&native_clip_choices, &humanoid_clip_choices);
        let graph_model = read_only_state
            .document
            .graph
            .as_ref()
            .map(|graph| self.load_animation_set_graph_model(graph));
        let title = format!(
            "Animation Set: {}{}",
            read_only_state.relative_path.display(),
            if read_only_state.is_dirty() { " *" } else { "" }
        );
        let pending_graph = self.pending_animation_set_graph.clone();
        let pending_stale = pending_graph
            .as_ref()
            .and_then(|graph| {
                self.load_animation_set_graph_model(graph)
                    .ok()
                    .map(|model| read_only_state.stale_bindings(&model.slots))
            })
            .unwrap_or_default();
        let pending_clear = self.pending_animation_set_clear;
        let mut event_draft = self.animation_set_event_draft.take();
        let mut open = true;
        let mut action = None;

        egui::Window::new(title)
            .id(animation_set_window_id())
            .open(&mut open)
            .default_width(620.0)
            .default_height(520.0)
            .show(context, |ui| {
                let Some(state) = self.animation_set_editor.as_mut() else {
                    return;
                };
                control_row(ui, |ui| {
                    if ui
                        .add_enabled(state.can_undo(), egui::Button::new("Undo"))
                        .clicked()
                    {
                        action = Some(AnimationSetUiAction::Undo);
                    }
                    if ui
                        .add_enabled(state.can_redo(), egui::Button::new("Redo"))
                        .clicked()
                    {
                        action = Some(AnimationSetUiAction::Redo);
                    }
                    if ui
                        .add_enabled(state.is_dirty(), egui::Button::new("Save"))
                        .clicked()
                    {
                        action = Some(AnimationSetUiAction::Save);
                    }
                });
                ui.separator();

                let selected_graph_label = state
                    .document
                    .graph
                    .as_ref()
                    .and_then(|selected| {
                        graph_choices
                            .iter()
                            .find(|choice| &choice.id == selected)
                            .map(|choice| choice.label.as_str())
                    })
                    .map(str::to_owned)
                    .or_else(|| {
                        state
                            .document
                            .graph
                            .as_ref()
                            .map(|graph| format!("Missing ({})", graph.as_str()))
                    })
                    .unwrap_or_else(|| "(Unassigned)".to_owned());
                control_row(ui, |ui| {
                    ui.label("Animation Graph");
                    egui::ComboBox::from_id_salt("animation_set_graph_picker")
                        .selected_text(selected_graph_label)
                        .show_ui(ui, |ui| {
                            for choice in &graph_choices {
                                let selected =
                                    state.document.graph.as_ref() == Some(&choice.id);
                                if ui.selectable_label(selected, &choice.label).clicked() {
                                    action = Some(AnimationSetUiAction::ChooseGraph(
                                        choice.id.clone(),
                                    ));
                                    ui.close();
                                }
                            }
                        });
                    if ui
                        .add_enabled(
                            state.document.graph.is_some(),
                            egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        action = Some(AnimationSetUiAction::BeginClear);
                    }
                });

                match &graph_model {
                    Some(Err(error)) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 92, 92),
                            format!("Missing or invalid Graph: {error}"),
                        );
                        ui.small("Choose another Graph above, or Clear the broken reference.");
                    }
                    None => {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Assign an Animation Graph to enable Binding editing.",
                        );
                    }
                    Some(Ok(model)) => {
                        ui.separator();
                        ui.heading("Bindings");
                        if model.slots.is_empty() {
                            ui.label("The selected Graph has no Motion Slots.");
                        }
                        for slot in &model.slots {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&slot.display_name);
                                    ui.monospace(slot.id.as_str());
                                });
                                if let Some(states) = model.states.get(&slot.id)
                                    && !states.is_empty() {
                                        ui.small(format!("Used by: {}", states.join(", ")));
                                    }
                                let mut selected_source = state
                                    .document
                                    .bindings
                                    .get(&slot.id)
                                    .map(|binding| binding.clip.clone());
                                let selected_label = selected_source
                                    .as_ref()
                                    .map(|source| {
                                        motion_source_display_label(source, &motion_source_choices)
                                    })
                                    .unwrap_or_else(|| "(Unassigned)".to_owned());
                                let response =
                                    egui::ComboBox::from_id_salt((
                                        "animation_set_clip",
                                        slot.id.as_str(),
                                    ))
                                    .selected_text(selected_label)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_value(
                                                &mut selected_source,
                                                None,
                                                "(Unassigned)",
                                            )
                                            .changed()
                                        {
                                            action = Some(AnimationSetUiAction::SetBinding {
                                                slot: slot.clone(),
                                                motion: None,
                                            });
                                        }
                                        for choice in &motion_source_choices {
                                            if ui
                                                .selectable_value(
                                                    &mut selected_source,
                                                    Some(choice.source.clone()),
                                                    &choice.label,
                                                )
                                                .changed()
                                            {
                                                action =
                                                    Some(AnimationSetUiAction::SetBinding {
                                                        slot: slot.clone(),
                                                        motion: Some(choice.source.clone()),
                                                    });
                                            }
                                        }
                                    })
                                    .response;
                                if let Some(payload) =
                                    response.dnd_hover_payload::<DragPayload>()
                                    && payload.kind == AssetKind::AnimationClip {
                                        ui.painter().rect_stroke(
                                            response.rect,
                                            2.0,
                                            egui::Stroke::new(
                                                2.0_f32,
                                                egui::Color32::from_rgb(90, 180, 255),
                                            ),
                                            egui::StrokeKind::Outside,
                                        );
                                    }
                                if let Some(payload) =
                                    response.dnd_release_payload::<DragPayload>()
                                    && payload.kind == AssetKind::AnimationClip {
                                        action = Some(AnimationSetUiAction::SetBinding {
                                            slot: slot.clone(),
                                            motion: Some(engine_authoring::MotionSourceRef::native(
                                                payload.asset_id.clone(),
                                            )),
                                        });
                                    }
                                if let Some(binding) = state.document.bindings.get(&slot.id) {
                                    ui.small("Overlays (later entries have higher priority)");
                                    for (index, overlay) in binding.overlays.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            ui.label(motion_source_display_label(
                                                overlay,
                                                &motion_source_choices,
                                            ));
                                            if ui.small_button("↑").clicked() && index > 0 {
                                                action = Some(AnimationSetUiAction::MoveOverlay {
                                                    slot: slot.id.clone(),
                                                    index,
                                                    new_index: index - 1,
                                                });
                                            }
                                            if ui.small_button("↓").clicked()
                                                && index + 1 < binding.overlays.len()
                                            {
                                                action = Some(AnimationSetUiAction::MoveOverlay {
                                                    slot: slot.id.clone(),
                                                    index,
                                                    new_index: index + 1,
                                                });
                                            }
                                            if ui.small_button("Remove").clicked() {
                                                action = Some(AnimationSetUiAction::RemoveOverlay {
                                                    slot: slot.id.clone(),
                                                    index,
                                                });
                                            }
                                        });
                                    }
                                    egui::ComboBox::from_id_salt((
                                        "animation_set_overlay",
                                        slot.id.as_str(),
                                    ))
                                    .selected_text("Add overlay…")
                                    .show_ui(ui, |ui| {
                                        for choice in &motion_source_choices {
                                            let duplicate = binding.clip == choice.source
                                                || binding.overlays.contains(&choice.source);
                                            if ui
                                                .add_enabled(
                                                    !duplicate,
                                                    egui::Button::new(&choice.label),
                                                )
                                                .clicked()
                                            {
                                                action = Some(
                                                    AnimationSetUiAction::AddOverlay {
                                                        slot: slot.id.clone(),
                                                        motion: choice.source.clone(),
                                                    },
                                                );
                                            }
                                        }
                                    });
                                    ui.small("Events (emitted when playback crosses the time)");
                                    for (index, event) in binding.events.iter().enumerate() {
                                        if let Some(row_action) = show_animation_set_event_row(
                                            ui,
                                            &slot.id,
                                            index,
                                            event,
                                            &mut event_draft,
                                        ) {
                                            action = Some(row_action);
                                        }
                                    }
                                    if ui.small_button("+ Add Event").clicked() {
                                        action = Some(AnimationSetUiAction::AddEvent {
                                            slot: slot.id.clone(),
                                        });
                                    }
                                }
                            });
                        }
                    }
                }

                if let Some(graph) = &pending_graph {
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!(
                            "Changing Graph will remove {} Binding(s) whose MotionSlotId does not exist in the new Graph.",
                            pending_stale.len()
                        ),
                    );
                    for slot in &pending_stale {
                        ui.monospace(slot.as_str());
                    }
                    control_row(ui, |ui| {
                        if ui.button("Change Graph and Remove").clicked() {
                            action = Some(AnimationSetUiAction::ConfirmGraphChange(
                                graph.clone(),
                            ));
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(AnimationSetUiAction::CancelGraphChange);
                        }
                    });
                }

                if pending_clear {
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Clear the Graph reference. What should happen to existing Bindings?",
                    );
                    control_row(ui, |ui| {
                        if ui.button("Graph Reference Only").clicked() {
                            action = Some(AnimationSetUiAction::ClearGraph(false));
                        }
                        if ui.button("Graph and Bindings").clicked() {
                            action = Some(AnimationSetUiAction::ClearGraph(true));
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(AnimationSetUiAction::CancelClear);
                        }
                    });
                }
            });

        if !open {
            self.animation_set_editor = None;
            self.pending_animation_set_graph = None;
            self.pending_animation_set_clear = false;
            self.animation_set_event_draft = None;
            return;
        }
        self.animation_set_event_draft = event_draft;
        self.apply_animation_set_ui_action(action);
    }

    /// Routes one editor-wide document shortcut to the Animation Set window.
    ///
    /// An Animation Set is edited in a modeless window instead of a workspace
    /// tab, so `DocumentWorkspace` knows nothing about it and the shortcuts
    /// always reached the active Scene, Graph, or UI document. Saving a Set was
    /// therefore only possible through its own button. Claiming the shortcut
    /// while the Set window is frontmost keeps Save, Undo, and Redo pointed at
    /// the surface the user is working in.
    ///
    /// Returns whether the shortcut was consumed.
    pub(in crate::ui) fn claim_animation_set_shortcut(
        &mut self,
        context: &egui::Context,
        shortcut: DocumentShortcut,
    ) -> bool {
        if self.animation_set_editor.is_none() {
            return false;
        }
        let window = egui::LayerId::new(egui::Order::Middle, animation_set_window_id());
        if context.top_layer_id() != Some(window) {
            return false;
        }
        let action = match shortcut {
            DocumentShortcut::Save => AnimationSetUiAction::Save,
            DocumentShortcut::Undo => AnimationSetUiAction::Undo,
            DocumentShortcut::Redo => AnimationSetUiAction::Redo,
        };
        self.apply_animation_set_ui_action(Some(action));
        true
    }

    fn load_animation_set_graph_model(
        &self,
        graph: &AssetId,
    ) -> Result<AnimationSetGraphModel, String> {
        let project = self
            .project_root
            .as_ref()
            .ok_or_else(|| "no project is open".to_owned())?;
        let entry = self
            .asset_manifest
            .get(graph)
            .ok_or_else(|| format!("asset `{}` is not registered", graph.as_str()))?;
        let path = project
            .resolve_asset(&entry.path)
            .map_err(|error| error.to_string())?;
        let graph_document = if let Some((working_copy, _revision)) = graph_working_copy(&self.session, &path) {
            working_copy.clone()
        } else {
            let json = fs::read_to_string(&path).map_err(|error| error.to_string())?;
            serde_json::from_str::<engine_authoring::Graph>(&json)
                .map_err(|error| error.to_string())?
        };
        if graph_document.kind.as_str() != "anim.graph" {
            return Err(format!(
                "{} is `{}`, not an Animation Graph",
                entry.path,
                graph_document.kind.as_str()
            ));
        }
        let slots = engine_authoring::animation_graph_motion_slots(&graph_document)
            .map_err(|error| format!("{} has invalid Motion Slots: {error}", entry.path))?;
        let mut states =
            std::collections::BTreeMap::<engine_authoring::MotionSlotId, Vec<String>>::new();
        for node in graph_document.nodes.values() {
            if node.node_type.as_str() != "anim.state" {
                continue;
            }
            let Value::Object(properties) = &node.properties else {
                continue;
            };
            let Some(Value::String(slot)) = properties.get("motion_slot") else {
                continue;
            };
            let Ok(slot) = engine_authoring::MotionSlotId::from_stable_id(
                engine_authoring::StableId::new(slot),
            ) else {
                continue;
            };
            states.entry(slot).or_default().push(
                node.name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| node.id.as_str().to_owned()),
            );
        }
        Ok(AnimationSetGraphModel { slots, states })
    }

    /// Persists the current Animation Set through its validated atomic save adapter.
    pub(in crate::ui) fn save_animation_set_document(&mut self) -> Result<(), String> {
        let state = self
            .animation_set_editor
            .as_ref()
            .ok_or_else(|| "Animation Set editor is closed".to_owned())?;
        validate_animation_set_clip_references(&state.document, &self.asset_manifest)?;
        self.animation_set_editor
            .as_mut()
            .expect("checked above")
            .save()
    }

    fn apply_animation_set_ui_action(&mut self, action: Option<AnimationSetUiAction>) {
        let Some(action) = action else {
            return;
        };
        match action {
            AnimationSetUiAction::Undo => {
                if let Some(state) = self.animation_set_editor.as_mut() {
                    state.undo();
                }
            }
            AnimationSetUiAction::Redo => {
                if let Some(state) = self.animation_set_editor.as_mut() {
                    state.redo();
                }
            }
            AnimationSetUiAction::Save => {
                // Catch parent-source references left by older invalid data or
                // introduced through another path, across the whole document,
                // immediately before it is written back to the file.
                let validation_result = self
                    .animation_set_editor
                    .as_ref()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| {
                        validate_animation_set_clip_references(
                            &state.document,
                            &self.asset_manifest,
                        )
                    });

                // Run the existing atomic save only when validation succeeded.
                // On failure the document stays dirty so the author can repair
                // the reference.
                let result = validation_result.and_then(|()| {
                    self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(AnimationSetEditorState::save)
                });
                match result {
                    Ok(()) => {
                        self.push_notification(
                            EditorNotificationLevel::Success,
                            "Animation Set saved".to_owned(),
                        );
                        self.refresh_scene_problems();
                    }
                    Err(error) => self.report_error(
                        "editor.animation_set_save_failed",
                        format!("could not save Animation Set: {error}"),
                    ),
                }
            }
            AnimationSetUiAction::ChooseGraph(graph) => {
                match self.load_animation_set_graph_model(&graph) {
                    Ok(model) => {
                        let stale = self
                            .animation_set_editor
                            .as_ref()
                            .map(|state| state.stale_bindings(&model.slots))
                            .unwrap_or_default();
                        if stale.is_empty() {
                            if let Some(state) = self.animation_set_editor.as_mut() {
                                state.assign_graph(graph, &model.slots, false);
                            }
                        } else {
                            self.pending_animation_set_graph = Some(graph);
                        }
                    }
                    Err(error) => self.report_error(
                        "editor.animation_set_graph_invalid",
                        format!("could not select Animation Graph: {error}"),
                    ),
                }
            }
            AnimationSetUiAction::ConfirmGraphChange(graph) => {
                match self.load_animation_set_graph_model(&graph) {
                    Ok(model) => {
                        if let Some(state) = self.animation_set_editor.as_mut() {
                            state.assign_graph(graph, &model.slots, true);
                        }
                        self.pending_animation_set_graph = None;
                    }
                    Err(error) => self.report_error(
                        "editor.animation_set_graph_invalid",
                        format!("could not select Animation Graph: {error}"),
                    ),
                }
            }
            AnimationSetUiAction::CancelGraphChange => {
                self.pending_animation_set_graph = None;
            }
            AnimationSetUiAction::BeginClear => {
                self.pending_animation_set_clear = true;
            }
            AnimationSetUiAction::ClearGraph(clear_bindings) => {
                if let Some(state) = self.animation_set_editor.as_mut() {
                    state.clear_graph(clear_bindings);
                }
                self.pending_animation_set_clear = false;
                self.pending_animation_set_graph = None;
            }
            AnimationSetUiAction::CancelClear => {
                self.pending_animation_set_clear = false;
            }
            AnimationSetUiAction::SetBinding { slot, motion } => {
                if let Some(motion) = motion.as_ref()
                    && let Err(error) = validate_imported_animation_motion_source_reference(
                        &self.asset_manifest,
                        motion,
                    )
                {
                    self.report_error(
                        "editor.animation_set_binding_failed",
                        format!("binding `{}` primary clip: {error}", slot.display_name),
                    );
                    return;
                }

                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.set_binding_source(&slot, motion));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_binding_failed", error);
                }
            }
            AnimationSetUiAction::AddOverlay { slot, motion } => {
                if let Err(error) = validate_imported_animation_motion_source_reference(
                    &self.asset_manifest,
                    &motion,
                ) {
                    self.report_error(
                        "editor.animation_set_binding_failed",
                        format!("binding `{}` overlay: {error}", slot.as_str()),
                    );
                    return;
                }

                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.add_overlay_source(&slot, motion));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_binding_failed", error);
                }
            }
            AnimationSetUiAction::RemoveOverlay { slot, index } => {
                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.remove_overlay(&slot, index));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_binding_failed", error);
                }
            }
            AnimationSetUiAction::MoveOverlay {
                slot,
                index,
                new_index,
            } => {
                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.move_overlay(&slot, index, new_index));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_binding_failed", error);
                }
            }
            AnimationSetUiAction::AddEvent { slot } => {
                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.add_event(&slot));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_event_failed", error);
                }
            }
            AnimationSetUiAction::RemoveEvent { slot, index } => {
                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.remove_event(&slot, index));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_event_failed", error);
                }
            }
            AnimationSetUiAction::SetEvent {
                slot,
                index,
                time,
                name,
            } => {
                let result = self
                    .animation_set_editor
                    .as_mut()
                    .ok_or_else(|| "Animation Set editor is closed".to_owned())
                    .and_then(|state| state.set_event(&slot, index, time, &name));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_event_failed", error);
                }
            }
        }
    }

}
