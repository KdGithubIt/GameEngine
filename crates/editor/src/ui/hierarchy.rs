//! Scene hierarchy panel and scene entity clipboard and layout operations.

use super::*;

impl EditorApp {
    pub(super) fn delete_selected_entity(&mut self) {
        let Some(id) = self.selected_entity.clone() else {
            return;
        };
        let selected = if self.selected_entities.is_empty() {
            std::iter::once(id).collect::<Vec<_>>()
        } else {
            self.selected_entities.iter().cloned().collect()
        };
        if selected
            .iter()
            .any(|entity| self.locked_entities.contains(entity))
        {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::warning(
                    "editor.delete_locked_selection",
                    "unlock selected entities before deleting them",
                ));
            return;
        }
        let result = self.session.delete_scene_entity_subtrees(selected);
        if result.is_ok() {
            self.select_single_entity(None);
        }
        self.apply_ui_result(result);
    }

    pub(super) fn show_scene_hierarchy(&mut self, ui: &mut egui::Ui) {
        let is_playing = self.is_playing();
        let mut create_requested = None;
        control_row(ui, |ui| {
            ui.heading("Hierarchy");
            if !is_playing {
                ui.menu_button("+ Create", |ui| {
                    show_entity_preset_menu(ui, &mut create_requested);
                })
                .response
                .on_hover_text("Create an entity at the Scene Root");
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.hierarchy_filter).hint_text("Search entities..."),
        );
        ui.separator();

        let filter = self.hierarchy_filter.trim();
        let entities: Vec<_> = self
            .session
            .scene()
            .map(|scene| scene_hierarchy_rows(scene, filter, &self.collapsed_entities))
            .unwrap_or_default();
        let visible_order = entities
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();

        let mut delete_request = None;
        let mut duplicate_request = None;
        let mut copy_request = None;
        let mut reparent_request = None;
        // Dropping a mesh asset here creates scene content, so the Hierarchy
        // is a placement target alongside the Scene View.
        let mut asset_drop: Option<(DragPayload, Option<EntityId>)> = None;
        let mut create_prefab_request = None;
        let mut enable_request = None;

        // Scene Root is a presentation-only drop target for `parent: None`.
        // It is intentionally not serialized as an entity, so existing scenes
        // keep their current authoring format while users always have an
        // obvious way to detach an entity from its parent.
        let root_response = ui
            .add_sized(
                [ui.available_width(), 28.0],
                egui::Button::new("Scene Root (no parent)"),
            )
            .on_hover_text("Drop an entity here to remove its parent");
        if !self.is_playing() {
            if let Some(payload) =
                release_drag_payload_in_rect::<HierarchyDragPayload>(ui, root_response.rect)
            {
                reparent_request = Some((payload.entity.clone(), None));
            } else if let Some(payload) =
                release_drag_payload_in_rect::<DragPayload>(ui, root_response.rect)
            {
                asset_drop = Some((payload.as_ref().clone(), None));
            }
        }
        ui.separator();

        if entities.is_empty() {
            ui.label("(empty)");
        }

        for row in entities {
            let has_parent = row.parent.is_some();
            let id = row.id.clone();
            let label = if row.display_name.is_empty() {
                row.name.clone()
            } else {
                format!("{} ({})", row.display_name, row.name)
            };

            let selected =
                self.selected_entities.contains(&id) || self.selected_entity.as_ref() == Some(&id);
            let is_hidden = self.hidden_entities.contains(&id);
            let is_locked = self.locked_entities.contains(&id);
            let mut visibility_request = None;
            let mut lock_request = None;
            let mut label_response = None;
            let row_response = ui.horizontal(|ui| {
                ui.add_space(row.depth as f32 * 14.0);
                // A model instantiates as a whole subtree, so folding is
                // what keeps one placed model reading as one item.
                if row.has_children {
                    let collapsed = self.collapsed_entities.contains(&id);
                    if ui
                        .small_button(if collapsed { "▶" } else { "▼" })
                        .on_hover_text("Fold this entity's children")
                        .clicked()
                        && !self.collapsed_entities.remove(&id)
                    {
                        self.collapsed_entities.insert(id.clone());
                    }
                } else {
                    ui.add_space(18.0);
                }
                let label_text = if row.enabled {
                    egui::RichText::new(label)
                } else {
                    // Disabled subtrees stay visible for editing but read
                    // as inactive, mirroring the runtime skip.
                    egui::RichText::new(label).weak()
                };
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 24.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let has_state_override = !row.enabled || is_hidden || is_locked;
                        let status_text = if has_state_override {
                            egui::RichText::new("⋯").color(egui::Color32::YELLOW)
                        } else {
                            egui::RichText::new("⋯")
                        };
                        ui.menu_button(status_text, |ui| {
                            ui.strong("Entity State");

                            let mut enabled = row.enabled;
                            if ui
                                .add_enabled(
                                    !is_playing,
                                    egui::Checkbox::new(&mut enabled, "Enabled in Play"),
                                )
                                .on_disabled_hover_text(
                                    "Stop Play mode before changing authored state",
                                )
                                .changed()
                            {
                                enable_request = Some((id.clone(), enabled));
                            }

                            let mut visible = !is_hidden;
                            if ui.checkbox(&mut visible, "Visible in Editor").changed() {
                                visibility_request = Some(visible);
                            }

                            let mut locked = is_locked;
                            if ui.checkbox(&mut locked, "Lock Editing").changed() {
                                lock_request = Some(locked);
                            }
                        })
                        .response
                        .on_hover_text(hierarchy_entity_state_summary(
                            row.enabled,
                            is_hidden,
                            is_locked,
                        ));

                        // Truncation keeps an entity name on one readable
                        // line when the Scene View takes most of the width.
                        // The full name remains available in the tooltip.
                        label_response = Some(
                            ui.add_sized(
                                [ui.available_width(), 22.0],
                                egui::Button::selectable(selected, label_text).truncate(),
                            )
                            .on_hover_text(hierarchy_entity_tooltip(&row, is_hidden, is_locked)),
                        );
                    },
                );
            });
            paint_hierarchy_guides(ui, row_response.response.rect, row.depth);

            if let Some(visible) = visibility_request {
                if visible {
                    self.hidden_entities.remove(&id);
                } else {
                    self.hidden_entities.insert(id.clone());
                }
            }
            if let Some(locked) = lock_request {
                if locked {
                    self.locked_entities.insert(id.clone());
                } else {
                    self.locked_entities.remove(&id);
                }
            }

            let Some(response) = label_response else {
                continue;
            };
            if response.clicked() {
                let additive = ui.input(|input| input.modifiers.ctrl || input.modifiers.command);
                let range = ui.input(|input| input.modifiers.shift);
                if range {
                    if let Some(anchor) = self.hierarchy_selection_anchor.as_ref() {
                        if let (Some(start), Some(end)) = (
                            visible_order
                                .iter()
                                .position(|candidate| candidate == anchor),
                            visible_order.iter().position(|candidate| candidate == &id),
                        ) {
                            if !additive {
                                self.selected_entities.clear();
                            }
                            let (minimum, maximum) = if start <= end {
                                (start, end)
                            } else {
                                (end, start)
                            };
                            self.selected_entities
                                .extend(visible_order[minimum..=maximum].iter().cloned());
                            self.selected_entity = Some(id.clone());
                        }
                    } else {
                        self.selected_entities.clear();
                        self.selected_entities.insert(id.clone());
                        self.selected_entity = Some(id.clone());
                        self.hierarchy_selection_anchor = Some(id.clone());
                    }
                } else if additive {
                    if !self.selected_entities.remove(&id) {
                        self.selected_entities.insert(id.clone());
                        self.selected_entity = Some(id.clone());
                    } else if self.selected_entity.as_ref() == Some(&id) {
                        self.selected_entity = self.selected_entities.iter().next_back().cloned();
                    }
                } else {
                    self.selected_entities.clear();
                    self.selected_entities.insert(id.clone());
                    self.selected_entity = Some(id.clone());
                }
                if !range {
                    self.hierarchy_selection_anchor = Some(id.clone());
                }
            }
            if !self.is_playing() {
                response
                    .clone()
                    .interact(egui::Sense::click_and_drag())
                    .dnd_set_drag_payload(HierarchyDragPayload { entity: id.clone() });
                if let Some(payload) = release_drag_payload_in_rect::<HierarchyDragPayload>(
                    ui,
                    row_response.response.rect,
                ) {
                    if payload.entity != id {
                        reparent_request = Some((payload.entity.clone(), Some(id.clone())));
                    }
                } else if let Some(payload) =
                    release_drag_payload_in_rect::<DragPayload>(ui, row_response.response.rect)
                {
                    asset_drop = Some((payload.as_ref().clone(), Some(id.clone())));
                }
                let context_selection_count = if selected {
                    self.selected_scene_ids().len()
                } else {
                    1
                };
                response.context_menu(|ui| {
                    let duplicate_label = if context_selection_count > 1 {
                        format!("Duplicate {context_selection_count} Entities")
                    } else {
                        "Duplicate".to_owned()
                    };
                    if ui.button(duplicate_label).clicked() {
                        duplicate_request = Some(id.clone());
                        ui.close();
                    }
                    let copy_label = if context_selection_count > 1 {
                        format!("Copy {context_selection_count} Entities")
                    } else {
                        "Copy".to_owned()
                    };
                    if ui.button(copy_label).clicked() {
                        copy_request = Some(id.clone());
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.project_root.is_some(),
                            egui::Button::new("Create Prefab..."),
                        )
                        .clicked()
                    {
                        create_prefab_request = Some(id.clone());
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_parent, egui::Button::new("Move to Scene Root"))
                        .on_disabled_hover_text("This entity already has no parent")
                        .clicked()
                    {
                        reparent_request = Some((id.clone(), None));
                        ui.close();
                    }
                    ui.separator();
                    let delete_label = if context_selection_count > 1 {
                        format!("Delete {context_selection_count} Entities")
                    } else {
                        "Delete".to_owned()
                    };
                    if ui.button(delete_label).clicked() {
                        delete_request = Some(id.clone());
                        ui.close();
                    }
                });
            }
        }

        // Claim the rest of the visible Hierarchy viewport instead of making
        // only a one-line footer interactive. Users can now right-click any
        // unused area to create or paste an entity, even in a short scene.
        // Entity 行をすべて配置した後、現在のコンテンツ UI に残っている
        // 高さを取得する。
        //
        // `available_height` は ScrollArea 内部の max_rect とカーソルから
        // 同じコンテンツ座標系で計算されるため、スクロール offset が
        // 空白領域の高さへ再加算されない。ビューポートの clip_rect と
        // スクロールで移動する min_rect を直接比較すると、スクロールする
        // たびにコンテンツ総量が増えるため使用しない。
        let empty_space_height = hierarchy_empty_space_height(ui.available_height());
        let empty_space = ui
            .add_sized(
                [ui.available_width(), empty_space_height],
                egui::Label::new(
                    egui::RichText::new("Right-click empty space to create an entity")
                        .small()
                        .color(egui::Color32::GRAY),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Open the Hierarchy context menu");
        let mut paste_requested = false;
        if !is_playing {
            if let Some(payload) =
                release_drag_payload_in_rect::<HierarchyDragPayload>(ui, empty_space.rect)
            {
                reparent_request = Some((payload.entity.clone(), None));
            } else if let Some(payload) =
                release_drag_payload_in_rect::<DragPayload>(ui, empty_space.rect)
            {
                asset_drop = Some((payload.as_ref().clone(), None));
            }
            empty_space.context_menu(|ui| {
                ui.menu_button("Create", |ui| {
                    show_entity_preset_menu(ui, &mut create_requested);
                });
                if ui
                    .add_enabled(self.entity_clipboard.is_some(), egui::Button::new("Paste"))
                    .clicked()
                {
                    paste_requested = true;
                    ui.close();
                }
            });
        }

        if let Some(id) = duplicate_request {
            if !self.selected_entities.contains(&id)
                && self.selected_entity.as_ref() != Some(&id)
            {
                self.select_single_entity(Some(id));
            }
            self.duplicate_selected_entity();
        }
        if let Some(id) = copy_request {
            if !self.selected_entities.contains(&id)
                && self.selected_entity.as_ref() != Some(&id)
            {
                self.select_single_entity(Some(id));
            }
            self.copy_selected_entity();
        }
        if let Some(id) = create_prefab_request {
            self.create_prefab_from_selected_entity_id(id);
        }
        if let Some(preset) = create_requested {
            self.create_scene_entity_from_preset(preset);
        }
        if paste_requested {
            self.paste_entity();
        }

        if let Some(id) = delete_request {
            if !self.selected_entities.contains(&id)
                && self.selected_entity.as_ref() != Some(&id)
            {
                self.select_single_entity(Some(id));
            }
            self.delete_selected_entity();
        }
        if let Some((entity, parent)) = reparent_request {
            let result = self.session.set_scene_entity_parent(entity, parent);
            self.apply_ui_result(result);
        }
        if let Some((entity, enabled)) = enable_request {
            let result = self.session.set_scene_entity_enabled(entity, enabled);
            self.apply_ui_result(result);
        }
        if let Some((payload, parent)) = asset_drop {
            self.create_entity_from_dropped_asset(&payload, parent);
        }
    }

    fn create_scene_entity_from_preset(&mut self, preset: EntityPreset) {
        let registry = engine::builtin_registry();
        let component = |type_id: &str| {
            let component_type = ComponentTypeId::new(type_id);
            registry
                .get(&component_type)
                .map(|definition| (component_type, definition.schema.default_value()))
                .expect("entity presets only reference registered built-in components")
        };
        let kinematic_body = || {
            (
                ComponentTypeId::new(engine::scene_bridge::PHYSICS_BODY_COMPONENT),
                Value::Object(std::collections::BTreeMap::from([(
                    "kind".to_owned(),
                    Value::String("kinematic".to_owned()),
                )])),
            )
        };
        let (name, display_name, description, components) = match preset {
            EntityPreset::Empty => (
                "new_entity",
                "New Entity",
                "An empty authoring entity.",
                Vec::new(),
            ),
            EntityPreset::Player => (
                "player",
                "Player",
                "A controllable action-game character with collision, health, and lock-on camera behavior.",
                vec![
                    component(engine::scene_bridge::TRANSFORM_COMPONENT),
                    component(engine::scene_bridge::PLAYER_MARKER_COMPONENT),
                    component(engine::scene_bridge::PLAYER_CONTROLLER_COMPONENT),
                    component(engine::scene_bridge::CHARACTER_CONTROLLER_COMPONENT),
                    component(engine::scene_bridge::COLLIDER_COMPONENT),
                    kinematic_body(),
                    component(engine::scene_bridge::DAMAGE_RECEIVER_COMPONENT),
                    component(engine::scene_bridge::LOCK_ON_CAMERA_COMPONENT),
                ],
            ),
            EntityPreset::Enemy => (
                "enemy",
                "Enemy",
                "A NavMesh-driven combat target with collision and damage state.",
                vec![
                    component(engine::scene_bridge::TRANSFORM_COMPONENT),
                    component(engine::scene_bridge::NAV_MESH_AGENT_COMPONENT),
                    component(engine::scene_bridge::COLLIDER_COMPONENT),
                    kinematic_body(),
                    component(engine::scene_bridge::DAMAGE_RECEIVER_COMPONENT),
                    component(engine::scene_bridge::LOCK_ON_TARGET_COMPONENT),
                    component(engine::scene_bridge::RUNTIME_METADATA_COMPONENT),
                ],
            ),
            EntityPreset::Camera => (
                "main_camera",
                "Main Camera",
                "The primary perspective camera for the scene.",
                vec![
                    component(engine::scene_bridge::TRANSFORM_COMPONENT),
                    component(engine::scene_bridge::CAMERA_COMPONENT),
                ],
            ),
            EntityPreset::DirectionalLight => (
                "directional_light",
                "Directional Light",
                "A scene-wide directional light suitable for sunlight.",
                vec![
                    component(engine::scene_bridge::TRANSFORM_COMPONENT),
                    component(engine::scene_bridge::DIRECTIONAL_LIGHT_COMPONENT),
                ],
            ),
            EntityPreset::Triangle | EntityPreset::Quad => {
                let (name, label, mesh_id) = match preset {
                    EntityPreset::Triangle => (
                        "triangle",
                        "Triangle",
                        engine::scene_bridge::BUILTIN_TRIANGLE_ASSET_ID,
                    ),
                    EntityPreset::Quad => (
                        "quad",
                        "Quad",
                        engine::scene_bridge::BUILTIN_QUAD_ASSET_ID,
                    ),
                    _ => unreachable!("primitive branch only receives primitive presets"),
                };
                (
                    name,
                    label,
                    "A renderable built-in primitive.",
                    vec![
                        component(engine::scene_bridge::TRANSFORM_COMPONENT),
                        (
                            ComponentTypeId::new(
                                engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
                            ),
                            Value::Object(std::collections::BTreeMap::from([
                                ("mesh".to_owned(), Value::AssetRef(builtin_asset_id(mesh_id))),
                                (
                                    "material".to_owned(),
                                    Value::AssetRef(builtin_asset_id(
                                        engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID,
                                    )),
                                ),
                                ("material_slots".to_owned(), Value::Array(Vec::new())),
                            ])),
                        ),
                    ],
                )
            }
        };
        match self.session.create_scene_entity_with_components(
            name,
            display_name,
            description,
            components,
        ) {
            Ok(id) => self.selected_entity = Some(id),
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
        self.refresh_scene_problems();
    }

    pub(super) fn duplicate_selected_entity(&mut self) {
        let Some(id) = self.selected_entity.clone() else {
            return;
        };
        let selected = if self.selected_entities.is_empty() {
            vec![id]
        } else {
            self.selected_entities.iter().cloned().collect()
        };
        match self
            .session
            .duplicate_scene_entities(selected, self.duplicate_offset)
        {
            Ok(new_ids) => {
                self.selected_entities = new_ids.iter().cloned().collect();
                self.selected_entity = new_ids.last().cloned();
                self.hierarchy_selection_anchor = self.selected_entity.clone();
                self.last_duplicate_selection = new_ids;
            }
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    pub(super) fn repeat_last_duplicate(&mut self) {
        if self.last_duplicate_selection.is_empty() {
            return;
        }
        match self
            .session
            .duplicate_scene_entities(self.last_duplicate_selection.clone(), self.duplicate_offset)
        {
            Ok(new_ids) => {
                self.selected_entities = new_ids.iter().cloned().collect();
                self.selected_entity = new_ids.last().cloned();
                self.hierarchy_selection_anchor = self.selected_entity.clone();
                self.last_duplicate_selection = new_ids;
            }
            Err(error) => self.apply_ui_result::<(), _>(Err(error)),
        }
    }

    /// Replaces the whole scene selection with at most one entity.
    ///
    /// `selected_entity`, `selected_entities`, and the Shift-range anchor must
    /// change together; updating only the primary field leaves stale Hierarchy
    /// highlights and stale multi-entity command targets behind.
    pub(super) fn select_single_entity(&mut self, entity: Option<EntityId>) {
        self.selected_entities.clear();
        self.selected_entities.extend(entity.clone());
        self.hierarchy_selection_anchor = entity.clone();
        self.selected_entity = entity;
    }

    /// Adds or removes one entity from the selection (Ctrl-click semantics).
    pub(super) fn toggle_entity_selection(&mut self, entity: EntityId) {
        if !self.selected_entities.remove(&entity) {
            self.selected_entities.insert(entity.clone());
            self.hierarchy_selection_anchor = Some(entity.clone());
            self.selected_entity = Some(entity);
        } else if self.selected_entity.as_ref() == Some(&entity) {
            self.selected_entity = self.selected_entities.iter().next_back().cloned();
        }
    }

    /// Drops one entity from the selection after it was deleted.
    pub(super) fn remove_entity_from_selection(&mut self, entity: &EntityId) {
        self.selected_entities.remove(entity);
        if self.hierarchy_selection_anchor.as_ref() == Some(entity) {
            self.hierarchy_selection_anchor = None;
        }
        if self.selected_entity.as_ref() == Some(entity) {
            self.selected_entity = self.selected_entities.iter().next_back().cloned();
        }
    }

    pub(super) fn selected_scene_ids(&self) -> Vec<EntityId> {
        if self.selected_entities.is_empty() {
            self.selected_entity.iter().cloned().collect()
        } else {
            self.selected_entities.iter().cloned().collect()
        }
    }

    pub(super) fn align_selected(&mut self, axis: SceneAxis, alignment: SceneAlignment) {
        let selected = self.selected_scene_ids();
        let result = self.session.align_scene_entities(selected, axis, alignment);
        self.apply_ui_result(result);
    }

    pub(super) fn distribute_selected(&mut self, axis: SceneAxis) {
        let selected = self.selected_scene_ids();
        let result = self.session.distribute_scene_entities(selected, axis);
        self.apply_ui_result(result);
    }

    pub(super) fn copy_selected_entity(&mut self) {
        let selected = self.selected_scene_ids();
        if selected.is_empty() {
            return;
        }
        let copied = selected
            .iter()
            .filter_map(|id| self.session.scene_entity(id).cloned())
            .collect::<Vec<_>>();
        self.entity_clipboard = (!copied.is_empty()).then_some(copied);
    }

    pub(super) fn paste_entity(&mut self) {
        let Some(copied) = self.entity_clipboard.clone() else {
            return;
        };
        let new_ids = match self.session.paste_scene_entities(&copied) {
            Ok(ids) => ids,
            Err(error) => {
                self.apply_ui_result::<(), _>(Err(error));
                return;
            }
        };
        self.selected_entities = new_ids.iter().cloned().collect();
        self.selected_entity = new_ids.last().cloned();
        self.hierarchy_selection_anchor = self.selected_entity.clone();
    }
}

/// Multi-component templates available from the Hierarchy creation menu.
#[derive(Clone, Copy)]
enum EntityPreset {
    Empty,
    Player,
    Enemy,
    Camera,
    DirectionalLight,
    Triangle,
    Quad,
}

/// Returns a released drag payload only when its concrete type matches `T`.
///
/// egui's `take_payload` removes the shared payload before attempting its
/// downcast. The Hierarchy accepts both entity moves and Asset Browser drops,
/// so probing the wrong type with `take_payload` would otherwise discard the
/// payload before the matching handler can see it. The non-destructive
/// `payload` lookup performs the type check first, and the payload is consumed
/// only after the pointer is released inside this target rectangle.
pub(super) fn release_drag_payload_in_rect<T>(
    ui: &egui::Ui,
    target_rect: egui::Rect,
) -> Option<std::sync::Arc<T>>
where
    T: std::any::Any + Send + Sync,
{
    // A row can contain several child widgets. Checking the full row rectangle
    // through the UI lets any point on the visible entity row act as the
    // parenting target instead of limiting drops to the entity-name button.
    if !ui.rect_contains_pointer(target_rect) || !ui.input(|input| input.pointer.any_released()) {
        return None;
    }

    // This lookup only clones a correctly typed Arc and leaves a payload of a
    // different type untouched for the next supported Hierarchy handler.
    egui::DragAndDrop::payload::<T>(ui.ctx())?;

    // The type is now known to match, so consuming the shared payload is safe
    // and prevents another overlapping target from processing the same drop.
    egui::DragAndDrop::take_payload::<T>(ui.ctx())
}

/// Draws the shared list of root-level entity templates.
///
/// Both the persistent Hierarchy toolbar and the empty-space context menu use
/// this function so their creation choices cannot drift apart.
fn show_entity_preset_menu(ui: &mut egui::Ui, request: &mut Option<EntityPreset>) {
    for (label, preset) in [
        ("Empty Entity", EntityPreset::Empty),
        ("Player", EntityPreset::Player),
        ("Enemy", EntityPreset::Enemy),
        ("Camera", EntityPreset::Camera),
        ("Directional Light", EntityPreset::DirectionalLight),
        ("Primitive / Triangle", EntityPreset::Triangle),
        ("Primitive / Quad", EntityPreset::Quad),
    ] {
        if ui.button(label).clicked() {
            *request = Some(preset);
            ui.close();
        }
    }
}

/// Hierarchy の末尾に配置する空白領域の高さを返す。
///
/// `available_height` は ScrollArea 内部の同じコンテンツ座標系から取得する。
/// そのため、ユーザーがスクロールしてもスクロール量が高さへ再加算されず、
/// コンテンツ総量が無限に増えることを防げる。
///
/// 短いシーンでは右クリック可能な領域をビューポートの残りまで広げる。
/// 長いシーンでは、末尾操作領域として最低 48px だけを確保する。
pub(super) fn hierarchy_empty_space_height(available_height: f32) -> f32 {
    // 末尾領域が小さ過ぎると右クリック操作が困難になるため、
    // コンテンツ量に関係なく最低 48px を保証する。
    available_height.max(48.0)
}

/// Paints subtle continuation lines that make nesting visible independently
/// from the entity names and row-state controls.
fn paint_hierarchy_guides(ui: &egui::Ui, row_rect: egui::Rect, depth: usize) {
    if depth == 0 {
        return;
    }

    let stroke = egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color().gamma_multiply(0.45));
    for level in 0..depth {
        let x = row_rect.left() + 7.0 + level as f32 * 14.0;
        ui.painter().line_segment(
            [
                egui::pos2(x, row_rect.top()),
                egui::pos2(x, row_rect.bottom()),
            ],
            stroke,
        );
    }

    let parent_x = row_rect.left() + 7.0 + (depth - 1) as f32 * 14.0;
    ui.painter().line_segment(
        [
            egui::pos2(parent_x, row_rect.center().y),
            egui::pos2(parent_x + 11.0, row_rect.center().y),
        ],
        stroke,
    );
}

/// Returns the compact status explanation shown by the single row menu.
pub(super) fn hierarchy_entity_state_summary(
    enabled: bool,
    is_hidden: bool,
    is_locked: bool,
) -> String {
    format!(
        "{} · {} · {}",
        if enabled { "Enabled" } else { "Disabled" },
        if is_hidden { "Hidden" } else { "Visible" },
        if is_locked { "Locked" } else { "Editable" },
    )
}

/// Builds a full-name tooltip for rows whose visible label may be truncated.
fn hierarchy_entity_tooltip(row: &HierarchyRow, is_hidden: bool, is_locked: bool) -> String {
    let title = if row.display_name.is_empty() {
        row.name.clone()
    } else {
        format!("{} ({})", row.display_name, row.name)
    };
    let parent = row.parent.as_ref().map_or("Scene Root", EntityId::as_str);
    format!(
        "{title}\nParent: {parent}\n{}",
        hierarchy_entity_state_summary(row.enabled, is_hidden, is_locked)
    )
}

#[derive(Clone)]
pub(super) struct HierarchyRow {
    pub(super) id: EntityId,
    pub(super) name: String,
    pub(super) display_name: String,
    pub(super) parent: Option<EntityId>,
    pub(super) depth: usize,
    pub(super) enabled: bool,
    /// Whether this entity has children, so the row can offer a fold toggle.
    pub(super) has_children: bool,
}

#[derive(Clone)]
pub(super) struct HierarchyDragPayload {
    pub(super) entity: EntityId,
}

/// Builds the visible hierarchy rows in depth-first order.
///
/// Subtrees under an entity in `collapsed` are omitted so an instantiated
/// model can occupy one line. A search overrides folding: hiding a match
/// because an ancestor happens to be folded would make the filter look
/// broken.
pub(super) fn scene_hierarchy_rows(
    scene: &AuthoringScene,
    filter: &str,
    collapsed: &std::collections::BTreeSet<EntityId>,
) -> Vec<HierarchyRow> {
    use std::collections::{BTreeMap, BTreeSet};

    let filter = filter.trim().to_ascii_lowercase();
    let empty_collapsed = BTreeSet::new();
    let collapsed = if filter.is_empty() {
        collapsed
    } else {
        &empty_collapsed
    };
    let mut visible = BTreeSet::new();
    for (id, entity) in scene.entities() {
        let searchable = format!(
            "{} {} {} {} {}",
            id.as_str(),
            entity.name,
            entity.display_name,
            entity.description,
            entity
                .components
                .keys()
                .map(ComponentTypeId::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_ascii_lowercase();
        if filter.is_empty() || searchable.contains(&filter) {
            visible.insert(id.clone());
            let mut parent = entity.parent.clone();
            while let Some(parent_id) = parent {
                if !visible.insert(parent_id.clone()) {
                    break;
                }
                parent = scene
                    .entity(&parent_id)
                    .and_then(|parent| parent.parent.clone());
            }
        }
    }

    let mut children: BTreeMap<Option<EntityId>, Vec<EntityId>> = BTreeMap::new();
    for (id, entity) in scene.entities() {
        let parent = entity
            .parent
            .clone()
            .filter(|parent| scene.entity(parent).is_some());
        children.entry(parent).or_default().push(id.clone());
    }

    #[allow(clippy::too_many_arguments)]
    fn append_rows(
        scene: &AuthoringScene,
        children: &BTreeMap<Option<EntityId>, Vec<EntityId>>,
        visible: &BTreeSet<EntityId>,
        collapsed: &BTreeSet<EntityId>,
        parent: Option<EntityId>,
        depth: usize,
        rows: &mut Vec<HierarchyRow>,
    ) {
        let Some(ids) = children.get(&parent) else {
            return;
        };
        for id in ids {
            let Some(entity) = scene.entity(id) else {
                continue;
            };
            let has_children = children
                .get(&Some(id.clone()))
                .is_some_and(|entries| entries.iter().any(|child| visible.contains(child)));
            if visible.contains(id) {
                rows.push(HierarchyRow {
                    id: id.clone(),
                    name: entity.name.clone(),
                    display_name: entity.display_name.clone(),
                    parent: entity.parent.clone(),
                    depth,
                    enabled: entity.enabled,
                    has_children,
                });
                if collapsed.contains(id) {
                    continue;
                }
            }
            append_rows(
                scene,
                children,
                visible,
                collapsed,
                Some(id.clone()),
                depth + 1,
                rows,
            );
        }
    }

    let mut rows = Vec::new();
    append_rows(scene, &children, &visible, collapsed, None, 0, &mut rows);
    rows
}
