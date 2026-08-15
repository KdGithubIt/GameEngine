//! Material asset data model (Phase 35 / ADR 0029).
//!
//! Material assets are stored under `assets/` with the suffix
//! `.material.json`.  They describe the visual properties of a surface:
//! base color, roughness, metallic factor, and optional texture references.
//!
//! Texture references use [`AssetId`] values recorded in the asset manifest
//! (ADR 0021 / ADR 0029).

use crate::id::AssetId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Schema version for `.material.json` files.
pub const MATERIAL_SCHEMA_VERSION: u32 = 3;

// ---------------------------------------------------------------------------
// RGBA color helper
// ---------------------------------------------------------------------------

/// A linear RGBA color stored as four `f32` values in [0, 1].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinearRgba {
    /// Red channel.
    pub r: f32,
    /// Green channel.
    pub g: f32,
    /// Blue channel.
    pub b: f32,
    /// Alpha channel.
    pub a: f32,
}

/// How the renderer interprets the material alpha channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialAlphaMode {
    /// Alpha does not affect coverage.
    #[default]
    Opaque,
    /// Fragments below `alpha_cutoff` are discarded.
    Mask,
    /// Fragments are alpha blended with the existing color target.
    Blend,
}

/// Which triangle faces are discarded before shading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialCullMode {
    /// Cull back-facing triangles.
    #[default]
    Back,
    /// Cull front-facing triangles.
    Front,
    /// Render both sides.
    None,
}

/// Shading models supported by the current material contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialShadingModel {
    /// Apply directional, ambient, shadow, and environment lighting.
    #[default]
    StandardLit,
    /// Apply stepped diffuse lighting and stylized specular/rim terms.
    ToonLit,
    /// Draw base/emissive color without scene lighting.
    Unlit,
}

/// How a sphere-map sample combines with the shaded surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialSphereBlendMode {
    /// Multiply the shaded color by the sphere-map sample.
    #[default]
    Multiply,
    /// Add the sphere-map sample to the shaded color.
    Add,
}

/// Which coordinates address a sphere-map texture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialSphereCoordinateSource {
    /// Derive coordinates from the transformed surface normal.
    #[default]
    ViewNormal,
    /// Use the first generic additional UV channel.
    AdditionalUv0,
}

/// Toon-only lighting and texture inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToonLitProperties {
    /// Optional one-dimensional or two-dimensional toon ramp texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_texture: Option<AssetId>,
    /// Color multiplied into the dark side of the diffuse ramp.
    pub shadow_color: LinearRgba,
    /// Material-local ambient contribution.
    pub ambient_color: LinearRgba,
    /// Color of the compact toon specular highlight.
    pub specular_color: LinearRgba,
    /// Exponent controlling toon specular highlight size.
    pub specular_power: f32,
    /// Optional sphere-map texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sphere_texture: Option<AssetId>,
    /// Sphere-map compositing operation.
    pub sphere_blend: MaterialSphereBlendMode,
    /// Sphere-map coordinate source.
    pub sphere_coordinates: MaterialSphereCoordinateSource,
    /// Rim-light color.
    pub rim_color: LinearRgba,
    /// Rim-light exponent.
    pub rim_power: f32,
    /// Rim-light intensity; zero disables the term.
    pub rim_intensity: f32,
}

impl ToonLitProperties {
    fn default_shadow_color() -> LinearRgba {
        LinearRgba {
            r: 0.55,
            g: 0.55,
            b: 0.62,
            a: 1.0,
        }
    }

    fn default_ambient_color() -> LinearRgba {
        LinearRgba {
            r: 0.2,
            g: 0.2,
            b: 0.2,
            a: 1.0,
        }
    }

    fn default_specular_power() -> f32 {
        16.0
    }

    fn default_rim_power() -> f32 {
        3.0
    }
}

impl Default for ToonLitProperties {
    fn default() -> Self {
        Self {
            ramp_texture: None,
            shadow_color: Self::default_shadow_color(),
            ambient_color: Self::default_ambient_color(),
            specular_color: LinearRgba::WHITE,
            specular_power: Self::default_specular_power(),
            sphere_texture: None,
            sphere_blend: MaterialSphereBlendMode::Multiply,
            sphere_coordinates: MaterialSphereCoordinateSource::ViewNormal,
            rim_color: LinearRgba::WHITE,
            rim_power: Self::default_rim_power(),
            rim_intensity: 0.0,
        }
    }
}

