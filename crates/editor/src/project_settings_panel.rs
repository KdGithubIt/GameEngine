//! Project Settings editor panel (Phase 34).
//!
//! Provides a toolkit-independent data model for displaying and editing
//! project-wide settings (Tags, Layers, Input Actions, Start Scene).
//! GUI rendering is a thin egui wrapper that delegates all logic to the
//! [`engine_authoring::project_settings`] types.

use engine::postprocess::PostProcessSettings;
use engine::postprocess::ToneMapOperator;
use engine_authoring::project_settings::{
    AxisBinding, InputAction, KeyAxisBinding, Layer, ProjectSettings,
};

// ---------------------------------------------------------------------------
// Panel state
// ---------------------------------------------------------------------------

/// Transient editor-side state for the Project Settings panel.
///
/// Holds the current (possibly unsaved) edit state.  Call
/// [`ProjectSettingsPanel::commit`] to obtain the `ProjectSettings` that
/// should be saved back to disk.
pub struct ProjectSettingsPanel {
    /// The settings currently displayed in the panel.
    pub settings: ProjectSettings,
    /// Runtime post-processing settings (Phase 45). Not serialised into
    /// `ProjectSettings`; the caller injects this into the engine world resource.
    pub post_process: PostProcessSettings,
    /// `true` when the panel contains unsaved changes.
    pub is_dirty: bool,
}

impl ProjectSettingsPanel {
    /// Creates a panel backed by the given `settings`.
    pub fn new(settings: ProjectSettings) -> Self {
        Self {
            settings,
            post_process: PostProcessSettings::default(),
            is_dirty: false,
        }
    }

    /// Returns the current settings.
    pub fn commit(&mut self) -> ProjectSettings {
        self.is_dirty = false;
        self.settings.clone()
    }

    // ── Tags ──────────────────────────────────────────────────────────────

    /// Appends `tag` to the tag list unless it is already present.
    pub fn add_tag(&mut self, tag: impl Into<String>) {
        let tag = tag.into();
        if !self.settings.tags.contains(&tag) {
            self.settings.tags.push(tag);
            self.is_dirty = true;
        }
    }

    /// Removes `tag` from the tag list.
    pub fn remove_tag(&mut self, tag: &str) {
        let before = self.settings.tags.len();
        self.settings.tags.retain(|t| t != tag);
        if self.settings.tags.len() != before {
            self.is_dirty = true;
        }
    }

    // ── Layers ────────────────────────────────────────────────────────────

    /// Adds a new layer with the next available index (up to 31).
    ///
    /// Does nothing if all 32 layer slots are occupied.
    pub fn add_layer(&mut self, name: impl Into<String>) {
        let used: std::collections::BTreeSet<u32> =
            self.settings.layers.iter().map(|l| l.index).collect();
        for idx in 0u32..32 {
            if !used.contains(&idx) {
                self.settings.layers.push(Layer {
                    index: idx,
                    name: name.into(),
                });
                self.settings.layers.sort_by_key(|l| l.index);
                self.is_dirty = true;
                return;
            }
        }
    }

    /// Removes the layer with `index`.
    pub fn remove_layer(&mut self, index: u32) {
        let before = self.settings.layers.len();
        self.settings.layers.retain(|l| l.index != index);
        if self.settings.layers.len() != before {
            self.is_dirty = true;
        }
    }

    // ── Input Actions ─────────────────────────────────────────────────────

    /// Adds or replaces an input action's key bindings.
    ///
    /// Existing gamepad bindings are preserved when the action already exists.
    pub fn set_input_action(&mut self, name: impl Into<String>, keys: Vec<String>) {
        let name = name.into();
        if let Some(existing) = self
            .settings
            .input_actions
            .iter_mut()
            .find(|a| a.name == name)
        {
            existing.keys = keys;
        } else {
            self.settings.input_actions.push(InputAction {
                name,
                keys,
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            });
        }
        self.is_dirty = true;
    }

