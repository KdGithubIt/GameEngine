//! Built-in Transform Inspector and the fixed row geometry it depends on.

use crate::ui::*;

/// Presents the complete built-in transform as familiar grouped vectors.
/// Missing v2 fields use their compatibility defaults until the user edits
/// them, so simply inspecting a schema-v1 scene does not rewrite the document.
pub(in crate::ui) fn show_transform_object_editor(
    ui: &mut egui::Ui,
    fields: &mut std::collections::BTreeMap<String, Value>,
) -> Option<ComponentEdit> {
    let groups = [
        (
            "Position",
            " m",
            [("x", "X", 0.0), ("y", "Y", 0.0), ("z", "Z", 0.0)],
        ),
        (
            "Rotation",
            "°",
            [
                ("rotation_x_degrees", "X", 0.0),
                ("rotation_y_degrees", "Y", 0.0),
                ("rotation_z_degrees", "Z", 0.0),
            ],
        ),
        (
            "Scale",
            "×",
            [
                ("scale_x", "X", 1.0),
                ("scale_y", "Y", 1.0),
                ("scale_z", "Z", 1.0),
            ],
        ),
    ];

    for (title, suffix, axes) in groups {
        let mut group_edit = None;
        // Reserve the complete row before placing any controls. A normal
        // horizontal layout grows downward as taller widgets are encountered,
        // which can give later DragValue controls a different vertical origin.
        let row_width = ui.available_width();
        let axis_spacing = ui.spacing().item_spacing.x;
        let row_height = transform_row_height(ui);
        let (row_rect, _) =
            ui.allocate_exact_size(egui::vec2(row_width, row_height), egui::Sense::hover());
        let cell_rects = transform_row_rects(row_rect, axis_spacing);

        // Every cell uses the same precomputed vertical bounds. `place` paints
        // inside those bounds without advancing or resizing the parent row.
        let _ = ui.place(
            cell_rects[0],
            egui::Label::new(egui::RichText::new(title).strong()).truncate(),
        );

        for ((key, axis, default), cell_rect) in
            axes.into_iter().zip(cell_rects.into_iter().skip(1))
        {
            // Compatibility defaults remain transient until the author edits
            // an axis, preserving older scene documents exactly as before.
            let mut numeric = fields
                .get(key)
                .and_then(numeric_value_as_f64)
                .unwrap_or(default);

            // Position the control inside its fixed cell so X, Y, and Z share
            // an identical top edge, center line, and bottom edge.
            let response = ui.place(
                cell_rect,
                egui::DragValue::new(&mut numeric)
                    .speed(if title == "Rotation" { 0.5 } else { 0.05 })
                    .prefix(format!("{axis} "))
                    .suffix(suffix),
            );

            // Keep the existing drag buffering and property-command behavior.
            // Only the visual allocation changes; authoring state flow does not.
            if group_edit.is_none() {
                group_edit = numeric_drag_response(response, &numeric, |value| Value::F64(*value))
                    .map(|edit| {
                        prepend_property_segment(
                            edit,
                            PropertyPathSegment::Field { name: key.into() },
                        )
                    });
            }
        }
        if group_edit.is_some() {
            return group_edit;
        }
    }
    None
}

/// Returns enough height for both the configured DragValue text and padding.
///
/// Computing this before allocating the row prevents an individual control
/// from enlarging the row after earlier controls have already been positioned.
fn transform_row_height(ui: &egui::Ui) -> f32 {
    const MINIMUM_ROW_HEIGHT: f32 = 22.0;

    // DragValue switches between a button and a text editor. Both modes must
    // fit the same row so focusing a value cannot move the surrounding axes.
    let text_height = ui.text_style_height(&ui.style().drag_value_text_style);
    let padded_text_height = text_height + ui.spacing().button_padding.y * 2.0;

    MINIMUM_ROW_HEIGHT
        .max(ui.spacing().interact_size.y)
        .max(padded_text_height)
}

/// Returns the label and per-axis widths for one compact Transform row.
pub(in crate::ui) fn transform_row_widths(row_width: f32, item_spacing: f32) -> (f32, f32) {
    let title_width = (row_width * 0.20).clamp(58.0, 76.0);
    // Keep vector controls compact when the Inspector is wide. The minimum
    // preserves usability in a narrow dock, while the maximum prevents X, Y,
    // and Z from stretching merely to consume otherwise unused space.
    let axis_width = ((row_width - title_width - item_spacing * 3.0) / 3.0).clamp(44.0, 92.0);
    (title_width, axis_width)
}

/// Divides one reserved Transform row into a title cell and three axis cells.
///
/// All returned rectangles deliberately preserve the row's vertical bounds.
/// This invariant prevents sequential egui layout growth from creating a
/// staircase across the X, Y, and Z controls.
pub(in crate::ui) fn transform_row_rects(
    row_rect: egui::Rect,
    item_spacing: f32,
) -> [egui::Rect; 4] {
    let (title_width, axis_width) = transform_row_widths(row_rect.width(), item_spacing);
    let widths = [title_width, axis_width, axis_width, axis_width];
    let mut next_left = row_rect.left();

    std::array::from_fn(|index| {
        // Every cell starts at the row's top and uses the full row height.
        // Only the horizontal origin and width differ between cells.
        let cell_rect = egui::Rect::from_min_size(
            egui::pos2(next_left, row_rect.top()),
            egui::vec2(widths[index], row_rect.height()),
        );

        // Advance to the next column while retaining the editor-wide spacing.
        next_left = cell_rect.right() + item_spacing;
        cell_rect
    })
}
