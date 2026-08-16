use std::fmt;
use std::sync::Arc;

/// Reports texture data that cannot be decoded or uploaded safely.
#[derive(Debug)]
pub enum TextureUploadError {
    /// Image bytes could not be decoded.
    Decode(image::ImageError),
    /// Image dimensions are empty or exceed the device limit.
    InvalidDimensions {
        /// The requested image width.
        width: u32,
        /// The requested image height.
        height: u32,
        /// The maximum supported two-dimensional texture size.
        maximum: u32,
    },
    /// Decoded RGBA storage did not match the declared dimensions.
    InvalidDataLength {
        /// Number of bytes required for tightly packed RGBA8.
        expected: usize,
        /// Number of supplied bytes.
        actual: usize,
    },
}

impl fmt::Display for TextureUploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "failed to decode image data: {error}"),
            Self::InvalidDimensions {
                width,
                height,
                maximum,
            } => write!(
                formatter,
                "texture dimensions {width}x{height} must be non-zero and at most {maximum}x{maximum}"
            ),
            Self::InvalidDataLength { expected, actual } => write!(
                formatter,
                "texture RGBA8 data has {actual} bytes but dimensions require {expected}"
            ),
        }
    }
}

impl std::error::Error for TextureUploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            Self::InvalidDimensions { .. } | Self::InvalidDataLength { .. } => None,
        }
    }
}

/// Owns a sampled GPU texture, view, and sampler.
pub struct Texture {
    /// The underlying GPU texture.
    pub texture: wgpu::Texture,
    /// The default view used for sampling.
    pub view: wgpu::TextureView,
    /// The sampler used by the built-in material pipeline.
    pub sampler: wgpu::Sampler,
}

/// CPU-decoded RGBA texture waiting for the renderer's GPU preparation pass.
///
/// Scene conversion runs in headless tests and before a render device may be
/// available. Keeping decoded pixels here lets that boundary validate images
/// once while GPU upload remains owned by `WorldRenderer`.
#[derive(Debug)]
pub struct DecodedTexture {
    /// Debug label and source name used by GPU validation messages.
    pub label: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Tightly packed RGBA8 pixels.
    ///
    /// The upload slot decides whether RGB is interpreted as sRGB color or
    /// linear data.
    pub rgba8: Vec<u8>,
}

impl DecodedTexture {
    /// Decodes supported texture bytes without requiring a GPU device.
    pub fn from_bytes(bytes: &[u8], label: impl Into<String>) -> Result<Self, TextureUploadError> {
        let image = image::load_from_memory(bytes).map_err(TextureUploadError::Decode)?;
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self {
            label: label.into(),
            width,
            height,
            rgba8: rgba.into_raw(),
        })
    }
}

/// CPU-decoded material texture slots transferred from asset resolution into the renderer.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct PendingMaterialTextures {
    /// Base-color pixels awaiting sRGB GPU upload.
    pub base: Option<Arc<DecodedTexture>>,
    /// Tangent-space normal pixels awaiting linear GPU upload.
    pub normal: Option<Arc<DecodedTexture>>,
    /// Packed roughness/metallic pixels awaiting linear GPU upload.
    pub metallic_roughness: Option<Arc<DecodedTexture>>,
    /// Ambient-occlusion pixels awaiting linear GPU upload.
    pub occlusion: Option<Arc<DecodedTexture>>,
    /// Emissive-color pixels awaiting sRGB GPU upload.
    pub emissive: Option<Arc<DecodedTexture>>,
    /// Toon-ramp pixels awaiting sRGB GPU upload.
    pub ramp: Option<Arc<DecodedTexture>>,
    /// Sphere/matcap pixels awaiting sRGB GPU upload.
    pub sphere: Option<Arc<DecodedTexture>>,
}

impl Texture {
    /// Decodes supported texture bytes and uploads the resulting texture.
    ///
    /// # Errors
    ///
    /// Returns an error when the image bytes cannot be decoded or the decoded
    /// dimensions cannot be represented by the device.
    pub fn from_bytes(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
        label: &str,
    ) -> Result<Self, TextureUploadError> {
        let image = image::load_from_memory(bytes).map_err(TextureUploadError::Decode)?;
        Self::from_image(device, queue, &image, label)
    }

