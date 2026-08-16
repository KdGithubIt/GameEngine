//! Completion surface for ADR 0125 VFX authoring and deterministic preview.

use super::replace_file_contents;
use eframe::egui;
use engine::glam::Vec3;
use engine::vfx::{VfxPlayer, VfxRestartPolicy, VFX_PREVIEW_STEP_SECONDS};
use engine_authoring::{
    AssetId, StableId, VfxAuthoringService, VfxCommand, VfxCurve, VfxCurveInterpolation,
    ProjectRoot, VfxCurveKey, VfxCurveKeyId, VfxEffect, VfxEmitterId, VfxGradient, VfxGradientKey,
    VfxGradientKeyId, VfxModule, VfxModuleId, VfxModuleOperation, VfxRandomChannel,
    VfxScalarValue, VfxShape, VfxTemplate, VfxTextureSheet, VfxVectorValue,
};
use std::collections::BTreeMap;

const PREVIEW_DURATION_SECONDS: f32 = 10.0;

/// Transient editor-only draft for one module inspector.
struct VfxModuleDraft {
    module_id: VfxModuleId,
    operation: VfxModuleOperation,
    mesh_asset: String,
    material_asset: String,
}

impl VfxModuleDraft {
    fn from_module(module: &VfxModule) -> Self {
        let (mesh_asset, material_asset) = asset_strings(&module.operation);
        Self {
            module_id: module.id.clone(),
            operation: module.operation.clone(),
            mesh_asset,
            material_asset,
        }
    }

    fn matches(&self, module: &VfxModule) -> bool {
        self.module_id == module.id && self.operation == module.operation
    }
}

/// Transient deterministic preview state. The simulation is the production VFX runtime.
struct VfxPreviewState {
    player: Option<VfxPlayer>,
    playback_speed: f32,
    preview_seed: u32,
    current_time: f32,
    status: Option<String>,
    last_effect: Option<VfxEffect>,
}

impl Default for VfxPreviewState {
    fn default() -> Self {
        Self {
            player: None,
            playback_speed: 1.0,
            preview_seed: 0,
            current_time: 0.0,
            status: None,
            last_effect: None,
        }
    }
}

impl VfxPreviewState {
    fn open_effect(&mut self, effect: &VfxEffect) {
        self.preview_seed = effect.seed;
        self.current_time = 0.0;
        self.recompile(effect, false);
        self.last_effect = Some(effect.clone());
    }

    fn sync_effect(&mut self, effect: &VfxEffect) {
        if self.last_effect.as_ref() == Some(effect) {
            return;
        }
        let same_document = self.last_effect.as_ref().is_some_and(|previous| {
            previous.emitters.len() == effect.emitters.len()
                && previous
                    .emitters
                    .iter()
                    .zip(&effect.emitters)
                    .all(|(left, right)| left.id == right.id)
        });
        if !same_document {
            self.preview_seed = effect.seed;
            self.current_time = 0.0;
        }
        self.recompile(effect, same_document);
        self.last_effect = Some(effect.clone());
    }

    fn recompile(&mut self, effect: &VfxEffect, preserve_time: bool) {
        let target_time = if preserve_time { self.current_time } else { 0.0 };
        let was_playing = preserve_time
            && self
                .player
                .as_ref()
                .is_some_and(VfxPlayer::is_playing);
        let compilation = VfxAuthoringService::new().compile(effect);
        let Some(compiled) = compilation.compiled_effect else {
            self.player = None;
            self.status = Some("Preview unavailable until blocking VFX diagnostics are fixed.".into());
            return;
        };
        let mut player = VfxPlayer::new(
            compiled,
            was_playing,
            false,
            VfxRestartPolicy::Manual,
            self.playback_speed,
            Some(self.preview_seed),
            BTreeMap::new(),
        );
        player
            .instance_mut()
            .seek_preview(target_time, Vec3::ZERO);
        self.current_time = player.instance().elapsed_seconds();
        self.player = Some(player);
        self.status = None;
    }

    fn seek_to(&mut self, seconds: f32) {
        self.current_time = seconds.max(0.0);
        if let Some(player) = self.player.as_mut() {
            player
                .instance_mut()
                .seek_preview(self.current_time, Vec3::ZERO);
            self.current_time = player.instance().elapsed_seconds();
        }
    }

