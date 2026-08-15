//! Asset browser panels, asset registration and import, and asset editing windows.

use super::*;

struct AnimationSetGraphModel {
    slots: Vec<engine_authoring::MotionSlot>,
    states: std::collections::BTreeMap<engine_authoring::MotionSlotId, Vec<String>>,
}

/// One of the editor-wide document shortcuts a modeless editor window may claim
/// while it is the frontmost window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DocumentShortcut {
    Save,
    Undo,
    Redo,
}

/// Stable identity for the Animation Set editor window.
///
/// `egui::Window` derives its area ID from its title, and this window's title
/// carries the document path plus a dirty marker. Without an explicit ID every
/// save or edit renamed the window, so egui saw a window it had never laid out
/// and dropped it back to the default position and size.
pub(super) fn animation_set_window_id() -> egui::Id {
    egui::Id::new("animation_set_editor_window")
}

/// One Animation Set event row being edited before the change is committed.
///
/// A drag or a partially typed name is held here so the widget can show it
/// while the document, its undo history, and its dirty flag stay untouched
/// until the edit finishes (ADR 0116).
pub(super) struct AnimationSetEventDraft {
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
        clip: Option<AssetId>,
    },
    AddOverlay {
        slot: engine_authoring::MotionSlotId,
        clip: AssetId,
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

/// Animation Setへ保存しようとしている参照が、インポート処理で生成された
/// Animation Clipサブアセットを指していることを確認する。
///
/// VMDやPMXなどの親ソースも拡張子上はアニメーションを生成できるが、
/// Animation Setが保持すべきものは親ソースではなく、そこから生成された
/// [`engine::ImportedSubAssetKind::Animation`] の安定IDである。
///
/// コンボボックスの候補制限だけに依存すると、ドラッグ＆ドロップ、古い文書、
/// 将来追加される入力経路から不正IDが渡る可能性があるため、文書を変更する
/// 直前と保存直前の共通防御として使用する。
pub(super) fn validate_imported_animation_clip_reference(
    manifest: &engine::AssetManifest,
    clip: &AssetId,
) -> Result<(), String> {
    match manifest.imported_sub_asset(clip) {
        // インポート済みサブアセットがAnimationなら、Animation Setが要求する
        // 参照契約を満たしているため、そのまま割り当てまたは保存を許可する。
        Some((_, _, sub_asset))
            if sub_asset.kind == engine::ImportedSubAssetKind::Animation =>
        {
            Ok(())
        }

        // サブアセットとしては存在しても、MeshやMaterialなどをAnimation Clip
        // として保存することはできないため、型の不一致として拒否する。
        Some((_, _, _)) => Err(format!(
            "asset `{}` is an imported sub-asset, but it is not an Animation Clip",
            clip.as_str()
        )),

        // トップレベルの登録アセットとして存在する場合は、今回の不具合で
        // 選択されていたVMDなどの親ソースであり、その派生Clipを選ぶ必要がある。
        None if manifest.get(clip).is_some() => Err(format!(
            "asset `{}` is a source asset; select its imported Animation Clip sub-asset instead",
            clip.as_str()
        )),

        // マニフェストのどこにも存在しないIDは、削除済みまたは破損した参照として
        // 扱い、不正な参照を正規データとして再保存することを禁止する。
        None => Err(format!(
            "asset `{}` is not a registered imported Animation Clip sub-asset",
            clip.as_str()
        )),
    }
}

/// Animation Set文書に含まれるprimary clipと全overlayを保存前に検証する。
///
/// 既存の不正な文書はEditorで開いて修正できるようにする一方、不正な状態を
/// 再保存して確定することは防止する。エラーにはbinding名と参照位置を含め、
/// 複数のoverlayが存在しても修正対象を特定できるようにする。
pub(super) fn validate_animation_set_clip_references(
    document: &engine_authoring::AnimationSet,
    manifest: &engine::AssetManifest,
) -> Result<(), String> {
    for binding in document.bindings.values() {
        // primary clipは各bindingに必須なので、最初に個別検証する。
        validate_imported_animation_clip_reference(manifest, &binding.clip)
            .map_err(|error| format!("binding `{}` primary clip: {error}", binding.name))?;

        // overlayもprimaryと同じAnimation Clip参照契約を持つ。表示上の番号に
        // 合わせて1始まりのインデックスを報告し、修正箇所を明確にする。
        for (index, overlay) in binding.overlays.iter().enumerate() {
            validate_imported_animation_clip_reference(manifest, overlay).map_err(|error| {
                format!(
                    "binding `{}` overlay {}: {error}",
                    binding.name,
                    index + 1
                )
            })?;
        }
    }

    Ok(())
}

impl EditorApp {
    pub(super) fn notify_registered_assets(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let file_names = paths
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        let noun = if file_names.len() == 1 {
            "asset"
        } else {
            "assets"
        };
        self.push_notification(
            EditorNotificationLevel::Success,
            format!(
                "Registered {} {noun}: {}",
                file_names.len(),
                file_names.join(", ")
            ),
        );
    }

    pub(super) fn notify_asset_error(&mut self, message: impl Into<String>) {
        self.report_error("editor.asset_error", message.into());
    }

    pub(super) fn open_animation_set_editor(
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

    pub(super) fn show_animation_set_editor_window(&mut self, context: &egui::Context) {
        let Some(read_only_state) = self.animation_set_editor.as_ref() else {
            return;
        };
        let graph_choices = asset_choices_for_kind(
            engine::AssetKind::AnimationGraph,
            &self.asset_manifest,
            self.project_root
                .as_ref()
                .map(ProjectRoot::assets_root)
                .as_deref(),
        );
        let clip_choices = asset_choices_for_kind(
            engine::AssetKind::AnimationClip,
            &self.asset_manifest,
            self.project_root
                .as_ref()
                .map(ProjectRoot::assets_root)
                .as_deref(),
        );
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
                                let mut selected_clip = state
                                    .document
                                    .bindings
                                    .get(&slot.id)
                                    .map(|binding| binding.clip.clone());
                                let selected_label = selected_clip
                                    .as_ref()
                                    .and_then(|selected| {
                                        clip_choices
                                            .iter()
                                            .find(|choice| &choice.id == selected)
                                            .map(|choice| choice.label.as_str())
                                    })
                                    .map(str::to_owned)
                                    .or_else(|| {
                                        selected_clip.as_ref().map(|clip| {
                                            format!("Missing ({})", clip.as_str())
                                        })
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
                                                &mut selected_clip,
                                                None,
                                                "(Unassigned)",
                                            )
                                            .changed()
                                        {
                                            action = Some(AnimationSetUiAction::SetBinding {
                                                slot: slot.clone(),
                                                clip: None,
                                            });
                                        }
                                        for choice in &clip_choices {
                                            if ui
                                                .selectable_value(
                                                    &mut selected_clip,
                                                    Some(engine_authoring::MotionSourceRef::native(choice.id.clone())),
                                                    &choice.label,
                                                )
                                                .changed()
                                            {
                                                action =
                                                    Some(AnimationSetUiAction::SetBinding {
                                                        slot: slot.clone(),
                                                        clip: Some(choice.id.clone()),
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
                                            clip: Some(payload.asset_id.clone()),
                                        });
                                    }
                                if let Some(binding) = state.document.bindings.get(&slot.id) {
                                    ui.small("Overlays (later entries have higher priority)");
                                    for (index, overlay) in binding.overlays.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            let label = clip_choices
                                                .iter()
                                                .find(|choice| choice.id == *overlay)
                                                .map(|choice| choice.label.clone())
                                                .unwrap_or_else(|| {
                                                    format!("Missing ({})", overlay.as_str())
                                                });
                                            ui.label(label);
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
                                        for choice in &clip_choices {
                                            let duplicate = binding.clip == choice.id
                                                || binding
                                                    .overlays
                                                    .iter()
                                                    .any(|overlay| overlay == &choice.id);
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
                                                        clip: choice.id.clone(),
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
    pub(super) fn claim_animation_set_shortcut(
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
        let json = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let graph_document = serde_json::from_str::<engine_authoring::Graph>(&json)
            .map_err(|error| error.to_string())?;
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
                // 古い不正データや別経路から混入した親ソース参照を、ファイルへ
                // 書き戻す直前に文書全体で検出する。
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

                // 検証成功時だけ既存のatomic saveを実行する。失敗時はdirty状態を
                // 維持し、ユーザーが参照を修正できるようにする。
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
            AnimationSetUiAction::SetBinding { slot, clip } => {
                // Unassignedへの変更には参照がないため検証不要。IDが指定された場合は、
                // 文書とundo履歴を変更する前にAnimationサブアセットか確認する。
                if let Some(clip) = clip.as_ref()
                    && let Err(error) =
                        validate_imported_animation_clip_reference(&self.asset_manifest, clip)
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
                    .and_then(|state| state.set_binding(&slot, clip));
                if let Err(error) = result {
                    self.report_error("editor.animation_set_binding_failed", error);
                }
            }
            AnimationSetUiAction::AddOverlay { slot, clip } => {
                // Overlayもprimaryと同じインポート済みAnimation Clipだけを受け入れる。
                // 検証を先に行い、失敗時にundo履歴やoverlay順序を変化させない。
                if let Err(error) =
                    validate_imported_animation_clip_reference(&self.asset_manifest, &clip)
                {
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
                    .and_then(|state| state.add_overlay(&slot, clip));
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

    pub(super) fn show_material_editor_window(&mut self, context: &egui::Context) {
        self.flush_pending_material_saves(context);
        self.flush_material_scene_preview_refresh(context);
        if !self.show_material_editor {
            return;
        }
        let texture_choices = self.material_texture_choices();
        self.refresh_material_texture_preview(context);
        let mut open = self.show_material_editor;
        let mut changed = false;
        let mut reimport_preview = false;
        egui::Window::new("Material Editor")
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                changed =
                    show_material_editor_panel(
                        &mut self.material_editor,
                        ui,
                        texture_choices.as_slice(),
                    );
                ui.separator();
                ui.heading("Preview");
                if let Some(material) = self.material_editor.active_material() {
                    show_material_preview(ui, material, self.material_texture_preview.as_ref());
                }
                reimport_preview = ui
                    .button("Reimport textures")
                    .on_hover_text("Decode the registered source files again and refresh Scene View diagnostics")
                    .clicked();
            });
        self.show_material_editor = open;
        if changed {
            self.queue_active_material_save(context);
        }
        if !self.show_material_editor {
            self.flush_all_pending_material_saves();
            self.flush_material_scene_preview_refresh(context);
        }
        if reimport_preview {
            self.material_preview_asset = None;
            self.refresh_material_texture_preview(context);
            self.refresh_scene_problems();
            context.request_repaint();
        }
    }

    /// Reuses texture picker choices while neither the project nor manifest
    /// changed, avoiding a project-wide asset scan on every color-drag frame.
    fn material_texture_choices(&mut self) -> Arc<Vec<(AssetId, String)>> {
        let assets_root = self.project_root.as_ref().map(ProjectRoot::assets_root);
        let manifest_revision = self.asset_manifest.revision();
        if let Some(cache) = &self.material_texture_choices_cache
            && cache.manifest_revision == manifest_revision
            && cache.assets_root == assets_root
        {
            return Arc::clone(&cache.choices);
        }
        let choices = Arc::new(
            asset_choices_for_kind(
                engine::AssetKind::Texture,
                &self.asset_manifest,
                assets_root.as_deref(),
            )
            .into_iter()
            .map(|choice| (choice.id, choice.label))
            .collect(),
        );
        self.material_texture_choices_cache = Some(MaterialTextureChoicesCache {
            manifest_revision,
            assets_root,
            choices: Arc::clone(&choices),
        });
        choices
    }

    /// Queues the latest active value and replaces any older value for the
    /// same path while a slider or color picker is still moving.
    pub(super) fn queue_active_material_save(&mut self, context: &egui::Context) {
        let Some(relative_path) = self.material_editor.active.clone() else {
            return;
        };
        let Some(material) = self.material_editor.active_material().cloned() else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(120);
        self.pending_material_saves.insert(
            relative_path,
            PendingMaterialSave { material, deadline },
        );
        context.request_repaint_after(std::time::Duration::from_millis(120));
    }

    /// Writes each Material once after its continuous edit reaches a quiet
    /// period, avoiding synchronous file replacement and fsync every frame.
    pub(super) fn flush_pending_material_saves(&mut self, context: &egui::Context) {
        let now = std::time::Instant::now();
        let due = self
            .pending_material_saves
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in due {
            if let Some(pending) = self.pending_material_saves.remove(&path) {
                self.persist_material(path, pending.material);
            }
        }
        if let Some(next) = self
            .pending_material_saves
            .values()
            .map(|pending| pending.deadline)
            .min()
        {
            context.request_repaint_after(next.saturating_duration_since(now));
        }
    }

    /// Persists every queued value before closing its editor or replacing the
    /// project that owns its relative path.
    pub(super) fn flush_all_pending_material_saves(&mut self) {
        let pending = std::mem::take(&mut self.pending_material_saves);
        for (path, pending) in pending {
            self.persist_material(path, pending.material);
        }
    }

    /// Applies one deferred Scene View rebuild after continuous Material edits
    /// have been quiet long enough to avoid rebuilding on every drag frame.
    fn flush_material_scene_preview_refresh(&mut self, context: &egui::Context) {
        let Some(deadline) = self.material_scene_preview_deadline else {
            return;
        };
        let now = std::time::Instant::now();
        if now < deadline {
            context.request_repaint_after(deadline.saturating_duration_since(now));
            return;
        }
        self.material_scene_preview_deadline = None;
        self.scene_view.invalidate_asset_preview();
        context.request_repaint();
    }

    fn refresh_material_texture_preview(&mut self, context: &egui::Context) {
        let selected = self
            .material_editor
            .active_material()
            .and_then(|material| material.base_color_texture.clone());
        if selected == self.material_preview_asset {
            return;
        }
        self.material_preview_asset = selected.clone();
        self.material_texture_preview = selected.and_then(|asset| {
            let project = self.project_root.as_ref()?;
            let entry = self.asset_manifest.get(&asset)?;
            let path = project.assets_root().join(&entry.path);
            load_texture_preview(context, &path, PathBuf::from(&entry.path)).ok()
        });
    }

    pub(super) fn show_texture_preview_window(&mut self, context: &egui::Context) {
        let Some(preview) = &self.texture_preview else {
            return;
        };
        let mut open = true;
        let mut reimport = false;
        egui::Window::new("Texture Preview")
            .open(&mut open)
            .default_width(360.0)
            .show(context, |ui| {
                ui.strong(preview.relative_path.display().to_string());
                ui.label(format!(
                    "{} × {} px",
                    preview.dimensions[0], preview.dimensions[1]
                ));
                let available = ui.available_width().min(320.0);
                ui.add(
                    egui::Image::new((preview.texture.id(), egui::vec2(available, available)))
                        .maintain_aspect_ratio(true),
                );
                reimport = ui.button("Reimport").clicked();
            });
        if !open {
            self.texture_preview = None;
            return;
        }
        if reimport {
            let relative = preview.relative_path.clone();
            let result = self
                .project_root
                .as_ref()
                .map(|project| project.assets_root().join(&relative))
                .ok_or_else(|| "no project is open".to_owned())
                .and_then(|path| load_texture_preview(context, &path, relative));
            match result {
                Ok(preview) => {
                    self.texture_preview = Some(preview);
                    self.material_preview_asset = None;
                    self.refresh_scene_problems();
                    context.request_repaint();
                }
                Err(error) => self
                    .session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.texture_reimport_failed",
                        format!("texture reimport failed: {error}"),
                    )),
            }
        }
    }

    /// Shows the skeleton bind report detail view opened from a Problems
    /// panel `anim.skeleton_rebind` entry (ADR 0077 §6, AP-5).
    pub(super) fn show_skeleton_bind_report_window(&mut self, context: &egui::Context) {
        let Some(source_id) = self.show_skeleton_bind_report.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Skeleton Bind Report")
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                ui.label(format!("Source: {source_id}"));
                match self.skeleton_rebind_reports.get(&source_id) {
                    Some(reports) if !reports.is_empty() => {
                        for report in reports {
                            ui.separator();
                            ui.strong(format!("Skeleton {}", report.skeleton_id));
                            crate::anim_ux::show_skeleton_bind_report(ui, report);
                        }
                    }
                    _ => {
                        ui.label("No bind report is recorded for this source.");
                    }
                }
            });
        if !open {
            self.show_skeleton_bind_report = None;
        }
    }

    /// Opens the RetargetMap inspector window for a `*.retarget.json` asset
    /// (AP-5, ADR 0079 §1).
    pub(super) fn open_retarget_map_editor(&mut self, relative_path: PathBuf, abs_path: PathBuf) {
        let result = fs::read_to_string(&abs_path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                engine::RetargetMap::from_json(&json).map_err(|error| error.to_string())
            });
        match result {
            Ok(map) => {
                self.retarget_map_editor = Some(RetargetMapEditorState { relative_path, map });
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_open_failed",
                    format!("failed to open {}: {error}", abs_path.display()),
                )),
        }
    }

    pub(super) fn show_retarget_map_editor_window(&mut self, context: &egui::Context) {
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let model =
            crate::anim_ux::build_retarget_map_inspector_model(&state.map, &self.asset_manifest);
        let title = format!("Retarget Map: {}", state.relative_path.display());
        let mut open = true;
        let mut action = None;
        egui::Window::new(title)
            .open(&mut open)
            .default_width(480.0)
            .show(context, |ui| {
                action = crate::anim_ux::show_retarget_map_inspector(ui, &model);
            });
        match action {
            Some(crate::anim_ux::RetargetMapInspectorAction::RerunNameMatching) => {
                self.rerun_retarget_map_name_matching();
            }
            Some(crate::anim_ux::RetargetMapInspectorAction::SetAlwaysPackage(always_package)) => {
                self.set_retarget_map_always_package(always_package);
            }
            None => {}
        }
        if !open {
            self.retarget_map_editor = None;
        }
    }

    /// Handles the RetargetMap inspector's "Always package" checkbox (AP-7):
    /// writes [`engine::RetargetMap::always_package`] back to the open
    /// `*.retarget.json` file, the same write path as "Re-run name matching".
    pub(super) fn set_retarget_map_always_package(&mut self, always_package: bool) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let mut updated = state.map.clone();
        updated.always_package = always_package;
        let json = match updated.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.retarget_map_save_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        let relative_path = state.relative_path.clone();
        let full_path = project.assets_root().join(&relative_path);
        if let Err(error) = replace_file_contents(&full_path, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_save_failed",
                    format!("failed to save {}: {error}", full_path.display()),
                ));
            return;
        }
        if let Some(state) = &mut self.retarget_map_editor {
            state.map = updated;
        }
        self.refresh_scene_problems();
    }

    /// Handles the RetargetMap inspector's "Re-run name matching" action:
    /// regenerates name-matched `bone_pairs` (explicit pairs win, per
    /// [`crate::anim_ux::merge_bone_pairs`]) and writes the result straight
    /// back to the open `*.retarget.json` file.
    fn rerun_retarget_map_name_matching(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(state) = &self.retarget_map_editor else {
            return;
        };
        let source_record = crate::anim_ux::find_skeleton_record(
            &self.asset_manifest,
            state.map.source_skeleton.as_str(),
        )
        .cloned();
        let target_record = crate::anim_ux::find_skeleton_record(
            &self.asset_manifest,
            state.map.target_skeleton.as_str(),
        )
        .cloned();
        let (Some(source_record), Some(target_record)) = (source_record, target_record) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.retarget_map_rerun_unresolvable",
                    "cannot re-run name matching: one of the mapped skeletons is not currently \
                     registered in this project",
                ));
            return;
        };
        let updated = crate::anim_ux::rerun_name_matching(
            &state.map,
            &source_record.bones,
            &target_record.bones,
        );
        let json = match updated.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.retarget_map_save_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        let relative_path = state.relative_path.clone();
        let full_path = project.assets_root().join(&relative_path);
        if let Err(error) = replace_file_contents(&full_path, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.retarget_map_save_failed",
                    format!("failed to save {}: {error}", full_path.display()),
                ));
            return;
        }
        if let Some(state) = &mut self.retarget_map_editor {
            state.map = updated;
        }
        self.refresh_scene_problems();
    }

    /// Opens the Import Settings window for a registered glTF/GLB source row
    /// (contact-bones override editing + contact interval display, AP-5).
    pub(super) fn open_import_settings_editor(&mut self, index: usize) {
        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let Some((source_id, _)) = self.manifest_entry_for(&entry.relative_path) else {
            return;
        };
        let contact_bones = self
            .asset_manifest
            .get(&source_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();
        let motion_pairing = engine::asset_path_matches_kind(
            engine::AssetKind::MotionSource,
            &entry.relative_path,
        )
        .then(|| MotionPairingState {
            motion_path: self
                .project_root
                .as_ref()
                .map(|project| project.assets_root().join(&entry.relative_path)),
            original: self
                .asset_manifest
                .get(&source_id)
                .and_then(|entry| entry.import_settings.motion_original_model_source.as_deref())
                .and_then(|original| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(original)).ok()
                }),
            selected: self
                .asset_manifest
                .get(&source_id)
                .map(|entry| {
                    entry
                        .import_settings
                        .resolved_motion_model_sources()
                        .into_iter()
                        .filter_map(|paired| {
                            AssetId::from_stable_id(engine_authoring::StableId::new(paired)).ok()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            candidates: pmx_model_sources(&self.asset_manifest),
            candidate_paths: self
                .project_root
                .as_ref()
                .map(|project| pmx_model_paths(&self.asset_manifest, &project.assets_root()))
                .unwrap_or_default(),
            retarget_pairs: self
                .project_root
                .as_ref()
                .map(|project| {
                    registered_model_retarget_pairs(
                        &self.asset_manifest,
                        &project.assets_root(),
                    )
                })
                .unwrap_or_default(),
            recorded_model_name: self.project_root.as_ref().and_then(|project| {
                engine::vmd_recorded_model_name_path(
                    &project.assets_root().join(&entry.relative_path),
                )
                .ok()
                    .filter(|name| !name.trim().is_empty())
            }),
            compatibility_reports: Vec::new(),
        });
        self.import_settings_editor = Some(ImportSettingsEditorState {
            source_id,
            relative_path: entry.relative_path.clone(),
            contact_bones,
            motion_pairing,
        });
    }

    pub(super) fn show_import_settings_editor_window(&mut self, context: &egui::Context) {
        let Some(state) = &mut self.import_settings_editor else {
            return;
        };
        let mut open = true;
        let mut save_requested = false;
        let source_id_str = state.source_id.as_str().to_owned();
        let clip_contacts = self.clip_contact_summaries.get(&source_id_str).cloned();
        let title = format!("Import Settings: {}", state.relative_path.display());
        egui::Window::new(title)
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                // Edited in place; persisted only when Save is clicked so an
                // in-progress edit is never written half-typed.
                if let Some(pairing) = &mut state.motion_pairing {
                    show_motion_pairing_editor(ui, pairing);
                    ui.separator();
                }
                let _ = crate::anim_ux::show_contact_bones_editor(ui, &mut state.contact_bones);
                save_requested = ui
                    .button("Save and Reimport")
                    .on_hover_text(
                        "Writes the contact-bones override and re-detects contact intervals \
                         (ADR 0080 §1) by reimporting this source",
                    )
                    .clicked();
                ui.separator();
                ui.strong("Detected contact intervals (latest import)");
                match clip_contacts {
                    Some(clips) if !clips.is_empty() => {
                        for clip in &clips {
                            ui.label(format!("Clip: {}", clip.clip_name));
                            if clip.intervals.is_empty() {
                                ui.label("  (no contact intervals detected)");
                            }
                            for interval in &clip.intervals {
                                ui.label(format!(
                                    "  {} : {:.3}s - {:.3}s",
                                    interval.bone_name, interval.start, interval.end
                                ));
                            }
                        }
                    }
                    _ => {
                        ui.label("No contact interval data yet; run Reimport to detect intervals.");
                    }
                }
            });
        if save_requested {
            self.save_import_settings_editor();
        }
        if !open {
            self.import_settings_editor = None;
        }
    }

    /// Persists the Import Settings window's edited `contact_bones` back into
    /// the manifest and queues a reimport so the displayed contact intervals
    /// refresh against the new override (ADR 0080 §1, AP-5).
    fn save_import_settings_editor(&mut self) {
        let Some(state) = &self.import_settings_editor else {
            return;
        };
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let source_id = state.source_id.clone();
        let contact_bones = state.contact_bones.clone();
        let relative_path = state.relative_path.clone();
        let motion_model_sources = state.motion_pairing.as_ref().map(|pairing| {
            pairing
                .selected
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>()
        });
        let motion_original_model_source = state.motion_pairing.as_ref().and_then(|pairing| {
            pairing
                .original
                .as_ref()
                .map(|id| id.as_str().to_owned())
        });
        let mut manifest = self.asset_manifest.clone();
        let Some(entry) = manifest.get_mut(&source_id) else {
            return;
        };
        entry.import_settings.contact_bones = contact_bones;
        // Only a motion source carries a pairing at all; the outer `Option`
        // distinguishes "this source has no pairing field" from "the author
        // cleared the pairing".
        if let Some(paired) = motion_model_sources {
            entry.import_settings.motion_model_sources = paired;
            entry.import_settings.motion_original_model_source = motion_original_model_source;
        }
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;
        let source_path = project.assets_root().join(&relative_path);
        self.queue_model_import(source_id, source_path);
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.reimport_started",
                format!(
                    "reimporting `{}` in the background",
                    relative_path.display()
                ),
            ));
    }

    /// Creates a `*.retarget.json` map for the (source, target) skeleton pair
    /// and registers it like any other created asset (AP-5 creation flow for
    /// `anim.retarget_map_missing`).
    ///
    /// When a side binds to more than one skeleton (multiple skins in one
    /// imported file), the pair is ambiguous: rather than guessing via
    /// `skeleton_records.first()`, a picker window opens so the user chooses
    /// which skeleton on each side to map (AP-6 scope (b)). Exactly one
    /// record on both sides keeps the one-click behavior below.
    pub(super) fn create_retarget_map_from_browser(
        &mut self,
        source_index: usize,
        target_source_id: AssetId,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(source_index) else {
            return;
        };
        let source_relative_path = entry.relative_path.clone();
        let Some((source_id, _)) = self.manifest_entry_for(&source_relative_path) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.retarget_map_create_unregistered",
                    "register the source asset before creating a retarget map for it",
                ));
            return;
        };
        let source_records = self
            .asset_manifest
            .get(&source_id)
            .map(|entry| entry.import_settings.skeleton_records.clone())
            .unwrap_or_default();
        if source_records.is_empty() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    "source has no recorded skeleton; import it before creating a retarget map",
                ));
            return;
        }
        let target_records = self
            .asset_manifest
            .get(&target_source_id)
            .map(|entry| entry.import_settings.skeleton_records.clone())
            .unwrap_or_default();
        if target_records.is_empty() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    "target has no recorded skeleton; import it before creating a retarget map",
                ));
            return;
        }

        if source_records.len() == 1 && target_records.len() == 1 {
            self.write_retarget_map_for_pair(
                &project,
                &source_relative_path,
                &source_records[0],
                &target_source_id,
                &target_records[0],
            );
            return;
        }

        self.retarget_map_creation_picker = Some(RetargetMapCreationPickerState {
            source_relative_path,
            target_source_id,
            source_records,
            target_records,
            selected_source: 0,
            selected_target: 0,
        });
    }

    /// Generates and registers a `*.retarget.json` map for one resolved
    /// (source, target) skeleton pair.
    ///
    /// Shared by [`Self::create_retarget_map_from_browser`]'s one-click path
    /// and the multi-skin creation picker's confirm action (AP-6 scope (b)),
    /// so both write the map through the same asset-registration logic.
    pub(super) fn write_retarget_map_for_pair(
        &mut self,
        project: &ProjectRoot,
        source_relative_path: &Path,
        source_record: &engine::SkeletonRecord,
        target_source_id: &AssetId,
        target_record: &engine::SkeletonRecord,
    ) {
        let Some(source_skeleton_id) =
            AssetId::from_stable_id(engine_authoring::StableId::new(source_record.id.clone())).ok()
        else {
            return;
        };
        let Some(target_skeleton_id) =
            AssetId::from_stable_id(engine_authoring::StableId::new(target_record.id.clone())).ok()
        else {
            return;
        };
        let source_asset = crate::anim_ux::synthetic_skeleton_asset(
            source_skeleton_id,
            source_record.identity,
            &source_record.bones,
        );
        let target_asset = crate::anim_ux::synthetic_skeleton_asset(
            target_skeleton_id,
            target_record.identity,
            &target_record.bones,
        );
        let map = engine::generate_retarget_map(&source_asset, &target_asset);

        let source_stem = source_relative_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "source".to_owned());
        let target_stem = self
            .asset_manifest
            .get(target_source_id)
            .map(|entry| Path::new(&entry.path))
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "target".to_owned());
        let file_name = crate::anim_ux::retarget_map_file_name(&source_stem, &target_stem);
        let relative = file_name.clone();
        let destination = project.assets_root().join(&relative);
        if destination.exists() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.retarget_map_create_exists",
                    format!("retarget map destination already exists: {relative}"),
                ));
            return;
        }
        let json = match map.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.retarget_map_create_failed",
                        error.to_string(),
                    ));
                return;
            }
        };
        if let Err(error) = replace_file_contents(&destination, &json) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.retarget_map_create_failed",
                    format!("failed to write {relative}: {error}"),
                ));
            return;
        }
        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        let name = unique_asset_name(&source_stem, &manifest);
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(name),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(project, &manifest) {
            let _ = fs::remove_file(&destination);
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;
        self.asset_browser.refresh(&project.assets_root());
        self.asset_browser
            .select_relative_path(Path::new(&relative));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.retarget_map_created",
                format!(
                    "created retarget map `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
    }

    /// Renders the multi-skin retarget-map creation picker window opened by
    /// [`Self::create_retarget_map_from_browser`] (AP-6 scope (b)). Confirm
    /// writes the map for the currently selected pair through
    /// [`Self::write_retarget_map_for_pair`]; Cancel and closing the window
    /// both discard the picker state without writing anything.
    pub(super) fn show_retarget_map_creation_picker_window(&mut self, context: &egui::Context) {
        let Some(state) = &mut self.retarget_map_creation_picker else {
            return;
        };
        let model = crate::anim_ux::build_retarget_map_creation_picker_model(
            &self.asset_manifest,
            &state.source_records,
            &state.target_records,
        );
        let mut open = true;
        let mut action = None;
        egui::Window::new("Choose Retarget Skeletons")
            .open(&mut open)
            .default_width(360.0)
            .show(context, |ui| {
                action = crate::anim_ux::show_retarget_map_creation_picker(
                    ui,
                    &model,
                    &mut state.selected_source,
                    &mut state.selected_target,
                );
            });
        match action {
            Some(crate::anim_ux::RetargetMapCreationPickerAction::Confirm) => {
                let state = self
                    .retarget_map_creation_picker
                    .take()
                    .expect("state was matched as Some above");
                let Some(project) = self.project_root.clone() else {
                    return;
                };
                let Some(source_record) = state.source_records.get(state.selected_source).cloned()
                else {
                    return;
                };
                let Some(target_record) = state.target_records.get(state.selected_target).cloned()
                else {
                    return;
                };
                self.write_retarget_map_for_pair(
                    &project,
                    &state.source_relative_path,
                    &source_record,
                    &state.target_source_id,
                    &target_record,
                );
            }
            Some(crate::anim_ux::RetargetMapCreationPickerAction::Cancel) => {
                self.retarget_map_creation_picker = None;
            }
            None => {
                if !open {
                    self.retarget_map_creation_picker = None;
                }
            }
        }
    }

    pub(super) fn begin_asset_mutation(&mut self, index: usize, kind: AssetMutationKind) {
        let Some(source) = self
            .asset_browser
            .entries()
            .get(index)
            .map(|entry| entry.relative_path.clone())
        else {
            return;
        };
        let selection_contains_source = self
            .asset_browser
            .selected_paths()
            .any(|path| path == &source);
        if !selection_contains_source {
            self.asset_browser.select_path(&source, false);
        }
        let batch_operation = matches!(kind, AssetMutationKind::Move | AssetMutationKind::Trash)
            && self.asset_browser.selected_paths().count() > 1;
        let destination = if batch_operation {
            source
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned()
        } else {
            source.to_string_lossy().into_owned()
        };
        self.pending_asset_mutation = Some(PendingAssetMutation {
            source,
            destination,
            kind,
        });
    }

    pub(super) fn begin_folder_create(&mut self) {
        let parent = self.asset_browser.selected_folder().to_path_buf();
        self.pending_asset_mutation = Some(PendingAssetMutation {
            source: parent.clone(),
            destination: parent.join("New Folder").to_string_lossy().into_owned(),
            kind: AssetMutationKind::CreateFolder,
        });
    }

    pub(super) fn begin_folder_mutation(&mut self, source: PathBuf, kind: AssetMutationKind) {
        self.pending_asset_mutation = Some(PendingAssetMutation {
            destination: source.to_string_lossy().into_owned(),
            source,
            kind,
        });
    }

    pub(super) fn move_selected_assets_to_folder(&mut self, folder: PathBuf) {
        let sources = self
            .asset_browser
            .selected_paths()
            .cloned()
            .collect::<Vec<_>>();
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let moved_rust = sources.iter().any(|path| path.starts_with("scripts/rust"))
            || folder.starts_with("scripts/rust");
        match crate::asset_management::move_asset_batch(
            &project,
            &mut self.asset_manifest,
            &sources,
            &folder,
        ) {
            Ok(report) => {
                self.asset_browser.refresh(&project.assets_root());
                self.asset_browser.set_selected_folder(folder);
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.asset_batch_moved",
                        format!(
                            "moved {} paths and updated {} manifest entries",
                            report.moves.len(),
                            report.manifest_entries
                        ),
                    ));
                self.refresh_scene_problems();
                if moved_rust {
                    self.refresh_rust_after_asset_mutation(&project);
                }
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.asset_batch_move_failed",
                    error.to_string(),
                )),
        }
    }

    pub(super) fn show_asset_mutation_window(&mut self, context: &egui::Context) {
        let mut selected_sources = self
            .asset_browser
            .selected_paths()
            .cloned()
            .collect::<Vec<_>>();
        let Some(pending) = self.pending_asset_mutation.as_mut() else {
            return;
        };
        let supports_batch = matches!(
            pending.kind,
            AssetMutationKind::Move | AssetMutationKind::Trash
        );
        if !supports_batch || !selected_sources.iter().any(|path| path == &pending.source) {
            selected_sources.clear();
            selected_sources.push(pending.source.clone());
        }
        let source_count = selected_sources.len();
        let title = match pending.kind {
            AssetMutationKind::Rename => "Rename Asset",
            AssetMutationKind::Move if source_count > 1 => "Move Assets",
            AssetMutationKind::Move => "Move Asset",
            AssetMutationKind::Trash if source_count > 1 => "Delete Assets",
            AssetMutationKind::Trash => "Delete Asset",
            AssetMutationKind::CreateFolder => "Create Asset Folder",
            AssetMutationKind::RenameFolder => "Rename Asset Folder",
            AssetMutationKind::TrashFolder => "Delete Asset Folder",
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                if !matches!(pending.kind, AssetMutationKind::CreateFolder) {
                    if supports_batch && source_count > 1 {
                        ui.label(format!("Selected: {source_count} assets"));
                        for source in selected_sources.iter().take(4) {
                            ui.monospace(source.display().to_string());
                        }
                        if source_count > 4 {
                            ui.small(format!("...and {} more", source_count - 4));
                        }
                    } else {
                        ui.label(format!("Source: {}", pending.source.display()));
                    }
                }
                if !matches!(
                    pending.kind,
                    AssetMutationKind::Trash | AssetMutationKind::TrashFolder
                ) {
                    if matches!(pending.kind, AssetMutationKind::Move) && source_count > 1 {
                        ui.label("Destination folder relative to Assets");
                    } else {
                        ui.label("Destination path relative to Assets");
                    }
                    ui.text_edit_singleline(&mut pending.destination);
                } else {
                    ui.label(format!(
                        "Deleting {source_count} item(s) unregisters them and moves them to .engine/asset_trash, so they can still be recovered manually."
                    ));
                }
                control_row(ui, |ui| {
                    confirmed = ui.button("Apply").clicked();
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if !open || cancelled {
            self.pending_asset_mutation = None;
            return;
        }
        if !confirmed {
            return;
        }
        let pending = self
            .pending_asset_mutation
            .take()
            .expect("pending asset mutation exists while its window is open");
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let moved_rust = selected_sources
            .iter()
            .any(|source| source.starts_with("scripts/rust"))
            || Path::new(pending.destination.trim()).starts_with("scripts/rust");
        let result: Result<String, crate::asset_management::AssetManagementError> =
            match pending.kind {
                AssetMutationKind::Rename => crate::asset_management::move_asset(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "moved {} to {} ({} manifest entries updated)",
                        report.source.display(),
                        report.destination.display(),
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::Move if source_count > 1 => {
                    crate::asset_management::move_asset_batch(
                        &project,
                        &mut self.asset_manifest,
                        &selected_sources,
                        Path::new(pending.destination.trim()),
                    )
                    .map(|report| {
                        format!(
                            "moved {} assets and updated {} manifest entries",
                            report.moves.len(),
                            report.manifest_entries
                        )
                    })
                }
                AssetMutationKind::Move => crate::asset_management::move_asset(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "moved {} to {} ({} manifest entries updated)",
                        report.source.display(),
                        report.destination.display(),
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::Trash if source_count > 1 => {
                    crate::asset_management::move_asset_paths_to_trash(
                        &project,
                        &mut self.asset_manifest,
                        &selected_sources,
                    )
                    .map(|report| {
                        format!(
                            "deleted {} assets ({} registered assets, recoverable in .engine/asset_trash)",
                            source_count,
                            report.manifest_entries
                        )
                    })
                }
                AssetMutationKind::Trash => crate::asset_management::move_asset_to_trash(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                )
                .map(|report| {
                    format!(
                        "deleted {} (recoverable in .engine/asset_trash)",
                        report.source.display()
                    )
                }),
                AssetMutationKind::CreateFolder => crate::asset_management::create_asset_folder(
                    &project,
                    Path::new(pending.destination.trim()),
                )
                .map(|path| format!("created asset folder {}", path.display())),
                AssetMutationKind::RenameFolder => crate::asset_management::move_asset_path(
                    &project,
                    &mut self.asset_manifest,
                    &pending.source,
                    Path::new(pending.destination.trim()),
                )
                .map(|report| {
                    format!(
                        "renamed folder and updated {} manifest entries",
                        report.manifest_entries
                    )
                }),
                AssetMutationKind::TrashFolder => {
                    crate::asset_management::move_asset_paths_to_trash(
                        &project,
                        &mut self.asset_manifest,
                        std::slice::from_ref(&pending.source),
                    )
                    .map(|report| {
                        format!(
                            "deleted folder ({} registered assets, recoverable in .engine/asset_trash)",
                            report.manifest_entries
                        )
                    })
                }
            };
        match result {
            Ok(message) => {
                self.asset_browser.refresh(&project.assets_root());
                self.refresh_scene_problems();
                if moved_rust {
                    self.refresh_rust_after_asset_mutation(&project);
                }
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.asset_moved",
                        message,
                    ));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.asset_move_failed",
                    error.to_string(),
                )),
        }
    }

    fn refresh_rust_after_asset_mutation(&mut self, project: &ProjectRoot) {
        match engine_authoring::refresh_game_module_indexes(project) {
            Ok(()) => {
                self.component_source_index =
                    ComponentSourceIndex::build(&project.rust_scripts_dir());
                // Relocating a source changes its Rust module path. Rewriting
                // `use` paths is a separate refactoring feature, so the build
                // that follows is what reports the references left behind.
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.rust_module_path_changed",
                        "Rust module paths changed; `use` paths are not updated automatically, so fix any reference the next build reports",
                    ));
                self.request_game_build_after_edit();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.game_index_refresh_failed",
                    error.to_string(),
                )),
        }
    }

    #[cfg(test)]
    pub(super) fn save_active_material(&mut self) {
        let Some(relative_path) = self.material_editor.active.clone() else {
            return;
        };
        let Some(material) = self.material_editor.active_material().cloned() else {
            return;
        };
        self.persist_material(relative_path, material);
    }

    /// Persists one captured Material value and schedules one preview rebuild.
    fn persist_material(
        &mut self,
        relative_path: PathBuf,
        material: engine_authoring::MaterialAsset,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let relative = relative_path.to_string_lossy();
        let result = material
            .validate()
            .map_err(|error| error.to_string())
            .and_then(|()| material.to_json().map_err(|error| error.to_string()))
            .and_then(|json| {
                project
                    .resolve_asset_for_write(&relative)
                    .map_err(|error| error.to_string())
                    .and_then(|path| {
                        replace_file_contents(&path, &json).map_err(|error| error.to_string())
                    })
            });
        match result {
            Ok(()) => {
                self.material_scene_preview_deadline = Some(std::time::Instant::now());
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_save_failed",
                    format!("failed to save {}: {error}", relative_path.display()),
                )),
        }
    }

    /// Opens the asset browser entry at `index` based on its [`AssetKind`].
    pub(super) fn open_from_browser(&mut self, index: usize, context: &egui::Context) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.open_blocked_while_playing",
                    "stop Play before opening another document",
                ));
            return;
        }

        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let kind = entry.kind;

        let relative = match entry.relative_path.to_str() {
            Some(s) => s.to_string(),
            None => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.open_asset_failed",
                        "asset path contains non-UTF-8 characters",
                    ));
                return;
            }
        };

        let Some(root) = &self.project_root else {
            return;
        };

        let abs_path = match root.resolve_asset(&relative) {
            Ok(p) => p,
            Err(e) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.open_asset_failed",
                        format!("path resolution failed: {e}"),
                    ));
                return;
            }
        };

        let pending = match kind {
            AssetKind::Scene => PendingOpen::Scene(abs_path),
            AssetKind::Graph => PendingOpen::Graph(abs_path),
            AssetKind::UiDocument => PendingOpen::Ui(abs_path),
            AssetKind::GraphView => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.open_asset_view_only",
                        "open the corresponding .graph.json file instead of the .graph.view.json",
                    ));
                return;
            }
            AssetKind::Material => {
                let result = fs::read_to_string(&abs_path)
                    .map_err(|error| error.to_string())
                    .and_then(|json| {
                        engine_authoring::MaterialAsset::from_json(&json)
                            .map_err(|error| error.to_string())
                    });
                match result {
                    Ok(material) => {
                        self.material_editor
                            .open_material(PathBuf::from(relative), material);
                        self.show_material_editor = true;
                    }
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.material_open_failed",
                                format!("failed to open {}: {error}", abs_path.display()),
                            ))
                    }
                }
                return;
            }
            AssetKind::Texture => {
                match load_texture_preview(context, &abs_path, PathBuf::from(&relative)) {
                    Ok(preview) => self.texture_preview = Some(preview),
                    Err(error) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.texture_preview_failed",
                                format!("failed to preview {}: {error}", abs_path.display()),
                            ))
                    }
                }
                return;
            }
            AssetKind::RetargetMap => {
                self.open_retarget_map_editor(PathBuf::from(&relative), abs_path);
                return;
            }
            AssetKind::AnimationSet => {
                self.open_animation_set_editor(PathBuf::from(&relative), abs_path);
                return;
            }
            AssetKind::Mesh
            | AssetKind::AnimationClip
            | AssetKind::MotionSource
            | AssetKind::Audio
            | AssetKind::Prefab
            | AssetKind::NavMesh
            | AssetKind::Script
            | AssetKind::RustComponent
            | AssetKind::RustResource
            | AssetKind::RustSystem
            | AssetKind::RustModule => {
                if let Err(error) = open::that(&abs_path) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.external_asset_open_failed",
                            format!(
                                "the OS could not open {} for editing or preview: {error}",
                                abs_path.display()
                            ),
                        ));
                }
                return;
            }
        };
        self.request_open(pending);
    }

    /// Creates a valid UI document at a collision-free project-relative path
    /// and opens it in the visual builder.
    pub(super) fn create_ui_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let directory = project.assets_root().join("ui");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.ui_document_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = (1_u32..)
            .map(|suffix| {
                let name = if suffix == 1 {
                    "new_ui.ui.json".to_owned()
                } else {
                    format!("new_ui_{suffix}.ui.json")
                };
                directory.join(name)
            })
            .find(|candidate| !candidate.exists())
            .unwrap_or_else(|| directory.join("new_ui_document.ui.json"));
        let document = engine_authoring::UiDocument::default();
        let result = document
            .to_json_string()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                self.ui_builder.selected_node = Some("root".to_owned());
                self.request_open(PendingOpen::Ui(path));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.ui_document_create_failed",
                    format!("could not create UI document: {error}"),
                )),
        }
    }

    /// Creates an empty scene document at a collision-free path under
    /// `assets/scenes/` and opens it (respecting the unsaved-changes flow).
    pub(super) fn create_scene_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.scene_create_without_project",
                    "open a project before creating scenes",
                ));
            return;
        };
        let directory = project.assets_root().join("scenes");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.scene_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_scene", ".scene.json");
        let scene_json = "{\n    \"schema_version\": 1,\n    \"entities\": []\n}\n";
        match replace_file_contents(&path, scene_json) {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                self.request_open(PendingOpen::Scene(path));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.scene_create_failed",
                    format!("could not create scene: {error}"),
                )),
        }
    }

    /// Creates a registered Animation Graph with the required Entry node.
    ///
    /// Both the semantic `*.anim.graph.json` file and its editor-only sibling
    /// view are written through `EditorSession`, then the semantic asset is
    /// registered in the project manifest before it is opened in a tab.
    pub(super) fn create_animation_graph_document(&mut self) {
        self.create_animation_graph_document_in_folder(Path::new("animation"));
    }

    /// Creates an Animation Graph below the selected Asset Browser folder.
    ///
    /// The browser supplies an asset-relative folder, so the same graph
    /// creation path can be used by the File menu and folder context menus
    /// without duplicating graph serialization or manifest registration.
    pub(super) fn create_animation_graph_document_in_folder(&mut self, destination_folder: &Path) {
        self.create_animation_graph_document_internal(destination_folder, None);
    }

    /// Creates a Graph for one Controller, assigns it while the Scene tab is
    /// still active, and only then opens the new Graph tab.
    pub(super) fn create_animation_graph_for_controller(&mut self, entity: EntityId) {
        self.create_animation_graph_document_internal(Path::new("animation"), Some(entity));
    }

    fn create_animation_graph_document_internal(
        &mut self,
        destination_folder: &Path,
        controller: Option<EntityId>,
    ) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_graph_create_while_playing",
                    "stop Play mode before creating an Animation Graph",
                ));
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_graph_create_without_project",
                    "open a project before creating an Animation Graph",
                ));
            return;
        };
        if !destination_folder.as_os_str().is_empty()
            && asset_relative_path_string(destination_folder).is_none()
        {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    "Animation Graph destination must be an asset-relative folder",
                ));
            return;
        }
        let directory = project.assets_root().join(destination_folder);
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }

        let path = unique_document_path(&directory, "new_animation", ".anim.graph.json");
        let Some(relative) = path
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!(
                        "could not derive an asset-relative path for {}",
                        path.display()
                    ),
                ));
            return;
        };

        let mut graph_session = EditorSession::empty_animation_graph();
        if let Err(error) = graph_session.save_as(path.clone()) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_create_failed",
                    format!("could not create Animation Graph: {error}"),
                ));
            return;
        }

        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        let name = unique_asset_name("new_animation", &manifest);
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(name),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            let _ = fs::remove_file(&path);
            if let Some(view_path) = crate::document::derive_view_path(&path) {
                let _ = fs::remove_file(view_path);
            }
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_graph_manifest_save_failed",
                    error,
                ));
            return;
        }

        self.asset_manifest = manifest;
        self.asset_browser.refresh(&project.assets_root());
        self.asset_browser
            .select_relative_path(Path::new(&relative));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "editor.animation_graph_created",
                format!(
                    "created Animation Graph `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
        if let Some(entity) = controller {
            self.assign_animation_controller_asset_reference(entity, "graph", asset_id);
        }
        self.request_open(PendingOpen::Graph(path));
    }

    /// Creates and registers an empty Animation Set for one Animation Graph.
    ///
    /// The set starts empty because imported clip choices are project-specific.
    /// It remains a valid editing document, while controller validation keeps
    /// playback inactive until every graph motion slot has a clip binding.
    pub(super) fn create_animation_set_document_in_folder(
        &mut self,
        graph: Option<AssetId>,
        destination_folder: &Path,
    ) {
        self.create_animation_set_document_internal(graph, destination_folder, None);
    }

    /// Creates a Set beside its Graph, assigns it to the Controller, and opens
    /// the dedicated typed Animation Set editor.
    pub(super) fn create_animation_set_for_controller(&mut self, entity: EntityId, graph: AssetId) {
        if let Err(error) = self.resolve_animation_asset(&graph, engine::AssetKind::AnimationGraph)
        {
            self.report_error("editor.animation_set_create_failed", error);
            return;
        }
        let destination = self
            .asset_manifest
            .get(&graph)
            .and_then(|entry| Path::new(&entry.path).parent())
            .unwrap_or(Path::new("animation"))
            .to_path_buf();
        self.create_animation_set_document_internal(Some(graph), &destination, Some(entity));
    }

    fn create_animation_set_document_internal(
        &mut self,
        graph: Option<AssetId>,
        destination_folder: &Path,
        controller: Option<EntityId>,
    ) {
        if self.is_playing() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_set_create_while_playing",
                    "stop Play mode before creating an Animation Set",
                ));
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.animation_set_create_without_project",
                    "open a project before creating an Animation Set",
                ));
            return;
        };
        if !destination_folder.as_os_str().is_empty()
            && asset_relative_path_string(destination_folder).is_none()
        {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    "Animation Set destination must be an asset-relative folder",
                ));
            return;
        }
        let directory = project.assets_root().join(destination_folder);
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_animation", ".animset.json");
        let Some(relative) = path
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!(
                        "could not derive an asset-relative path for {}",
                        path.display()
                    ),
                ));
            return;
        };
        let document = graph
            .map(engine_authoring::AnimationSet::new)
            .unwrap_or_else(engine_authoring::AnimationSet::empty);
        let result = document
            .to_canonical_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_create_failed",
                    format!("could not create Animation Set: {error}"),
                ));
            return;
        }

        let asset_id = AssetId::generate();
        let mut manifest = self.asset_manifest.clone();
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative.clone(),
                name: Some(unique_asset_name("new_animation_set", &manifest)),
                import_settings: engine::ImportSettings::default(),
            },
        );
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            let _ = fs::remove_file(&path);
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.animation_set_manifest_save_failed",
                    error,
                ));
            return;
        }

        self.asset_manifest = manifest;
        self.asset_browser.refresh(&project.assets_root());
        self.asset_browser
            .select_relative_path(Path::new(&relative));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "editor.animation_set_created",
                format!(
                    "created Animation Set `{relative}` as `{}`",
                    asset_id.as_str()
                ),
            ));
        if let Some(entity) = controller {
            self.assign_animation_controller_asset_reference(entity, "animation_set", asset_id);
        }
        self.open_animation_set_editor(PathBuf::from(&relative), path);
    }

    /// Opens a registered Animation Graph after checking its manifest kind.
    pub(super) fn open_animation_graph_asset(&mut self, asset: &AssetId) {
        match self.resolve_animation_asset(asset, engine::AssetKind::AnimationGraph) {
            Ok((_, absolute)) => self.request_open(PendingOpen::Graph(absolute)),
            Err(error) => self.report_error("editor.animation_graph_open_failed", error),
        }
    }

    /// Opens a registered Animation Set in its dedicated typed editor.
    pub(super) fn open_animation_set_asset(&mut self, asset: &AssetId) {
        match self.resolve_animation_asset(asset, engine::AssetKind::AnimationSet) {
            Ok((relative, absolute)) => self.open_animation_set_editor(relative, absolute),
            Err(error) => self.report_error("editor.animation_set_open_failed", error),
        }
    }

    /// Returns the manifest ID of the Animation Graph open in the active tab.
    ///
    /// Graph documents store their own [`engine_authoring::GraphId`], while
    /// Animation Sets refer to the project-level [`AssetId`] registered for
    /// the graph file. Resolving through the manifest keeps the reverse lookup
    /// rename-safe without adding a second persisted reference to the graph.
    pub(super) fn current_animation_graph_asset(&self) -> Option<AssetId> {
        if !self.session.is_animation_graph() {
            return None;
        }
        let graph_path = self.session.current_document_path()?;
        let project = self.project_root.as_ref()?;
        self.asset_manifest
            .iter()
            .find(|(_, entry)| {
                project
                    .resolve_asset(&entry.path)
                    .is_ok_and(|candidate| candidate == graph_path)
            })
            .map(|(asset, _)| asset.clone())
    }

    /// Finds Animation Sets whose persisted target is `graph`.
    ///
    /// The relation is intentionally derived from each Set's forward
    /// reference. The Graph document therefore remains independent of asset
    /// creation, deletion, and reassignment, while this editor list always
    /// reflects the saved project state.
    pub(super) fn animation_sets_for_graph(&self, graph: &AssetId) -> Vec<AssetChoice> {
        let Some(project) = self.project_root.as_ref() else {
            return Vec::new();
        };
        let assets_root = project.assets_root();
        let mut sets = self
            .asset_manifest
            .iter()
            .filter(|(_, entry)| {
                manifest_path_matches_asset_kind(
                    engine::AssetKind::AnimationSet,
                    Path::new(&entry.path),
                    Some(assets_root.as_path()),
                )
            })
            .filter_map(|(asset, entry)| {
                let path = project.resolve_asset(&entry.path).ok()?;
                let json = fs::read_to_string(path).ok()?;
                let set = engine_authoring::AnimationSet::from_json(&json).ok()?;
                (set.graph.as_ref() == Some(graph)).then(|| AssetChoice {
                    label: entry.name.clone().unwrap_or_else(|| entry.path.clone()),
                    id: asset.clone(),
                })
            })
            .collect::<Vec<_>>();
        sets.sort_by(|left, right| {
            left.label
                .cmp(&right.label)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        sets
    }

    /// Creates an Animation Set beside its target Graph and opens it.
    pub(super) fn create_animation_set_for_graph(&mut self, graph: AssetId) {
        if let Err(error) = self.resolve_animation_asset(&graph, engine::AssetKind::AnimationGraph)
        {
            self.report_error("editor.animation_set_create_failed", error);
            return;
        }
        let destination = self
            .asset_manifest
            .get(&graph)
            .and_then(|entry| Path::new(&entry.path).parent())
            .unwrap_or(Path::new("animation"))
            .to_path_buf();
        self.create_animation_set_document_internal(Some(graph), &destination, None);
    }

    /// Resolves one registered author-owned animation asset without accepting
    /// a missing manifest row, a mismatched suffix, or a missing file.
    fn resolve_animation_asset(
        &self,
        asset: &AssetId,
        expected: engine::AssetKind,
    ) -> Result<(PathBuf, PathBuf), String> {
        let project = self
            .project_root
            .as_ref()
            .ok_or_else(|| "no project is open".to_owned())?;
        let entry = self
            .asset_manifest
            .get(asset)
            .ok_or_else(|| format!("asset `{}` is not registered", asset.as_str()))?;
        let relative = PathBuf::from(&entry.path);
        let assets_root = project.assets_root();
        if !manifest_path_matches_asset_kind(expected, &relative, Some(assets_root.as_path())) {
            return Err(format!(
                "asset `{}` is not the expected animation asset kind",
                asset.as_str()
            ));
        }
        let absolute = project
            .resolve_asset(&entry.path)
            .map_err(|error| error.to_string())?;
        if !absolute.is_file() {
            return Err(format!("asset file `{}` does not exist", entry.path));
        }
        Ok((relative, absolute))
    }

    /// Creates a default standalone material asset under `assets/materials/`
    /// and opens it in the Material Editor.
    pub(super) fn create_material_document(&mut self) {
        let Some(project) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.material_create_without_project",
                    "open a project before creating materials",
                ));
            return;
        };
        let directory = project.assets_root().join("materials");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_create_failed",
                    format!("could not create {}: {error}", directory.display()),
                ));
            return;
        }
        let path = unique_document_path(&directory, "new_material", ".material.json");
        let material = engine_authoring::MaterialAsset::default();
        let result = material
            .to_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                replace_file_contents(&path, &json).map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.asset_browser.refresh(&project.assets_root());
                let relative = path
                    .strip_prefix(project.assets_root())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|_| path.clone());
                self.material_editor.open_material(relative, material);
                self.show_material_editor = true;
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_create_failed",
                    format!("could not create material: {error}"),
                )),
        }
    }

    /// Opens the OS file manager at the folder containing an asset.
    pub(super) fn show_asset_in_explorer(&mut self, relative: &Path) {
        let Some(project) = self.project_root.as_ref() else {
            return;
        };
        let absolute = project.assets_root().join(relative);
        let target = if absolute.is_dir() {
            absolute.clone()
        } else {
            absolute
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(absolute.clone())
        };
        if let Err(error) = open::that(&target) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.show_in_explorer_failed",
                    format!("could not open {}: {error}", target.display()),
                ));
        }
    }

    /// Creates an entity from a registered mesh asset via the context menu,
    /// mirroring the Scene View drop behavior.
    pub(super) fn add_mesh_asset_to_scene(&mut self, index: usize) {
        let Some(entry) = self.asset_browser.entries().get(index).cloned() else {
            return;
        };
        let Some((asset_id, _)) = self.manifest_entry_for(&entry.relative_path) else {
            self.push_notification(
                EditorNotificationLevel::Info,
                "Register the asset before adding it to the scene".into(),
            );
            return;
        };
        match self.session.create_entity_from_mesh_asset(asset_id, None) {
            Ok(entity) => {
                self.select_single_entity(Some(entity));
                self.refresh_scene_problems();
            }
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    fn manifest_entry_for(&self, relative: &Path) -> Option<(AssetId, String)> {
        let relative = relative.to_string_lossy().replace('\\', "/");
        self.asset_manifest
            .iter()
            .find(|(_, entry)| entry.path == relative)
            .map(|(id, entry)| (id.clone(), entry.path.clone()))
    }

    pub(super) fn instantiate_prefab_from_browser(&mut self, index: usize) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        if entry.kind != AssetKind::Prefab {
            return;
        }
        let source = project.assets_root().join(&entry.relative_path);
        match crate::prefab_workflow::instantiate_prefab(
            &mut self.session,
            &source,
            self.selected_entity.clone(),
        ) {
            Ok(root) => {
                self.select_single_entity(Some(root));
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_instantiate_failed",
                    error.to_string(),
                )),
        }
    }

    /// Instantiates the prefab generated for a registered glTF/GLB source.
    ///
    /// The source row is what the author places (ADR 0075), so this resolves
    /// the hidden artifact on their behalf and explains the two states where
    /// there is nothing to place yet: an import that has not run, and a
    /// document that draws nothing.
    pub(super) fn instantiate_model_source(&mut self, asset_id: &engine_authoring::AssetId) {
        self.instantiate_model_source_under(asset_id, self.selected_entity.clone());
    }

    /// Instantiates a model's generated prefab under an explicit parent.
    pub(super) fn instantiate_model_source_under(
        &mut self,
        asset_id: &engine_authoring::AssetId,
        parent: Option<EntityId>,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let relative = self
            .asset_manifest
            .get(asset_id)
            .and_then(|entry| entry.import_settings.generated_prefab.clone());
        let Some(relative) = relative else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.model_not_imported_yet",
                    format!(
                        "`{}` has no placeable content yet; wait for its import to finish, or use Reimport",
                        asset_id.as_str()
                    ),
                ));
            return;
        };
        let source = project.path().join(&relative);
        if !source.is_file() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.generated_prefab_missing",
                    format!(
                        "the generated content for `{}` is missing; use Reimport to rebuild it",
                        asset_id.as_str()
                    ),
                ));
            return;
        }
        match crate::prefab_workflow::instantiate_prefab(&mut self.session, &source, parent) {
            Ok(root) => {
                // A model arrives as a whole subtree; folding it keeps the
                // hierarchy reading as one placed object until the author
                // opens it.
                self.collapsed_entities.insert(root.clone());
                self.select_single_entity(Some(root));
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_instantiate_failed",
                    error.to_string(),
                )),
        }
    }

    pub(super) fn create_prefab_from_selected_entity(&mut self) {
        let Some(selected) = self.selected_entity.clone() else {
            return;
        };
        self.create_prefab_from_selected_entity_id(selected);
    }

    /// Opens the destination picker for one hierarchy entity and persists the
    /// resulting prefab through the same manifest registration path used by
    /// drag-and-drop creation.
    pub(super) fn create_prefab_from_selected_entity_id(&mut self, selected: EntityId) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(entity) = self.session.scene_entity(&selected) else {
            return;
        };
        let default_name = format!("{}.prefab.json", prefab_file_stem(&entity.name));
        let directory = project
            .assets_root()
            .join(self.asset_browser.selected_folder());
        let Some(destination) = rfd::FileDialog::new()
            .set_directory(directory)
            .set_file_name(default_name)
            .add_filter("Prefab", &["json"])
            .save_file()
        else {
            return;
        };
        self.create_prefab_asset(&project, std::slice::from_ref(&selected), destination);
    }

    /// Creates a prefab directly in the folder that received an Entity drop.
    ///
    /// The generated filename is collision-free and the scene entity remains
    /// unchanged. This is the safe default for drag-and-drop; replacing the
    /// entity with an instance remains an explicit future operation.
    pub(super) fn create_prefab_from_entity_in_folder(
        &mut self,
        entity: EntityId,
        destination_folder: PathBuf,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(source_entity) = self.session.scene_entity(&entity) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    format!("entity `{entity}` is no longer present in the open scene"),
                ));
            return;
        };
        let stem = prefab_file_stem(&source_entity.name);
        let mut counter = 1_u32;
        let relative = loop {
            let filename = if counter == 1 {
                format!("{stem}.prefab.json")
            } else {
                format!("{stem}_{counter}.prefab.json")
            };
            let candidate = destination_folder.join(filename);
            if !project.assets_root().join(&candidate).exists() {
                break candidate;
            }
            counter = counter.saturating_add(1);
        };
        let Some(relative_string) = asset_relative_path_string(&relative) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    "generated prefab path contains non-UTF-8 characters",
                ));
            return;
        };
        let destination = match project.resolve_asset_for_write(&relative_string) {
            Ok(path) => path,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.prefab_create_failed",
                        format!("prefab destination is not writable: {error}"),
                    ));
                return;
            }
        };
        self.create_prefab_asset(&project, std::slice::from_ref(&entity), destination);
    }

    /// Writes a prefab and registers it in the project manifest as one editor
    /// operation. Existing files are rejected to avoid silently destroying a
    /// user's authored asset when a drag creates a same-named prefab.
    fn create_prefab_asset(
        &mut self,
        project: &ProjectRoot,
        selection: &[EntityId],
        destination: PathBuf,
    ) {
        if destination.exists() {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.prefab_create_exists",
                    format!(
                        "prefab destination already exists: {}",
                        destination.display()
                    ),
                ));
            return;
        }
        let Some(relative) = destination
            .strip_prefix(project.assets_root())
            .ok()
            .and_then(asset_relative_path_string)
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_outside_assets",
                    "prefabs must be created inside the project Assets folder",
                ));
            return;
        };
        if Path::new(&relative).starts_with("scripts") {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_script_folder",
                    "script folders only accept their declared script type",
                ));
            return;
        }
        let Some(scene) = self.session.scene() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.prefab_create_no_scene",
                    "open a scene before creating a prefab",
                ));
            return;
        };
        match crate::prefab_workflow::create_prefab_from_selection(scene, selection, &destination) {
            Ok(_) => {
                let asset_id = AssetId::generate();
                let mut manifest = self.asset_manifest.clone();
                let name = unique_asset_name(
                    destination
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("prefab"),
                    &manifest,
                );
                manifest.insert(
                    asset_id.clone(),
                    engine::ManifestEntry {
                        path: relative.clone(),
                        name: Some(name),
                        import_settings: engine::ImportSettings::default(),
                    },
                );
                if let Err(error) = save_asset_manifest(project, &manifest) {
                    let _ = fs::remove_file(&destination);
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "editor.prefab_manifest_save_failed",
                            error,
                        ));
                    return;
                }
                self.asset_manifest = manifest;
                self.asset_browser.refresh(&project.assets_root());
                self.asset_browser
                    .select_relative_path(Path::new(&relative));
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.prefab_created",
                        format!("created prefab `{relative}` as `{}`", asset_id.as_str()),
                    ));
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.prefab_create_failed",
                    error.to_string(),
                )),
        }
    }

    /// Registers a model source if needed and queues it for import.
    ///
    /// This is the single entry point every arrival path funnels through
    /// (ADR 0075), so a file copied in with the file manager, dropped on the
    /// editor, or pulled in by version control all end up importable without
    /// the author performing a separate step.
    pub(super) fn auto_import_model_source(&mut self, relative_path: &Path) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let is_motion =
            engine::asset_path_matches_kind(engine::AssetKind::MotionSource, relative_path);
        if !is_motion
            && !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, relative_path)
        {
            return;
        }
        let source_path = project.assets_root().join(relative_path);
        if !source_path.is_file() {
            return;
        }
        if is_motion {
            match engine::classify_vmd_path(&source_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        format!(
                            "`{}` contains camera, light, or self-shadow motion and was not registered as a model MotionSource",
                            relative_path.display()
                        ),
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        format!("`{}` contains no animation keys", relative_path.display()),
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect `{}`: {error}", relative_path.display()),
                    ));
                    return;
                }
            }
        }
        let Some(relative_string) = asset_relative_path_string(relative_path) else {
            return;
        };
        let existing = self
            .asset_manifest
            .iter()
            .find(|(_, entry)| {
                normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative_string)
            })
            .map(|(id, _)| id.clone());

        let asset_id = match existing {
            Some(id) => id,
            None => {
                let id = engine_authoring::id::AssetId::generate();
                let name = unique_asset_name(
                    relative_path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(if is_motion { "motion" } else { "model" }),
                    &self.asset_manifest,
                );
                let mut import_settings = engine::ImportSettings::default();
                if is_motion {
                    // A motion cannot be imported without a paired PMX (ADR
                    // 0097 §3). Picking it automatically when the project
                    // holds exactly one keeps the common single-character
                    // case a drag-and-drop away; anything ambiguous is left
                    // unpaired for the author to choose in Import Settings,
                    // since a wrong guess bakes a plausible-looking but wrong
                    // clip.
                    import_settings.motion_model_sources = sole_pmx_model_source(&self.asset_manifest)
                        .map(|id| vec![id.as_str().to_owned()])
                        .unwrap_or_default();
                }
                let mut manifest = self.asset_manifest.clone();
                manifest.insert(
                    id.clone(),
                    engine::ManifestEntry {
                        path: relative_string.clone(),
                        name: Some(name),
                        import_settings,
                    },
                );
                if let Err(error) = save_asset_manifest(&project, &manifest) {
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::error(
                            "asset.manifest_save_failed",
                            error,
                        ));
                    return;
                }
                if let Some(watcher) = &mut self.file_watcher {
                    watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                }
                self.asset_manifest = manifest;
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "asset.model_auto_registered",
                        format!("registered model `{relative_string}` as `{}`", id.as_str()),
                    ));
                id
            }
        };
        self.queue_model_import(asset_id, source_path);
    }

    /// Adds one source to the import queue, ignoring duplicates.
    pub(super) fn queue_model_import(
        &mut self,
        asset_id: engine_authoring::AssetId,
        source_path: PathBuf,
    ) {
        // Deduplicate against the running job as well as the queue: a burst
        // of writes reports the same source many times, and re-importing what
        // is already in flight would repeat the whole parse for nothing.
        // `Reimport` stays available for a change that lands mid-import.
        if self.asset_import.active_source() == Some(&asset_id)
            || self
                .pending_model_imports
                .iter()
                .any(|(queued, _)| queued == &asset_id)
        {
            return;
        }
        self.pending_model_imports
            .push_back((asset_id, source_path));
        self.start_next_model_import();
    }

    /// Starts the next queued import when the single worker is free.
    pub(super) fn start_next_model_import(&mut self) {
        if self.asset_import.is_running() {
            return;
        }
        let Some(project) = self.project_root.clone() else {
            self.pending_model_imports.clear();
            return;
        };
        let Some((asset_id, source_path)) = self.pending_model_imports.pop_front() else {
            return;
        };
        // Every registered source's ledger is offered so the dedupe rule
        // (ADR 0077 §4) can recognize a rig imported from a different
        // source, not just this source's own prior import.
        let existing_skeletons: Vec<engine::SkeletonRecord> = self
            .asset_manifest
            .iter()
            .flat_map(|(_, entry)| entry.import_settings.skeleton_records.iter().cloned())
            .collect();
        // This source's own contact-bone override (ADR 0080 §1, AP-5); empty
        // keeps the default foot/ankle/toe name heuristic.
        let contact_bones = self
            .asset_manifest
            .get(&asset_id)
            .map(|entry| entry.import_settings.contact_bones.clone())
            .unwrap_or_default();
        let started = if engine::asset_path_matches_kind(
            engine::AssetKind::MotionSource,
            &source_path,
        ) {
            match engine::classify_vmd_path(&source_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        format!(
                            "`{}` is scene-level VMD motion and was not paired with a PMX model",
                            source_path.display()
                        ),
                    ));
                    self.start_next_model_import();
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        format!("`{}` contains no animation keys", source_path.display()),
                    ));
                    self.start_next_model_import();
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect `{}`: {error}", source_path.display()),
                    ));
                    self.start_next_model_import();
                    return;
                }
            }
            let targets = self.paired_motion_models(&asset_id);
            if targets.is_empty() {
                // Not an error the author can act on from a background queue,
                // so it is reported once here and again by scene validation
                // (`scene.motion_source_unpaired`) if the motion is used.
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "asset.motion_source_unpaired",
                        format!(
                            "`{}` is not paired with a PMX model yet; choose one in Import Settings to bake its clips",
                            source_path.display()
                        ),
                    ));
                self.start_next_model_import();
                return;
            }
            let original_model_is_configured = self
                .asset_manifest
                .get(&asset_id)
                .is_some_and(|entry| {
                    entry
                        .import_settings
                        .motion_original_model_source
                        .is_some()
                });
            let original_model = self.original_motion_model(&asset_id).map(
                |(model_source_id, model_relative)| crate::asset_import::MotionImportTarget {
                    contact_bones: self
                        .asset_manifest
                        .get(&model_source_id)
                        .map(|entry| entry.import_settings.contact_bones.clone())
                        .unwrap_or_default(),
                    model_source_id,
                    model_path: project.assets_root().join(model_relative),
                },
            );
            if original_model_is_configured && original_model.is_none() {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.motion_original_model_unregistered",
                        format!(
                            "`{}` selects an original PMX that is no longer registered; choose another original model or select Direct bake in Import Settings",
                            source_path.display()
                        ),
                    ));
                self.start_next_model_import();
                return;
            }
            let retarget_maps = engine::load_registered_retarget_maps(
                &project.assets_root(),
                &self.asset_manifest,
            );
            self.asset_import
                .start_vmd(crate::asset_import::MotionImportJob {
                    project_path: project.path().to_path_buf(),
                    source_id: asset_id,
                    source_path: source_path.clone(),
                    targets: targets
                        .into_iter()
                        .map(|(model_source_id, model_relative)| {
                            let contact_bones = self
                                .asset_manifest
                                .get(&model_source_id)
                                .map(|entry| entry.import_settings.contact_bones.clone())
                                .unwrap_or_default();
                            crate::asset_import::MotionImportTarget {
                                model_source_id,
                                model_path: project.assets_root().join(model_relative),
                                contact_bones,
                            }
                        })
                        .collect(),
                    original_model,
                    retarget_maps,
                    existing_skeletons,
                    contact_bones,
                })
        } else {
            let existing_humanoid_profiles = self
                .asset_manifest
                .get(&asset_id)
                .map(|entry| entry.import_settings.humanoid_profiles.clone())
                .unwrap_or_default();
            self.asset_import.start_gltf(
                project.path().to_path_buf(),
                asset_id,
                source_path.clone(),
                existing_skeletons,
                existing_humanoid_profiles,
                contact_bones,
            )
        };
        if let Err(error) = started {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.gltf_import_start_failed",
                    format!(
                        "failed to start import of `{}`: {error}",
                        source_path.display()
                    ),
                ));
        }
    }

    /// Resolves a motion source's selected PMX models to stable IDs and
    /// project-relative paths, dropping malformed or dangling targets.
    fn paired_motion_models(
        &self,
        motion_id: &engine_authoring::AssetId,
    ) -> Vec<(engine_authoring::AssetId, String)> {
        let Some(entry) = self.asset_manifest.get(motion_id) else {
            return Vec::new();
        };
        let mut resolved = Vec::new();
        for paired in entry.import_settings.resolved_motion_model_sources() {
            let Ok(model_id) = engine_authoring::AssetId::from_stable_id(
                engine_authoring::StableId::new(paired),
            ) else {
                continue;
            };
            let Some(model_entry) = self.asset_manifest.get(&model_id) else {
                continue;
            };
            if !resolved.iter().any(|(existing, _)| existing == &model_id) {
                resolved.push((model_id, model_entry.path.clone()));
            }
        }
        resolved
    }

    /// Resolves the optional original PMX selected for a motion source.
    ///
    /// A missing, malformed, or dangling ID resolves to `None`; manifest and
    /// scene validation report those authoring errors separately, while the
    /// background queue remains robust against a concurrently edited asset.
    fn original_motion_model(
        &self,
        motion_id: &engine_authoring::AssetId,
    ) -> Option<(engine_authoring::AssetId, String)> {
        let original = self
            .asset_manifest
            .get(motion_id)?
            .import_settings
            .motion_original_model_source
            .as_deref()?;
        let model_id = engine_authoring::AssetId::from_stable_id(
            engine_authoring::StableId::new(original),
        )
        .ok()?;
        let entry = self.asset_manifest.get(&model_id)?;
        Some((model_id, entry.path.clone()))
    }

    /// Queues every registered or unregistered model in the project.
    ///
    /// The watcher only reports changes after its first snapshot, so opening
    /// a project would otherwise leave models that were added while the
    /// editor was closed without an import.
    pub(super) fn import_models_missing_catalogs(&mut self) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let assets_root = project.assets_root();
        let already_imported = |entry: &engine::ManifestEntry| {
            // A motion source draws nothing, so it never generates a
            // placement prefab; requiring one would requeue every `.vmd` on
            // every project open.
            let needs_prefab =
                !engine::asset_path_matches_kind(engine::AssetKind::MotionSource, Path::new(&entry.path));
            entry.import_settings.source_fingerprint.is_some()
                && (!needs_prefab
                    || entry
                        .import_settings
                        .generated_prefab
                        .as_ref()
                        .is_some_and(|relative| project.path().join(relative).is_file()))
        };
        let pending: Vec<(engine_authoring::AssetId, PathBuf)> = self
            .asset_manifest
            .iter()
            .filter(|(_, entry)| {
                is_importable_source_path(Path::new(&entry.path))
                    && assets_root.join(&entry.path).is_file()
                    && !already_imported(entry)
            })
            .map(|(id, entry)| (id.clone(), assets_root.join(&entry.path)))
            .collect();
        for (asset_id, source_path) in pending {
            self.queue_model_import(asset_id, source_path);
        }

        let unregistered: Vec<PathBuf> = self
            .asset_browser
            .entries()
            .iter()
            .filter(|entry| is_importable_source_path(&entry.relative_path))
            .map(|entry| entry.relative_path.clone())
            .filter(|relative| {
                asset_relative_path_string(relative).is_some_and(|relative| {
                    !self.asset_manifest.iter().any(|(_, entry)| {
                        normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative)
                    })
                })
            })
            .collect();
        for relative in unregistered {
            self.auto_import_model_source(&relative);
        }
    }

    pub(super) fn register_asset_from_browser(&mut self, index: usize) {
        let Some(project_root) = self.project_root.clone() else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.register_without_project",
                    "open a project before registering assets",
                ));
            return;
        };
        let Some(entry) = self.asset_browser.entries().get(index).cloned() else {
            return;
        };
        if !is_registerable_asset(&entry.relative_path) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.unsupported_registration",
                    "this file type is not a supported runtime asset",
                ));
            return;
        }
        let motion_source =
            engine::asset_path_matches_kind(engine::AssetKind::MotionSource, &entry.relative_path);
        if motion_source {
            let motion_path = project_root.assets_root().join(&entry.relative_path);
            match engine::classify_vmd_path(&motion_path) {
                Ok(engine::VmdContentKind::Scene) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::warning(
                        "vmd.scene_motion_unsupported",
                        "this VMD contains scene-level camera, light, or self-shadow motion and cannot be registered as a model MotionSource",
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Empty) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.empty_motion",
                        "this VMD contains no animation keys",
                    ));
                    return;
                }
                Ok(engine::VmdContentKind::Model | engine::VmdContentKind::Mixed) => {}
                Err(error) => {
                    self.session.push_diagnostic(engine_authoring::Diagnostic::error(
                        "vmd.motion_invalid",
                        format!("could not inspect VMD: {error}"),
                    ));
                    return;
                }
            }
        }
        // Both kinds queue a background catalog job after registration; only
        // the importer they dispatch to differs.
        let import_source = motion_source || is_importable_source_path(&entry.relative_path);
        let Some(relative_path) = asset_relative_path_string(&entry.relative_path) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.register_failed",
                    "asset path contains non-UTF-8 characters",
                ));
            return;
        };
        if let Err(error) = project_root.resolve_asset(&relative_path) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.register_failed",
                    format!("asset path `{relative_path}` is not readable: {error}"),
                ));
            return;
        }

        if let Some((id, _)) = self.asset_manifest.iter().find(|(_, manifest_entry)| {
            normalize_manifest_path(&manifest_entry.path) == normalize_manifest_path(&relative_path)
        }) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.already_registered",
                    format!(
                        "asset `{relative_path}` is already registered as `{}`",
                        id.as_str()
                    ),
                ));
            return;
        }

        let asset_id = engine_authoring::id::AssetId::generate();
        let name = unique_asset_name(&entry.display_name, &self.asset_manifest);
        let mut import_settings = engine::ImportSettings::default();
        if motion_source {
            // See `auto_import_model_source` for why an ambiguous project is
            // left unpaired rather than guessed at.
            import_settings.motion_model_sources = sole_pmx_model_source(&self.asset_manifest)
                .map(|id| vec![id.as_str().to_owned()])
                .unwrap_or_default();
        }
        let mut manifest = self.asset_manifest.clone();
        manifest.insert(
            asset_id.clone(),
            engine::ManifestEntry {
                path: relative_path.clone(),
                name: Some(name.clone()),
                import_settings,
            },
        );
        if let Err(error) = save_asset_manifest(&project_root, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error.clone(),
                ));
            self.notify_asset_error(format!("Asset registration failed: {error}"));
            return;
        }

        self.asset_manifest = manifest;
        self.notify_registered_assets(std::slice::from_ref(&entry.relative_path));
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.registered",
                format!(
                    "registered `{relative_path}` as `{}` ({name}){}",
                    asset_id.as_str(),
                    if import_source {
                        "; background import queued"
                    } else {
                        ""
                    }
                ),
            ));
        if import_source {
            let source_path = project_root.assets_root().join(&relative_path);
            self.queue_model_import(asset_id, source_path);
        }
    }

    pub(super) fn import_external_asset_files(&mut self, sources: Vec<PathBuf>) {
        let Some(project) = self.project_root.clone() else {
            self.notify_asset_error("Asset import failed: no project is open");
            return;
        };
        let destination_folder = self.asset_browser.selected_folder().to_path_buf();
        match crate::asset_management::import_external_asset_files(
            &project,
            &mut self.asset_manifest,
            &sources,
            &destination_folder,
        ) {
            Ok(report) => {
                if !report.registered.is_empty() {
                    let destinations = report
                        .registered
                        .iter()
                        .map(|imported| imported.destination.clone())
                        .collect::<Vec<_>>();
                    if let Some(watcher) = &mut self.file_watcher {
                        watcher.suppress_once(PathBuf::from("asset_manifest.json"));
                        for destination in &destinations {
                            watcher.suppress_once(PathBuf::from("assets").join(destination));
                        }
                    }
                    self.asset_browser.refresh(&project.assets_root());
                    self.asset_thumbnails
                        .retain(|path, _| project.assets_root().join(path).is_file());
                    self.notify_registered_assets(&destinations);
                    // Dropping a model used to register it without ever
                    // cataloging it, leaving a file that resolved to nothing
                    // (ADR 0075).
                    for imported in &report.registered {
                        let source_path = project.assets_root().join(&imported.destination);
                        if engine::asset_path_matches_kind(
                            engine::AssetKind::GltfSource,
                            &imported.destination,
                        ) {
                            self.queue_model_import(imported.asset_id.clone(), source_path);
                        }
                    }
                    self.session
                        .push_diagnostic(engine_authoring::Diagnostic::info(
                            "asset.external_import_succeeded",
                            format!(
                                "registered {} externally dropped asset(s) in `{}`",
                                destinations.len(),
                                if destination_folder.as_os_str().is_empty() {
                                    "assets".to_owned()
                                } else {
                                    destination_folder.display().to_string()
                                }
                            ),
                        ));
                }

                if !report.failures.is_empty() {
                    let details = report
                        .failures
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    for failure in &report.failures {
                        let code = match failure.kind {
                            crate::asset_management::ExternalAssetImportFailureKind::UnsupportedFormat => {
                                "asset.external_unsupported_format"
                            }
                            crate::asset_management::ExternalAssetImportFailureKind::CopyFailed(_) => {
                                "asset.external_copy_failed"
                            }
                            crate::asset_management::ExternalAssetImportFailureKind::InvalidFileName => {
                                "asset.external_invalid_filename"
                            }
                        };
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                code,
                                failure.to_string(),
                            ));
                    }
                    self.notify_asset_error(format!(
                        "Failed to import {} file(s): {}",
                        details.len(),
                        details.join("; ")
                    ));
                }
                self.refresh_scene_problems();
            }
            Err(error) => {
                let message = format!("Asset import failed: {error}");
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "asset.external_import_failed",
                        message.clone(),
                    ));
                self.notify_asset_error(message);
                self.asset_browser.refresh(&project.assets_root());
                self.refresh_scene_problems();
            }
        }
    }

    pub(super) fn reimport_asset_from_browser(&mut self, index: usize) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let Some(browser_entry) = self.asset_browser.entries().get(index) else {
            return;
        };
        let Some(relative_path) = asset_relative_path_string(&browser_entry.relative_path) else {
            return;
        };
        let Some(source_id) = self
            .asset_manifest
            .iter()
            .find(|(_, entry)| {
                normalize_manifest_path(&entry.path) == normalize_manifest_path(&relative_path)
            })
            .map(|(id, _)| id.clone())
        else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.reimport_unregistered",
                    format!("register `{relative_path}` before reimporting it"),
                ));
            return;
        };
        let source_path = project.assets_root().join(&relative_path);
        self.queue_model_import(source_id, source_path);
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.reimport_started",
                format!("reimporting `{relative_path}` in the background"),
            ));
    }

    pub(super) fn handle_asset_import_result(&mut self, result: AssetImportResult) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        if project.path() != result.project_path {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.import_project_changed",
                    "discarded an import result because a different project is open",
                ));
            return;
        }
        self.asset_import_problems.retain(|diagnostic| {
            !matches!(
                diagnostic.target.as_ref(),
                Some(engine_authoring::DiagnosticTarget::Asset { id }) if id == &result.source_id
            )
        });
        if result.cancelled {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "asset.import_cancelled",
                    format!("cancelled import of `{}`", result.source_path.display()),
                ));
            return;
        }
        if let Some(error) = result.error {
            let diagnostic = engine_authoring::Diagnostic::error(
                "asset.gltf_import_failed",
                format!(
                    "failed to import `{}`: {error}",
                    result.source_path.display()
                ),
            )
            .with_target(engine_authoring::DiagnosticTarget::Asset {
                id: result.source_id,
            });
            self.asset_import_problems.push(diagnostic.clone());
            self.session.push_diagnostic(diagnostic);
            self.refresh_scene_problems();
            return;
        }

        let assets_root = project.assets_root();
        let dependencies = result
            .source_dependencies
            .iter()
            .map(|dependency| {
                dependency
                    .strip_prefix(&assets_root)
                    .map_err(|_| {
                        format!(
                            "dependency `{}` escapes the project assets root",
                            dependency.display()
                        )
                    })
                    .and_then(|relative| {
                        asset_relative_path_string(relative).ok_or_else(|| {
                            format!("dependency `{}` has an invalid path", dependency.display())
                        })
                    })
            })
            .collect::<Result<Vec<_>, _>>();
        let dependencies = match dependencies {
            Ok(dependencies) => dependencies,
            Err(error) => {
                let diagnostic =
                    engine_authoring::Diagnostic::error("asset.import_dependency_invalid", error)
                        .with_target(engine_authoring::DiagnosticTarget::Asset {
                            id: result.source_id,
                        });
                self.asset_import_problems.push(diagnostic.clone());
                self.session.push_diagnostic(diagnostic);
                self.refresh_scene_problems();
                return;
            }
        };

        let mut manifest = self.asset_manifest.clone();
        let Some(entry) = manifest.get_mut(&result.source_id) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "asset.import_source_removed",
                    "discarded an import result because its source is no longer registered",
                ));
            return;
        };
        // Captured before the overwrite so the skeleton bind report (ADR
        // 0077 §6, AP-5) can compare the previous and current bone ledgers.
        let previous_skeleton_records = entry.import_settings.skeleton_records.clone();
        entry.import_settings.source_fingerprint = result.source_fingerprint;
        entry.import_settings.source_stamp = result.source_stamp;
        entry.import_settings.source_dependencies = dependencies;
        entry.import_settings.sub_assets = result.sub_assets;
        entry.import_settings.skeleton_records = result.skeleton_records.clone();
        entry.import_settings.humanoid_profiles = result.humanoid_profiles.clone();
        let sub_asset_count = entry.import_settings.sub_assets.len();

        let prefab_path = match result.prefab {
            Some(prefab) => match write_generated_prefab(&project, &result.source_id, &prefab) {
                Ok(path) => Some(path),
                Err(error) => {
                    let diagnostic = engine_authoring::Diagnostic::error(
                        "asset.generated_prefab_failed",
                        format!(
                            "imported `{}` but could not write its placement prefab: {error}",
                            result.source_path.display()
                        ),
                    )
                    .with_target(engine_authoring::DiagnosticTarget::Asset {
                        id: result.source_id.clone(),
                    });
                    self.asset_import_problems.push(diagnostic.clone());
                    self.session.push_diagnostic(diagnostic);
                    None
                }
            },
            None => None,
        };
        let Some(entry) = manifest.get_mut(&result.source_id) else {
            return;
        };
        entry.import_settings.generated_prefab = prefab_path;
        if let Err(error) = save_asset_manifest(&project, &manifest) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "asset.manifest_save_failed",
                    error,
                ));
            return;
        }
        self.asset_manifest = manifest;

        // AP-5: rebuild this source's bind report and contact interval
        // summary from the fresh import result, in memory only.
        let source_key = result.source_id.as_str().to_owned();
        let bind_reports: Vec<crate::anim_ux::SkeletonBindReport> = result
            .skeleton_records
            .iter()
            .map(|record| {
                let previous_bones = previous_skeleton_records
                    .iter()
                    .find(|previous| previous.id == record.id)
                    .map_or(&[][..], |previous| previous.bones.as_slice());
                crate::anim_ux::build_skeleton_bind_report(
                    &record.id,
                    previous_bones,
                    &record.bones,
                )
            })
            .collect();
        if bind_reports.is_empty() {
            self.skeleton_rebind_reports.remove(&source_key);
        } else {
            self.skeleton_rebind_reports
                .insert(source_key.clone(), bind_reports);
        }
        if result.animation_contacts.is_empty() {
            self.clip_contact_summaries.remove(&source_key);
        } else {
            self.clip_contact_summaries
                .insert(source_key, result.animation_contacts);
        }

        for diagnostic in result.diagnostics {
            let diagnostic = diagnostic.with_target(engine_authoring::DiagnosticTarget::Asset {
                id: result.source_id.clone(),
            });
            self.asset_import_problems.push(diagnostic.clone());
            self.session.push_diagnostic(diagnostic);
        }
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::info(
                "asset.import_succeeded",
                format!(
                    "imported `{}` with {sub_asset_count} stable sub-assets",
                    result.source_path.display()
                ),
            ));
        self.requeue_motions_paired_with(&result.source_id);
        self.refresh_scene_problems();
    }

    /// Reimports every `.vmd` motion baked against `model_source_id`
    /// (ADR 0097 §3).
    ///
    /// A motion's clip is a function of the PMX too — its IK chains and
    /// appended-parent links shape every baked curve — so a reimported model
    /// leaves each paired motion's catalog stale. Requeuing here is what makes
    /// "edit the model, motions follow" hold without the author reimporting
    /// each motion by hand.
    fn requeue_motions_paired_with(&mut self, model_source_id: &engine_authoring::AssetId) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let paired_key = model_source_id.as_str();
        let paired: Vec<(engine_authoring::AssetId, PathBuf)> = self
            .asset_manifest
            .iter()
            .filter(|(id, entry)| {
                id != &model_source_id
                    && entry
                        .import_settings
                        .resolved_motion_model_sources()
                        .contains(&paired_key)
            })
            .map(|(id, entry)| (id.clone(), project.assets_root().join(&entry.path)))
            .collect();
        for (motion_id, motion_path) in paired {
            self.queue_model_import(motion_id, motion_path);
        }
    }
}