    /// Sets the gamepad button indices bound to an existing action (Phase 43).
    ///
    /// Does nothing when the action is not found.
    pub fn set_input_action_gamepad_buttons(&mut self, name: &str, buttons: Vec<u32>) {
        if let Some(existing) = self
            .settings
            .input_actions
            .iter_mut()
            .find(|a| a.name == name)
        {
            existing.gamepad_buttons = buttons;
            self.is_dirty = true;
        }
    }

    /// Sets named mouse buttons bound to an existing action.
    pub fn set_input_action_mouse_buttons(&mut self, name: &str, buttons: Vec<String>) {
        if let Some(existing) = self
            .settings
            .input_actions
            .iter_mut()
            .find(|action| action.name == name)
        {
            existing.mouse_buttons = buttons;
            self.is_dirty = true;
        }
    }

    /// Sets the gamepad axis bindings for an existing action (Phase 43).
    ///
    /// Does nothing when the action is not found.
    pub fn set_input_action_gamepad_axes(&mut self, name: &str, axes: Vec<AxisBinding>) {
        if let Some(existing) = self
            .settings
            .input_actions
            .iter_mut()
            .find(|a| a.name == name)
        {
            existing.gamepad_axes = axes;
            self.is_dirty = true;
        }
    }

    /// Sets keyboard positive/negative axis pairs for an existing action.
    pub fn set_input_action_key_axes(&mut self, name: &str, axes: Vec<KeyAxisBinding>) {
        if let Some(existing) = self
            .settings
            .input_actions
            .iter_mut()
            .find(|action| action.name == name)
        {
            existing.key_axes = axes;
            self.is_dirty = true;
        }
    }

    /// Removes an input action by name.
    pub fn remove_input_action(&mut self, name: &str) {
        let before = self.settings.input_actions.len();
        self.settings.input_actions.retain(|a| a.name != name);
        if self.settings.input_actions.len() != before {
            self.is_dirty = true;
        }
    }

    // ── Start Scene ───────────────────────────────────────────────────────

    /// Sets the start scene relative path.
    pub fn set_start_scene(&mut self, path: Option<String>) {
        self.settings.start_scene = path;
        self.is_dirty = true;
    }
}

// ---------------------------------------------------------------------------
// egui rendering
// ---------------------------------------------------------------------------

