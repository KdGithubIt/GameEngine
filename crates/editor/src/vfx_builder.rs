//! Typed visual authoring surface for `*.vfx.json` effect assets.
//!
//! The window translates gestures into [`VfxCommand`] values and applies them
//! through [`VfxAuthoringService`]. It never edits raw JSON as business logic.

#[path = "vfx_builder_background.rs"]
mod background;
#[path = "vfx_builder_completion.rs"]
mod completion;

use eframe::egui;
use engine_authoring::{
    ProjectRoot, VfxAuthoringService, VfxCommand, VfxCurve, VfxEmitter, VfxEmitterId, VfxEffect,
    VfxGradient, VfxModule, VfxModuleOperation, VfxPhase, VfxScalarValue, VfxShape,
    VfxVectorValue,
};
use std::path::{Path, PathBuf};

const MAX_HISTORY: usize = 100;

/// Transient state owned by the modeless VFX Builder window.
#[derive(Default)]
pub(crate) struct VfxBuilderState {
    effect: Option<VfxEffect>,
    path: Option<PathBuf>,
    selected_emitter: Option<VfxEmitterId>,
    completion: completion::VfxCompletionState,
    module_filter: String,
    emitter_name: String,
    emitter_max_particles: u32,
    undo_stack: Vec<Vec<VfxCommand>>,
    redo_stack: Vec<Vec<VfxCommand>>,
    status: Option<String>,
    background: background::VfxBackgroundTasks,
}

impl VfxBuilderState {
    /// Draws the VFX Builder for the current project.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, project: &ProjectRoot) {
        self.poll_background();
        let io_busy = self.background.io_busy();
        let mut pending_command = None;
        let mut request_open = false;
        let mut request_save = false;
        let mut request_undo = false;
        let mut request_redo = false;

        ui.horizontal(|ui| {
            request_open = ui
                .add_enabled(!io_busy, egui::Button::new("Open VFX..."))
                .clicked();
            request_save = ui
                .add_enabled(self.effect.is_some() && !io_busy, egui::Button::new("Save"))
                .clicked();
            ui.separator();
            request_undo = ui
                .add_enabled(!self.undo_stack.is_empty(), egui::Button::new("Undo"))
                .clicked();
            request_redo = ui
                .add_enabled(!self.redo_stack.is_empty(), egui::Button::new("Redo"))
                .clicked();
            if let Some(path) = &self.path {
                ui.separator();
                ui.monospace(project_relative_label(project, path));
            }
        });

        if request_open {
            self.open_with_picker(project, ui.ctx());
        }
        if request_save {
            self.save(ui.ctx());
        }
        if request_undo {
            self.undo();
        }
        if request_redo {
            self.redo();
        }

