//! Dedicated clip, transition, and Animation Graph preview window.

use super::*;
use crate::scene_view::{AnimationPreviewMode, AnimationPreviewRequest};

/// User-facing mode selected in the Animation Preview window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnimationPreviewTab {
    Clip,
    Transition,
    Graph,
}

/// One runtime preview key paired with its author-facing display name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnimationClipChoice {
    value: String,
    label: String,
}

/// Editor-only state for the dedicated animation inspection workflow.
pub(super) struct AnimationPreviewWindow {
    pub(super) open: bool,
    /// Full source snapshot used by target and clip selectors.
    scene: Option<AuthoringScene>,
    /// Render-only scene containing the selected rig and its visual hierarchy.
    preview_scene: Option<AuthoringScene>,
    scene_revision: u64,
    target: Option<EntityId>,
    view: SceneView,
    tab: AnimationPreviewTab,
    clip: String,
    from_clip: String,
    to_clip: String,
    trigger_seconds: f32,
    fade_duration: f32,
    repeat_transition: bool,
    transition_cycle_seconds: f32,
    parameters: std::collections::BTreeMap<String, bool>,
    playing: bool,
}

impl Default for AnimationPreviewWindow {
    fn default() -> Self {
        let mut view = SceneView::new();
        view.show_ui_overlay = false;
        view.show_lod_debug = false;
        view.particle_preview_enabled = false;
        view.show_particle_debug = false;
        view.animation_preview_enabled = true;
        Self {
            open: false,
            scene: None,
            preview_scene: None,
            scene_revision: 0,
            target: None,
            view,
            tab: AnimationPreviewTab::Graph,
            clip: String::new(),
            from_clip: String::new(),
            to_clip: String::new(),
            trigger_seconds: 0.5,
            fade_duration: 0.2,
            repeat_transition: true,
            transition_cycle_seconds: 3.0,
            parameters: std::collections::BTreeMap::new(),
            playing: true,
        }
    }
}

impl AnimationPreviewWindow {
    /// Opens or refreshes the preview against an authoring scene snapshot.
    pub(super) fn open_for_scene(
        &mut self,
        scene: &AuthoringScene,
        scene_revision: u64,
        preferred_target: Option<&EntityId>,
        manifest: &engine::AssetManifest,
        assets_root: Option<&Path>,
    ) -> bool {
        self.open = true;
        self.synchronize_scene(
            scene,
            scene_revision,
            preferred_target,
            manifest,
            assets_root,
        )
    }

    /// Keeps an open preview synchronized with its source scene tab.
    pub(super) fn synchronize_scene(
        &mut self,
        scene: &AuthoringScene,
        scene_revision: u64,
        preferred_target: Option<&EntityId>,
        manifest: &engine::AssetManifest,
        assets_root: Option<&Path>,
    ) -> bool {
        let target = preferred_target
            .filter(|entity| has_animation_controller(scene, entity))
            .cloned()
            .or_else(|| {
                self.target
                    .as_ref()
                    .filter(|entity| has_animation_controller(scene, entity))
                    .cloned()
            })
            .or_else(|| first_animation_controller(scene));
        let Some(target) = target else {
            self.scene = Some(scene.clone());
            self.preview_scene = None;
            self.scene_revision = scene_revision;
            self.target = None;
            self.view.set_animation_preview_request(None);
            return false;
        };

        let target_changed = self.target.as_ref() != Some(&target);
        let scene_changed = self.scene_revision != scene_revision || self.scene.is_none();
        if scene_changed {
            self.scene = Some(scene.clone());
            self.scene_revision = scene_revision;
        }
        if target_changed {
            self.target = Some(target.clone());
        }
        if target_changed || scene_changed {
            self.rebuild_preview_scene();
            self.refresh_target_defaults(manifest, assets_root);
            if let Some(scene) = &self.preview_scene {
                self.view.focus_entity(scene, &target);
            }
            self.view.restart_animation_preview();
        }
        true
    }