    /// Uploads an image as an sRGB texture.
    ///
    /// # Errors
    ///
    /// Returns an error when the image dimensions are empty or exceed the
    /// device's two-dimensional texture limit.
    pub fn from_image(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &image::DynamicImage,
        label: &str,
    ) -> Result<Self, TextureUploadError> {
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        Self::from_rgba8(
            device,
            queue,
            width,
            height,
            rgba.as_raw(),
            label,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        )
    }

    /// Uploads pixels that were decoded at the scene/asset boundary.
    pub fn from_decoded(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        decoded: &DecodedTexture,
    ) -> Result<Self, TextureUploadError> {
        Self::from_rgba8(
            device,
            queue,
            decoded.width,
            decoded.height,
            &decoded.rgba8,
            &decoded.label,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        )
    }

    /// Uploads decoded values without applying an sRGB transfer function.
    ///
    /// Normal maps encode vector components rather than colors, so sampling
    /// them through an sRGB view would bend even a flat `(0.5, 0.5, 1.0)`
    /// normal. This explicit path keeps slot color-space behavior truthful.
    pub fn from_decoded_linear(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        decoded: &DecodedTexture,
    ) -> Result<Self, TextureUploadError> {
        Self::from_rgba8(
            device,
            queue,
            decoded.width,
            decoded.height,
            &decoded.rgba8,
            &decoded.label,
            wgpu::TextureFormat::Rgba8Unorm,
        )
    }

    /// Owns the one GPU upload path for both immediate and deferred decoding.
    fn from_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgba8: &[u8],
        label: &str,
        format: wgpu::TextureFormat,
    ) -> Result<Self, TextureUploadError> {
        use wgpu::util::DeviceExt;

        validate_texture_dimensions(width, height, device.limits().max_texture_dimension_2d)?;
        validate_rgba8_length(width, height, rgba8.len())?;
        let mip_level_count = texture_mip_level_count(width, height);
        let mip_data = (mip_level_count > 1).then(|| {
            generate_rgba8_mip_chain(
                width,
                height,
                rgba8,
                format.is_srgb(),
            )
        });
        let upload_data = mip_data.as_deref().unwrap_or(rgba8);
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            upload_data,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        Ok(Self {
            texture,
            view,
            sampler,
        })
    }

    /// Creates a one-pixel white texture for untextured materials.
    pub fn white(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::solid_rgba(device, queue, [255, 255, 255, 255], "white_texture", true)
            .expect("one-pixel white texture must fit every valid GPU device")
    }

    /// Creates a one-pixel fallback texture in the requested color space.
    #[doc(hidden)]
    pub fn solid_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgba: [u8; 4],
        label: &str,
        srgb: bool,
    ) -> Result<Self, TextureUploadError> {
        Self::from_rgba8(
            device,
            queue,
            1,
            1,
            &rgba,
            label,
            if srgb {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            },
        )
    }
}

fn texture_mip_level_count(width: u32, height: u32) -> u32 {
    let maximum = width.max(height);
    if maximum == 0 {
        0
    } else {
        u32::BITS - maximum.leading_zeros()
    }
}

fn generate_rgba8_mip_chain(
    width: u32,
    height: u32,
    rgba8: &[u8],
    srgb: bool,
) -> Vec<u8> {
    let mut chain = rgba8.to_vec();
    let mut source_offset = 0;
    let mut source_width = width;
    let mut source_height = height;

    while source_width > 1 || source_height > 1 {
        let source_len = source_width as usize * source_height as usize * 4;
        let next = downsample_rgba8(
            &chain[source_offset..source_offset + source_len],
            source_width,
            source_height,
            srgb,
        );
        source_offset += source_len;
        source_width = (source_width / 2).max(1);
        source_height = (source_height / 2).max(1);
        chain.extend_from_slice(&next);
    }

    chain
}

