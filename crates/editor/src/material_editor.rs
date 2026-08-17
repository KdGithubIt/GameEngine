//! Material editor panel (Phase 35).
//!
//! Provides a toolkit-independent data model for the material editor and a
//! thin egui rendering function.  GUI rendering is intentionally kept in a
//! single function so that the data model remains testable without egui.

use engine_authoring::material_asset::{
    LinearRgba, MaterialAlphaMode, MaterialAsset, MaterialCullMode, MaterialShadingModel,
};
use engine_authoring::{
    AssetId, AuthoringPermission, AuthoringPermissions, TypedDocumentAuthoringError,
    TypedDocumentAuthoringMutation,
    TypedDocumentAuthoringService, TypedDocumentAuthoringSnapshot, TypedDocumentAuthoringState,
    TypedDocumentAuthoringValidation,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const UNDO_LIMIT: usize = 100;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Holds the loaded material assets being edited in the Material Editor panel.
///
/// Materials are keyed by their relative path under `assets/`.
#[derive(Default)]
pub struct MaterialEditorPanel {
    /// Materials currently open for editing, keyed by asset-relative path.
    pub materials: BTreeMap<PathBuf, MaterialAsset>,
    /// The relative path of the currently selected / active material.
    pub active: Option<PathBuf>,
    authoring: BTreeMap<PathBuf, TypedDocumentAuthoringState>,
    undo: BTreeMap<PathBuf, Vec<MaterialAsset>>,
    redo: BTreeMap<PathBuf, Vec<MaterialAsset>>,
}

impl MaterialEditorPanel {
    /// Creates an empty panel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a material in the panel and sets it as active.
    pub fn open_material(&mut self, rel_path: PathBuf, material: MaterialAsset) {
        self.active = Some(rel_path.clone());
        self.authoring
            .insert(rel_path.clone(), TypedDocumentAuthoringState::new());
        self.undo.insert(rel_path.clone(), Vec::new());
        self.redo.insert(rel_path.clone(), Vec::new());
        self.materials.insert(rel_path, material);
    }

    /// Returns the currently active material, if any.
    pub fn active_material(&self) -> Option<&MaterialAsset> {
        self.active.as_ref().and_then(|k| self.materials.get(k))
    }

    /// Returns the currently active material mutably, if any.
    pub fn active_material_mut(&mut self) -> Option<&mut MaterialAsset> {
        if let Some(key) = self.active.clone() {
            self.materials.get_mut(&key)
        } else {
            None
        }
    }

    /// Inspects the active Material through the shared typed-document service.
    pub fn structured_inspect(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<Option<TypedDocumentAuthoringSnapshot<MaterialAsset>>, TypedDocumentAuthoringError> {
        let Some(key) = self.active.as_ref() else {
            return Ok(None);
        };
        let Some(document) = self.materials.get(key) else {
            return Ok(None);
        };
        let Some(state) = self.authoring.get(key) else {
            return Ok(None);
        };
        TypedDocumentAuthoringService::new()
            .inspect(document, state, permissions)
            .map(Some)
    }

    /// Validates the active Material through the shared typed-document service.
    pub fn structured_validate(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<Option<TypedDocumentAuthoringValidation>, TypedDocumentAuthoringError> {
        let Some(key) = self.active.as_ref() else {
            return Ok(None);
        };
        let Some(document) = self.materials.get(key) else {
            return Ok(None);
        };
        let Some(state) = self.authoring.get(key) else {
            return Ok(None);
        };
        TypedDocumentAuthoringService::new()
            .validate(document, state, permissions)
            .map(Some)
    }

    /// Previews replacing the active Material without mutation.
    pub fn structured_preview(
        &self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        replacement: MaterialAsset,
    ) -> Result<Option<TypedDocumentAuthoringMutation<MaterialAsset>>, TypedDocumentAuthoringError> {
        let Some(key) = self.active.as_ref() else {
            return Ok(None);
        };
        let Some(document) = self.materials.get(key) else {
            return Ok(None);
        };
        let Some(state) = self.authoring.get(key) else {
            return Ok(None);
        };
        TypedDocumentAuthoringService::new()
            .preview(
                document,
                state,
                permissions,
                expected_revision,
                expected_generation,
                replacement,
            )
            .map(Some)
    }

    /// Applies one atomic replacement to the active Material as one undoable edit.
    pub fn structured_apply(
        &mut self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        replacement: MaterialAsset,
    ) -> Result<Option<TypedDocumentAuthoringMutation<MaterialAsset>>, TypedDocumentAuthoringError> {
        let Some(key) = self.active.clone() else {
            return Ok(None);
        };
        let Some(before) = self.materials.get(&key).cloned() else {
            return Ok(None);
        };
        let mutation = {
            let (Some(document), Some(state)) =
                (self.materials.get_mut(&key), self.authoring.get_mut(&key))
            else {
                return Ok(None);
            };
            TypedDocumentAuthoringService::new().apply(
                document,
                state,
                permissions,
                expected_revision,
                expected_generation,
                replacement,
            )?
        };
        if mutation.success && !mutation.diff.is_empty() {
            self.push_undo_snapshot(&key, before);
            self.redo.entry(key).or_default().clear();
        }
        Ok(Some(mutation))
    }

    /// Returns whether the active Material has an undo entry.
    pub fn can_undo(&self) -> bool {
        self.active
            .as_ref()
            .and_then(|key| self.undo.get(key))
            .is_some_and(|history| !history.is_empty())
    }

    /// Returns whether the active Material has a redo entry.
    pub fn can_redo(&self) -> bool {
        self.active
            .as_ref()
            .and_then(|key| self.redo.get(key))
            .is_some_and(|history| !history.is_empty())
    }

    /// Restores the previous Material snapshot through the shared semantic boundary.
    pub fn undo(&mut self) -> bool {
        let Some(key) = self.active.clone() else {
            return false;
        };
        let Some(previous) = self.undo.get_mut(&key).and_then(Vec::pop) else {
            return false;
        };
        let Some(current) = self.materials.get(&key).cloned() else {
            self.undo.entry(key).or_default().push(previous);
            return false;
        };
        match self.apply_history_replacement(&key, previous.clone()) {
            Ok(true) => {
                self.redo.entry(key).or_default().push(current);
                true
            }
            Ok(false) | Err(_) => {
                self.undo.entry(key).or_default().push(previous);
                false
            }
        }
    }

    /// Reapplies the next Material snapshot through the shared semantic boundary.
    pub fn redo(&mut self) -> bool {
        let Some(key) = self.active.clone() else {
            return false;
        };
        let Some(next) = self.redo.get_mut(&key).and_then(Vec::pop) else {
            return false;
        };
        let Some(current) = self.materials.get(&key).cloned() else {
            self.redo.entry(key).or_default().push(next);
            return false;
        };
        match self.apply_history_replacement(&key, next.clone()) {
            Ok(true) => {
                self.push_undo_snapshot(&key, current);
                true
            }
            Ok(false) | Err(_) => {
                self.redo.entry(key).or_default().push(next);
                false
            }
        }
    }

    fn apply_history_replacement(
        &mut self,
        key: &Path,
        replacement: MaterialAsset,
    ) -> Result<bool, TypedDocumentAuthoringError> {
        let (Some(document), Some(state)) =
            (self.materials.get_mut(key), self.authoring.get_mut(key))
        else {
            return Ok(false);
        };
        let revision = state.revision();
        let generation = state.generation();
        let permissions = AuthoringPermissions::read_only()
            .with(AuthoringPermission::ProjectDataWrite);
        let mutation = TypedDocumentAuthoringService::new().apply(
            document,
            state,
            &permissions,
            revision,
            generation,
            replacement,
        )?;
        Ok(mutation.success && !mutation.diff.is_empty())
    }

    fn push_undo_snapshot(&mut self, key: &Path, snapshot: MaterialAsset) {
        let history = self.undo.entry(key.to_path_buf()).or_default();
        if history.len() >= UNDO_LIMIT {
            history.remove(0);
        }
        history.push(snapshot);
    }
}


// ---------------------------------------------------------------------------
// egui rendering
// ---------------------------------------------------------------------------

/// Renders the material editor panel inside `ui`.
///
/// Returns `true` when the user made a change that should be saved.
pub fn show_material_editor_panel(
    panel: &mut MaterialEditorPanel,
    ui: &mut eframe::egui::Ui,
    texture_choices: &[(AssetId, String)],
) -> bool {
    let mut changed = false;

    let before = panel.active_material().cloned();
    let Some(mat) = panel.active_material_mut() else {
        ui.label("No material selected.");
        return false;
    };

    ui.heading("Material");

    // Base color picker
    ui.horizontal(|ui| {
        ui.label("Base color");
        let mut color = [
            mat.base_color.r,
            mat.base_color.g,
            mat.base_color.b,
            mat.base_color.a,
        ];
        if ui.color_edit_button_rgba_unmultiplied(&mut color).changed() {
            mat.base_color = LinearRgba {
                r: color[0],
                g: color[1],
                b: color[2],
                a: color[3],
            };
            changed = true;
        }
    });

    // Roughness slider
    ui.horizontal(|ui| {
        ui.label("Roughness");
        if ui
            .add(eframe::egui::Slider::new(&mut mat.roughness, 0.0..=1.0))
            .changed()
        {
            changed = true;
        }
    });

    // Metallic slider
    ui.horizontal(|ui| {
        ui.label("Metallic");
        if ui
            .add(eframe::egui::Slider::new(&mut mat.metallic, 0.0..=1.0))
            .changed()
        {
            changed = true;
        }
    });

    changed |= texture_picker(
        ui,
        "Base color texture",
        &mut mat.base_color_texture,
        texture_choices,
    );
    changed |= texture_picker(
        ui,
        "Normal texture",
        &mut mat.normal_texture,
        texture_choices,
    );
    changed |= texture_picker(
        ui,
        "Emissive texture",
        &mut mat.emissive_texture,
        texture_choices,
    );

    ui.horizontal(|ui| {
        ui.label("Emissive color");
        let mut rgb = [
            mat.emissive_color.r,
            mat.emissive_color.g,
            mat.emissive_color.b,
        ];
        if ui.color_edit_button_rgb(&mut rgb).changed() {
            mat.emissive_color = LinearRgba {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 1.0,
            };
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("Alpha mode");
        eframe::egui::ComboBox::from_id_salt("material_alpha_mode")
            .selected_text(format!("{:?}", mat.alpha_mode))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut mat.alpha_mode, MaterialAlphaMode::Opaque, "Opaque")
                    .changed();
                changed |= ui
                    .selectable_value(&mut mat.alpha_mode, MaterialAlphaMode::Mask, "Mask")
                    .changed();
                changed |= ui
                    .selectable_value(&mut mat.alpha_mode, MaterialAlphaMode::Blend, "Blend")
                    .changed();
            });
    });
    if mat.alpha_mode == MaterialAlphaMode::Mask {
        ui.horizontal(|ui| {
            ui.label("Alpha cutoff");
            changed |= ui
                .add(eframe::egui::Slider::new(&mut mat.alpha_cutoff, 0.0..=1.0))
                .changed();
        });
    }

    ui.horizontal(|ui| {
        ui.label("Cull mode");
        eframe::egui::ComboBox::from_id_salt("material_cull_mode")
            .selected_text(format!("{:?}", mat.cull_mode))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut mat.cull_mode, MaterialCullMode::Back, "Back")
                    .changed();
                changed |= ui
                    .selectable_value(&mut mat.cull_mode, MaterialCullMode::Front, "Front")
                    .changed();
                changed |= ui
                    .selectable_value(&mut mat.cull_mode, MaterialCullMode::None, "None")
                    .changed();
            });
    });

    ui.horizontal(|ui| {
        ui.label("Shading");
        eframe::egui::ComboBox::from_id_salt("material_shading_model")
            .selected_text(format!("{:?}", mat.shading_model))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(
                        &mut mat.shading_model,
                        MaterialShadingModel::StandardLit,
                        "Standard Lit",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut mat.shading_model,
                        MaterialShadingModel::ToonLit,
                        "Toon Lit",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(&mut mat.shading_model, MaterialShadingModel::Unlit, "Unlit")
                    .changed();
            });
    });

    if mat.shading_model == MaterialShadingModel::ToonLit {
        changed |= texture_picker(ui, "Toon ramp", &mut mat.toon.ramp_texture, texture_choices);
        changed |= texture_picker(ui, "Sphere map", &mut mat.toon.sphere_texture, texture_choices);
        ui.horizontal(|ui| {
            ui.label("Specular power");
            changed |= ui.add(eframe::egui::DragValue::new(&mut mat.toon.specular_power).range(0.0..=256.0)).changed();
        });
        ui.horizontal(|ui| {
            ui.label("Rim intensity");
            changed |= ui.add(eframe::egui::Slider::new(&mut mat.toon.rim_intensity, 0.0..=4.0)).changed();
        });
    }

    changed |= ui.checkbox(&mut mat.cast_shadow, "Cast shadow").changed();
    changed |= ui.checkbox(&mut mat.receive_shadow, "Receive shadow").changed();
    changed |= ui.checkbox(&mut mat.outline.enabled, "Outline").changed();
    if mat.outline.enabled {
        ui.horizontal(|ui| {
            ui.label("Outline width");
            changed |= ui.add(eframe::egui::DragValue::new(&mut mat.outline.width).speed(0.001).range(0.0..=1.0)).changed();
        });
    }

    if let Err(error) = mat.validate() {
        ui.colored_label(eframe::egui::Color32::RED, error.to_string());
    }

    if changed {
        let replacement = mat.clone();
        if let Some(before) = before {
            *mat = before;
        }
        let permissions = AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite);
        if let Ok(Some(base)) = panel.structured_inspect(&permissions) {
            return panel
                .structured_apply(
                    &permissions,
                    base.revision,
                    base.generation,
                    replacement,
                )
                .ok()
                .flatten()
                .is_some_and(|mutation| mutation.success && !mutation.diff.is_empty());
        }
        return false;
    }

    false
}