/// Sub-assets of the selected glTF source shown as draggable rows.
///
/// Imported meshes, materials, and textures were previously invisible until
/// a picker was opened; listing them here also provides the drag source for
/// Inspector and Scene View drops. Clips are listed for discoverability —
/// the Animator's clip source takes the glTF file itself.
///
/// This is a drag source only — searching, filtering, selecting, and editing
/// all belong on the Inspector's own sub-asset list, which lives in the same
/// panel as the detail/edit fields it drives.
fn show_selected_gltf_sub_assets(
    ui: &mut egui::Ui,
    browser: &AssetBrowser,
    manifest: &engine::AssetManifest,
) {
    let mut selected = browser.selected_paths();
    let (Some(path), None) = (selected.next().cloned(), selected.next()) else {
        return;
    };
    let relative = path.to_string_lossy().replace('\\', "/");
    let Some((source_id, entry)) = manifest.iter().find(|(_, entry)| entry.path == relative)
    else {
        return;
    };
    if entry.import_settings.sub_assets.is_empty() {
        return;
    }
    ui.separator();
    ui.strong(format!("Sub-assets of {relative}"));
    for sub_asset in &entry.import_settings.sub_assets {
        if is_legacy_motion_clip_alias(source_id, sub_asset) {
            continue;
        }
        let (badge, kind) = match sub_asset.kind {
            engine::ImportedSubAssetKind::Mesh => ("[mesh]", Some(AssetKind::Mesh)),
            engine::ImportedSubAssetKind::Material => ("[mat]", Some(AssetKind::Material)),
            engine::ImportedSubAssetKind::Texture => ("[tex]", Some(AssetKind::Texture)),
            engine::ImportedSubAssetKind::Animation => ("[clip]", Some(AssetKind::AnimationClip)),
            _ => ("[sub]", None),
        };
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let target_label = sub_asset
                .target_model_source
                .as_deref()
                .and_then(|target| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
                })
                .and_then(|target| manifest.get(&target))
                .map(|entry| entry.name.as_deref().unwrap_or(&entry.path));
            let display_name = target_label.map_or_else(
                || sub_asset.name.clone(),
                |target| format!("{} — {target}", sub_asset.name),
            );
            let override_label = sub_asset_override_target(entry, sub_asset).map(|target| {
                AssetId::from_stable_id(engine_authoring::StableId::new(target))
                    .ok()
                    .and_then(|target| override_target_label(&target, manifest))
                    .unwrap_or_else(|| format!("missing: {target}"))
            });
            let display_name = override_label.map_or(display_name.clone(), |target| {
                format!("{display_name} [overridden -> {target}]")
            });
            let response = ui.add(
                egui::Label::new(format!("{badge} {display_name}"))
                    .sense(egui::Sense::click_and_drag()),
            );
            if let Some(kind) = kind {
                let stable = engine_authoring::StableId::new(&sub_asset.id);
                if let Ok(asset_id) = AssetId::from_stable_id(stable) {
                    response
                        .on_hover_text("Drag onto the Scene View or an Inspector field")
                        .dnd_set_drag_payload(DragPayload {
                            asset_id,
                            relative_path: path.clone(),
                            kind,
                            // A sub-asset has no file of its own, so it can be
                            // referenced but never relocated.
                            paths: Vec::new(),
                        });
                }
            }
        });
    }
}

