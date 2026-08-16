use bytemuck::{Pod, Zeroable};
use std::fmt;
use std::sync::{Arc, Weak};

use crate::camera::{select_active_game_camera, Camera3D, ViewportSize};
use crate::debug_draw::{DebugLine, DebugLines};
use crate::environment::EnvironmentGpuState;
use crate::light::{AmbientLight, DirectionalLight, SkySettings};
use crate::lod::InstanceStats;
use crate::material::{
    AlphaMode, CullMode, Material, MaterialSlots, ShadingModel, SphereBlendMode,
    SphereCoordinateSource,
};
use crate::material::{DecodedTexture, Texture};
use crate::mesh::{
    GpuMesh, GpuMeshCache, InstanceData, Mesh, MeshValidationError, TangentVertexData, Vertex,
};
use crate::postprocess::{PostProcessSettings, ToneMapOperator};
use crate::shadow::{
    cascade_view_projections, EnvironmentLighting, ShadowSettings, SHADOW_CASCADE_COUNT,
};
use engine_rig::skinning::{JointPalette, SkinnedMesh, MAX_JOINTS};
use crate::transform::{GlobalTransform, Parent};

/// Vertical reference resolution used to normalize projected outline widths.
const OUTLINE_REFERENCE_HEIGHT: u32 = 1024;
/// Maximum mask density relative to the 1024-texel outline-width reference.
const OUTLINE_MASK_MAX_HEIGHT: u32 = OUTLINE_REFERENCE_HEIGHT * 2;
/// Maximum mask texel budget, matching a 2560x1440 classification target.
const OUTLINE_MASK_MAX_TEXELS: u64 = 2560 * 1440;
/// Stores linear outline color and normalized projected radius.
const OUTLINE_STYLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Stores one transient runtime hierarchy identifier per visible surface.
const OUTLINE_GROUP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;
/// Low bits in the packed material identity; the high byte stores strength.
const OUTLINE_MATERIAL_ID_MASK: u32 = 0x00ff_ffff;
/// Fixed multisample count for the primary 3D raster pass.
///
/// This is an engine render-target contract, not an author-facing quality
/// setting: the primary color/depth attachments and every pipeline used in
/// that pass must agree on this value.
pub(crate) const MAIN_PASS_SAMPLE_COUNT: u32 = 4;

fn main_multisample_state() -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: MAIN_PASS_SAMPLE_COUNT,
        ..Default::default()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    world_position: [f32; 3],
    viewport_aspect: f32,
    view: [[f32; 4]; 4],
}

impl CameraUniform {
    fn identity() -> Self {
        Self {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            world_position: [0.0; 3],
            viewport_aspect: 1.0,
            view: glam::Mat4::IDENTITY.to_cols_array_2d(),
        }
    }

    fn from_matrices_position(
        view_proj: glam::Mat4,
        view: glam::Mat4,
        world_position: glam::Vec3,
        viewport_aspect: f32,
    ) -> Self {
        Self {
            view_proj: view_proj.to_cols_array_2d(),
            world_position: world_position.to_array(),
            viewport_aspect: valid_viewport_aspect(viewport_aspect),
            view: view.to_cols_array_2d(),
        }
    }
}

/// Returns a finite positive aspect ratio suitable for GPU projection math.
fn valid_viewport_aspect(aspect: f32) -> f32 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    }
}

// Must match the ShadowUniform struct in mesh.wgsl / mesh_skinned.wgsl:
// one light view-projection per cascade plus a parameter vector.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ShadowUniform {
    light_view_proj: [[[f32; 4]; 4]; SHADOW_CASCADE_COUNT],
    /// x: depth bias, y: normal bias, z: enabled (>0.5), w: shadow texel size.
    params: [f32; 4],
}

impl ShadowUniform {
    fn disabled() -> Self {
        Self {
            light_view_proj: [glam::Mat4::IDENTITY.to_cols_array_2d(); SHADOW_CASCADE_COUNT],
            params: [0.0; 4],
        }
    }
}

// Must match the LightUniform struct in mesh.wgsl — 48 bytes, stride 16.
// ambient_color[0-11], ambient_intensity[12-15],
// dir_direction[16-27], dir_intensity[28-31],
// dir_color[32-43], _padding[44-47]
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct LightUniform {
    ambient_color: [f32; 3],
    ambient_intensity: f32,
    dir_direction: [f32; 3],
    dir_intensity: f32,
    dir_color: [f32; 3],
    _padding: f32,
}

impl LightUniform {
    fn from_resources(ambient: &AmbientLight, directional: &DirectionalLight) -> Self {
        let dir = directional.direction.normalize_or_zero();
        Self {
            ambient_color: ambient.color.to_array(),
            ambient_intensity: ambient.intensity,
            dir_direction: dir.to_array(),
            dir_intensity: directional.intensity,
            dir_color: directional.color.to_array(),
            _padding: 0.0,
        }
    }
}

#[derive(Debug)]
pub(crate) enum RenderPreparationError {
    Mesh {
        entity: engine_ecs::Entity,
        source: MeshValidationError,
    },
    World(engine_ecs::WorldError),
}

impl fmt::Display for RenderPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mesh { entity, source } => {
                write!(formatter, "entity {entity} has an invalid mesh: {source}")
            }
            Self::World(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RenderPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mesh { source, .. } => Some(source),
            Self::World(error) => Some(error),
        }
    }
}

impl From<engine_ecs::WorldError> for RenderPreparationError {
    fn from(value: engine_ecs::WorldError) -> Self {
        Self::World(value)
    }
}

#[derive(Debug)]
pub(crate) enum RenderFrameError {
    Preparation(RenderPreparationError),
    Target(MainPassTargetError),
}

impl fmt::Display for RenderFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparation(error) => error.fmt(formatter),
            Self::Target(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RenderFrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Preparation(error) => Some(error),
            Self::Target(error) => Some(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MainPassTargetSpec {
    resolve_size: [u32; 2],
    resolve_samples: u32,
    depth_size: [u32; 2],
    depth_samples: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainPassTargetError {
    ResolveMustBeSingleSample { actual: u32 },
    DepthSampleCount { expected: u32, actual: u32 },
    SizeMismatch { resolve: [u32; 2], depth: [u32; 2] },
}

impl fmt::Display for MainPassTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResolveMustBeSingleSample { actual } => write!(
                formatter,
                "main-pass resolve target must be single-sampled, got {actual} samples"
            ),
            Self::DepthSampleCount { expected, actual } => write!(
                formatter,
                "main-pass depth target must use {expected} samples, got {actual}"
            ),
            Self::SizeMismatch { resolve, depth } => write!(
                formatter,
                "main-pass resolve/depth sizes differ: resolve={}x{}, depth={}x{}",
                resolve[0], resolve[1], depth[0], depth[1]
            ),
        }
    }
}

impl std::error::Error for MainPassTargetError {}

fn validate_main_pass_target_spec(spec: MainPassTargetSpec) -> Result<(), MainPassTargetError> {
    if spec.resolve_samples != 1 {
        return Err(MainPassTargetError::ResolveMustBeSingleSample {
            actual: spec.resolve_samples,
        });
    }
    if spec.depth_samples != MAIN_PASS_SAMPLE_COUNT {
        return Err(MainPassTargetError::DepthSampleCount {
            expected: MAIN_PASS_SAMPLE_COUNT,
            actual: spec.depth_samples,
        });
    }
    if spec.resolve_size != spec.depth_size {
        return Err(MainPassTargetError::SizeMismatch {
            resolve: spec.resolve_size,
            depth: spec.depth_size,
        });
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RenderStateError(wgpu::Error);

impl fmt::Display for RenderStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "render pipeline validation failed: {}", self.0)
    }
}

impl std::error::Error for RenderStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// A lightweight vertex used only by the debug line pipeline.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct DebugVertex {
    position: [f32; 3],
    color: [f32; 4],
}

impl DebugVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<DebugVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x4,
        ],
    };
}

struct DebugRenderState {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_capacity: usize,
}

impl DebugRenderState {
    async fn new(
        device: &wgpu::Device,
        camera_bgl: &wgpu::BindGroupLayout,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderStateError> {
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Debug shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/debug.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Debug pipeline layout"),
            bind_group_layouts: &[Some(camera_bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Debug line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[DebugVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: main_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        if let Some(error) = error_scope.pop().await {
            return Err(RenderStateError(error));
        }

        Ok(Self {
            pipeline,
            vertex_buffer: None,
            vertex_capacity: 0,
        })
    }

    fn upload_and_draw<'a>(
        &'a mut self,
        pass: &mut wgpu::RenderPass<'a>,
        camera_bind_group: &'a wgpu::BindGroup,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        lines: &[DebugLine],
    ) {
        if lines.is_empty() {
            return;
        }

        let vertices: Vec<DebugVertex> = lines
            .iter()
            .flat_map(|line| {
                [
                    DebugVertex {
                        position: line.from.to_array(),
                        color: line.color,
                    },
                    DebugVertex {
                        position: line.to.to_array(),
                        color: line.color,
                    },
                ]
            })
            .collect();

        let needed = vertices.len();
        if needed > self.vertex_capacity {
            let capacity = needed.next_power_of_two().max(64);
            let size = (capacity * std::mem::size_of::<DebugVertex>()) as u64;
            self.vertex_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Debug vertex buffer"),
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vertex_capacity = capacity;
        }

        if let Some(buf) = &self.vertex_buffer {
            queue.write_buffer(buf, 0, bytemuck::cast_slice(&vertices));
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, camera_bind_group, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..needed as u32, 0..1);
        }
    }
}

pub(crate) struct RenderState {
    camera_buffer: wgpu::Buffer,
    pub(crate) camera_bind_group: wgpu::BindGroup,
    pub(crate) camera_bgl: wgpu::BindGroupLayout,
    pub(crate) texture_bind_group_layout: wgpu::BindGroupLayout,
    white_texture: Arc<Texture>,
    flat_normal_texture: Arc<Texture>,
    environment: EnvironmentGpuState,
    light_buffer: wgpu::Buffer,
    pub(crate) light_bind_group: wgpu::BindGroup,
    pipelines: [[wgpu::RenderPipeline; 3]; 3],
    skinned_pipelines: [[wgpu::RenderPipeline; 3]; 3],
    outline_mask_pipelines: [wgpu::RenderPipeline; 3],
    outline_mask_skinned_pipelines: [wgpu::RenderPipeline; 3],
    outline_composite_bgl: wgpu::BindGroupLayout,
    outline_composite_pipeline: wgpu::RenderPipeline,
    joint_palette_bgl: wgpu::BindGroupLayout,
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_skinned_pipeline: wgpu::RenderPipeline,
    shadow_uniform_buffer: wgpu::Buffer,
    shadow_cascade_buffers: Vec<wgpu::Buffer>,
    shadow_cascade_bind_groups: Vec<wgpu::BindGroup>,
    shadow_layer_views: Vec<wgpu::TextureView>,
    sky_buffer: wgpu::Buffer,
    sky_bind_group: wgpu::BindGroup,
    sky_pipeline: wgpu::RenderPipeline,
}

/// GPU uniform for the procedural gradient sky.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyUniform {
    inv_view_proj: [[f32; 4]; 4],
    zenith: [f32; 4],
    horizon: [f32; 4],
    ground: [f32; 4],
}

impl SkyUniform {
    fn from_settings(view_projection: glam::Mat4, sky: &SkySettings) -> Self {
        let extend = |rgb: [f32; 3]| [rgb[0], rgb[1], rgb[2], 1.0];
        Self {
            inv_view_proj: view_projection.inverse().to_cols_array_2d(),
            zenith: extend(sky.zenith),
            horizon: extend(sky.horizon),
            ground: extend(sky.ground),
        }
    }
}

/// Pipeline state that cannot be changed through per-instance shader data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MaterialPipelineKey {
    alpha_mode: AlphaMode,
    cull_mode: CullMode,
    cast_shadow: bool,
    outline_enabled: bool,
}

impl MaterialPipelineKey {
    fn from_material(material: &Material) -> Self {
        Self {
            alpha_mode: material.alpha_mode,
            cull_mode: material.cull_mode,
            cast_shadow: material.cast_shadow,
            outline_enabled: material.outline.enabled,
        }
    }

    const fn alpha_index(self) -> usize {
        match self.alpha_mode {
            AlphaMode::Opaque => 0,
            AlphaMode::Mask => 1,
            AlphaMode::Blend => 2,
        }
    }

    const fn cull_index(self) -> usize {
        match self.cull_mode {
            CullMode::Back => 0,
            CullMode::Front => 1,
            CullMode::None => 2,
        }
    }
}

fn material_pipeline_key(alpha_index: usize, cull_index: usize) -> MaterialPipelineKey {
    let alpha_mode = [AlphaMode::Opaque, AlphaMode::Mask, AlphaMode::Blend][alpha_index];
    let cull_mode = [CullMode::Back, CullMode::Front, CullMode::None][cull_index];
    MaterialPipelineKey {
        alpha_mode,
        cull_mode,
        cast_shadow: true,
        outline_enabled: false,
    }
}

/// Material data that is constant across all instances sharing a bind group.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniformData {
    toon_shadow: [f32; 4],
    toon_ambient: [f32; 4],
    toon_specular: [f32; 4],
    toon_rim: [f32; 4],
    toon_params: [f32; 4],
    outline: [f32; 4],
    pbr_params: [f32; 4],
}