    fn show(
        &mut self,
        ui: &mut egui::Ui,
        effect: &VfxEffect,
        selected_emitter: Option<&VfxEmitterId>,
    ) {
        ui.separator();
        ui.heading("Preview");

        let mut seek_requested = false;
        let mut seed_changed = false;
        ui.horizontal_wrapped(|ui| {
            let is_playing = self
                .player
                .as_ref()
                .is_some_and(VfxPlayer::is_playing);
            if ui.button(if is_playing { "Pause" } else { "Play" }).clicked() {
                if let Some(player) = self.player.as_mut() {
                    if is_playing {
                        player.pause();
                    } else {
                        player.play();
                    }
                }
            }
            if ui.button("Restart").clicked() {
                if let Some(player) = self.player.as_mut() {
                    player.restart();
                }
                self.current_time = 0.0;
            }
            if ui.button("Step").clicked() {
                if let Some(player) = self.player.as_mut() {
                    player
                        .instance_mut()
                        .step(VFX_PREVIEW_STEP_SECONDS, Vec3::ZERO);
                    self.current_time = player.instance().elapsed_seconds();
                }
            }
            ui.separator();
            ui.label("Speed");
            if ui
                .add(
                    egui::DragValue::new(&mut self.playback_speed)
                        .speed(0.05)
                        .range(0.05..=8.0)
                        .suffix("x"),
                )
                .changed()
            {
                if let Some(player) = self.player.as_mut() {
                    player.time_scale = self.playback_speed;
                }
            }
            ui.label("Seed");
            seed_changed = ui
                .add(egui::DragValue::new(&mut self.preview_seed))
                .changed();
            ui.label("Time");
            seek_requested = ui
                .add(
                    egui::Slider::new(&mut self.current_time, 0.0..=PREVIEW_DURATION_SECONDS)
                        .show_value(true)
                        .suffix(" s"),
                )
                .changed();
        });

        if seed_changed {
            self.recompile(effect, true);
        } else if seek_requested {
            if let Some(player) = self.player.as_mut() {
                player
                    .instance_mut()
                    .seek_preview(self.current_time, Vec3::ZERO);
                self.current_time = player.instance().elapsed_seconds();
            }
        }

        if self
            .player
            .as_ref()
            .is_some_and(VfxPlayer::is_playing)
        {
            let dt = ui.ctx().input(|input| input.stable_dt).min(0.1);
            if let Some(player) = self.player.as_mut() {
                player.step(dt, Vec3::ZERO);
                self.current_time = player.instance().elapsed_seconds();
            }
            ui.ctx().request_repaint();
        }

        draw_preview_viewport(ui, self.player.as_ref(), effect, selected_emitter);

        if let Some(player) = self.player.as_ref() {
            let stats = player.instance().stats();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Live {}", stats.live_particles));
                ui.label(format!("Spawned {}", stats.spawned_particles));
                ui.label(format!("Dropped {}", stats.dropped_particles));
                ui.label(format!("Backend {:?}", stats.backend));
                if let Some(emitter_id) = selected_emitter {
                    if let Some(runtime) = player
                        .instance()
                        .emitters()
                        .iter()
                        .find(|runtime| runtime.source() == emitter_id)
                    {
                        ui.label(format!("Emitter live {}", runtime.live_count()));
                    }
                }
            });
        }
        if let Some(emitter_id) = selected_emitter {
            if let Some(emitter) = effect.emitters.iter().find(|emitter| &emitter.id == emitter_id) {
                egui::CollapsingHeader::new("Curve / Gradient Overview")
                    .default_open(true)
                    .show(ui, |ui| {
                        for module in &emitter.modules {
                            match &module.operation {
                                VfxModuleOperation::ColorOverLife { gradient } => {
                                    ui.small("Color Over Life");
                                    draw_gradient(ui, gradient);
                                }
                                VfxModuleOperation::SizeOverLife { curve } => {
                                    ui.small("Size Over Life");
                                    draw_curve(ui, curve);
                                }
                                VfxModuleOperation::RotationOverLife { curve } => {
                                    ui.small("Rotation Over Life");
                                    draw_curve(ui, curve);
                                }
                                VfxModuleOperation::Billboard {
                                    texture_sheet: Some(sheet),
                                    ..
                                }
                                | VfxModuleOperation::Mesh {
                                    texture_sheet: Some(sheet),
                                    ..
                                } => {
                                    ui.small("Texture Sheet Frame Over Life");
                                    draw_curve(ui, &sheet.frame_over_life);
                                }
                                _ => {}
                            }
                        }
                    });
            }
        }
        if let Some(status) = &self.status {
            ui.small(status);
        }
    }
}


#[derive(Default)]
pub(super) struct VfxCompletionState {
    selected_module: Option<VfxModuleId>,
    module_draft: Option<VfxModuleDraft>,
    preview: VfxPreviewState,
}

impl VfxCompletionState {
    pub(super) fn is_selected(&self, module: &VfxModule) -> bool {
        self.selected_module.as_ref() == Some(&module.id)
    }

    pub(super) fn select_module(&mut self, module: &VfxModule) {
        self.selected_module = Some(module.id.clone());
        self.module_draft = Some(VfxModuleDraft::from_module(module));
    }
}

