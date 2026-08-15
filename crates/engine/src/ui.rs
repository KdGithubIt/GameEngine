//! Runtime HUD and in-game menu UI integration.
//!
//! Register UI drawing callbacks with [`crate::App::add_ui_system`].  The host
//! (editor or standalone game loop) calls [`crate::App::run_ui_systems`] each
//! frame after the wgpu scene render, passing the active egui context and the
//! game viewport rectangle.  Each registered system receives those values and
//! may draw [`egui::Area`] widgets that float above the game texture.

use egui::{Context, Rect, Vec2};
use engine_ecs::World;

/// Where the game screen is presented, and how large that screen is.
///
/// A UI document's authored offsets and sizes are logical units on the *target
/// screen*, not points in the host window. Those two sizes are equal when the
/// game fills its own window, but an editor surface presents the target screen
/// shrunk into a dock panel. Keeping both in one value means a host cannot
/// supply the on-screen rectangle alone and silently make a HUD cover a
/// quarter of a small preview but a twentieth of the shipped screen
/// (ADR 0090).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiViewport {
    /// Egui-space rectangle the screen is presented in.
    rect: Rect,
    /// Logical size of the target screen, in target pixels.
    screen_size: Vec2,
}

impl UiViewport {
    /// Presents the target screen at 1:1, so one logical unit is one point.
    ///
    /// This is the runtime case: the game draws into its own window, and the
    /// window is the screen.
    pub fn direct(rect: Rect) -> Self {
        Self::scaled(rect, rect.size())
    }

    /// Presents a `screen_size` target screen scaled uniformly into `rect`.
    ///
    /// Use this for any surface that shows the shipped screen at a different
    /// size than it will ship at: the Scene View game frame, a letterboxed Game
    /// View, or the zoomable UI Builder canvas.
    pub fn scaled(rect: Rect, screen_size: Vec2) -> Self {
        Self {
            rect,
            screen_size: Vec2::new(screen_size.x.max(1.0), screen_size.y.max(1.0)),
        }
    }

    /// Egui-space rectangle the screen is presented in.
    ///
    /// Add this rectangle's origin to any fixed-position HUD coordinates so
    /// that overlays stay within the visible game area regardless of editor
    /// panel layout.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Logical size of the target screen that layout resolves against.
    pub fn screen_size(&self) -> Vec2 {
        self.screen_size
    }

    /// Uniform factor from logical screen units to presented points.
    ///
    /// One for a [`UiViewport::direct`] presentation. The smaller axis ratio
    /// wins so a mismatched aspect ratio keeps the whole screen inside `rect`
    /// instead of cropping it.
    pub fn display_scale(&self) -> f32 {
        let horizontal = self.rect.width() / self.screen_size.x;
        let vertical = self.rect.height() / self.screen_size.y;
        horizontal.min(vertical).clamp(0.01, 100.0)
    }
}

/// Egui context and game viewport injected by the host each frame.
///
/// The host inserts this resource into the runtime world before calling each
/// [`UiSystem`], so regular ECS systems can also draw HUD elements by reading
/// `Res<UiContext>`.
pub struct UiContext {
    /// The shared egui rendering context.
    pub ctx: Context,
    /// Presented rectangle and target screen size of the game viewport.
    pub viewport: UiViewport,
}

impl UiContext {
    /// Wraps an existing egui context and a game viewport.
    pub fn new(ctx: Context, viewport: UiViewport) -> Self {
        Self { ctx, viewport }
    }

    /// Egui-space rectangle the game viewport occupies.
    pub fn viewport_rect(&self) -> Rect {
        self.viewport.rect()
    }
}

/// Draws HUD or overlay elements using an egui context each rendered frame.
///
/// Implement this trait to add score displays, health bars, pause menus, or
/// any other in-game overlay.  Register implementations with
/// [`crate::App::add_ui_system`].
///
/// # Example
///
/// ```rust,ignore
/// use engine::ui::UiSystem;
/// use engine::ecs::World;
///
/// struct TimerHud;
/// impl UiSystem for TimerHud {
///     fn run(&mut self, ctx: &egui::Context, world: &World) {
///         egui::Area::new(egui::Id::new("hud_timer"))
///             .fixed_pos(egui::pos2(10.0, 10.0))
///             .show(ctx, |ui| {
///                 ui.label("00:30");
///             });
///     }
/// }
/// ```
pub trait UiSystem: Send + Sync + 'static {
    /// Draws HUD elements for the current frame.
    ///
    /// The world reference is read-only.  To enqueue mutations use
    /// [`engine_ecs::Commands`] stored as a resource, or apply changes through
    /// a regular ECS system on the following frame.
    fn run(&mut self, ctx: &Context, world: &World);
}

impl<F> UiSystem for F
where
    F: FnMut(&Context, &World) + Send + Sync + 'static,
{
    fn run(&mut self, ctx: &Context, world: &World) {
        (self)(ctx, world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect::from_min_size(egui::pos2(x, y), Vec2::new(width, height))
    }

    #[test]
    fn direct_viewport_presents_one_logical_unit_per_point() {
        let viewport = UiViewport::direct(rect(10.0, 20.0, 1280.0, 720.0));

        assert_eq!(viewport.screen_size(), Vec2::new(1280.0, 720.0));
        assert!((viewport.display_scale() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn scaled_viewport_reports_the_shrink_factor() {
        let viewport = UiViewport::scaled(rect(0.0, 0.0, 480.0, 270.0), Vec2::new(1920.0, 1080.0));

        assert!((viewport.display_scale() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn mismatched_aspect_keeps_the_whole_screen_inside_the_rect() {
        let viewport = UiViewport::scaled(rect(0.0, 0.0, 1920.0, 270.0), Vec2::new(1920.0, 1080.0));

        assert!((viewport.display_scale() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn degenerate_sizes_do_not_produce_a_zero_or_infinite_scale() {
        let empty_screen = UiViewport::scaled(rect(0.0, 0.0, 800.0, 600.0), Vec2::ZERO);
        let empty_rect = UiViewport::direct(rect(0.0, 0.0, 0.0, 0.0));

        assert!(empty_screen.display_scale().is_finite());
        assert!(empty_rect.display_scale() > 0.0);
    }
}