    /// Rebuilds the isolated authoring snapshot consumed by this window's
    /// private Scene View. Selection lists continue to use the full source
    /// scene, while runtime conversion sees only the selected character.
    fn rebuild_preview_scene(&mut self) {
        self.preview_scene = self
            .scene
            .as_ref()
            .zip(self.target.as_ref())
            .and_then(|(scene, target)| isolate_animation_preview_scene(scene, target));
    }

    /// Configures the window to inspect one explicit graph transition.
    pub(super) fn preview_transition(
        &mut self,
        from_clip: impl Into<String>,
        to_clip: impl Into<String>,
        fade_duration: f32,
    ) {
        self.from_clip = from_clip.into();
        self.to_clip = to_clip.into();
        self.fade_duration = fade_duration.max(0.0);
        self.tab = AnimationPreviewTab::Transition;
        self.open = true;
        self.playing = true;
        self.view.restart_animation_preview();
    }

    /// Configures the window to inspect one graph State clip.
    pub(super) fn preview_clip(&mut self, clip: impl Into<String>) {
        self.clip = clip.into();
        self.tab = AnimationPreviewTab::Clip;
        self.open = true;
        self.playing = true;
        self.view.restart_animation_preview();
    }

    /// Adds graph condition names without overwriting temporary user values.
    pub(super) fn merge_graph_parameters(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            if !name.trim().is_empty() {
                self.parameters.entry(name).or_insert(false);
            }
        }
    }

    fn refresh_target_defaults(
        &mut self,
        manifest: &engine::AssetManifest,
        assets_root: Option<&Path>,
    ) {
        let choices = self
            .scene
            .as_ref()
            .zip(self.target.as_ref())
            .map(|(scene, target)| animation_clip_choices(scene, target, manifest, assets_root))
            .unwrap_or_default();
        if !choices
            .iter()
            .any(|choice| choice.value.as_str() == self.clip.as_str())
        {
            self.clip = choices
                .first()
                .map(|choice| choice.value.clone())
                .unwrap_or_default();
        }
        if !choices
            .iter()
            .any(|choice| choice.value.as_str() == self.from_clip.as_str())
        {
            self.from_clip = choices
                .first()
                .map(|choice| choice.value.clone())
                .unwrap_or_default();
        }
        if !choices
            .iter()
            .any(|choice| choice.value.as_str() == self.to_clip.as_str())
        {
            self.to_clip = choices
                .get(1)
                .or_else(|| choices.first())
                .map(|choice| choice.value.clone())
                .unwrap_or_default();
        }
        if let Some(Value::Object(fields)) = self
            .scene
            .as_ref()
            .zip(self.target.as_ref())
            .and_then(|(scene, target)| animation_controller_value(scene, target))
        {
            if let Some(Value::Object(parameters)) = fields.get("parameters") {
                for (name, value) in parameters {
                    if let Value::Bool(value) = value {
                        self.parameters.entry(name.clone()).or_insert(*value);
                    }
                }
            }
            if let Some(fade) = fields.get("fade_duration").and_then(numeric_value_as_f32)
                && fade.is_finite() && fade >= 0.0 {
                    self.fade_duration = fade;
                }
        }
    }

    fn request(&self) -> Option<AnimationPreviewRequest> {
        let target = self.target.clone()?;
        let mode = match self.tab {
            AnimationPreviewTab::Clip => AnimationPreviewMode::Clip {
                clip: self.clip.clone(),
            },
            AnimationPreviewTab::Transition => AnimationPreviewMode::Transition {
                from_clip: self.from_clip.clone(),
                to_clip: self.to_clip.clone(),
                trigger_seconds: self.trigger_seconds,
                fade_duration: self.fade_duration,
            },
            AnimationPreviewTab::Graph => AnimationPreviewMode::Graph {
                parameters: self.parameters.clone(),
            },
        };
        Some(AnimationPreviewRequest { target, mode })
    }

    /// Draws the floating preview window and its independent 3D viewport.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn show(
        &mut self,
        context: &egui::Context,
        frame: &mut eframe::Frame,
        project_root: Option<&ProjectRoot>,
        manifest: &engine::AssetManifest,
        game_module: Option<&Arc<engine::game_module::GameModule>>,
    ) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let Some(render_state) = frame.wgpu_render_state() else {
            egui::Window::new("Animation Preview")
                .open(&mut open)
                .show(context, |ui| ui.label("WGPU render state unavailable"));
            self.open = open;
            return;
        };

        let target_choices = self
            .scene
            .as_ref()
            .map(animation_controller_entities)
            .unwrap_or_default();
        let clip_choices = self
            .scene
            .as_ref()
            .zip(self.target.as_ref())
            .map(|(scene, target)| {
                animation_clip_choices(
                    scene,
                    target,
                    manifest,
                    project_root.map(ProjectRoot::assets_root).as_deref(),
                )
            })
            .unwrap_or_default();
        let mut target_changed = false;
        let mut restart = false;

        egui::Window::new("Animation Preview")
            .open(&mut open)
            .default_size(egui::vec2(900.0, 680.0))
            .min_size(egui::vec2(520.0, 420.0))
            .show(context, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Target");
                    egui::ComboBox::from_id_salt("animation_preview_target")
                        .selected_text(
                            self.target
                                .as_ref()
                                .and_then(|target| {
                                    target_choices
                                        .iter()
                                        .find(|(id, _)| id == target)
                                        .map(|(_, name)| name.as_str())
                                })
                                .unwrap_or("Select an Animation Controller"),
                        )
                        .show_ui(ui, |ui| {
                            for (entity, name) in &target_choices {
                                target_changed |= ui
                                    .selectable_value(&mut self.target, Some(entity.clone()), name)
                                    .changed();
                            }
                        });
                    ui.separator();
                    ui.selectable_value(&mut self.tab, AnimationPreviewTab::Clip, "Clip");
                    ui.selectable_value(
                        &mut self.tab,
                        AnimationPreviewTab::Transition,
                        "Transition",
                    );
                    ui.selectable_value(&mut self.tab, AnimationPreviewTab::Graph, "Graph");
                });

                ui.horizontal_wrapped(|ui| match self.tab {
                    AnimationPreviewTab::Clip => {
                        clip_combo(
                            ui,
                            "animation_preview_clip",
                            "Clip",
                            &mut self.clip,
                            &clip_choices,
                        );
                    }
                    AnimationPreviewTab::Transition => {
                        clip_combo(
                            ui,
                            "animation_preview_from",
                            "From",
                            &mut self.from_clip,
                            &clip_choices,
                        );
                        ui.label("→");
                        clip_combo(
                            ui,
                            "animation_preview_to",
                            "To",
                            &mut self.to_clip,
                            &clip_choices,
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.trigger_seconds)
                                .range(0.0..=60.0)
                                .speed(0.01)
                                .prefix("Trigger ")
                                .suffix(" s"),
                        );
                        ui.add(
                            egui::DragValue::new(&mut self.fade_duration)
                                .range(0.0..=60.0)
                                .speed(0.01)
                                .prefix("Fade ")
                                .suffix(" s"),
                        );
                        ui.checkbox(&mut self.repeat_transition, "Repeat");
                        if self.repeat_transition {
                            ui.add(
                                egui::DragValue::new(&mut self.transition_cycle_seconds)
                                    .range(0.1..=60.0)
                                    .speed(0.05)
                                    .prefix("Cycle ")
                                    .suffix(" s"),
                            );
                        }
                    }
                    AnimationPreviewTab::Graph => {
                        if self.parameters.is_empty() {
                            ui.label(
                                "No boolean parameters are declared by this controller or graph.",
                            );
                        } else {
                            ui.label("Preview Parameters");
                            for (name, value) in &mut self.parameters {
                                ui.checkbox(value, name);
                            }
                        }
                    }
                });

                control_row(ui, |ui| {
                    if ui
                        .button(if self.playing { "Pause" } else { "Play" })
                        .clicked()
                    {
                        self.playing = !self.playing;
                    }
                    if ui.button("Restart").clicked() {
                        restart = true;
                    }
                    ui.add(
                        egui::DragValue::new(&mut self.view.animation_preview_speed)
                            .range(0.0..=4.0)
                            .speed(0.05)
                            .prefix("Speed ")
                            .suffix("x"),
                    );
                    ui.checkbox(
                        &mut self.view.animation_secondary_physics_enabled,
                        "Secondary Physics",
                    )
                    .on_hover_text(
                        "Simulate isolated imported hair, skirt, and other MMD rigid-body motion. Gameplay physics remains disabled.",
                    );
                    let mut time = self.view.animation_preview_time();
                    if ui
                        .add(
                            egui::Slider::new(&mut time, 0.0..=10.0)
                                .text("Time")
                                .suffix(" s"),
                        )
                        .changed()
                    {
                        self.playing = false;
                        self.view.seek_animation_preview(time);
                    }
                });

                if let Some(status) = self.view.animation_preview_status() {
                    ui.horizontal_wrapped(|ui| {
                        let active_clip = status
                            .active_clip
                            .as_deref()
                            .map(|clip| clip_display_name(clip, &clip_choices))
                            .unwrap_or("none");
                        ui.label(format!("Active: {active_clip}"));
                        ui.label(format!("Clip time: {:.3}s", status.clip_time));
                        if let Some(progress) = status.crossfade_progress {
                            ui.label(format!("Blend: {:.0}%", progress * 100.0));
                        }
                        if let Some(transition) = &status.last_transition {
                            ui.label(format!("Last transition: {transition}"));
                        }
                    });
                    let selected_motion = match self.tab {
                        AnimationPreviewTab::Clip => !self.clip.trim().is_empty(),
                        AnimationPreviewTab::Transition => {
                            !self.from_clip.trim().is_empty() && !self.to_clip.trim().is_empty()
                        }
                        AnimationPreviewTab::Graph => false,
                    };
                    if self.playing
                        && selected_motion
                        && self.view.animation_preview_time() > 0.05
                        && status.clip_time <= f32::EPSILON
                    {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Animation did not start. Check the target Skinned Model, Animation Set, Graph, and Motion Slot bindings.",
                        );
                    }
                    if let Some(issue) = &status.runtime_issue {
                        ui.colored_label(egui::Color32::LIGHT_RED, issue);
                    }
                }

                ui.separator();
                let Some(scene) = self.preview_scene.as_ref() else {
                    ui.label("Open a scene containing an Animation Controller to preview it.");
                    return;
                };
                if self.target.is_none() {
                    ui.label("Add or select an Animation Controller in an open scene.");
                    return;
                }
                if self.tab == AnimationPreviewTab::Transition && self.repeat_transition {
                    let minimum_cycle = self.trigger_seconds + self.fade_duration + 0.1;
                    let cycle = self.transition_cycle_seconds.max(minimum_cycle);
                    if self.view.animation_preview_time() >= cycle {
                        self.view.restart_animation_preview();
                    }
                }
                self.view.animation_preview_enabled = self.playing;
                self.view.set_animation_preview_request(self.request());
                if restart {
                    self.view.restart_animation_preview();
                }
                let _ = self.view.show(
                    ui,
                    scene,
                    self.scene_revision,
                    project_root,
                    manifest,
                    game_module,
                    None,
                    None,
                    GizmoMode::Translate,
                    GizmoSpace::Global,
                    None,
                    render_state,
                );
            });

        if target_changed {
            self.refresh_target_defaults(
                manifest,
                project_root.map(ProjectRoot::assets_root).as_deref(),
            );
            self.rebuild_preview_scene();
            if let (Some(scene), Some(target)) = (&self.preview_scene, &self.target) {
                self.view.focus_entity(scene, target);
            }
            self.view.restart_animation_preview();
        }
        if !open {
            self.view.release(render_state);
        }
        self.open = open;
    }
}