fn downsample_rgba8(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    srgb: bool,
) -> Vec<u8> {
    let target_width = (source_width / 2).max(1);
    let target_height = (source_height / 2).max(1);
    let mut target = vec![0; target_width as usize * target_height as usize * 4];
    let target_width_u64 = u64::from(target_width);
    let target_height_u64 = u64::from(target_height);

    for target_y in 0..target_height {
        let y_start = u64::from(target_y) * u64::from(source_height);
        let y_end = u64::from(target_y + 1) * u64::from(source_height);
        let source_y_start = (y_start / target_height_u64) as u32;
        let source_y_end = y_end.div_ceil(target_height_u64) as u32;

        for target_x in 0..target_width {
            let x_start = u64::from(target_x) * u64::from(source_width);
            let x_end = u64::from(target_x + 1) * u64::from(source_width);
            let source_x_start = (x_start / target_width_u64) as u32;
            let source_x_end = x_end.div_ceil(target_width_u64) as u32;
            let mut accumulated = [0.0_f64; 4];
            let mut total_weight = 0.0_f64;

            for source_y in source_y_start..source_y_end {
                let pixel_y_start = u64::from(source_y) * target_height_u64;
                let pixel_y_end = u64::from(source_y + 1) * target_height_u64;
                let overlap_y = y_end.min(pixel_y_end) - y_start.max(pixel_y_start);

                for source_x in source_x_start..source_x_end {
                    let pixel_x_start = u64::from(source_x) * target_width_u64;
                    let pixel_x_end = u64::from(source_x + 1) * target_width_u64;
                    let overlap_x = x_end.min(pixel_x_end) - x_start.max(pixel_x_start);
                    let weight = (overlap_x * overlap_y) as f64;
                    let source_index =
                        (source_y as usize * source_width as usize + source_x as usize) * 4;

                    for (channel, accumulator) in accumulated.iter_mut().enumerate() {
                        let encoded = source[source_index + channel];
                        let value = if srgb && channel < 3 {
                            srgb_u8_to_linear(encoded)
                        } else {
                            f64::from(encoded) / 255.0
                        };
                        *accumulator += value * weight;
                    }
                    total_weight += weight;
                }
            }

            let target_index =
                (target_y as usize * target_width as usize + target_x as usize) * 4;
            for (channel, accumulated) in accumulated.into_iter().enumerate() {
                let value = accumulated / total_weight;
                target[target_index + channel] = if srgb && channel < 3 {
                    linear_to_srgb_u8(value)
                } else {
                    linear_to_unorm_u8(value)
                };
            }
        }
    }

    target
}

fn srgb_u8_to_linear(encoded: u8) -> f64 {
    let encoded = f64::from(encoded) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb_u8(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    linear_to_unorm_u8(encoded)
}

fn linear_to_unorm_u8(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn validate_texture_dimensions(
    width: u32,
    height: u32,
    maximum: u32,
) -> Result<(), TextureUploadError> {
    if width == 0 || height == 0 || width > maximum || height > maximum {
        return Err(TextureUploadError::InvalidDimensions {
            width,
            height,
            maximum,
        });
    }
    Ok(())
}

fn validate_rgba8_length(width: u32, height: u32, actual: usize) -> Result<(), TextureUploadError> {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if actual == expected {
        Ok(())
    } else {
        Err(TextureUploadError::InvalidDataLength { expected, actual })
    }
}

/// Coverage mode consumed by the mesh render pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AlphaMode {
    /// Ignore alpha for coverage and write an opaque surface.
    #[default]
    Opaque,
    /// Discard fragments whose alpha is below the material cutoff.
    Mask,
    /// Blend source alpha over the existing color target.
    Blend,
}

impl AlphaMode {
    /// Stable numeric value passed through per-instance GPU data.
    #[doc(hidden)]
    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Opaque => 0.0,
            Self::Mask => 1.0,
            Self::Blend => 2.0,
        }
    }
}

/// Triangle-face culling mode consumed when choosing a render pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CullMode {
    /// Cull back-facing triangles.
    #[default]
    Back,
    /// Cull front-facing triangles.
    Front,
    /// Render both sides.
    None,
}

/// Lighting model supported by the built-in mesh shader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShadingModel {
    /// Apply environment, diffuse, and physically based specular lighting.
    #[default]
    StandardLit,
    /// Apply stepped diffuse, compact specular, sphere-map, and rim lighting.
    ToonLit,
    /// Bypass scene lighting while retaining base and emissive color.
    Unlit,
}

/// Runtime sphere-map compositing operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SphereBlendMode {
    /// Multiply the shaded surface by the sphere map.
    #[default]
    Multiply,
    /// Add the sphere map to the shaded surface.
    Add,
}

