//! Animation Controller component Inspector and its asset lifecycle actions.
//!
//! The controller owns two author-created assets, so its Inspector is grouped
//! by authoring task and offers create/open actions beside the pickers rather
//! than leaving the author to build the Graph and Set elsewhere first.

use crate::ui::*;

/// Describes an action requested by the Animation Controller asset controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum AnimationControllerAssetAction {
    OpenGraph(AssetId),
    CreateGraph,
    OpenSet(AssetId),
    CreateSet { graph: AssetId },
    OpenPreview,
}

impl EditorApp {
    /// Draws lifecycle actions for the two author-owned assets referenced by
    /// an Animation Controller. The controls are rendered inside the
    /// controller's `Rig & Clips` section beside the schema-driven pickers.
    fn show_animation_controller_asset_actions(
        ui: &mut egui::Ui,
        fields: &std::collections::BTreeMap<String, Value>,
    ) -> Option<AnimationControllerAssetAction> {
        let graph = fields.get("graph").and_then(|value| match value {
            Value::AssetRef(asset) => Some(asset.clone()),
            _ => None,
        });
        let animation_set = fields.get("animation_set").and_then(|value| match value {
            Value::AssetRef(asset) => Some(asset.clone()),
            _ => None,
        });

        let mut action = None;
        control_row(ui, |ui| {
            ui.small("Graph");
            action = match graph.clone() {
                Some(graph) if ui.button("Open Graph").clicked() => {
                    Some(AnimationControllerAssetAction::OpenGraph(graph))
                }
                None if ui.button("Create Graph").clicked() => {
                    Some(AnimationControllerAssetAction::CreateGraph)
                }
                _ => None,
            };

            ui.small("Set");
            if action.is_none() {
                action = match animation_set {
                    Some(animation_set) if ui.button("Open Set").clicked() => {
                        Some(AnimationControllerAssetAction::OpenSet(animation_set))
                    }
                    None => {
                        let response = ui
                            .add_enabled(graph.is_some(), egui::Button::new("Create Set"))
                            .on_disabled_hover_text(
                                "Create or assign an Animation Graph before creating its Set",
                            );
                        response
                            .clicked()
                            .then(|| AnimationControllerAssetAction::CreateSet {
                                graph: graph.clone().expect("enabled Create Set requires a Graph"),
                            })
                    }
                    _ => None,
                };
            }
        });

        // The preview samples this controller's rig without entering Play, so
        // it stays reachable even while the Graph or Set reference is still
        // missing; the window itself reports what it cannot resolve.
        if ui
            .button("Open Animation Preview")
            .on_hover_text("Inspect one clip, one transition, or the full Animation Graph")
            .clicked()
        {
            action = Some(AnimationControllerAssetAction::OpenPreview);
        }
        action
    }

    /// Executes one Animation Controller asset action after the Inspector has
    /// released its immutable scene and manifest borrows.
    pub(in crate::ui) fn perform_animation_controller_asset_action(
        &mut self,
        entity: EntityId,
        action: AnimationControllerAssetAction,
    ) {
        match action {
            AnimationControllerAssetAction::OpenGraph(graph) => {
                self.open_animation_graph_asset(&graph);
            }
            AnimationControllerAssetAction::CreateGraph => {
                self.create_animation_graph_for_controller(entity);
            }
            AnimationControllerAssetAction::OpenSet(animation_set) => {
                self.open_animation_set_asset(&animation_set);
            }
            AnimationControllerAssetAction::CreateSet { graph } => {
                self.create_animation_set_for_controller(entity, graph);
            }
            AnimationControllerAssetAction::OpenPreview => {
                self.open_animation_preview_for(Some(entity));
            }
        }
    }