/// Builds the minimal authoring scene needed to preview one Animation
/// Controller without rendering unrelated gameplay objects.
///
/// The selected controller/model entity and every skinned renderer that
/// explicitly references it are visual roots. Their descendants are retained
/// for character attachments, and their ancestors are retained for transform
/// inheritance. Only transform, animation-model, mesh-rendering, and isolated
/// Secondary Motion components are copied. Cameras, UI, gameplay scripts,
/// lights, audio, colliders, and gameplay rigid bodies cannot leak into the
/// dedicated preview world.
fn isolate_animation_preview_scene(
    source: &AuthoringScene,
    target: &EntityId,
) -> Option<AuthoringScene> {
    if !has_animation_controller(source, target) {
        return None;
    }

    let mut visual_roots = std::collections::BTreeSet::from([target.clone()]);
    visual_roots.extend(
        source
            .entities()
            .filter(|(_, entity)| skinned_renderer_targets(entity, target))
            .map(|(id, _)| id.clone()),
    );

    // Character attachments are commonly parented below the controller or a
    // mesh entity. Retain those subtrees, but do not retain siblings of an
    // ancestor (which would pull the rest of the gameplay scene back in).
    let mut included = visual_roots.clone();
    let mut frontier = visual_roots.into_iter().collect::<Vec<_>>();
    while let Some(parent) = frontier.pop() {
        for (id, entity) in source.entities() {
            if entity.parent.as_ref() == Some(&parent) && included.insert(id.clone()) {
                frontier.push(id.clone());
            }
        }
    }

    // Preserve world placement by retaining every ancestor of each visual
    // entity. Ancestor components are filtered below, so an enclosing scene
    // root contributes its Transform without contributing unrelated systems.
    let descendants = included.iter().cloned().collect::<Vec<_>>();
    for id in descendants {
        let mut current = source.entity(&id).and_then(|entity| entity.parent.clone());
        let mut depth = 0_usize;
        while let Some(parent) = current {
            if depth > source.entity_count() {
                break;
            }
            depth += 1;
            included.insert(parent.clone());
            current = source
                .entity(&parent)
                .and_then(|entity| entity.parent.clone());
        }
    }

    let mut isolated = AuthoringScene::new();
    let mut transaction = Transaction::begin(&isolated);

    // Create all entities before copying components. Parent references may
    // point to an entity created later in stable-ID order; final transaction
    // validation runs only after the complete set exists.
    for id in &included {
        let entity = source.entity(id)?;
        transaction.apply(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: entity.name.clone(),
            parent: entity
                .parent
                .as_ref()
                .filter(|parent| included.contains(*parent))
                .cloned(),
        });
    }

    for id in &included {
        let entity = source.entity(id)?;
        for (component_type, value) in &entity.components {
            let is_target_controller = id == target
                && component_type.as_str() == engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT;
            if !is_target_controller && !is_animation_preview_visual_component(component_type) {
                continue;
            }
            transaction.apply(AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type: component_type.clone(),
                value: value.clone(),
            });
        }
    }

    transaction.commit(&mut isolated).ok()?;
    Some(isolated)
}