/// Runtime sphere-map coordinate source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SphereCoordinateSource {
    /// Derive coordinates from the transformed normal.
    #[default]
    ViewNormal,
    /// Use the first additional UV channel carried by each vertex.
    AdditionalUv0,
}

/// Runtime toon-lighting inputs.
#[derive(Clone)]
pub struct ToonMaterial {
    /// Optional GPU toon-ramp texture.
    pub ramp_texture: Option<Arc<Texture>>,
    /// CPU-decoded ramp uploaded lazily by the renderer.
    pub pending_ramp_texture: Option<Arc<DecodedTexture>>,
    /// Dark-side diffuse color.
    pub shadow_color: [f32; 3],
    /// Material-local ambient color.
    pub ambient_color: [f32; 3],
    /// Compact highlight color.
    pub specular_color: [f32; 3],
    /// Compact highlight exponent.
    pub specular_power: f32,
    /// Optional GPU sphere-map texture.
    pub sphere_texture: Option<Arc<Texture>>,
    /// CPU-decoded sphere map uploaded lazily by the renderer.
    pub pending_sphere_texture: Option<Arc<DecodedTexture>>,
    /// Sphere-map blend operation.
    pub sphere_blend: SphereBlendMode,
    /// Sphere-map coordinate source.
    pub sphere_coordinates: SphereCoordinateSource,
    /// Rim-light color.
    pub rim_color: [f32; 3],
    /// Rim-light exponent.
    pub rim_power: f32,
    /// Rim-light intensity.
    pub rim_intensity: f32,
}

impl Default for ToonMaterial {
    fn default() -> Self {
        Self {
            ramp_texture: None,
            pending_ramp_texture: None,
            shadow_color: [0.55, 0.55, 0.62],
            ambient_color: [0.2; 3],
            specular_color: [1.0; 3],
            specular_power: 16.0,
            sphere_texture: None,
            pending_sphere_texture: None,
            sphere_blend: SphereBlendMode::Multiply,
            sphere_coordinates: SphereCoordinateSource::ViewNormal,
            rim_color: [1.0; 3],
            rim_power: 3.0,
            rim_intensity: 0.0,
        }
    }
}

/// Independent screen-space outline-pass settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineMaterial {
    /// Whether the material is submitted to the outline pass.
    pub enabled: bool,
    /// Linear outline color.
    pub color: [f32; 4],
    /// Object-space reference width projected after applying per-vertex scale.
    pub width: f32,
    /// Strength of boundaries against a different material under the same hierarchy root.
    pub internal_boundary_strength: f32,
}

impl Default for OutlineMaterial {
    fn default() -> Self {
        Self {
            enabled: false,
            color: [0.02, 0.02, 0.04, 1.0],
            width: 0.01,
            internal_boundary_strength: 0.0,
        }
    }
}

