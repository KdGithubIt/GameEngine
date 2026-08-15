//! Target screen shapes shared by the Game View image and the Scene View
//! game frame.
//!
//! Both views answer the same question — "what will the shipped screen look
//! like?" — so they must offer the same presets. Keeping one enum here means a
//! preset added for one view cannot silently disagree with the other.

use eframe::egui;

/// Screen shape a view is constrained to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewAspect {
    /// Fill the whole panel; no target screen shape is assumed.
    #[default]
    Free,
    /// Widescreen 16:9.
    Wide16x9,
    /// Standard 4:3.
    Standard4x3,
    /// Fixed 1920x1080 render target, displayed at 16:9.
    Fixed1080,
}

impl ViewAspect {
    /// Every preset, in the order the toolbar lists them.
    pub const ALL: [Self; 4] = [
        Self::Free,
        Self::Wide16x9,
        Self::Standard4x3,
        Self::Fixed1080,
    ];

    /// Short toolbar label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Wide16x9 => "16:9",
            Self::Standard4x3 => "4:3",
            Self::Fixed1080 => "1920x1080",
        }
    }

    /// Width-to-height ratio of the target screen.
    ///
    /// Returns `None` for [`ViewAspect::Free`], which adopts whatever shape the
    /// panel currently has.
    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Free => None,
            Self::Wide16x9 | Self::Fixed1080 => Some(16.0 / 9.0),
            Self::Standard4x3 => Some(4.0 / 3.0),
        }
    }

    /// Target screen resolution a UI document is laid out against.
    ///
    /// A UI document's authored offsets and sizes are pixels on the shipped
    /// screen, so previewing one requires a screen size, not just a shape
    /// (ADR 0090). The ratio presets therefore name a canonical resolution:
    /// the most common desktop mode for that shape. Returns `None` for
    /// [`ViewAspect::Free`], which has no target screen and previews at panel
    /// resolution.
    pub fn target_resolution(self) -> Option<egui::Vec2> {
        match self {
            Self::Free => None,
            Self::Wide16x9 | Self::Fixed1080 => Some(egui::vec2(1920.0, 1080.0)),
            Self::Standard4x3 => Some(egui::vec2(1440.0, 1080.0)),
        }
    }

    /// Presentation of the target screen inside `rect`, for UI document layout.
    ///
    /// [`ViewAspect::Free`] presents at panel resolution; every other preset
    /// presents its [`ViewAspect::target_resolution`] uniformly scaled into
    /// `rect`, so a HUD covers the same fraction of the frame as of the shipped
    /// screen no matter how the editor dock is sized.
    pub fn ui_viewport(self, rect: egui::Rect) -> engine::UiViewport {
        match self.target_resolution() {
            Some(resolution) => engine::UiViewport::scaled(rect, resolution),
            None => engine::UiViewport::direct(rect),
        }
    }

    /// Largest rectangle with this aspect ratio, centered inside `available`.
    ///
    /// [`ViewAspect::Free`] returns `available` unchanged, so callers can use
    /// the result unconditionally.
    pub fn fit_rect(self, available: egui::Rect) -> egui::Rect {
        let Some(ratio) = self.ratio() else {
            return available;
        };
        let width = available
            .width()
            .min(available.height() * ratio)
            .max(1.0)
            .min(available.width());
        let height = (width / ratio).max(1.0).min(available.height());
        egui::Rect::from_center_size(available.center(), egui::vec2(width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(width, height))
    }

    #[test]
    fn free_aspect_keeps_the_whole_panel() {
        let available = rect(640.0, 100.0);
        assert_eq!(ViewAspect::Free.fit_rect(available), available);
    }

    #[test]
    fn wide_frame_is_letterboxed_inside_a_tall_panel() {
        let fitted = ViewAspect::Wide16x9.fit_rect(rect(320.0, 400.0));
        assert!((fitted.width() - 320.0).abs() < 0.01);
        assert!((fitted.height() - 180.0).abs() < 0.01);
        assert!((fitted.center().y - 220.0).abs() < 0.01);
    }

    #[test]
    fn wide_frame_is_pillarboxed_inside_a_wide_panel() {
        let fitted = ViewAspect::Wide16x9.fit_rect(rect(800.0, 180.0));
        assert!((fitted.width() - 320.0).abs() < 0.01);
        assert!((fitted.height() - 180.0).abs() < 0.01);
        assert!((fitted.center().x - 410.0).abs() < 0.01);
    }

    #[test]
    fn every_constrained_preset_names_a_target_resolution_with_its_own_ratio() {
        for aspect in ViewAspect::ALL {
            match (aspect.ratio(), aspect.target_resolution()) {
                (None, None) => {}
                (Some(ratio), Some(resolution)) => {
                    assert!(
                        (resolution.x / resolution.y - ratio).abs() < 0.01,
                        "{aspect:?} resolution {resolution:?} must match ratio {ratio}"
                    );
                }
                (ratio, resolution) => {
                    panic!("{aspect:?} has ratio {ratio:?} but resolution {resolution:?}")
                }
            }
        }
    }

    #[test]
    fn a_constrained_frame_presents_its_target_screen_scaled_down() {
        let frame = ViewAspect::Wide16x9.fit_rect(rect(640.0, 360.0));
        let viewport = ViewAspect::Wide16x9.ui_viewport(frame);

        assert_eq!(viewport.screen_size(), egui::vec2(1920.0, 1080.0));
        assert!((viewport.display_scale() - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn a_free_frame_presents_at_panel_resolution() {
        let frame = rect(640.0, 360.0);
        let viewport = ViewAspect::Free.ui_viewport(frame);

        assert_eq!(viewport.screen_size(), frame.size());
        assert!((viewport.display_scale() - 1.0).abs() < 0.01);
    }

    #[test]
    fn fitted_frame_never_leaves_the_available_rect() {
        for aspect in ViewAspect::ALL {
            let available = rect(4.0, 300.0);
            let fitted = aspect.fit_rect(available);
            assert!(available.contains_rect(fitted), "{aspect:?}");
        }
    }
}
