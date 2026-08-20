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
/// Foreground color for a state that is ready and needs nothing from the user.
pub(crate) const SUCCESS: egui::Color32 = egui::Color32::from_rgb(126, 196, 140);

/// Corner radius shared by cards, buttons, and input fields.
const CORNER_RADIUS: u8 = 7;

/// Primes `ui` with the AI Studio palette, spacing, and type scale.
///
/// Only this `Ui` and its children are affected, so the surrounding Editor
/// chrome keeps the style installed by `crate::ui::chrome`.
pub(crate) fn apply_studio_style(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    // egui does not wrap text inside a plain horizontal row, and the studio
    // reports host status, provider errors, and filesystem paths whose length
    // is not known when the row is written. Without this, one long line widens
    // the whole presentation instead of taking a second line.
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
    // A selectable label claims the text cursor for every glyph it covers, and
    // the studio is mostly labels: four of every five widgets here are text, so
    // the pointer would flip between the arrow and the I-beam across every row
    // of controls. Studio chrome is therefore not selectable, and the text
    // worth copying opts back in through [`selectable_text`].
    style.interaction.selectable_labels = false;
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

/// Draws studio text the reader may need to copy out.
///
/// This is the exception to the studio's non-selectable chrome, for text the
/// host produced rather than text the studio authored: a message, an error, an
/// endpoint, an identifier, a path. Losing the ability to copy those costs more
/// than the pointer flicker they reintroduce, and they are a small enough part
/// of the surface that the flicker stays local to them.
pub(crate) fn selectable_text(
    ui: &mut egui::Ui,
    text: impl Into<egui::RichText>,
) -> egui::Response {
    ui.add(egui::Label::new(text.into()).selectable(true))
}

/// Returns the frame shared by every raised card.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(CORNER_RADIUS + 2))
        .inner_margin(egui::Margin::same(12))
}

/// Makes a card span the panel instead of shrinking to its contents.
///
/// An `egui::Frame` sizes to what it holds, so a card of short fields would
/// otherwise be narrower than a card of prose, and the header rule drawn across
/// the available width would overhang the border.
fn full_width<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.set_width(ui.available_width());
    add_contents(ui)
}

/// Draws a card that spans the surface it is drawn on.
///
/// Preferred over `egui::Ui::group` inside the studio: a group draws the
/// Editor's own frame at the width of its contents, so a column of them reads
/// as a ragged stack of boxes rather than as one surface.
pub(crate) fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame()
        .show(ui, |ui| full_width(ui, add_contents))
        .inner
}

/// Draws a card whose border is tinted because it is waiting on the user.
pub(crate) fn attention_card<R>(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card_frame()
        .stroke(egui::Stroke::new(1.0_f32, accent))
        .show(ui, |ui| full_width(ui, add_contents))
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
    ui.painter().hline(
        rule.x_range(),
        rule.top(),
        egui::Stroke::new(1.0_f32, BORDER),
    );
    ui.add_space(7.0);
}

/// Draws a secondary explanatory line.
pub(crate) fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(egui::RichText::new(text.into()).small().color(TEXT_MUTED));
}

/// Draws a short label with its normative sentence available on demand.
///
/// ADR 0158 keeps control labels readable as labels: the specification text
/// that explains a control stays reachable from it rather than being drawn
/// beside it on every frame. Consequences of irreversible or outward-facing
/// actions are stated at the action itself and do not use this.
pub(crate) fn spec_note(ui: &mut egui::Ui, label: &str, specification: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(label).small().color(TEXT_MUTED));
        ui.menu_button("?", |ui| {
            ui.set_max_width(420.0);
            ui.label(specification);
        });
    });
}