/// First free `name`, `name_2`, ... path with the given multi-part suffix.
pub(super) fn unique_document_path(directory: &Path, stem: &str, suffix: &str) -> PathBuf {
    (1_u32..)
        .map(|counter| {
            let name = if counter == 1 {
                format!("{stem}{suffix}")
            } else {
                format!("{stem}_{counter}{suffix}")
            };
            directory.join(name)
        })
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| directory.join(format!("{stem}_new{suffix}")))
}

pub(super) enum AssetBrowserAction {
    Open(usize),
    Register(usize),
    Reimport(usize),
    InstantiatePrefab(usize),
    InstantiateModel(usize),
    CreatePrefabFromEntity {
        entity: EntityId,
        destination_folder: PathBuf,
    },
    RenameAsset(usize),
    MoveAsset(usize),
    TrashAsset(usize),
    NewUiDocument,
    NewScene,
    NewAnimationGraph {
        destination_folder: PathBuf,
    },
    NewAnimationSet {
        graph: Option<AssetId>,
        destination_folder: PathBuf,
    },
    NewMaterial,
    NewRhaiScript,
    NewRustScript,
    NewFolder,
    ShowInExplorer(PathBuf),
    AddMeshToScene(usize),
    RenameFolder(PathBuf),
    TrashFolder(PathBuf),
    MoveSelectionToFolder(PathBuf),
    /// Opens the Import Settings window for a registered glTF/GLB source
    /// (contact-bones override editing + contact interval display, AP-5).
    EditImportSettings(usize),
    /// Creates a `*.retarget.json` map for `source` (a glTF/GLB row) onto
    /// `target_source_id`'s skeleton (AP-5 creation flow for
    /// `anim.retarget_map_missing`).
    CreateRetargetMap {
        source: usize,
        target_source_id: AssetId,
    },
}