impl MaterialUniformData {
    fn from_material(material: &Material) -> Self {
        let has_ramp = material.toon.ramp_texture.is_some()
            || material.toon.pending_ramp_texture.is_some();
        let has_sphere = material.toon.sphere_texture.is_some()
            || material.toon.pending_sphere_texture.is_some();
        Self {
            toon_shadow: [
                material.toon.shadow_color[0],
                material.toon.shadow_color[1],
                material.toon.shadow_color[2],
                f32::from(has_ramp),
            ],
            toon_ambient: [
                material.toon.ambient_color[0],
                material.toon.ambient_color[1],
                material.toon.ambient_color[2],
                f32::from(material.receive_shadow),
            ],
            toon_specular: [
                material.toon.specular_color[0],
                material.toon.specular_color[1],
                material.toon.specular_color[2],
                material.toon.specular_power,
            ],
            toon_rim: [
                material.toon.rim_color[0],
                material.toon.rim_color[1],
                material.toon.rim_color[2],
                material.toon.rim_intensity,
            ],
            toon_params: [
                material.toon.rim_power,
                match material.toon.sphere_blend {
                    SphereBlendMode::Multiply => 0.0,
                    SphereBlendMode::Add => 1.0,
                },
                match material.toon.sphere_coordinates {
                    SphereCoordinateSource::ViewNormal => 0.0,
                    SphereCoordinateSource::AdditionalUv0 => 1.0,
                },
                f32::from(has_sphere),
            ],
            outline: [
                material.outline.color[0],
                material.outline.color[1],
                material.outline.color[2],
                if material.outline.enabled { material.outline.width } else { 0.0 },
            ],
            pbr_params: [
                material.normal_scale,
                material.occlusion_strength,
                0.0,
                0.0,
            ],
        }
    }

    fn key(self) -> [u32; 28] {
        bytemuck::cast(self)
    }
}

/// Per-instance payload used only while building the outline masks.
///
/// Keeping the transient hierarchy identifier out of [`InstanceData`]
/// preserves the public mesh layout and avoids charging ordinary material
/// draws for data that only the outline pass consumes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct OutlineInstanceData {
    /// Column-major model matrix shared with the ordinary mesh draw.
    model: [[f32; 4]; 4],
    /// RGBA instance tint; alpha participates in mask alpha testing.
    color: [f32; 4],
    /// Roughness, metallic, alpha cutoff, and alpha-mode numeric code.
    surface: [f32; 4],
    /// `x` stores the hierarchy root and `y` packs material identity/strength.
    outline_identity: [u32; 4],
}

impl OutlineInstanceData {
    /// Vertex layout paired with [`Vertex::LAYOUT`] in outline mask pipelines.
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            4 => Float32x4,
            5 => Float32x4,
            6 => Float32x4,
            7 => Float32x4,
            8 => Float32x4,
            12 => Float32x4,
            14 => Uint32x4,
        ],
    };

    /// Copies the shared transform and alpha data and appends its group ID.
    fn from_instance(
        instance: InstanceData,
        outline_group: u32,
        outline_material: u32,
    ) -> Self {
        Self {
            model: instance.model,
            color: instance.color,
            surface: instance.surface,
            outline_identity: [outline_group, outline_material, 0, 0],
        }
    }
}

/// Packs a transient material identity and normalized internal-boundary strength.
fn outline_material_identity(
    bind_group: &Arc<wgpu::BindGroup>,
    internal_boundary_strength: f32,
) -> u32 {
    let address = Arc::as_ptr(bind_group) as usize as u64;
    let mixed = address ^ (address >> 21) ^ (address >> 43);
    let material_id = ((mixed as u32) & OUTLINE_MATERIAL_ID_MASK).max(1);
    let strength = (internal_boundary_strength.clamp(0.0, 1.0) * 255.0).round() as u32;
    material_id | (strength << 24)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MaterialBindGroupKey {
    base: usize,
    normal: usize,
    metallic_roughness: usize,
    occlusion: usize,
    emissive: usize,
    ramp: usize,
    sphere: usize,
    uniform: [u32; 28],
}

/// Borrowed resources used to construct one generic material bind group.
struct MaterialBindResources<'a> {
    base: &'a Texture,
    normal: &'a Texture,
    metallic_roughness: &'a Texture,
    occlusion: &'a Texture,
    emissive: &'a Texture,
    ramp: &'a Texture,
    sphere: &'a Texture,
    uniform_buffer: &'a wgpu::Buffer,
}

struct StaticBatch {
    pipeline_key: MaterialPipelineKey,
    gpu_mesh: GpuMesh,
    /// Submesh drawn by this batch (ADR 0076).
    submesh: usize,
    texture_bind_group: Arc<wgpu::BindGroup>,
    instances: Vec<InstanceData>,
    /// Mask instances parallel the ordinary draw while carrying root IDs.
    outline_instances: Vec<OutlineInstanceData>,
}

#[derive(Debug, Clone, Copy)]
enum BlendDraw {
    StaticBatch(usize),
    Skinned(usize),
}

#[derive(Clone, Copy)]
struct MaterialShaderStages<'a> {
    vertex: &'a wgpu::ShaderModule,
    fragment: &'a wgpu::ShaderModule,
}

fn create_material_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shaders: MaterialShaderStages<'_>,
    format: wgpu::TextureFormat,
    buffers: &[wgpu::VertexBufferLayout<'_>],
    key: MaterialPipelineKey,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shaders.vertex,
            entry_point: Some("vs_main"),
            buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shaders.fragment,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: match key.alpha_mode {
                    AlphaMode::Blend => Some(wgpu::BlendState::ALPHA_BLENDING),
                    AlphaMode::Opaque | AlphaMode::Mask => None,
                },
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: match key.cull_mode {
                CullMode::Back => Some(wgpu::Face::Back),
                CullMode::Front => Some(wgpu::Face::Front),
                CullMode::None => None,
            },
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(key.alpha_mode != AlphaMode::Blend),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: main_multisample_state(),
        multiview_mask: None,
        cache: None,
    })
}

/// Declares how one material texture slot interprets stored RGBA8 values.
///
/// Color textures are stored with the sRGB transfer function and decode to
/// scene-linear RGB when sampled. Numeric/vector data textures must bypass
/// that transfer function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureSampleEncoding {
    SrgbColor,
    LinearData,
}

struct CachedDecodedTexture {
    source: Weak<DecodedTexture>,
    texture: Arc<Texture>,
}

struct CachedMaterialBindGroup {
    base: Weak<Texture>,
    normal: Weak<Texture>,
    metallic_roughness: Weak<Texture>,
    occlusion: Weak<Texture>,
    emissive: Weak<Texture>,
    ramp: Weak<Texture>,
    sphere: Weak<Texture>,
    _uniform_buffer: wgpu::Buffer,
    bind_group: Arc<wgpu::BindGroup>,
}

/// GPU attachments used to classify visible surfaces before compositing.
struct OutlineTargets {
    /// Dimensions are cached so render-viewport changes recreate attachments.
    size: [u32; 2],
    /// Owns the style texture for the lifetime of its view and bind group.
    _style_texture: wgpu::Texture,
    style_view: wgpu::TextureView,
    /// Owns the integer hierarchy-group texture.
    _group_texture: wgpu::Texture,
    group_view: wgpu::TextureView,
    /// Owns depth used to retain only the nearest visible surface.
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// Samples both color attachments in the fullscreen composite.
    composite_bind_group: wgpu::BindGroup,
}

impl OutlineTargets {
    /// Allocates viewport-tracking mask attachments and their sample bind group.
    fn new(
        device: &wgpu::Device,
        composite_bgl: &wgpu::BindGroupLayout,
        viewport_size: [u32; 2],
    ) -> Self {
        let extent = outline_mask_extent(device, viewport_size);
        let style_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Outline style mask"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTLINE_STYLE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let style_view = style_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let group_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Outline group mask"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTLINE_GROUP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let group_view = group_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Outline mask depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Outline composite BG"),
            layout: composite_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&style_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&group_view),
                },
            ],
        });

        Self {
            size: [extent.width, extent.height],
            _style_texture: style_texture,
            style_view,
            _group_texture: group_texture,
            group_view,
            _depth_texture: depth_texture,
            depth_view,
            composite_bind_group,
        }
    }
}

/// Chooses a viewport-tracking mask within the renderer's quality budget.
fn outline_mask_extent(device: &wgpu::Device, viewport_size: [u32; 2]) -> wgpu::Extent3d {
    outline_mask_extent_for_limit(viewport_size, device.limits().max_texture_dimension_2d)
}

/// Applies the outline-mask quality budget while preserving viewport aspect.
fn outline_mask_extent_for_limit(
    viewport_size: [u32; 2],
    max_texture_dimension_2d: u32,
) -> wgpu::Extent3d {
    let maximum = max_texture_dimension_2d.max(1) as f64;
    let width = viewport_size[0].max(1) as f64;
    let height = viewport_size[1].max(1) as f64;
    let texel_scale = (OUTLINE_MASK_MAX_TEXELS as f64 / (width * height)).sqrt();

    let scale = 1.0_f64
        .min(maximum / width)
        .min(maximum / height)
        .min(OUTLINE_MASK_MAX_HEIGHT as f64 / height)
        .min(texel_scale);
    let width = (width * scale).floor().max(1.0) as u32;
    let height = (height * scale).floor().max(1.0) as u32;

    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

/// Multisampled color storage resolved into the caller-owned single-sample view.
struct MainPassColorTarget {
    size: [u32; 2],
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl MainPassColorTarget {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: [u32; 2],
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Main pass MSAA color"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MAIN_PASS_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            size,
            _texture: texture,
            view,
        }
    }
}

pub(crate) struct WorldRenderer {
    render: RenderState,
    debug: DebugRenderState,
    color_format: wgpu::TextureFormat,
    main_color_target: Option<MainPassColorTarget>,
    outline_targets: Option<OutlineTargets>,
    decoded_srgb_cache: std::collections::HashMap<usize, CachedDecodedTexture>,
    decoded_linear_cache: std::collections::HashMap<usize, CachedDecodedTexture>,
    material_bind_group_cache:
        std::collections::HashMap<MaterialBindGroupKey, CachedMaterialBindGroup>,
}

impl WorldRenderer {
    pub(crate) async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderStateError> {
        let render = RenderState::new(device, queue, format).await?;
        let debug = DebugRenderState::new(device, &render.camera_bgl, format).await?;
        Ok(Self {
            render,
            debug,
            color_format: format,
            main_color_target: None,
            outline_targets: None,
            decoded_srgb_cache: Default::default(),
            decoded_linear_cache: Default::default(),
            material_bind_group_cache: Default::default(),
        })
    }

    /// Validates the caller-owned resolve/depth pair and (re)allocates the
    /// transient multisampled color attachment when the viewport size changes.
    fn ensure_main_pass_target(
        &mut self,
        device: &wgpu::Device,
        resolve_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), MainPassTargetError> {
        let resolve_texture = resolve_view.texture();
        let depth_texture = depth_view.texture();
        let resolve_extent = resolve_texture.size();
        let depth_extent = depth_texture.size();
        let spec = MainPassTargetSpec {
            resolve_size: [resolve_extent.width, resolve_extent.height],
            resolve_samples: resolve_texture.sample_count(),
            depth_size: [depth_extent.width, depth_extent.height],
            depth_samples: depth_texture.sample_count(),
        };
        validate_main_pass_target_spec(spec)?;

        if self
            .main_color_target
            .as_ref()
            .is_some_and(|target| target.size == spec.resolve_size)
        {
            return Ok(());
        }
        self.main_color_target = Some(MainPassColorTarget::new(
            device,
            self.color_format,
            spec.resolve_size,
        ));
        Ok(())
    }

    /// Recreates mask attachments when the chosen render-viewport extent changes.
    fn ensure_outline_targets(&mut self, device: &wgpu::Device, viewport_size: [u32; 2]) {
        let expected = outline_mask_extent(device, viewport_size);
        let expected_size = [expected.width, expected.height];
        if self
            .outline_targets
            .as_ref()
            .is_some_and(|targets| targets.size == expected_size)
        {
            return;
        }

        self.outline_targets = Some(OutlineTargets::new(
            device,
            &self.render.outline_composite_bgl,
            viewport_size,
        ));
    }

    pub(crate) fn render_to_view(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), RenderFrameError> {
        let camera = Self::get_camera(world);
        match camera {
            Some((camera, transform)) => self.render_to_view_with_camera(
                world,
                &camera,
                &transform,
                device,
                queue,
                color_view,
                depth_view,
            ),
            None => self.render_to_view_without_camera(
                world, device, queue, color_view, depth_view,
            ),
        }
    }