/// Draws a filled dot used as a compact status marker.
pub(crate) fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// What a reported state means for the reader, independent of its wording.
///
/// The studio reports many states — a provider, a run, a backend, a resource —
/// and each used to be spelled out as a sentence in the same weight and color
/// as everything around it. A tone lets one glance answer whether a state is
/// finished, working, waiting on the user, or broken, and keeps that answer
/// consistent between surfaces that otherwise share no code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusTone {
    /// Nothing has happened yet, or the state does not apply here.
    Idle,
    /// Work is under way and needs nothing from the user.
    Busy,
    /// The state is settled and usable.
    Ready,
    /// Usable only after the user does something.
    Attention,
    /// Failed, or unusable until something is repaired.
    Blocked,
}

impl StatusTone {
    /// Returns the foreground color that carries this tone.
    pub(crate) const fn color(self) -> egui::Color32 {
        match self {
            Self::Idle => TEXT_MUTED,
            Self::Busy => ACCENT_TEXT,
            Self::Ready => SUCCESS,
            Self::Attention => WARNING,
            Self::Blocked => DANGER,
        }
    }

    /// Returns the fill behind a pill of this tone.
    ///
    /// Tinted rather than saturated: a status pill must be findable at a glance
    /// without becoming the brightest thing on a surface that may show several
    /// of them at once.
    fn tint(self) -> egui::Color32 {
        let color = self.color();
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30)
    }

    /// Returns the hairline around a pill of this tone.
    fn edge(self) -> egui::Color32 {
        let color = self.color();
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 90)
    }
}

/// Draws a compact tinted badge naming one state.
///
/// This is the studio's answer to "what is happening right now": the state is
/// named in two or three words and colored by [`StatusTone`], so the reader
/// never has to parse a sentence to find out whether something is ready.
pub(crate) fn status_pill(ui: &mut egui::Ui, tone: StatusTone, text: impl Into<String>) {
    let text = text.into();
    egui::Frame::NONE
        .fill(tone.tint())
        .stroke(egui::Stroke::new(1.0_f32, tone.edge()))
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                status_dot(ui, tone.color());
                ui.label(
                    egui::RichText::new(text)
                        .small()
                        .strong()
                        .color(tone.color()),
                );
            });
        });
}

/// Width reserved for the label column of [`field_row`].
const FIELD_LABEL_WIDTH: f32 = 124.0;

/// Draws one labeled fact as a column-aligned row.
///
/// Reported facts used to be concatenated into one sentence per line, which
/// left nothing to scan down: the reader had to read every line to find the one
/// they wanted. Aligning the labels turns a card into a table.
pub(crate) fn field_row(ui: &mut egui::Ui, label: &str, value: impl Into<egui::RichText>) {
    labeled_row(ui, label, |ui| {
        ui.label(value.into());
    });
}

/// Draws a labeled fact whose value is a state, as a column-aligned pill row.
pub(crate) fn field_row_pill(ui: &mut egui::Ui, label: &str, tone: StatusTone, value: &str) {
    labeled_row(ui, label, |ui| status_pill(ui, tone, value));
}

/// Draws one row of the label column with `value` filling the value column.
///
/// The value is drawn inside its own column rather than beside the label, so a
/// value that wraps continues under itself instead of returning to the left
/// edge of the card and breaking the column it belongs to.
fn labeled_row(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(FIELD_LABEL_WIDTH, ui.spacing().interact_size.y),
            egui::Sense::hover(),
        );
        ui.painter().text(
            egui::pos2(rect.left(), rect.top() + 4.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::TextStyle::Small.resolve(ui.style()),
            TEXT_MUTED,
        );
        ui.vertical(|ui| value(ui));
    });
}

/// Draws a row of capability chips, lit for what is supported.
///
/// Capabilities used to be printed as `name true`, which asks the reader to
/// translate a debug value back into a yes or no. A lit chip is the yes.
pub(crate) fn capability_chips(ui: &mut egui::Ui, capabilities: &[(&str, bool)]) {
    ui.horizontal_wrapped(|ui| {
        for (name, supported) in capabilities {
            let tone = if *supported {
                StatusTone::Ready
            } else {
                StatusTone::Idle
            };
            status_pill(ui, tone, *name);
        }
    });
}
