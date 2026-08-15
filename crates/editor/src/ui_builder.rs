//! Visual authoring surface for declarative UI documents.
//!
//! The builder translates palette, hierarchy, and Inspector gestures into
//! [`UiDocumentCommand`] values. It never writes JSON directly and therefore
//! shares validation and undo semantics with other authoring adapters.

use crate::session::{EditorSession, EditorSessionError};
use eframe::egui;
use engine_authoring::{
    find_ui_node, UiAnchor, UiDocumentCommand, UiElementConstraints, UiLayout, UiNode, UiNodeKind,
    UiNumber, UiScaleMatch, UiScalePolicy, UiString,
};
use std::collections::BTreeSet;

/// Persistent presentation state for one editor UI Builder surface.
#[derive(Default)]
pub struct UiBuilderState {
    /// Document node selected by the hierarchy or preview.
    pub selected_node: Option<String>,
    /// Complete multi-selection used by batch UI operations.
    pub selected_nodes: BTreeSet<String>,
    /// Shift-range selection anchor.
    pub selection_anchor: Option<String>,
    /// Case-insensitive hierarchy filter.
    pub hierarchy_filter: String,
    /// Index into the built-in preview resolution list.
    pub preview_preset: usize,
    /// Scale used to fit large logical resolutions into the editor window.
    pub preview_scale: f32,
    /// Freely resizable logical preview dimensions.
    pub custom_preview_size: [f32; 2],
    /// Draws the document reference-resolution boundary.
    pub show_reference_bounds: bool,
    /// Draws the resolved safe-area boundary.
    pub show_safe_area: bool,
    /// Allows preview widgets to consume input without changing selection.
    pub preview_interactions: bool,
    /// Transient binding name to sample value table used only by the
    /// preview; never serialized into the UI document.
    pub preview_bindings: std::collections::BTreeMap<String, String>,
    /// Project root anchoring relative image sources in the preview.
    pub preview_base_path: Option<std::path::PathBuf>,
    /// Registered texture paths (relative to `assets/`) offered by the
    /// Image source picker; refreshed by the editor shell each frame.
    pub texture_source_choices: Vec<String>,
    /// Node that should be revealed after creation or programmatic selection.
    reveal_node: Option<String>,
    /// Short result message for the most recent structural edit.
    edit_feedback: Option<String>,
    clipboard: Option<UiNode>,
}

impl UiBuilderState {
    /// Creates builder state with a readable default preview scale.
    pub fn new() -> Self {
        Self {
            preview_scale: 0.5,
            custom_preview_size: [1440.0, 900.0],
            show_reference_bounds: true,
            show_safe_area: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy)]
enum PaletteKind {
    Panel,
    Text,
    Button,
    Spacer,
    Image,
    ProgressBar,
    Stack,
    Grid,
    Overlay,
    ScrollView,
}

/// Describes where a newly created node will be inserted.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InsertionTarget {
    /// Container that receives the new child.
    parent: String,
    /// Human-readable explanation shown before the user creates the node.
    description: String,
}

#[derive(Clone, Copy)]
enum BatchUiAction {
    Duplicate,
    Delete,
    MoveToRoot,
    MoveUp,
    MoveDown,
    AlignX,
    AlignY,
    DistributeX,
    DistributeY,
}