impl super::VfxBuilderState {
    pub(super) fn show_completion_empty(&mut self, ui: &mut egui::Ui, project: &ProjectRoot) {
        ui.add_space(12.0);
        ui.label("Or create a normal VFX document from a shared template:");
        let mut requested = None;
        ui.horizontal(|ui| {
            for (label, template) in [
                ("Spark", VfxTemplate::Spark),
                ("Smoke", VfxTemplate::Smoke),
                ("Burst", VfxTemplate::Burst),
                ("Trail", VfxTemplate::Trail),
            ] {
                if ui.button(label).clicked() {
                    requested = Some(template);
                }
            }
        });
        if let Some(template) = requested {
            self.create_from_template(project, template);
        }
    }

    pub(super) fn show_completion_properties(&mut self, ui: &mut egui::Ui, effect: &VfxEffect) {
        let Some(emitter_id) = self.selected_emitter.as_ref() else {
            return;
        };
        let Some(emitter) = effect.emitters.iter().find(|emitter| &emitter.id == emitter_id) else {
            return;
        };
        let selected_is_valid = self
            .completion
            .selected_module
            .as_ref()
            .is_some_and(|id| emitter.modules.iter().any(|module| &module.id == id));
        if !selected_is_valid {
            if let Some(module) = emitter.modules.first() {
                self.completion.select_module(module);
            } else {
                self.completion.selected_module = None;
                self.completion.module_draft = None;
                return;
            }
        }
        let Some(module_id) = self.completion.selected_module.clone() else {
            return;
        };
        let Some(module) = emitter.modules.iter().find(|module| module.id == module_id) else {
            return;
        };
        let command = match show_module_properties(
            ui,
            &emitter.id,
            module,
            &mut self.completion.module_draft,
        ) {
            Ok(command) => command,
            Err(message) => {
                self.status = Some(message);
                None
            }
        };
        if let Some(command) = command {
            self.apply_user_commands(vec![command]);
        }
    }

    pub(super) fn show_completion_preview(&mut self, ui: &mut egui::Ui, effect: &VfxEffect) {
        self.completion.preview.sync_effect(effect);
        self.completion
            .preview
            .show(ui, effect, self.selected_emitter.as_ref());
    }

    fn create_from_template(&mut self, project: &ProjectRoot, template: VfxTemplate) {
        let Some(path) = rfd::FileDialog::new()
            .set_directory(project.assets_root())
            .set_file_name("effect.vfx.json")
            .add_filter("VFX effect", &["json"])
            .save_file()
        else {
            return;
        };
        if !super::is_project_vfx_path(project, &path) {
            self.status = Some(
                "VFX assets must be saved under this project's assets directory with a .vfx.json suffix."
                    .to_owned(),
            );
            return;
        }
        let service = VfxAuthoringService::new();
        let effect = service.template(template);
        let json = match service.effect_to_canonical_json(&effect) {
            Ok(json) => json,
            Err(error) => {
                self.status = Some(format!("Template creation failed: {error}"));
                return;
            }
        };
        if let Err(error) = replace_file_contents(&path, &json) {
            self.status = Some(format!("Template save failed: {error}"));
            return;
        }
        self.effect = Some(effect.clone());
        self.path = Some(path);
        self.selected_emitter = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.completion = VfxCompletionState::default();
        self.completion.preview.open_effect(&effect);
        self.normalize_selection(&effect);
        self.status = Some("Created VFX asset from shared template.".to_owned());
    }

    #[cfg(feature = "visual-validation")]
    pub(crate) fn prepare_visual_validation(&mut self) {
        let service = VfxAuthoringService::new();
        let effect = service.template(VfxTemplate::Smoke);
        self.effect = Some(effect.clone());
        self.path = None;
        self.selected_emitter = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.completion = VfxCompletionState::default();
        self.normalize_selection(&effect);
        if let Some(module) = effect.emitters.first().and_then(|emitter| {
            emitter
                .modules
                .iter()
                .find(|module| matches!(module.operation, VfxModuleOperation::ColorOverLife { .. }))
        }) {
            self.completion.select_module(module);
        }
        self.completion.preview.open_effect(&effect);
        self.completion.preview.seek_to(1.25);
        self.status = Some("Visual validation fixture: Smoke template at 1.25 s.".to_owned());
    }
}

