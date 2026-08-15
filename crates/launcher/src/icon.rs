//! Window icon rasterized from the same mark the header band paints.
//!
//! Generating the icon avoids shipping a binary asset next to the executable
//! and keeps the taskbar mark in sync with [`crate::theme::engine_mark_faces`]
//! whenever the mark changes.

use crate::theme;
use eframe::egui;

/// Edge length in pixels of the generated icon.
const ICON_SIZE: u32 = 64;
/// Sub-samples taken per axis inside each pixel.
///
/// The mark is built from diagonals, which alias badly at icon sizes, so
/// coverage is averaged instead of tested once at the pixel center.
const SAMPLES_PER_AXIS: u32 = 4;
/// Fraction of the icon's half-width occupied by the mark, leaving the padding
/// that desktop environments expect around a taskbar glyph.
const MARK_SCALE: f32 = 0.82;

/// Builds the Launcher window icon.
///
/// Pixels outside the mark stay fully transparent so the icon reads as a shape
/// rather than a tile on any desktop background.
pub(crate) fn launcher_icon() -> egui::IconData {
    let faces = theme::engine_mark_faces();
    let center = ICON_SIZE as f32 / 2.0;
    let radius = center * MARK_SCALE;
    let samples_per_pixel = (SAMPLES_PER_AXIS * SAMPLES_PER_AXIS) as f32;
    let sample_step = 1.0 / SAMPLES_PER_AXIS as f32;

    let mut rgba = vec![0_u8; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let mut covered = 0.0_f32;
            let mut red = 0.0_f32;
            let mut green = 0.0_f32;
            let mut blue = 0.0_f32;
            for sample_y in 0..SAMPLES_PER_AXIS {
                for sample_x in 0..SAMPLES_PER_AXIS {
                    let point = egui::vec2(
                        x as f32 + (sample_x as f32 + 0.5) * sample_step - center,
                        y as f32 + (sample_y as f32 + 0.5) * sample_step - center,
                    ) / radius;
                    // Faces tile the hexagon, so the first hit is the only hit
                    // and coverage can never exceed one sample.
                    let Some(color) = faces
                        .iter()
                        .find(|(face, _)| quad_contains(face, point))
                        .map(|(_, color)| *color)
                    else {
                        continue;
                    };
                    covered += 1.0;
                    red += f32::from(color.r());
                    green += f32::from(color.g());
                    blue += f32::from(color.b());
                }
            }

            if covered == 0.0 {
                continue;
            }
            let offset = ((y * ICON_SIZE + x) * 4) as usize;
            rgba[offset] = (red / covered).round() as u8;
            rgba[offset + 1] = (green / covered).round() as u8;
            rgba[offset + 2] = (blue / covered).round() as u8;
            rgba[offset + 3] = (255.0 * covered / samples_per_pixel).round() as u8;
        }
    }

    egui::IconData {
        rgba,
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

/// Returns whether `point` lies inside the convex quad `quad`.
fn quad_contains(quad: &[egui::Vec2; 4], point: egui::Vec2) -> bool {
    triangle_contains(quad[0], quad[1], quad[2], point)
        || triangle_contains(quad[0], quad[2], quad[3], point)
}

/// Returns whether `point` lies inside or on the edge of triangle `a b c`.
fn triangle_contains(
    a: egui::Vec2,
    b: egui::Vec2,
    c: egui::Vec2,
    point: egui::Vec2,
) -> bool {
    let first = edge_side(a, b, point);
    let second = edge_side(b, c, point);
    let third = edge_side(c, a, point);
    let any_negative = first < 0.0 || second < 0.0 || third < 0.0;
    let any_positive = first > 0.0 || second > 0.0 || third > 0.0;
    !(any_negative && any_positive)
}

/// Returns the signed area of the triangle `from`, `to`, `point`.
///
/// The sign tells which side of the directed edge the point falls on.
fn edge_side(from: egui::Vec2, to: egui::Vec2, point: egui::Vec2) -> f32 {
    let edge = to - from;
    let offset = point - from;
    edge.x * offset.y - edge.y * offset.x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon must be a complete RGBA buffer of the declared size, or the
    /// window backend rejects it at startup.
    #[test]
    fn launcher_icon_has_a_complete_rgba_buffer() {
        let icon = launcher_icon();

        assert_eq!(icon.width, ICON_SIZE);
        assert_eq!(icon.height, ICON_SIZE);
        assert_eq!(icon.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    /// The mark must cover the middle of the icon and leave the corners clear,
    /// which is what makes it read as a shape instead of a filled square.
    #[test]
    fn launcher_icon_is_opaque_at_the_center_and_clear_at_the_corners() {
        let icon = launcher_icon();
        let center = (((ICON_SIZE / 2) * ICON_SIZE + ICON_SIZE / 2) * 4) as usize;
        let bottom_right = ((ICON_SIZE * ICON_SIZE - 1) * 4) as usize;

        assert_eq!(icon.rgba[center + 3], 255);
        assert_eq!(icon.rgba[3], 0);
        assert_eq!(icon.rgba[bottom_right + 3], 0);
    }
}