/// Optional screen-space silhouette outline authored independently of shading model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialOutline {
    /// Whether this material participates in the outline pass.
    pub enabled: bool,
    /// Linear outline color.
    pub color: LinearRgba,
    /// Object-space reference width projected to the outline mask after per-vertex scaling.
    pub width: f32,
    /// Strength of outlines against other materials in the same model hierarchy.
    ///
    /// `0.0` preserves silhouette-only suppression, while `1.0` uses the full
    /// authored width and opacity. Intermediate values are useful for skin
    /// against clothing or hair without restoring every internal mesh seam.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub internal_boundary_strength: f32,
}

impl MaterialOutline {
    fn default_color() -> LinearRgba {
        LinearRgba {
            r: 0.02,
            g: 0.02,
            b: 0.04,
            a: 1.0,
        }
    }

    fn default_width() -> f32 {
        0.01
    }
}

impl Default for MaterialOutline {
    fn default() -> Self {
        Self {
            enabled: false,
            color: Self::default_color(),
            width: Self::default_width(),
            internal_boundary_strength: 0.0,
        }
    }
}

impl LinearRgba {
    /// Opaque white — the default base color.
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
}

impl Default for LinearRgba {
    fn default() -> Self {
        Self::WHITE
    }
}

// ---------------------------------------------------------------------------
// Material asset
// ---------------------------------------------------------------------------

/// A material asset stored as a `.material.json` file (Phase 35).
///
/// Materials describe the visual surface properties of a mesh.  Texture
/// references are resolved via the asset manifest (ADR 0021 / ADR 0029).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialAsset {
    /// Schema version for exact current-format validation.
    pub schema_version: u32,
    /// Base color (albedo) multiplier.  Applied before the base color texture.
    pub base_color: LinearRgba,
    /// Optional [`AssetId`] of a base-color texture in the asset manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color_texture: Option<AssetId>,
    /// Optional tangent-space normal texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<AssetId>,
    /// Optional packed metallic/roughness texture. Green stores roughness and blue stores metallic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic_roughness_texture: Option<AssetId>,
    /// Optional ambient-occlusion texture. Red stores the occlusion value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion_texture: Option<AssetId>,
    /// Optional emissive texture multiplied by `emissive_color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<AssetId>,
    /// Linear HDR emissive multiplier. RGB may exceed 1; alpha is ignored.
    pub emissive_color: LinearRgba,
    /// Scale applied to tangent-space normal-map X/Y before normalization.
    pub normal_scale: f32,
    /// Strength of the ambient-occlusion texture in [0, 1].
    pub occlusion_strength: f32,
    /// Roughness in [0, 1].  0 = mirror, 1 = fully diffuse.
    pub roughness: f32,
    /// Metallic factor in [0, 1].
    pub metallic: f32,
    /// Alpha coverage policy.
    pub alpha_mode: MaterialAlphaMode,
    /// Mask threshold used only by `alpha_mode = mask`.
    pub alpha_cutoff: f32,
    /// Triangle culling policy.
    pub cull_mode: MaterialCullMode,
    /// Lighting model supported by the current renderer.
    pub shading_model: MaterialShadingModel,
    /// Toon-only inputs, ignored by StandardLit and Unlit.
    pub toon: ToonLitProperties,
    /// Independent outline-pass settings.
    pub outline: MaterialOutline,
    /// Whether this surface contributes to shadow-depth passes.
    pub cast_shadow: bool,
    /// Whether this surface samples scene shadows.
    pub receive_shadow: bool,
}

impl MaterialAsset {
    fn default_roughness() -> f32 {
        0.5
    }

    fn default_emissive_color() -> LinearRgba {
        LinearRgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    }

    fn default_alpha_cutoff() -> f32 {
        0.5
    }

    /// Validates all scalar and color constraints shared by editor and runtime.
    pub fn validate(&self) -> Result<(), MaterialAssetError> {
        validate_unit_color("base_color", self.base_color)?;
        validate_non_negative_color("emissive_color", self.emissive_color)?;
        validate_finite_scalar("normal_scale", self.normal_scale)?;
        validate_unit_scalar("occlusion_strength", self.occlusion_strength)?;
        validate_unit_scalar("roughness", self.roughness)?;
        validate_unit_scalar("metallic", self.metallic)?;
        validate_unit_scalar("alpha_cutoff", self.alpha_cutoff)?;
        validate_unit_color("toon.shadow_color", self.toon.shadow_color)?;
        validate_non_negative_color("toon.ambient_color", self.toon.ambient_color)?;
        validate_non_negative_color("toon.specular_color", self.toon.specular_color)?;
        validate_non_negative_color("toon.rim_color", self.toon.rim_color)?;
        validate_non_negative_scalar("toon.specular_power", self.toon.specular_power)?;
        validate_non_negative_scalar("toon.rim_power", self.toon.rim_power)?;
        validate_non_negative_scalar("toon.rim_intensity", self.toon.rim_intensity)?;
        validate_non_negative_color("outline.color", self.outline.color)?;
        validate_non_negative_scalar("outline.width", self.outline.width)?;
        validate_unit_scalar(
            "outline.internal_boundary_strength",
            self.outline.internal_boundary_strength,
        )?;
        Ok(())
    }
}