/// Describes the built-in mesh material.
#[derive(Clone)]
pub struct Material {
    /// The base RGBA color with channels in the `0.0..=1.0` range.
    pub color: [f32; 4],
    /// The sampled texture, or `None` to use the built-in white texture.
    pub texture: Option<Arc<Texture>>,
    /// CPU-decoded base-color texture uploaded lazily by the renderer.
    pub pending_texture: Option<Arc<DecodedTexture>>,
    /// GPU normal texture, sampled as linear tangent-space data.
    pub normal_texture: Option<Arc<Texture>>,
    /// CPU-decoded normal texture uploaded lazily as a linear texture.
    pub pending_normal_texture: Option<Arc<DecodedTexture>>,
    /// GPU packed metallic/roughness texture sampled as linear data.
    pub metallic_roughness_texture: Option<Arc<Texture>>,
    /// CPU-decoded packed metallic/roughness texture uploaded lazily as linear data.
    pub pending_metallic_roughness_texture: Option<Arc<DecodedTexture>>,
    /// GPU ambient-occlusion texture sampled as linear data.
    pub occlusion_texture: Option<Arc<Texture>>,
    /// CPU-decoded ambient-occlusion texture uploaded lazily as linear data.
    pub pending_occlusion_texture: Option<Arc<DecodedTexture>>,
    /// GPU emissive texture, sampled in sRGB color space.
    pub emissive_texture: Option<Arc<Texture>>,
    /// CPU-decoded emissive texture uploaded lazily by the renderer.
    pub pending_emissive_texture: Option<Arc<DecodedTexture>>,
    /// Linear HDR emissive RGB added after scene lighting.
    pub emissive_color: [f32; 3],
    /// Scale applied to tangent-space normal-map X/Y before normalization.
    pub normal_scale: f32,
    /// Strength of ambient occlusion in the StandardLit indirect term.
    pub occlusion_strength: f32,
    /// Micro-surface roughness used by the specular BRDF.
    pub roughness: f32,
    /// Metallic blend factor used by the diffuse/specular BRDF.
    pub metallic: f32,
    /// Alpha coverage behavior.
    pub alpha_mode: AlphaMode,
    /// Alpha threshold used when `alpha_mode` is [`AlphaMode::Mask`].
    pub alpha_cutoff: f32,
    /// Face-culling policy.
    pub cull_mode: CullMode,
    /// Whether scene lighting is evaluated.
    pub shading_model: ShadingModel,
    /// Toon-specific inputs, ignored by the other shading models.
    pub toon: ToonMaterial,
    /// Independent screen-space silhouette outline settings.
    pub outline: OutlineMaterial,
    /// Whether the material contributes to shadow-depth passes.
    pub cast_shadow: bool,
    /// Whether the material samples scene shadows.
    pub receive_shadow: bool,
}

/// One material per submesh of the entity's mesh, in submesh order
/// (ADR 0076).
///
/// A submesh with no slot falls back to the entity's [`Material`], and then
/// to [`Material::default`], so a mesh that gains parts still draws.
#[derive(Clone, Default)]
pub struct MaterialSlots {
    /// Materials indexed by submesh.
    pub materials: Vec<Material>,
}

impl MaterialSlots {
    /// Returns the material for `submesh`, falling back to `fallback`.
    pub fn resolve<'a>(&'a self, submesh: usize, fallback: &'a Material) -> &'a Material {
        self.materials.get(submesh).unwrap_or(fallback)
    }
}

impl Material {
    /// Creates an untextured opaque material with the given RGB color.
    pub fn color(red: f32, green: f32, blue: f32) -> Self {
        Self {
            color: [red, green, blue, 1.0],
            texture: None,
            pending_texture: None,
            ..Self::default_surface()
        }
    }

    /// Creates an opaque white material that samples `texture`.
    pub fn textured(texture: Arc<Texture>) -> Self {
        Self {
            color: [1.0, 1.0, 1.0, 1.0],
            texture: Some(texture),
            pending_texture: None,
            ..Self::default_surface()
        }
    }

    /// Creates a material whose texture is decoded but not yet GPU-backed.
    pub fn pending_textured(color: [f32; 4], texture: Arc<DecodedTexture>) -> Self {
        Self {
            color,
            texture: None,
            pending_texture: Some(texture),
            ..Self::default_surface()
        }
    }