/// Renders the Project Settings panel inside `ui`.
///
/// Returns `true` when the user made a change that should be saved.
pub fn show_project_settings_panel(
    panel: &mut ProjectSettingsPanel,
    ui: &mut eframe::egui::Ui,
) -> bool {
    let before_dirty = panel.is_dirty;
    let mut changed = false;

    ui.heading("Project Settings");

    // Tags
    ui.collapsing("Tags", |ui| {
        let tags = panel.settings.tags.clone();
        for (index, tag) in tags.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut edited = tag.clone();
                if ui.text_edit_singleline(&mut edited).changed()
                    && !edited.trim().is_empty()
                    && !panel
                        .settings
                        .tags
                        .iter()
                        .enumerate()
                        .any(|(other, value)| other != index && value == edited.trim())
                {
                    panel.settings.tags[index] = edited.trim().to_owned();
                    panel.is_dirty = true;
                    changed = true;
                }
                if ui.small_button("✕").clicked() {
                    panel.remove_tag(tag);
                    changed = true;
                }
            });
        }
        if ui.button("+ Add Tag").clicked() {
            panel.add_tag("NewTag");
            changed = true;
        }
    });

    // Layers
    ui.collapsing("Layers", |ui| {
        let layers: Vec<_> = panel.settings.layers.clone();
        for (row, layer) in layers.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(format!("[{}]", layer.index));
                let mut name = layer.name.clone();
                if ui.text_edit_singleline(&mut name).changed()
                    && !name.trim().is_empty()
                    && !panel
                        .settings
                        .layers
                        .iter()
                        .enumerate()
                        .any(|(other, value)| {
                            other != row && value.name.eq_ignore_ascii_case(name.trim())
                        })
                    && let Some(target) = panel
                        .settings
                        .layers
                        .iter_mut()
                        .find(|candidate| candidate.index == layer.index)
                    {
                        target.name = name.trim().to_owned();
                        panel.is_dirty = true;
                        changed = true;
                    }
                if layer.index != 0 && ui.small_button("✕").clicked() {
                    panel.remove_layer(layer.index);
                    changed = true;
                }
            });
        }
        if ui.button("+ Add Layer").clicked() {
            panel.add_layer("NewLayer");
            changed = true;
        }
    });

    // Input Actions
    ui.collapsing("Input Actions", |ui| {
        let actions: Vec<_> = panel.settings.input_actions.clone();
        let mut remove_action = None;
        for (index, action) in actions.iter().enumerate() {
            ui.collapsing(&action.name, |ui| {
                let mut edited = action.clone();
                let mut action_changed = false;
                ui.horizontal(|ui| {
                    ui.label("Name");
                    action_changed |= ui.text_edit_singleline(&mut edited.name).changed();
                    if ui.small_button("Delete Action").clicked() {
                        remove_action = Some(index);
                    }
                });
                action_changed |= edit_csv(ui, "Keys", &mut edited.keys);
                action_changed |= edit_csv(ui, "Mouse Buttons", &mut edited.mouse_buttons);

                let mut gamepad_buttons = edited
                    .gamepad_buttons
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                ui.horizontal(|ui| {
                    ui.label("Gamepad Buttons");
                    if ui.text_edit_singleline(&mut gamepad_buttons).changed()
                        && let Some(parsed) = parse_u32_csv(&gamepad_buttons) {
                            edited.gamepad_buttons = parsed;
                            action_changed = true;
                        }
                });

                ui.label("Gamepad Axes");
                let mut remove_axis = None;
                for (axis_index, axis) in edited.gamepad_axes.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label("Axis");
                        action_changed |= ui
                            .add(eframe::egui::DragValue::new(&mut axis.axis))
                            .changed();
                        ui.label("Deadzone");
                        action_changed |= ui
                            .add(eframe::egui::DragValue::new(&mut axis.deadzone).range(0.0..=1.0))
                            .changed();
                        ui.label("Scale");
                        action_changed |= ui
                            .add(eframe::egui::DragValue::new(&mut axis.scale))
                            .changed();
                        action_changed |= ui.checkbox(&mut axis.invert, "Invert").changed();
                        if ui.small_button("−").clicked() {
                            remove_axis = Some(axis_index);
                        }
                    });
                }
                if let Some(axis) = remove_axis {
                    edited.gamepad_axes.remove(axis);
                    action_changed = true;
                }
                if ui.small_button("+ Gamepad Axis").clicked() {
                    edited.gamepad_axes.push(AxisBinding {
                        axis: 0,
                        deadzone: 0.15,
                        scale: 1.0,
                        invert: false,
                    });
                    action_changed = true;
                }

                ui.label("Keyboard Axes");
                let mut remove_key_axis = None;
                for (axis_index, axis) in edited.key_axes.iter_mut().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label("Component");
                            action_changed |= ui
                                .add(
                                    eframe::egui::DragValue::new(&mut axis.vector_component)
                                        .range(0..=1),
                                )
                                .changed();
                            ui.label("Scale");
                            action_changed |= ui
                                .add(eframe::egui::DragValue::new(&mut axis.scale))
                                .changed();
                            if ui.small_button("−").clicked() {
                                remove_key_axis = Some(axis_index);
                            }
                        });
                        action_changed |= edit_csv(ui, "Negative Keys", &mut axis.negative_keys);
                        action_changed |= edit_csv(ui, "Positive Keys", &mut axis.positive_keys);
                    });
                }
                if let Some(axis) = remove_key_axis {
                    edited.key_axes.remove(axis);
                    action_changed = true;
                }
                if ui.small_button("+ Keyboard Axis").clicked() {
                    edited.key_axes.push(KeyAxisBinding {
                        vector_component: 0,
                        negative_keys: Vec::new(),
                        positive_keys: Vec::new(),
                        scale: 1.0,
                    });
                    action_changed = true;
                }

                let name_valid = !edited.name.trim().is_empty()
                    && !panel
                        .settings
                        .input_actions
                        .iter()
                        .enumerate()
                        .any(|(other, item)| {
                            other != index && item.name.eq_ignore_ascii_case(edited.name.trim())
                        });
                if action_changed && name_valid {
                    edited.name = edited.name.trim().to_owned();
                    panel.settings.input_actions[index] = edited;
                    panel.is_dirty = true;
                    changed = true;
                } else if !name_valid {
                    ui.colored_label(
                        eframe::egui::Color32::RED,
                        "Action name must be unique and non-empty",
                    );
                }
            });
        }
        if let Some(index) = remove_action {
            panel.settings.input_actions.remove(index);
            panel.is_dirty = true;
            changed = true;
        }
        if ui.button("+ Add Input Action").clicked() {
            let name = unique_action_name(&panel.settings.input_actions);
            panel.settings.input_actions.push(InputAction {
                name,
                keys: Vec::new(),
                mouse_buttons: Vec::new(),
                gamepad_buttons: Vec::new(),
                gamepad_axes: Vec::new(),
                key_axes: Vec::new(),
            });
            panel.is_dirty = true;
            changed = true;
        }
    });

    // Post-Processing (Phase 45)
    ui.collapsing("Post-Processing", |ui| {
        changed |= ui
            .checkbox(&mut panel.post_process.enabled, "Enabled")
            .changed();

        if panel.post_process.enabled {
            ui.horizontal(|ui| {
                ui.label("Exposure");
                changed |= ui
                    .add(
                        eframe::egui::Slider::new(&mut panel.post_process.exposure, 0.0..=8.0)
                            .step_by(0.05),
                    )
                    .changed();
            });

            ui.label("Tone Mapping");
            ui.horizontal(|ui| {
                let is_aces = panel.post_process.tone_map == ToneMapOperator::AcesFitted;
                if ui.radio(is_aces, "ACES Fitted").clicked() {
                    panel.post_process.tone_map = ToneMapOperator::AcesFitted;
                    changed = true;
                }
                let is_rein = panel.post_process.tone_map == ToneMapOperator::Reinhard;
                if ui.radio(is_rein, "Reinhard").clicked() {
                    panel.post_process.tone_map = ToneMapOperator::Reinhard;
                    changed = true;
                }
            });

            changed |= ui
                .checkbox(&mut panel.post_process.bloom.enabled, "Bloom")
                .changed();

            if panel.post_process.bloom.enabled {
                ui.horizontal(|ui| {
                    ui.label("Threshold");
                    changed |= ui
                        .add(
                            eframe::egui::Slider::new(
                                &mut panel.post_process.bloom.threshold,
                                0.0..=4.0,
                            )
                            .step_by(0.05),
                        )
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Intensity");
                    changed |= ui
                        .add(
                            eframe::egui::Slider::new(
                                &mut panel.post_process.bloom.intensity,
                                0.0..=2.0,
                            )
                            .step_by(0.01),
                        )
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Radius");
                    changed |= ui
                        .add(eframe::egui::Slider::new(
                            &mut panel.post_process.bloom.radius,
                            0.0..=16.0,
                        ))
                        .changed();
                });
            }

            changed |= ui
                .checkbox(
                    &mut panel.post_process.color_grading.enabled,
                    "Color Grading",
                )
                .changed();
            if panel.post_process.color_grading.enabled {
                changed |= ui
                    .color_edit_button_rgb(&mut panel.post_process.color_grading.tint)
                    .changed();
                changed |= ui
                    .add(eframe::egui::Slider::new(
                        &mut panel.post_process.color_grading.saturation,
                        0.0..=2.0,
                    ).text("Saturation"))
                    .changed();
                changed |= ui
                    .add(eframe::egui::Slider::new(
                        &mut panel.post_process.color_grading.contrast,
                        0.0..=2.0,
                    ).text("Contrast"))
                    .changed();
                changed |= ui
                    .add(eframe::egui::Slider::new(
                        &mut panel.post_process.color_grading.gamma,
                        0.1..=3.0,
                    ).text("Gamma"))
                    .changed();
            }
        }
    });

    // Start Scene
    ui.horizontal(|ui| {
        ui.label("Start Scene");
        let mut path = panel.settings.start_scene.clone().unwrap_or_default();
        if ui
            .add(eframe::egui::TextEdit::singleline(&mut path).hint_text("scenes/main.scene.json"))
            .changed()
        {
            panel.set_start_scene((!path.trim().is_empty()).then(|| path.trim().to_owned()));
            changed = true;
        }
    });

    changed || panel.is_dirty != before_dirty
}

