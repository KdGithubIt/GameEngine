//! Renderer-owned multi-resolution bloom resources.

use bytemuck::{Pod, Zeroable};

use crate::postprocess::BloomSettings;

const MAX_BLOOM_LEVELS: usize = 5;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BloomUniformData {
    threshold: f32,
    intensity: f32,
    radius: f32,
    _padding: f32,
}

impl BloomUniformData {
    fn from_settings(settings: &BloomSettings) -> Self {
        Self {
            threshold: finite_nonnegative(settings.threshold),
            intensity: finite_nonnegative(settings.intensity),
            radius: finite_nonnegative(settings.radius),
            _padding: 0.0,
        }
    }
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

struct BloomImage {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl BloomImage {
    fn new(
        device: &wgpu::Device,
        label: &str,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> Self {
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
        Self {
            _texture: texture,
            view,
        }
    }
}

struct BloomTargets {
    downsampled: Vec<BloomImage>,
    upsampled: Vec<BloomImage>,
    composite: BloomImage,
    prefilter_bind_group: wgpu::BindGroup,
    downsample_bind_groups: Vec<wgpu::BindGroup>,
    upsample_bind_groups: Vec<wgpu::BindGroup>,
    composite_bind_group: wgpu::BindGroup,
}

impl BloomTargets {
    fn new(
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform_buffer: &wgpu::Buffer,
    ) -> Self {
        let extent = source_view.texture().size();
        let source_size = [extent.width.max(1), extent.height.max(1)];
        let level_sizes = bloom_level_extents(source_size);

        let downsampled = level_sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                BloomImage::new(device, &format!("Bloom downsample {index}"), *size, format)
            })
            .collect::<Vec<_>>();
        let upsampled = level_sizes
            .iter()
            .take(level_sizes.len().saturating_sub(1))
            .enumerate()
            .map(|(index, size)| {
                BloomImage::new(device, &format!("Bloom upsample {index}"), *size, format)
            })
            .collect::<Vec<_>>();
        let composite = BloomImage::new(device, "Bloom HDR composite", source_size, format);

        let prefilter_bind_group = make_bind_group(
            device,
            bgl,
            source_view,
            source_view,
            sampler,
            uniform_buffer,
            "Bloom prefilter BG",
        );
        let downsample_bind_groups = downsampled
            .windows(2)
            .enumerate()
            .map(|(index, pair)| {
                make_bind_group(
                    device,
                    bgl,
                    &pair[0].view,
                    &pair[0].view,
                    sampler,
                    uniform_buffer,
                    &format!("Bloom downsample BG {index}"),
                )
            })
            .collect::<Vec<_>>();

        let mut upsample_bind_groups = Vec::with_capacity(upsampled.len());
        for index in 0..upsampled.len() {
            let coarse_view = if index + 1 == downsampled.len() - 1 {
                &downsampled[index + 1].view
            } else {
                &upsampled[index + 1].view
            };
            upsample_bind_groups.push(make_bind_group(
                device,
                bgl,
                &downsampled[index].view,
                coarse_view,
                sampler,
                uniform_buffer,
                &format!("Bloom upsample BG {index}"),
            ));
        }

        let bloom_view = upsampled
            .first()
            .map(|image| &image.view)
            .unwrap_or(&downsampled[0].view);
        let composite_bind_group = make_bind_group(
            device,
            bgl,
            source_view,
            bloom_view,
            sampler,
            uniform_buffer,
            "Bloom composite BG",
        );

        Self {
            downsampled,
            upsampled,
            composite,
            prefilter_bind_group,
            downsample_bind_groups,
            upsample_bind_groups,
            composite_bind_group,
        }
    }
}

fn bloom_level_extents(source_size: [u32; 2]) -> Vec<[u32; 2]> {
    let mut size = [
        source_size[0].max(1).div_ceil(2),
        source_size[1].max(1).div_ceil(2),
    ];
    let mut levels = Vec::with_capacity(MAX_BLOOM_LEVELS);
    for _ in 0..MAX_BLOOM_LEVELS {
        levels.push(size);
        if size == [1, 1] {
            break;
        }
        size = [size[0].div_ceil(2), size[1].div_ceil(2)];
    }
    levels
}

fn make_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    source_a: &wgpu::TextureView,
    source_b: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform_buffer: &wgpu::Buffer,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source_a),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(source_b),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: uniform_buffer.as_entire_binding(),
            },
        ],
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    entry_point: &'static str,
    label: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry_point),
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
    })
}

fn encode_fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
    label: &'static str,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
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
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

pub(crate) struct BloomPass {
    format: wgpu::TextureFormat,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    prefilter_pipeline: wgpu::RenderPipeline,
    downsample_pipeline: wgpu::RenderPipeline,
    upsample_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    targets: BloomTargets,
}