    /// Converts persisted material semantics without performing asset I/O.
    ///
    /// Scene conversion owns manifest lookup and image decoding; keeping enum
    /// and scalar mapping here prevents every importer/preview path from
    /// reimplementing the runtime meaning of the same authoring document.
    pub fn from_authoring_asset(asset: &engine_authoring::MaterialAsset) -> Self {
        Self {
            color: [
                asset.base_color.r,
                asset.base_color.g,
                asset.base_color.b,
                asset.base_color.a,
            ],
            emissive_color: [
                asset.emissive_color.r,
                asset.emissive_color.g,
                asset.emissive_color.b,
            ],
            normal_scale: asset.normal_scale,
            occlusion_strength: asset.occlusion_strength,
            roughness: asset.roughness,
            metallic: asset.metallic,
            alpha_mode: match asset.alpha_mode {
                engine_authoring::MaterialAlphaMode::Opaque => AlphaMode::Opaque,
                engine_authoring::MaterialAlphaMode::Mask => AlphaMode::Mask,
                engine_authoring::MaterialAlphaMode::Blend => AlphaMode::Blend,
            },
            alpha_cutoff: asset.alpha_cutoff,
            cull_mode: match asset.cull_mode {
                engine_authoring::MaterialCullMode::Back => CullMode::Back,
                engine_authoring::MaterialCullMode::Front => CullMode::Front,
                engine_authoring::MaterialCullMode::None => CullMode::None,
            },
            shading_model: match asset.shading_model {
                engine_authoring::MaterialShadingModel::StandardLit => ShadingModel::StandardLit,
                engine_authoring::MaterialShadingModel::ToonLit => ShadingModel::ToonLit,
                engine_authoring::MaterialShadingModel::Unlit => ShadingModel::Unlit,
            },
            toon: ToonMaterial {
                shadow_color: [asset.toon.shadow_color.r, asset.toon.shadow_color.g, asset.toon.shadow_color.b],
                ambient_color: [asset.toon.ambient_color.r, asset.toon.ambient_color.g, asset.toon.ambient_color.b],
                specular_color: [asset.toon.specular_color.r, asset.toon.specular_color.g, asset.toon.specular_color.b],
                specular_power: asset.toon.specular_power,
                sphere_blend: match asset.toon.sphere_blend {
                    engine_authoring::MaterialSphereBlendMode::Multiply => SphereBlendMode::Multiply,
                    engine_authoring::MaterialSphereBlendMode::Add => SphereBlendMode::Add,
                },
                sphere_coordinates: match asset.toon.sphere_coordinates {
                    engine_authoring::MaterialSphereCoordinateSource::ViewNormal => SphereCoordinateSource::ViewNormal,
                    engine_authoring::MaterialSphereCoordinateSource::AdditionalUv0 => SphereCoordinateSource::AdditionalUv0,
                },
                rim_color: [asset.toon.rim_color.r, asset.toon.rim_color.g, asset.toon.rim_color.b],
                rim_power: asset.toon.rim_power,
                rim_intensity: asset.toon.rim_intensity,
                ..ToonMaterial::default()
            },
            outline: OutlineMaterial {
                enabled: asset.outline.enabled,
                color: [asset.outline.color.r, asset.outline.color.g, asset.outline.color.b, asset.outline.color.a],
                width: asset.outline.width,
                internal_boundary_strength: asset.outline.internal_boundary_strength,
            },
            cast_shadow: asset.cast_shadow,
            receive_shadow: asset.receive_shadow,
            ..Self::default_surface()
        }
    }

    /// Attaches CPU-decoded slot data after manifest resolution.
    #[doc(hidden)]
    pub fn with_pending_texture_slots(mut self, slots: PendingMaterialTextures) -> Self {
        self.pending_texture = slots.base;
        self.pending_normal_texture = slots.normal;
        self.pending_metallic_roughness_texture = slots.metallic_roughness;
        self.pending_occlusion_texture = slots.occlusion;
        self.pending_emissive_texture = slots.emissive;
        self.toon.pending_ramp_texture = slots.ramp;
        self.toon.pending_sphere_texture = slots.sphere;
        self
    }

    /// Returns fields shared by every convenience constructor.
    fn default_surface() -> Self {
        Self {
            color: [1.0; 4],
            texture: None,
            pending_texture: None,
            normal_texture: None,
            pending_normal_texture: None,
            metallic_roughness_texture: None,
            pending_metallic_roughness_texture: None,
            occlusion_texture: None,
            pending_occlusion_texture: None,
            emissive_texture: None,
            pending_emissive_texture: None,
            emissive_color: [0.0; 3],
            normal_scale: 1.0,
            occlusion_strength: 1.0,
            roughness: 0.5,
            metallic: 0.0,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            cull_mode: CullMode::Back,
            shading_model: ShadingModel::StandardLit,
            toon: ToonMaterial::default(),
            outline: OutlineMaterial::default(),
            cast_shadow: true,
            receive_shadow: true,
        }
    }
}

