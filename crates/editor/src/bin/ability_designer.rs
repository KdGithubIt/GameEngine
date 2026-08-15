use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use engine::ability::{AbilityDefinition, AbilityLibrary, AbilityPhase};

struct AbilityDesigner {
    library: AbilityLibrary,
    selected: Option<usize>,
    path: Option<PathBuf>,
    dirty: bool,
    status: String,
    preview_seconds: f32,
}

impl Default for AbilityDesigner {
    fn default() -> Self {
        Self {
            library: AbilityLibrary::default(),
            selected: Some(0),
            path: None,
            dirty: false,
            status: "Ready".to_owned(),
            preview_seconds: 0.0,
        }
    }
}

impl AbilityDesigner {
    fn new_document(&mut self) {
        self.library = AbilityLibrary::default();
        self.selected = Some(0);
        self.path = None;
        self.dirty = false;
        self.preview_seconds = 0.0;
        self.status = "Created a new ability library".to_owned();
    }

    fn open_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Ability Library", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                AbilityLibrary::from_json_str(&json).map_err(|error| error.to_string())
            }) {
            Ok(library) => {
                self.library = library;
                self.selected = (!self.library.abilities.is_empty()).then_some(0);
                self.path = Some(path.clone());
                self.dirty = false;
                self.preview_seconds = 0.0;
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
            .add_filter("Ability Library", &["json"])
            .set_file_name("abilities.ability.json");
        if let Some(path) = &self.path
            && let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
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

    fn add_ability(&mut self) {
        let mut ability = AbilityDefinition::default();
        let mut suffix = self.library.abilities.len() + 1;
        while self
            .library
            .abilities
            .iter()
            .any(|current| current.id == ability.id)
        {
            ability.id = format!("new_ability_{suffix}");
            suffix += 1;
        }
        self.library.abilities.push(ability);
        self.selected = Some(self.library.abilities.len() - 1);
        self.dirty = true;
        self.preview_seconds = 0.0;
    }

    fn duplicate_selected(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(mut copy) = self.library.abilities.get(index).cloned() else {
            return;
        };
        let base = format!("{}_copy", copy.id);
        copy.id = base.clone();
        let mut suffix = 2;
        while self
            .library
            .abilities
            .iter()
            .any(|current| current.id == copy.id)
        {
            copy.id = format!("{base}_{suffix}");
            suffix += 1;
        }
        self.library.abilities.insert(index + 1, copy);
        self.selected = Some(index + 1);
        self.dirty = true;
        self.preview_seconds = 0.0;
    }

    fn delete_selected(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        if index >= self.library.abilities.len() {
            return;
        }
        self.library.abilities.remove(index);
        self.selected = if self.library.abilities.is_empty() {
            None
        } else {
            Some(index.min(self.library.abilities.len() - 1))
        };
        self.dirty = true;
        self.preview_seconds = 0.0;
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
            let path = self
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Unsaved library".to_owned());
            ui.label(path);
            if self.dirty {
                ui.strong("Modified");
            }
        });
    }

    fn ability_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Abilities");
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                self.add_ability();
            }
            if ui
                .add_enabled(self.selected.is_some(), egui::Button::new("Duplicate"))
                .clicked()
            {
                self.duplicate_selected();
            }
            if ui
                .add_enabled(self.selected.is_some(), egui::Button::new("Delete"))
                .clicked()
            {
                self.delete_selected();
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, ability) in self.library.abilities.iter().enumerate() {
                if ui
                    .selectable_label(self.selected == Some(index), &ability.id)
                    .clicked()
                {
                    self.selected = Some(index);
                    self.preview_seconds = 0.0;
                }
            }
        });
    }

    fn ability_editor(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.selected else {
            ui.centered_and_justified(|ui| {
                ui.label("Add or select an ability to edit it.");
            });
            return;
        };
        let Some(ability) = self.library.abilities.get_mut(index) else {
            self.selected = None;
            return;
        };

        ui.heading("Ability Designer");
        ui.label("Edit timings, validate the asset, and scrub the full activation timeline.");
        ui.separator();

        let mut changed = false;
        egui::Grid::new("ability_fields")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("Stable ID");
                changed |= ui.text_edit_singleline(&mut ability.id).changed();
                ui.end_row();

                changed |= duration_row(ui, "Startup", &mut ability.startup_seconds);
                changed |= duration_row(ui, "Active", &mut ability.active_seconds);
                changed |= duration_row(ui, "Recovery", &mut ability.recovery_seconds);
                changed |= duration_row(ui, "Cooldown", &mut ability.cooldown_seconds);
            });
        if changed {
            self.dirty = true;
        }

        let preview = ability.clone();

        ui.separator();
        match preview.validate() {
            Ok(()) => {
                ui.label("Validation: valid");
            }
            Err(error) => {
                ui.label(format!("Validation: {error}"));
            }
        }
        if let Err(error) = self.library.validate() {
            ui.label(format!("Library: {error}"));
        }

        ui.separator();
        ui.heading("Timeline Preview");
        let total = preview.total_seconds();
        let preview_max = total.max(0.01);
        self.preview_seconds = self.preview_seconds.clamp(0.0, preview_max);
        ui.add(
            egui::Slider::new(&mut self.preview_seconds, 0.0..=preview_max)
                .text("Seconds")
                .show_value(true),
        );
        let phase = preview.phase_at(self.preview_seconds);
        ui.strong(format!("Current phase: {phase:?}"));
        ui.label(format!("Total duration: {total:.3} seconds"));
        ui.add_space(4.0);
        timeline_row(
            ui,
            "Startup",
            preview.startup_seconds,
            total,
            phase == AbilityPhase::Startup,
        );
        timeline_row(
            ui,
            "Active",
            preview.active_seconds,
            total,
            phase == AbilityPhase::Active,
        );
        timeline_row(
            ui,
            "Recovery",
            preview.recovery_seconds,
            total,
            phase == AbilityPhase::Recovery,
        );
        timeline_row(
            ui,
            "Cooldown",
            preview.cooldown_seconds,
            total,
            phase == AbilityPhase::Cooldown,
        );
    }
}

impl eframe::App for AbilityDesigner {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("ability_toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("ability_status")
            .exact_size(28.0)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&self.status);
                });
            });
        egui::Panel::left("ability_list")
            .resizable(true)
            .default_size(260.0)
            .show_inside(ui, |ui| self.ability_list(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.ability_editor(ui));
        });
    }
}

fn duration_row(ui: &mut egui::Ui, label: &str, value: &mut f32) -> bool {
    ui.label(format!("{label} (s)"));
    let changed = ui
        .add(
            egui::DragValue::new(value)
                .speed(0.01)
                .range(0.0..=600.0)
                .fixed_decimals(3),
        )
        .changed();
    ui.end_row();
    changed
}

fn timeline_row(ui: &mut egui::Ui, label: &str, duration: f32, total: f32, active: bool) {
    let fraction = if total > 0.0 {
        (duration / total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ui.horizontal(|ui| {
        if active {
            ui.strong(format!("{label}: {duration:.3}s"));
        } else {
            ui.label(format!("{label}: {duration:.3}s"));
        }
        ui.add(egui::ProgressBar::new(fraction).desired_width(260.0));
    });
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([820.0, 560.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Ability Designer",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(AbilityDesigner::default()))
        }),
    )
}