/// Returns whether an authoring entity is a skinned render part driven by the
/// rig of the selected Animation Controller's entity.
fn skinned_renderer_targets(entity: &AuthoringEntity, target: &EntityId) -> bool {
    matches!(
        entity.components.get(&ComponentTypeId::new(
            engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT
        )),
        Some(Value::Object(fields))
            if matches!(fields.get("model"), Some(Value::EntityRef(id)) if id == target)
    )
}

/// Rendering components safe and useful in an isolated character preview.
fn is_animation_preview_visual_component(component_type: &ComponentTypeId) -> bool {
    [
        engine::scene_bridge::TRANSFORM_COMPONENT,
        engine::scene_bridge::SKINNED_MODEL_COMPONENT,
        engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
        engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
        engine::scene_bridge::LOD_GROUP_COMPONENT,
        engine::scene_bridge::SECONDARY_MOTION_COMPONENT,
    ]
    .contains(&component_type.as_str())
}

impl EditorApp {
    /// Opens the dedicated preview using the selected controller or the first
    /// controller in an open scene tab.
    pub(super) fn open_animation_preview_window(&mut self) {
        self.open_animation_preview_for(self.selected_entity.clone());
    }

    /// Opens the dedicated preview on one explicit Animation Controller.
    ///
    /// The Inspector reaches the window from the controller it is drawing
    /// rather than from Hierarchy selection, so the target is passed in
    /// instead of read back from editor state.
    pub(super) fn open_animation_preview_for(&mut self, preferred_target: Option<EntityId>) {
        let context = self
            .session
            .scene_context(preferred_target.as_ref())
            .map(|(scene, revision)| (scene.clone(), revision));
        let Some((scene, revision)) = context else {
            self.push_notification(
                EditorNotificationLevel::Info,
                "Open a scene containing an Animation Controller first".into(),
            );
            return;
        };
        if !self.animation_preview.open_for_scene(
            &scene,
            revision,
            preferred_target.as_ref(),
            &self.asset_manifest,
            self.project_root
                .as_ref()
                .map(ProjectRoot::assets_root)
                .as_deref(),
        ) {
            self.push_notification(
                EditorNotificationLevel::Info,
                "Add an Animation Controller to a scene entity before previewing".into(),
            );
        }
        self.animation_preview
            .merge_graph_parameters(self.current_graph_parameter_names());
    }