fn show_module_properties(
    ui: &mut egui::Ui,
    emitter: &VfxEmitterId,
    module: &VfxModule,
    draft: &mut Option<VfxModuleDraft>,
) -> Result<Option<VfxCommand>, String> {
    if !draft.as_ref().is_some_and(|draft| draft.matches(module)) {
        *draft = Some(VfxModuleDraft::from_module(module));
    }
    let draft = draft.as_mut().expect("module draft was initialized above");

    ui.separator();
    ui.strong(format!("{} Properties", module.operation.type_id()));
    edit_operation(ui, draft);

    if ui.button("Apply Module Properties").clicked() {
        apply_asset_strings(draft)?;
        return Ok(Some(VfxCommand::ReplaceModuleOperation {
            emitter: emitter.clone(),
            module: module.id.clone(),
            operation: draft.operation.clone(),
        }));
    }
    Ok(None)
}

fn edit_operation(ui: &mut egui::Ui, draft: &mut VfxModuleDraft) {
    match &mut draft.operation {
        VfxModuleOperation::SpawnRate {
            particles_per_second,
        } => scalar_field(ui, "Particles / second", particles_per_second, 0.1),
        VfxModuleOperation::Burst { time, count } => {
            scalar_field(ui, "Time", time, 0.01);
            ui.horizontal(|ui| {
                ui.label("Count");
                ui.add(egui::DragValue::new(count).range(0..=u32::MAX));
            });
        }
        VfxModuleOperation::Shape { shape } => edit_shape(ui, shape),
        VfxModuleOperation::Lifetime { value }
        | VfxModuleOperation::InitialSpeed { value }
        | VfxModuleOperation::InitialSize { value }
        | VfxModuleOperation::InitialRotation { value } => edit_scalar_value(ui, value),
        VfxModuleOperation::InitialVelocity { value } => edit_vector_value(ui, value),
        VfxModuleOperation::InitialColor { color } => {
            ui.label("Linear RGBA");
            ui.color_edit_button_rgba_unmultiplied(color);
        }
        VfxModuleOperation::Force { acceleration } => vector3_field(ui, "Acceleration", acceleration),
        VfxModuleOperation::Drag { coefficient } => scalar_field(ui, "Coefficient", coefficient, 0.01),
        VfxModuleOperation::ColorOverLife { gradient } => edit_gradient(ui, gradient),
        VfxModuleOperation::SizeOverLife { curve }
        | VfxModuleOperation::RotationOverLife { curve } => edit_curve(ui, curve),
        VfxModuleOperation::Billboard { texture_sheet, .. } => {
            asset_text_field(ui, "Material asset", &mut draft.material_asset, true);
            edit_texture_sheet(ui, texture_sheet);
        }
        VfxModuleOperation::Mesh { texture_sheet, .. } => {
            asset_text_field(ui, "Mesh asset", &mut draft.mesh_asset, false);
            asset_text_field(ui, "Material asset", &mut draft.material_asset, true);
            edit_texture_sheet(ui, texture_sheet);
        }
    }
}

fn edit_shape(ui: &mut egui::Ui, shape: &mut VfxShape) {
    let current = match shape {
        VfxShape::Point => "Point",
        VfxShape::Box { .. } => "Box",
        VfxShape::Sphere { .. } => "Sphere",
        VfxShape::Cone { .. } => "Cone",
    };
    egui::ComboBox::from_label("Shape")
        .selected_text(current)
        .show_ui(ui, |ui| {
            if ui.selectable_label(current == "Point", "Point").clicked() {
                *shape = VfxShape::Point;
            }
            if ui.selectable_label(current == "Box", "Box").clicked() {
                *shape = VfxShape::Box {
                    half_extents: [0.5; 3],
                };
            }
            if ui.selectable_label(current == "Sphere", "Sphere").clicked() {
                *shape = VfxShape::Sphere { radius: 0.5 };
            }
            if ui.selectable_label(current == "Cone", "Cone").clicked() {
                *shape = VfxShape::Cone {
                    direction: [0.0, 1.0, 0.0],
                    angle_radians: 0.35,
                    radius: 0.25,
                };
            }
        });
    match shape {
        VfxShape::Point => {}
        VfxShape::Box { half_extents } => vector3_field(ui, "Half extents", half_extents),
        VfxShape::Sphere { radius } => scalar_field(ui, "Radius", radius, 0.01),
        VfxShape::Cone {
            direction,
            angle_radians,
            radius,
        } => {
            vector3_field(ui, "Direction", direction);
            scalar_field(ui, "Half angle (rad)", angle_radians, 0.01);
            scalar_field(ui, "Base radius", radius, 0.01);
        }
    }
}