/// Draws the palette, hierarchy, and responsive preview canvas.
pub fn show_ui_builder(
    ui: &mut egui::Ui,
    session: &mut EditorSession,
    state: &mut UiBuilderState,
) -> Result<(), EditorSessionError> {
    let Some(document) = session.ui_document().cloned() else {
        ui.label("No UI document is open.");
        return Ok(());
    };
    state
        .selected_nodes
        .retain(|selected| find_ui_node(&document, selected).is_some());
    if state
        .selected_node
        .as_ref()
        .is_none_or(|selected| find_ui_node(&document, selected).is_none())
    {
        state.selected_node = Some(document.root.id.clone());
        state.selected_nodes.clear();
        state.selected_nodes.insert(document.root.id.clone());
        state.selection_anchor = Some(document.root.id.clone());
    }

    let mut command = None;
    let builder_height = ui.available_height();
    ui.horizontal(|ui| {
        // Keep the structure tree usable even when the fitted preview is
        // short. Without an explicit minimum, the horizontal row shrink-wraps
        // to the canvas and clips most hierarchy entries into a tiny scroller.
        ui.set_min_height(builder_height);
        ui.vertical(|ui| {
            ui.set_width(220.0);
            ui.strong("UI Structure");
            let insertion_target = insertion_target(&document.root, state);
            ui.label(
                egui::RichText::new(format!("Destination: {}", insertion_target.description))
                    .color(egui::Color32::LIGHT_BLUE),
            );
            let mut requested_kind = None;
            ui.menu_button("+ Add Element", |ui| {
                ui.set_min_width(300.0);
                ui.strong(format!("Add to {}", insertion_target.description));
                ui.small("The new element will be selected and revealed automatically.");
                ui.separator();

                ui.strong("Content");
                for kind in [
                    PaletteKind::Text,
                    PaletteKind::Image,
                    PaletteKind::Button,
                    PaletteKind::ProgressBar,
                ] {
                    if palette_menu_entry(ui, kind) {
                        requested_kind = Some(kind);
                    }
                }

                ui.separator();
                ui.strong("Layout Containers");
                for kind in [
                    PaletteKind::Panel,
                    PaletteKind::Stack,
                    PaletteKind::Grid,
                    PaletteKind::Overlay,
                    PaletteKind::ScrollView,
                ] {
                    if palette_menu_entry(ui, kind) {
                        requested_kind = Some(kind);
                    }
                }

                ui.separator();
                ui.strong("Layout Helper");
                if palette_menu_entry(ui, PaletteKind::Spacer) {
                    requested_kind = Some(PaletteKind::Spacer);
                }
            });
            if let Some(kind) = requested_kind {
                let label = palette_label(kind);
                let id = unique_node_id(&document.root, label.to_ascii_lowercase());
                command = Some(UiDocumentCommand::InsertNode {
                    parent: insertion_target.parent.clone(),
                    index: usize::MAX,
                    node: palette_node(kind, id.clone()),
                });
                state.selected_node = Some(id.clone());
                state.selected_nodes.clear();
                state.selected_nodes.insert(id.clone());
                state.selection_anchor = Some(id.clone());
                state.reveal_node = Some(id.clone());
                state.edit_feedback = Some(format!(
                    "Added {label} to {}. Edit it in Properties.",
                    insertion_target.description
                ));
            }
            if let Some(feedback) = state.edit_feedback.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(feedback).small());
                        if ui.small_button("Dismiss").clicked() {
                            state.edit_feedback = None;
                        }
                    });
                });
            }
            ui.separator();
            egui::CollapsingHeader::new("Responsive Document")
                .default_open(false)
                .show(ui, |ui| {
                    let mut reference = document.reference_resolution;
                    let mut policy = document.scale_policy.clone();
                    let mut safe = document.safe_area_padding;
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label("Reference");
                        changed |= ui
                            .add(egui::DragValue::new(&mut reference[0]).range(1.0..=16384.0))
                            .changed();
                        ui.label("x");
                        changed |= ui
                            .add(egui::DragValue::new(&mut reference[1]).range(1.0..=16384.0))
                            .changed();
                    });
                    egui::ComboBox::from_label("Scale Policy")
                        .selected_text(scale_policy_label(&policy))
                        .show_ui(ui, |ui| {
                            changed |= ui
                                .selectable_value(
                                    &mut policy,
                                    UiScalePolicy::ConstantPixels,
                                    "Constant Pixels",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut policy,
                                    UiScalePolicy::ScaleWithViewport {
                                        match_axis: UiScaleMatch::Width,
                                        blend: 0.5,
                                    },
                                    "Scale with Width",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut policy,
                                    UiScalePolicy::ScaleWithViewport {
                                        match_axis: UiScaleMatch::Height,
                                        blend: 0.5,
                                    },
                                    "Scale with Height",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut policy,
                                    UiScalePolicy::ScaleWithViewport {
                                        match_axis: UiScaleMatch::WidthHeight,
                                        blend: 0.5,
                                    },
                                    "Width / Height Blend",
                                )
                                .changed();
                            changed |= ui
                                .selectable_value(
                                    &mut policy,
                                    UiScalePolicy::ConstantPhysicalSize {
                                        reference_dpi: 96.0,
                                    },
                                    "Constant Physical Size",
                                )
                                .changed();
                        });
                    if let UiScalePolicy::ScaleWithViewport {
                        match_axis: UiScaleMatch::WidthHeight,
                        blend,
                    } = &mut policy
                    {
                        changed |= ui
                            .add(egui::Slider::new(blend, 0.0..=1.0).text("Height Blend"))
                            .changed();
                    }
                    if let UiScalePolicy::ConstantPhysicalSize { reference_dpi } = &mut policy {
                        changed |= ui
                            .add(
                                egui::DragValue::new(reference_dpi)
                                    .range(1.0..=1000.0)
                                    .prefix("DPI "),
                            )
                            .changed();
                    }
                    ui.label("Safe Area (L/T/R/B)");
                    ui.horizontal(|ui| {
                        for value in &mut safe {
                            changed |= ui
                                .add(egui::DragValue::new(value).range(0.0..=4096.0))
                                .changed();
                        }
                    });
                    if changed {
                        command = Some(UiDocumentCommand::SetResponsiveSettings {
                            reference_resolution: reference,
                            scale_policy: policy,
                            safe_area_padding: safe,
                        });
                    }
                });
            ui.separator();
            show_preview_bindings_table(ui, &document, state);
            ui.separator();
            ui.strong("UI Tree");
            ui.add(
                egui::TextEdit::singleline(&mut state.hierarchy_filter).hint_text("Search nodes"),
            );
            egui::ScrollArea::vertical()
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    let mut visible_ids = Vec::new();
                    collect_visible_ids(
                        &document.root,
                        state.hierarchy_filter.trim(),
                        &mut visible_ids,
                    );
                    show_hierarchy_node(ui, &document.root, 0, state, &visible_ids);
                });
        });

        ui.separator();
        ui.vertical(|ui| {
            show_preview_toolbar(ui, state);
            ui.separator();
            show_preview(ui, &document, state);
        });
    });

    if let Some(command) = command {
        session.apply_ui_command(command)?;
    }
    Ok(())
}

/// Editable sample values for every binding referenced by the document.
///
/// Without sample values a bound Text renders literally as `{name}`, so
/// layout could not be judged with realistic content until Play. The table
/// lives in transient editor state and is never saved to the document.
fn show_preview_bindings_table(
    ui: &mut egui::Ui,
    document: &engine_authoring::UiDocument,
    state: &mut UiBuilderState,
) {
    egui::CollapsingHeader::new("Preview Bindings")
        .default_open(false)
        .show(ui, |ui| {
            ui.small("Sample values shown only in this preview.");
            let mut names = BTreeSet::new();
            collect_binding_names(&document.root, &mut names);
            if names.is_empty() {
                ui.label("No bindings in this document");
                return;
            }
            for name in names {
                ui.horizontal(|ui| {
                    ui.monospace(&name);
                    let entry = state.preview_bindings.entry(name.clone()).or_default();
                    ui.add(egui::TextEdit::singleline(entry).desired_width(120.0));
                });
            }
        });
}

/// Collects every binding name referenced by a document subtree.
fn collect_binding_names(node: &UiNode, names: &mut BTreeSet<String>) {
    match &node.kind {
        UiNodeKind::Text {
            content: UiString::Bind(name),
            ..
        }
        | UiNodeKind::Button {
            label: UiString::Bind(name),
            ..
        } => {
            names.insert(name.clone());
        }
        UiNodeKind::ProgressBar { value, maximum, .. } => {
            for number in [value, maximum] {
                if let UiNumber::Bind(name) = number {
                    names.insert(name.clone());
                }
            }
        }
        _ => {}
    }
    for child in &node.children {
        collect_binding_names(child, names);
    }
}