    /// Opens Transition mode for one selected State-to-State graph edge.
    pub(super) fn preview_animation_transition(
        &mut self,
        from_clip: String,
        to_clip: String,
        fade_duration: f32,
    ) {
        self.open_animation_preview_window();
        if self.animation_preview.target.is_some() {
            self.animation_preview
                .preview_transition(from_clip, to_clip, fade_duration);
        }
    }

    /// Opens Clip mode for one selected Animation Graph State.
    pub(super) fn preview_animation_clip(&mut self, clip: String) {
        self.open_animation_preview_window();
        if self.animation_preview.target.is_some() {
            self.animation_preview.preview_clip(clip);
        }
    }

    /// Synchronizes and draws the floating Animation Preview surface.
    pub(super) fn show_animation_preview_window(
        &mut self,
        context: &egui::Context,
        frame: &mut eframe::Frame,
    ) {
        if !self.animation_preview.open {
            return;
        }
        let scene_context = self
            .session
            .scene_context(self.animation_preview.target.as_ref())
            .map(|(scene, revision)| (scene.clone(), revision));
        if let Some((scene, revision)) = scene_context {
            self.animation_preview.synchronize_scene(
                &scene,
                revision,
                self.selected_entity.as_ref(),
                &self.asset_manifest,
                self.project_root
                    .as_ref()
                    .map(ProjectRoot::assets_root)
                    .as_deref(),
            );
        }
        self.animation_preview
            .merge_graph_parameters(self.current_graph_parameter_names());
        self.animation_preview.show(
            context,
            frame,
            self.project_root.as_ref(),
            &self.asset_manifest,
            self.game_module.as_ref(),
        );
    }