#[derive(Clone, Copy)]
pub(super) enum AssetMutationKind {
    Rename,
    Move,
    Trash,
    CreateFolder,
    RenameFolder,
    TrashFolder,
}

pub(super) struct PendingAssetMutation {
    source: PathBuf,
    destination: String,
    kind: AssetMutationKind,
}

#[derive(Clone)]
struct AssetPathDragPayload {
    paths: Vec<PathBuf>,
}

/// Reports whether a file drag from the Asset Browser was released here.
///
/// A registered asset carries its paths inside [`DragPayload`] so the Scene
/// View and folder targets can both read one payload (egui keeps only one);
/// unregistered files use [`AssetPathDragPayload`]. Folder targets accept
/// either.
fn dropped_asset_paths(response: &egui::Response) -> bool {
    if let Some(payload) = response.dnd_release_payload::<DragPayload>() {
        return !payload.paths.is_empty();
    }
    response
        .dnd_release_payload::<AssetPathDragPayload>()
        .is_some_and(|payload| !payload.paths.is_empty())
}

/// Selects which project content occupies the Assets utility dock.
///
/// Runtime assets and Rust game code use separate full-height views so the
/// asset folder tree and asset grid never need an enclosing shared scroll area.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectBrowserTab {
    Assets,
}

pub(super) struct TexturePreview {
    pub(super) relative_path: PathBuf,
    pub(super) dimensions: [usize; 2],
    pub(super) texture: egui::TextureHandle,
}