impl Default for Material {
    fn default() -> Self {
        Self::color(1.0, 1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_texture_accepts_bmp_bytes() {
        // Encode a real BMP in memory so this regression test covers the
        // codec feature as well as the shared CPU texture decode path used by
        // material loading, Texture Preview, and UI images.
        let source = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([12, 34, 56, 255]),
        ));
        let mut bytes = std::io::Cursor::new(Vec::new());
        source
            .write_to(&mut bytes, image::ImageFormat::Bmp)
            .expect("BMP fixture must encode");

        let decoded = DecodedTexture::from_bytes(bytes.get_ref(), "fixture.bmp")
            .expect("BMP texture must decode");

        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.rgba8, [12, 34, 56, 255, 12, 34, 56, 255]);
    }

    #[test]
    fn mip_level_count_reaches_one_by_one() {
        assert_eq!(texture_mip_level_count(1, 1), 1);
        assert_eq!(texture_mip_level_count(2, 1), 2);
        assert_eq!(texture_mip_level_count(1, 8), 4);
        assert_eq!(texture_mip_level_count(4, 2), 3);
        assert_eq!(texture_mip_level_count(3, 5), 3);
    }

    #[test]
    fn linear_mipmap_filtering_averages_linear_values() {
        let source = [
            0, 0, 0, 255, // black
            255, 255, 255, 255, // white
        ];

        let chain = generate_rgba8_mip_chain(2, 1, &source, false);

        assert_eq!(chain.len(), source.len() + 4);
        assert_eq!(&chain[source.len()..], &[128, 128, 128, 255]);
    }

    #[test]
    fn srgb_mipmap_filtering_averages_rgb_in_linear_light() {
        let source = [
            0, 0, 0, 0, // black and transparent
            255, 255, 255, 255, // white and opaque
        ];

        let chain = generate_rgba8_mip_chain(2, 1, &source, true);

        assert_eq!(&chain[source.len()..], &[188, 188, 188, 128]);
    }

    #[test]
    fn mipmap_filtering_keeps_odd_edge_texels() {
        let source = [
            0, 0, 0, 255, // first texel
            0, 0, 0, 255, // second texel
            255, 255, 255, 255, // odd edge texel
        ];

        let horizontal = generate_rgba8_mip_chain(3, 1, &source, false);
        let vertical = generate_rgba8_mip_chain(1, 3, &source, false);

        assert_eq!(&horizontal[source.len()..], &[85, 85, 85, 255]);
        assert_eq!(&vertical[source.len()..], &[85, 85, 85, 255]);
    }

    #[test]
    fn texture_dimensions_must_be_non_zero_and_within_device_limit() {
        assert!(matches!(
            validate_texture_dimensions(0, 1, 4096),
            Err(TextureUploadError::InvalidDimensions { .. })
        ));
        assert!(matches!(
            validate_texture_dimensions(4097, 1, 4096),
            Err(TextureUploadError::InvalidDimensions { .. })
        ));
        assert!(validate_texture_dimensions(4096, 4096, 4096).is_ok());
    }

    #[test]
    fn rgba8_storage_must_match_declared_dimensions() {
        assert!(validate_rgba8_length(2, 3, 24).is_ok());
        assert!(matches!(
            validate_rgba8_length(2, 3, 23),
            Err(TextureUploadError::InvalidDataLength {
                expected: 24,
                actual: 23
            })
        ));
    }

    #[test]
    fn authoring_material_semantics_map_in_the_material_boundary() {
        let asset = engine_authoring::MaterialAsset {
            base_color: engine_authoring::LinearRgba {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.4,
            },
            emissive_color: engine_authoring::LinearRgba {
                r: 2.0,
                g: 1.0,
                b: 0.5,
                a: 1.0,
            },
            normal_scale: 0.6,
            occlusion_strength: 0.4,
            roughness: 0.25,
            metallic: 0.75,
            alpha_mode: engine_authoring::MaterialAlphaMode::Blend,
            alpha_cutoff: 0.3,
            cull_mode: engine_authoring::MaterialCullMode::None,
            shading_model: engine_authoring::MaterialShadingModel::Unlit,
            ..engine_authoring::MaterialAsset::default()
        };

        let runtime = Material::from_authoring_asset(&asset);
        assert_eq!(runtime.color, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(runtime.emissive_color, [2.0, 1.0, 0.5]);
        assert_eq!(runtime.normal_scale, 0.6);
        assert_eq!(runtime.occlusion_strength, 0.4);
        assert_eq!(runtime.roughness, 0.25);
        assert_eq!(runtime.metallic, 0.75);
        assert_eq!(runtime.alpha_mode, AlphaMode::Blend);
        assert_eq!(runtime.cull_mode, CullMode::None);
        assert_eq!(runtime.shading_model, ShadingModel::Unlit);
    }
}
