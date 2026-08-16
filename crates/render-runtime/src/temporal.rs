//! Process-local temporal frame history owned by the runtime renderer.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec2};

const JITTER_PERIOD: u64 = 8;

/// Identifies the camera source so a camera switch invalidates accumulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemporalCameraSource {
    WorldEntity { id: u32, generation: u32 },
    Explicit,
}

/// Unjittered camera state retained across successful render frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TemporalCameraSample {
    source: TemporalCameraSource,
    view_projection: Mat4,
}

impl TemporalCameraSample {
    pub(crate) fn new(source: TemporalCameraSource, view_projection: Mat4) -> Self {
        Self {
            source,
            view_projection,
        }
    }
}

// Fields are consumed as one raw GPU uniform block rather than read individually in Rust.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TemporalUniformData {
    current_view_projection: [[f32; 4]; 4],
    previous_view_projection: [[f32; 4]; 4],
    /// xy=current candidate jitter in NDC, zw=previous candidate jitter in NDC.
    jitter: [f32; 4],
    /// x=history usable, y=frame index modulo 65536, z/w reserved.
    params: [f32; 4],
}

impl TemporalUniformData {
    fn initial() -> Self {
        Self {
            current_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
            previous_view_projection: Mat4::IDENTITY.to_cols_array_2d(),
            jitter: [0.0; 4],
            params: [0.0; 4],
        }
    }
}

struct TemporalTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    copy_bind_group: wgpu::BindGroup,
}

struct TemporalTargets {
    size: [u32; 2],
    targets: [TemporalTarget; 2],
    write_index: usize,
    history_valid: bool,
}

impl TemporalTargets {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        copy_bgl: &wgpu::BindGroupLayout,
        uniform_buffer: &wgpu::Buffer,
    ) -> Self {
        let make_target = |label: &'static str| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let copy_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Temporal output copy BG"),
                layout: copy_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                ],
            });
            TemporalTarget {
                _texture: texture,
                view,
                copy_bind_group,
            }
        };

        Self {
            size,
            targets: [
                make_target("Temporal scene color A"),
                make_target("Temporal scene color B"),
            ],
            write_index: 0,
            history_valid: false,
        }
    }

    fn current(&self) -> &TemporalTarget {
        &self.targets[self.write_index]
    }

    fn advance(&mut self) {
        self.history_valid = true;
        self.write_index ^= 1;
    }

    fn reset(&mut self) {
        self.write_index = 0;
        self.history_valid = false;
    }
}

/// Renderer-owned temporal resources around the ordinary scene renderer.
pub(crate) struct TemporalHistory {
    format: wgpu::TextureFormat,
    copy_bgl: wgpu::BindGroupLayout,
    copy_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    targets: Option<TemporalTargets>,
    frame_index: u64,
    previous_camera: Option<TemporalCameraSample>,
    previous_jitter: Vec2,
    pending_camera: Option<TemporalCameraSample>,
    pending_jitter: Vec2,
}