/// Open document state for the RetargetMap inspector window (AP-5).
pub(super) struct RetargetMapEditorState {
    /// Asset-relative path of the open `*.retarget.json` file.
    pub(super) relative_path: PathBuf,
    /// Currently edited map. Every "Re-run name matching" click writes
    /// straight back to disk (mirroring the Material Editor's
    /// edit-then-persist flow), so this is never a separate unsaved buffer.
    pub(super) map: engine::RetargetMap,
}

/// Open state for the multi-skin retarget-map creation picker (AP-6 scope
/// (b)), shown when either side of a (source, target) pair records more than
/// one skeleton. Pure UI state: canceling or closing the window drops it
/// without writing anything.
pub(super) struct RetargetMapCreationPickerState {
    /// Asset-relative path of the source file (used to derive the created
    /// map's file name and stem, same as the one-click path).
    pub(super) source_relative_path: PathBuf,
    /// Manifest [`AssetId`] of the target source.
    pub(super) target_source_id: AssetId,
    /// Every skeleton recorded on the source side.
    pub(super) source_records: Vec<engine::SkeletonRecord>,
    /// Every skeleton recorded on the target side.
    pub(super) target_records: Vec<engine::SkeletonRecord>,
    /// Index into `source_records` currently selected in the picker.
    pub(super) selected_source: usize,
    /// Index into `target_records` currently selected in the picker.
    pub(super) selected_target: usize,
}