    fn current_graph_parameter_names(&self) -> Vec<String> {
        if !self.session.is_animation_graph() {
            return Vec::new();
        }
        self.session
            .graph()
            .edges
            .values()
            .filter_map(|edge| match edge.annotations.get("condition") {
                Some(Value::String(condition)) if !condition.trim().is_empty() => {
                    Some(condition.clone())
                }
                _ => None,
            })
            .collect()
    }
}

fn animation_controller_type() -> ComponentTypeId {
    ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT)
}

fn has_animation_controller(scene: &AuthoringScene, entity: &EntityId) -> bool {
    scene
        .entity(entity)
        .is_some_and(|entity| entity.components.contains_key(&animation_controller_type()))
}

fn first_animation_controller(scene: &AuthoringScene) -> Option<EntityId> {
    scene.entities().find_map(|(id, entity)| {
        entity
            .components
            .contains_key(&animation_controller_type())
            .then(|| id.clone())
    })
}

fn animation_controller_entities(scene: &AuthoringScene) -> Vec<(EntityId, String)> {
    scene
        .entities()
        .filter(|(_, entity)| entity.components.contains_key(&animation_controller_type()))
        .map(|(id, entity)| (id.clone(), entity.name.clone()))
        .collect()
}

fn animation_controller_value<'a>(
    scene: &'a AuthoringScene,
    entity: &EntityId,
) -> Option<&'a Value> {
    scene
        .entity(entity)?
        .components
        .get(&animation_controller_type())
}

