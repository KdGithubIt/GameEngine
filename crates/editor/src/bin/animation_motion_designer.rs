use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use engine::animation_parameters::{
    AnimationMotionLibrary, AnimationParameterDeclaration, AnimationParameterKind,
    AnimationParameterValue, Blend1dDefinition, Blend1dPoint,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DesignerTab {
    Parameters,
    Blend1d,
}

struct AnimationMotionDesigner {
    library: AnimationMotionLibrary,
    tab: DesignerTab,
    selected_blend: Option<usize>,
    path: Option<PathBuf>,
    dirty: bool,
    status: String,
    sample_value: f32,
}

impl Default for AnimationMotionDesigner {
    fn default() -> Self {
        Self {
            library: AnimationMotionLibrary::default(),
            tab: DesignerTab::Parameters,
            selected_blend: Some(0),
            path: None,
            dirty: false,
            status: "Ready".to_owned(),
            sample_value: 0.0,
        }
    }
}

impl AnimationMotionDesigner {
    fn new_document(&mut self) {
        *self = Self::default();
        self.status = "Created a new animation motion library".to_owned();
    }

    fn open_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Animation Motion Library", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                AnimationMotionLibrary::from_json_str(&json).map_err(|error| error.to_string())
            }) {
            Ok(library) => {
                self.library = library;
                self.selected_blend = (!self.library.blends.is_empty()).then_some(0);
                self.path = Some(path.clone());
                self.dirty = false;
                self.sample_value = 0.0;
                self.status = format!("Opened {}", path.display());
            }
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.save_as();
            return;
        };
        self.save_to(&path);
    }

    fn save_as(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Animation Motion Library", &["json"])
            .set_file_name("character.animation-motion.json");
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        self.save_to(&path);
    }

    fn save_to(&mut self, path: &Path) {
        match self
            .library
            .to_json_string()
            .map_err(|error| error.to_string())
            .and_then(|json| fs::write(path, json).map_err(|error| error.to_string()))
        {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("New").clicked() {
                self.new_document();
            }
            if ui.button("Open...").clicked() {
                self.open_dialog();
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("Save As...").clicked() {
                self.save_as();
            }
            ui.separator();
            if ui
                .selectable_label(self.tab == DesignerTab::Parameters, "Parameters")
                .clicked()
            {
                self.tab = DesignerTab::Parameters;
            }
            if ui
                .selectable_label(self.tab == DesignerTab::Blend1d, "Blend1D")
                .clicked()
            {
                self.tab = DesignerTab::Blend1d;
            }
            ui.separator();
            ui.label(
                self.path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unsaved library".to_owned()),
            );
            if self.dirty {
                ui.strong("Modified");
            }
        });
    }

    fn parameters_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Animation Parameters");
        ui.label("Declare stable Bool, Float, and one-shot Trigger values used by graphs.");
        if ui.button("Add Parameter").clicked() {
            let name = unique_name(
                "parameter",
                self.library
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.as_str()),
            );
            self.library.parameters.push(AnimationParameterDeclaration {
                name,
                default: AnimationParameterValue::Bool(false),
            });
            self.dirty = true;
        }
        ui.separator();

        let mut remove = None;
        egui::Grid::new("animation_parameter_grid")
            .num_columns(5)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Name");
                ui.strong("Kind");
                ui.strong("Default");
                ui.strong("Used by Blend1D");
                ui.label("");
                ui.end_row();

                for (index, declaration) in self.library.parameters.iter_mut().enumerate() {
                    if ui.text_edit_singleline(&mut declaration.name).changed() {
                        self.dirty = true;
                    }

                    let current_kind = declaration.default.kind();
                    let mut requested_kind = current_kind;
                    egui::ComboBox::from_id_salt(("parameter_kind", index))
                        .selected_text(kind_label(current_kind))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut requested_kind,
                                AnimationParameterKind::Bool,
                                "Bool",
                            );
                            ui.selectable_value(
                                &mut requested_kind,
                                AnimationParameterKind::Float,
                                "Float",
                            );
                            ui.selectable_value(
                                &mut requested_kind,
                                AnimationParameterKind::Trigger,
                                "Trigger",
                            );
                        });
                    if requested_kind != current_kind {
                        declaration.default = default_for_kind(requested_kind);
                        self.dirty = true;
                    }

                    match &mut declaration.default {
                        AnimationParameterValue::Bool(value) => {
                            if ui.checkbox(value, "").changed() {
                                self.dirty = true;
                            }
                        }
                        AnimationParameterValue::Float(value) => {
                            if ui.add(egui::DragValue::new(value).speed(0.05)).changed() {
                                self.dirty = true;
                            }
                        }
                        AnimationParameterValue::Trigger(value) => {
                            *value = false;
                            ui.weak("Unconsumed");
                        }
                    }

                    let usage = self
                        .library
                        .blends
                        .iter()
                        .filter(|blend| blend.parameter == declaration.name)
                        .count();
                    ui.label(usage.to_string());
                    if ui.small_button("Delete").clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            let removed_name = self.library.parameters[index].name.clone();
            self.library.parameters.remove(index);
            for blend in &mut self.library.blends {
                if blend.parameter == removed_name {
                    blend.parameter.clear();
                }
            }
            self.dirty = true;
        }

        ui.separator();
        validation_label(ui, &self.library);
    }

    fn blends_ui(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |columns| {
            let list_ui = &mut columns[0];
            list_ui.heading("Blend1D Definitions");
            list_ui.horizontal(|ui| {
                if ui.button("Add").clicked() {
                    let id = unique_name(
                        "blend",
                        self.library.blends.iter().map(|blend| blend.id.as_str()),
                    );
                    let parameter = self
                        .library
                        .parameters
                        .iter()
                        .find(|parameter| parameter.default.kind() == AnimationParameterKind::Float)
                        .map(|parameter| parameter.name.clone())
                        .unwrap_or_default();
                    self.library.blends.push(Blend1dDefinition {
                        id,
                        parameter,
                        points: vec![Blend1dPoint {
                            threshold: 0.0,
                            motion: "motion".to_owned(),
                        }],
                    });
                    self.selected_blend = Some(self.library.blends.len() - 1);
                    self.dirty = true;
                }
                let can_delete = self.selected_blend.is_some();
                if ui
                    .add_enabled(can_delete, egui::Button::new("Delete"))
                    .clicked()
                {
                    if let Some(index) = self.selected_blend {
                        self.library.blends.remove(index);
                        self.selected_blend = if self.library.blends.is_empty() {
                            None
                        } else {
                            Some(index.min(self.library.blends.len() - 1))
                        };
                        self.dirty = true;
                    }
                }
            });
            list_ui.separator();
            for (index, blend) in self.library.blends.iter().enumerate() {
                if list_ui
                    .selectable_label(self.selected_blend == Some(index), &blend.id)
                    .clicked()
                {
                    self.selected_blend = Some(index);
                    self.sample_value = 0.0;
                }
            }

            let editor_ui = &mut columns[1];
            let Some(index) = self.selected_blend else {
                editor_ui.centered_and_justified(|ui| {
                    ui.label("Add or select a Blend1D definition.");
                });
                return;
            };
            let float_parameters = self
                .library
                .parameters
                .iter()
                .filter(|parameter| parameter.default.kind() == AnimationParameterKind::Float)
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            let Some(blend) = self.library.blends.get_mut(index) else {
                self.selected_blend = None;
                return;
            };

            editor_ui.heading("Blend1D Editor");
            egui::Grid::new("blend_fields")
                .num_columns(2)
                .striped(true)
                .show(editor_ui, |ui| {
                    ui.label("Stable ID");
                    if ui.text_edit_singleline(&mut blend.id).changed() {
                        self.dirty = true;
                    }
                    ui.end_row();

                    ui.label("Float parameter");
                    egui::ComboBox::from_id_salt("blend_parameter")
                        .selected_text(if blend.parameter.is_empty() {
                            "Select parameter"
                        } else {
                            &blend.parameter
                        })
                        .show_ui(ui, |ui| {
                            for parameter in &float_parameters {
                                if ui
                                    .selectable_label(blend.parameter == *parameter, parameter)
                                    .clicked()
                                {
                                    blend.parameter = parameter.clone();
                                    self.dirty = true;
                                }
                            }
                        });
                    ui.end_row();
                });

            editor_ui.separator();
            editor_ui.horizontal(|ui| {
                ui.heading("Thresholds");
                if ui.button("Add Point").clicked() {
                    let threshold = blend
                        .points
                        .iter()
                        .map(|point| point.threshold)
                        .max_by(f32::total_cmp)
                        .unwrap_or(-1.0)
                        + 1.0;
                    blend.points.push(Blend1dPoint {
                        threshold,
                        motion: "motion".to_owned(),
                    });
                    self.dirty = true;
                }
            });
            let mut remove_point = None;
            egui::Grid::new("blend_points")
                .num_columns(3)
                .striped(true)
                .show(editor_ui, |ui| {
                    ui.strong("Threshold");
                    ui.strong("Motion Slot / ID");
                    ui.label("");
                    ui.end_row();
                    for (point_index, point) in blend.points.iter_mut().enumerate() {
                        if ui
                            .add(egui::DragValue::new(&mut point.threshold).speed(0.05))
                            .changed()
                        {
                            self.dirty = true;
                        }
                        if ui.text_edit_singleline(&mut point.motion).changed() {
                            self.dirty = true;
                        }
                        if ui.small_button("Delete").clicked() {
                            remove_point = Some(point_index);
                        }
                        ui.end_row();
                    }
                });
            if let Some(point_index) = remove_point {
                blend.points.remove(point_index);
                self.dirty = true;
            }

            editor_ui.separator();
            editor_ui.heading("Live Sample");
            let threshold_range = blend
                .points
                .iter()
                .map(|point| point.threshold)
                .fold(None::<(f32, f32)>, |range, threshold| {
                    Some(match range {
                        Some((minimum, maximum)) => {
                            (minimum.min(threshold), maximum.max(threshold))
                        }
                        None => (threshold, threshold),
                    })
                })
                .unwrap_or((-1.0, 1.0));
            let sample_min = threshold_range.0.min(0.0) - 1.0;
            let sample_max = threshold_range.1.max(0.0) + 1.0;
            editor_ui.add(
                egui::Slider::new(&mut self.sample_value, sample_min..=sample_max)
                    .text("Parameter value"),
            );
            match blend.build() {
                Ok(runtime) => {
                    let sample = runtime.sample(self.sample_value);
                    editor_ui.label(format!(
                        "{}: {:.1}%",
                        sample.lower.motion,
                        sample.lower_weight * 100.0
                    ));
                    if let Some(upper) = sample.upper {
                        editor_ui.label(format!(
                            "{}: {:.1}%",
                            upper.motion,
                            sample.upper_weight * 100.0
                        ));
                    }
                }
                Err(error) => {
                    editor_ui.label(format!("Cannot sample: {error}"));
                }
            }
            editor_ui.separator();
            validation_label(editor_ui, &self.library);
        });
    }
}