/// Draws the Inspector for the currently selected UI node.
pub fn show_ui_builder_inspector(
    ui: &mut egui::Ui,
    session: &mut EditorSession,
    state: &mut UiBuilderState,
) -> Result<(), EditorSessionError> {
    ui.heading("Properties");
    let Some(document) = session.ui_document().cloned() else {
        ui.label("No UI document is open.");
        return Ok(());
    };
    let Some(selected) = state.selected_node.clone() else {
        ui.label("Select a node in the Hierarchy or Canvas.");
        return Ok(());
    };
    let Some(node) = find_ui_node(&document, &selected).cloned() else {
        state.selected_node = None;
        return Ok(());
    };

    ui.horizontal_wrapped(|ui| {
        ui.strong(node_kind_badge(&node.kind));
        ui.monospace(&node.id);
    });
    if let Some(path) = ui_node_path(&document.root, &selected) {
        ui.small(format!("UI / {}", path.join(" / ")));
    }

    if state.selected_nodes.is_empty() {
        state.selected_nodes.insert(selected.clone());
    }
    let selected_roots = selected_root_ids(&document.root, &state.selected_nodes);
    if state.selected_nodes.len() > 1 {
        ui.label(format!("{} nodes selected", state.selected_nodes.len()));
        ui.small("Batch operations validate the complete command list and create one undo step.");
        let mut batch = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button("Duplicate Set").clicked() {
                batch = Some(BatchUiAction::Duplicate);
            }
            if ui.button("Delete Set").clicked() {
                batch = Some(BatchUiAction::Delete);
            }
            if ui.button("Move to Root").clicked() {
                batch = Some(BatchUiAction::MoveToRoot);
            }
            if ui.button("Up").clicked() {
                batch = Some(BatchUiAction::MoveUp);
            }
            if ui.button("Down").clicked() {
                batch = Some(BatchUiAction::MoveDown);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Align X").clicked() {
                batch = Some(BatchUiAction::AlignX);
            }
            if ui.button("Align Y").clicked() {
                batch = Some(BatchUiAction::AlignY);
            }
            if ui.button("Distribute X").clicked() {
                batch = Some(BatchUiAction::DistributeX);
            }
            if ui.button("Distribute Y").clicked() {
                batch = Some(BatchUiAction::DistributeY);
            }
        });
        if let Some(batch) = batch {
            match batch {
                BatchUiAction::Duplicate => {
                    let mut reserved = BTreeSet::new();
                    collect_all_ids(&document.root, &mut reserved);
                    let mut commands = Vec::new();
                    let mut new_selection = BTreeSet::new();
                    for id in &selected_roots {
                        let Some(source) = find_node(&document.root, id) else {
                            continue;
                        };
                        let Some((parent, index)) = find_parent_and_index(&document.root, id)
                        else {
                            continue;
                        };
                        let mut copy = source.clone();
                        remap_subtree_ids(&mut copy, &mut reserved);
                        new_selection.insert(copy.id.clone());
                        commands.push(UiDocumentCommand::InsertNode {
                            parent: parent.to_owned(),
                            index: index + 1,
                            node: copy,
                        });
                    }
                    session.apply_ui_commands(commands)?;
                    state.selected_nodes = new_selection;
                    state.selected_node = state.selected_nodes.iter().next().cloned();
                    return Ok(());
                }
                BatchUiAction::Delete => {
                    let commands = selected_roots
                        .iter()
                        .filter(|id| **id != document.root.id)
                        .map(|node| UiDocumentCommand::RemoveNode { node: node.clone() })
                        .collect::<Vec<_>>();
                    session.apply_ui_commands(commands)?;
                    state.selected_nodes.clear();
                    state.selected_node = None;
                    return Ok(());
                }
                BatchUiAction::MoveToRoot => {
                    let commands = selected_roots
                        .iter()
                        .filter(|id| **id != document.root.id)
                        .map(|node| UiDocumentCommand::MoveNode {
                            node: node.clone(),
                            parent: document.root.id.clone(),
                            index: usize::MAX,
                        })
                        .collect::<Vec<_>>();
                    session.apply_ui_commands(commands)?;
                    return Ok(());
                }
                BatchUiAction::MoveUp | BatchUiAction::MoveDown => {
                    if let Some((parent, index)) = find_parent_and_index(&document.root, &selected)
                    {
                        let next = if matches!(batch, BatchUiAction::MoveUp) {
                            index.saturating_sub(1)
                        } else {
                            index.saturating_add(1)
                        };
                        session.apply_ui_command(UiDocumentCommand::MoveNode {
                            node: selected.clone(),
                            parent: parent.to_owned(),
                            index: next,
                        })?;
                    }
                    return Ok(());
                }
                BatchUiAction::AlignX
                | BatchUiAction::AlignY
                | BatchUiAction::DistributeX
                | BatchUiAction::DistributeY => {
                    let axis = usize::from(matches!(
                        batch,
                        BatchUiAction::AlignY | BatchUiAction::DistributeY
                    ));
                    let distribute = matches!(
                        batch,
                        BatchUiAction::DistributeX | BatchUiAction::DistributeY
                    );
                    let mut panels = state
                        .selected_nodes
                        .iter()
                        .filter_map(|id| find_node(&document.root, id).cloned())
                        .filter_map(|node| ui_panel_offset(&node).map(|offset| (node, offset)))
                        .collect::<Vec<_>>();
                    panels.sort_by(|left, right| left.1[axis].total_cmp(&right.1[axis]));
                    let minimum = panels.first().map_or(0.0, |item| item.1[axis]);
                    let maximum = panels.last().map_or(minimum, |item| item.1[axis]);
                    let denominator = panels.len().saturating_sub(1).max(1) as f32;
                    let commands = panels
                        .into_iter()
                        .enumerate()
                        .map(|(index, (mut node, mut offset))| {
                            offset[axis] = if distribute {
                                minimum + (maximum - minimum) * index as f32 / denominator
                            } else {
                                minimum
                            };
                            set_ui_panel_offset(&mut node, offset);
                            UiDocumentCommand::ReplaceNode {
                                node: node.id.clone(),
                                replacement: node,
                            }
                        })
                        .collect::<Vec<_>>();
                    session.apply_ui_commands(commands)?;
                    return Ok(());
                }
            }
        }
        ui.separator();
    }

    let mut replacement = node.clone();
    let mut changed = false;
    let mut renamed = None;
    let mut id = replacement.id.clone();
    let mut constraints = document.constraints.get(&selected).cloned();
    let mut constraints_enabled = constraints.is_some();
    let mut constraints_changed = false;
    egui::CollapsingHeader::new("Responsive Layout")
        .default_open(false)
        .show(ui, |ui| {
            constraints_changed |= ui
                .checkbox(&mut constraints_enabled, "Enable constraints")
                .changed();
            if constraints_enabled {
                let value = constraints.get_or_insert_with(UiElementConstraints::default);
                constraints_changed |=
                    edit_optional_vec2(ui, "Minimum Size", &mut value.minimum_size);
                constraints_changed |=
                    edit_optional_vec2(ui, "Maximum Size", &mut value.maximum_size);
                constraints_changed |=
                    edit_optional_float(ui, "Aspect Ratio", &mut value.aspect_ratio, 0.01..=100.0);
                constraints_changed |=
                    edit_optional_vec2_range(ui, "Anchor Min", &mut value.anchor_min, 0.0..=1.0);
                constraints_changed |=
                    edit_optional_vec2_range(ui, "Anchor Max", &mut value.anchor_max, 0.0..=1.0);
            } else {
                constraints = None;
            }
        });
    if constraints_changed {
        session.apply_ui_command(UiDocumentCommand::SetNodeConstraints {
            node: selected.clone(),
            constraints,
        })?;
        return Ok(());
    }
    ui.separator();
    ui.strong("Element Settings");

    match &mut replacement.kind {
        UiNodeKind::Panel {
            anchor,
            offset_x,
            offset_y,
            layout,
            spacing,
            padding,
            background,
        } => {
            changed |= enum_combo(ui, "Anchor", anchor, &all_anchors());
            changed |= enum_combo(
                ui,
                "Layout",
                layout,
                &[UiLayout::Vertical, UiLayout::Horizontal],
            );
            changed |= drag(ui, "Offset X", offset_x, " px");
            changed |= drag(ui, "Offset Y", offset_y, " px");
            changed |= drag(ui, "Spacing", spacing, " px");
            changed |= drag(ui, "Padding", padding, " px");
            let mut enabled = background.is_some();
            if ui.checkbox(&mut enabled, "Background").changed() {
                *background = enabled.then_some([0.1, 0.1, 0.1, 0.9]);
                changed = true;
            }
            if let Some(color) = background {
                changed |= ui.color_edit_button_rgba_unmultiplied(color).changed();
            }
        }
        UiNodeKind::Text {
            content,
            size,
            color,
        } => {
            changed |= edit_ui_string(ui, "Content", content);
            changed |= drag(ui, "Font Size", size, " pt");
            changed |= ui.color_edit_button_rgba_unmultiplied(color).changed();
        }
        UiNodeKind::Button { label, event } => {
            changed |= edit_ui_string(ui, "Label", label);
            ui.label("Event");
            changed |= ui.text_edit_singleline(event).changed();
        }
        UiNodeKind::Spacer { size } => changed |= drag(ui, "Size", size, " px"),
        UiNodeKind::Image {
            source,
            width,
            height,
            tint,
            nine_slice,
        } => {
            ui.label("Image Source");
            ui.horizontal(|ui| {
                changed |= ui.text_edit_singleline(source).changed();
                if !state.texture_source_choices.is_empty() {
                    egui::ComboBox::from_id_salt(("ui_image_source_picker", &selected))
                        .selected_text("Pick…")
                        .show_ui(ui, |ui| {
                            for choice in &state.texture_source_choices {
                                if ui.selectable_label(false, choice).clicked() {
                                    // Sources stay project-root-relative with
                                    // the assets/ prefix so packaged games can
                                    // resolve them from their package root.
                                    *source = format!("assets/{choice}");
                                    changed = true;
                                }
                            }
                        });
                }
            });
            changed |= drag(ui, "Width", width, " px");
            changed |= drag(ui, "Height", height, " px");
            changed |= ui.color_edit_button_rgba_unmultiplied(tint).changed();
            let mut enabled = nine_slice.is_some();
            if ui.checkbox(&mut enabled, "Nine-slice").changed() {
                *nine_slice = enabled.then_some([8.0; 4]);
                changed = true;
            }
            if let Some(border) = nine_slice {
                for (index, label) in ["Left", "Top", "Right", "Bottom"].iter().enumerate() {
                    changed |= drag(ui, label, &mut border[index], " px");
                }
            }
        }
        UiNodeKind::ProgressBar {
            value,
            maximum,
            width,
            height,
            fill,
            background,
            show_label,
        } => {
            changed |= edit_ui_number(ui, "Value", value);
            changed |= edit_ui_number(ui, "Maximum", maximum);
            changed |= drag(ui, "Width", width, " px");
            changed |= drag(ui, "Height", height, " px");
            ui.label("Fill");
            changed |= ui.color_edit_button_rgba_unmultiplied(fill).changed();
            ui.label("Background");
            changed |= ui.color_edit_button_rgba_unmultiplied(background).changed();
            changed |= ui.checkbox(show_label, "Show numeric label").changed();
        }
        UiNodeKind::Stack {
            direction,
            spacing,
            padding,
            background,
        } => {
            changed |= enum_combo(
                ui,
                "Direction",
                direction,
                &[UiLayout::Vertical, UiLayout::Horizontal],
            );
            changed |= drag(ui, "Spacing", spacing, " px");
            changed |= drag(ui, "Padding", padding, " px");
            changed |= edit_optional_color(ui, background);
        }
        UiNodeKind::Grid {
            columns,
            spacing,
            padding,
            background,
        } => {
            ui.horizontal(|ui| {
                ui.label("Columns");
                changed |= ui
                    .add(egui::DragValue::new(columns).range(1..=32))
                    .changed();
            });
            changed |= drag(ui, "Spacing", spacing, " px");
            changed |= drag(ui, "Padding", padding, " px");
            changed |= edit_optional_color(ui, background);
        }
        UiNodeKind::Overlay {
            padding,
            background,
        } => {
            changed |= drag(ui, "Padding", padding, " px");
            changed |= edit_optional_color(ui, background);
        }
        UiNodeKind::ScrollView {
            horizontal,
            vertical,
            max_width,
            max_height,
        } => {
            changed |= ui.checkbox(horizontal, "Horizontal scrolling").changed();
            changed |= ui.checkbox(vertical, "Vertical scrolling").changed();
            changed |= edit_optional_size(ui, "Max Width", max_width);
            changed |= edit_optional_size(ui, "Max Height", max_height);
        }
    }

    ui.separator();
    egui::CollapsingHeader::new("Advanced")
        .default_open(false)
        .show(ui, |ui| {
            ui.label("Node ID");
            if ui.text_edit_singleline(&mut id).changed() && !id.trim().is_empty() {
                renamed = Some(id.trim().to_owned());
            }
            ui.small("Bindings and responsive constraints use this stable identifier.");
        });
    let is_root = selected == document.root.id;
    let mut paste_copy = None;
    let mut delete_requested = false;
    ui.horizontal(|ui| {
        if ui.button("Copy").clicked() {
            state.clipboard = Some(node.clone());
        }
        if ui
            .add_enabled(state.clipboard.is_some(), egui::Button::new("Paste Child"))
            .clicked()
            && let Some(mut copy) = state.clipboard.clone() {
                copy.id = unique_node_id(&document.root, copy.id);
                if node.kind.is_container() {
                    paste_copy = Some(copy);
                }
            }
        if ui
            .add_enabled(!is_root, egui::Button::new("Delete"))
            .clicked()
        {
            delete_requested = true;
        }
    });
    if let Some(copy) = paste_copy {
        let new_id = copy.id.clone();
        session.apply_ui_command(UiDocumentCommand::InsertNode {
            parent: selected.clone(),
            index: usize::MAX,
            node: copy,
        })?;
        state.selected_node = Some(new_id);
        state.selected_nodes.clear();
        state
            .selected_nodes
            .insert(state.selected_node.clone().unwrap());
        return Ok(());
    }
    if delete_requested {
        session.apply_ui_command(UiDocumentCommand::RemoveNode {
            node: selected.clone(),
        })?;
        state.selected_node = None;
        state.selected_nodes.clear();
        return Ok(());
    }

    if let Some(new_id) = renamed
        && new_id != selected {
            session.apply_ui_command(UiDocumentCommand::RenameNode {
                node: selected,
                new_id: new_id.clone(),
            })?;
            state.selected_node = Some(new_id);
            state.selected_nodes.clear();
            state
                .selected_nodes
                .insert(state.selected_node.clone().unwrap());
            return Ok(());
        }
    if changed {
        session.apply_ui_command(UiDocumentCommand::ReplaceNode {
            node: selected,
            replacement,
        })?;
    }
    Ok(())
}