    /// Assigns a created asset through the same command-backed component edit
    /// path as the ordinary AssetRef picker.
    pub(in crate::ui) fn assign_animation_controller_asset_reference(
        &mut self,
        entity: EntityId,
        field: &'static str,
        asset: AssetId,
    ) {
        let component_type =
            ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
        let Some(current) = self
            .session
            .scene_entity(&entity)
            .and_then(|entity| entity.components.get(&component_type))
            .cloned()
        else {
            self.report_error(
                "editor.animation_controller_missing",
                "the selected entity no longer has an Animation Controller",
            );
            return;
        };

        self.apply_component_edit(
            entity,
            component_type,
            &current,
            ComponentEdit::Property {
                path: vec![PropertyPathSegment::Field {
                    name: field.to_owned(),
                }],
                value: Value::AssetRef(asset),
            },
        );
    }
}

/// Organizes the Animation Controller's long field list by authoring task.
///
/// Rig and playback settings stay open because they are the common path.
/// Variable-length event and parameter editors start folded so one controller
/// does not consume the entire Inspector before the author needs those lists.
pub(in crate::ui) fn show_animation_controller_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
    schema: &engine_authoring::ComponentSchema,
    hints: &[engine::FieldDef],
    context: &InspectorEditContext<'_>,
    animation_asset_action: &mut Option<AnimationControllerAssetAction>,
) -> Option<ComponentEdit> {
    const RIG_AND_CLIPS: &[&str] = &["enabled", "skeleton", "animation_set", "graph"];
    const PLAYBACK: &[&str] = &[
        "looping",
        "playback_speed",
        "completion_event",
        "root_motion_mode",
        "fade_duration",
    ];
    const PARAMETERS: &[&str] = &["parameters"];
    const GROUPED: &[&str] = &[
        "enabled",
        "skeleton",
        "animation_set",
        "graph",
        "looping",
        "playback_speed",
        "completion_event",
        "root_motion_mode",
        "fade_duration",
        "parameters",
    ];

    for (title, names, default_open) in [
        ("Rig & Clips", RIG_AND_CLIPS, true),
        ("Playback", PLAYBACK, true),
        ("Parameters", PARAMETERS, false),
    ] {
        let mut section_edit = None;
        egui::CollapsingHeader::new(title)
            .id_salt((schema.type_id.as_str(), title))
            .default_open(default_open)
            .show(ui, |ui| {
                section_edit =
                    show_schema_fields_editor(ui, fields, schema, hints, context, Some(names));
                if title == "Rig & Clips" {
                    show_animation_controller_assignment_notice(ui, fields);
                    if animation_asset_action.is_none() {
                        *animation_asset_action =
                            EditorApp::show_animation_controller_asset_actions(ui, fields);
                    }
                }
            });
        if section_edit.is_some() {
            return section_edit;
        }
    }

    let additional = schema
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .filter(|name| !GROUPED.contains(name))
        .collect::<Vec<_>>();
    if additional.is_empty() {
        return None;
    }

    let mut section_edit = None;
    egui::CollapsingHeader::new("Additional Settings")
        .id_salt((schema.type_id.as_str(), "additional"))
        .default_open(true)
        .show(ui, |ui| {
            section_edit =
                show_schema_fields_editor(ui, fields, schema, hints, context, Some(&additional));
        });
    section_edit
}

/// Explains whether an Animation Controller will animate or stay in its rest pose.
///
/// Animation Graph and Animation Set references are a pair under ADR 0085.
/// Keeping this notice beside their pickers makes the inactive or invalid
/// state visible without requiring the author to switch to the Problems panel.
fn show_animation_controller_assignment_notice(
    ui: &mut egui::Ui,
    fields: &std::collections::BTreeMap<String, Value>,
) {
    let has_animation_set = matches!(fields.get("animation_set"), Some(Value::AssetRef(_)));
    let has_graph = matches!(fields.get("graph"), Some(Value::AssetRef(_)));
    match (has_animation_set, has_graph) {
        (false, false) => {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Rest pose only. Assign both an Animation Set and Animation Graph to enable playback.",
            );
        }
        (false, true) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 92, 92),
                "Animation Set is required when an Animation Graph is assigned.",
            );
        }
        (true, false) => {
            ui.colored_label(
                egui::Color32::from_rgb(230, 92, 92),
                "Animation Graph is required when an Animation Set is assigned.",
            );
        }
        (true, true) => {}
    }
}
