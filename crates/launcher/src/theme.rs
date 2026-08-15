//! Launcher visual system: palette, egui style, and the painted engine mark.
//!
//! The Editor owns an equivalent style in `crates/editor/src/ui/chrome.rs`, but
//! the Launcher is a separate process and must not depend on the Editor crate,
//! which would pull the renderer into a window that never draws a scene. The
//! background and selection values are therefore restated here and kept
//! deliberately identical so both windows read as one product.

use eframe::egui;

/// Window background behind every panel.
pub(crate) const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(24, 27, 32);
/// Fill of raised cards such as the action panel and the recent-project rows.
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(33, 37, 44);
/// Fill of an interactive surface under the pointer.
pub(crate) const SURFACE_HOVERED: egui::Color32 = egui::Color32::from_rgb(44, 50, 60);
/// Fill of an interactive surface while it is being pressed.
pub(crate) const SURFACE_ACTIVE: egui::Color32 = egui::Color32::from_rgb(52, 59, 71);
/// Recessed fill used by text fields and other input areas.
pub(crate) const INPUT_BACKGROUND: egui::Color32 = egui::Color32::from_rgb(21, 24, 29);
/// Hairline border separating a card from the window background.
pub(crate) const BORDER: egui::Color32 = egui::Color32::from_rgb(52, 58, 69);
/// Primary action color, shared with the Editor's selection highlight.
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(46, 103, 168);
/// Primary action color under the pointer.
pub(crate) const ACCENT_HOVERED: egui::Color32 = egui::Color32::from_rgb(58, 124, 197);
/// Accent tint bright enough for small glyphs and thin marks.
pub(crate) const ACCENT_TEXT: egui::Color32 = egui::Color32::from_rgb(126, 184, 255);
/// Default foreground color.
pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(226, 230, 237);
/// Foreground color for secondary lines such as paths and hints.
pub(crate) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(141, 150, 165);
/// Foreground color for outcomes that completed as requested.
pub(crate) const SUCCESS: egui::Color32 = egui::Color32::from_rgb(108, 198, 148);
/// Foreground color for states that need attention but are not failures.
pub(crate) const WARNING: egui::Color32 = egui::Color32::from_rgb(232, 190, 92);
/// Foreground color for failures.
pub(crate) const DANGER: egui::Color32 = egui::Color32::from_rgb(226, 104, 104);

/// Top color of the header band gradient.
const HERO_TOP: egui::Color32 = egui::Color32::from_rgb(36, 48, 68);
/// Bottom color of the header band gradient, matching [`BACKGROUND`].
const HERO_BOTTOM: egui::Color32 = BACKGROUND;

/// Corner radius shared by cards, buttons, and input fields.
pub(crate) const CORNER_RADIUS: u8 = 7;

/// Applies the Launcher's dark visual system to `context`.
///
/// The theme is pinned to dark rather than following the system preference,
/// because the palette is shared with the Editor window, which is dark-only.
pub(crate) fn apply_launcher_style(context: &egui::Context) {
    context.set_theme(egui::ThemePreference::Dark);

    let mut visuals = egui::Visuals::dark();
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
    visuals.window_corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
    visuals.menu_corner_radius = egui::CornerRadius::same(CORNER_RADIUS);

    let corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
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

    context.set_visuals_of(egui::Theme::Dark, visuals);
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.indent = 16.0;
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::proportional(21.0)),
            (egui::TextStyle::Body, egui::FontId::proportional(14.0)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(13.0)),
            (egui::TextStyle::Button, egui::FontId::proportional(14.0)),
            (egui::TextStyle::Small, egui::FontId::proportional(11.5)),
        ]
        .into();
    });
}

/// Returns the frame shared by every raised card.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS + 2))
        .inner_margin(egui::Margin::same(14))
}

/// Draws the small caption that titles a section.
///
/// The caption is uppercased so section titles stay distinguishable from
/// project names without competing with them for weight.
pub(crate) fn section_caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(TEXT_MUTED),
    )
}

/// Draws the full-width hairline that closes a section caption.
pub(crate) fn section_rule(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    let rule = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rule.x_range(), rule.top(), egui::Stroke::new(1.0_f32, BORDER));
    ui.add_space(8.0);
}

/// Fills `rect` with the header band gradient and its bottom accent rule.
pub(crate) fn paint_hero_background(painter: &egui::Painter, rect: egui::Rect) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), HERO_TOP);
    mesh.colored_vertex(rect.right_top(), HERO_TOP);
    mesh.colored_vertex(rect.left_bottom(), HERO_BOTTOM);
    mesh.colored_vertex(rect.right_bottom(), HERO_BOTTOM);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));

    // A short accent run under the mark ties the band to the primary action
    // color without drawing a full-width line across the window.
    let baseline = rect.bottom() - 1.0;
    painter.hline(
        rect.x_range(),
        baseline,
        egui::Stroke::new(1.0_f32, BORDER),
    );
    painter.hline(
        rect.left()..=rect.left() + 148.0,
        baseline,
        egui::Stroke::new(2.0_f32, ACCENT),
    );
}

/// Returns the three faces of the isometric cube mark in unit space.
///
/// Offsets are centered on the origin with a circumradius of 1 so the on-screen
/// painter and the window-icon rasterizer scale one shared shape. Faces are
/// ordered top, right, then left, and they tile the hexagon without overlapping.
pub(crate) fn engine_mark_faces() -> [([egui::Vec2; 4], egui::Color32); 3] {
    // Half-width of a regular hexagon with circumradius 1.
    const HALF_WIDTH: f32 = 0.866_025_4;
    let top = egui::vec2(0.0, -1.0);
    let upper_right = egui::vec2(HALF_WIDTH, -0.5);
    let lower_right = egui::vec2(HALF_WIDTH, 0.5);
    let bottom = egui::vec2(0.0, 1.0);
    let lower_left = egui::vec2(-HALF_WIDTH, 0.5);
    let upper_left = egui::vec2(-HALF_WIDTH, -0.5);
    let center = egui::Vec2::ZERO;
    [
        (
            [upper_left, top, upper_right, center],
            egui::Color32::from_rgb(126, 184, 255),
        ),
        (
            [center, upper_right, lower_right, bottom],
            egui::Color32::from_rgb(70, 124, 196),
        ),
        (
            [upper_left, center, bottom, lower_left],
            egui::Color32::from_rgb(42, 82, 138),
        ),
    ]
}

/// Draws the engine mark centered on `center` with the given circumradius.
pub(crate) fn paint_engine_mark(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    for (face, color) in engine_mark_faces() {
        let points = face
            .iter()
            .map(|offset| center + *offset * radius)
            .collect::<Vec<_>>();
        // The seam stroke matches the band behind the mark, so the three faces
        // stay separated at any size without a visible outline.
        painter.add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::new(1.0_f32, HERO_TOP),
        ));
    }
}