fn show_preview_toolbar(ui: &mut egui::Ui, state: &mut UiBuilderState) {
    const PRESETS: [(&str, [f32; 2]); 7] = [
        ("Desktop FHD", [1920.0, 1080.0]),
        ("Desktop HD", [1280.0, 720.0]),
        ("Ultrawide", [3440.0, 1440.0]),
        ("Handheld", [1280.0, 800.0]),
        ("Classic 4:3", [1024.0, 768.0]),
        ("Portrait", [1080.0, 1920.0]),
        ("Custom", [0.0, 0.0]),
    ];
    ui.horizontal_wrapped(|ui| {
        ui.strong("Canvas");
        ui.selectable_value(&mut state.preview_interactions, false, "Design")
            .on_hover_text("Click the deepest visible element to select it");
        ui.selectable_value(&mut state.preview_interactions, true, "Interact")
            .on_hover_text("Use buttons and scroll areas without changing selection");
        ui.separator();
        egui::ComboBox::from_id_salt("ui_preview_resolution")
            .selected_text(PRESETS[state.preview_preset.min(PRESETS.len() - 1)].0)
            .show_ui(ui, |ui| {
                for (index, (label, _)) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut state.preview_preset, index, *label);
                }
            });
        ui.add(
            egui::Slider::new(&mut state.preview_scale, 0.2..=1.0)
                .text("Zoom")
                .logarithmic(true),
        );
        if ui.small_button("Fit").clicked() {
            state.preview_scale = 0.5;
        }
        ui.checkbox(&mut state.show_reference_bounds, "Reference");
        ui.checkbox(&mut state.show_safe_area, "Safe Area");
    });
    if state.preview_preset == PRESETS.len() - 1 {
        ui.horizontal(|ui| {
            ui.label("Custom Size");
            ui.add(egui::DragValue::new(&mut state.custom_preview_size[0]).range(240.0..=8192.0));
            ui.label("x");
            ui.add(egui::DragValue::new(&mut state.custom_preview_size[1]).range(240.0..=8192.0));
        });
    }
}