fn edit_scalar_value(ui: &mut egui::Ui, value: &mut VfxScalarValue) {
    let is_range = matches!(value, VfxScalarValue::Range { .. });
    ui.horizontal(|ui| {
        ui.label("Mode");
        if ui.selectable_label(!is_range, "Constant").clicked() && is_range {
            let next = match value {
                VfxScalarValue::Range { min, max, .. } => (*min + *max) * 0.5,
                VfxScalarValue::Constant { value } => *value,
            };
            *value = VfxScalarValue::Constant { value: next };
        }
        if ui.selectable_label(is_range, "Range").clicked() && !is_range {
            let current = match value {
                VfxScalarValue::Constant { value } => *value,
                VfxScalarValue::Range { min, .. } => *min,
            };
            *value = VfxScalarValue::Range {
                min: current,
                max: current,
                channel: VfxRandomChannel::new(0),
            };
        }
    });
    match value {
        VfxScalarValue::Constant { value } => scalar_field(ui, "Value", value, 0.01),
        VfxScalarValue::Range { min, max, channel } => {
            scalar_field(ui, "Minimum", min, 0.01);
            scalar_field(ui, "Maximum", max, 0.01);
            let mut index = channel.index();
            ui.horizontal(|ui| {
                ui.label("Random channel");
                if ui.add(egui::DragValue::new(&mut index)).changed() {
                    *channel = VfxRandomChannel::new(index);
                }
            });
        }
    }
}

fn edit_vector_value(ui: &mut egui::Ui, value: &mut VfxVectorValue) {
    let is_range = matches!(value, VfxVectorValue::Range { .. });
    ui.horizontal(|ui| {
        ui.label("Mode");
        if ui.selectable_label(!is_range, "Constant").clicked() && is_range {
            let current = match value {
                VfxVectorValue::Range { min, max, .. } => std::array::from_fn(|i| (min[i] + max[i]) * 0.5),
                VfxVectorValue::Constant { value } => *value,
            };
            *value = VfxVectorValue::Constant { value: current };
        }
        if ui.selectable_label(is_range, "Range").clicked() && !is_range {
            let current = match value {
                VfxVectorValue::Constant { value } => *value,
                VfxVectorValue::Range { min, .. } => *min,
            };
            *value = VfxVectorValue::Range {
                min: current,
                max: current,
                channel: VfxRandomChannel::new(0),
            };
        }
    });
    match value {
        VfxVectorValue::Constant { value } => vector3_field(ui, "Value", value),
        VfxVectorValue::Range { min, max, channel } => {
            vector3_field(ui, "Minimum", min);
            vector3_field(ui, "Maximum", max);
            let mut index = channel.index();
            ui.horizontal(|ui| {
                ui.label("Random channel");
                if ui.add(egui::DragValue::new(&mut index)).changed() {
                    *channel = VfxRandomChannel::new(index);
                }
            });
        }
    }
}

fn edit_curve(ui: &mut egui::Ui, curve: &mut VfxCurve) {
    ui.label("Curve");
    draw_curve(ui, curve);
    let mut remove = None;
    let key_count = curve.keys.len();
    for (index, key) in curve.keys.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.monospace(short_id(key.id.as_str()));
            ui.label("t");
            ui.add(egui::DragValue::new(&mut key.time).speed(0.01).range(0.0..=1.0));
            ui.label("v");
            ui.add(egui::DragValue::new(&mut key.value).speed(0.01));
            egui::ComboBox::from_id_salt(("vfx_curve_interp", key.id.as_str()))
                .selected_text(match key.interpolation {
                    VfxCurveInterpolation::Step => "Step",
                    VfxCurveInterpolation::Linear => "Linear",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut key.interpolation, VfxCurveInterpolation::Step, "Step");
                    ui.selectable_value(&mut key.interpolation, VfxCurveInterpolation::Linear, "Linear");
                });
            if key_count > 1 && ui.small_button("Delete").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        curve.keys.remove(index);
    }
    if ui.small_button("+ Curve Key").clicked() {
        insert_curve_key(curve);
    }
}

fn edit_gradient(ui: &mut egui::Ui, gradient: &mut VfxGradient) {
    ui.label("Gradient");
    draw_gradient(ui, gradient);
    let mut remove = None;
    let key_count = gradient.keys.len();
    for (index, key) in gradient.keys.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.monospace(short_id(key.id.as_str()));
            ui.label("t");
            ui.add(egui::DragValue::new(&mut key.time).speed(0.01).range(0.0..=1.0));
            ui.color_edit_button_rgba_unmultiplied(&mut key.color);
            if key_count > 1 && ui.small_button("Delete").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        gradient.keys.remove(index);
    }
    if ui.small_button("+ Gradient Key").clicked() {
        insert_gradient_key(gradient);
    }
}