fn edit_csv(ui: &mut eframe::egui::Ui, label: &str, values: &mut Vec<String>) -> bool {
    let mut text = values.join(", ");
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.text_edit_singleline(&mut text).changed() {
            *values = text
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect();
            changed = true;
        }
    });
    changed
}

fn parse_u32_csv(text: &str) -> Option<Vec<u32>> {
    text.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn unique_action_name(actions: &[InputAction]) -> String {
    (1_u32..)
        .map(|suffix| {
            if suffix == 1 {
                "new_action".to_owned()
            } else {
                format!("new_action_{suffix}")
            }
        })
        .find(|candidate| actions.iter().all(|action| action.name != *candidate))
        .unwrap_or_else(|| "new_action_fallback".to_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_tag_marks_panel_dirty() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        assert!(!panel.is_dirty);
        panel.add_tag("Player");
        assert!(panel.is_dirty);
        assert!(panel.settings.tags.contains(&"Player".to_string()));
    }

    #[test]
    fn add_tag_is_idempotent() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.add_tag("Player");
        panel.add_tag("Player");
        assert_eq!(
            panel
                .settings
                .tags
                .iter()
                .filter(|t| *t == "Player")
                .count(),
            1
        );
    }

    #[test]
    fn remove_tag_deletes_entry() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.add_tag("Enemy");
        panel.is_dirty = false;
        panel.remove_tag("Enemy");
        assert!(panel.is_dirty);
        assert!(!panel.settings.tags.contains(&"Enemy".to_string()));
    }

    #[test]
    fn add_layer_assigns_next_available_index() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.add_layer("Enemies");
        let layer = panel.settings.layers.iter().find(|l| l.name == "Enemies");
        assert!(layer.is_some());
        assert_eq!(layer.unwrap().index, 1, "index 0 is Default, 1 is next");
    }

    #[test]
    fn set_input_action_overrides_existing() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.set_input_action("move_forward", vec!["ArrowUp".into()]);
        assert!(
            panel
                .settings
                .keys_for_action("move_forward")
                .contains(&"ArrowUp".to_string()),
            "move_forward must now be bound to ArrowUp"
        );
    }

    #[test]
    fn advanced_input_binding_setters_preserve_one_action_document() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.set_input_action("aim", Vec::new());
        panel.set_input_action_mouse_buttons("aim", vec!["Right".to_owned()]);
        panel.set_input_action_gamepad_axes(
            "aim",
            vec![AxisBinding {
                axis: 2,
                deadzone: 0.15,
                scale: 0.5,
                invert: true,
            }],
        );
        panel.set_input_action_key_axes(
            "aim",
            vec![KeyAxisBinding {
                vector_component: 0,
                negative_keys: vec!["ArrowLeft".to_owned()],
                positive_keys: vec!["ArrowRight".to_owned()],
                scale: 1.0,
            }],
        );

        let action = panel
            .settings
            .input_actions
            .iter()
            .find(|action| action.name == "aim")
            .unwrap();
        assert_eq!(action.mouse_buttons, ["Right"]);
        assert_eq!(action.gamepad_axes[0].scale, 0.5);
        assert!(action.gamepad_axes[0].invert);
        assert_eq!(action.key_axes[0].positive_keys, ["ArrowRight"]);
        assert!(panel.is_dirty);
    }

    #[test]
    fn commit_clears_dirty_flag() {
        let mut panel = ProjectSettingsPanel::new(ProjectSettings::default());
        panel.add_tag("Hero");
        assert!(panel.is_dirty);
        let _ = panel.commit();
        assert!(!panel.is_dirty);
    }
}