fn animation_clip_choices(
    scene: &AuthoringScene,
    entity: &EntityId,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AnimationClipChoice> {
    let Some(Value::Object(fields)) = animation_controller_value(scene, entity) else {
        return Vec::new();
    };
    // The controller names its clips through the assigned Animation Set only.
    let (Some(Value::AssetRef(animation_set)), Some(assets_root)) =
        (fields.get("animation_set"), assets_root)
    else {
        return Vec::new();
    };
    manifest
        .get(animation_set)
        .and_then(|entry| std::fs::read_to_string(assets_root.join(&entry.path)).ok())
        .and_then(|json| engine_authoring::AnimationSet::from_json(&json).ok())
        .map(|set| animation_set_clip_choices(&set))
        .unwrap_or_default()
}

fn animation_set_clip_choices(set: &engine_authoring::AnimationSet) -> Vec<AnimationClipChoice> {
    let mut choices = set
        .bindings
        .iter()
        .map(|(slot, binding)| AnimationClipChoice {
            value: slot.as_str().to_owned(),
            label: binding.name.clone(),
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.value.cmp(&right.value))
    });
    choices
}

fn clip_display_name<'a>(selected: &'a str, choices: &'a [AnimationClipChoice]) -> &'a str {
    choices
        .iter()
        .find(|choice| choice.value == selected)
        .map(|choice| choice.label.as_str())
        .unwrap_or(selected)
}

fn numeric_value_as_f32(value: &Value) -> Option<f32> {
    match value {
        Value::F64(value) => Some(*value as f32),
        Value::I64(value) => Some(*value as f32),
        Value::U64(value) => Some(*value as f32),
        _ => None,
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;

    fn entity_id(value: &str) -> EntityId {
        EntityId::from_stable_id(StableId::new(value.to_owned())).expect("fixture entity id")
    }

    #[test]
    fn isolated_preview_keeps_current_skinned_model_and_external_renderers() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [
                    {
                        "id": "entity_01JP0000000000000000000001",
                        "name": "character_root",
                        "components": {
                            "engine.transform": {"x": 3.0, "y": 0.0, "z": 0.0}
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000002",
                        "name": "model",
                        "parent": "entity_01JP0000000000000000000001",
                        "components": {
                            "engine.skinned_model": {
                                "skeleton": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000001"}
                            },
                            "engine.animation_controller": {},
                            "engine.secondary_motion": {
                                "rig": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000004"}
                            },
                            "engine.physics_body": {"kind": "dynamic"},
                            "game.character": {"health": 100}
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000003",
                        "name": "body",
                        "parent": "entity_01JP0000000000000000000001",
                        "components": {
                            "engine.transform": {"x": 0.0, "y": 0.0, "z": 0.0},
                            "engine.skinned_mesh_renderer": {
                                "mesh": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000002"},
                                "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000002"},
                                "material": {"$type": "asset_ref", "id": "asset_01JP0000000000000000000003"},
                                "material_slots": []
                            }
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000004",
                        "name": "body_attachment",
                        "parent": "entity_01JP0000000000000000000003",
                        "components": {
                            "engine.transform": {"x": 0.0, "y": 1.0, "z": 0.0}
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000005",
                        "name": "unrelated_camera",
                        "parent": "entity_01JP0000000000000000000001",
                        "components": {
                            "engine.camera": {"fov_y_degrees": 60.0, "near": 0.1, "far": 100.0},
                            "engine.transform": {"x": 0.0, "y": 0.0, "z": 10.0}
                        }
                    }
                ]
            }"#,
        )
        .expect("preview isolation fixture must load");
        let root = entity_id("entity_01JP0000000000000000000001");
        let target = entity_id("entity_01JP0000000000000000000002");
        let body = entity_id("entity_01JP0000000000000000000003");
        let attachment = entity_id("entity_01JP0000000000000000000004");
        let unrelated = entity_id("entity_01JP0000000000000000000005");

        let isolated = isolate_animation_preview_scene(&scene, &target)
            .expect("selected controller must produce a preview scene");

        assert!(isolated.entity(&root).is_some());
        assert!(isolated.entity(&target).is_some());
        assert!(isolated.entity(&body).is_some());
        assert!(isolated.entity(&attachment).is_some());
        assert!(isolated.entity(&unrelated).is_none());
        let target_components = &isolated
            .entity(&target)
            .expect("target retained")
            .components;
        assert!(target_components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::SKINNED_MODEL_COMPONENT,
        )));
        assert!(target_components.contains_key(&animation_controller_type()));
        assert!(target_components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::SECONDARY_MOTION_COMPONENT,
        )));
        assert!(!target_components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::PHYSICS_BODY_COMPONENT,
        )));
        assert!(!target_components.contains_key(&ComponentTypeId::new("game.character")));
        assert!(isolated.validate().is_empty());
    }

    #[test]
    fn renderer_target_detection_follows_the_skinned_model_reference() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [
                    {
                        "id": "entity_01JP0000000000000000000001",
                        "name": "model",
                        "components": {
                            "engine.animation_controller": {}
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000002",
                        "name": "current",
                        "components": {
                            "engine.skinned_mesh_renderer": {
                                "model": {"$type": "entity_ref", "id": "entity_01JP0000000000000000000001"}
                            }
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000003",
                        "name": "unrelated",
                        "components": {
                            "engine.skinned_mesh_renderer": {}
                        }
                    }
                ]
            }"#,
        )
        .expect("renderer detection fixture must load");
        let target = entity_id("entity_01JP0000000000000000000001");
        assert!(skinned_renderer_targets(
            scene
                .entity(&entity_id("entity_01JP0000000000000000000002"))
                .expect("renderer retained"),
            &target,
        ));
        assert!(!skinned_renderer_targets(
            scene
                .entity(&entity_id("entity_01JP0000000000000000000003"))
                .expect("renderer retained"),
            &target,
        ));
    }
}