        let Some(effect) = self.effect.clone() else {
            ui.separator();
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.heading("VFX Builder");
                ui.label("Open a project-local *.vfx.json asset to begin authoring.");
                ui.small("All edits use the shared typed VFX authoring service.");
                self.show_completion_empty(ui, project);
            });
            self.show_status(ui);
            return;
        };

        self.normalize_selection(&effect);
        let compilation = self.background.compilation_for(&effect, ui.ctx());

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.heading(&effect.name);
            ui.label(format!("Seed {}", effect.seed));
            ui.label(format!("Effect cap {}", effect.max_particles));
            let estimated = compilation
                .as_ref()
                .and_then(|compilation| compilation.compiled_effect.as_ref())
                .map(|compiled| {
                    compiled
                        .emitters
                        .iter()
                        .fold(0_u32, |total, emitter| {
                            total.saturating_add(emitter.estimated_capacity)
                        })
                })
                .unwrap_or(0);
            ui.label(format!("Estimated live {estimated}"));
        });

        ui.columns(2, |columns| {
            columns[0].set_min_width(270.0);
            columns[0].heading("Emitters");
            if columns[0].button("+ Add Emitter").clicked() {
                pending_command = Some(VfxCommand::AddEmitter {
                    emitter: VfxEmitter::new("Emitter", 512),
                    index: effect.emitters.len(),
                });
            }
            columns[0].separator();

            egui::ScrollArea::vertical()
                .id_salt("vfx_emitter_list")
                .show(&mut columns[0], |ui| {
                    for (index, emitter) in effect.emitters.iter().enumerate() {
                        let selected = self.selected_emitter.as_ref() == Some(&emitter.id);
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, &emitter.name).clicked() {
                                self.select_emitter(emitter);
                            }
                            let mut enabled = emitter.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                pending_command = Some(VfxCommand::SetEmitterEnabled {
                                    emitter: emitter.id.clone(),
                                    enabled,
                                });
                            }
                        });
                        if selected {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(index > 0, egui::Button::new("Up"))
                                    .clicked()
                                {
                                    pending_command = Some(VfxCommand::MoveEmitter {
                                        emitter: emitter.id.clone(),
                                        index: index - 1,
                                    });
                                }
                                if ui
                                    .add_enabled(
                                        index + 1 < effect.emitters.len(),
                                        egui::Button::new("Down"),
                                    )
                                    .clicked()
                                {
                                    pending_command = Some(VfxCommand::MoveEmitter {
                                        emitter: emitter.id.clone(),
                                        index: index + 1,
                                    });
                                }
                                if ui.button("Duplicate").clicked() {
                                    pending_command = Some(VfxCommand::AddEmitter {
                                        emitter: duplicate_emitter(emitter),
                                        index: index + 1,
                                    });
                                }
                                if ui.button("Delete").clicked() {
                                    pending_command = Some(VfxCommand::RemoveEmitter {
                                        emitter: emitter.id.clone(),
                                    });
                                }
                            });
                        }
                        ui.add_space(4.0);
                    }
                });

            columns[1].heading("Module Stack");
            let Some(emitter_id) = self.selected_emitter.clone() else {
                columns[1].label("Add an emitter to begin authoring modules.");
                return;
            };
            let Some(emitter) = effect.emitters.iter().find(|entry| entry.id == emitter_id) else {
                columns[1].label("The selected emitter no longer exists.");
                return;
            };

            columns[1].group(|ui| {
                ui.strong("Emitter Properties");
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.emitter_name);
                });
                ui.horizontal(|ui| {
                    ui.label("Max particles");
                    ui.add(egui::DragValue::new(&mut self.emitter_max_particles).range(1..=u32::MAX));
                });
                if ui.button("Apply Emitter Properties").clicked() {
                    let commands = vec![
                        VfxCommand::RenameEmitter {
                            emitter: emitter.id.clone(),
                            name: self.emitter_name.clone(),
                        },
                        VfxCommand::SetEmitterMaxParticles {
                            emitter: emitter.id.clone(),
                            max_particles: self.emitter_max_particles,
                        },
                    ];
                    self.apply_user_commands(commands);
                }
            });

            columns[1].horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.module_filter)
                        .hint_text("Search modules...")
                        .desired_width(240.0),
                );
                show_add_module_menu(ui, emitter, &self.module_filter, &mut pending_command);
            });
            columns[1].separator();

            let display_names = VfxAuthoringService::new()
                .schemas()
                .modules
                .into_iter()
                .map(|schema| (schema.type_id, schema.display_name))
                .collect::<std::collections::BTreeMap<_, _>>();

            egui::ScrollArea::vertical()
                .id_salt("vfx_module_stack")
                .show(&mut columns[1], |ui| {
                    for phase in [VfxPhase::Spawn, VfxPhase::Update, VfxPhase::Render] {
                        ui.strong(phase_label(phase));
                        let modules = emitter
                            .modules
                            .iter()
                            .filter(|module| module.phase == phase)
                            .collect::<Vec<_>>();
                        if modules.is_empty() {
                            ui.small("No modules");
                        }
                        for (phase_index, module) in modules.iter().enumerate() {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let mut enabled = module.enabled;
                                    if ui.checkbox(&mut enabled, "").changed() {
                                        pending_command = Some(VfxCommand::SetModuleEnabled {
                                            emitter: emitter.id.clone(),
                                            module: module.id.clone(),
                                            enabled,
                                        });
                                    }
                                    let label = display_names
                                        .get(module.operation.type_id())
                                        .map(String::as_str)
                                        .unwrap_or_else(|| module.operation.type_id());
                                    let selected = self.completion.is_selected(module);
                                    if ui.selectable_label(selected, label).clicked() {
                                        self.completion.select_module(module);
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Delete").clicked() {
                                                pending_command = Some(VfxCommand::RemoveModule {
                                                    emitter: emitter.id.clone(),
                                                    module: module.id.clone(),
                                                });
                                            }
                                            if ui
                                                .add_enabled(
                                                    phase_index + 1 < modules.len(),
                                                    egui::Button::new("Down"),
                                                )
                                                .clicked()
                                            {
                                                pending_command = Some(VfxCommand::MoveModule {
                                                    emitter: emitter.id.clone(),
                                                    module: module.id.clone(),
                                                    phase_index: phase_index + 1,
                                                });
                                            }
                                            if ui
                                                .add_enabled(
                                                    phase_index > 0,
                                                    egui::Button::new("Up"),
                                                )
                                                .clicked()
                                            {
                                                pending_command = Some(VfxCommand::MoveModule {
                                                    emitter: emitter.id.clone(),
                                                    module: module.id.clone(),
                                                    phase_index: phase_index - 1,
                                                });
                                            }
                                        },
                                    );
                                });
                                ui.small(module_summary(&module.operation));
                            });
                            ui.add_space(4.0);
                        }
                        ui.add_space(8.0);
                    }
                });
            self.show_completion_properties(&mut columns[1], &effect);
        });

        self.show_completion_preview(ui, &effect, compilation.as_ref());

        if let Some(command) = pending_command {
            self.apply_user_commands(vec![command]);
        }

        if let Some(compilation) = compilation
            && !compilation.diagnostics.is_empty()
        {
            ui.separator();
            ui.collapsing("Diagnostics", |ui| {
                for diagnostic in compilation.diagnostics {
                    ui.label(format!("{}: {}", diagnostic.code, diagnostic.message));
                }
            });
        }
        self.show_status(ui);
    }

    fn open_with_picker(&mut self, project: &ProjectRoot, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .set_directory(project.assets_root())
            .add_filter("VFX effect", &["json"])
            .pick_file()
        else {
            return;
        };
        if !is_project_vfx_path(project, &path) {
            self.status = Some("VFX Builder only opens project-local *.vfx.json assets.".to_owned());
            return;
        }
        self.background.open(path, ctx);
        self.status = Some("Opening VFX effect...".to_owned());
    }

    fn save(&mut self, ctx: &egui::Context) {
        let (Some(effect), Some(path)) = (self.effect.as_ref(), self.path.as_ref()) else {
            return;
        };
        let json = match VfxAuthoringService::new().effect_to_canonical_json(effect) {
            Ok(json) => json,
            Err(error) => {
                self.status = Some(format!("Save blocked: {error}"));
                return;
            }
        };
        self.background.save(path.clone(), json, ctx);
        self.status = Some("Saving VFX effect...".to_owned());
    }

    fn poll_background(&mut self) {
        let Some(completion) = self.background.take_io_completion() else {
            return;
        };
        match completion {
            background::VfxIoCompletion::Open { path, result } => match result {
                Ok(effect) => self.install_effect(path, effect, "Opened VFX effect."),
                Err(error) => self.status = Some(format!("Open failed: {error}")),
            },
            background::VfxIoCompletion::Save { result } => match result {
                Ok(()) => self.status = Some("Saved VFX effect.".to_owned()),
                Err(error) => self.status = Some(format!("Save failed: {error}")),
            },
            background::VfxIoCompletion::Create {
                path,
                effect,
                result,
            } => match result {
                Ok(()) => self.install_effect(path, effect, "Created VFX asset from shared template."),
                Err(error) => self.status = Some(format!("Template save failed: {error}")),
            },
        }
    }

    fn install_effect(&mut self, path: PathBuf, effect: VfxEffect, message: &str) {
        self.effect = Some(effect.clone());
        self.path = Some(path);
        self.selected_emitter = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.completion = completion::VfxCompletionState::default();
        self.normalize_selection(&effect);
        self.status = Some(message.to_owned());
    }

    fn apply_user_commands(&mut self, commands: Vec<VfxCommand>) {
        let Some(effect) = self.effect.as_ref() else {
            return;
        };
        let result = VfxAuthoringService::new().apply(effect, &commands);
        if !result.success {
            self.status = Some(first_blocking_message(&result.diagnostics));
            return;
        }
        let Some(effect) = result.effect else {
            self.status = Some("VFX edit produced no committed effect.".to_owned());
            return;
        };
        push_history(&mut self.undo_stack, result.undo_commands);
        self.redo_stack.clear();
        self.effect = Some(effect);
        self.status = Some("VFX edit applied.".to_owned());
        if let Some(effect) = self.effect.clone() {
            self.normalize_selection(&effect);
        }
    }

    fn undo(&mut self) {
        let Some(commands) = self.undo_stack.pop() else {
            return;
        };
        let Some(effect) = self.effect.as_ref() else {
            return;
        };
        let result = VfxAuthoringService::new().apply(effect, &commands);
        if !result.success {
            self.status = Some(first_blocking_message(&result.diagnostics));
            self.undo_stack.push(commands);
            return;
        }
        if let Some(effect) = result.effect {
            self.effect = Some(effect);
            push_history(&mut self.redo_stack, result.undo_commands);
            self.status = Some("Undid VFX edit.".to_owned());
            if let Some(effect) = self.effect.clone() {
                self.normalize_selection(&effect);
            }
            self.sync_selected_emitter();
        }
    }

    fn redo(&mut self) {
        let Some(commands) = self.redo_stack.pop() else {
            return;
        };
        let Some(effect) = self.effect.as_ref() else {
            return;
        };
        let result = VfxAuthoringService::new().apply(effect, &commands);
        if !result.success {
            self.status = Some(first_blocking_message(&result.diagnostics));
            self.redo_stack.push(commands);
            return;
        }
        if let Some(effect) = result.effect {
            self.effect = Some(effect);
            push_history(&mut self.undo_stack, result.undo_commands);
            self.status = Some("Redid VFX edit.".to_owned());
            if let Some(effect) = self.effect.clone() {
                self.normalize_selection(&effect);
            }
            self.sync_selected_emitter();
        }
    }

    fn normalize_selection(&mut self, effect: &VfxEffect) {
        let selection_is_valid = self
            .selected_emitter
            .as_ref()
            .is_some_and(|selected| effect.emitters.iter().any(|emitter| &emitter.id == selected));
        if selection_is_valid {
            return;
        }
        if let Some(emitter) = effect.emitters.first() {
            self.select_emitter(emitter);
        } else {
            self.selected_emitter = None;
            self.emitter_name.clear();
            self.emitter_max_particles = 0;
        }
    }

    fn sync_selected_emitter(&mut self) {
        let selected = self.selected_emitter.clone();
        let emitter = self.effect.as_ref().and_then(|effect| {
            selected
                .as_ref()
                .and_then(|selected| effect.emitters.iter().find(|emitter| &emitter.id == selected))
                .cloned()
        });
        if let Some(emitter) = emitter {
            self.select_emitter(&emitter);
        }
    }

    fn select_emitter(&mut self, emitter: &VfxEmitter) {
        self.selected_emitter = Some(emitter.id.clone());
        self.emitter_name = emitter.name.clone();
        self.emitter_max_particles = emitter.max_particles;
    }

    fn show_status(&self, ui: &mut egui::Ui) {
        if let Some(status) = &self.status {
            ui.separator();
            ui.small(status);
        }
    }
}