fn show_preview(
    ui: &mut egui::Ui,
    document: &engine_authoring::UiDocument,
    state: &mut UiBuilderState,
) {
    const SIZES: [[f32; 2]; 6] = [
        [1920.0, 1080.0],
        [1280.0, 720.0],
        [3440.0, 1440.0],
        [1280.0, 800.0],
        [1024.0, 768.0],
        [1080.0, 1920.0],
    ];
    let logical = SIZES
        .get(state.preview_preset)
        .copied()
        .unwrap_or(state.custom_preview_size);
    let available = ui.available_size();
    if available.x < 64.0 || available.y < 64.0 {
        ui.centered_and_justified(|ui| {
            ui.label("Expand the UI Builder canvas to show the preview.");
        });
        return;
    }
    let fit = (available.x / logical[0]).min(available.y / logical[1]);
    let scale = (fit * state.preview_scale.max(0.2) * 2.0).min(fit);
    let size = egui::vec2(logical[0] * scale, logical[1] * scale);
    let bindings = preview_bindings(state);
    egui::Frame::canvas(ui.style()).show(ui, |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        let canvas = ui.max_rect();

        // Draw the document through the runtime interpreter (ADR 0073), telling
        // it that this canvas presents a `logical`-sized screen: the zoom then
        // scales the whole document instead of leaving authored pixel sizes at
        // canvas scale, so what the canvas shows is the chosen resolution
        // (ADR 0090).
        let viewport = engine::UiViewport::scaled(canvas, egui::vec2(logical[0], logical[1]));
        let report = engine::ui_document::draw_ui_document_with_frame(
            ui.ctx(),
            document,
            engine::UiDrawFrame {
                bindings: &bindings,
                events: None,
                diagnostics: None,
                viewport,
                base_path: state.preview_base_path.as_deref(),
            },
            engine::UiDocumentDrawOptions::editor_preview(),
        );

        // Guides, selection, and hover paint above the interpreter's Area layers
        // so opaque panel fills cannot hide them.
        let overlay = ui
            .ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("ui_builder_preview_overlay"),
            ))
            .with_clip_rect(canvas);

        if state.show_reference_bounds {
            overlay.rect_stroke(
                canvas,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(90, 150, 230)),
                egui::StrokeKind::Inside,
            );
        }
        if state.show_safe_area {
            // Mirror the interpreter's safe-area math: padding is in logical
            // units, so it takes the same document scale the draw used.
            let document_scale = engine::ui_document_scale(document, viewport);
            let safe = document
                .safe_area_padding
                .map(|value| value * document_scale);
            let safe_rect = egui::Rect::from_min_max(
                canvas.min + egui::vec2(safe[0], safe[1]),
                canvas.max - egui::vec2(safe[2], safe[3]),
            );
            overlay.rect_stroke(
                safe_rect,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(80, 220, 150)),
                egui::StrokeKind::Inside,
            );
        }

        for node in &report.nodes {
            let selected = state.selected_nodes.contains(&node.node_id)
                || state.selected_node.as_deref() == Some(node.node_id.as_str());
            if selected {
                overlay.rect_stroke(
                    node.rect,
                    2.0,
                    egui::Stroke::new(2.0_f32, egui::Color32::LIGHT_BLUE),
                    egui::StrokeKind::Outside,
                );
            }
        }

        if !state.preview_interactions {
            let pointer = ui.ctx().pointer_interact_pos();
            if let Some(hovered) =
                pointer.and_then(|position| frontmost_preview_node(&report.nodes, position))
            {
                overlay.rect_stroke(
                    hovered.rect,
                    2.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(130)),
                    egui::StrokeKind::Outside,
                );
            }
            if let Some(position) = preview_click_position(ui.ctx(), canvas)
                && let Some(hit) = frontmost_preview_node(&report.nodes, position) {
                    let id = hit.node_id.clone();
                    let modifiers = ui.input(|input| input.modifiers);
                    update_ui_selection(state, id, modifiers);
                }
        }
    });
}

/// Builds a runtime binding table from the transient Preview Bindings values.
///
/// Empty entries are skipped so the interpreter renders its own missing-binding
/// placeholder, matching Play. Values are supplied as text; the interpreter
/// parses them for numeric bindings, so one table serves both text and numbers.
fn preview_bindings(state: &UiBuilderState) -> engine::UiBindings {
    let mut bindings = engine::UiBindings::new();
    for (name, value) in &state.preview_bindings {
        if !value.is_empty() {
            bindings.set(name.clone(), engine::UiBindingValue::Text(value.clone()));
        }
    }
    bindings
}