    // This mirrors the existing render entry point plus the explicit camera
    // pair; grouping the caller-owned wgpu views would obscure that contract.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_to_view_with_camera(
        &mut self,
        world: &mut engine_ecs::World,
        camera: &Camera3D,
        camera_transform: &crate::transform::Transform,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), RenderFrameError> {
        self.render_to_view_with_optional_camera(
            world,
            Some((camera, camera_transform)),
            device,
            queue,
            color_view,
            depth_view,
        )
    }

    fn render_to_view_without_camera(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), RenderFrameError> {
        self.render_to_view_with_optional_camera(
            world, None, device, queue, color_view, depth_view,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_to_view_with_optional_camera(
        &mut self,
        world: &mut engine_ecs::World,
        shadow_camera: Option<(&Camera3D, &crate::transform::Transform)>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), RenderFrameError> {
        upload_pending_meshes(world, device).map_err(RenderFrameError::Preparation)?;
        upload_morphed_vertices(world, queue);

        let (vp, view, camera_position, viewport_aspect) = shadow_camera
            .map(|(camera, transform)| {
                (
                    camera.view_projection_matrix(transform),
                    Camera3D::view_matrix(transform),
                    transform.translation,
                    valid_viewport_aspect(camera.aspect),
                )
            })
            .unwrap_or((
                glam::Mat4::IDENTITY,
                glam::Mat4::IDENTITY,
                glam::Vec3::ZERO,
                1.0,
            ));
        self.render
            .update_camera(queue, vp, view, camera_position, viewport_aspect);

        let sky = world
            .get_resource::<SkySettings>()
            .cloned()
            .unwrap_or_default();
        self.render.update_sky(queue, vp, &sky);

        let environment = world
            .get_resource::<EnvironmentLighting>()
            .cloned()
            .unwrap_or_default();
        let skybox = Self::environment_texture(world, environment.skybox);
        let diffuse_irradiance =
            Self::environment_texture(world, environment.diffuse_irradiance);
        self.render.update_environment(
            device,
            queue,
            &environment,
            skybox.as_ref(),
            diffuse_irradiance.as_ref(),
        );

        let ambient = world
            .get_resource::<AmbientLight>()
            .cloned()
            .unwrap_or_default();
        let directional = world
            .get_resource::<DirectionalLight>()
            .cloned()
            .unwrap_or_default();
        self.render.update_light(queue, &ambient, &directional);

        let shadow_settings = world
            .get_resource::<ShadowSettings>()
            .cloned()
            .unwrap_or_default();
        let shadows_enabled = shadow_settings.enabled
            && directional.intensity > 0.0
            && shadow_camera.is_some()
            && directional.direction.normalize_or_zero() != glam::Vec3::ZERO;
        let cascade_matrices = match (shadow_camera, shadows_enabled) {
            (Some((camera, camera_transform)), true) => cascade_view_projections(
                camera,
                camera_transform,
                directional.direction,
                &shadow_settings,
            ),
            _ => [glam::Mat4::IDENTITY; SHADOW_CASCADE_COUNT],
        };
        self.render
            .update_shadows(queue, &cascade_matrices, &shadow_settings, shadows_enabled);

        let batches = self.collect_batches(world, device, queue, camera_position);
        let skinned_draws = self.collect_skinned_draws(world, device, queue, camera_position);
        let outlines_enabled = batches.iter().any(|batch| {
            batch.pipeline_key.outline_enabled && !batch.instances.is_empty()
        }) || skinned_draws
            .iter()
            .any(|draw| draw.pipeline_key.outline_enabled);

        if outlines_enabled {
            let viewport_size = world
                .get_resource::<ViewportSize>()
                .map(|viewport| [viewport.width.max(1), viewport.height.max(1)])
                .unwrap_or_else(|| {
                    let height = OUTLINE_REFERENCE_HEIGHT;
                    let width = (height as f32 * viewport_aspect).round().max(1.0) as u32;
                    [width, height]
                });
            self.ensure_outline_targets(device, viewport_size);
        }

        // Update InstanceStats resource if present.
        if let Some(stats) = world.get_resource_mut::<InstanceStats>() {
            stats.batch_count = batches.len();
            stats.total_instances = batches.iter().map(|batch| batch.instances.len()).sum();
        }

        // Build instance buffers before opening the render pass so they outlive it.
        let instance_buffers: Vec<wgpu::Buffer> = batches
            .iter()
            .map(|batch| {
                self.render
                    .make_instance_buffer(device, bytemuck::cast_slice(&batch.instances))
            })
            .collect();

        // Mask buffers are omitted entirely when no visible material asks
        // for outlines, keeping the historical no-outline frame cost.
        let outline_instance_buffers: Vec<wgpu::Buffer> = if outlines_enabled {
            batches
                .iter()
                .map(|batch| {
                    self.render.make_instance_buffer(
                        device,
                        bytemuck::cast_slice(&batch.outline_instances),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        // Per-skinned-draw GPU resources, created up front for the same reason.
        let skinned_resources: Vec<SkinnedFrameResources> = skinned_draws
            .iter()
            .map(|draw| {
                let instance_buffer = self
                    .render
                    .make_instance_buffer(device, bytemuck::bytes_of(&draw.instance));
                let outline_instance_buffer = outlines_enabled.then(|| {
                    self.render.make_instance_buffer(
                        device,
                        bytemuck::bytes_of(&draw.outline_instance),
                    )
                });
                let palette_buffer = RenderState::make_uniform_buffer(
                    device,
                    bytemuck::cast_slice(&draw.palette),
                    "Joint palette",
                );
                let palette_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Joint palette BG"),
                    layout: &self.render.joint_palette_bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: palette_buffer.as_entire_binding(),
                    }],
                });
                SkinnedFrameResources {
                    instance_buffer,
                    outline_instance_buffer,
                    palette_bind_group,
                }
            })
            .collect();

        // Blended static batches and skinned draws share one back-to-front
        // order. Sorting each family independently is insufficient when, for
        // example, a transparent environment mesh overlaps a character.
        let mut blend_draws = batches
            .iter()
            .enumerate()
            .filter(|(_, batch)| batch.pipeline_key.alpha_mode == AlphaMode::Blend)
            .map(|(index, batch)| {
                (
                    BlendDraw::StaticBatch(index),
                    batch_distance_squared(batch, camera_position),
                )
            })
            .chain(
                skinned_draws
                    .iter()
                    .enumerate()
                    .filter(|(_, draw)| draw.pipeline_key.alpha_mode == AlphaMode::Blend)
                    .map(|(index, draw)| {
                        (
                            BlendDraw::Skinned(index),
                            instance_distance_squared(&draw.instance, camera_position),
                        )
                    }),
            )
            .collect::<Vec<_>>();
        blend_draws.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        self.ensure_main_pass_target(device, color_view, depth_view)
            .map_err(RenderFrameError::Target)?;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Frame encoder"),
        });

        // Depth-only shadow passes, one per cascade (Phase 50, ADR 0036).
        // Skinned meshes do not cast shadows in v1; batched static and
        // particle instances do.
        if shadows_enabled {
            for (cascade_index, layer_view) in self.render.shadow_layer_views.iter().enumerate() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Shadow pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: layer_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.render.shadow_pipeline);
                pass.set_bind_group(
                    0,
                    &self.render.shadow_cascade_bind_groups[cascade_index],
                    &[],
                );
                for (batch, instance_buf) in batches.iter().zip(instance_buffers.iter()) {
                    if batch.instances.is_empty() || !batch.pipeline_key.cast_shadow {
                        continue;
                    }
                    batch.gpu_mesh.draw_instanced_submesh(
                        &mut pass,
                        instance_buf,
                        batch.instances.len() as u32,
                        Some(batch.submesh),
                    );
                }

                // Skinned entities whose mesh has no skinning data cast a
                // static shadow while the static pipeline is still bound.
                for (draw, resources) in
                    skinned_draws.iter().zip(skinned_resources.iter())
                {
                    if draw.pipeline_key.cast_shadow && draw.gpu_mesh.skinning_buffer.is_none() {
                        draw.gpu_mesh.draw_instanced_submesh(
                            &mut pass,
                            &resources.instance_buffer,
                            1,
                            Some(draw.submesh),
                        );
                    }
                }

                // Skinned casters deform with the same palette as the main
                // pass so shadows match the rendered pose (Phase 50-D).
                if skinned_draws
                    .iter()
                    .any(|draw| draw.gpu_mesh.skinning_buffer.is_some())
                {
                    pass.set_pipeline(&self.render.shadow_skinned_pipeline);
                    pass.set_bind_group(
                        0,
                        &self.render.shadow_cascade_bind_groups[cascade_index],
                        &[],
                    );
                    for (draw, resources) in
                        skinned_draws.iter().zip(skinned_resources.iter())
                    {
                        if !draw.pipeline_key.cast_shadow || draw.gpu_mesh.skinning_buffer.is_none() {
                            continue;
                        }
                        pass.set_bind_group(1, &resources.palette_bind_group, &[]);
                        draw.gpu_mesh.draw_skinned_submesh(
                            &mut pass,
                            &resources.instance_buffer,
                            Some(draw.submesh),
                        );
                    }
                }
            }
        }

        if outlines_enabled {
            let targets = self
                .outline_targets
                .as_ref()
                .expect("enabled outline rendering must allocate mask targets");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Outline mask pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.style_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: &targets.group_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.render.camera_bind_group, &[]);

            // Every visible surface enters the group/depth masks, including
            // outline-disabled occluders that must hide outlines behind them.
            for (batch, outline_buffer) in batches.iter().zip(outline_instance_buffers.iter()) {
                if batch.instances.is_empty() {
                    continue;
                }
                pass.set_pipeline(
                    &self.render.outline_mask_pipelines[batch.pipeline_key.cull_index()],
                );
                pass.set_bind_group(1, batch.texture_bind_group.as_ref(), &[]);
                batch.gpu_mesh.draw_instanced_submesh(
                    &mut pass,
                    outline_buffer,
                    batch.instances.len() as u32,
                    Some(batch.submesh),
                );
            }

            for (draw, resources) in skinned_draws.iter().zip(skinned_resources.iter()) {
                let outline_buffer = resources
                    .outline_instance_buffer
                    .as_ref()
                    .expect("enabled outline rendering must create mask instance buffers");
                if draw.gpu_mesh.skinning_buffer.is_some() {
                    pass.set_pipeline(
                        &self.render.outline_mask_skinned_pipelines
                            [draw.pipeline_key.cull_index()],
                    );
                    pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                    pass.set_bind_group(2, &resources.palette_bind_group, &[]);
                    draw.gpu_mesh.draw_skinned_submesh(
                        &mut pass,
                        outline_buffer,
                        Some(draw.submesh),
                    );
                } else {
                    pass.set_pipeline(
                        &self.render.outline_mask_pipelines[draw.pipeline_key.cull_index()],
                    );
                    pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                    draw.gpu_mesh.draw_instanced_submesh(
                        &mut pass,
                        outline_buffer,
                        1,
                        Some(draw.submesh),
                    );
                }
            }
        }

        let debug_line_segments: Vec<DebugLine> = world
            .get_resource::<DebugLines>()
            .filter(|debug_lines| debug_lines.enabled)
            .map(|debug_lines| debug_lines.lines.clone())
            .unwrap_or_default();

        {
            let main_color_view = &self
                .main_color_target
                .as_ref()
                .expect("validated main pass must allocate an MSAA color target")
                .view;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Main pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: main_color_view,
                    resolve_target: Some(color_view),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_bind_group(0, &self.render.camera_bind_group, &[]);
            pass.set_bind_group(2, &self.render.light_bind_group, &[]);

            if sky.enabled || self.render.environment.has_skybox() {
                // A resolved skybox replaces the procedural gradient. Both
                // draw before geometry with depth writes disabled.
                pass.set_pipeline(&self.render.sky_pipeline);
                pass.set_bind_group(0, &self.render.sky_bind_group, &[]);
                pass.set_bind_group(1, self.render.environment.bind_group(), &[]);
                pass.draw(0..3, 0..1);
                pass.set_bind_group(0, &self.render.camera_bind_group, &[]);
            }
            pass.set_bind_group(4, self.render.environment.bind_group(), &[]);

            // Opaque and masked geometry from every mesh path writes depth
            // before any blended draw. Keeping static and skinned paths in
            // the same two phases prevents transparent static geometry from
            // accidentally preceding an opaque character.
            {
                let blend_phase = false;

                for (batch, instance_buf) in batches.iter().zip(instance_buffers.iter()) {
                    if batch.instances.is_empty()
                        || (batch.pipeline_key.alpha_mode == AlphaMode::Blend) != blend_phase
                    {
                        continue;
                    }
                    pass.set_pipeline(
                        &self.render.pipelines[batch.pipeline_key.alpha_index()]
                            [batch.pipeline_key.cull_index()],
                    );
                    pass.set_bind_group(1, batch.texture_bind_group.as_ref(), &[]);
                    batch.gpu_mesh.draw_material_instanced_submesh(
                        &mut pass,
                        instance_buf,
                        batch.instances.len() as u32,
                        Some(batch.submesh),
                    );
                }

                // A selected glTF primitive may legitimately have no skin
                // attributes. It keeps the selected material pipeline while
                // degrading to a static draw.
                for (draw, resources) in
                    skinned_draws.iter().zip(skinned_resources.iter())
                {
                    if draw.gpu_mesh.skinning_buffer.is_some()
                        || (draw.pipeline_key.alpha_mode == AlphaMode::Blend) != blend_phase
                    {
                        continue;
                    }
                    pass.set_pipeline(
                        &self.render.pipelines[draw.pipeline_key.alpha_index()]
                            [draw.pipeline_key.cull_index()],
                    );
                    pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                    draw.gpu_mesh.draw_material_instanced_submesh(
                        &mut pass,
                        &resources.instance_buffer,
                        1,
                        Some(draw.submesh),
                    );
                }

                for (draw, resources) in
                    skinned_draws.iter().zip(skinned_resources.iter())
                {
                    if draw.gpu_mesh.skinning_buffer.is_none()
                        || (draw.pipeline_key.alpha_mode == AlphaMode::Blend) != blend_phase
                    {
                        continue;
                    }
                    pass.set_pipeline(
                        &self.render.skinned_pipelines[draw.pipeline_key.alpha_index()]
                            [draw.pipeline_key.cull_index()],
                    );
                    pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                    pass.set_bind_group(3, &resources.palette_bind_group, &[]);
                    draw.gpu_mesh.draw_material_skinned_submesh(
                        &mut pass,
                        &resources.instance_buffer,
                        Some(draw.submesh),
                    );
                }
            }

            // Debug lines are world-space geometry, not a screen overlay, so
            // they belong between the two phases. Blended draws never write
            // depth, so a line drawn after them depth-tests against whatever
            // stood behind them and paints over a surface that is actually in
            // front — which is how the editor's floor grid showed through a
            // character's clothing. Drawn here, blended surfaces composite
            // over the line the way they do over the rest of the scene.
            self.debug.upload_and_draw(
                &mut pass,
                &self.render.camera_bind_group,
                device,
                queue,
                &debug_line_segments,
            );

            for (draw, _) in &blend_draws {
                match *draw {
                    BlendDraw::StaticBatch(index) => {
                        let batch = &batches[index];
                        let instance_buffer = &instance_buffers[index];
                        pass.set_pipeline(
                            &self.render.pipelines[batch.pipeline_key.alpha_index()]
                                [batch.pipeline_key.cull_index()],
                        );
                        pass.set_bind_group(1, batch.texture_bind_group.as_ref(), &[]);
                        batch.gpu_mesh.draw_material_instanced_submesh(
                            &mut pass,
                            instance_buffer,
                            batch.instances.len() as u32,
                            Some(batch.submesh),
                        );
                    }
                    BlendDraw::Skinned(index) => {
                        let draw = &skinned_draws[index];
                        let resources = &skinned_resources[index];
                        if draw.gpu_mesh.skinning_buffer.is_some() {
                            pass.set_pipeline(
                                &self.render.skinned_pipelines[draw.pipeline_key.alpha_index()]
                                    [draw.pipeline_key.cull_index()],
                            );
                            pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                            pass.set_bind_group(3, &resources.palette_bind_group, &[]);
                            draw.gpu_mesh.draw_material_skinned_submesh(
                                &mut pass,
                                &resources.instance_buffer,
                                Some(draw.submesh),
                            );
                        } else {
                            pass.set_pipeline(
                                &self.render.pipelines[draw.pipeline_key.alpha_index()]
                                    [draw.pipeline_key.cull_index()],
                            );
                            pass.set_bind_group(1, draw.texture_bind_group.as_ref(), &[]);
                            draw.gpu_mesh.draw_material_instanced_submesh(
                                &mut pass,
                                &resources.instance_buffer,
                                1,
                                Some(draw.submesh),
                            );
                        }
                    }
                }
            }

        }

        // Outline classification stays single-sampled because its integer
        // hierarchy mask is sampled directly. Composite after the 4x scene
        // has resolved, so the screen-space pass also remains 1x and cannot
        // accidentally mismatch the multisampled main pipelines.
        if outlines_enabled {
            let targets = self
                .outline_targets
                .as_ref()
                .expect("enabled outline rendering must allocate mask targets");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Outline composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.render.outline_composite_pipeline);
            pass.set_bind_group(0, &targets.composite_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        if let Some(debug_lines) = world.get_resource_mut::<DebugLines>() {
            debug_lines.clear();
        }

        queue.submit(std::iter::once(encoder.finish()));
        Ok(())
    }

    fn environment_texture(
        world: &engine_ecs::World,
        runtime_id: Option<crate::asset::RuntimeAssetId>,
    ) -> Option<Arc<Texture>> {
        let runtime_id = runtime_id?;
        let assets = world.get_resource::<crate::asset::Assets<Arc<Texture>>>()?;
        let handle = assets.handle(runtime_id)?;
        assets.get(&handle).cloned()
    }

    /// Returns a clone of the selected Game View camera and its transform.
    fn get_camera(
        world: &mut engine_ecs::World,
    ) -> Option<(Camera3D, crate::transform::Transform)> {
        use crate::transform::Transform;
        use engine_ecs::Query;

        let query = Query::<(&Camera3D, &Transform)>::new(world);
        select_active_game_camera(query.iter())
            .map(|(_, (camera, transform))| (camera.clone(), transform.clone()))
    }

    /// Returns the stable-per-frame outline group for one render entity.
    ///
    /// Static meshes share the topmost reachable transform ancestor. Skinned
    /// meshes start at their rig so independently spawned skin parts still
    /// suppress seams when they belong to the same character hierarchy.
    /// Invalid parent cycles resolve to the lowest entity ID in the cycle,
    /// keeping the mask deterministic without trusting malformed hierarchy
    /// data indefinitely.
    fn outline_group_id(world: &engine_ecs::World, entity: engine_ecs::Entity) -> u32 {
        let mut current = world
            .get_component::<SkinnedMesh>(entity)
            .map(|skinned| skinned.rig)
            .unwrap_or(entity);
        let mut visited: Vec<engine_ecs::Entity> = Vec::new();

        loop {
            if let Some(cycle_start) = visited.iter().position(|candidate| *candidate == current) {
                current = visited[cycle_start..]
                    .iter()
                    .copied()
                    .min_by_key(|candidate| candidate.id())
                    .unwrap_or(current);
                break;
            }

            visited.push(current);
            let Some(parent) = world.get_component::<Parent>(current) else {
                break;
            };
            current = parent.0;
        }

        current.id()
    }

    /// Adds one instance per submesh of `gpu_mesh` to the matching batches.
    ///
    /// Each submesh resolves its own material through `slots` (ADR 0076), so
    /// instances of the same submesh with the same material still batch
    /// together while different parts of one mesh reach different pipelines
    /// and bind groups.
    #[allow(clippy::too_many_arguments)]
    fn batch_submeshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        batches: &mut hashbrown::HashMap<(usize, usize, usize, MaterialPipelineKey), StaticBatch>,
        gpu_mesh: &GpuMesh,
        matrix: glam::Mat4,
        material: &Material,
        slots: &MaterialSlots,
        outline_group: u32,
    ) {
        let mesh_key = Arc::as_ptr(&gpu_mesh.vertex_buffer) as usize;
        for submesh in 0..gpu_mesh.submeshes.len().max(1) {
            let resolved = slots.resolve(submesh, material);
            let texture_bind_group = self.resolve_material_bind_group(device, queue, resolved);
            let tex_key = Arc::as_ptr(&texture_bind_group) as usize;
            let pipeline_key = MaterialPipelineKey::from_material(resolved);
            let instance = instance_from_material(matrix, resolved.color, resolved);
            let outline_material = outline_material_identity(
                &texture_bind_group,
                resolved.outline.internal_boundary_strength,
            );
            let batch = batches
                .entry((mesh_key, submesh, tex_key, pipeline_key))
                .or_insert_with(|| StaticBatch {
                    pipeline_key,
                    gpu_mesh: gpu_mesh.clone(),
                    submesh,
                    texture_bind_group: Arc::clone(&texture_bind_group),
                    instances: Vec::new(),
                    outline_instances: Vec::new(),
                });
            batch.instances.push(instance);
            batch
                .outline_instances
                .push(OutlineInstanceData::from_instance(
                    instance,
                    outline_group,
                    outline_material,
                ));
        }
    }

    /// Records one skinned draw per submesh, resolving each slot's material.
    ///
    /// Skinned meshes already draw once per entity, so splitting by submesh
    /// only multiplies the draw count by the number of parts the mesh has.
    #[allow(clippy::too_many_arguments)]
    fn push_skinned_submeshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draws: &mut Vec<SkinnedDraw>,
        gpu_mesh: GpuMesh,
        matrix: glam::Mat4,
        palette: &[[f32; 16]],
        material: &Material,
        slots: &MaterialSlots,
        outline_group: u32,
    ) {
        for submesh in 0..gpu_mesh.submeshes.len().max(1) {
            let resolved = slots.resolve(submesh, material);
            let texture_bind_group = self.resolve_material_bind_group(device, queue, resolved);
            let instance = instance_from_material(matrix, resolved.color, resolved);
            let outline_material = outline_material_identity(
                &texture_bind_group,
                resolved.outline.internal_boundary_strength,
            );
            draws.push(SkinnedDraw {
                gpu_mesh: gpu_mesh.clone(),
                submesh,
                texture_bind_group,
                instance,
                outline_instance: OutlineInstanceData::from_instance(
                    instance,
                    outline_group,
                    outline_material,
                ),
                pipeline_key: MaterialPipelineKey::from_material(resolved),
                palette: palette.to_vec(),
            });
        }
    }

    /// Groups visible entities by mesh, texture, and fixed pipeline state.
    ///
    /// Scalar/color material fields remain per-instance, while alpha blending
    /// and culling join the batch key because wgpu fixes those values when a
    /// render pipeline is created.
    fn collect_batches(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_position: glam::Vec3,
    ) -> Vec<StaticBatch> {
        use crate::asset::Handle;
        use engine_ecs::{Query, Without};
        use hashbrown::HashMap;

        self.decoded_srgb_cache
            .retain(|_, cached| cached.source.strong_count() > 0);
        self.decoded_linear_cache
            .retain(|_, cached| cached.source.strong_count() > 0);
        self.material_bind_group_cache.retain(|_, cached| {
            cached.base.strong_count() > 0
                && cached.normal.strong_count() > 0
                && cached.metallic_roughness.strong_count() > 0
                && cached.occlusion.strong_count() > 0
                && cached.emissive.strong_count() > 0
                && cached.ramp.strong_count() > 0
                && cached.sphere.strong_count() > 0
        });

        type BatchMap = HashMap<(usize, usize, usize, MaterialPipelineKey), StaticBatch>;
        let mut batches: BatchMap = HashMap::new();

        // -- Handle<Mesh> entities: resolve through GpuMeshCache --
        // Skinned entities (JointPalette) draw through the skinned path.
        {
            type HandleMeshQuery<'w> = Query<
                'w,
                (
                    &'w Handle<Mesh>,
                    &'w GlobalTransform,
                    Option<&'w Material>,
                    Option<&'w MaterialSlots>,
                ),
                Without<JointPalette>,
            >;
            let q = HandleMeshQuery::new(world);
            let entries: Vec<_> = q
                .iter()
                .map(|(entity, (handle, global, material, slots))| {
                    (
                        entity,
                        handle.id(),
                        global.matrix(),
                        material.cloned().unwrap_or_default(),
                        slots.cloned().unwrap_or_default(),
                    )
                })
                .collect();

            for (entity, mesh_id, matrix, material, slots) in entries {
                let gpu_mesh = match world.get_resource::<GpuMeshCache>() {
                    Some(cache) => match cache.get(mesh_id) {
                        Some(gm) => gm.clone(),
                        None => continue,
                    },
                    None => continue,
                };
                self.batch_submeshes(
                    device,
                    queue,
                    &mut batches,
                    &gpu_mesh,
                    matrix,
                    &material,
                    &slots,
                    Self::outline_group_id(world, entity),
                );
            }
        }

        // -- Direct GpuMesh entities (no Handle<Mesh>): batch by vertex_buffer_ptr --
        {
            type DirectMeshQuery<'w> = Query<
                'w,
                (
                    &'w GpuMesh,
                    &'w GlobalTransform,
                    Option<&'w Material>,
                    Option<&'w MaterialSlots>,
                ),
                (Without<Handle<Mesh>>, Without<JointPalette>),
            >;
            let q = DirectMeshQuery::new(world);
            let entries: Vec<_> = q
                .iter()
                .map(|(entity, (gm, global, material, slots))| {
                    (
                        entity,
                        gm.clone(),
                        global.matrix(),
                        material.cloned().unwrap_or_default(),
                        slots.cloned().unwrap_or_default(),
                    )
                })
                .collect();

            for (entity, gpu_mesh, matrix, material, slots) in entries {
                self.batch_submeshes(
                    device,
                    queue,
                    &mut batches,
                    &gpu_mesh,
                    matrix,
                    &material,
                    &slots,
                    Self::outline_group_id(world, entity),
                );
            }
        }

        // -- Particle emitters: one instance per live particle (ADR 0044) --
        {
            use crate::particles::ParticleEmitter;
            type EmitterQuery<'w> = Query<'w, (&'w ParticleEmitter, Option<&'w Material>)>;
            let q = EmitterQuery::new(world);
            let entries: Vec<_> = q
                .iter()
                .map(|(entity, (emitter, material))| {
                    // Unmaterialed particles keep the historical alpha-blend
                    // behavior; an explicit material may opt into mask/opaque.
                    let material = material.cloned().unwrap_or_else(|| Material {
                        alpha_mode: AlphaMode::Blend,
                        cull_mode: CullMode::None,
                        ..Material::default()
                    });
                    let tint = material.color;
                    let instances: Vec<InstanceData> = emitter
                        .particles
                        .iter()
                        .map(|particle| {
                            let factor = particle.life_factor();
                            let color = emitter.color_at(factor);
                            let color = [
                                color[0] * tint[0],
                                color[1] * tint[1],
                                color[2] * tint[2],
                                color[3] * tint[3],
                            ];
                            let matrix = glam::Mat4::from_scale_rotation_translation(
                                glam::Vec3::splat(emitter.size_at(factor)),
                                glam::Quat::IDENTITY,
                                particle.position,
                            );
                            instance_from_material(matrix, color, &material)
                        })
                        .collect();
                    (entity, emitter.mesh.id(), instances, material)
                })
                .collect();

            for (entity, mesh_id, instances, material) in entries {
                if instances.is_empty() {
                    continue;
                }
                let Some(gpu_mesh) = world
                    .get_resource::<GpuMeshCache>()
                    .and_then(|cache| cache.get(mesh_id).cloned())
                else {
                    continue;
                };
                let texture_bind_group = self.resolve_material_bind_group(device, queue, &material);
                let mesh_key = Arc::as_ptr(&gpu_mesh.vertex_buffer) as usize;
                let tex_key = Arc::as_ptr(&texture_bind_group) as usize;
                let pipeline_key = MaterialPipelineKey::from_material(&material);
                // A particle mesh is a single-surface quad, so slot 0 is the
                // only submesh it can have.
                let batch = batches
                    .entry((mesh_key, 0, tex_key, pipeline_key))
                    .or_insert_with(|| StaticBatch {
                        pipeline_key,
                        gpu_mesh: gpu_mesh.clone(),
                        submesh: 0,
                        texture_bind_group: Arc::clone(&texture_bind_group),
                        instances: Vec::new(),
                        outline_instances: Vec::new(),
                    });
                let outline_group = Self::outline_group_id(world, entity);
                let outline_material = outline_material_identity(
                    &texture_bind_group,
                    material.outline.internal_boundary_strength,
                );
                batch.outline_instances.extend(
                    instances
                        .iter()
                        .copied()
                        .map(|instance| {
                            OutlineInstanceData::from_instance(
                                instance,
                                outline_group,
                                outline_material,
                            )
                        }),
                );
                batch.instances.extend(instances);
            }
        }

        let mut batches: Vec<_> = batches.into_values().collect();
        for batch in &mut batches {
            if batch.pipeline_key.alpha_mode == AlphaMode::Blend {
                batch.instances.sort_by(|left, right| {
                    instance_distance_squared(right, camera_position)
                        .partial_cmp(&instance_distance_squared(left, camera_position))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
        // Opaque/masked geometry must populate depth before blended geometry.
        // Transparent batches are then drawn back-to-front relative to the
        // active camera; ties retain deterministic material-key ordering.
        batches.sort_by(|left, right| {
            match (
                left.pipeline_key.alpha_mode == AlphaMode::Blend,
                right.pipeline_key.alpha_mode == AlphaMode::Blend,
            ) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                (false, false) => left.pipeline_key.cmp(&right.pipeline_key),
                (true, true) => batch_distance_squared(right, camera_position)
                    .partial_cmp(&batch_distance_squared(left, camera_position))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.pipeline_key.cmp(&right.pipeline_key)),
            }
        });
        batches
    }

    /// Resolves all material texture slots and caches their combined bind group.
    fn resolve_material_bind_group(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        material: &Material,
    ) -> Arc<wgpu::BindGroup> {
        let base_fallback = Arc::clone(&self.render.white_texture);
        let normal_fallback = Arc::clone(&self.render.flat_normal_texture);
        // A missing emissive map behaves as white so the authored emissive
        // factor still works on its own; the default factor is black.
        let emissive_fallback = Arc::clone(&self.render.white_texture);
        let base = self.resolve_texture_slot(
            device,
            queue,
            material.texture.as_ref(),
            material.pending_texture.as_ref(),
            base_fallback,
            TextureSampleEncoding::SrgbColor,
        );
        let normal = self.resolve_texture_slot(
            device,
            queue,
            material.normal_texture.as_ref(),
            material.pending_normal_texture.as_ref(),
            normal_fallback,
            TextureSampleEncoding::LinearData,
        );
        let metallic_roughness = self.resolve_texture_slot(
            device,
            queue,
            material.metallic_roughness_texture.as_ref(),
            material.pending_metallic_roughness_texture.as_ref(),
            Arc::clone(&self.render.white_texture),
            TextureSampleEncoding::LinearData,
        );
        let occlusion = self.resolve_texture_slot(
            device,
            queue,
            material.occlusion_texture.as_ref(),
            material.pending_occlusion_texture.as_ref(),
            Arc::clone(&self.render.white_texture),
            TextureSampleEncoding::LinearData,
        );
        let emissive = self.resolve_texture_slot(
            device,
            queue,
            material.emissive_texture.as_ref(),
            material.pending_emissive_texture.as_ref(),
            emissive_fallback,
            TextureSampleEncoding::SrgbColor,
        );
        let ramp = self.resolve_texture_slot(
            device,
            queue,
            material.toon.ramp_texture.as_ref(),
            material.toon.pending_ramp_texture.as_ref(),
            Arc::clone(&self.render.white_texture),
            TextureSampleEncoding::SrgbColor,
        );
        let sphere = self.resolve_texture_slot(
            device,
            queue,
            material.toon.sphere_texture.as_ref(),
            material.toon.pending_sphere_texture.as_ref(),
            Arc::clone(&self.render.white_texture),
            TextureSampleEncoding::SrgbColor,
        );
        let uniform = MaterialUniformData::from_material(material);
        let key = MaterialBindGroupKey {
            base: Arc::as_ptr(&base) as usize,
            normal: Arc::as_ptr(&normal) as usize,
            metallic_roughness: Arc::as_ptr(&metallic_roughness) as usize,
            occlusion: Arc::as_ptr(&occlusion) as usize,
            emissive: Arc::as_ptr(&emissive) as usize,
            ramp: Arc::as_ptr(&ramp) as usize,
            sphere: Arc::as_ptr(&sphere) as usize,
            uniform: uniform.key(),
        };
        self.material_bind_group_cache
            .entry(key)
            .or_insert_with(|| {
                let uniform_buffer = RenderState::make_uniform_buffer(
                    device,
                    bytemuck::bytes_of(&uniform),
                    "Material uniform",
                );
                let bind_group = Arc::new(RenderState::make_material_bind_group(
                    device,
                    &self.render.texture_bind_group_layout,
                    MaterialBindResources {
                        base: &base,
                        normal: &normal,
                        metallic_roughness: &metallic_roughness,
                        occlusion: &occlusion,
                        emissive: &emissive,
                        ramp: &ramp,
                        sphere: &sphere,
                        uniform_buffer: &uniform_buffer,
                    },
                ));
                CachedMaterialBindGroup {
                    base: Arc::downgrade(&base),
                    normal: Arc::downgrade(&normal),
                    metallic_roughness: Arc::downgrade(&metallic_roughness),
                    occlusion: Arc::downgrade(&occlusion),
                    emissive: Arc::downgrade(&emissive),
                    ramp: Arc::downgrade(&ramp),
                    sphere: Arc::downgrade(&sphere),
                    _uniform_buffer: uniform_buffer,
                    bind_group,
                }
            });
        Arc::clone(&self.material_bind_group_cache[&key].bind_group)
    }

    fn resolve_texture_slot(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: Option<&Arc<Texture>>,
        pending: Option<&Arc<DecodedTexture>>,
        fallback: Arc<Texture>,
        encoding: TextureSampleEncoding,
    ) -> Arc<Texture> {
        if let Some(texture) = texture {
            return Arc::clone(texture);
        }
        let Some(pending) = pending else {
            return fallback;
        };
        let cache = match encoding {
            TextureSampleEncoding::SrgbColor => &mut self.decoded_srgb_cache,
            TextureSampleEncoding::LinearData => &mut self.decoded_linear_cache,
        };
        let key = Arc::as_ptr(pending) as usize;
        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
            let uploaded = match encoding {
                TextureSampleEncoding::SrgbColor => Texture::from_decoded(device, queue, pending),
                TextureSampleEncoding::LinearData => {
                    Texture::from_decoded_linear(device, queue, pending)
                }
            };
            let Ok(texture) = uploaded else {
                return fallback;
            };
            entry.insert(CachedDecodedTexture {
                source: Arc::downgrade(pending),
                texture: Arc::new(texture),
            });
        }
        Arc::clone(&cache[&key].texture)
    }

    /// Collects one draw per skinned entity (identified by [`JointPalette`]).
    ///
    /// Skinned entities are excluded from the instancing batcher (ADR 0043),
    /// so each entry becomes an individual draw with its own joint palette.
    fn collect_skinned_draws(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_position: glam::Vec3,
    ) -> Vec<SkinnedDraw> {
        use crate::asset::Handle;
        use engine_ecs::{Query, Without};

        let mut draws = Vec::new();

        // -- Handle<Mesh> entities: resolve through GpuMeshCache --
        {
            type SkinnedHandleQuery<'w> = Query<
                'w,
                (
                    &'w Handle<Mesh>,
                    &'w GlobalTransform,
                    &'w JointPalette,
                    Option<&'w Material>,
                    Option<&'w MaterialSlots>,
                ),
            >;
            let q = SkinnedHandleQuery::new(world);
            let entries: Vec<_> = q
                .iter()
                .map(|(entity, (handle, global, palette, material, slots))| {
                    (
                        entity,
                        handle.id(),
                        global.matrix(),
                        padded_palette(palette),
                        material.cloned().unwrap_or_default(),
                        slots.cloned().unwrap_or_default(),
                    )
                })
                .collect();

            for (entity, mesh_id, matrix, palette, material, slots) in entries {
                let Some(gpu_mesh) = world
                    .get_resource::<GpuMeshCache>()
                    .and_then(|cache| cache.get(mesh_id).cloned())
                else {
                    continue;
                };
                self.push_skinned_submeshes(
                    device,
                    queue,
                    &mut draws,
                    gpu_mesh,
                    matrix,
                    &palette,
                    &material,
                    &slots,
                    Self::outline_group_id(world, entity),
                );
            }
        }

        // -- Direct GpuMesh entities (no Handle<Mesh>) --
        {
            type SkinnedDirectQuery<'w> = Query<
                'w,
                (
                    &'w GpuMesh,
                    &'w GlobalTransform,
                    &'w JointPalette,
                    Option<&'w Material>,
                    Option<&'w MaterialSlots>,
                ),
                Without<Handle<Mesh>>,
            >;
            let q = SkinnedDirectQuery::new(world);
            let entries: Vec<_> = q
                .iter()
                .map(|(entity, (gpu_mesh, global, palette, material, slots))| {
                    (
                        entity,
                        gpu_mesh.clone(),
                        global.matrix(),
                        padded_palette(palette),
                        material.cloned().unwrap_or_default(),
                        slots.cloned().unwrap_or_default(),
                    )
                })
                .collect();

            for (entity, gpu_mesh, matrix, palette, material, slots) in entries {
                self.push_skinned_submeshes(
                    device,
                    queue,
                    &mut draws,
                    gpu_mesh,
                    matrix,
                    &palette,
                    &material,
                    &slots,
                    Self::outline_group_id(world, entity),
                );
            }
        }

        draws.sort_by(|left, right| {
            match (
                left.pipeline_key.alpha_mode == AlphaMode::Blend,
                right.pipeline_key.alpha_mode == AlphaMode::Blend,
            ) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                (false, false) => left.pipeline_key.cmp(&right.pipeline_key),
                (true, true) => instance_distance_squared(&right.instance, camera_position)
                    .partial_cmp(&instance_distance_squared(&left.instance, camera_position))
                    .unwrap_or(std::cmp::Ordering::Equal),
            }
        });
        draws
    }
}