fn show_add_module_menu(
    ui: &mut egui::Ui,
    emitter: &VfxEmitter,
    filter: &str,
    command: &mut Option<VfxCommand>,
) {
    let filter = filter.trim().to_ascii_lowercase();
    ui.menu_button("+ Add Module", |ui| {
        let mut current_category = String::new();
        for schema in VfxAuthoringService::new().schemas().modules {
            if !filter.is_empty()
                && !schema.display_name.to_ascii_lowercase().contains(&filter)
                && !schema.type_id.to_ascii_lowercase().contains(&filter)
            {
                continue;
            }
            if current_category != schema.category {
                if !current_category.is_empty() {
                    ui.separator();
                }
                ui.strong(&schema.category);
                current_category = schema.category.clone();
            }
            if let Some(module) = default_module(&schema.type_id) {
                if ui.button(&schema.display_name).clicked() {
                    *command = Some(VfxCommand::AddModule {
                        emitter: emitter.id.clone(),
                        module,
                        index: emitter.modules.len(),
                    });
                    ui.close();
                }
            } else {
                ui.add_enabled(false, egui::Button::new(&schema.display_name))
                    .on_hover_text("Choose required asset references before adding this module.");
            }
        }
    });
}

fn default_module(type_id: &str) -> Option<VfxModule> {
    let operation = match type_id {
        "engine.vfx.spawn_rate" => VfxModuleOperation::SpawnRate {
            particles_per_second: 10.0,
        },
        "engine.vfx.burst" => VfxModuleOperation::Burst {
            time: 0.0,
            count: 16,
        },
        "engine.vfx.shape" => VfxModuleOperation::Shape {
            shape: VfxShape::Point,
        },
        "engine.vfx.lifetime" => VfxModuleOperation::Lifetime {
            value: VfxScalarValue::Constant { value: 1.0 },
        },
        "engine.vfx.initial_speed" => VfxModuleOperation::InitialSpeed {
            value: VfxScalarValue::Constant { value: 1.0 },
        },
        "engine.vfx.initial_velocity" => VfxModuleOperation::InitialVelocity {
            value: VfxVectorValue::Constant {
                value: [0.0, 1.0, 0.0],
            },
        },
        "engine.vfx.initial_color" => VfxModuleOperation::InitialColor {
            color: [1.0; 4],
        },
        "engine.vfx.initial_size" => VfxModuleOperation::InitialSize {
            value: VfxScalarValue::Constant { value: 1.0 },
        },
        "engine.vfx.initial_rotation" => VfxModuleOperation::InitialRotation {
            value: VfxScalarValue::Constant { value: 0.0 },
        },
        "engine.vfx.force" => VfxModuleOperation::Force {
            acceleration: [0.0, -9.81, 0.0],
        },
        "engine.vfx.drag" => VfxModuleOperation::Drag { coefficient: 0.0 },
        "engine.vfx.color_over_life" => VfxModuleOperation::ColorOverLife {
            gradient: VfxGradient::linear([1.0; 4], [1.0, 1.0, 1.0, 0.0]),
        },
        "engine.vfx.size_over_life" => VfxModuleOperation::SizeOverLife {
            curve: VfxCurve::constant(1.0),
        },
        "engine.vfx.rotation_over_life" => VfxModuleOperation::RotationOverLife {
            curve: VfxCurve::constant(0.0),
        },
        "engine.vfx.billboard" => VfxModuleOperation::Billboard {
            material: None,
            texture_sheet: None,
        },
        "engine.vfx.mesh" => return None,
        _ => return None,
    };
    Some(VfxModule::new(operation))
}

