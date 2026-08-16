//! Scene workspace viewport, gizmo edits, and behavior graph canvas actions.

use super::*;

/// Minimum width of the transform tool and handle-space combo boxes.
///
/// The egui default leaves a wide gap after short labels such as `Move`, which
/// is exactly the space a narrow Scene View cannot spare.
const GIZMO_SELECTOR_WIDTH: f32 = 56.0;

/// Menu configuration for toolbar menus that group several toggles.
///
/// The egui default closes a menu on any click, which would force the user to
/// reopen the menu for every single checkbox.
fn sticky_menu_config() -> egui::containers::menu::MenuConfig {
    egui::containers::menu::MenuConfig::new()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
}

impl EditorApp {
    pub(super) fn handle_canvas_actions(&mut self, actions: Vec<GraphCanvasAction>) {
        for action in actions {
            match action {
                GraphCanvasAction::NodeClicked { node } => {
                    if let Some(source) = self.pending_connect_source.take()
                        && source != node {
                            match self.session.connect_nodes(source, node.clone()) {
                                Ok(edge) => {
                                    let result = self.session.select_edge(Some(edge));
                                    self.apply_ui_result(result);
                                    self.sync_property_buffer();
                                    continue;
                                }
                                Err(error) => self.apply_ui_result::<(), _>(Err(error)),
                            }
                        }
                    let result = self.session.select_node(Some(node));
                    self.apply_ui_result(result);
                    self.sync_property_buffer();
                }
                GraphCanvasAction::EdgeClicked { edge } => {
                    self.pending_connect_source = None;
                    let result = self.session.select_edge(Some(edge));
                    self.apply_ui_result(result);
                    self.sync_property_buffer();
                }
                GraphCanvasAction::NodeDragReleased { node, position } => {
                    let result = self.session.move_node(node, position);
                    self.apply_ui_result(result);
                }
                GraphCanvasAction::AddNode { kind, position } => {
                    self.add_graph_node(kind, Some(position));
                }
                GraphCanvasAction::ConnectFrom { node } => {
                    let result = self.session.select_node(Some(node.clone()));
                    self.apply_ui_result(result);
                    self.pending_connect_source = Some(node);
                    self.sync_property_buffer();
                }
                GraphCanvasAction::ConnectNodes { source, target } => {
                    self.pending_connect_source = None;
                    match self.session.connect_nodes(source, target) {
                        Ok(edge) => {
                            let result = self.session.select_edge(Some(edge));
                            self.apply_ui_result(result);
                            self.sync_property_buffer();
                        }
                        Err(error) => self.apply_ui_result::<(), _>(Err(error)),
                    }
                }
                GraphCanvasAction::SetPinned { node, pinned } => {
                    let result = self.session.set_node_pinned(node, pinned);
                    self.apply_ui_result(result);
                }
                GraphCanvasAction::DeleteNode { node } => {
                    self.pending_connect_source = None;
                    let result = self.session.delete_node(node);
                    self.apply_ui_result(result);
                    self.sync_property_buffer();
                }
                GraphCanvasAction::DeleteEdge { edge } => {
                    self.pending_connect_source = None;
                    let result = self.session.delete_edge(edge);
                    self.apply_ui_result(result);
                    self.sync_property_buffer();
                }
                GraphCanvasAction::ApplyIncrementalLayout => {
                    let result = self.session.apply_incremental_layout();
                    self.apply_ui_result(result);
                }
            }
        }
    }

    pub(super) fn show_add_node_menu(&mut self, ui: &mut egui::Ui) {
        let mut last_category = String::new();
        for kind in self.session.available_graph_node_kinds() {
            if kind.category() != last_category.as_str() {
                if !last_category.is_empty() {
                    ui.separator();
                }
                last_category = kind.category().to_owned();
                ui.strong(&last_category);
            }
            let label = kind.label().to_owned();
            if ui.button(label).clicked() {
                self.add_graph_node(kind, None);
                ui.close();
            }
        }
    }

    fn add_graph_node(
        &mut self,
        kind: GraphNodeInsertKind,
        requested_position: Option<engine_authoring::Vec2>,
    ) {
        let position = crate::canvas::next_node_position(
            &self.session,
            requested_position,
            self.canvas.visible_center_graph(),
        );
        let result =
            self.session
                .add_graph_node(kind, self.new_node_behavior.clone(), Some(position));
        self.apply_ui_result(result);
        self.sync_property_buffer();
    }