/// Picks the front-most recorded node covering `position`.
///
/// The interpreter paints containers before their children, so the largest
/// `draw_order` under the pointer is the innermost, front-most node — the one
/// the user sees and expects to select. This mirrors Scene View's
/// `frontmost_ui_region`, keeping selection consistent across both surfaces.
fn frontmost_preview_node(
    nodes: &[engine::UiNodeDrawRecord],
    position: egui::Pos2,
) -> Option<&engine::UiNodeDrawRecord> {
    nodes
        .iter()
        .filter(|node| node.rect.contains(position))
        .max_by_key(|node| node.draw_order)
}

/// Reads a raw primary click inside the preview canvas.
///
/// Widgets such as buttons consume their own click response even in design
/// mode, so selection reads the pointer state directly.
fn preview_click_position(ctx: &egui::Context, canvas: egui::Rect) -> Option<egui::Pos2> {
    ctx.input(|input| {
        input
            .pointer
            .button_clicked(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
            .filter(|position| canvas.contains(*position))
    })
}

fn show_hierarchy_node(
    ui: &mut egui::Ui,
    node: &UiNode,
    depth: usize,
    state: &mut UiBuilderState,
    visible_ids: &[String],
) {
    let filter = state.hierarchy_filter.trim().to_ascii_lowercase();
    let matches = filter.is_empty()
        || node.id.to_ascii_lowercase().contains(&filter)
        || node
            .children
            .iter()
            .any(|child| subtree_matches(child, &filter));
    if !matches {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 14.0);
        let label = format!("{}  {}", node_kind_badge(&node.kind), node.id);
        let is_selected = state.selected_nodes.contains(&node.id)
            || state.selected_node.as_deref() == Some(node.id.as_str());
        let response = ui.selectable_label(is_selected, label);
        if state.reveal_node.as_deref() == Some(node.id.as_str()) {
            response.scroll_to_me(Some(egui::Align::Center));
            state.reveal_node = None;
        }
        if response.clicked() {
            let modifiers = ui.input(|input| input.modifiers);
            if modifiers.shift {
                let anchor = state
                    .selection_anchor
                    .as_ref()
                    .or(state.selected_node.as_ref())
                    .cloned();
                if let Some(anchor) = anchor
                    && let (Some(start), Some(end)) = (
                        visible_ids.iter().position(|id| id == &anchor),
                        visible_ids.iter().position(|id| id == &node.id),
                    ) {
                        if !modifiers.command {
                            state.selected_nodes.clear();
                        }
                        let (start, end) = if start <= end {
                            (start, end)
                        } else {
                            (end, start)
                        };
                        state
                            .selected_nodes
                            .extend(visible_ids[start..=end].iter().cloned());
                        state.selected_node = Some(node.id.clone());
                    }
            } else {
                update_ui_selection(state, node.id.clone(), modifiers);
            }
        }
    });
    for child in &node.children {
        show_hierarchy_node(ui, child, depth + 1, state, visible_ids);
    }
}

fn subtree_matches(node: &UiNode, filter: &str) -> bool {
    node.id.to_ascii_lowercase().contains(filter)
        || node
            .children
            .iter()
            .any(|child| subtree_matches(child, filter))
}

fn collect_visible_ids(node: &UiNode, filter: &str, output: &mut Vec<String>) {
    let filter = filter.to_ascii_lowercase();
    if !filter.is_empty() && !subtree_matches(node, &filter) {
        return;
    }
    output.push(node.id.clone());
    for child in &node.children {
        collect_visible_ids(child, &filter, output);
    }
}

fn collect_all_ids(node: &UiNode, output: &mut BTreeSet<String>) {
    output.insert(node.id.clone());
    for child in &node.children {
        collect_all_ids(child, output);
    }
}

fn update_ui_selection(state: &mut UiBuilderState, id: String, modifiers: egui::Modifiers) {
    if modifiers.command || modifiers.ctrl {
        if !state.selected_nodes.remove(&id) {
            state.selected_nodes.insert(id.clone());
        }
        state.selected_node = state
            .selected_nodes
            .contains(&id)
            .then_some(id.clone())
            .or_else(|| state.selected_nodes.iter().next().cloned());
    } else {
        state.selected_nodes.clear();
        state.selected_nodes.insert(id.clone());
        state.selected_node = Some(id.clone());
    }
    state.selection_anchor = Some(id);
}

fn find_parent_and_index<'a>(node: &'a UiNode, id: &str) -> Option<(&'a str, usize)> {
    if let Some(index) = node.children.iter().position(|child| child.id == id) {
        return Some((&node.id, index));
    }
    node.children
        .iter()
        .find_map(|child| find_parent_and_index(child, id))
}

fn subtree_contains_id(node: &UiNode, id: &str) -> bool {
    node.id == id
        || node
            .children
            .iter()
            .any(|child| subtree_contains_id(child, id))
}

fn selected_root_ids(root: &UiNode, selected: &BTreeSet<String>) -> Vec<String> {
    selected
        .iter()
        .filter(|id| {
            !selected.iter().any(|candidate| {
                candidate != *id
                    && find_node(root, candidate).is_some_and(|node| subtree_contains_id(node, id))
            })
        })
        .cloned()
        .collect()
}

fn remap_subtree_ids(node: &mut UiNode, reserved: &mut BTreeSet<String>) {
    let base = format!("{}_copy", node.id);
    let mut candidate = base.clone();
    let mut suffix = 2_u32;
    while reserved.contains(&candidate) {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    node.id = candidate.clone();
    reserved.insert(candidate);
    for child in &mut node.children {
        remap_subtree_ids(child, reserved);
    }
}

fn ui_panel_offset(node: &UiNode) -> Option<[f32; 2]> {
    if let UiNodeKind::Panel {
        offset_x, offset_y, ..
    } = &node.kind
    {
        Some([*offset_x, *offset_y])
    } else {
        None
    }
}

fn set_ui_panel_offset(node: &mut UiNode, offset: [f32; 2]) {
    if let UiNodeKind::Panel {
        offset_x, offset_y, ..
    } = &mut node.kind
    {
        *offset_x = offset[0];
        *offset_y = offset[1];
    }
}

fn insertion_target(root: &UiNode, state: &UiBuilderState) -> InsertionTarget {
    let Some(selected) = state.selected_node.as_deref() else {
        return InsertionTarget {
            parent: root.id.clone(),
            description: format!("inside \"{}\"", root.id),
        };
    };
    let Some(node) = find_node(root, selected) else {
        return InsertionTarget {
            parent: root.id.clone(),
            description: format!("inside \"{}\"", root.id),
        };
    };
    if node.kind.is_container() {
        return InsertionTarget {
            parent: node.id.clone(),
            description: format!("inside \"{}\"", node.id),
        };
    }
    find_parent_and_index(root, selected).map_or_else(
        || InsertionTarget {
            parent: root.id.clone(),
            description: format!("inside \"{}\"", root.id),
        },
        |(parent, _)| InsertionTarget {
            parent: parent.to_owned(),
            description: format!("beside \"{}\" in \"{}\"", node.id, parent),
        },
    )
}

fn palette_menu_entry(ui: &mut egui::Ui, kind: PaletteKind) -> bool {
    let label = format!("{}  -  {}", palette_label(kind), palette_description(kind));
    let clicked = ui
        .add_sized([ui.available_width(), 24.0], egui::Button::new(label))
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn palette_label(kind: PaletteKind) -> &'static str {
    match kind {
        PaletteKind::Panel => "Panel",
        PaletteKind::Text => "Text",
        PaletteKind::Button => "Button",
        PaletteKind::Spacer => "Spacer",
        PaletteKind::Image => "Image",
        PaletteKind::ProgressBar => "Progress",
        PaletteKind::Stack => "Stack",
        PaletteKind::Grid => "Grid",
        PaletteKind::Overlay => "Overlay",
        PaletteKind::ScrollView => "Scroll",
    }
}

fn palette_description(kind: PaletteKind) -> &'static str {
    match kind {
        PaletteKind::Panel => "Anchored top-level or nested layout container",
        PaletteKind::Text => "Text label using a literal or named binding",
        PaletteKind::Button => "Clickable button that emits a named UI event",
        PaletteKind::Spacer => "Fixed empty space inside a layout container",
        PaletteKind::Image => "Image element with optional nine-slice borders",
        PaletteKind::ProgressBar => "Bound or literal progress indicator",
        PaletteKind::Stack => "Vertical or horizontal child layout",
        PaletteKind::Grid => "Multi-column child layout",
        PaletteKind::Overlay => "Children drawn in the same layout region",
        PaletteKind::ScrollView => "Scrollable child container",
    }
}