fn duplicate_emitter(source: &VfxEmitter) -> VfxEmitter {
    let mut duplicate = VfxEmitter::new(format!("{} Copy", source.name), source.max_particles);
    duplicate.enabled = source.enabled;
    duplicate.modules = source
        .modules
        .iter()
        .map(|module| {
            let mut duplicate = VfxModule::new(module.operation.clone());
            duplicate.enabled = module.enabled;
            duplicate
        })
        .collect();
    duplicate
}

fn module_summary(operation: &VfxModuleOperation) -> String {
    match operation {
        VfxModuleOperation::SpawnRate {
            particles_per_second,
        } => format!("{particles_per_second:.2} particles/sec"),
        VfxModuleOperation::Burst { time, count } => format!("{count} particles at {time:.2}s"),
        VfxModuleOperation::Shape { shape } => format!("{shape:?}"),
        VfxModuleOperation::Lifetime { value } => format!("Lifetime {value:?}"),
        VfxModuleOperation::InitialSpeed { value } => format!("Speed {value:?}"),
        VfxModuleOperation::InitialVelocity { value } => format!("Velocity {value:?}"),
        VfxModuleOperation::InitialColor { color } => format!("RGBA {color:?}"),
        VfxModuleOperation::InitialSize { value } => format!("Size {value:?}"),
        VfxModuleOperation::InitialRotation { value } => format!("Rotation {value:?}"),
        VfxModuleOperation::Force { acceleration } => format!("Acceleration {acceleration:?}"),
        VfxModuleOperation::Drag { coefficient } => format!("Coefficient {coefficient:.3}"),
        VfxModuleOperation::ColorOverLife { gradient } => {
            format!("{} gradient keys", gradient.keys.len())
        }
        VfxModuleOperation::SizeOverLife { curve }
        | VfxModuleOperation::RotationOverLife { curve } => {
            format!("{} curve keys", curve.keys.len())
        }
        VfxModuleOperation::Billboard { texture_sheet, .. } => {
            if texture_sheet.is_some() {
                "Billboard with texture sheet".to_owned()
            } else {
                "Billboard".to_owned()
            }
        }
        VfxModuleOperation::Mesh { texture_sheet, .. } => {
            if texture_sheet.is_some() {
                "Mesh with texture sheet".to_owned()
            } else {
                "Mesh".to_owned()
            }
        }
    }
}