fn edit_texture_sheet(ui: &mut egui::Ui, texture_sheet: &mut Option<VfxTextureSheet>) {
    let mut enabled = texture_sheet.is_some();
    if ui.checkbox(&mut enabled, "Texture Sheet").changed() {
        if enabled && texture_sheet.is_none() {
            *texture_sheet = Some(VfxTextureSheet {
                columns: 1,
                rows: 1,
                frame_over_life: VfxCurve::constant(0.0),
            });
        } else if !enabled {
            *texture_sheet = None;
        }
    }
    if let Some(sheet) = texture_sheet {
        ui.horizontal(|ui| {
            ui.label("Columns");
            ui.add(egui::DragValue::new(&mut sheet.columns).range(1..=256));
            ui.label("Rows");
            ui.add(egui::DragValue::new(&mut sheet.rows).range(1..=256));
        });
        ui.small("Frame over life");
        edit_curve(ui, &mut sheet.frame_over_life);
    }
}

fn draw_curve(ui: &mut egui::Ui, curve: &VfxCurve) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(420.0), 100.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    if curve.keys.is_empty() {
        return;
    }
    let (min_value, max_value) = curve.keys.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), key| {
        (min.min(key.value), max.max(key.value))
    });
    let span = (max_value - min_value).abs().max(1.0e-4);
    let points = curve
        .keys
        .iter()
        .map(|key| {
            egui::pos2(
                egui::lerp(rect.left()..=rect.right(), key.time.clamp(0.0, 1.0)),
                egui::lerp(rect.bottom()..=rect.top(), ((key.value - min_value) / span).clamp(0.0, 1.0)),
            )
        })
        .collect::<Vec<_>>();
    painter.line(points, egui::Stroke::new(2.0, ui.visuals().text_color()));
    for point in curve.keys.iter().map(|key| {
        egui::pos2(
            egui::lerp(rect.left()..=rect.right(), key.time.clamp(0.0, 1.0)),
            egui::lerp(rect.bottom()..=rect.top(), ((key.value - min_value) / span).clamp(0.0, 1.0)),
        )
    }) {
        painter.circle_filled(point, 3.5, ui.visuals().strong_text_color());
    }
}

fn draw_gradient(ui: &mut egui::Ui, gradient: &VfxGradient) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(420.0), 30.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let segments = 48;
    for index in 0..segments {
        let left_t = index as f32 / segments as f32;
        let right_t = (index + 1) as f32 / segments as f32;
        let color = rgba(gradient.evaluate((left_t + right_t) * 0.5));
        let cell = egui::Rect::from_x_y_ranges(
            egui::lerp(rect.left()..=rect.right(), left_t)..=egui::lerp(rect.left()..=rect.right(), right_t),
            rect.top()..=rect.bottom(),
        );
        painter.rect_filled(cell, 0.0, color);
    }
}

fn insert_curve_key(curve: &mut VfxCurve) {
    let (index, time) = insertion_point(curve.keys.iter().map(|key| key.time));
    let value = curve.evaluate(time);
    curve.keys.insert(
        index,
        VfxCurveKey {
            id: VfxCurveKeyId::generate(),
            time,
            value,
            interpolation: VfxCurveInterpolation::Linear,
        },
    );
}

fn insert_gradient_key(gradient: &mut VfxGradient) {
    let (index, time) = insertion_point(gradient.keys.iter().map(|key| key.time));
    let color = gradient.evaluate(time);
    gradient.keys.insert(
        index,
        VfxGradientKey {
            id: VfxGradientKeyId::generate(),
            time,
            color,
        },
    );
}

fn insertion_point(times: impl Iterator<Item = f32>) -> (usize, f32) {
    let times = times.collect::<Vec<_>>();
    if times.is_empty() {
        return (0, 0.0);
    }
    if times.len() == 1 {
        let time = if times[0] <= 0.5 { (times[0] + 1.0) * 0.5 } else { times[0] * 0.5 };
        return (usize::from(time > times[0]), time);
    }
    let mut best = (1, (times[0] + times[1]) * 0.5, times[1] - times[0]);
    for (left_index, pair) in times.windows(2).enumerate() {
        let gap = pair[1] - pair[0];
        if gap > best.2 {
            best = (left_index + 1, (pair[0] + pair[1]) * 0.5, gap);
        }
    }
    (best.0, best.1)
}

fn apply_asset_strings(draft: &mut VfxModuleDraft) -> Result<(), String> {
    match &mut draft.operation {
        VfxModuleOperation::Billboard { material, .. } => {
            *material = parse_optional_asset(&draft.material_asset)?;
        }
        VfxModuleOperation::Mesh { mesh, material, .. } => {
            *mesh = parse_required_asset(&draft.mesh_asset, "mesh")?;
            *material = parse_optional_asset(&draft.material_asset)?;
        }
        _ => {}
    }
    Ok(())
}

fn parse_required_asset(text: &str, label: &str) -> Result<AssetId, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} asset ID must not be empty"));
    }
    AssetId::from_stable_id(StableId::new(trimmed)).map_err(|error| error.to_string())
}

