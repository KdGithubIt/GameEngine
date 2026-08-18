//! Visual system for the AI Studio presentations.
//!
//! AI Studio is drawn inside the Editor's egui context, so this module never
//! mutates global style: every helper works on a scoped [`egui::Ui`] whose
//! style was primed by [`apply_studio_style`]. The palette restates the
//! Launcher values from `crates/launcher/src/theme.rs` rather than importing
//! them, because the Launcher is a separate process and the Editor must not
//! depend on it. The two are kept deliberately identical so the Launcher, the
//! Editor, and a detached AI Studio window read as one product.

use eframe::egui;

/// Fill behind the AI Studio body.
pub(crate) const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(24, 27, 32);
/// Fill of raised cards such as a message bubble or a section card.
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(33, 37, 44);
/// Fill of an interactive surface under the pointer.
pub(crate) const SURFACE_HOVERED: egui::Color32 = egui::Color32::from_rgb(44, 50, 60);
/// Fill of an interactive surface while it is being pressed.
pub(crate) const SURFACE_ACTIVE: egui::Color32 = egui::Color32::from_rgb(52, 59, 71);
/// Recessed fill used by text fields and other input areas.
pub(crate) const INPUT_BACKGROUND: egui::Color32 = egui::Color32::from_rgb(21, 24, 29);
/// Hairline border separating a card from its background.
pub(crate) const BORDER: egui::Color32 = egui::Color32::from_rgb(52, 58, 69);
/// Primary action color, shared with the Editor's selection highlight.
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(46, 103, 168);
/// Accent tint bright enough for small glyphs and thin marks.
pub(crate) const ACCENT_TEXT: egui::Color32 = egui::Color32::from_rgb(126, 184, 255);
/// Default foreground color.
pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 230, 237);
/// Foreground color for secondary lines such as hints and provenance.
pub(crate) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(141, 150, 165);
/// Foreground color for outcomes that completed as requested.
pub(crate) const SUCCESS: egui::Color32 = egui::Color32::from_rgb(108, 198, 148);
/// Foreground color for states that need attention but are not failures.
pub(crate) const WARNING: egui::Color32 = egui::Color32::from_rgb(232, 190, 92);
/// Foreground color for failures.
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(226, 104, 104);

/// Top color of the header band gradient.
const HEADER_TOP: egui::Color32 = egui::Color32::from_rgb(36, 48, 68);

/// Corner radius shared by cards, buttons, and input fields.
const CORNER_RADIUS: u8 = 7;

/// Primes `ui` with the AI Studio palette, spacing, and type scale.
///
/// Only this `Ui` and its children are affected, so the surrounding Editor
/// chrome keeps the style installed by `crate::ui::chrome`.
pub(crate) fn apply_studio_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    let corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
    let visuals = &mut style.visuals;
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, BORDER);
    visuals.extreme_bg_color = INPUT_BACKGROUND;
    visuals.faint_bg_color = SURFACE_HOVERED;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT_TEXT);
    visuals.hyperlink_color = ACCENT_TEXT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = DANGER;
    visuals.window_corner_radius = corner_radius;
    visuals.menu_corner_radius = corner_radius;

    visuals.widgets.noninteractive.bg_fill = SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = SURFACE;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    visuals.widgets.noninteractive.corner_radius = corner_radius;

    visuals.widgets.inactive.bg_fill = SURFACE_HOVERED;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_HOVERED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    visuals.widgets.inactive.corner_radius = corner_radius;

    visuals.widgets.hovered.bg_fill = SURFACE_ACTIVE;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_ACTIVE;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    visuals.widgets.hovered.corner_radius = corner_radius;

    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT_TEXT);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    visuals.widgets.active.corner_radius = corner_radius;

    visuals.widgets.open = visuals.widgets.inactive;

    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(11.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.indent = 14.0;
    style.text_styles = [
        (egui::TextStyle::Heading, egui::FontId::proportional(17.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(13.5)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(12.5)),
        (egui::TextStyle::Button, egui::FontId::proportional(13.5)),
        (egui::TextStyle::Small, egui::FontId::proportional(11.5)),
    ]
    .into();
}

/// Returns the frame shared by every raised card.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS + 2))
        .inner_margin(egui::Margin::same(12))
}