impl Default for MaterialAsset {
    fn default() -> Self {
        Self {
            schema_version: MATERIAL_SCHEMA_VERSION,
            base_color: LinearRgba::WHITE,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            emissive_color: Self::default_emissive_color(),
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            roughness: Self::default_roughness(),
            metallic: 0.0,
            alpha_mode: MaterialAlphaMode::Opaque,
            alpha_cutoff: Self::default_alpha_cutoff(),
            cull_mode: MaterialCullMode::Back,
            shading_model: MaterialShadingModel::StandardLit,
            toon: ToonLitProperties::default(),
            outline: MaterialOutline::default(),
            cast_shadow: true,
            receive_shadow: true,
        }
    }
}

fn validate_non_negative_scalar(
    field: &'static str,
    value: f32,
) -> Result<(), MaterialAssetError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(MaterialAssetError::InvalidValue {
            field,
            expected: "a finite non-negative value",
        })
    }
}

fn validate_finite_scalar(
    field: &'static str,
    value: f32,
) -> Result<(), MaterialAssetError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(MaterialAssetError::InvalidValue {
            field,
            expected: "a finite value",
        })
    }
}

/// Returns whether an optional normalized scalar is zero and can be omitted.
fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

fn validate_unit_scalar(field: &'static str, value: f32) -> Result<(), MaterialAssetError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(MaterialAssetError::InvalidValue {
            field,
            expected: "a finite value between 0.0 and 1.0",
        })
    }
}

fn validate_unit_color(field: &'static str, color: LinearRgba) -> Result<(), MaterialAssetError> {
    for value in [color.r, color.g, color.b, color.a] {
        validate_unit_scalar(field, value)?;
    }
    Ok(())
}

fn validate_non_negative_color(
    field: &'static str,
    color: LinearRgba,
) -> Result<(), MaterialAssetError> {
    if [color.r, color.g, color.b, color.a]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
    {
        Ok(())
    } else {
        Err(MaterialAssetError::InvalidValue {
            field,
            expected: "finite non-negative linear color channels",
        })
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Describes why a [`MaterialAsset`] operation failed.
#[derive(Debug)]
pub enum MaterialAssetError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The file does not use the current material schema version.
    UnsupportedVersion {
        /// The version number found in the file.
        found: u32,
    },
    /// One persisted scalar or color violated the material contract.
    InvalidValue {
        /// Stable field name.
        field: &'static str,
        /// Human-readable accepted value.
        expected: &'static str,
    },
}

impl fmt::Display for MaterialAssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "material JSON error: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "material schema_version {found} is not supported (expected: {MATERIAL_SCHEMA_VERSION})"
            ),
            Self::InvalidValue { field, expected } => {
                write!(f, "material field `{field}` must be {expected}")
            }
        }
    }
}

impl std::error::Error for MaterialAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::UnsupportedVersion { .. } | Self::InvalidValue { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Parse / serialize
// ---------------------------------------------------------------------------

impl MaterialAsset {
    /// Parses a `.material.json` string.
    ///
    /// # Errors
    ///
    /// - [`MaterialAssetError::Json`] for malformed JSON or a missing required
    ///   current-format field.
    /// - [`MaterialAssetError::UnsupportedVersion`] when `schema_version`
    ///   differs from [`MATERIAL_SCHEMA_VERSION`].
    pub fn from_json(json: &str) -> Result<Self, MaterialAssetError> {
        let asset: MaterialAsset = serde_json::from_str(json).map_err(MaterialAssetError::Json)?;
        if asset.schema_version != MATERIAL_SCHEMA_VERSION {
            return Err(MaterialAssetError::UnsupportedVersion {
                found: asset.schema_version,
            });
        }
        asset.validate()?;
        Ok(asset)
    }