fn parse_optional_asset(text: &str) -> Result<Option<AssetId>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    AssetId::from_stable_id(StableId::new(trimmed))
        .map(Some)
        .map_err(|error| error.to_string())
}

fn asset_strings(operation: &VfxModuleOperation) -> (String, String) {
    match operation {
        VfxModuleOperation::Billboard { material, .. } => (
            String::new(),
            material.as_ref().map(ToString::to_string).unwrap_or_default(),
        ),
        VfxModuleOperation::Mesh { mesh, material, .. } => (
            mesh.to_string(),
            material.as_ref().map(ToString::to_string).unwrap_or_default(),
        ),
        _ => (String::new(), String::new()),
    }
}

fn asset_text_field(ui: &mut egui::Ui, label: &str, value: &mut String, optional: bool) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(value);
        if optional {
            ui.small("(optional)");
        }
    });
}

fn scalar_field(ui: &mut egui::Ui, label: &str, value: &mut f32, speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).speed(speed));
    });
}

fn vector3_field(ui: &mut egui::Ui, label: &str, value: &mut [f32; 3]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for (axis, component) in ["X", "Y", "Z"].into_iter().zip(value.iter_mut()) {
            ui.label(axis);
            ui.add(egui::DragValue::new(component).speed(0.01));
        }
    });
}

fn short_id(id: &str) -> &str {
    id.rsplit('_').next().map_or(id, |suffix| &suffix[suffix.len().saturating_sub(6)..])
}

fn draw_preview_viewport(
    ui: &mut egui::Ui,
    player: Option<&VfxPlayer>,
    effect: &VfxEffect,
    selected_emitter: Option<&VfxEmitterId>,
) {
    let width = ui.available_width().max(320.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 300.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, egui::Color32::from_gray(18));
    let center = rect.center();
    let scale = 54.0;

    for index in -5..=5 {
        let offset = index as f32 * scale * 0.5;
        painter.line_segment(
            [egui::pos2(rect.left(), center.y + offset), egui::pos2(rect.right(), center.y + offset)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(32)),
        );
        painter.line_segment(
            [egui::pos2(center.x + offset, rect.top()), egui::pos2(center.x + offset, rect.bottom())],
            egui::Stroke::new(1.0, egui::Color32::from_gray(32)),
        );
    }

    if let Some(player) = player {
        for particle in player.render_particles() {
            let point = project(particle.position, center, scale);
            if rect.contains(point) {
                painter.circle_filled(point, (particle.size.abs() * 3.0).clamp(1.5, 10.0), rgba(particle.color));
            }
        }
    }

    if let Some(emitter_id) = selected_emitter {
        if let Some(emitter) = effect.emitters.iter().find(|emitter| &emitter.id == emitter_id) {
            if let Some(shape) = emitter.modules.iter().find_map(|module| match &module.operation {
                VfxModuleOperation::Shape { shape } if module.enabled => Some(shape),
                _ => None,
            }) {
                draw_shape(&painter, rect, center, scale, shape);
            }
        }
        if let Some(runtime) = player.and_then(|player| {
            player
                .instance()
                .emitters()
                .iter()
                .find(|runtime| runtime.source() == emitter_id)
        }) {
            if let Some((min, max)) = runtime.live_bounds() {
                draw_bounds(&painter, center, scale, min, max, egui::Color32::LIGHT_GREEN);
            }
        }
    }

    if let Some(player) = player {
        let mut bounds = None;
        for runtime in player.instance().emitters() {
            if let Some((min, max)) = runtime.live_bounds() {
                bounds = Some(match bounds {
                    Some((current_min, current_max)) => (current_min.min(min), current_max.max(max)),
                    None => (min, max),
                });
            }
        }
        if let Some((min, max)) = bounds {
            draw_bounds(&painter, center, scale, min, max, egui::Color32::LIGHT_BLUE);
        }
    }

    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        "Production CPU reference runtime",
        egui::FontId::monospace(11.0),
        egui::Color32::GRAY,
    );
}