impl TemporalHistory {
    pub(crate) async fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Result<Self, wgpu::Error> {
        use wgpu::util::DeviceExt;

        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Temporal frame uniform"),
            contents: bytemuck::bytes_of(&TemporalUniformData::initial()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let copy_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Temporal output copy BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Temporal output copy shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/temporal_copy.wgsl").into(),
            ),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Temporal output copy pipeline layout"),
            bind_group_layouts: &[Some(&copy_bgl)],
            immediate_size: 0,
        });
        let copy_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Temporal output copy pipeline"),
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
                    format,
                    blend: None,
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

        if let Some(error) = error_scope.pop().await {
            return Err(error);
        }

        Ok(Self {
            format,
            copy_bgl,
            copy_pipeline,
            uniform_buffer,
            targets: None,
            frame_index: 0,
            previous_camera: None,
            previous_jitter: Vec2::ZERO,
            pending_camera: None,
            pending_jitter: Vec2::ZERO,
        })
    }

    pub(crate) fn prepare<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        caller_color_view: &wgpu::TextureView,
        camera: Option<TemporalCameraSample>,
    ) -> &'a wgpu::TextureView {
        let extent = caller_color_view.texture().size();
        let size = [extent.width.max(1), extent.height.max(1)];
        let needs_recreate = self
            .targets
            .as_ref()
            .is_none_or(|targets| targets.size != size);
        if needs_recreate {
            self.targets = Some(TemporalTargets::new(
                device,
                self.format,
                size,
                &self.copy_bgl,
                &self.uniform_buffer,
            ));
            self.reset_sequence();
        }

        let current_jitter = jitter_ndc(self.frame_index, size);
        let targets = self
            .targets
            .as_ref()
            .expect("temporal target creation must succeed for a valid device");
        let history_usable = camera_history_usable(
            self.previous_camera,
            camera,
            targets.history_valid,
        );
        let current_view_projection = camera
            .map(|sample| sample.view_projection)
            .unwrap_or(Mat4::IDENTITY);
        let previous_view_projection = if history_usable {
            self.previous_camera
                .map(|sample| sample.view_projection)
                .unwrap_or(current_view_projection)
        } else {
            current_view_projection
        };
        let previous_jitter = if history_usable {
            self.previous_jitter
        } else {
            current_jitter
        };
        let uniform = TemporalUniformData {
            current_view_projection: current_view_projection.to_cols_array_2d(),
            previous_view_projection: previous_view_projection.to_cols_array_2d(),
            jitter: [
                current_jitter.x,
                current_jitter.y,
                previous_jitter.x,
                previous_jitter.y,
            ],
            params: [
                f32::from(u8::from(history_usable)),
                (self.frame_index % 65_536) as f32,
                0.0,
                0.0,
            ],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        self.pending_camera = camera;
        self.pending_jitter = current_jitter;

        &self
            .targets
            .as_ref()
            .expect("temporal targets must exist after preparation")
            .current()
            .view
    }

    pub(crate) fn copy_current_to(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        caller_color_view: &wgpu::TextureView,
    ) {
        let target = self
            .targets
            .as_ref()
            .expect("temporal output copy requires a prepared target")
            .current();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Temporal output copy encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Temporal output copy pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: caller_color_view,
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
            pass.set_pipeline(&self.copy_pipeline);
            pass.set_bind_group(0, &target.copy_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    pub(crate) fn commit(&mut self) {
        self.previous_camera = self.pending_camera;
        self.previous_jitter = self.pending_jitter;
        self.frame_index = self.frame_index.wrapping_add(1);
        if let Some(targets) = &mut self.targets {
            targets.advance();
        }
    }

    pub(crate) fn reset(&mut self) {
        if let Some(targets) = &mut self.targets {
            targets.reset();
        }
        self.reset_sequence();
    }

    fn reset_sequence(&mut self) {
        self.frame_index = 0;
        self.previous_camera = None;
        self.previous_jitter = Vec2::ZERO;
        self.pending_camera = None;
        self.pending_jitter = Vec2::ZERO;
    }
}

fn camera_history_usable(
    previous: Option<TemporalCameraSample>,
    current: Option<TemporalCameraSample>,
    target_history_valid: bool,
) -> bool {
    target_history_valid
        && previous
            .zip(current)
            .is_some_and(|(previous, current)| previous.source == current.source)
}

fn jitter_ndc(frame_index: u64, size: [u32; 2]) -> Vec2 {
    let sample = frame_index % JITTER_PERIOD + 1;
    let x = halton(sample, 2) - 0.5;
    let y = halton(sample, 3) - 0.5;
    Vec2::new(
        2.0 * x / size[0].max(1) as f32,
        2.0 * y / size[1].max(1) as f32,
    )
}

fn halton(mut index: u64, base: u64) -> f32 {
    let mut result = 0.0;
    let mut fraction = 1.0 / base as f32;
    while index > 0 {
        result += fraction * (index % base) as f32;
        index /= base;
        fraction /= base as f32;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_jitter_is_deterministic_bounded_and_periodic() {
        let size = [1920, 1080];
        let first = jitter_ndc(0, size);
        assert_eq!(first, jitter_ndc(JITTER_PERIOD, size));
        for frame in 0..JITTER_PERIOD {
            let jitter = jitter_ndc(frame, size);
            assert!(jitter.x.abs() <= 1.0 / size[0] as f32);
            assert!(jitter.y.abs() <= 1.0 / size[1] as f32);
        }
    }

    #[test]
    fn temporal_camera_switch_invalidates_accumulation_without_heuristics() {
        let matrix = Mat4::IDENTITY;
        let first = Some(TemporalCameraSample::new(
            TemporalCameraSource::WorldEntity { id: 1, generation: 0 },
            matrix,
        ));
        let same = Some(TemporalCameraSample::new(
            TemporalCameraSource::WorldEntity { id: 1, generation: 0 },
            matrix,
        ));
        let switched = Some(TemporalCameraSample::new(
            TemporalCameraSource::WorldEntity { id: 2, generation: 0 },
            matrix,
        ));

        assert!(camera_history_usable(first, same, true));
        assert!(!camera_history_usable(first, switched, true));
        assert!(!camera_history_usable(first, same, false));
        assert!(!camera_history_usable(first, None, true));
    }

    #[test]
    fn temporal_copy_pipeline_validates_when_a_gpu_adapter_is_available() {
        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };
        pollster::block_on(TemporalHistory::new(
            context.device(),
            wgpu::TextureFormat::Rgba16Float,
        ))
        .expect("temporal history pipeline must validate");
    }
}
