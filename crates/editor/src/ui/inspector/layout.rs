//! Layout primitives shared by every Inspector control.
//!
//! The Inspector is one long narrow column, so these helpers exist to keep a
//! single component, list, or field row from consuming the space the controls
//! below it need.

use crate::ui::*;

/// Height at which an in-card list starts scrolling instead of growing.
///
/// Roughly eight rows: enough to read a list at a glance, short enough that
/// the components below the card stay reachable without scrolling past it.
const BOUNDED_LIST_HEIGHT: f32 = 150.0;

/// Draws a list whose length comes from project data inside a fixed-height
/// scroll area.
///
/// The surrounding Inspector is one long column, so an unbounded list pushes
/// every later component out of view; the list scrolls on its own instead.
pub(in crate::ui) fn bounded_list_scroll_area(
    ui: &mut egui::Ui,
    salt: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::ScrollArea::vertical()
        .id_salt(salt)
        .max_height(BOUNDED_LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, add_contents);
}

/// Fixed height of the Add Component choice list.
///
/// Taller than [`BOUNDED_LIST_HEIGHT`] because the grouped list spends two
/// rows on the owner and category headings before the first component, but
/// still short enough to read the panel below it without scrolling far.
pub(in crate::ui) const ADD_COMPONENT_LIST_HEIGHT: f32 = 260.0;

/// Draws one component choice as a full-width row and reports a click.
///
/// Left-aligned full-width rows keep long component names readable in a narrow
/// Inspector, where a centered button label would be truncated on both sides.
pub(in crate::ui) fn add_component_choice_button(ui: &mut egui::Ui, label: &str) -> bool {
    let width = ui.available_width();
    ui.add(
        egui::Button::new(label)
            .truncate()
            .min_size(egui::vec2(width, ui.spacing().interact_size.y)),
    )
    .clicked()
}

/// Draws one Inspector component section as a separated card.
///
/// A bare run of collapsing headers reads as a single continuous field list,
/// so the boundary between two components is only visible while every one of
/// them is collapsed. The card gives each component its own background, border,
/// and trailing gap, which keeps the boundary readable when they are expanded.
pub(in crate::ui) fn inspector_component_card<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let fill = ui.visuals().faint_bg_color;
    let stroke = egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color);
    let inner = egui::Frame::group(ui.style())
        .fill(fill)
        .stroke(stroke)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, add_contents)
        .inner;
    ui.add_space(2.0);
    inner
}

/// Single-line or multi-line text field that buffers while focused and
/// returns the new text once when focus leaves (Enter included).
///
/// Committing per keystroke floods the undo stack with one step per typed
/// character; Escape discards the buffered draft instead of committing it.
pub(in crate::ui) fn draft_text_value(
    ui: &mut egui::Ui,
    salt: &str,
    current: &str,
    multiline: bool,
) -> Option<String> {
    let id = ui.id().with((salt, "text_draft"));
    let mut draft = ui
        .data_mut(|data| data.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_owned());
    let response = if multiline {
        ui.text_edit_multiline(&mut draft)
    } else {
        ui.text_edit_singleline(&mut draft)
    };
    if response.lost_focus() {
        ui.data_mut(|data| data.remove::<String>(id));
        let discard = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if !discard && draft != current {
            return Some(draft);
        }
        return None;
    }
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, draft));
    }
    None
}

/// Width at which a generated Inspector row can keep both columns useful.
const INSPECTOR_INLINE_FIELD_MIN_WIDTH: f32 = 260.0;

/// Lays out a generated Inspector field without squeezing either column.
///
/// Wide docks retain the familiar label-and-editor row. Narrow docks place the
/// editor below its label, giving reference lists and other compound controls
/// the complete viewport width instead of compressing text into vertical runs.
pub(in crate::ui) fn inspector_field_row<R>(
    ui: &mut egui::Ui,
    display_name: &str,
    description: &str,
    add_editor: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if ui.available_width() >= INSPECTOR_INLINE_FIELD_MIN_WIDTH {
        ui.horizontal(|ui| {
            show_inspector_field_label(ui, display_name, description);
            add_editor(ui)
        })
        .inner
    } else {
        ui.vertical(|ui| {
            let label_width = ui.available_width();
            ui.add_sized(
                [label_width, 20.0],
                egui::Label::new(display_name).truncate(),
            )
            .on_hover_text(description);
            add_editor(ui)
        })
        .inner
    }
}

/// Allocates a stable label column for a wide generated Inspector row.
fn show_inspector_field_label(
    ui: &mut egui::Ui,
    display_name: &str,
    description: &str,
) -> egui::Response {
    let width = (ui.available_width() * 0.34).clamp(112.0, 132.0);
    ui.add_sized([width, 20.0], egui::Label::new(display_name).truncate())
        .on_hover_text(description)
}