fn phase_label(phase: VfxPhase) -> &'static str {
    match phase {
        VfxPhase::Spawn => "Spawn",
        VfxPhase::Update => "Update",
        VfxPhase::Render => "Render",
    }
}

fn is_project_vfx_path(project: &ProjectRoot, path: &Path) -> bool {
    path.starts_with(project.assets_root())
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().ends_with(".vfx.json"))
}

fn project_relative_label(project: &ProjectRoot, path: &Path) -> String {
    path.strip_prefix(project.path())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn first_blocking_message(diagnostics: &[engine_authoring::VfxDiagnostic]) -> String {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.is_blocking())
        .map(|diagnostic| diagnostic.message.clone())
        .unwrap_or_else(|| "VFX transaction was rejected.".to_owned())
}

fn push_history(stack: &mut Vec<Vec<VfxCommand>>, commands: Vec<VfxCommand>) {
    if stack.len() >= MAX_HISTORY {
        stack.remove(0);
    }
    stack.push(commands);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_emitter_regenerates_stable_ids() {
        let mut source = VfxEmitter::new("Smoke", 128);
        source.modules.push(VfxModule::new(VfxModuleOperation::Billboard {
            material: None,
            texture_sheet: None,
        }));

        let duplicate = duplicate_emitter(&source);

        assert_ne!(duplicate.id, source.id);
        assert_ne!(duplicate.modules[0].id, source.modules[0].id);
        assert_eq!(duplicate.modules[0].operation, source.modules[0].operation);
    }

    #[test]
    fn default_module_covers_every_asset_free_schema() {
        for schema in VfxAuthoringService::new().schemas().modules {
            if schema.type_id == "engine.vfx.mesh" {
                assert!(default_module(&schema.type_id).is_none());
            } else {
                let module = default_module(&schema.type_id)
                    .unwrap_or_else(|| panic!("missing default for {}", schema.type_id));
                assert_eq!(module.operation.type_id(), schema.type_id);
                assert_eq!(module.phase, schema.phase);
            }
        }
    }

    #[test]
    fn undo_and_redo_use_service_inverse_commands() {
        let mut effect = VfxEffect::new("Test", 256);
        let emitter = VfxEmitter::new("Emitter", 128);
        let emitter_id = emitter.id.clone();
        effect.emitters.push(emitter);
        let service = VfxAuthoringService::new();

        let applied = service.apply(
            &effect,
            &[VfxCommand::RenameEmitter {
                emitter: emitter_id.clone(),
                name: "Renamed".to_owned(),
            }],
        );
        let renamed = applied.effect.expect("rename commits");
        assert_eq!(renamed.emitters[0].name, "Renamed");

        let undone = service.apply(&renamed, &applied.undo_commands);
        let original = undone.effect.expect("undo commits");
        assert_eq!(original.emitters[0].name, "Emitter");

        let redone = service.apply(&original, &undone.undo_commands);
        assert_eq!(redone.effect.expect("redo commits").emitters[0].name, "Renamed");
    }
}