    pub(super) fn show_scene_workspace(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        ui.horizontal_wrapped(|ui| {
            // A narrow Scene View must wrap toolbar items onto another row
            // instead of wrapping each label into a column of glyphs.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            if let Some(source) = &self.prefab_placement_source {
                let text = format!(
                    "Prefab Placement: {} (click canvas, Esc to finish)",
                    source.display()
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(egui::Color32::LIGHT_GREEN))
                        .wrap(),
                );
                if ui.button("Finish Placement").clicked() {
                    self.prefab_placement_source = None;
                }
                ui.separator();
            }
            self.show_gizmo_selectors(ui);
            ui.separator();
            self.show_camera_menu(ui);
            self.show_display_menu(ui);
            self.show_preview_menu(ui);
        });
        ui.separator();
        let Some(render_state) = frame.wgpu_render_state() else {
            ui.label("WGPU render state unavailable");
            return;
        };
        let Some(scene) = self.session.scene() else {
            ui.label("No scene open");
            return;
        };
        let component_preview = self.pending_component_drag.as_ref().and_then(|pending| {
            self.session
                .scene_entity(&pending.entity)
                .and_then(|entity| entity.components.get(&pending.component_type))
                .and_then(|component| pending.scene_preview(component))
        });
        let scene_revision = self.session.document_revision();
        // The document open in UI Builder is authored in memory; pass its live
        // version so the Scene View overlay reflects Builder edits immediately.
        let open_ui_asset = self.open_ui_document_asset();
        let open_ui_document = open_ui_asset.as_ref().zip(self.session.ui_document());
        let output = self.scene_view.show(
            ui,
            scene,
            scene_revision,
            self.project_root.as_ref(),
            &self.asset_manifest,
            self.game_module.as_ref(),
            component_preview.as_ref(),
            self.selected_entity.as_ref(),
            self.gizmo_mode,
            self.gizmo_space,
            open_ui_document,
            render_state,
        );
        // Refresh Problems only when the visible Scene View diagnostic changes.
        // The existing Problems-to-Console mirror then records a new appearance
        // once without flooding Console on every rendered frame.
        if self.scene_view_problem != output.preview_diagnostic {
            self.scene_view_problem = output.preview_diagnostic.clone();
            self.refresh_scene_problems();
        }
        if let Some(entity) = output.picked_entity {
            let additive = ui.input(|input| input.modifiers.ctrl || input.modifiers.command);
            if additive {
                self.toggle_entity_selection(entity);
            } else {
                self.select_single_entity(Some(entity));
            }
        }
        if let Some(ids) = output.box_selected {
            let additive = ui.input(|input| input.modifiers.ctrl || input.modifiers.command);
            if !additive {
                self.select_single_entity(None);
            }
            for id in &ids {
                self.selected_entities.insert(id.clone());
            }
            self.selected_entity = ids.last().cloned();
            self.hierarchy_selection_anchor = self.selected_entity.clone();
        }
        if let Some(selection) = output.picked_ui_node {
            self.open_scene_ui_node(selection);
        }
        if let Some(edit) = output.gizmo_edit {
            self.apply_scene_gizmo_edit(edit);
        }
        if let (Some(source), Some(position)) = (
            self.prefab_placement_source.clone(),
            output.placement_position,
        ) {
            match crate::prefab_workflow::instantiate_prefab(&mut self.session, &source, None) {
                Ok(root) => {
                    let _ = self
                        .session
                        .set_scene_entity_positions([root.clone()], position.map(Some));
                    self.select_single_entity(Some(root));
                    self.refresh_scene_problems();
                }
                Err(error) => self
                    .session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.prefab_placement_failed",
                        error.to_string(),
                    )),
            }
        }
        if output.response.dnd_hover_payload::<DragPayload>().is_some() {
            ui.painter().rect_stroke(
                output.response.rect,
                0.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 180, 255)),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(payload) = output.response.dnd_release_payload::<DragPayload>() {
            if payload.kind == AssetKind::Mesh {
                self.create_entity_from_dropped_asset(&payload, None);
            } else if payload.kind == AssetKind::Material {
                self.assign_dropped_material(ui, payload.asset_id.clone());
            }
        }
    }

    /// Creates scene content for a mesh asset dropped on the Scene View or
    /// the Hierarchy.
    ///
    /// A model source expands to the whole hierarchy it describes, materials
    /// included (ADR 0075); a plain mesh file becomes one entity.
    pub(super) fn create_entity_from_dropped_asset(
        &mut self,
        payload: &DragPayload,
        parent: Option<EntityId>,
    ) {
        // FBX内のMeshサブアセットも、表示上のrelative_pathには元のFBXパスを持つ。
        // パスだけで判定するとサブアセットまでモデルソース本体として扱われるため、
        // トップレベルのマニフェスト登録IDであることも同時に確認する。
        if dropped_asset_is_model_source(&self.asset_manifest, payload) {
            self.instantiate_model_source_under(&payload.asset_id, parent);
            return;
        }

        // トップレベルのモデルソースでなければ、AssetIdそのものをMesh参照として使用する。
        // これにより、FBX配下の`[mesh]`サブアセットは通常のMesh Entityとして生成される。
        match self
            .session
            .create_entity_from_mesh_asset(payload.asset_id.clone(), parent)
        {
            Ok(entity) => {
                self.select_single_entity(Some(entity));
                self.refresh_scene_problems();
            }
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    /// Assigns a material dropped from the Asset Browser to the entity under
    /// the pointer, adding the component when the entity lacks one.
    fn assign_dropped_material(&mut self, ui: &egui::Ui, asset: AssetId) {
        let Some(position) = ui.ctx().pointer_interact_pos() else {
            return;
        };
        let Some(target) = self.scene_view.pick_last_frame(position) else {
            self.push_notification(
                EditorNotificationLevel::Info,
                "Drop a material onto an entity to assign it".into(),
            );
            return;
        };
        // Every render component owns its own material reference, so a
        // drag-and-drop edit is a field assignment on the component that draws.
        let material_owner = self.session.scene_entity(&target).and_then(|entity| {
            [
                engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
                engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                engine::scene_bridge::PARTICLE_EMITTER_COMPONENT,
            ]
            .into_iter()
            .map(ComponentTypeId::new)
            .find(|component_type| entity.components.contains_key(component_type))
        });
        let Some(component_type) = material_owner else {
            self.push_notification(
                EditorNotificationLevel::Info,
                "That entity has no renderer to assign a material to".into(),
            );
            return;
        };
        let result = self.session.assign_asset_to_field(
            target,
            component_type,
            vec![PropertyPathSegment::Field {
                name: "material".to_owned(),
            }],
            asset,
        );
        self.apply_ui_result(result);
        self.refresh_scene_problems();
    }

    /// Transform tool and handle space pickers.
    ///
    /// Both stay visible as compact combo boxes because they are the only
    /// toolbar controls that change what a canvas drag does.
    fn show_gizmo_selectors(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("scene_gizmo_mode")
            .selected_text(self.gizmo_mode.label())
            .width(GIZMO_SELECTOR_WIDTH)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.gizmo_mode, GizmoMode::Translate, "Move")
                    .on_hover_text("Move the selected entity (T)");
                ui.selectable_value(&mut self.gizmo_mode, GizmoMode::Rotate, "Rotate")
                    .on_hover_text("Rotate the selected entity (R)");
                ui.selectable_value(&mut self.gizmo_mode, GizmoMode::Scale, "Scale")
                    .on_hover_text("Scale the selected entity (S)");
            })
            .response
            .on_hover_text("Transform tool for the selected entity (T / R / S)");
        egui::ComboBox::from_id_salt("scene_gizmo_space")
            .selected_text(self.gizmo_space.label())
            .width(GIZMO_SELECTOR_WIDTH)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.gizmo_space, GizmoSpace::Global, "Global")
                    .on_hover_text("Handles follow the world axes");
                ui.selectable_value(&mut self.gizmo_space, GizmoSpace::Local, "Local")
                    .on_hover_text("Handles follow the selected entity's rotation");
            })
            .response
            .on_hover_text("Coordinate space used by the translate handles");
    }

    /// Camera framing plus the actions that copy poses between the editor
    /// view and the selected camera entity.
    fn show_camera_menu(&mut self, ui: &mut egui::Ui) {
        let camera_type = ComponentTypeId::new(engine::scene_bridge::CAMERA_COMPONENT);
        let selected_camera = self.selected_entity.clone().filter(|entity| {
            self.session
                .scene_entity(entity)
                .is_some_and(|item| item.components.contains_key(&camera_type))
        });
        let enabled = selected_camera.is_some();
        egui::containers::menu::MenuButton::new("Camera").ui(ui, |ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("Focus Selected"))
                .on_hover_text("Frame the selected entity (F)")
                .clicked()
                && let (Some(scene), Some(entity)) =
                    (self.session.scene(), self.selected_entity.as_ref())
                {
                    self.scene_view.focus_entity(scene, entity);
                }
            if ui
                .button("Reset Camera")
                .on_hover_text("Restore the default Scene View camera")
                .clicked()
            {
                self.scene_view.reset_camera();
            }
            ui.separator();
            if ui
                .add_enabled(enabled, egui::Button::new("Align Camera to View"))
                .on_hover_text("Move the selected camera entity to the current editor viewpoint")
                .on_disabled_hover_text("Select an entity with a Camera component")
                .clicked()
                && let Some(entity) = selected_camera.clone() {
                    self.align_camera_entity_to_view(entity);
                }
            if ui
                .add_enabled(enabled, egui::Button::new("Align View to Camera"))
                .on_hover_text("Move the editor viewpoint to the selected camera entity")
                .on_disabled_hover_text("Select an entity with a Camera component")
                .clicked()
                && let Some(entity) = selected_camera {
                    self.align_view_to_camera_entity(&entity);
                }
        });
    }

    /// Scene View-only display toggles that never touch the scene or assets.
    fn show_display_menu(&mut self, ui: &mut egui::Ui) {
        egui::containers::menu::MenuButton::new("Display")
            .config(sticky_menu_config())
            .ui(ui, |ui| {
                ui.checkbox(&mut self.scene_view.show_ui_overlay, "Show UI")
                    .on_hover_text(
                        "Show the UI overlay only in Scene View without changing the game or assets",
                    );
                if !self.scene_view.show_ui_overlay {
                    self.scene_view.ui_selection_enabled = false;
                }
                ui.add_enabled_ui(self.scene_view.show_ui_overlay, |ui| {
                    ui.checkbox(&mut self.scene_view.ui_selection_enabled, "Select UI")
                        .on_hover_text(
                            "Select the front-most UI node; turn this off to select 3D entities through the overlay",
                        );
                });
                ui.horizontal(|ui| {
                    ui.label("Game Frame");
                    egui::ComboBox::from_id_salt("scene_view_game_frame_aspect")
                        .selected_text(self.scene_view.game_frame_aspect.label())
                        .show_ui(ui, |ui| {
                            for preset in ViewAspect::ALL {
                                if ui
                                    .selectable_label(
                                        self.scene_view.game_frame_aspect == preset,
                                        preset.label(),
                                    )
                                    .clicked()
                                {
                                    self.scene_view.game_frame_aspect = preset;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Show the UI overlay as it appears on this target screen (16:9 = 1920x1080, 4:3 = 1440x1080) instead of at Scene View panel size",
                        );
                });
                ui.separator();
                ui.checkbox(&mut self.scene_view.show_sky, "Sky")
                    .on_hover_text("Draw the gradient sky behind the scene preview");
                ui.checkbox(&mut self.scene_view.show_lod_debug, "LOD Debug")
                    .on_hover_text(
                        "Show authored LOD thresholds and the active level for the selection",
                    );
            });
    }

    /// Particle and animation preview playback controls.
    fn show_preview_menu(&mut self, ui: &mut egui::Ui) {
        egui::containers::menu::MenuButton::new("Preview")
            .config(sticky_menu_config())
            .ui(ui, |ui| {
                ui.checkbox(
                    &mut self.scene_view.particle_preview_enabled,
                    "Particle Preview",
                );
                ui.checkbox(&mut self.scene_view.show_particle_debug, "Particle Debug")
                    .on_hover_text(
                        "Show live bounds, emission direction, rate, and pool occupancy",
                    );
                if ui.button("Restart Particles").clicked() {
                    self.scene_view.restart_particle_preview();
                }
                ui.separator();
                ui.checkbox(
                    &mut self.scene_view.animation_preview_enabled,
                    "Animation Preview",
                )
                .on_hover_text(
                    "Sample authored Animator clips without changing scene or Play state",
                );
                if ui.button("Restart Animation").clicked() {
                    self.scene_view.restart_animation_preview();
                }
                ui.add(
                    egui::DragValue::new(&mut self.scene_view.animation_preview_speed)
                        .range(0.0..=4.0)
                        .speed(0.05)
                        .prefix("Speed ")
                        .suffix("x"),
                );
                ui.separator();
                if ui
                    .button("Animation Preview...")
                    .on_hover_text("Inspect one clip, one transition, or the full Animation Graph")
                    .clicked()
                {
                    self.open_animation_preview_window();
                    ui.close();
                }
            });
    }

    fn align_camera_entity_to_view(&mut self, entity: EntityId) {
        use engine::glam::EulerRot;
        let type_id = crate::gizmo::transform_component_type();
        let Some(transform) = self
            .session
            .scene_entity(&entity)
            .and_then(|item| item.components.get(&type_id).cloned())
        else {
            return;
        };
        let Value::Object(mut fields) = transform else {
            return;
        };
        let camera_transform = self.scene_view.camera.to_transform();
        let eye = camera_transform.translation;
        let (rx, ry, rz) = camera_transform.rotation.to_euler(EulerRot::XYZ);
        for (key, value) in [
            ("x", eye.x as f64),
            ("y", eye.y as f64),
            ("z", eye.z as f64),
            ("rotation_x_degrees", rx.to_degrees() as f64),
            ("rotation_y_degrees", ry.to_degrees() as f64),
            ("rotation_z_degrees", rz.to_degrees() as f64),
        ] {
            fields.insert(key.to_owned(), Value::F64(value));
        }
        let result = self
            .session
            .set_scene_component_value(entity, type_id, Value::Object(fields));
        self.apply_ui_result(result);
        self.refresh_scene_problems();
    }

    fn align_view_to_camera_entity(&mut self, entity: &EntityId) {
        use engine::glam::{EulerRot, Quat, Vec3};
        let type_id = crate::gizmo::transform_component_type();
        let Some(Value::Object(fields)) = self
            .session
            .scene_entity(entity)
            .and_then(|item| item.components.get(&type_id).cloned())
        else {
            return;
        };
        let number = |key: &str, default: f64| match fields.get(key) {
            Some(Value::F64(value)) => *value,
            Some(Value::I64(value)) => *value as f64,
            _ => default,
        };
        let eye = Vec3::new(
            number("x", 0.0) as f32,
            number("y", 0.0) as f32,
            number("z", 0.0) as f32,
        );
        let rotation = Quat::from_euler(
            EulerRot::XYZ,
            (number("rotation_x_degrees", 0.0) as f32).to_radians(),
            (number("rotation_y_degrees", 0.0) as f32).to_radians(),
            (number("rotation_z_degrees", 0.0) as f32).to_radians(),
        );
        let forward = rotation * Vec3::NEG_Z;
        self.scene_view.camera.align_to(eye, forward);
    }

    /// Opens the UI asset selected through the Scene View runtime layout and
    /// transfers its node identity into UI Builder presentation state.
    fn open_scene_ui_node(&mut self, selection: SceneUiNodeSelection) {
        let Some(project_root) = self.project_root.clone() else {
            return;
        };
        let Some(entry) = self.asset_manifest.get(&selection.document_asset) else {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.scene_ui_asset_missing",
                    format!(
                        "UI node `{}` belongs to unregistered asset `{}`",
                        selection.node_id, selection.document_asset
                    ),
                ));
            return;
        };
        let path = match project_root.resolve_asset(&entry.path) {
            Ok(path) => path,
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.scene_ui_asset_unresolved",
                        format!(
                            "could not open UI asset `{}` for node `{}`: {error}",
                            selection.document_asset, selection.node_id
                        ),
                    ));
                return;
            }
        };

        self.request_open(PendingOpen::Ui(path.clone()));
        if self.session.current_document_path() == Some(path.as_path())
            && self.session.ui_document().is_some_and(|document| {
                engine_authoring::find_ui_node(document, &selection.node_id).is_some()
            })
        {
            self.ui_builder.selected_node = Some(selection.node_id.clone());
            self.ui_builder.selected_nodes.clear();
            self.ui_builder
                .selected_nodes
                .insert(selection.node_id.clone());
            self.ui_builder.selection_anchor = Some(selection.node_id);
        }
    }

    /// Mirrors UI Builder node changes back to an existing Scene View
    /// selection without coupling either presentation surface to authoring
    /// document persistence.
    pub(super) fn sync_scene_view_ui_selection(&mut self) {
        let Some(path) = self.session.current_document_path() else {
            return;
        };
        let Some(project_root) = self.project_root.as_ref() else {
            return;
        };
        let Some((asset, _)) = self.asset_manifest.iter().find(|(_, entry)| {
            project_root
                .resolve_asset(&entry.path)
                .is_ok_and(|candidate| candidate == path)
        }) else {
            return;
        };
        self.scene_view
            .sync_ui_builder_selection(asset, self.ui_builder.selected_node.as_deref());
    }

    /// Resolves the asset id of the document currently open in the editor, used
    /// to refresh its scene placements with the live UI Builder document.
    ///
    /// Returns `None` when no project is open or the active document is not a
    /// manifest-registered file; the Scene View then leaves placed documents at
    /// their loaded values.
    fn open_ui_document_asset(&self) -> Option<AssetId> {
        let path = self.session.current_document_path()?;
        let project_root = self.project_root.as_ref()?;
        self.asset_manifest
            .iter()
            .find(|(_, entry)| {
                project_root
                    .resolve_asset(&entry.path)
                    .is_ok_and(|candidate| candidate == path)
            })
            .map(|(asset, _)| asset.clone())
    }

    fn apply_scene_gizmo_edit(&mut self, edit: GizmoEdit) {
        use crate::gizmo::{apply_rotate_delta, apply_scale_delta, transform_component_type};
        let Some(sel_id) = self.selected_entity.clone() else {
            return;
        };
        let type_id = transform_component_type();
        let Some(transform) = self
            .session
            .scene_entity(&sel_id)
            .and_then(|e| e.components.get(&type_id).cloned())
        else {
            return;
        };
        if edit.mode == GizmoMode::Translate {
            // Local-space handles may point along an oblique world direction,
            // so translation applies as a vector to x/y/z in one command.
            let world_delta = [
                (edit.axis_direction[0] * edit.delta) as f64,
                (edit.axis_direction[1] * edit.delta) as f64,
                (edit.axis_direction[2] * edit.delta) as f64,
            ];
            let Some(value) = crate::gizmo::apply_translate_vector(&transform, world_delta) else {
                return;
            };
            let result = self
                .session
                .set_scene_component_value(sel_id, type_id, value);
            self.apply_ui_result(result);
            self.refresh_scene_problems();
            return;
        }
        let changed = match edit.mode {
            GizmoMode::Translate => unreachable!("translate handled above"),
            GizmoMode::Rotate => apply_rotate_delta(&transform, edit.axis, edit.delta),
            GizmoMode::Scale => apply_scale_delta(&transform, edit.axis, edit.delta),
        };
        let Some((path, value)) = changed else {
            return;
        };
        // Transform v2 added rotation and scale fields as optional serialized
        // values. Old scenes therefore legitimately lack those keys. Editing a
        // displayed default must materialize the key instead of failing with a
        // property-not-found diagnostic.
        let result = if property_value(&transform, &path).is_some() {
            self.session
                .set_scene_component_property(sel_id, type_id, path, value)
        } else {
            let mut upgraded = transform;
            if upsert_property_value(&mut upgraded, &path, value.clone()) {
                self.session
                    .set_scene_component_value(sel_id, type_id, upgraded)
            } else {
                self.session
                    .set_scene_component_property(sel_id, type_id, path, value)
            }
        };
        self.apply_ui_result(result);
        self.refresh_scene_problems();
    }
}

/// ドロップされたAssetIdが、配置可能なモデルソース本体を表すか判定する。
///
/// FBX、glTF、GLBから派生したMeshサブアセットも元ソースのパスを保持するため、
/// `DragPayload::relative_path`だけでは本体とサブアセットを区別できない。
/// トップレベルのマニフェスト登録をAssetIdで解決し、その登録パスがモデル形式である場合だけ
/// モデル全体をインスタンス化する。
pub(super) fn dropped_asset_is_model_source(
    manifest: &engine::AssetManifest,
    payload: &DragPayload,
) -> bool {
    // 派生サブアセットはトップレベルのManifestEntryを持たない。
    // その場合はモデルソース本体ではなく、個別のMesh参照として扱う。
    let Some(entry) = manifest.get(&payload.asset_id) else {
        return false;
    };

    // AssetIdで解決した正規の登録パスを使い、FBX、glTF、GLBかを判定する。
    engine::asset_path_matches_kind(
        engine::AssetKind::GltfSource,
        std::path::Path::new(&entry.path),
    )
}
