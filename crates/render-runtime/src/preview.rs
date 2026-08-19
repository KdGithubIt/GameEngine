//! Offscreen preview rendering for editor integrations.

use std::fmt;

use crate::camera::Camera3D;
use crate::native_2d::Camera2d;
use crate::renderer::WorldRenderer;
use crate::transform::Transform;

/// Storage format for preview color textures.
///
/// egui-wgpu expects registered native textures to hold gamma-encoded data
/// (its shader does not decode sRGB on sample), and PNG capture expects
/// sRGB-encoded bytes. Both therefore sample/copy through this non-sRGB
/// format, while rendering writes through [`PREVIEW_RENDER_FORMAT`] so that
/// linear shader output is sRGB-encoded on store.
pub const PREVIEW_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Render-attachment view format for preview color textures.
///
/// Create the texture with [`PREVIEW_COLOR_FORMAT`] plus this format in
/// `view_formats`, render through a view of this format, and sample/copy
/// through the default view. This matches what an sRGB window surface does
/// to the same shader output.
pub const PREVIEW_RENDER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Depth format used by preview render targets.
pub const PREVIEW_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Sample count required by the depth attachment passed to [`PreviewRenderer`].
///
/// Preview color views remain single-sampled because they are the resolve
/// targets registered with egui or copied for frame capture. This constant is
/// the fixed attachment contract for the engine's default 3D raster pass; it
/// is not a serialized or user-configurable quality setting.
pub const PREVIEW_MSAA_SAMPLE_COUNT: u32 = crate::renderer::MAIN_PASS_SAMPLE_COUNT;

/// Renderer that draws a runtime world into caller-owned texture views.
pub struct PreviewRenderer {
    renderer: WorldRenderer,
}

/// Per-frame preview GPU streaming telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewUploadReport {
    /// Bytes represented by unique GPU work discovered this frame.
    pub queued_bytes: u64,
    /// Bytes admitted by the frame upload budget.
    pub uploaded_bytes: u64,
    /// Unique upload work items discovered this frame.
    pub queued_uploads: u32,
    /// Upload work items admitted this frame.
    pub uploaded_uploads: u32,
    /// Upload work items deferred to a later frame.
    pub deferred_uploads: u32,
    /// Mesh or texture residency hits that required no new upload.
    pub cache_hits: u32,
}

impl PreviewUploadReport {
    /// Returns whether another frame is needed to drain deferred GPU work.
    pub const fn has_deferred_work(self) -> bool {
        self.deferred_uploads != 0
    }
}

impl PreviewRenderer {
    /// Creates an offscreen preview renderer for the requested color format.
    pub async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Result<Self, PreviewRendererError> {
        let renderer = WorldRenderer::new(device, queue, color_format)
            .await
            .map_err(|source| PreviewRendererError::RenderState {
                message: source.to_string(),
            })?;
        Ok(Self { renderer })
    }

    /// Configures project-shared decoded-texture residency and the per-frame upload budget.
    #[doc(hidden)]
    pub fn configure_streaming(
        &mut self,
        texture_cache: crate::material::SharedGpuTextureCache,
        max_bytes: u64,
        max_uploads: u32,
    ) {
        self.renderer.set_shared_texture_cache(texture_cache);
        self.renderer.set_upload_budget(max_bytes, max_uploads);
    }

    /// Returns deterministic upload telemetry from the most recent preview frame.
    pub fn upload_report(&self) -> PreviewUploadReport {
        let report = self.renderer.upload_report();
        PreviewUploadReport {
            queued_bytes: report.queued_bytes,
            uploaded_bytes: report.uploaded_bytes,
            queued_uploads: report.queued_uploads,
            uploaded_uploads: report.uploaded_uploads,
            deferred_uploads: report.deferred_uploads,
            cache_hits: report.cache_hits,
        }
    }

    /// Drops recreatable view-local bind groups/targets while retaining immutable pipelines.
    #[doc(hidden)]
    pub fn release_recreatable_resources(&mut self) {
        self.renderer.release_recreatable_resources();
    }

    /// Renders `world` into a multisampled scene target, resolving into the
    /// provided single-sample color view. `depth_view` must use
    /// [`PREVIEW_MSAA_SAMPLE_COUNT`] samples and match the color dimensions.
    pub fn render_to_view(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), PreviewRendererError> {
        self.renderer
            .render_to_view(world, device, queue, color_view, depth_view)
            .map_err(|source| PreviewRendererError::RenderFrame {
                message: source.to_string(),
            })
    }

    /// Renders `world` through a caller-owned camera without changing camera entities.
    ///
    /// This entry point is intended for editor observation surfaces that must
    /// inspect a live runtime world without inserting, removing, or selecting
    /// a camera component in that world. The caller is responsible for
    /// deriving the camera aspect ratio from its own render target. The color
    /// and depth attachment contract is the same as [`Self::render_to_view`].
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_view_with_camera(
        &mut self,
        world: &mut engine_ecs::World,
        camera: &Camera3D,
        camera_transform: &Transform,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), PreviewRendererError> {
        self.renderer
            .render_to_view_with_camera(
                world,
                camera,
                camera_transform,
                device,
                queue,
                color_view,
                depth_view,
            )
            .map_err(|source| PreviewRendererError::RenderFrame {
                message: source.to_string(),
            })
    }

    /// Renders `world` through an explicit Native 2D camera.
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_view_with_camera_2d(
        &mut self,
        world: &mut engine_ecs::World,
        camera: &Camera2d,
        camera_transform: &Transform,
        viewport: [u32; 2],
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), PreviewRendererError> {
        self.renderer
            .render_to_view_with_camera_2d(
                world,
                camera,
                camera_transform,
                viewport,
                device,
                queue,
                color_view,
                depth_view,
            )
            .map_err(|source| PreviewRendererError::RenderFrame {
                message: source.to_string(),
            })
    }
}

/// Error returned while creating or rendering an offscreen preview.
#[derive(Debug)]
pub enum PreviewRendererError {
    /// Render pipeline setup failed.
    RenderState {
        /// Human-readable source error.
        message: String,
    },
    /// A preview frame could not be rendered.
    RenderFrame {
        /// Human-readable source error.
        message: String,
    },
}

impl fmt::Display for PreviewRendererError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RenderState { message } | Self::RenderFrame { message } => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for PreviewRendererError {}