/// Returns a frame that reads as recessed rather than raised.
///
/// Used for read-only evidence such as the event timeline so it does not
/// compete with the interactive cards around it.
pub(crate) fn well_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(INPUT_BACKGROUND)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS))
        .inner_margin(egui::Margin::same(10))
}

/// Draws a card with the shared surface treatment.
pub(crate) fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame().show(ui, add_contents).inner
}

/// Draws a card whose border is tinted because it is waiting on the user.
pub(crate) fn attention_card<R>(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card_frame()
        .stroke(egui::Stroke::new(1.0_f32, accent))
        .show(ui, add_contents)
        .inner
}

/// Draws the small uppercase caption that titles a section or a card.
///
/// The caption is uppercased so structural labels stay distinguishable from
/// authored text without competing with it for weight.
pub(crate) fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(TEXT_MUTED),
    );
}

/// Draws a card caption followed by the hairline that closes it.
pub(crate) fn card_header(ui: &mut egui::Ui, text: &str) {
    caption(ui, text);
    ui.add_space(1.0);
    let rule = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rule.x_range(), rule.top(), egui::Stroke::new(1.0_f32, BORDER));
    ui.add_space(7.0);
}

/// Draws a secondary explanatory line.
pub(crate) fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(egui::RichText::new(text.into()).small().color(TEXT_MUTED));
}

/// Draws a filled badge that names a state in one or two words.
pub(crate) fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.5), color);
    let padding = egui::vec2(7.0, 3.0);
    let (rect, _) = ui.allocate_exact_size(galley.size() + padding * 2.0, egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(CORNER_RADIUS - 2),
        color.gamma_multiply(0.2),
    );
    ui.painter().galley(rect.min + padding, galley, color);
}

/// Draws a filled dot used as a compact status marker.
pub(crate) fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Draws one navigation entry for the section list.
///
/// `trailing` names a state worth surfacing before the section is opened, such
/// as a pending review count.
pub(crate) fn nav_entry(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    trailing: Option<(String, egui::Color32)>,
) -> egui::Response {
    let fill = if selected {
        ACCENT.gamma_multiply(0.3)
    } else {
        egui::Color32::TRANSPARENT
    };
    let stroke_color = if selected {
        ACCENT
    } else {
        egui::Color32::TRANSPARENT
    };
    let response = egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, stroke_color))
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new(label)
                        .color(if selected { TEXT } else { TEXT_MUTED })
                        .strong(),
                );
                if let Some((text, color)) = trailing {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        badge(ui, &text, color);
                    });
                }
            });
        })
        .response;
    response.interact(egui::Sense::click())
}

/// Returns the header band gradient covering `rect`.
///
/// The band restates the Launcher's hero treatment so a detached AI Studio
/// window is recognizable as the same product as the project picker. It is
/// returned rather than painted because the caller reserves its shape index
/// before laying out the header, whose height is only known afterwards.
pub(crate) fn header_background_mesh(rect: egui::Rect) -> egui::Shape {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), HEADER_TOP);
    mesh.colored_vertex(rect.right_top(), HEADER_TOP);
    mesh.colored_vertex(rect.left_bottom(), BACKGROUND);
    mesh.colored_vertex(rect.right_bottom(), BACKGROUND);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    egui::Shape::mesh(mesh)
}

/// Draws the hairline and accent run that close the header band.
pub(crate) fn paint_header_rules(painter: &egui::Painter, rect: egui::Rect) {
    let baseline = rect.bottom() - 0.5;
    painter.hline(rect.x_range(), baseline, egui::Stroke::new(1.0_f32, BORDER));
    // A short accent run ties the band to the primary action color without
    // drawing a full-width line across the surface.
    painter.hline(
        rect.left()..=rect.left() + 116.0,
        baseline,
        egui::Stroke::new(2.0_f32, ACCENT),
    );
}