fn find_node<'a>(node: &'a UiNode, id: &str) -> Option<&'a UiNode> {
    (node.id == id)
        .then_some(node)
        .or_else(|| node.children.iter().find_map(|child| find_node(child, id)))
}

fn ui_node_path(root: &UiNode, id: &str) -> Option<Vec<String>> {
    if root.id == id {
        return Some(vec![root.id.clone()]);
    }
    for child in &root.children {
        if let Some(mut path) = ui_node_path(child, id) {
            path.insert(0, root.id.clone());
            return Some(path);
        }
    }
    None
}

fn unique_node_id(root: &UiNode, base: String) -> String {
    let normalized = base
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let base = normalized.trim_matches('_');
    let base = if base.is_empty() { "node" } else { base };
    if find_node(root, base).is_none() {
        return base.to_owned();
    }
    (2_u32..)
        .map(|suffix| format!("{base}_{suffix}"))
        .find(|candidate| find_node(root, candidate).is_none())
        .unwrap_or_else(|| format!("{base}_copy"))
}

fn palette_node(kind: PaletteKind, id: String) -> UiNode {
    let kind = match kind {
        PaletteKind::Panel => UiNodeKind::Panel {
            anchor: UiAnchor::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
            layout: UiLayout::Vertical,
            spacing: 4.0,
            padding: 8.0,
            background: Some([0.08, 0.08, 0.1, 0.9]),
        },
        PaletteKind::Text => UiNodeKind::Text {
            content: UiString::Literal("Text".to_owned()),
            size: 16.0,
            color: [1.0; 4],
        },
        PaletteKind::Button => UiNodeKind::Button {
            label: UiString::Literal("Button".to_owned()),
            event: "button_clicked".to_owned(),
        },
        PaletteKind::Spacer => UiNodeKind::Spacer { size: 8.0 },
        PaletteKind::Image => UiNodeKind::Image {
            source: "assets/ui/image.png".to_owned(),
            width: 128.0,
            height: 128.0,
            tint: [1.0; 4],
            nine_slice: None,
        },
        PaletteKind::ProgressBar => UiNodeKind::ProgressBar {
            value: UiNumber::Literal(75.0),
            maximum: UiNumber::Literal(100.0),
            width: 200.0,
            height: 18.0,
            fill: [0.2, 0.8, 0.3, 1.0],
            background: [0.1, 0.1, 0.1, 0.8],
            show_label: true,
        },
        PaletteKind::Stack => UiNodeKind::Stack {
            direction: UiLayout::Vertical,
            spacing: 4.0,
            padding: 8.0,
            background: None,
        },
        PaletteKind::Grid => UiNodeKind::Grid {
            columns: 2,
            spacing: 4.0,
            padding: 8.0,
            background: None,
        },
        PaletteKind::Overlay => UiNodeKind::Overlay {
            padding: 0.0,
            background: Some([0.0, 0.0, 0.0, 0.5]),
        },
        PaletteKind::ScrollView => UiNodeKind::ScrollView {
            horizontal: false,
            vertical: true,
            max_width: Some(320.0),
            max_height: Some(240.0),
        },
    };
    UiNode {
        id,
        kind,
        children: Vec::new(),
    }
}

fn edit_ui_string(ui: &mut egui::Ui, label: &str, value: &mut UiString) -> bool {
    let mut binding = matches!(value, UiString::Bind(_));
    let mut changed = ui
        .checkbox(&mut binding, format!("{label} Binding"))
        .changed();
    let mut text = match value {
        UiString::Literal(text) | UiString::Bind(text) => text.clone(),
    };
    ui.label(if binding { "Binding name" } else { label });
    changed |= ui.text_edit_singleline(&mut text).changed();
    if changed {
        *value = if binding {
            UiString::Bind(text)
        } else {
            UiString::Literal(text)
        };
    }
    changed
}

fn edit_ui_number(ui: &mut egui::Ui, label: &str, value: &mut UiNumber) -> bool {
    let mut binding = matches!(value, UiNumber::Bind(_));
    let mut changed = ui
        .checkbox(&mut binding, format!("{label} Binding"))
        .changed();
    if binding {
        let mut name = match value {
            UiNumber::Bind(name) => name.clone(),
            UiNumber::Literal(_) => String::new(),
        };
        changed |= ui.text_edit_singleline(&mut name).changed();
        if changed {
            *value = UiNumber::Bind(name);
        }
    } else {
        let mut number = match value {
            UiNumber::Literal(value) => *value,
            UiNumber::Bind(_) => 0.0,
        };
        changed |= drag(ui, label, &mut number, "");
        if changed {
            *value = UiNumber::Literal(number);
        }
    }
    changed
}

fn edit_optional_color(ui: &mut egui::Ui, color: &mut Option<[f32; 4]>) -> bool {
    let mut enabled = color.is_some();
    let mut changed = ui.checkbox(&mut enabled, "Background").changed();
    if changed {
        *color = enabled.then_some([0.08, 0.08, 0.1, 0.9]);
    }
    if let Some(color) = color {
        changed |= ui.color_edit_button_rgba_unmultiplied(color).changed();
    }
    changed
}