fn draw_shape(
    painter: &egui::Painter,
    rect: egui::Rect,
    center: egui::Pos2,
    scale: f32,
    shape: &VfxShape,
) {
    let stroke = egui::Stroke::new(1.5, egui::Color32::YELLOW);
    match shape {
        VfxShape::Point => {
            painter.line_segment([center - egui::vec2(8.0, 0.0), center + egui::vec2(8.0, 0.0)], stroke);
            painter.line_segment([center - egui::vec2(0.0, 8.0), center + egui::vec2(0.0, 8.0)], stroke);
        }
        VfxShape::Box { half_extents } => {
            let corners = [
                Vec3::new(-half_extents[0], -half_extents[1], 0.0),
                Vec3::new(half_extents[0], -half_extents[1], 0.0),
                Vec3::new(half_extents[0], half_extents[1], 0.0),
                Vec3::new(-half_extents[0], half_extents[1], 0.0),
            ];
            for pair in corners.windows(2) {
                painter.line_segment([project(pair[0], center, scale), project(pair[1], center, scale)], stroke);
            }
            painter.line_segment([project(corners[3], center, scale), project(corners[0], center, scale)], stroke);
        }
        VfxShape::Sphere { radius } => {
            painter.circle_stroke(center, radius * scale, stroke);
        }
        VfxShape::Cone {
            direction,
            angle_radians,
            radius,
        } => {
            let axis = Vec3::from_array(*direction).normalize_or_zero();
            let length = radius.max(0.5) / angle_radians.tan().abs().max(0.15);
            let tip = project(Vec3::ZERO, center, scale);
            let end = axis * length;
            let base_center = project(end, center, scale);
            let radius_pixels = radius * scale;
            painter.line_segment([tip, base_center + egui::vec2(radius_pixels, 0.0)], stroke);
            painter.line_segment([tip, base_center - egui::vec2(radius_pixels, 0.0)], stroke);
            painter.line_segment(
                [base_center - egui::vec2(radius_pixels, 0.0), base_center + egui::vec2(radius_pixels, 0.0)],
                stroke,
            );
        }
    }
    let _ = rect;
}

fn draw_bounds(
    painter: &egui::Painter,
    center: egui::Pos2,
    scale: f32,
    min: Vec3,
    max: Vec3,
    color: egui::Color32,
) {
    let a = project(min, center, scale);
    let b = project(max, center, scale);
    let left = a.x.min(b.x);
    let right = a.x.max(b.x);
    let top = a.y.min(b.y);
    let bottom = a.y.max(b.y);
    let stroke = egui::Stroke::new(1.0, color);
    painter.line_segment([egui::pos2(left, top), egui::pos2(right, top)], stroke);
    painter.line_segment([egui::pos2(right, top), egui::pos2(right, bottom)], stroke);
    painter.line_segment([egui::pos2(right, bottom), egui::pos2(left, bottom)], stroke);
    painter.line_segment([egui::pos2(left, bottom), egui::pos2(left, top)], stroke);
}

fn project(position: Vec3, center: egui::Pos2, scale: f32) -> egui::Pos2 {
    egui::pos2(
        center.x + (position.x - position.z * 0.35) * scale,
        center.y - (position.y + position.z * 0.18) * scale,
    )
}

fn rgba(color: [f32; 4]) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(
        color[0].clamp(0.0, 1.0),
        color[1].clamp(0.0, 1.0),
        color[2].clamp(0.0, 1.0),
        color[3].clamp(0.0, 1.0),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{VfxEmitter, VfxPhase};

    #[test]
    fn insertion_point_preserves_time_order_for_default_curve() {
        let curve = VfxCurve::constant(1.0);
        let (index, time) = insertion_point(curve.keys.iter().map(|key| key.time));
        assert_eq!(index, 1);
        assert_eq!(time, 0.5);
    }

    #[test]
    fn preview_recompile_uses_production_seek_semantics() {
        let mut effect = VfxEffect::new("preview", 64);
        let mut emitter = VfxEmitter::new("emitter", 64);
        emitter.modules.push(VfxModule::new(VfxModuleOperation::SpawnRate {
            particles_per_second: 10.0,
        }));
        emitter.modules.push(VfxModule::new(VfxModuleOperation::Lifetime {
            value: VfxScalarValue::Constant { value: 2.0 },
        }));
        emitter.modules.push(VfxModule::new(VfxModuleOperation::Billboard {
            material: None,
            texture_sheet: None,
        }));
        assert!(emitter.modules.iter().all(|module| module.phase == module.operation.required_phase()));
        effect.emitters.push(emitter);

        let mut preview = VfxPreviewState::default();
        preview.open_effect(&effect);
        preview.current_time = 0.75;
        preview.recompile(&effect, true);

        let compiled = VfxAuthoringService::new()
            .compile(&effect)
            .compiled_effect
            .expect("valid effect compiles");
        let mut expected = VfxPlayer::new(
            compiled,
            false,
            false,
            VfxRestartPolicy::Manual,
            1.0,
            Some(effect.seed),
            BTreeMap::new(),
        );
        expected.instance_mut().seek_preview(0.75, Vec3::ZERO);

        let actual = preview.player.expect("preview player exists");
        assert_eq!(actual.instance().stats(), expected.instance().stats());
        assert_eq!(actual.render_particles().len(), expected.render_particles().len());
        assert_eq!(VfxPhase::Render, effect.emitters[0].modules[2].phase);
    }
}