fn texture_picker(
    ui: &mut eframe::egui::Ui,
    label: &str,
    selected: &mut Option<AssetId>,
    choices: &[(AssetId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let selected_text = selected
            .as_ref()
            .and_then(|id| choices.iter().find(|(candidate, _)| candidate == id))
            .map(|(_, label)| label.as_str())
            .or_else(|| selected.as_ref().map(AssetId::as_str))
            .unwrap_or("(none)");
        eframe::egui::ComboBox::from_id_salt(label)
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                changed |= ui.selectable_value(selected, None, "(none)").changed();
                for (id, choice_label) in choices {
                    changed |= ui
                        .selectable_value(selected, Some(id.clone()), choice_label)
                        .changed();
                }
            });
    });
    changed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_material_sets_active() {
        let mut panel = MaterialEditorPanel::new();
        let mat = MaterialAsset::default();
        panel.open_material(PathBuf::from("materials/hero.material.json"), mat.clone());
        assert_eq!(
            panel.active,
            Some(PathBuf::from("materials/hero.material.json"))
        );
        assert!(panel.active_material().is_some());
    }

    #[test]
    fn active_material_mut_allows_editing() {
        let mut panel = MaterialEditorPanel::new();
        panel.open_material(
            PathBuf::from("materials/m.material.json"),
            MaterialAsset::default(),
        );
        if let Some(mat) = panel.active_material_mut() {
            mat.roughness = 0.9;
        }
        assert!((panel.active_material().unwrap().roughness - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn panel_empty_by_default_returns_no_active_material() {
        let panel = MaterialEditorPanel::new();
        assert!(panel.active_material().is_none());
    }
}
