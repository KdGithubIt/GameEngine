//! Post-processing settings and CPU reference tone mapping.

/// HDR target format selected by ADR 0040.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderTargetFormat {
    /// 16-bit floating point RGBA target.
    Rgba16Float,
}

impl HdrRenderTargetFormat {
    /// Returns the wgpu texture format for this HDR target.
    pub fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        }
    }
}

/// Tone-mapping operator used by the post-process pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMapOperator {
    /// ACES fitted tone mapping.
    AcesFitted,
    /// Reinhard tone mapping.
    Reinhard,
}

/// Bloom controls exposed through the post-process settings resource.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BloomSettings {
    /// Whether the bloom pass is enabled.
    pub enabled: bool,
    /// Luminance threshold above which pixels contribute to bloom.
    pub threshold: f32,
    /// Bloom contribution intensity.
    pub intensity: f32,
    /// Approximate blur radius in pixels.
    pub radius: f32,
}

impl Default for BloomSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 1.0,
            intensity: 0.15,
            radius: 4.0,
        }
    }
}

/// Color-grading controls applied after tone mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorGradingSettings {
    /// Whether color grading is enabled.
    pub enabled: bool,
    /// Linear RGB tint multiplier.
    pub tint: [f32; 3],
    /// Saturation multiplier; one preserves the input.
    pub saturation: f32,
    /// Contrast multiplier around middle gray.
    pub contrast: f32,
    /// Output gamma; one preserves the input.
    pub gamma: f32,
}

impl Default for ColorGradingSettings {
    fn default() -> Self {
        Self { enabled: false, tint: [1.0; 3], saturation: 1.0, contrast: 1.0, gamma: 1.0 }
    }
}

/// Runtime post-processing settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostProcessSettings {
    /// Whether the post-processing pipeline is enabled.
    pub enabled: bool,
    /// Exposure multiplier applied before tone mapping.
    pub exposure: f32,
    /// Tone-mapping operator.
    pub tone_map: ToneMapOperator,
    /// HDR render target format.
    pub hdr_format: HdrRenderTargetFormat,
    /// Bloom controls.
    pub bloom: BloomSettings,
    /// Final color-grading controls.
    pub color_grading: ColorGradingSettings,
}

impl PostProcessSettings {
    /// Applies exposure and tone mapping to a linear HDR color.
    pub fn tone_map_color(&self, color: glam::Vec3) -> glam::Vec3 {
        let exposed = color * self.exposure.max(0.0);
        let mapped = match self.tone_map {
            ToneMapOperator::AcesFitted => aces_fitted(exposed),
            ToneMapOperator::Reinhard => reinhard(exposed),
        };
        if !self.color_grading.enabled {
            return mapped;
        }
        let tinted = mapped * glam::Vec3::from_array(self.color_grading.tint);
        let luminance = tinted.dot(glam::Vec3::new(0.2126, 0.7152, 0.0722));
        let saturated = glam::Vec3::splat(luminance)
            .lerp(tinted, self.color_grading.saturation.max(0.0));
        let contrasted = (saturated - glam::Vec3::splat(0.5))
            * self.color_grading.contrast.max(0.0)
            + glam::Vec3::splat(0.5);
        contrasted.max(glam::Vec3::ZERO).powf(1.0 / self.color_grading.gamma.max(0.001))
    }

    /// Returns a swapchain clear color after applying the configured preview tone mapper.
    pub fn tone_mapped_clear_color(&self, color: glam::Vec3) -> wgpu::Color {
        let mapped = if self.enabled {
            self.tone_map_color(color)
        } else {
            color
        }
        .clamp(glam::Vec3::ZERO, glam::Vec3::ONE);

        wgpu::Color {
            r: mapped.x as f64,
            g: mapped.y as f64,
            b: mapped.z as f64,
            a: 1.0,
        }
    }
}

impl Default for PostProcessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            exposure: 1.0,
            tone_map: ToneMapOperator::AcesFitted,
            hdr_format: HdrRenderTargetFormat::Rgba16Float,
            bloom: BloomSettings::default(),
            color_grading: ColorGradingSettings::default(),
        }
    }
}

fn reinhard(color: glam::Vec3) -> glam::Vec3 {
    color / (glam::Vec3::ONE + color)
}

fn aces_fitted(color: glam::Vec3) -> glam::Vec3 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    ((color * (a * color + glam::Vec3::splat(b)))
        / (color * (c * color + glam::Vec3::splat(d)) + glam::Vec3::splat(e)))
    .clamp(glam::Vec3::ZERO, glam::Vec3::ONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_adr_0040() {
        let settings = PostProcessSettings::default();

        assert!(settings.enabled);
        assert_eq!(settings.hdr_format, HdrRenderTargetFormat::Rgba16Float);
        assert_eq!(settings.tone_map, ToneMapOperator::AcesFitted);
        assert_eq!(settings.exposure, 1.0);
    }

    #[test]
    fn reinhard_compresses_hdr_values() {
        let settings = PostProcessSettings {
            tone_map: ToneMapOperator::Reinhard,
            ..Default::default()
        };

        let mapped = settings.tone_map_color(glam::Vec3::splat(3.0));

        assert!(mapped.x > 0.0);
        assert!(mapped.x < 1.0);
    }
}