impl eframe::App for AnimationMotionDesigner {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("motion_toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("motion_status")
            .exact_size(28.0)
            .show_inside(ui, |ui| {
                ui.label(&self.status);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.tab {
                DesignerTab::Parameters => self.parameters_ui(ui),
                DesignerTab::Blend1d => self.blends_ui(ui),
            });
        });
    }
}

fn kind_label(kind: AnimationParameterKind) -> &'static str {
    match kind {
        AnimationParameterKind::Bool => "Bool",
        AnimationParameterKind::Float => "Float",
        AnimationParameterKind::Trigger => "Trigger",
    }
}

fn default_for_kind(kind: AnimationParameterKind) -> AnimationParameterValue {
    match kind {
        AnimationParameterKind::Bool => AnimationParameterValue::Bool(false),
        AnimationParameterKind::Float => AnimationParameterValue::Float(0.0),
        AnimationParameterKind::Trigger => AnimationParameterValue::Trigger(false),
    }
}

fn unique_name<'a>(base: &str, names: impl Iterator<Item = &'a str>) -> String {
    let existing = names.collect::<std::collections::BTreeSet<_>>();
    if !existing.contains(base) {
        return base.to_owned();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn validation_label(ui: &mut egui::Ui, library: &AnimationMotionLibrary) {
    match library.validate() {
        Ok(()) => {
            ui.label("Validation: valid");
        }
        Err(error) => {
            ui.label(format!("Validation: {error}"));
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([920.0, 620.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Animation Motion Designer",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(AnimationMotionDesigner::default()))
        }),
    )
}
