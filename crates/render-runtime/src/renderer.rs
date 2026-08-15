//! Public facade for the private runtime rendering backend.

use std::fmt;

use crate::camera::Camera3D;
use crate::transform::Transform;

/// Fixed multisample count used by the primary 3D render pass.
pub const MAIN_PASS_SAMPLE_COUNT: u32 = crate::render_backend::MAIN_PASS_SAMPLE_COUNT;

/// Error returned while constructing runtime render pipelines.
#[derive(Debug)]
pub struct RenderStateError(crate::render_backend::RenderStateError);

impl fmt::Display for RenderStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RenderStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Error returned while preparing or rendering one runtime frame.
#[derive(Debug)]
pub struct RenderFrameError(crate::render_backend::RenderFrameError);

impl fmt::Display for RenderFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for RenderFrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Stateful renderer for ECS worlds and caller-owned render targets.
pub struct WorldRenderer {
    inner: crate::render_backend::WorldRenderer,
}

impl WorldRenderer {
    /// Creates renderer pipelines for `format`.
    pub async fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Result<Self, RenderStateError> {
        let inner = crate::render_backend::WorldRenderer::new(device, queue, format)
            .await
            .map_err(RenderStateError)?;
        Ok(Self { inner })
    }

    /// Renders the active game camera into caller-owned color and depth views.
    pub fn render_to_view(
        &mut self,
        world: &mut engine_ecs::World,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) -> Result<(), RenderFrameError> {
        self.inner
            .render_to_view(world, device, queue, color_view, depth_view)
            .map_err(RenderFrameError)
    }

    /// Renders through an explicit camera without mutating camera entities.
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
    ) -> Result<(), RenderFrameError> {
        self.inner
            .render_to_view_with_camera(
                world,
                camera,
                camera_transform,
                device,
                queue,
                color_view,
                depth_view,
            )
            .map_err(RenderFrameError)
    }
}

/// Fullscreen HDR-to-output tone-map pass used by the windowed runtime host.
pub struct ToneMapPass {
    inner: crate::render_backend::ToneMapPass,
}

impl ToneMapPass {
    /// Creates a tone-map pipeline that samples `hdr_view`.
    pub async fn new(
        device: &wgpu::Device,
        swapchain_format: wgpu::TextureFormat,
        hdr_view: &wgpu::TextureView,
    ) -> Result<Self, RenderStateError> {
        let inner = crate::render_backend::ToneMapPass::new(device, swapchain_format, hdr_view)
            .await
            .map_err(RenderStateError)?;
        Ok(Self { inner })
    }

    /// Rebinds the source HDR texture after a render-target resize.
    pub fn update_bind_group(&mut self, device: &wgpu::Device, hdr_view: &wgpu::TextureView) {
        self.inner.update_bind_group(device, hdr_view);
    }

    /// Applies post-processing and writes the final frame into `swapchain_view`.
    pub fn execute(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        swapchain_view: &wgpu::TextureView,
        settings: &crate::postprocess::PostProcessSettings,
    ) {
        self.inner.execute(device, queue, swapchain_view, settings);
    }
}
