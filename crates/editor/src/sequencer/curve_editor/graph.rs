use super::sample_keys;
use eframe::egui;
use engine_authoring::{TimelineClip, TimelineKey, TimelineTick};

const HEIGHT: f32 = 140.0;
const SAMPLES: usize = 96;

pub(super) fn show(
    ui: &mut egui::Ui,
    clip: &TimelineClip,
    keys: &[TimelineKey],
    preview_tick: TimelineTick,
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width().max(240.0),
            ui.available_height().clamp(64.0, HEIGHT),
        ),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(27));

    let (minimum, maximum) = value_range(keys);
    for step in 1..4 {
        let ratio = step as f32 / 4.0;
        let x = rect.left() + rect.width() * ratio;
        let y = rect.top() + rect.height() * ratio;
        let grid = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(45));
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            grid,
        );
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            grid,
        );
    }

    let duration = clip.duration().get().max(1);
    let mut points = Vec::with_capacity(SAMPLES + 1);
    for sample in 0..=SAMPLES {
        let ratio = sample as f32 / SAMPLES as f32;
        let tick = TimelineTick((duration as f32 * ratio).round() as i64);
        points.push(egui::pos2(
            rect.left() + rect.width() * ratio,
            value_y(rect, minimum, maximum, sample_keys(keys, tick)),
        ));
    }
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 165, 220)),
    ));

    for key in keys {
        let ratio = key.tick.get() as f32 / duration as f32;
        painter.circle_filled(
            egui::pos2(
                rect.left() + rect.width() * ratio.clamp(0.0, 1.0),
                value_y(rect, minimum, maximum, key.value),
            ),
            4.5,
            egui::Color32::from_rgb(125, 175, 230),
        );
    }

    if preview_tick >= clip.start && preview_tick <= clip.end {
        let local = (preview_tick.get() - clip.start.get()).clamp(0, duration);
        let x = rect.left() + rect.width() * local as f32 / duration as f32;
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(230, 190, 90)),
        );
    }

    ui.small(format!(
        "Value range {:.3} … {:.3} · keys are clip-local",
        minimum, maximum
    ));
}

fn value_range(keys: &[TimelineKey]) -> (f32, f32) {
    let Some(first) = keys.first() else {
        return (-1.0, 1.0);
    };
    let mut minimum = first.value;
    let mut maximum = first.value;
    for key in keys.iter().skip(1) {
        minimum = minimum.min(key.value);
        maximum = maximum.max(key.value);
    }
    let span = maximum - minimum;
    let padding = if span.abs() < f32::EPSILON {
        (maximum.abs() * 0.1).max(1.0)
    } else {
        span.abs() * 0.1
    };
    (minimum - padding, maximum + padding)
}

fn value_y(rect: egui::Rect, minimum: f32, maximum: f32, value: f32) -> f32 {
    let ratio = ((value - minimum) / (maximum - minimum).max(f32::EPSILON)).clamp(0.0, 1.0);
    rect.bottom() - rect.height() * ratio
}
