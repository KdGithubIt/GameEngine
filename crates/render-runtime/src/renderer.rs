//! Public facade for the private runtime rendering backend.

use std::fmt;

use crate::bloom::BloomPass;
use crate::camera::{select_active_game_camera, Camera3D};
use crate::temporal::{TemporalCameraSample, TemporalCameraSource, TemporalHistory};
use crate::transform::Transform;

/// Fixed multisample count used by the primary 3D render pass.
pub const MAIN_PASS_SAMPLE_COUNT: u32 = crate::render_backend::MAIN_PASS_SAMPLE_COUNT;

#[derive(Debug)]
enum RenderStateErrorKind {
    Backend(crate::render_backend::RenderStateError),
    Bloom(wgpu::Error),
    Temporal(wgpu::Error),
}

/// Error returned while constructing runtime render pipelines.
#[derive(Debug)]
pub struct RenderStateError(RenderStateErrorKind);

impl fmt::Display for RenderStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            RenderStateErrorKind::Backend(error) => error.fmt(formatter),
            RenderStateErrorKind::Bloom(error) => {
                write!(formatter, "bloom render pipeline validation failed: {error}")
            }
            RenderStateErrorKind::Temporal(error) => {
                write!(formatter, "temporal render pipeline validation failed: {error}")
            }
        }
    }
}

impl std::error::Error for RenderStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            RenderStateErrorKind::Backend(error) => Some(error),
            RenderStateErrorKind::Bloom(error) => Some(error),
            RenderStateErrorKind::Temporal(error) => Some(error),
        }
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
    temporal: TemporalHistory,
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
            .map_err(|error| RenderStateError(RenderStateErrorKind::Backend(error)))?;
        let temporal = TemporalHistory::new(device, format)
            .await
            .map_err(|error| RenderStateError(RenderStateErrorKind::Temporal(error)))?;
        Ok(Self { inner, temporal })
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
        if color_view.texture().sample_count() != 1 {
            return self
                .inner
                .render_to_view(world, device, queue, color_view, depth_view)
                .map_err(RenderFrameError);
        }

        let camera_sample = Self::active_temporal_camera(world);
        let temporal_view = self
            .temporal
            .prepare(device, queue, color_view, camera_sample);
        self.inner
            .render_to_view(world, device, queue, temporal_view, depth_view)
            .map_err(RenderFrameError)?;
        self.temporal
            .copy_current_to(device, queue, color_view);
        self.temporal.commit();
        Ok(())
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
        if color_view.texture().sample_count() != 1 {
            return self
                .inner
                .render_to_view_with_camera(
                    world,
                    camera,
                    camera_transform,
                    device,
                    queue,
                    color_view,
                    depth_view,
                )
                .map_err(RenderFrameError);
        }

        let camera_sample = Some(TemporalCameraSample::new(
            TemporalCameraSource::Explicit,
            camera.view_projection_matrix(camera_transform),
        ));
        let temporal_view = self
            .temporal
            .prepare(device, queue, color_view, camera_sample);
        self.inner
            .render_to_view_with_camera(
                world,
                camera,
                camera_transform,
                device,
                queue,
                temporal_view,
                depth_view,
            )
            .map_err(RenderFrameError)?;
        self.temporal
            .copy_current_to(device, queue, color_view);
        self.temporal.commit();
        Ok(())
    }

    /// Invalidates renderer-owned temporal history after a discontinuous camera change.
    ///
    /// Resizing the render target and switching to another active camera entity
    /// invalidate history automatically. Call this after a cut, teleport, or
    /// projection discontinuity that intentionally keeps the same camera source.
    pub fn reset_temporal_history(&mut self) {
        self.temporal.reset();
    }

    fn active_temporal_camera(
        world: &mut engine_ecs::World,
    ) -> Option<TemporalCameraSample> {
        let query = engine_ecs::Query::<(&Camera3D, &Transform)>::new(world);
        select_active_game_camera(query.iter()).map(|(entity, (camera, transform))| {
            TemporalCameraSample::new(
                TemporalCameraSource::WorldEntity {
                    id: entity.id(),
                    generation: entity.generation(),
                },
                camera.view_projection_matrix(transform),
            )
        })
    }
}

/// Fullscreen HDR-to-output tone-map pass used by the windowed runtime host.
pub struct ToneMapPass {
    inner: crate::render_backend::ToneMapPass,
    bloom: BloomPass,
    source_hdr_view: wgpu::TextureView,
    inner_reads_bloom: bool,
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
            .map_err(|error| RenderStateError(RenderStateErrorKind::Backend(error)))?;
        let bloom = BloomPass::new(device, hdr_view)
            .await
            .map_err(|error| RenderStateError(RenderStateErrorKind::Bloom(error)))?;
        Ok(Self {
            inner,
            bloom,
            source_hdr_view: hdr_view.clone(),
            inner_reads_bloom: false,
        })
    }

    /// Rebinds the source HDR texture after a render-target resize.
    pub fn update_bind_group(&mut self, device: &wgpu::Device, hdr_view: &wgpu::TextureView) {
        self.source_hdr_view = hdr_view.clone();
        self.bloom.update_source(device, hdr_view);
        self.inner.update_bind_group(device, hdr_view);
        self.inner_reads_bloom = false;
    }

    /// Applies post-processing and writes the final frame into `swapchain_view`.
    pub fn execute(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        swapchain_view: &wgpu::TextureView,
        settings: &crate::postprocess::PostProcessSettings,
    ) {
        let use_bloom = settings.enabled && settings.bloom.enabled;
        if use_bloom {
            self.bloom.execute(device, queue, &settings.bloom);
            if !self.inner_reads_bloom {
                let bloom_view = self.bloom.output_view().clone();
                self.inner.update_bind_group(device, &bloom_view);
                self.inner_reads_bloom = true;
            }

            let mut tone_map_settings = *settings;
            tone_map_settings.bloom.enabled = false;
            self.inner
                .execute(device, queue, swapchain_view, &tone_map_settings);
        } else {
            if self.inner_reads_bloom {
                self.inner
                    .update_bind_group(device, &self.source_hdr_view);
                self.inner_reads_bloom = false;
            }
            self.inner.execute(device, queue, swapchain_view, settings);
        }
    }
}