/// One skinned entity's draw data for the current frame.
struct SkinnedDraw {
    gpu_mesh: GpuMesh,
    /// Submesh drawn by this record (ADR 0076).
    submesh: usize,
    texture_bind_group: Arc<wgpu::BindGroup>,
    instance: InstanceData,
    /// Mask payload uses the rig-root hierarchy ID for every render part.
    outline_instance: OutlineInstanceData,
    pipeline_key: MaterialPipelineKey,
    /// Column-major palette matrices padded to [`MAX_JOINTS`] entries.
    palette: Vec<[f32; 16]>,
}

/// GPU buffers and palette bind group retained for one skinned frame draw.
struct SkinnedFrameResources {
    instance_buffer: wgpu::Buffer,
    /// Allocated only while at least one material requests an outline.
    outline_instance_buffer: Option<wgpu::Buffer>,
    palette_bind_group: wgpu::BindGroup,
}

fn instance_from_material(
    matrix: glam::Mat4,
    color: [f32; 4],
    material: &Material,
) -> InstanceData {
    InstanceData::from_transform_material(
        matrix,
        color,
        [
            material.emissive_color[0],
            material.emissive_color[1],
            material.emissive_color[2],
            match material.shading_model {
                ShadingModel::StandardLit => 0.0,
                ShadingModel::ToonLit => 1.0,
                ShadingModel::Unlit => 2.0,
            },
        ],
        [
            material.roughness,
            material.metallic,
            material.alpha_cutoff,
            material.alpha_mode.shader_value(),
        ],
    )
}

