//! Shared sizing rules for editor-rendered offscreen viewports.

use eframe::egui;

/// Converts an egui logical size into a GPU render-target size in physical pixels.
///
/// `logical_size` is measured in egui points. `pixels_per_point` must come from
/// [`egui::Context::pixels_per_point`], so native display scaling and egui zoom
/// are applied exactly once. The result is uniformly reduced when necessary to
/// stay within the GPU's two-dimensional texture limit.
pub(crate) fn render_target_size_in_pixels(
    logical_size: egui::Vec2,
    pixels_per_point: f32,
    max_texture_dimension_2d: u32,
) -> [u32; 2] {
    let pixels_per_point = sanitize_pixels_per_point(pixels_per_point) as f64;
    fit_requested_pixels(
        sanitize_logical_dimension(logical_size.x) * pixels_per_point,
        sanitize_logical_dimension(logical_size.y) * pixels_per_point,
        max_texture_dimension_2d,
    )
}

/// Fits an explicit physical-pixel resolution inside the GPU texture limit.
///
/// This is used by fixed render presets such as 1920x1080. No editor DPI scale
/// is applied here; a fixed pixel resolution is already expressed in the GPU's
/// coordinate space.
pub(crate) fn clamp_render_target_size_in_pixels(
    requested: [u32; 2],
    max_texture_dimension_2d: u32,
) -> [u32; 2] {
    fit_requested_pixels(
        requested[0].max(1) as f64,
        requested[1].max(1) as f64,
        max_texture_dimension_2d,
    )
}

fn sanitize_pixels_per_point(pixels_per_point: f32) -> f32 {
    if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    }
}

fn sanitize_logical_dimension(points: f32) -> f64 {
    if points.is_finite() && points > 0.0 {
        points as f64
    } else {
        1.0
    }
}

fn fit_requested_pixels(
    requested_width: f64,
    requested_height: f64,
    max_texture_dimension_2d: u32,
) -> [u32; 2] {
    let maximum = max_texture_dimension_2d.max(1) as f64;
    let fit = (maximum / requested_width.max(requested_height)).min(1.0);
    [
        (requested_width * fit).round().clamp(1.0, maximum) as u32,
        (requested_height * fit).round().clamp(1.0, maximum) as u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_target_size_tracks_common_dpi_scales() {
        let logical = egui::vec2(800.0, 480.0);

        assert_eq!(render_target_size_in_pixels(logical, 1.0, 8192), [800, 480]);
        assert_eq!(render_target_size_in_pixels(logical, 1.25, 8192), [1000, 600]);
        assert_eq!(render_target_size_in_pixels(logical, 1.5, 8192), [1200, 720]);
        assert_eq!(render_target_size_in_pixels(logical, 2.0, 8192), [1600, 960]);
    }

    #[test]
    fn invalid_dpi_falls_back_to_one() {
        assert_eq!(
            render_target_size_in_pixels(egui::vec2(800.0, 480.0), f32::NAN, 8192),
            [800, 480],
        );
    }

    #[test]
    fn gpu_limit_preserves_requested_aspect_ratio() {
        assert_eq!(
            render_target_size_in_pixels(egui::vec2(8000.0, 6000.0), 2.0, 8192),
            [8192, 6144],
        );
        assert_eq!(
            clamp_render_target_size_in_pixels([4000, 3000], 2048),
            [2048, 1536],
        );
    }
}
