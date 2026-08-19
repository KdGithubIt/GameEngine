//! Native 2D project authoring contracts (ADR 0127).
//!
//! Persisted project defaults and stable sorting identity live here. Runtime
//! solver state and backend handles are deliberately excluded.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Stable persisted identifier for one logical 2D sorting layer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SortingLayerId(String);

impl SortingLayerId {
    /// Generates a new stable sorting-layer identifier.
    pub fn generate() -> Self {
        Self(format!("sorting_layer_{}", ulid::Ulid::new()))
    }

    /// Parses and validates a persisted sorting-layer identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, SortingLayerIdError> {
        let value = value.into();
        let Some(suffix) = value.strip_prefix("sorting_layer_") else {
            return Err(SortingLayerIdError::WrongPrefix(value));
        };
        if ulid::Ulid::from_string(suffix).is_err() {
            return Err(SortingLayerIdError::InvalidUlid(value));
        }
        Ok(Self(value))
    }

    /// Returns the persisted opaque identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sorting-layer identifier validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortingLayerIdError {
    /// The value does not use the `sorting_layer_` prefix.
    WrongPrefix(String),
    /// The suffix after `sorting_layer_` is not a valid ULID.
    InvalidUlid(String),
}

impl fmt::Display for SortingLayerIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix(value) => write!(
                formatter,
                "sorting layer ID `{value}` must start with `sorting_layer_`"
            ),
            Self::InvalidUlid(value) => write!(
                formatter,
                "sorting layer ID `{value}` has an invalid ULID suffix"
            ),
        }
    }
}

impl std::error::Error for SortingLayerIdError {}

/// Default texture filtering used by Native 2D assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpriteFiltering {
    /// Preserve hard texel boundaries for pixel-art assets.
    Nearest,
    /// Interpolate neighboring texels.
    Linear,
}

/// Project default for pixel-aware 2D preview and Camera2D behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelPreviewPolicy {
    /// Do not request pixel-grid alignment behavior.
    Off,
    /// Show pixel-alignment guidance without forcing a projection policy.
    Advisory,
    /// Request the deterministic pixel-perfect camera policy.
    PixelPerfect,
}

/// One stable logical 2D draw layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortingLayer {
    /// Stable identity persisted by authored content.
    pub id: SortingLayerId,
    /// Human-readable display name; renaming does not alter [`Self::id`].
    pub name: String,
}

/// Typed project-wide Native 2D defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project2dSettings {
    /// Default source pixels represented by one world unit.
    pub default_pixels_per_unit: f32,
    /// Default texture filtering policy.
    pub default_filtering: SpriteFiltering,
    /// Fixed-step gravity in the world XY gameplay plane.
    ///
    /// Authoring retains `f64` values so JSON clients do not silently narrow
    /// decimal values before the runtime boundary.
    pub gravity: [f64; 2],
    /// Default pixel-aware preview policy.
    pub pixel_preview: PixelPreviewPolicy,
    /// Ordered logical draw layers addressed by stable identifiers.
    pub sorting_layers: Vec<SortingLayer>,
}

impl Default for Project2dSettings {
    fn default() -> Self {
        Self {
            default_pixels_per_unit: 100.0,
            default_filtering: SpriteFiltering::Nearest,
            gravity: [0.0, -9.81],
            pixel_preview: PixelPreviewPolicy::Advisory,
            sorting_layers: vec![SortingLayer {
                id: SortingLayerId("sorting_layer_00000000000000000000000000".to_owned()),
                name: "Default".to_owned(),
            }],
        }
    }
}

impl Project2dSettings {
    /// Validates project-level 2D invariants shared by every authoring client.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !self.default_pixels_per_unit.is_finite() || self.default_pixels_per_unit <= 0.0 {
            errors.push(
                "native_2d.default_pixels_per_unit must be finite and positive".to_owned(),
            );
        }
        if self.gravity.iter().any(|value| {
            !value.is_finite() || value.abs() > f64::from(f32::MAX)
        }) {
            errors.push("native_2d.gravity must fit finite f32 runtime values".to_owned());
        }
        if self.sorting_layers.is_empty() {
            errors.push("native_2d.sorting_layers must contain at least one layer".to_owned());
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for layer in &self.sorting_layers {
            if layer.name.trim().is_empty() {
                errors.push(format!(
                    "sorting layer `{}` has an empty name",
                    layer.id.as_str()
                ));
            }
            if !ids.insert(layer.id.as_str()) {
                errors.push(format!(
                    "duplicate sorting layer ID `{}`",
                    layer.id.as_str()
                ));
            }
            if !names.insert(layer.name.as_str()) {
                errors.push(format!("duplicate sorting layer name `{}`", layer.name));
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_2d_settings_have_downward_gravity_and_stable_default_layer() {
        let settings = Project2dSettings::default();
        assert_eq!(settings.gravity, [0.0, -9.81]);
        assert_eq!(settings.sorting_layers.len(), 1);
        assert!(settings.sorting_layers[0]
            .id
            .as_str()
            .starts_with("sorting_layer_"));
        assert!(settings.validate().is_empty());
    }

    #[test]
    fn sorting_layer_id_rejects_wrong_domain() {
        assert!(matches!(
            SortingLayerId::parse("sprite_01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Err(SortingLayerIdError::WrongPrefix(_))
        ));
    }

    #[test]
    fn validation_rejects_non_finite_gravity() {
        let mut settings = Project2dSettings::default();
        settings.gravity[1] = f64::NAN;
        assert!(!settings.validate().is_empty());
    }
}