fn instance_distance_squared(instance: &InstanceData, camera_position: glam::Vec3) -> f32 {
    let translation = glam::Vec3::new(
        instance.model[3][0],
        instance.model[3][1],
        instance.model[3][2],
    );
    translation.distance_squared(camera_position)
}

fn batch_distance_squared(batch: &StaticBatch, camera_position: glam::Vec3) -> f32 {
    batch
        .instances
        .first()
        .map(|instance| instance_distance_squared(instance, camera_position))
        .unwrap_or(0.0)
}

/// Pads `palette` with identity matrices to the fixed uniform array size.
fn padded_palette(palette: &JointPalette) -> Vec<[f32; 16]> {
    let mut matrices: Vec<[f32; 16]> = palette
        .matrices
        .iter()
        .take(MAX_JOINTS)
        .map(|matrix| matrix.to_cols_array())
        .collect();
    matrices.resize(MAX_JOINTS, glam::Mat4::IDENTITY.to_cols_array());
    matrices
}

impl RenderState {
    pub(crate) async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderStateError> {
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let camera_data = CameraUniform::identity();
        let camera_buffer =
            Self::make_uniform_buffer(device, bytemuck::bytes_of(&camera_data), "Camera");

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Camera BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera BG"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Texture BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 7,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 8,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                ],
            });

        let white_tex = Arc::new(Texture::white(device, queue));
        let flat_normal_tex = Arc::new(
            Texture::solid_rgba(
                device,
                queue,
                [128, 128, 255, 255],
                "flat_normal_texture",
                false,
            )
            .expect("one-pixel flat normal must fit every valid GPU device"),
        );
        let environment = EnvironmentGpuState::new(device, queue);

        let default_light =
            LightUniform::from_resources(&AmbientLight::default(), &DirectionalLight::default());
        let light_buffer =
            Self::make_uniform_buffer(device, bytemuck::bytes_of(&default_light), "Light");

        // Shadow resources (Phase 50, ADR 0036): the map size is fixed at
        // startup from the default settings; runtime resolution changes are
        // out of scope for the first shadow pass.
        let shadow_descriptor = ShadowSettings::default().map_descriptor();
        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow map"),
            size: shadow_descriptor.extent(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: shadow_descriptor.format.to_wgpu(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_layer_views: Vec<wgpu::TextureView> = (0..SHADOW_CASCADE_COUNT as u32)
            .map(|layer| {
                shadow_texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Shadow cascade layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let shadow_sample_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Shadow array view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Shadow comparison sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let shadow_uniform_buffer = Self::make_uniform_buffer(
            device,
            bytemuck::bytes_of(&ShadowUniform::disabled()),
            "Shadow uniform",
        );

        let light_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Light BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        sample_type: wgpu::TextureSampleType::Depth,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });

        let light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Light BG"),
            layout: &light_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: light_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: shadow_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&shadow_sample_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
            ],
        });

        // The static module owns the material fragment stage. Skinned material
        // pipelines pair their deformation vertex stage with this same
        // `fs_main`, so future surface/lighting work has one compiled source.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mesh pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_bgl),
                Some(&texture_bind_group_layout),
                Some(&light_bgl),
                None,
                Some(environment.bind_group_layout()),
            ],
            immediate_size: 0,
        });

        let pipelines = std::array::from_fn(|alpha_index| {
            std::array::from_fn(|cull_index| {
                let key = material_pipeline_key(alpha_index, cull_index);
                create_material_pipeline(
                    device,
                    "Mesh material pipeline",
                    &pipeline_layout,
                    MaterialShaderStages {
                        vertex: &shader,
                        fragment: &shader,
                    },
                    format,
                    &[
                        Vertex::LAYOUT,
                        InstanceData::LAYOUT,
                        TangentVertexData::LAYOUT,
                    ],
                    key,
                )
            })
        });

        let joint_palette_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Joint palette BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Skinned mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mesh_skinned.wgsl").into()),
        });

        let skinned_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Skinned mesh pipeline layout"),
                bind_group_layouts: &[
                    Some(&camera_bgl),
                    Some(&texture_bind_group_layout),
                    Some(&light_bgl),
                    Some(&joint_palette_bgl),
                    Some(environment.bind_group_layout()),
                ],
                immediate_size: 0,
            });

        let skinned_pipelines = std::array::from_fn(|alpha_index| {
            std::array::from_fn(|cull_index| {
                let key = material_pipeline_key(alpha_index, cull_index);
                create_material_pipeline(
                    device,
                    "Skinned material pipeline",
                    &skinned_pipeline_layout,
                    MaterialShaderStages {
                        vertex: &skinned_shader,
                        fragment: &shader,
                    },
                    format,
                    &[
                        Vertex::LAYOUT,
                        InstanceData::LAYOUT,
                        crate::mesh::SkinningVertexData::LAYOUT,
                        TangentVertexData::LAYOUT,
                    ],
                    key,
                )
            })
        });

        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Outline mask shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/outline.wgsl").into()),
        });
        let outline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Outline mask pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&texture_bind_group_layout)],
            immediate_size: 0,
        });
        let outline_skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Skinned outline mask shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/outline_skinned.wgsl").into()),
        });
        let outline_skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Skinned outline mask pipeline layout"),
            bind_group_layouts: &[
                Some(&camera_bgl),
                Some(&texture_bind_group_layout),
                Some(&joint_palette_bgl),
            ],
            immediate_size: 0,
        });
        let cull_modes = [Some(wgpu::Face::Back), Some(wgpu::Face::Front), None];
        let make_outline_mask_pipeline = |
            label: &str,
            shader: &wgpu::ShaderModule,
            layout: &wgpu::PipelineLayout,
            buffers: &[wgpu::VertexBufferLayout<'_>],
            cull_mode: Option<wgpu::Face>,
        | {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[
                        Some(wgpu::ColorTargetState {
                            format: OUTLINE_STYLE_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                        Some(wgpu::ColorTargetState {
                            format: OUTLINE_GROUP_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                    ],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let outline_mask_pipelines = std::array::from_fn(|cull_index| {
            make_outline_mask_pipeline(
                "Outline mask pipeline",
                &outline_shader,
                &outline_layout,
                &[Vertex::LAYOUT, OutlineInstanceData::LAYOUT],
                cull_modes[cull_index],
            )
        });
        let outline_mask_skinned_pipelines = std::array::from_fn(|cull_index| {
            make_outline_mask_pipeline(
                "Skinned outline mask pipeline",
                &outline_skinned_shader,
                &outline_skinned_layout,
                &[
                    Vertex::LAYOUT,
                    OutlineInstanceData::LAYOUT,
                    crate::mesh::SkinningVertexData::LAYOUT,
                ],
                cull_modes[cull_index],
            )
        });

        let outline_composite_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Outline composite BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Uint,
                        },
                        count: None,
                    },
                ],
            });
        let outline_composite_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Outline composite shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/outline_composite.wgsl").into(),
                ),
            });
        let outline_composite_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Outline composite pipeline layout"),
                bind_group_layouts: &[Some(&outline_composite_bgl)],
                immediate_size: 0,
            });
        let outline_composite_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Outline composite pipeline"),
                layout: Some(&outline_composite_layout),
                vertex: wgpu::VertexState {
                    module: &outline_composite_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &outline_composite_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let shadow_camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Shadow camera BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mut shadow_cascade_buffers = Vec::with_capacity(SHADOW_CASCADE_COUNT);
        let mut shadow_cascade_bind_groups = Vec::with_capacity(SHADOW_CASCADE_COUNT);
        for _ in 0..SHADOW_CASCADE_COUNT {
            let buffer = Self::make_uniform_buffer(
                device,
                bytemuck::bytes_of(&CameraUniform::identity()),
                "Shadow cascade camera",
            );
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Shadow cascade BG"),
                layout: &shadow_camera_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            shadow_cascade_buffers.push(buffer);
            shadow_cascade_bind_groups.push(bind_group);
        }

        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shadow depth shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shadow_depth.wgsl").into()),
        });
        let shadow_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Shadow pipeline layout"),
                bind_group_layouts: &[Some(&shadow_camera_bgl)],
                immediate_size: 0,
            });
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Shadow depth pipeline"),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::LAYOUT, InstanceData::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: shadow_descriptor.format.to_wgpu(),
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                // Hardware slope-scaled bias combats acne; the shader-side
                // depth bias from ShadowSettings stacks on top.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let shadow_skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shadow skinned depth shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/shadow_depth_skinned.wgsl").into(),
            ),
        });
        let shadow_skinned_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Shadow skinned pipeline layout"),
                bind_group_layouts: &[Some(&shadow_camera_bgl), Some(&joint_palette_bgl)],
                immediate_size: 0,
            });
        let shadow_skinned_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Shadow skinned depth pipeline"),
                layout: Some(&shadow_skinned_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shadow_skinned_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[
                        Vertex::LAYOUT,
                        InstanceData::LAYOUT,
                        crate::mesh::SkinningVertexData::LAYOUT,
                    ],
                    compilation_options: Default::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: shadow_descriptor.format.to_wgpu(),
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState {
                        constant: 2,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sky shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/sky.wgsl").into()),
        });
        let sky_buffer = Self::make_uniform_buffer(
            device,
            bytemuck::bytes_of(&SkyUniform::from_settings(
                glam::Mat4::IDENTITY,
                &SkySettings::default(),
            )),
            "Sky uniform",
        );
        let sky_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Sky BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let sky_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Sky BG"),
            layout: &sky_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: sky_buffer.as_entire_binding(),
            }],
        });
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Sky pipeline layout"),
            bind_group_layouts: &[Some(&sky_bgl), Some(environment.bind_group_layout())],
            immediate_size: 0,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sky pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            // The sky never writes depth and always passes, so scene
            // geometry drawn afterwards covers it wherever depth testing
            // succeeds.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: main_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        let state = Self {
            camera_buffer,
            camera_bind_group,
            camera_bgl,
            texture_bind_group_layout,
            white_texture: white_tex,
            flat_normal_texture: flat_normal_tex,
            environment,
            light_buffer,
            light_bind_group,
            pipelines,
            skinned_pipelines,
            outline_mask_pipelines,
            outline_mask_skinned_pipelines,
            outline_composite_bgl,
            outline_composite_pipeline,
            joint_palette_bgl,
            shadow_pipeline,
            shadow_skinned_pipeline,
            shadow_uniform_buffer,
            shadow_cascade_buffers,
            shadow_cascade_bind_groups,
            shadow_layer_views,
            sky_buffer,
            sky_bind_group,
            sky_pipeline,
        };
        if let Some(error) = error_scope.pop().await {
            return Err(RenderStateError(error));
        }
        Ok(state)
    }

    fn make_uniform_buffer(device: &wgpu::Device, data: &[u8], label: &str) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        })
    }

    fn make_material_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        resources: MaterialBindResources<'_>,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Texture BG"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resources.base.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&resources.normal.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&resources.emissive.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&resources.ramp.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&resources.sphere.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&resources.base.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: resources.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&resources.metallic_roughness.view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&resources.occlusion.view),
                },
            ],
        })
    }

    pub(crate) fn update_camera(
        &self,
        queue: &wgpu::Queue,
        vp: glam::Mat4,
        view: glam::Mat4,
        world_position: glam::Vec3,
        viewport_aspect: f32,
    ) {
        let uniform =
            CameraUniform::from_matrices_position(vp, view, world_position, viewport_aspect);
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    pub(crate) fn update_light(
        &self,
        queue: &wgpu::Queue,
        ambient: &AmbientLight,
        directional: &DirectionalLight,
    ) {
        let uniform = LightUniform::from_resources(ambient, directional);
        queue.write_buffer(&self.light_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    pub(crate) fn update_environment(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: &EnvironmentLighting,
        skybox: Option<&Arc<Texture>>,
        diffuse_irradiance: Option<&Arc<Texture>>,
    ) {
        self.environment
            .update(device, queue, settings, skybox, diffuse_irradiance);
    }

    pub(crate) fn update_sky(&self, queue: &wgpu::Queue, vp: glam::Mat4, sky: &SkySettings) {
        let uniform = SkyUniform::from_settings(vp, sky);
        queue.write_buffer(&self.sky_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    pub(crate) fn update_shadows(
        &self,
        queue: &wgpu::Queue,
        matrices: &[glam::Mat4; SHADOW_CASCADE_COUNT],
        settings: &ShadowSettings,
        enabled: bool,
    ) {
        let mut light_view_proj = [glam::Mat4::IDENTITY.to_cols_array_2d(); SHADOW_CASCADE_COUNT];
        for (slot, matrix) in light_view_proj.iter_mut().zip(matrices) {
            *slot = matrix.to_cols_array_2d();
        }
        let uniform = ShadowUniform {
            light_view_proj,
            params: [
                settings.depth_bias,
                settings.normal_bias,
                if enabled { 1.0 } else { 0.0 },
                1.0 / settings.resolution.max(1) as f32,
            ],
        };
        queue.write_buffer(&self.shadow_uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        for (buffer, matrix) in self.shadow_cascade_buffers.iter().zip(matrices) {
            let cascade_uniform = CameraUniform::from_matrices_position(
                *matrix,
                glam::Mat4::IDENTITY,
                glam::Vec3::ZERO,
                1.0,
            );
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&cascade_uniform));
        }
    }

    /// Creates a transient VERTEX buffer holding `data` (raw `InstanceData` bytes).
    pub(crate) fn make_instance_buffer(&self, device: &wgpu::Device, data: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Instance buffer"),
            contents: data,
            usage: wgpu::BufferUsages::VERTEX,
        })
    }
}

/// Writes each morphing entity's changed vertices into its own GPU buffer
/// (ADR 0097 §5).
///
/// Runs after [`upload_pending_meshes`], so a newly spawned morphing entity
/// already has the private [`GpuMesh`] this writes into, and consumes the
/// dirty list `crate::morph::morph_blend_system` produced this frame. An
/// entity holding a pose reports nothing dirty and costs no GPU traffic at
/// all.
///
/// Changed vertices are written as **contiguous runs** rather than one write
/// per vertex: a morph's vertex set is authored as a region of the mesh
/// (an eyelid, a mouth), so the indices are naturally clustered and a run
/// walk turns thousands of tiny writes into a handful of larger ones.
pub(crate) fn upload_morphed_vertices(world: &mut engine_ecs::World, queue: &wgpu::Queue) {
    use crate::mesh::Vertex;
    use crate::morph::MorphDirtyVertices;
    use engine_ecs::Query;

    let stride = std::mem::size_of::<Vertex>() as u64;
    let mut query = Query::<(&Mesh, &GpuMesh, &mut MorphDirtyVertices)>::new(world);
    for (_, (mesh, gpu_mesh, dirty)) in query.iter_mut() {
        if dirty.changed.is_empty() {
            continue;
        }
        let mut run_start = 0usize;
        while run_start < dirty.changed.len() {
            let mut run_end = run_start + 1;
            while run_end < dirty.changed.len()
                && dirty.changed[run_end] == dirty.changed[run_end - 1] + 1
            {
                run_end += 1;
            }
            let first = dirty.changed[run_start] as usize;
            let last = dirty.changed[run_end - 1] as usize;
            if let Some(slice) = mesh.vertices.get(first..=last) {
                queue.write_buffer(
                    &gpu_mesh.vertex_buffer,
                    first as u64 * stride,
                    bytemuck::cast_slice(slice),
                );
            }
            run_start = run_end;
        }
        // Consumed: the GPU now matches the CPU mesh, so a frame that does
        // not blend again writes nothing.
        dirty.changed.clear();
    }
}

pub(crate) fn upload_pending_meshes(
    world: &mut engine_ecs::World,
    device: &wgpu::Device,
) -> Result<(), RenderPreparationError> {
    use crate::asset::{Assets, Handle, RuntimeAssetId};
    use engine_ecs::Query;
    use hashbrown::HashMap;

    // Direct Mesh components → GpuMesh per entity (used by Without<Handle<Mesh>> batch path).
    {
        let mut to_upload = HashMap::<engine_ecs::Entity, GpuMesh>::new();
        {
            let query = Query::<(&Mesh, Option<&GpuMesh>)>::new(world);
            for (entity, (mesh, gpu_mesh)) in &query {
                if gpu_mesh.is_none() {
                    let gm = GpuMesh::upload(device, mesh)
                        .map_err(|source| RenderPreparationError::Mesh { entity, source })?;
                    to_upload.insert(entity, gm);
                }
            }
        }
        for (entity, gpu_mesh) in to_upload {
            world.add_component(entity, gpu_mesh)?;
        }
    }

    // Handle<Mesh> → GpuMeshCache (shared across all entities referencing the same asset).
    {
        // Collect (representative_entity, handle_id) pairs not yet in cache.
        let pending: Vec<(engine_ecs::Entity, RuntimeAssetId)> = {
            let cached_ids: Vec<RuntimeAssetId> = world
                .get_resource::<GpuMeshCache>()
                .map(|cache| cache.runtime_ids().collect())
                .unwrap_or_default();
            let mut seen = hashbrown::HashSet::<RuntimeAssetId>::new();
            let mut pending = Vec::new();
            {
                let query = Query::<&Handle<Mesh>>::new(world);
                for (entity, handle) in query.iter() {
                    let id = handle.id();
                    if !cached_ids.contains(&id) && seen.insert(id) {
                        pending.push((entity, id));
                    }
                }
            }
            {
                // Particle emitters reference their mesh as a field, not a
                // component (ADR 0044), so they need their own sweep.
                let query = Query::<&crate::particles::ParticleEmitter>::new(world);
                for (entity, emitter) in query.iter() {
                    let id = emitter.mesh.id();
                    if !cached_ids.contains(&id) && seen.insert(id) {
                        pending.push((entity, id));
                    }
                }
            }
            pending
        };

        // Upload while holding Assets borrow, collect results, then insert into cache.
        let mut uploaded: Vec<(RuntimeAssetId, GpuMesh)> = Vec::new();
        let mut upload_error: Option<RenderPreparationError> = None;

        let shared = world
            .get_resource::<GpuMeshCache>()
            .map(GpuMeshCache::shared)
            .unwrap_or_default();
        if let Some(assets) = world.get_resource::<Assets<Mesh>>() {
            for (entity, id) in &pending {
                if let Some(handle) = assets.handle(*id)
                    && let Some(mesh) = assets.get(&handle) {
                        if let Some(gpu_mesh) = shared.get(mesh) {
                            uploaded.push((*id, gpu_mesh));
                            continue;
                        }
                        match GpuMesh::upload(device, mesh) {
                            Ok(gm) => {
                                shared.insert(mesh, gm.clone());
                                uploaded.push((*id, gm));
                            }
                            Err(source) => {
                                upload_error = Some(RenderPreparationError::Mesh {
                                    entity: *entity,
                                    source,
                                });
                                break;
                            }
                        }
                    }
            }
        }

        if let Some(err) = upload_error {
            return Err(err);
        }
        if let Some(cache) = world.get_resource_mut::<GpuMeshCache>() {
            for (id, gm) in uploaded {
                cache.insert(id, gm);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// ToneMapPass — HDR to swapchain fullscreen pass (Phase 45)
// ---------------------------------------------------------------------------

/// Raw uniform data mirroring the WGSL `PostProcessUniforms` struct.
///
/// Must be `repr(C)`, 16-byte aligned, and 64 bytes total so the Rust/WGSL
/// layouts remain identical.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct PostProcessUniformData {
    pub exposure: f32,
    pub tone_map_op: u32,
    pub bloom_enabled: u32,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    pub grading_enabled: u32,
    pub grading_saturation: f32,
    pub grading_contrast: f32,
    pub grading_gamma: f32,
    pub grading_tint_r: f32,
    pub grading_tint_g: f32,
    pub grading_tint_b: f32,
    pub output_srgb_encode: u32,
    pub _pad1: f32,
    pub _pad2: f32,
}

impl PostProcessUniformData {
    pub(crate) fn from_settings(s: &PostProcessSettings, output_srgb_encode: bool) -> Self {
        Self {
            exposure: if s.enabled { s.exposure } else { 1.0 },
            tone_map_op: if s.enabled && s.tone_map == ToneMapOperator::Reinhard {
                1
            } else {
                0
            },
            bloom_enabled: u32::from(s.enabled && s.bloom.enabled),
            bloom_threshold: s.bloom.threshold,
            bloom_intensity: s.bloom.intensity,
            bloom_radius: s.bloom.radius,
            grading_enabled: u32::from(s.enabled && s.color_grading.enabled),
            grading_saturation: s.color_grading.saturation,
            grading_contrast: s.color_grading.contrast,
            grading_gamma: s.color_grading.gamma,
            grading_tint_r: s.color_grading.tint[0],
            grading_tint_g: s.color_grading.tint[1],
            grading_tint_b: s.color_grading.tint[2],
            output_srgb_encode: u32::from(output_srgb_encode),
            _pad1: 0.0,
            _pad2: 0.0,
        }
    }
}

fn shader_encodes_srgb_output(swapchain_format: wgpu::TextureFormat) -> bool {
    !swapchain_format.is_srgb()
}

/// Fullscreen post-processing pass: applies tone mapping and optional bloom.
///
/// Reads an `Rgba16Float` HDR texture and outputs to the swapchain surface
/// format (Phase 45, ADR 0040).
pub(crate) struct ToneMapPass {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    output_srgb_encode: bool,
}

impl ToneMapPass {
    pub(crate) async fn new(
        device: &wgpu::Device,
        swapchain_format: wgpu::TextureFormat,
        hdr_view: &wgpu::TextureView,
    ) -> Result<Self, RenderStateError> {
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Tonemap sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let output_srgb_encode = shader_encodes_srgb_output(swapchain_format);
        let uniform_data = PostProcessUniformData::from_settings(
            &PostProcessSettings::default(),
            output_srgb_encode,
        );
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PostProcess uniform"),
            size: std::mem::size_of::<PostProcessUniformData>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        uniform_buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(bytemuck::bytes_of(&uniform_data));
        uniform_buffer.unmap();

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Tonemap BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let bind_group = Self::make_bind_group(device, &bgl, hdr_view, &sampler, &uniform_buffer);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Tonemap shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/tonemap.wgsl").into()),
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Tonemap pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Tonemap pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: swapchain_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        if let Some(err) = error_scope.pop().await {
            return Err(RenderStateError(err));
        }

        Ok(Self {
            pipeline,
            bgl,
            bind_group,
            uniform_buffer,
            sampler,
            output_srgb_encode,
        })
    }

    fn make_bind_group(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        hdr_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        uniform_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Tonemap BG"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        })
    }

    /// Recreates the bind group after the HDR texture is replaced on resize.
    pub(crate) fn update_bind_group(
        &mut self,
        device: &wgpu::Device,
        hdr_view: &wgpu::TextureView,
    ) {
        self.bind_group = Self::make_bind_group(
            device,
            &self.bgl,
            hdr_view,
            &self.sampler,
            &self.uniform_buffer,
        );
    }

    /// Uploads `settings` to the uniform buffer and runs the tonemap pass.
    pub(crate) fn execute(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        swapchain_view: &wgpu::TextureView,
        settings: &PostProcessSettings,
    ) {
        let uniform_data =
            PostProcessUniformData::from_settings(settings, self.output_srgb_encode);
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform_data));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Tonemap encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Tonemap pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: swapchain_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_texture_slots_distinguish_color_from_numeric_data() {
        assert_ne!(
            TextureSampleEncoding::SrgbColor,
            TextureSampleEncoding::LinearData
        );
    }

    #[test]
    fn output_transfer_is_applied_exactly_once() {
        assert!(!shader_encodes_srgb_output(
            wgpu::TextureFormat::Rgba8UnormSrgb
        ));
        assert!(!shader_encodes_srgb_output(
            wgpu::TextureFormat::Bgra8UnormSrgb
        ));
        assert!(shader_encodes_srgb_output(wgpu::TextureFormat::Rgba8Unorm));
        assert!(shader_encodes_srgb_output(wgpu::TextureFormat::Bgra8Unorm));

        let settings = PostProcessSettings::default();
        assert_eq!(
            PostProcessUniformData::from_settings(&settings, false).output_srgb_encode,
            0
        );
        assert_eq!(
            PostProcessUniformData::from_settings(&settings, true).output_srgb_encode,
            1
        );
    }

    #[test]
    fn main_pass_msaa_policy_matches_target_contract() {
        assert_eq!(MAIN_PASS_SAMPLE_COUNT, 4);
        assert_eq!(main_multisample_state().count, MAIN_PASS_SAMPLE_COUNT);
        assert_eq!(
            validate_main_pass_target_spec(MainPassTargetSpec {
                resolve_size: [1280, 720],
                resolve_samples: 1,
                depth_size: [1280, 720],
                depth_samples: MAIN_PASS_SAMPLE_COUNT,
            }),
            Ok(())
        );
    }

    #[test]
    fn main_pass_target_contract_rejects_sample_or_size_mismatches() {
        assert_eq!(
            validate_main_pass_target_spec(MainPassTargetSpec {
                resolve_size: [640, 360],
                resolve_samples: 4,
                depth_size: [640, 360],
                depth_samples: MAIN_PASS_SAMPLE_COUNT,
            }),
            Err(MainPassTargetError::ResolveMustBeSingleSample { actual: 4 })
        );
        assert_eq!(
            validate_main_pass_target_spec(MainPassTargetSpec {
                resolve_size: [640, 360],
                resolve_samples: 1,
                depth_size: [640, 360],
                depth_samples: 1,
            }),
            Err(MainPassTargetError::DepthSampleCount {
                expected: MAIN_PASS_SAMPLE_COUNT,
                actual: 1,
            })
        );
        assert_eq!(
            validate_main_pass_target_spec(MainPassTargetSpec {
                resolve_size: [640, 360],
                resolve_samples: 1,
                depth_size: [320, 180],
                depth_samples: MAIN_PASS_SAMPLE_COUNT,
            }),
            Err(MainPassTargetError::SizeMismatch {
                resolve: [640, 360],
                depth: [320, 180],
            })
        );
    }

    #[test]
    fn outline_mask_tracks_common_viewports_until_quality_budget() {
        let full_hd = outline_mask_extent_for_limit([1920, 1080], 8192);
        assert_eq!([full_hd.width, full_hd.height], [1920, 1080]);

        let quad_hd = outline_mask_extent_for_limit([2560, 1440], 8192);
        assert_eq!([quad_hd.width, quad_hd.height], [2560, 1440]);

        let ultra_hd = outline_mask_extent_for_limit([3840, 2160], 8192);
        assert_eq!([ultra_hd.width, ultra_hd.height], [2560, 1440]);
    }

    #[test]
    fn outline_mask_respects_device_dimension_limit() {
        let extent = outline_mask_extent_for_limit([3840, 2160], 1024);
        assert_eq!([extent.width, extent.height], [1024, 576]);
    }

    #[test]
    fn outline_mask_preserves_aspect_under_ultrawide_texel_budget() {
        let extent = outline_mask_extent_for_limit([5120, 1440], 8192);
        let mask_texels = u64::from(extent.width) * u64::from(extent.height);
        let viewport_aspect = 5120.0 / 1440.0;
        let mask_aspect = extent.width as f64 / extent.height as f64;

        assert!(mask_texels <= OUTLINE_MASK_MAX_TEXELS);
        assert!((mask_aspect - viewport_aspect).abs() < 0.01);
    }

    #[test]
    fn outline_mask_caps_density_to_bound_composite_radius() {
        let extent = outline_mask_extent_for_limit([512, 4096], 8192);
        assert!(extent.height <= OUTLINE_MASK_MAX_HEIGHT);
    }

    #[test]
    fn material_instance_payload_preserves_scalar_and_shading_properties() {
        let material = Material {
            emissive_color: [2.0, 0.5, 0.25],
            roughness: 0.2,
            metallic: 0.8,
            alpha_mode: AlphaMode::Mask,
            alpha_cutoff: 0.35,
            cull_mode: CullMode::Front,
            shading_model: ShadingModel::Unlit,
            ..Material::default()
        };
        let instance = instance_from_material(glam::Mat4::IDENTITY, material.color, &material);

        assert_eq!(instance.emissive_and_model, [2.0, 0.5, 0.25, 2.0]);
        assert_eq!(instance.surface, [0.2, 0.8, 0.35, 1.0]);
        assert_eq!(
            MaterialPipelineKey::from_material(&material),
            MaterialPipelineKey {
                alpha_mode: AlphaMode::Mask,
                cull_mode: CullMode::Front,
                cast_shadow: true,
                outline_enabled: false,
            }
        );
    }

    #[test]
    fn outline_groups_follow_transform_roots_and_skinned_rigs() {
        let mut world = engine_ecs::World::new();
        let root = world.spawn().unwrap();
        let child = world.spawn().unwrap();
        let rig = world.spawn().unwrap();
        let rig_child = world.spawn().unwrap();
        let skin = world.spawn().unwrap();
        let cycle_low = world.spawn().unwrap();
        let cycle_high = world.spawn().unwrap();

        world.add_component(child, Parent(root)).unwrap();
        world.add_component(rig_child, Parent(rig)).unwrap();
        world
            .add_component(
                skin,
                SkinnedMesh {
                    rig: rig_child,
                    joint_bones: Vec::new(),
                    inverse_bind_matrices: Vec::new(),
                    skin: None,
                },
            )
            .unwrap();
        world.add_component(cycle_low, Parent(cycle_high)).unwrap();
        world.add_component(cycle_high, Parent(cycle_low)).unwrap();

        assert_eq!(WorldRenderer::outline_group_id(&world, root), root.id());
        assert_eq!(WorldRenderer::outline_group_id(&world, child), root.id());
        assert_eq!(WorldRenderer::outline_group_id(&world, skin), rig.id());
        assert_eq!(
            WorldRenderer::outline_group_id(&world, cycle_high),
            cycle_low.id()
        );
    }

    #[test]
    fn invalid_camera_aspects_degrade_to_square_outline_projection() {
        assert_eq!(valid_viewport_aspect(0.0), 1.0);
        assert_eq!(valid_viewport_aspect(-2.0), 1.0);
        assert_eq!(valid_viewport_aspect(f32::NAN), 1.0);
        assert_eq!(valid_viewport_aspect(16.0 / 9.0), 16.0 / 9.0);
    }

    #[test]
    fn material_shader_pipeline_matrix_validates_when_a_gpu_adapter_is_available() {
        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };

        let mut renderer = pollster::block_on(WorldRenderer::new(
            context.device(),
            context.queue(),
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ))
        .expect("every static/skinned alpha and culling pipeline must validate");
        let pixels = || {
            Arc::new(DecodedTexture {
                label: "material_slot_test".into(),
                width: 1,
                height: 1,
                rgba8: vec![128, 128, 255, 255],
            })
        };
        let material = Material {
            pending_texture: Some(pixels()),
            pending_normal_texture: Some(pixels()),
            pending_emissive_texture: Some(pixels()),
            ..Material::default()
        };

        let _bind_group =
            renderer.resolve_material_bind_group(context.device(), context.queue(), &material);
        assert_eq!(renderer.decoded_srgb_cache.len(), 2);
        assert_eq!(renderer.decoded_linear_cache.len(), 1);
        assert_eq!(renderer.material_bind_group_cache.len(), 1);
    }

    fn reference_quad() -> Mesh {
        let vertices = vec![
            Vertex {
                position: [-0.5, -0.5, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                uv: [0.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [0.5, -0.5, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                uv: [1.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [0.5, 0.5, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                uv: [1.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [-0.5, 0.5, 0.0],
                normal: [0.0, 0.0, 1.0],
                color: [1.0; 3],
                uv: [0.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
        ];
        Mesh {
            vertices,
            indices: Some(vec![0, 1, 2, 0, 2, 3]),
            skinning: None,
            tangents: Some(vec![[1.0, 0.0, 0.0, 1.0]; 4]),
            submeshes: Vec::new(),
        }
    }

    fn readback_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let unpadded_bytes_per_row = width * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(alignment) * alignment;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Fixed camera/light reference readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Fixed camera/light reference readback encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("reference readback device poll must succeed");
        receiver
            .recv()
            .expect("reference readback callback must run")
            .expect("reference readback buffer must map");

        let mapped = slice.get_mapped_range();
        let mut rgba8 = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks(padded_bytes_per_row as usize) {
            rgba8.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        rgba8
    }

    fn rgb_sum_at(rgba8: &[u8], width: u32, x: u32, y: u32) -> u32 {
        let offset = ((y * width + x) * 4) as usize;
        u32::from(rgba8[offset])
            + u32::from(rgba8[offset + 1])
            + u32::from(rgba8[offset + 2])
    }

    fn peak_rgb_sum(
        rgba8: &[u8],
        width: u32,
        height: u32,
        start_x: u32,
        end_x: u32,
    ) -> u32 {
        let mut peak = 0;
        for y in 0..height {
            for x in start_x..end_x {
                peak = peak.max(rgb_sum_at(rgba8, width, x, y));
            }
        }
        peak
    }

    #[test]
    fn fixed_camera_light_reference_scene_preserves_standard_lit_occlusion_contrast() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };
        let device = context.device();
        let queue = context.queue();
        let mut renderer = pollster::block_on(WorldRenderer::new(device, queue, FORMAT))
            .expect("reference renderer pipelines must validate");

        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Fixed camera/light reference color"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Fixed camera/light reference depth"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MAIN_PASS_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let flat_normal = Arc::new(DecodedTexture {
            label: "reference_flat_normal".into(),
            width: 1,
            height: 1,
            rgba8: vec![128, 128, 255, 255],
        });
        let dielectric_rough = Arc::new(DecodedTexture {
            label: "reference_dielectric_rough".into(),
            width: 1,
            height: 1,
            rgba8: vec![0, 255, 0, 255],
        });
        let occlusion = |label: &str, value: u8| {
            Arc::new(DecodedTexture {
                label: label.into(),
                width: 1,
                height: 1,
                rgba8: vec![value, value, value, 255],
            })
        };
        let material = |occlusion_texture: Arc<DecodedTexture>| Material {
            color: [0.8, 0.8, 0.8, 1.0],
            pending_normal_texture: Some(Arc::clone(&flat_normal)),
            pending_metallic_roughness_texture: Some(Arc::clone(&dielectric_rough)),
            pending_occlusion_texture: Some(occlusion_texture),
            normal_scale: 0.0,
            occlusion_strength: 1.0,
            roughness: 1.0,
            metallic: 0.0,
            cull_mode: CullMode::None,
            cast_shadow: false,
            receive_shadow: false,
            ..Material::default()
        };

        let mut world = engine_ecs::World::new();
        world.insert_resource(AmbientLight {
            color: glam::Vec3::ONE,
            intensity: 0.6,
        });
        world.insert_resource(DirectionalLight {
            direction: glam::Vec3::NEG_Z,
            color: glam::Vec3::ONE,
            intensity: 1.0,
        });
        world.insert_resource(ShadowSettings {
            enabled: false,
            ..ShadowSettings::default()
        });
        world.insert_resource(SkySettings {
            enabled: false,
            ..SkySettings::default()
        });

        let unoccluded = world.spawn().expect("reference entity must spawn");
        world
            .add_component(unoccluded, reference_quad())
            .expect("reference mesh must insert");
        world
            .add_component(
                unoccluded,
                GlobalTransform(glam::Mat4::from_translation(glam::Vec3::new(
                    -0.7, 0.0, 0.0,
                ))),
            )
            .expect("reference transform must insert");
        world
            .add_component(
                unoccluded,
                material(occlusion("reference_ao_full", 255)),
            )
            .expect("reference material must insert");

        let occluded = world.spawn().expect("reference entity must spawn");
        world
            .add_component(occluded, reference_quad())
            .expect("reference mesh must insert");
        world
            .add_component(
                occluded,
                GlobalTransform(glam::Mat4::from_translation(glam::Vec3::new(
                    0.7, 0.0, 0.0,
                ))),
            )
            .expect("reference transform must insert");
        world
            .add_component(occluded, material(occlusion("reference_ao_zero", 0)))
            .expect("reference material must insert");

        let camera = Camera3D::new(60.0, 1.0, 0.1, 10.0);
        let camera_transform = crate::transform::Transform::looking_at(
            glam::Vec3::new(0.0, 0.0, 3.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        );
        renderer
            .render_to_view_with_camera(
                &mut world,
                &camera,
                &camera_transform,
                device,
                queue,
                &color_view,
                &depth_view,
            )
            .expect("fixed camera/light reference scene must render");

        let rgba8 = readback_rgba8(device, queue, &color_texture, WIDTH, HEIGHT);
        let background = rgb_sum_at(&rgba8, WIDTH, 0, 0);
        let unoccluded_peak = peak_rgb_sum(&rgba8, WIDTH, HEIGHT, 0, WIDTH / 2);
        let occluded_peak = peak_rgb_sum(&rgba8, WIDTH, HEIGHT, WIDTH / 2, WIDTH);
        assert!(
            occluded_peak > background + 100,
            "directionally lit occluded quad must remain visibly above the clear color: background={background}, occluded={occluded_peak}"
        );
        assert!(
            unoccluded_peak > occluded_peak + 120,
            "full AO must preserve substantially more ambient StandardLit energy: unoccluded={unoccluded_peak}, occluded={occluded_peak}"
        );
    }

    #[test]
    fn fixed_camera_reference_scene_receives_environment_specular_ibl() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 64;
        const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };
        let device = context.device();
        let queue = context.queue();
        let mut renderer = pollster::block_on(WorldRenderer::new(device, queue, FORMAT))
            .expect("environment reference renderer pipelines must validate");

        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Environment specular reference color"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Environment specular reference depth"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MAIN_PASS_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // The camera sees -Z at the screen center, while a +Z-facing metal
        // reflects +Z. Keep -Z black and put a compact white source around
        // +Z so this reference isolates the specular environment term.
        let sky_width = 8u32;
        let sky_height = 4u32;
        let mut sky_pixels = vec![0u8; (sky_width * sky_height * 4) as usize];
        for pixel in sky_pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        for y in 1..=2u32 {
            for x in 5..=6u32 {
                let offset = ((y * sky_width + x) * 4) as usize;
                sky_pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let skybox = Arc::new(
            Texture::from_decoded(
                device,
                queue,
                &DecodedTexture {
                    label: "environment_specular_reference".into(),
                    width: sky_width,
                    height: sky_height,
                    rgba8: sky_pixels,
                },
            )
            .expect("reference environment texture must upload"),
        );
        let mut texture_assets = crate::asset::Assets::<Arc<Texture>>::default();
        let skybox_id = texture_assets.add(skybox).id();

        let mut world = engine_ecs::World::new();
        world.insert_resource(texture_assets);
        world.insert_resource(AmbientLight {
            color: glam::Vec3::ZERO,
            intensity: 0.0,
        });
        world.insert_resource(DirectionalLight {
            direction: glam::Vec3::NEG_Z,
            color: glam::Vec3::ONE,
            intensity: 0.0,
        });
        world.insert_resource(ShadowSettings {
            enabled: false,
            ..ShadowSettings::default()
        });
        world.insert_resource(SkySettings {
            enabled: false,
            ..SkySettings::default()
        });
        world.insert_resource(EnvironmentLighting {
            skybox: Some(skybox_id),
            diffuse_irradiance: None,
            diffuse_color: glam::Vec3::ZERO,
            intensity: 2.0,
            diffuse_ibl_enabled: false,
        });

        let reflective = world.spawn().expect("reference entity must spawn");
        world
            .add_component(reflective, reference_quad())
            .expect("reference mesh must insert");
        world
            .add_component(reflective, GlobalTransform(glam::Mat4::IDENTITY))
            .expect("reference transform must insert");
        world
            .add_component(
                reflective,
                Material {
                    color: [1.0; 4],
                    roughness: 0.05,
                    metallic: 1.0,
                    cull_mode: CullMode::None,
                    cast_shadow: false,
                    receive_shadow: false,
                    ..Material::default()
                },
            )
            .expect("reference material must insert");

        let camera = Camera3D::new(60.0, 1.0, 0.1, 10.0);
        let camera_transform = crate::transform::Transform::looking_at(
            glam::Vec3::new(0.0, 0.0, 3.0),
            glam::Vec3::ZERO,
            glam::Vec3::Y,
        );
        renderer
            .render_to_view_with_camera(
                &mut world,
                &camera,
                &camera_transform,
                device,
                queue,
                &color_view,
                &depth_view,
            )
            .expect("environment reference scene must render");

        let rgba8 = readback_rgba8(device, queue, &color_texture, WIDTH, HEIGHT);
        let background = rgb_sum_at(&rgba8, WIDTH, 4, 4);
        let reflection = rgb_sum_at(&rgba8, WIDTH, WIDTH / 2, HEIGHT / 2);
        assert!(
            reflection > background + 80,
            "metallic StandardLit must receive skybox specular IBL: background={background}, reflection={reflection}"
        );
    }
}