fn edit_optional_size(ui: &mut egui::Ui, label: &str, value: &mut Option<f32>) -> bool {
    let mut enabled = value.is_some();
    let mut changed = ui
        .checkbox(&mut enabled, format!("Limit {label}"))
        .changed();
    if changed {
        *value = enabled.then_some(320.0);
    }
    if let Some(value) = value {
        changed |= drag(ui, label, value, " px");
    }
    changed
}

fn edit_optional_float(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<f32>,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let mut enabled = value.is_some();
    let mut changed = ui.checkbox(&mut enabled, label).changed();
    if enabled {
        let current = value.get_or_insert(1.0);
        changed |= ui.add(egui::DragValue::new(current).range(range)).changed();
    } else if changed {
        *value = None;
    }
    changed
}

fn edit_optional_vec2(ui: &mut egui::Ui, label: &str, value: &mut Option<[f32; 2]>) -> bool {
    edit_optional_vec2_range(ui, label, value, 0.0..=16384.0)
}

fn edit_optional_vec2_range(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<[f32; 2]>,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let mut enabled = value.is_some();
    let mut changed = ui.checkbox(&mut enabled, label).changed();
    if enabled {
        let current = value.get_or_insert([0.0; 2]);
        ui.horizontal(|ui| {
            changed |= ui
                .add(egui::DragValue::new(&mut current[0]).range(range.clone()))
                .changed();
            changed |= ui
                .add(egui::DragValue::new(&mut current[1]).range(range))
                .changed();
        });
    } else if changed {
        *value = None;
    }
    changed
}

fn drag(ui: &mut egui::Ui, label: &str, value: &mut f32, suffix: &str) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).speed(0.5).suffix(suffix))
            .changed()
    })
    .inner
}

fn enum_combo<T>(ui: &mut egui::Ui, label: &str, value: &mut T, values: &[T]) -> bool
where
    T: Copy + PartialEq + std::fmt::Debug,
{
    let mut changed = false;
    egui::ComboBox::from_label(label)
        .selected_text(format!("{value:?}"))
        .show_ui(ui, |ui| {
            for candidate in values {
                changed |= ui
                    .selectable_value(value, *candidate, format!("{candidate:?}"))
                    .changed();
            }
        });
    changed
}

fn all_anchors() -> [UiAnchor; 9] {
    [
        UiAnchor::TopLeft,
        UiAnchor::TopCenter,
        UiAnchor::TopRight,
        UiAnchor::CenterLeft,
        UiAnchor::Center,
        UiAnchor::CenterRight,
        UiAnchor::BottomLeft,
        UiAnchor::BottomCenter,
        UiAnchor::BottomRight,
    ]
}

fn scale_policy_label(policy: &UiScalePolicy) -> &'static str {
    match policy {
        UiScalePolicy::ConstantPixels => "Constant Pixels",
        UiScalePolicy::ScaleWithViewport {
            match_axis: UiScaleMatch::Width,
            ..
        } => "Scale with Width",
        UiScalePolicy::ScaleWithViewport {
            match_axis: UiScaleMatch::Height,
            ..
        } => "Scale with Height",
        UiScalePolicy::ScaleWithViewport {
            match_axis: UiScaleMatch::WidthHeight,
            ..
        } => "Width / Height Blend",
        UiScalePolicy::ConstantPhysicalSize { .. } => "Constant Physical Size",
    }
}

fn node_kind_badge(kind: &UiNodeKind) -> &'static str {
    match kind {
        UiNodeKind::Panel { .. } => "[Panel]",
        UiNodeKind::Text { .. } => "[Text]",
        UiNodeKind::Button { .. } => "[Button]",
        UiNodeKind::Spacer { .. } => "[Spacer]",
        UiNodeKind::Image { .. } => "[Image]",
        UiNodeKind::ProgressBar { .. } => "[Progress]",
        UiNodeKind::Stack { .. } => "[Stack]",
        UiNodeKind::Grid { .. } => "[Grid]",
        UiNodeKind::Overlay { .. } => "[Overlay]",
        UiNodeKind::ScrollView { .. } => "[Scroll]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_selection_adds_new_nodes_beside_the_leaf() {
        let mut document = engine_authoring::UiDocument::default();
        document.root.children.push(UiNode {
            id: "title".to_owned(),
            kind: UiNodeKind::Text {
                content: UiString::Literal("Title".to_owned()),
                size: 24.0,
                color: [1.0; 4],
            },
            children: Vec::new(),
        });
        let mut state = UiBuilderState::new();
        state.selected_node = Some("title".to_owned());

        let target = insertion_target(&document.root, &state);
        assert_eq!(target.parent, "root");
        assert_eq!(target.description, "beside \"title\" in \"root\"");
    }

    #[test]
    fn preview_click_prefers_the_deepest_node_over_its_container() {
        let nodes = vec![
            engine::UiNodeDrawRecord {
                node_id: "panel".to_owned(),
                rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
                draw_order: 0,
            },
            engine::UiNodeDrawRecord {
                node_id: "label".to_owned(),
                rect: egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(40.0, 20.0)),
                draw_order: 1,
            },
        ];

        let picked = frontmost_preview_node(&nodes, egui::pos2(20.0, 20.0))
            .map(|node| node.node_id.as_str());

        assert_eq!(picked, Some("label"));
    }

    #[test]
    fn preview_click_selects_container_outside_every_child() {
        let nodes = vec![
            engine::UiNodeDrawRecord {
                node_id: "panel".to_owned(),
                rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
                draw_order: 0,
            },
            engine::UiNodeDrawRecord {
                node_id: "label".to_owned(),
                rect: egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(40.0, 20.0)),
                draw_order: 1,
            },
        ];

        let picked = frontmost_preview_node(&nodes, egui::pos2(80.0, 80.0))
            .map(|node| node.node_id.as_str());

        assert_eq!(picked, Some("panel"));
        assert!(frontmost_preview_node(&nodes, egui::pos2(200.0, 200.0)).is_none());
    }

    #[test]
    fn ui_node_path_includes_each_visible_parent() {
        let mut document = engine_authoring::UiDocument::default();
        document.root.children.push(UiNode {
            id: "panel".to_owned(),
            kind: UiNodeKind::Panel {
                anchor: UiAnchor::TopLeft,
                offset_x: 0.0,
                offset_y: 0.0,
                layout: UiLayout::Vertical,
                spacing: 0.0,
                padding: 0.0,
                background: None,
            },
            children: vec![UiNode {
                id: "label".to_owned(),
                kind: UiNodeKind::Text {
                    content: UiString::Literal("Label".to_owned()),
                    size: 16.0,
                    color: [1.0; 4],
                },
                children: Vec::new(),
            }],
        });

        assert_eq!(
            ui_node_path(&document.root, "label"),
            Some(vec![
                "root".to_owned(),
                "panel".to_owned(),
                "label".to_owned()
            ])
        );
    }
}