    /// Serializes this material to canonical pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_material_has_white_base_color_and_half_roughness() {
        let mat = MaterialAsset::default();
        assert_eq!(mat.base_color, LinearRgba::WHITE);
        assert!((mat.roughness - 0.5).abs() < f32::EPSILON);
        assert!((mat.metallic).abs() < f32::EPSILON);
        assert!(mat.base_color_texture.is_none());
        assert!((mat.normal_scale - 1.0).abs() < f32::EPSILON);
        assert!((mat.occlusion_strength - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn material_json_roundtrip_preserves_all_fields() {
        let id = AssetId::generate();
        let mut mat = MaterialAsset {
            schema_version: MATERIAL_SCHEMA_VERSION,
            base_color: LinearRgba {
                r: 0.8,
                g: 0.2,
                b: 0.1,
                a: 1.0,
            },
            base_color_texture: Some(id.clone()),
            normal_texture: None,
            metallic_roughness_texture: Some(id.clone()),
            occlusion_texture: Some(id.clone()),
            emissive_texture: None,
            emissive_color: LinearRgba {
                r: 2.0,
                g: 0.5,
                b: 0.1,
                a: 1.0,
            },
            normal_scale: 0.75,
            occlusion_strength: 0.6,
            roughness: 0.3,
            metallic: 0.7,
            alpha_mode: MaterialAlphaMode::Mask,
            alpha_cutoff: 0.4,
            cull_mode: MaterialCullMode::None,
            shading_model: MaterialShadingModel::Unlit,
            ..MaterialAsset::default()
        };
        mat.outline.internal_boundary_strength = 0.55;

        let json = mat.to_json().expect("must serialize");
        let parsed = MaterialAsset::from_json(&json).expect("must parse");
        assert_eq!(parsed.schema_version, MATERIAL_SCHEMA_VERSION);
        assert_eq!(parsed.base_color, mat.base_color);
        assert_eq!(parsed.base_color_texture, Some(id));
        assert!(parsed.metallic_roughness_texture.is_some());
        assert!(parsed.occlusion_texture.is_some());
        assert!((parsed.normal_scale - 0.75).abs() < f32::EPSILON);
        assert!((parsed.occlusion_strength - 0.6).abs() < f32::EPSILON);
        assert!((parsed.roughness - 0.3).abs() < f32::EPSILON);
        assert!((parsed.metallic - 0.7).abs() < f32::EPSILON);
        assert_eq!(parsed.alpha_mode, MaterialAlphaMode::Mask);
        assert_eq!(parsed.cull_mode, MaterialCullMode::None);
        assert_eq!(parsed.shading_model, MaterialShadingModel::Unlit);
        assert!((parsed.outline.internal_boundary_strength - 0.55).abs() < f32::EPSILON);
    }

    #[test]
    fn from_json_rejects_previous_material_version() {
        let mut value = serde_json::to_value(MaterialAsset::default()).expect("serialize fixture");
        value["schema_version"] = serde_json::Value::from(2);
        let json = serde_json::to_string(&value).expect("encode fixture");
        assert!(matches!(
            MaterialAsset::from_json(&json),
            Err(MaterialAssetError::UnsupportedVersion { found: 2 })
        ));
    }

    #[test]
    fn default_material_omits_only_current_optional_fields() {
        let mat = MaterialAsset::default();
        let json = mat.to_json().expect("must serialize");
        assert!(
            !json.contains("base_color_texture"),
            "absent texture must not appear in JSON: {json}"
        );
        assert!(
            !json.contains("internal_boundary_strength"),
            "zero boundary strength is intentionally omitted by the current writer: {json}"
        );
        assert!(json.contains("\"shading_model\": \"standard_lit\""));
        assert!(json.contains("\"cast_shadow\": true"));
    }

    #[test]
    fn missing_current_material_fields_are_rejected() {
        assert!(matches!(
            MaterialAsset::from_json(r#"{"schema_version":3}"#),
            Err(MaterialAssetError::Json(_))
        ));
    }

    #[test]
    fn removed_lit_shading_alias_is_rejected() {
        let mut value = serde_json::to_value(MaterialAsset::default()).expect("serialize fixture");
        value["shading_model"] = serde_json::Value::String("lit".to_owned());
        let json = serde_json::to_string(&value).expect("encode fixture");
        assert!(matches!(
            MaterialAsset::from_json(&json),
            Err(MaterialAssetError::Json(_))
        ));
    }

    #[test]
    fn invalid_material_numbers_are_rejected_before_runtime() {
        let material = MaterialAsset {
            roughness: 1.5,
            ..MaterialAsset::default()
        };
        let json = material.to_json().expect("fixture must serialize");
        let error = MaterialAsset::from_json(&json)
            .expect_err("out-of-range roughness must fail");

        assert!(matches!(
            error,
            MaterialAssetError::InvalidValue {
                field: "roughness",
                ..
            }
        ));
    }
}