/// Open document state for a glTF/GLB source's Import Settings window
/// (contact-bones override editing + contact interval display, AP-5).
pub(super) struct ImportSettingsEditorState {
    /// The source's stable manifest [`AssetId`].
    pub(super) source_id: AssetId,
    /// Asset-relative path of the source file, kept for the window title.
    pub(super) relative_path: PathBuf,
    /// Editable copy of `ImportSettings::contact_bones`; written back to the
    /// manifest and reimported when the user clicks Save.
    pub(super) contact_bones: Vec<String>,
    /// For a `*.vmd` motion source: the pairing UI's state (ADR 0097 §3).
    /// `None` for every other source kind, which hides the picker entirely
    /// rather than showing a control that cannot apply.
    pub(super) motion_pairing: Option<MotionPairingState>,
}

/// The paired-model picker shown for a `*.vmd` motion source.
pub(super) struct MotionPairingState {
    /// Absolute VMD path used only when the author requests a compatibility
    /// check. The check never modifies or reimports this file.
    pub(super) motion_path: Option<PathBuf>,
    /// Optional PMX whose MMD IK and appended-parent rig is evaluated once.
    /// `None` is the ordinary direct-bake path, not an invalid state.
    pub(super) original: Option<AssetId>,
    /// Editable PMX target list. Each selected model produces its own stable
    /// Animation Clip sub-asset.
    pub(super) selected: Vec<AssetId>,
    /// Every registered `.pmx` in the project as `(id, display label)`, in
    /// stable manifest order so the list does not reshuffle between frames.
    pub(super) candidates: Vec<(AssetId, String)>,
    /// Absolute PMX paths parallel to `candidates`, retained because the
    /// compatibility button parses current source bytes on demand.
    pub(super) candidate_paths: Vec<(AssetId, PathBuf)>,
    /// Model-source pairs backed by a currently registered Retarget Map.
    /// Stored as source/target model IDs so the UI can report readiness
    /// without exposing internal skeleton sub-asset IDs.
    pub(super) retarget_pairs: Vec<(AssetId, AssetId)>,
    /// Presentation-only model name stored in the VMD header. It helps the
    /// author choose an original PMX but is never used for automatic pairing.
    pub(super) recorded_model_name: Option<String>,
    /// Transient reports from the latest explicit check. They are cleared
    /// whenever the original/output selection changes and are never saved.
    pub(super) compatibility_reports: Vec<MotionCompatibilityDisplay>,
}

/// One PMX result shown in the VMD Import Settings compatibility section.
pub(super) struct MotionCompatibilityDisplay {
    pub(super) model_source: AssetId,
    pub(super) result: Result<engine::VmdPmxCompatibilityReport, String>,
}

/// Width of one asset grid cell.
pub(super) const ASSET_GRID_CELL_WIDTH: f32 = 112.0;

/// Height of one asset grid cell.
///
/// The bottom dock's minimum height is derived from this so a resize cannot
/// be dragged below what a single row of assets needs.
pub(super) const ASSET_GRID_CELL_HEIGHT: f32 = 108.0;

fn asset_manifest_path(project_root: &ProjectRoot) -> PathBuf {
    project_root.path().join("asset_manifest.json")
}

/// Project-relative location of the prefab generated for one model source.
///
/// Keeping artifacts under `.engine/` puts them outside the Asset Browser,
/// which scans only `assets/`, so one model stays one row in the browser
/// (ADR 0075). Naming the file after the source asset ID means a reimport
/// overwrites the same artifact instead of accumulating one per import, and
/// survives renaming or moving the source file.
pub(super) fn generated_prefab_relative_path(source_id: &engine_authoring::AssetId) -> PathBuf {
    Path::new(".engine")
        .join("imported")
        .join(format!("{}.prefab.json", source_id.as_str()))
}

/// Writes the prefab regenerated from one glTF/GLB source and returns its
/// project-relative path (ADR 0075).
///
/// The file is import output, not authoring data: it is rewritten on every
/// successful import and is deliberately absent from the asset manifest.
/// Authors keep their own copies by instantiating it and saving a separate
/// prefab under `assets/`.
fn write_generated_prefab(
    project_root: &ProjectRoot,
    source_id: &engine_authoring::AssetId,
    prefab: &engine_authoring::prefab::PrefabAsset,
) -> Result<String, String> {
    let relative = generated_prefab_relative_path(source_id);
    let relative_string = relative
        .to_str()
        .ok_or_else(|| "generated prefab path is not representable".to_owned())?
        .to_owned();
    let json = prefab
        .to_json()
        .map_err(|error| format!("failed to serialize prefab: {error}"))?;
    let full_path = project_root.path().join(&relative);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    engine_authoring::persist::replace_file_contents(&full_path, &json)
        .map_err(|error| format!("failed to write {}: {error}", full_path.display()))?;
    Ok(relative_string)
}

fn save_asset_manifest(
    project_root: &ProjectRoot,
    manifest: &engine::AssetManifest,
) -> Result<(), String> {
    let json = manifest
        .to_canonical_json()
        .map_err(|error| format!("failed to serialize asset manifest: {error}"))?;
    let path = asset_manifest_path(project_root);
    replace_file_contents(&path, &json)
        .map_err(|error| format!("failed to save {}: {error}", path.display()))
}

pub(super) fn load_asset_manifest(
    project_root: &ProjectRoot,
) -> (engine::AssetManifest, Option<engine_authoring::Diagnostic>) {
    let path = asset_manifest_path(project_root);
    let json = match fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (engine::AssetManifest::default(), None);
        }
        Err(error) => {
            return (
                engine::AssetManifest::default(),
                Some(engine_authoring::Diagnostic::error(
                    "editor.asset_manifest_load_failed",
                    format!("failed to read {}: {error}", path.display()),
                )),
            );
        }
    };

    match engine::AssetManifest::from_json(&json) {
        Ok(manifest) => (manifest, None),
        Err(error) => (
            engine::AssetManifest::default(),
            Some(engine_authoring::Diagnostic::error(
                "editor.asset_manifest_load_failed",
                format!("failed to parse {}: {error}", path.display()),
            )),
        ),
    }
}

/// Renders the one physical project asset tree, including user-authored code.
#[allow(clippy::too_many_arguments)]
pub(super) fn show_project_browser(
    ui: &mut egui::Ui,
    assets: &mut AssetBrowser,
    asset_search: &mut String,
    asset_thumbnails: &mut std::collections::BTreeMap<PathBuf, TexturePreview>,
    content_scroll_reset: &mut bool,
    active_tab: &mut ProjectBrowserTab,
    project: Option<&ProjectRoot>,
    manifest: &engine::AssetManifest,
    can_create_rust_script: bool,
) -> Option<AssetBrowserAction> {
    let Some(project) = project else {
        ui.label("No project open");
        return None;
    };

    *active_tab = ProjectBrowserTab::Assets;
    show_asset_browser(
        ui,
        assets,
        asset_search,
        asset_thumbnails,
        content_scroll_reset,
        Some(project.assets_root().as_path()),
        manifest,
        can_create_rust_script,
    )
}

/// Returns whether `folder` is a direct child of `parent` in the physical
/// asset-folder hierarchy.
pub(super) fn is_direct_asset_folder_child(folder: &Path, parent: &Path) -> bool {
    !folder.as_os_str().is_empty() && folder.parent().unwrap_or(Path::new("")) == parent
}

/// Returns whether a folder owns at least one direct child folder and should
/// therefore display an expand/collapse affordance.
pub(super) fn asset_folder_has_children(folder: &Path, folders: &[crate::AssetFolder]) -> bool {
    folders
        .iter()
        .any(|candidate| is_direct_asset_folder_child(&candidate.relative_path, folder))
}

/// Produces the full user-facing path of a folder for hover text.
fn asset_folder_hover_path(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        "Assets".to_owned()
    } else {
        format!("Assets/{}", folder.display())
    }
}