impl BloomPass {
    pub(crate) async fn new(
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
    ) -> Result<Self, wgpu::Error> {
        use wgpu::util::DeviceExt;

        let format = source_view.texture().format();
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Bloom sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Bloom uniform"),
            contents: bytemuck::bytes_of(&BloomUniformData::from_settings(
                &BloomSettings::default(),
            )),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Bloom BGL"),
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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
            label: Some("Bloom shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bloom.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Bloom pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let prefilter_pipeline = create_pipeline(
            device,
            &layout,
            &shader,
            format,
            "fs_prefilter",
            "Bloom prefilter pipeline",
        );
        let downsample_pipeline = create_pipeline(
            device,
            &layout,
            &shader,
            format,
            "fs_downsample",
            "Bloom downsample pipeline",
        );
        let upsample_pipeline = create_pipeline(
            device,
            &layout,
            &shader,
            format,
            "fs_upsample",
            "Bloom upsample pipeline",
        );
        let composite_pipeline = create_pipeline(
            device,
            &layout,
            &shader,
            format,
            "fs_composite",
            "Bloom composite pipeline",
        );
        let targets = BloomTargets::new(device, source_view, format, &bgl, &sampler, &uniform_buffer);

        if let Some(error) = error_scope.pop().await {
            return Err(error);
        }

        Ok(Self {
            format,
            bgl,
            sampler,
            uniform_buffer,
            prefilter_pipeline,
            downsample_pipeline,
            upsample_pipeline,
            composite_pipeline,
            targets,
        })
    }

    pub(crate) fn update_source(
        &mut self,
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
    ) {
        debug_assert_eq!(source_view.texture().format(), self.format);
        self.targets = BloomTargets::new(
            device,
            source_view,
            self.format,
            &self.bgl,
            &self.sampler,
            &self.uniform_buffer,
        );
    }

    pub(crate) fn execute(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: &BloomSettings,
    ) {
        let uniform = BloomUniformData::from_settings(settings);
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Bloom encoder"),
        });
        encode_fullscreen_pass(
            &mut encoder,
            &self.prefilter_pipeline,
            &self.targets.prefilter_bind_group,
            &self.targets.downsampled[0].view,
            "Bloom prefilter pass",
        );
        for (index, bind_group) in self.targets.downsample_bind_groups.iter().enumerate() {
            encode_fullscreen_pass(
                &mut encoder,
                &self.downsample_pipeline,
                bind_group,
                &self.targets.downsampled[index + 1].view,
                "Bloom downsample pass",
            );
        }
        for index in (0..self.targets.upsampled.len()).rev() {
            encode_fullscreen_pass(
                &mut encoder,
                &self.upsample_pipeline,
                &self.targets.upsample_bind_groups[index],
                &self.targets.upsampled[index].view,
                "Bloom upsample pass",
            );
        }
        encode_fullscreen_pass(
            &mut encoder,
            &self.composite_pipeline,
            &self.targets.composite_bind_group,
            &self.targets.composite.view,
            "Bloom composite pass",
        );
        queue.submit(std::iter::once(encoder.finish()));
    }

    pub(crate) fn output_view(&self) -> &wgpu::TextureView {
        &self.targets.composite.view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_pyramid_halves_extent_and_stays_bounded() {
        assert_eq!(
            bloom_level_extents([1920, 1080]),
            vec![[960, 540], [480, 270], [240, 135], [120, 68], [60, 34]]
        );
        assert_eq!(bloom_level_extents([1, 1]), vec![[1, 1]]);
        assert_eq!(bloom_level_extents([3, 2]), vec![[2, 1], [1, 1]]);
    }

    #[test]
    fn bloom_uniform_rejects_negative_and_non_finite_values() {
        let uniform = BloomUniformData::from_settings(&BloomSettings {
            enabled: true,
            threshold: f32::NAN,
            intensity: -2.0,
            radius: f32::INFINITY,
        });

        assert_eq!(uniform.threshold, 0.0);
        assert_eq!(uniform.intensity, 0.0);
        assert_eq!(uniform.radius, 0.0);
    }

    #[test]
    fn bloom_pipeline_validates_when_a_gpu_adapter_is_available() {
        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };
        let device = context.device();
        let queue = context.queue();
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Bloom validation source"),
            size: wgpu::Extent3d {
                width: 128,
                height: 72,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let mut bloom = pollster::block_on(BloomPass::new(device, &source_view))
            .expect("multi-resolution bloom pipelines must validate");

        device.push_error_scope(wgpu::ErrorFilter::Validation);
        bloom.execute(
            device,
            queue,
            &BloomSettings {
                enabled: true,
                ..BloomSettings::default()
            },
        );
        let validation_error = pollster::block_on(device.pop_error_scope());
        assert!(
            validation_error.is_none(),
            "bloom execution must not report validation errors: {validation_error:?}"
        );
        assert_eq!(bloom.output_view().texture().size().width, 128);
        assert_eq!(bloom.output_view().texture().size().height, 72);
    }
}
