//! The compact object-reference row shared by asset and entity references,
//! and the two results a reference control can report back to the panel.

use crate::ui::*;

/// Describes editor-only navigation requested by an Inspector reference field.
///
/// Reference navigation never mutates authoring data. Asset references reveal
/// their source in the Asset Browser, while entity references synchronize the
/// Hierarchy selection and may additionally frame the target in Scene View.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum InspectorReferenceNavigation {
    RevealAsset(AssetId),
    SelectEntity(EntityId),
    FocusEntity(EntityId),
}

/// Distinguishes assigning a concrete reference from choosing the explicit
/// unassigned row in a searchable reference picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ReferencePickerAction<T> {
    Assign(T),
    Clear,
}

/// Width of the object-picker selector at the right edge of a reference row.
pub(in crate::ui) const REFERENCE_SELECTOR_WIDTH: f32 = 24.0;

/// Height of one compact reference row.
pub(in crate::ui) fn reference_row_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y.max(22.0)
}

/// Returns the width a reference field may occupy once `trailing` points are
/// reserved for widgets placed after it on the same row.
pub(in crate::ui) fn remaining_reference_row_width(
    available_width: f32,
    trailing: f32,
    item_spacing: f32,
) -> f32 {
    (available_width - trailing - item_spacing).max(REFERENCE_SELECTOR_WIDTH)
}

/// Splits one reserved reference row into its body and selector cells.
///
/// The Inspector panel sizes itself from its contents, so a row that allocates
/// more than the width it was offered widens the panel again on every frame.
/// Deriving both cells from a single reserved row keeps that impossible.
pub(in crate::ui) fn reference_row_rects(row_rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    const CELL_GAP: f32 = 2.0;

    let selector_left = (row_rect.right() - REFERENCE_SELECTOR_WIDTH).max(row_rect.left());
    let body = egui::Rect::from_min_max(
        row_rect.min,
        egui::pos2(
            (selector_left - CELL_GAP).max(row_rect.left()),
            row_rect.bottom(),
        ),
    );
    let selector = egui::Rect::from_min_max(
        egui::pos2(selector_left, row_rect.top()),
        egui::pos2(row_rect.right(), row_rect.bottom()),
    );
    (body, selector)
}

/// Draws a compact single-line object field body and its Unity-style selector
/// button. The body remains a drag-and-drop target while the selector owns the
/// searchable popup.
///
/// Both cells are painted into one reserved row instead of being added as
/// sequential widgets: a button whose text plus padding exceeds its cell would
/// otherwise report a wider desired size and grow the Inspector panel.
pub(in crate::ui) fn show_compact_reference_field(
    ui: &mut egui::Ui,
    icon: &str,
    icon_color: egui::Color32,
    label: &str,
    tooltip: &str,
) -> (egui::Response, egui::Response) {
    let height = reference_row_height(ui);
    let row_width = ui.available_width().max(REFERENCE_SELECTOR_WIDTH);
    let (row_rect, row_response) =
        ui.allocate_exact_size(egui::vec2(row_width, height), egui::Sense::hover());
    let (field_rect, selector_rect) = reference_row_rects(row_rect);

    let field_response = ui.interact(
        field_rect,
        row_response.id.with("field"),
        egui::Sense::click_and_drag(),
    );
    let visuals = ui.style().interact(&field_response);
    ui.painter().rect(
        field_rect,
        2.0,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        egui::pos2(field_rect.left() + 13.0, field_rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(16.0),
        icon_color,
    );
    ui.painter()
        .with_clip_rect(field_rect.shrink2(egui::vec2(28.0, 2.0)))
        .text(
            egui::pos2(field_rect.left() + 27.0, field_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            visuals.text_color(),
        );

    let selector_response = ui.interact(
        selector_rect,
        row_response.id.with("selector"),
        egui::Sense::click(),
    );
    let selector_visuals = ui.style().interact(&selector_response);
    ui.painter().rect(
        selector_rect,
        2.0,
        selector_visuals.bg_fill,
        selector_visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        selector_rect.center(),
        egui::Align2::CENTER_CENTER,
        "⊙",
        egui::FontId::proportional(14.0),
        selector_visuals.text_color(),
    );

    (
        field_response.on_hover_text(tooltip),
        selector_response.on_hover_text("Open object picker"),
    )
}