fn clip_combo(
    ui: &mut egui::Ui,
    id: &'static str,
    label: &str,
    selected: &mut String,
    choices: &[AnimationClipChoice],
) {
    ui.label(label);
    egui::ComboBox::from_id_salt(id)
        .selected_text(if selected.is_empty() {
            "No clip"
        } else {
            clip_display_name(selected, choices)
        })
        .show_ui(ui, |ui| {
            for choice in choices {
                let response = ui.selectable_value(selected, choice.value.clone(), &choice.label);
                if choice.value != choice.label {
                    response.on_hover_text(format!("Motion Slot: {}", choice.value));
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_choices_are_empty_without_an_assigned_animation_set() {
        let entity = EntityId::generate();
        let mut scene = AuthoringScene::new();
        let mut transaction = Transaction::begin(&scene);
        transaction.apply(AuthoringCommand::CreateEntity {
            id: entity.clone(),
            name: "Hero".into(),
            parent: None,
        });
        transaction.apply(AuthoringCommand::AddComponent {
            entity: entity.clone(),
            component_type: animation_controller_type(),
            value: Value::Object(std::collections::BTreeMap::new()),
        });
        transaction
            .commit(&mut scene)
            .expect("preview scene fixture must commit");

        assert!(animation_clip_choices(
            &scene,
            &entity,
            &engine::AssetManifest::default(),
            None
        )
        .is_empty());
    }

    #[test]
    fn animation_set_choices_show_binding_names_without_replacing_slot_ids() {
        let slot = engine_authoring::MotionSlotId::generate();
        let mut set = engine_authoring::AnimationSet::new(AssetId::generate());
        set.bindings.insert(
            slot.clone(),
            engine_authoring::AnimationBinding {
                name: "Idle".to_owned(),
                clip: engine_authoring::MotionSourceRef::native(AssetId::generate()),
                overlays: Vec::new(),
                events: Vec::new(),
            },
        );

        let choices = animation_set_clip_choices(&set);

        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].value.as_str(), slot.as_str());
        assert_eq!(choices[0].label, "Idle");
        assert_eq!(clip_display_name(slot.as_str(), &choices), "Idle");
    }
}
