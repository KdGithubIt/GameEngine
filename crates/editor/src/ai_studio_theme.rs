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
/// Foreground color for states that need attention but are not failures.
pub(crate) const WARNING: egui::Color32 = egui::Color32::from_rgb(232, 190, 92);
/// Foreground color for failures.
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(226, 104, 104);

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

/// Draws a filled dot used as a compact status marker.
pub(crate) fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

