//! Environment / lighting settings panel (Phase 35).
//!
//! Stores ambient and directional light settings that are reflected in the
//! Scene View and Game View renders.  The data model is intentionally free of
//! egui so that it can be tested independently.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// RGBA color with linear float components in [0, ∞) for HDR lights.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LightColor {
    /// Red channel (linear, may exceed 1.0 for HDR).
    pub r: f32,
    /// Green channel (linear, may exceed 1.0 for HDR).
    pub g: f32,
    /// Blue channel (linear, may exceed 1.0 for HDR).
    pub b: f32,
}

impl LightColor {
    /// Soft white (1.0, 1.0, 1.0).
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
}

impl Default for LightColor {
    fn default() -> Self {
        Self::WHITE
    }
}

/// Ambient light settings — a uniform color applied to all surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AmbientLight {
    /// Color of the ambient light.
    pub color: LightColor,
    /// Intensity multiplier for the ambient contribution.
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self {
        Self {
            color: LightColor::WHITE,
            intensity: 0.1,
        }
    }
}

/// Directional light settings — a parallel light source with a world-space
/// direction vector.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DirectionalLight {
    /// Normalized direction the light travels towards (world space).
    pub direction: [f32; 3],
    /// Color of the directional light.
    pub color: LightColor,
    /// Intensity multiplier for the directional contribution.
    pub intensity: f32,
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: [-0.577_350_3, -0.577_350_3, -0.577_350_3],
            color: LightColor::WHITE,
            intensity: 1.0,
        }
    }
}

/// Editor-side environment / lighting settings (Phase 35).
///
/// These settings control the ambient and directional lights used when
/// rendering the Scene View and Game View.  They are not persisted as a
/// project file in v1; persistence is left to a future phase.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSettings {
    /// Ambient light affecting all surfaces uniformly.
    pub ambient: AmbientLight,
    /// Primary directional light (sun / moon).
    pub directional: DirectionalLight,
}

// ---------------------------------------------------------------------------
// egui rendering
// ---------------------------------------------------------------------------

/// Renders the environment settings panel inside `ui`.
///
/// Returns `true` when the user made a change.
#[cfg(not(test))]
pub fn show_environment_panel(
    settings: &mut EnvironmentSettings,
    ui: &mut eframe::egui::Ui,
) -> bool {
    let mut changed = false;

    ui.heading("Lighting");

    ui.collapsing("Ambient", |ui| {
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut color = [
                settings.ambient.color.r,
                settings.ambient.color.g,
                settings.ambient.color.b,
            ];
            if ui.color_edit_button_rgb(&mut color).changed() {
                settings.ambient.color = LightColor {
                    r: color[0],
                    g: color[1],
                    b: color[2],
                };
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Intensity");
            if ui
                .add(eframe::egui::Slider::new(
                    &mut settings.ambient.intensity,
                    0.0..=2.0,
                ))
                .changed()
            {
                changed = true;
            }
        });
    });

    ui.collapsing("Directional", |ui| {
        ui.horizontal(|ui| {
            ui.label("Color");
            let mut color = [
                settings.directional.color.r,
                settings.directional.color.g,
                settings.directional.color.b,
            ];
            if ui.color_edit_button_rgb(&mut color).changed() {
                settings.directional.color = LightColor {
                    r: color[0],
                    g: color[1],
                    b: color[2],
                };
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Intensity");
            if ui
                .add(eframe::egui::Slider::new(
                    &mut settings.directional.intensity,
                    0.0..=5.0,
                ))
                .changed()
            {
                changed = true;
            }
        });
        for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(*label);
                if ui
                    .add(
                        eframe::egui::DragValue::new(&mut settings.directional.direction[i])
                            .range(-1.0..=1.0)
                            .speed(0.01),
                    )
                    .changed()
                {
                    changed = true;
                }
            });
        }
    });

    changed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_environment_has_low_ambient_and_strong_directional() {
        let env = EnvironmentSettings::default();
        assert!(
            env.ambient.intensity < 0.5,
            "default ambient intensity must be low"
        );
        assert!(
            env.directional.intensity >= 1.0,
            "default directional intensity must be at least 1.0"
        );
    }

    #[test]
    fn environment_settings_roundtrip() {
        let env = EnvironmentSettings {
            ambient: AmbientLight {
                color: LightColor {
                    r: 0.1,
                    g: 0.1,
                    b: 0.2,
                },
                intensity: 0.05,
            },
            directional: DirectionalLight {
                direction: [0.0, -1.0, 0.0],
                color: LightColor::WHITE,
                intensity: 2.0,
            },
        };

        let json = serde_json::to_string(&env).expect("must serialize");
        let parsed: EnvironmentSettings = serde_json::from_str(&json).expect("must parse");
        assert_eq!(parsed, env);
    }
}