/// Produces the user-facing final path component for a physical asset folder.
fn asset_folder_label(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        "Assets".to_owned()
    } else {
        folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

/// Draws a geometrically centered disclosure triangle inside a fixed-size
/// button. Painting the triangle directly avoids font-specific baseline and
/// glyph-bounds differences that made the previous `v` / `>` text sit lower
/// than the adjacent folder label.
fn asset_folder_toggle_button(ui: &mut egui::Ui, collapsed: bool) -> egui::Response {
    let response = ui.add_sized(egui::vec2(20.0, 20.0), egui::Button::new(""));
    let center = response.rect.center();
    let points = if collapsed {
        vec![
            center + egui::vec2(-2.5, -4.0),
            center + egui::vec2(-2.5, 4.0),
            center + egui::vec2(3.5, 0.0),
        ]
    } else {
        vec![
            center + egui::vec2(-4.0, -2.5),
            center + egui::vec2(4.0, -2.5),
            center + egui::vec2(0.0, 3.5),
        ]
    };
    let color = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response
}

/// Renders the runtime-asset portion of the project browser.
///
/// Returns the index of the entry that was double-clicked, if any.  The
/// caller is responsible for resolving the path and opening the document.
///
/// When `assets_root` is `None` (no project open), a placeholder message is
/// shown.  Selection state is stored in `browser`.
#[allow(clippy::too_many_arguments)]
fn show_asset_browser(
    ui: &mut egui::Ui,
    browser: &mut AssetBrowser,
    asset_search: &mut String,
    asset_thumbnails: &mut std::collections::BTreeMap<PathBuf, TexturePreview>,
    content_scroll_reset: &mut bool,
    assets_root: Option<&std::path::Path>,
    manifest: &engine::AssetManifest,
    can_create_rust_script: bool,
) -> Option<AssetBrowserAction> {
    match assets_root {
        None => {
            ui.label("No project open");
            None
        }
        Some(root) => {
            let mut action = None;
            let mut refresh_requested = false;
            let folders = browser.folders().to_vec();
            let reveal_folder = browser.take_pending_reveal();
            control_row(ui, |ui| {
                ui.strong("Assets");
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(asset_search)
                        .hint_text("Search assets...")
                        .desired_width(220.0),
                );
                if ui.small_button("Clear").clicked() {
                    asset_search.clear();
                }
            });
            ui.separator();

            // The browser is deliberately split into a fixed-width tree and a
            // flexible content area. This keeps folder navigation visible
            // while the right side changes to show only the selected folder.
            let available_width = ui.available_width();
            // No enclosing vertical ScrollArea exists here. Both children
            // receive the same finite height and maintain independent offsets.
            let browser_height = ui.available_height().max(1.0);
            let tree_width = (available_width * 0.24).clamp(180.0, 280.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(tree_width, browser_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.heading("Folders");
                        let tree_viewport = ui.available_rect_before_wrap();
                        egui::ScrollArea::vertical()
                            .id_salt("asset_folder_tree_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                // Same rule as the content pane: the tree
                                // background answers everywhere no folder row
                                // does, instead of only on a trailing strip.
                                let tree_background = ui.interact(
                                    tree_viewport,
                                    ui.id().with("asset_folder_tree_background"),
                                    egui::Sense::click(),
                                );
                                for folder in &folders {
                                    if !browser.folder_row_is_visible(&folder.relative_path) {
                                        continue;
                                    }
                                    let label = asset_folder_label(&folder.relative_path);
                                    let selected =
                                        browser.selected_folder() == folder.relative_path;
                                    let has_children = asset_folder_has_children(
                                        &folder.relative_path,
                                        &folders,
                                    );
                                    let collapsed =
                                        browser.is_folder_collapsed(&folder.relative_path);
                                    let (toggle_clicked, response) = ui
                                        .allocate_ui_with_layout(
                                            egui::vec2(ui.available_width(), 24.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                            ui.add_space(folder.depth as f32 * 14.0);
                                            let toggle_clicked = if has_children {
                                                asset_folder_toggle_button(ui, collapsed)
                                                    .on_hover_text(if collapsed {
                                                        "Expand folder"
                                                    } else {
                                                        "Collapse folder"
                                                    })
                                                    .clicked()
                                            } else {
                                                ui.add_sized(
                                                    egui::vec2(20.0, 20.0),
                                                    egui::Label::new(""),
                                                );
                                                false
                                            };
                                            let response = ui.selectable_label(selected, label);
                                            (toggle_clicked, response)
                                        },
                                        )
                                        .inner;
                                    if toggle_clicked {
                                        browser.toggle_folder_collapsed(&folder.relative_path);
                                    }
                                    if reveal_folder.as_deref() == Some(&folder.relative_path) {
                                        response.scroll_to_me(None);
                                    }
                                    if response.clicked()
                                        && !selected
                                        && browser
                                            .set_selected_folder(folder.relative_path.clone())
                                    {
                                        *content_scroll_reset = true;
                                    }
                                    if dropped_asset_paths(&response) {
                                        action = Some(AssetBrowserAction::MoveSelectionToFolder(
                                            folder.relative_path.clone(),
                                        ));
                                    }
                                    if let Some(payload) =
                                        response.dnd_release_payload::<HierarchyDragPayload>()
                                    {
                                        action = Some(AssetBrowserAction::CreatePrefabFromEntity {
                                            entity: payload.entity.clone(),
                                            destination_folder: folder.relative_path.clone(),
                                        });
                                    }
                                    response.context_menu(|ui| {
                                        if ui.button("Create Child Folder...").clicked() {
                                            browser
                                                .set_selected_folder(folder.relative_path.clone());
                                            *content_scroll_reset = true;
                                            action = Some(AssetBrowserAction::NewFolder);
                                            ui.close();
                                        }
                                        if ui.button("Create Animation Graph").clicked() {
                                            action = Some(AssetBrowserAction::NewAnimationGraph {
                                                destination_folder: folder.relative_path.clone(),
                                            });
                                            ui.close();
                                        }
                                        if !folder.relative_path.as_os_str().is_empty() {
                                            if ui.button("Rename Folder...").clicked() {
                                                action = Some(AssetBrowserAction::RenameFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Delete Folder...").clicked() {
                                                action = Some(AssetBrowserAction::TrashFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                                ui.add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Label::new(
                                        egui::RichText::new("Right-click to create a folder")
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    ),
                                );
                                tree_background.context_menu(|ui| {
                                    if ui.button("Create Folder...").clicked() {
                                        action = Some(AssetBrowserAction::NewFolder);
                                        ui.close();
                                    }
                                });
                            });
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(
                        (available_width - tree_width - 12.0).max(160.0),
                        browser_height,
                    ),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        control_row(ui, |ui| {
                            // Each step of the working path navigates to that
                            // level, so a nested folder can be left one
                            // component at a time without hunting for the row
                            // in the tree.
                            let breadcrumbs =
                                crate::asset_browser::folder_breadcrumbs(browser.selected_folder());
                            let last = breadcrumbs.len().saturating_sub(1);
                            for (index, breadcrumb) in breadcrumbs.iter().enumerate() {
                                if index > 0 {
                                    ui.label("/");
                                }
                                if index == last {
                                    ui.strong(&breadcrumb.label);
                                    continue;
                                }
                                if ui
                                    .link(&breadcrumb.label)
                                    .on_hover_text(format!(
                                        "Open {}",
                                        asset_folder_hover_path(&breadcrumb.folder)
                                    ))
                                    .clicked()
                                    && browser.set_selected_folder(breadcrumb.folder.clone())
                                {
                                    *content_scroll_reset = true;
                                }
                            }
                            ui.separator();
                            if ui.small_button("New Folder").clicked() {
                                action = Some(AssetBrowserAction::NewFolder);
                            }
                            if ui.small_button("Refresh").clicked() {
                                refresh_requested = true;
                            }
                            let selected_count = browser.selected_paths().count();
                            if let Some(primary) = browser.selected().filter(|_| selected_count > 0) {
                                ui.separator();
                                ui.label(format!("{selected_count} selected"));
                                let move_label = if selected_count > 1 {
                                    "Move Selected..."
                                } else {
                                    "Move..."
                                };
                                if ui.small_button(move_label).clicked() {
                                    action = Some(AssetBrowserAction::MoveAsset(primary));
                                }
                                let delete_label = if selected_count > 1 {
                                    "Delete Selected..."
                                } else {
                                    "Delete..."
                                };
                                if ui.small_button(delete_label).clicked() {
                                    action = Some(AssetBrowserAction::TrashAsset(primary));
                                }
                            }
                        });
                        ui.separator();

                        // Captured before the scroll area so the background
                        // covers the visible viewport rather than the scrolled
                        // content origin.
                        let content_viewport = ui.available_rect_before_wrap();
                        let mut content_scroll = egui::ScrollArea::vertical()
                            .id_salt("asset_content_scroll")
                            .auto_shrink([false, false]);
                        if std::mem::take(content_scroll_reset) {
                            content_scroll = content_scroll.vertical_scroll_offset(0.0);
                        }
                        content_scroll.show(ui, |ui| {
                            // The folder background owns the create menu and the
                            // Entity drop, so every gap between tiles, beside a
                            // short last row, and below the rows answers alike.
                            // Registering it before the rows keeps the tiles on
                            // top: egui resolves a click to the last widget that
                            // contains the pointer, so an asset still wins over
                            // the background wherever an asset actually is.
                            let background = ui.interact(
                                content_viewport,
                                ui.id().with("asset_content_background"),
                                egui::Sense::click(),
                            );
                            let search = asset_search.trim().to_ascii_lowercase();
                            let selected_folder = browser.selected_folder().to_path_buf();
                            let visible_folders = folders
                                .iter()
                                .filter(|folder| {
                                    is_direct_asset_folder_child(
                                        &folder.relative_path,
                                        &selected_folder,
                                    )
                                })
                                .filter(|folder| {
                                    let label = asset_folder_label(&folder.relative_path)
                                        .to_ascii_lowercase();
                                    search.is_empty()
                                        || label.contains(&search)
                                        || folder
                                            .relative_path
                                            .to_string_lossy()
                                            .to_ascii_lowercase()
                                            .contains(&search)
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            let visible_entries = browser
                                .visible_entry_indices()
                                .into_iter()
                                .filter_map(|index| {
                                    browser
                                        .entries()
                                        .get(index)
                                        .cloned()
                                        .map(|entry| (index, entry))
                                })
                                .filter(|(_, entry)| {
                                    search.is_empty()
                                        || entry.display_name.to_ascii_lowercase().contains(&search)
                                        || entry
                                            .relative_path
                                            .to_string_lossy()
                                            .to_ascii_lowercase()
                                            .contains(&search)
                                })
                                .collect::<Vec<_>>();

                            if visible_folders.is_empty() && visible_entries.is_empty() {
                                ui.label("(empty folder)");
                            }

                            // Compute model-only choices once so every row's
                            // context menu reuses the same filtered list.
                            let skeleton_source_choices =
                                retarget_map_model_source_choices(manifest);

                            let tile_width = ASSET_GRID_CELL_WIDTH;
                            let tile_height = ASSET_GRID_CELL_HEIGHT;
                            let columns =
                                ((ui.available_width() / tile_width).floor() as usize).max(1);
                            for row in visible_folders.chunks(columns) {
                                ui.horizontal(|ui| {
                                    for folder in row {
                                        let selected = browser.selected_folder_tile()
                                            == Some(folder.relative_path.as_path());
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_width, tile_height),
                                            egui::Sense::click_and_drag(),
                                        );
                                        let preview_rect = egui::Rect::from_center_size(
                                            egui::pos2(rect.center().x, rect.top() + 42.0),
                                            egui::vec2(66.0, 66.0),
                                        );
                                        ui.painter().rect_filled(
                                            preview_rect.shrink2(egui::vec2(5.0, 12.0)),
                                            5.0,
                                            egui::Color32::from_rgb(184, 142, 48),
                                        );
                                        ui.painter().text(
                                            preview_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "DIR",
                                            egui::FontId::proportional(20.0),
                                            egui::Color32::WHITE,
                                        );
                                        ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                                            egui::pos2(rect.center().x, rect.bottom() - 7.0),
                                            egui::Align2::CENTER_BOTTOM,
                                            asset_folder_label(&folder.relative_path),
                                            egui::FontId::proportional(11.0),
                                            egui::Color32::WHITE,
                                        );
                                        if selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                4.0,
                                                egui::Stroke::new(
                                                    1.5_f32,
                                                    egui::Color32::LIGHT_BLUE,
                                                ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }

                                        if response.clicked() {
                                            browser.select_folder_tile(&folder.relative_path);
                                        }
                                        if response.double_clicked()
                                            && browser.set_selected_folder(
                                                folder.relative_path.clone(),
                                            )
                                        {
                                            *content_scroll_reset = true;
                                        }
                                        if dropped_asset_paths(&response) {
                                            action = Some(
                                                AssetBrowserAction::MoveSelectionToFolder(
                                                    folder.relative_path.clone(),
                                                ),
                                            );
                                        }
                                        if let Some(payload) =
                                            response.dnd_release_payload::<HierarchyDragPayload>()
                                        {
                                            action = Some(
                                                AssetBrowserAction::CreatePrefabFromEntity {
                                                    entity: payload.entity.clone(),
                                                    destination_folder: folder
                                                        .relative_path
                                                        .clone(),
                                                },
                                            );
                                        }
                                        if response.secondary_clicked() {
                                            browser.select_folder_tile(&folder.relative_path);
                                        }
                                        response.context_menu(|ui| {
                                            if ui.button("Open Folder").clicked() {
                                                if browser.set_selected_folder(
                                                    folder.relative_path.clone(),
                                                ) {
                                                    *content_scroll_reset = true;
                                                }
                                                ui.close();
                                            }
                                            ui.menu_button("Create", |ui| {
                                                ui.menu_button("General", |ui| {
                                                    if ui.button("Child Folder...").clicked() {
                                                        browser.set_selected_folder(
                                                            folder.relative_path.clone(),
                                                        );
                                                        *content_scroll_reset = true;
                                                        action = Some(AssetBrowserAction::NewFolder);
                                                        ui.close();
                                                    }
                                                    if ui.button("Scene").clicked() {
                                                        action = Some(AssetBrowserAction::NewScene);
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Rendering", |ui| {
                                                    if ui.button("Material").clicked() {
                                                        action = Some(AssetBrowserAction::NewMaterial);
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Animation", |ui| {
                                                    if ui.button("Animation Graph").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationGraph {
                                                                destination_folder: folder
                                                                    .relative_path
                                                                    .clone(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                                    if ui.button("Animation Set").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationSet {
                                                                graph: None,
                                                                destination_folder: folder
                                                                    .relative_path
                                                                    .clone(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("UI", |ui| {
                                                    if ui.button("UI Document").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewUiDocument,
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Scripting", |ui| {
                                                    if ui.button("Rhai Script...").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewRhaiScript,
                                                        );
                                                        ui.close();
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            can_create_rust_script,
                                                            egui::Button::new("Rust Script..."),
                                                        )
                                                        .on_disabled_hover_text(
                                                            "Initialize the Rust Game first",
                                                        )
                                                        .clicked()
                                                    {
                                                        browser.set_selected_folder(
                                                            folder.relative_path.clone(),
                                                        );
                                                        *content_scroll_reset = true;
                                                        action = Some(
                                                            AssetBrowserAction::NewRustScript,
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                            });
                                            if ui.button("Rename Folder...").clicked() {
                                                action = Some(AssetBrowserAction::RenameFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Delete Folder...").clicked() {
                                                action = Some(AssetBrowserAction::TrashFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            }
                            for row in visible_entries.chunks(columns) {
                                ui.horizontal(|ui| {
                                    for (index, entry) in row {
                                        let selected = browser
                                            .selected_paths()
                                            .any(|path| path == &entry.relative_path);
                                        let registered_asset =
                                            manifest.iter().find(|(_, manifest_entry)| {
                                                normalize_manifest_path(&manifest_entry.path)
                                                    == normalize_manifest_path(
                                                        &entry.relative_path.to_string_lossy(),
                                                    )
                                            });
                                        let registered = registered_asset.is_some();
                                        let gltf_source = engine::asset_path_matches_kind(
                                            engine::AssetKind::GltfSource,
                                            &entry.relative_path,
                                        );
                                        // Reimport and Import Settings apply
                                        // to `.vmd` motions too; Create
                                        // Retarget Map stays model-only,
                                        // since a motion owns no rig to map
                                        // between.
                                        let import_source =
                                            is_importable_source_path(&entry.relative_path);
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_width, tile_height),
                                            egui::Sense::click_and_drag(),
                                        );

                                        // Texture thumbnails use the same decoder as
                                        // the full preview window, but remain cached
                                        // for the lifetime of the open editor.
                                        let thumbnail = if entry.kind == AssetKind::Texture {
                                            let key = entry.relative_path.clone();
                                            if !asset_thumbnails.contains_key(&key)
                                                && let Ok(preview) = load_texture_preview(
                                                    ui.ctx(),
                                                    &root.join(&key),
                                                    key.clone(),
                                                ) {
                                                    asset_thumbnails.insert(key.clone(), preview);
                                                }
                                            asset_thumbnails.get(&key)
                                        } else {
                                            None
                                        };
                                        let preview_rect = egui::Rect::from_center_size(
                                            egui::pos2(rect.center().x, rect.top() + 42.0),
                                            egui::vec2(66.0, 66.0),
                                        );
                                        if let Some(thumbnail) = thumbnail {
                                            ui.painter().image(
                                                thumbnail.texture.id(),
                                                preview_rect,
                                                egui::Rect::from_min_max(
                                                    egui::pos2(0.0, 0.0),
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                egui::Color32::WHITE,
                                            );
                                        } else {
                                            ui.painter().text(
                                                preview_rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                asset_kind_icon(entry.kind),
                                                egui::FontId::proportional(38.0),
                                                asset_kind_color(entry.kind),
                                            );
                                        }
                                        ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                                            egui::pos2(rect.center().x, rect.bottom() - 7.0),
                                            egui::Align2::CENTER_BOTTOM,
                                            &entry.display_name,
                                            egui::FontId::proportional(11.0),
                                            egui::Color32::WHITE,
                                        );
                                        if selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                4.0,
                                                egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_BLUE),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                        if registered {
                                            ui.painter().text(
                                                rect.right_top() - egui::vec2(8.0, -8.0),
                                                egui::Align2::RIGHT_TOP,
                                                "✓",
                                                egui::FontId::proportional(14.0),
                                                egui::Color32::LIGHT_GREEN,
                                            );
                                        }

                                        if response.clicked() {
                                            let additive = ui.input(|input| {
                                                input.modifiers.ctrl || input.modifiers.command
                                            });
                                            browser.select_path(&entry.relative_path, additive);
                                        }
                                        if response.double_clicked() {
                                            action = Some(AssetBrowserAction::Open(*index));
                                        }
                                        let mut registered_drag = None;
                                        if let Some((asset_id, _)) = registered_asset {
                                            // Meshes drop into the Scene View to
                                            // spawn entities; materials/textures
                                            // drop onto entities and Inspector
                                            // fields to assign references. A
                                            // model source drops as its whole
                                            // generated hierarchy (ADR 0075).
                                            let draggable = matches!(
                                                entry.kind,
                                                AssetKind::Mesh
                                                    | AssetKind::Material
                                                    | AssetKind::Texture
                                                    | AssetKind::AnimationSet
                                                    | AssetKind::AnimationClip
                                            );
                                            registered_drag = draggable.then(|| asset_id.clone());
                                        }
                                        let drag_paths = if selected {
                                            browser.selected_paths().cloned().collect::<Vec<_>>()
                                        } else {
                                            vec![entry.relative_path.clone()]
                                        };
                                        // egui holds one payload per drag, so
                                        // setting a second one here would
                                        // silently replace the first and break
                                        // whichever drop target reads it.
                                        match registered_drag {
                                            Some(asset_id) => response.clone().dnd_set_drag_payload(
                                                DragPayload {
                                                    asset_id,
                                                    relative_path: entry.relative_path.clone(),
                                                    kind: entry.kind,
                                                    paths: drag_paths,
                                                },
                                            ),
                                            None => response.clone().dnd_set_drag_payload(
                                                AssetPathDragPayload { paths: drag_paths },
                                            ),
                                        }
                                        if let Some(payload) =
                                            response.dnd_release_payload::<HierarchyDragPayload>()
                                        {
                                            action =
                                                Some(AssetBrowserAction::CreatePrefabFromEntity {
                                                    entity: payload.entity.clone(),
                                                    destination_folder: browser
                                                        .selected_folder()
                                                        .to_path_buf(),
                                                });
                                        }
                                        let selected_count = if selected {
                                            browser.selected_paths().count()
                                        } else {
                                            1
                                        };
                                        response.context_menu(|ui| {
                                            if ui.button("Open").clicked() {
                                                action = Some(AssetBrowserAction::Open(*index));
                                                ui.close();
                                            }
                                            let registerable =
                                                is_registerable_asset(&entry.relative_path);
                                            if registerable
                                                && !registered
                                                && ui.button("Register Asset").clicked()
                                            {
                                                action = Some(AssetBrowserAction::Register(*index));
                                                ui.close();
                                            }
                                            if registered
                                                && import_source
                                                && ui.button("Reimport").clicked()
                                            {
                                                action = Some(AssetBrowserAction::Reimport(*index));
                                                ui.close();
                                            }
                                            if registered
                                                && import_source
                                                && ui.button("Edit Import Settings...").clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::EditImportSettings(*index),
                                                );
                                                ui.close();
                                            }
                                            if registered && gltf_source {
                                                let other_targets: Vec<&(AssetId, String)> =
                                                    skeleton_source_choices
                                                        .iter()
                                                        .filter(|(id, _)| {
                                                            registered_asset
                                                                .is_none_or(|(this_id, _)| this_id != id)
                                                        })
                                                        .collect();
                                                if !other_targets.is_empty() {
                                                    ui.menu_button(
                                                        "Create Retarget Map",
                                                        |ui| {
                                                            for (target_id, label) in other_targets {
                                                                if ui.button(label).clicked() {
                                                                    action = Some(
                                                                        AssetBrowserAction::CreateRetargetMap {
                                                                            source: *index,
                                                                            target_source_id: target_id.clone(),
                                                                        },
                                                                    );
                                                                    ui.close();
                                                                }
                                                            }
                                                        },
                                                    );
                                                }
                                            }
                                            if entry.kind == AssetKind::Graph
                                                && super::inspector::manifest_path_matches_asset_kind(
                                                    engine::AssetKind::AnimationGraph,
                                                    &entry.relative_path,
                                                    Some(root),
                                                )
                                                && let Some((graph_id, _)) = registered_asset
                                                    && ui
                                                        .button("Create Animation Set")
                                                        .clicked()
                                                    {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationSet {
                                                                graph: Some(graph_id.clone()),
                                                                destination_folder: entry
                                                                    .relative_path
                                                                    .parent()
                                                                    .unwrap_or(Path::new(""))
                                                                    .to_path_buf(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                            if entry.kind == AssetKind::Prefab
                                                && ui.button("Instantiate in Scene").clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::InstantiatePrefab(*index),
                                                );
                                                ui.close();
                                            }
                                            // The model source row is what the
                                            // author places; the prefab behind
                                            // it stays hidden (ADR 0075).
                                            if registered
                                                && gltf_source
                                                && ui.button("Instantiate in Scene").clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::InstantiateModel(*index),
                                                );
                                                ui.close();
                                            }
                                            if entry.kind == AssetKind::Mesh
                                                && registered
                                                && !gltf_source
                                                && ui.button("Add to Scene").clicked()
                                            {
                                                action = Some(AssetBrowserAction::AddMeshToScene(
                                                    *index,
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Show in Explorer").clicked() {
                                                action = Some(AssetBrowserAction::ShowInExplorer(
                                                    entry.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            ui.separator();
                                            if ui.button("Rename...").clicked() {
                                                action =
                                                    Some(AssetBrowserAction::RenameAsset(*index));
                                                ui.close();
                                            }
                                            let move_label = if selected_count > 1 {
                                                format!("Move {selected_count} Assets...")
                                            } else {
                                                "Move...".to_owned()
                                            };
                                            if ui.button(move_label).clicked() {
                                                action =
                                                    Some(AssetBrowserAction::MoveAsset(*index));
                                                ui.close();
                                            }
                                            let delete_label = if selected_count > 1 {
                                                format!("Delete {selected_count} Assets...")
                                            } else {
                                                "Delete...".to_owned()
                                            };
                                            if ui.button(delete_label).clicked() {
                                                action =
                                                    Some(AssetBrowserAction::TrashAsset(*index));
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            }

                            show_selected_gltf_sub_assets(ui, browser, manifest);

                            // A hint only: it must not sense clicks, or it would
                            // sit on top of the background and reintroduce a
                            // strip where the create menu behaves differently.
                            ui.add_sized(
                                [ui.available_width(), 36.0],
                                egui::Label::new(
                                    egui::RichText::new(
                                        "Right-click to create; drop an Entity here to create a Prefab",
                                    )
                                    .small()
                                    .color(egui::Color32::GRAY),
                                ),
                            );
                            // Read last, so a drop that a folder tile or asset
                            // row already claimed has taken the payload and only
                            // genuinely empty drops reach the selected folder.
                            if let Some(payload) =
                                background.dnd_release_payload::<HierarchyDragPayload>()
                            {
                                action = Some(AssetBrowserAction::CreatePrefabFromEntity {
                                    entity: payload.entity.clone(),
                                    destination_folder: browser.selected_folder().to_path_buf(),
                                });
                            }
                            background.context_menu(|ui| {
                                ui.menu_button("Create", |ui| {
                                    ui.menu_button("General", |ui| {
                                        if ui.button("Folder...").clicked() {
                                            action = Some(AssetBrowserAction::NewFolder);
                                            ui.close();
                                        }
                                        if ui.button("Scene").clicked() {
                                            action = Some(AssetBrowserAction::NewScene);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Rendering", |ui| {
                                        if ui.button("Material").clicked() {
                                            action = Some(AssetBrowserAction::NewMaterial);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Animation", |ui| {
                                        if ui.button("Animation Graph").clicked() {
                                            action = Some(
                                                AssetBrowserAction::NewAnimationGraph {
                                                    destination_folder: browser
                                                        .selected_folder()
                                                        .to_path_buf(),
                                                },
                                            );
                                            ui.close();
                                        }
                                        if ui.button("Animation Set").clicked() {
                                            action = Some(AssetBrowserAction::NewAnimationSet {
                                                graph: None,
                                                destination_folder: browser
                                                    .selected_folder()
                                                    .to_path_buf(),
                                            });
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("UI", |ui| {
                                        if ui.button("UI Document").clicked() {
                                            action = Some(AssetBrowserAction::NewUiDocument);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Scripting", |ui| {
                                        if ui.button("Rhai Script...").clicked() {
                                            action = Some(AssetBrowserAction::NewRhaiScript);
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                can_create_rust_script,
                                                egui::Button::new("Rust Script..."),
                                            )
                                            .on_disabled_hover_text(
                                                "Initialize the Rust Game first",
                                            )
                                            .clicked()
                                        {
                                            action = Some(AssetBrowserAction::NewRustScript);
                                            ui.close();
                                        }
                                    });
                                });
                                if ui.button("Refresh").clicked() {
                                    refresh_requested = true;
                                    ui.close();
                                }
                            });
                        });
                    },
                );
            });
            if refresh_requested {
                let previous_folder = browser.selected_folder().to_path_buf();
                browser.refresh(root);
                if browser.selected_folder() != previous_folder {
                    *content_scroll_reset = true;
                }
                asset_thumbnails.retain(|path, _| root.join(path).is_file());
            }
            action
        }
    }
}

/// Returns the compact visual symbol used for an asset tile when no raster
/// thumbnail is available. The symbol keeps the browser readable even for
/// formats that require a renderer-backed preview.
pub(super) fn asset_kind_icon(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Scene => "▣",
        AssetKind::Graph | AssetKind::GraphView => "◇",
        AssetKind::AnimationSet => "◎",
        AssetKind::AnimationClip => "▶",
        AssetKind::MotionSource => "↝",
        AssetKind::Texture => "▤",
        AssetKind::Mesh => "△",
        AssetKind::Audio => "♪",
        AssetKind::Material => "◈",
        AssetKind::Prefab => "◆",
        AssetKind::UiDocument => "▦",
        AssetKind::NavMesh => "⌁",
        AssetKind::RetargetMap => "⇄",
        AssetKind::Script
        | AssetKind::RustComponent
        | AssetKind::RustResource
        | AssetKind::RustSystem
        | AssetKind::RustModule => "‹›",
    }
}

/// Returns a stable accent color for each asset family.
pub(super) fn asset_kind_color(kind: AssetKind) -> egui::Color32 {
    match kind {
        AssetKind::Texture => egui::Color32::from_rgb(100, 190, 255),
        AssetKind::Mesh => egui::Color32::from_rgb(255, 190, 90),
        AssetKind::Prefab => egui::Color32::from_rgb(220, 130, 255),
        AssetKind::Scene => egui::Color32::from_rgb(120, 220, 170),
        AssetKind::Material => egui::Color32::from_rgb(255, 150, 150),
        AssetKind::Audio => egui::Color32::from_rgb(250, 220, 110),
        AssetKind::Graph | AssetKind::GraphView => egui::Color32::from_rgb(150, 180, 255),
        AssetKind::AnimationSet => egui::Color32::from_rgb(190, 150, 255),
        AssetKind::AnimationClip | AssetKind::MotionSource => {
            egui::Color32::from_rgb(120, 210, 255)
        }
        _ => egui::Color32::from_gray(180),
    }
}

/// Returns `true` for a `*.ui.json` declarative UI document path (Phase 54).
///
/// This test-only compatibility helper checks the same shared category filter
/// used by registration and Inspector pickers.
#[cfg(test)]
pub(super) fn is_registerable_ui_document(path: &Path) -> bool {
    manifest_path_matches_asset_kind(engine::AssetKind::UiDocument, path, None)
}

pub(super) fn asset_relative_path_string(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        parts.push(part.to_str()?);
    }
    Some(parts.join("/"))
}

/// Converts an entity name into a safe, readable prefab filename stem.
/// Slashes and other path separators are replaced so an Entity name can
/// never make a drag-and-drop operation escape its selected asset folder.
fn prefab_file_stem(name: &str) -> String {
    let mut stem = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            stem.push(character);
        } else {
            stem.push('_');
        }
    }
    let stem = stem.trim_matches('.').trim_matches('_');
    if stem.is_empty() {
        "new_prefab".to_owned()
    } else {
        stem.to_owned()
    }
}

fn normalize_manifest_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

/// Renders the paired-model picker for a `*.vmd` motion source (ADR 0097 §3).
///
/// A VMD names bones but carries no rig, so it cannot be imported until the
/// author says which PMX to bake it against. The list is deliberately just
/// the project's `.pmx` sources — an FBX or glTF has no MMD IK/appended-parent
/// data to evaluate, so offering one would only produce a failed import.
fn show_motion_pairing_editor(ui: &mut egui::Ui, pairing: &mut MotionPairingState) {
    ui.strong("Original PMX model (optional)");
    let previous_original = pairing.original.clone();
    let original_label = pairing
        .original
        .as_ref()
        .and_then(|selected| {
            pairing
                .candidates
                .iter()
                .find(|(id, _)| id == selected)
                .map(|(_, label)| label.as_str())
        })
        .unwrap_or("Not set - Direct bake");
    egui::ComboBox::from_id_salt("vmd_original_pmx")
        .selected_text(original_label)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut pairing.original, None, "Not set - Direct bake");
            for (id, label) in &pairing.candidates {
                ui.selectable_value(&mut pairing.original, Some(id.clone()), label);
            }
        });
    if pairing.original != previous_original {
        pairing.compatibility_reports.clear();
    }
    if let Some(recorded) = &pairing.recorded_model_name {
        ui.label(format!("VMD recorded model: {recorded}"));
    }
    if pairing.original.is_none() {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Direct bake evaluates MMD constraints separately on each output PMX.",
        );
    } else {
        ui.label("The VMD is baked once on the original PMX, then retargeted to each output.");
    }
    ui.add_space(6.0);
    ui.strong("Output PMX models");
    ui.label("One target-specific clip is produced for each selected model.");
    if pairing.candidates.is_empty() {
        ui.label("No .pmx model is registered in this project yet.");
        return;
    }
    let mut output_selection_changed = false;
    for (id, label) in &pairing.candidates {
        let mut selected = pairing.selected.contains(id);
        if ui.checkbox(&mut selected, label).changed() {
            output_selection_changed = true;
            if selected {
                pairing.selected.push(id.clone());
            } else {
                pairing.selected.retain(|selected_id| selected_id != id);
            }
        }
    }
    if output_selection_changed {
        pairing.compatibility_reports.clear();
    }
    if !pairing.selected.is_empty() {
        ui.add_space(4.0);
        ui.strong("Processing summary");
        for target in &pairing.selected {
            let target_label = pairing
                .candidates
                .iter()
                .find(|(id, _)| id == target)
                .map(|(_, label)| label.as_str())
                .unwrap_or(target.as_str());
            let summary = match &pairing.original {
                None => format!("Direct bake -> {target_label}"),
                Some(original) if original == target => {
                    format!("Original bake -> {target_label} (same PMX)")
                }
                Some(original) => {
                    let original_label = pairing
                        .candidates
                        .iter()
                        .find(|(id, _)| id == original)
                        .map(|(_, label)| label.as_str())
                        .unwrap_or(original.as_str());
                    let status = if pairing
                        .retarget_pairs
                        .iter()
                        .any(|(source, output)| source == original && output == target)
                    {
                        "Ready"
                    } else {
                        "Missing"
                    };
                    format!(
                        "{original_label} -> Retarget Map ({status}) -> {target_label}"
                    )
                }
            };
            ui.label(summary);
        }
        if pairing.original.is_some() {
            ui.label("Missing maps must be created as explicit Retarget Map assets before reimport.");
        }
    }
    show_motion_compatibility_checker(ui, pairing);
}

/// Shows the opt-in name/operation checker separately from Save and Reimport.
/// Results describe current source bytes and deliberately remain transient.
fn show_motion_compatibility_checker(ui: &mut egui::Ui, pairing: &mut MotionPairingState) {
    ui.add_space(8.0);
    ui.separator();
    ui.strong("VMD / PMX compatibility check");
    ui.label(
        "Checks meaningful VMD bone and morph tracks by exact Japanese name. Neutral-only tracks are ignored.",
    );

    let has_check_target = pairing.original.is_some() || !pairing.selected.is_empty();
    let can_check = pairing.motion_path.is_some() && has_check_target;
    if ui
        .add_enabled(can_check, egui::Button::new("Check compatibility"))
        .on_hover_text(
            "Reads the current VMD and selected PMX files. This does not save, reimport, or modify assets.",
        )
        .clicked()
    {
        run_motion_compatibility_checks(pairing);
    }
    if !has_check_target {
        ui.label("Select an original or at least one output PMX to run the check.");
    } else if pairing.motion_path.is_none() {
        ui.colored_label(egui::Color32::RED, "The VMD source path is unavailable.");
    }

    for display in &pairing.compatibility_reports {
        ui.add_space(6.0);
        let label = pairing
            .candidates
            .iter()
            .find(|(id, _)| id == &display.model_source)
            .map(|(_, label)| label.as_str())
            .unwrap_or(display.model_source.as_str());
        ui.strong(label);
        match &display.result {
            Ok(report) => show_motion_compatibility_report(ui, pairing, display, report),
            Err(error) => {
                ui.colored_label(egui::Color32::RED, format!("Check failed: {error}"));
            }
        }
    }
}

/// Parses each distinct selected role in stable source/output order.
fn run_motion_compatibility_checks(pairing: &mut MotionPairingState) {
    pairing.compatibility_reports.clear();
    let Some(vmd_path) = pairing.motion_path.as_deref() else {
        return;
    };
    let mut targets = Vec::new();
    if let Some(original) = &pairing.original {
        targets.push(original.clone());
    }
    for output in &pairing.selected {
        if !targets.contains(output) {
            targets.push(output.clone());
        }
    }
    for target in targets {
        let result = pairing
            .candidate_paths
            .iter()
            .find(|(id, _)| id == &target)
            .ok_or_else(|| "The registered PMX source path is unavailable.".to_owned())
            .and_then(|(_, pmx_path)| {
                engine::check_vmd_pmx_compatibility_path(vmd_path, pmx_path)
                    .map_err(|error| error.to_string())
            });
        pairing.compatibility_reports.push(MotionCompatibilityDisplay {
            model_source: target,
            result,
        });
    }
}

/// Presents source and output roles without implying that direct output bone
/// names determine a Retarget Map conversion's success.
fn show_motion_compatibility_report(
    ui: &mut egui::Ui,
    pairing: &MotionPairingState,
    display: &MotionCompatibilityDisplay,
    report: &engine::VmdPmxCompatibilityReport,
) {
    let is_original = pairing.original.as_ref() == Some(&display.model_source);
    let is_retarget_output = pairing.original.is_some() && !is_original;
    if is_original {
        ui.label("Role: Original PMX (source bake rig)");
    } else {
        ui.label("Role: Output PMX");
    }
    let bone_label = if is_original {
        "Bone compatibility"
    } else if is_retarget_output {
        "Direct bone-name compatibility"
    } else {
        "Bone name compatibility"
    };
    show_compatibility_summary(ui, bone_label, report.bones);
    if is_retarget_output {
        ui.label("Informational only - bone conversion uses Retarget Map.");
    }
    show_compatibility_summary(ui, "Morph name compatibility", report.morphs);

    if report.issues.is_empty() {
        ui.colored_label(
            egui::Color32::GREEN,
            "No name ambiguity, missing track, or used-operation issue found.",
        );
        return;
    }
    let shown = report.issues.len().min(20);
    for issue in report.issues.iter().take(shown) {
        ui.colored_label(
            egui::Color32::YELLOW,
            format!(
                "{}: {} ({} keys)",
                compatibility_issue_label(issue.kind),
                issue.name,
                issue.keyframe_count
            ),
        );
    }
    if report.issues.len() > shown {
        ui.label(format!("... and {} more issues", report.issues.len() - shown));
    }
}

fn show_compatibility_summary(
    ui: &mut egui::Ui,
    label: &str,
    summary: engine::VmdPmxCompatibilitySummary,
) {
    match summary.compatibility_percent() {
        Some(percent) => ui.label(format!(
            "{label}: {percent:.1}% (unique {}/{}, missing {}, ambiguous {})",
            summary.unique_tracks,
            summary.used_tracks,
            summary.missing_tracks,
            summary.ambiguous_tracks
        )),
        None => ui.label(format!("{label}: N/A (no meaningful VMD tracks)")),
    };
}

fn compatibility_issue_label(kind: engine::VmdPmxCompatibilityIssueKind) -> &'static str {
    use engine::VmdPmxCompatibilityIssueKind as Kind;
    match kind {
        Kind::MissingBone => "Missing bone",
        Kind::AmbiguousBone => "Ambiguous bone name",
        Kind::RotationUnsupported => "Used rotation is not supported",
        Kind::TranslationUnsupported => "Used translation is not supported",
        Kind::MissingMorph => "Missing morph",
        Kind::AmbiguousMorph => "Ambiguous morph name",
    }
}

/// Resolves registered skeleton-to-skeleton Retarget Maps back to their PMX
/// model sources for the VMD Import Settings readiness summary.
/// Lists registered model sources that own at least one recorded skeleton.
///
/// VMD motion sources may repeat the skeleton records of the PMX models they
/// were baked against. They do not own those rigs, so accepting every manifest
/// entry with a skeleton record would incorrectly expose motions as retarget
/// map targets.
pub(super) fn retarget_map_model_source_choices(
    manifest: &engine::AssetManifest,
) -> Vec<(AssetId, String)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            // GltfSource is the legacy category name shared by all supported
            // model documents: glTF, GLB, FBX, and PMX.
            engine::asset_path_matches_kind(
                engine::AssetKind::GltfSource,
                Path::new(&entry.path),
            ) && !entry.import_settings.skeleton_records.is_empty()
        })
        .map(|(id, entry)| {
            (
                id.clone(),
                entry
                    .name
                    .clone()
                    .unwrap_or_else(|| entry.path.clone()),
            )
        })
        .collect()
}

fn registered_model_retarget_pairs(
    manifest: &engine::AssetManifest,
    assets_root: &Path,
) -> Vec<(AssetId, AssetId)> {
    let maps = engine::load_registered_retarget_maps(assets_root, manifest);
    let owner = |skeleton: &AssetId| {
        manifest.iter().find_map(|(model, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
                .then_some(())?;
            entry
                .import_settings
                .skeleton_records
                .iter()
                .any(|record| record.id == skeleton.as_str())
                .then(|| model.clone())
        })
    };
    let mut pairs = maps
        .iter()
        .filter_map(|(_, map)| Some((owner(&map.source_skeleton)?, owner(&map.target_skeleton)?)))
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    pairs.dedup();
    pairs
}

/// Lists every registered `.pmx` source as `(id, display label)` for the
/// motion pairing picker, in the manifest's own stable order.
fn pmx_model_sources(manifest: &engine::AssetManifest) -> Vec<(AssetId, String)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        })
        .map(|(id, entry)| {
            let label = entry
                .name
                .clone()
                .unwrap_or_else(|| entry.path.clone());
            (id.clone(), label)
        })
        .collect()
}

/// Resolves registered PMX source IDs to absolute project asset paths for the
/// on-demand compatibility checker. The manifest remains the source of truth;
/// no filesystem scan or filename-based pairing is performed.
fn pmx_model_paths(
    manifest: &engine::AssetManifest,
    assets_root: &Path,
) -> Vec<(AssetId, PathBuf)> {
    manifest
        .iter()
        .filter(|(_, entry)| {
            Path::new(&entry.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        })
        .map(|(id, entry)| (id.clone(), assets_root.join(&entry.path)))
        .collect()
}

/// Returns whether `path` names a source the background importer can catalog:
/// a model document or a `.vmd` motion (ADR 0097 §3).
pub(super) fn is_importable_source_path(path: &Path) -> bool {
    engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path)
        || engine::asset_path_matches_kind(engine::AssetKind::MotionSource, path)
}

/// Returns the registered `.pmx` model source when the project holds exactly
/// one, so a newly registered motion can be paired without asking.
///
/// Deliberately `None` for zero or several: pairing a motion with the wrong
/// rig bakes a clip that looks plausible and is wrong (ADR 0097 §3's
/// `vmd.rest_pose_mismatch` is a warning, not a hard stop), so the ambiguous
/// case belongs to the author.
pub(super) fn sole_pmx_model_source(manifest: &engine::AssetManifest) -> Option<engine_authoring::AssetId> {
    let mut models = manifest.iter().filter(|(_, entry)| {
        Path::new(&entry.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
    });
    let (id, _) = models.next()?;
    models.next().is_none().then(|| id.clone())
}

pub(super) fn unique_asset_name(display_name: &str, manifest: &engine::AssetManifest) -> String {
    let base = asset_name_slug(display_name);
    if !manifest
        .iter()
        .any(|(_, entry)| entry.name.as_deref() == Some(base.as_str()))
    {
        return base;
    }

    for suffix in 2.. {
        let candidate = format!("{base}_{suffix}");
        if !manifest
            .iter()
            .any(|(_, entry)| entry.name.as_deref() == Some(candidate.as_str()))
        {
            return candidate;
        }
    }
    unreachable!("usize suffix space must not be exhausted")
}

pub(super) fn asset_name_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator && !slug.is_empty() {
            slug.push('_');
            previous_was_separator = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "asset".into()
    } else {
        slug
    }
}

pub(super) fn load_texture_preview(
    context: &egui::Context,
    source: &Path,
    relative_path: PathBuf,
) -> Result<TexturePreview, String> {
    let bytes = fs::read(source).map_err(|error| error.to_string())?;
    let decoded = engine::DecodedTexture::from_bytes(&bytes, source.display().to_string())
        .map_err(|error| error.to_string())?;
    if decoded.width > engine::MAX_TEXTURE_DIMENSION
        || decoded.height > engine::MAX_TEXTURE_DIMENSION
    {
        return Err(format!(
            "{}x{} exceeds the {} px renderer limit",
            decoded.width,
            decoded.height,
            engine::MAX_TEXTURE_DIMENSION
        ));
    }
    let dimensions = [decoded.width as usize, decoded.height as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(dimensions, &decoded.rgba8);
    let texture = context.load_texture(
        format!("asset_preview:{}", relative_path.display()),
        color_image,
        egui::TextureOptions::LINEAR,
    );
    Ok(TexturePreview {
        relative_path,
        dimensions,
        texture,
    })
}

fn show_material_preview(
    ui: &mut egui::Ui,
    material: &engine_authoring::MaterialAsset,
    texture: Option<&TexturePreview>,
) {
    let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let tint = egui::Color32::from_rgba_unmultiplied(
        to_byte(material.base_color.r),
        to_byte(material.base_color.g),
        to_byte(material.base_color.b),
        to_byte(material.base_color.a),
    );
    let size = egui::vec2(180.0, 180.0);
    if let Some(texture) = texture {
        ui.add(egui::Image::new((texture.texture.id(), size)).tint(tint));
        ui.small(format!(
            "Base texture: {} × {} px",
            texture.dimensions[0], texture.dimensions[1]
        ));
    } else {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(rect, 6.0, tint);
        ui.painter().rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0_f32, egui::Color32::GRAY),
            egui::StrokeKind::Inside,
        );
        ui.small("Base color (no base texture)");
    }
    ui.small(format!(
        "roughness {:.2}  metallic {:.2}  {:?} / {:?} / {:?}",
        material.roughness,
        material.metallic,
        material.alpha_mode,
        material.cull_mode,
        material.shading_model
    ));
}

pub(super) fn is_registerable_asset(path: &Path) -> bool {
    crate::asset_management::is_registerable_asset_path(path)
}
