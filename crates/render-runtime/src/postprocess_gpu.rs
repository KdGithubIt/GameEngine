use bytemuck::{Pod, Zeroable};

use crate::postprocess::PostProcessSettings;

const BLOOM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const MAX_BLOOM_LEVELS: usize = 6;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BloomUniformData {
    threshold: f32,
    radius: f32,
    intensity: f32,
    _padding: f32,
}

impl BloomUniformData {
    fn downsample(settings: &PostProcessSettings, level: usize) -> Self {
        Self {
            threshold: if level == 0 {
                settings.bloom.threshold.max(0.0)
            } else {
                0.0
            },
            radius: bloom_filter_radius(settings.bloom.radius),
            intensity: 0.0,
            _padding: 0.0,
        }
    }

    fn upsample(settings: &PostProcessSettings) -> Self {
        Self {
            threshold: 0.0,
            radius: bloom_filter_radius(settings.bloom.radius),
            intensity: 0.0,
            _padding: 0.0,
        }
    }

    fn composite(settings: &PostProcessSettings) -> Self {
        Self {
            threshold: 0.0,
            radius: 0.0,
            intensity: settings.bloom.intensity.max(0.0),
            _padding: 0.0,
        }
    }
}

fn bloom_filter_radius(radius: f32) -> f32 {
    if radius.is_finite() {
        (radius.max(0.0) / 4.0).min(4.0)
    } else {
        1.0
    }
}

fn bloom_pyramid_extents(width: u32, height: u32) -> Vec<[u32; 2]> {
    let mut width = width.max(1);
    let mut height = height.max(1);
    let mut extents = Vec::with_capacity(MAX_BLOOM_LEVELS);

    for _ in 0..MAX_BLOOM_LEVELS {
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        extents.push([width, height]);
        if width == 1 && height == 1 {
            break;
        }
    }

    extents
}

struct BloomTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl BloomTarget {
    fn new(device: &wgpu::Device, extent: [u32; 2], label: &str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: extent[0],
                height: extent[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: BLOOM_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
        }
    }
}

struct BloomResources {
    source_view: wgpu::TextureView,
    downsample_targets: Vec<BloomTarget>,
    upsample_targets: Vec<BloomTarget>,
    composite_target: BloomTarget,
    downsample_uniforms: Vec<wgpu::Buffer>,
    upsample_uniforms: Vec<wgpu::Buffer>,
    composite_uniform: wgpu::Buffer,
    downsample_bind_groups: Vec<wgpu::BindGroup>,
    upsample_bind_groups: Vec<wgpu::BindGroup>,
    composite_bind_group: wgpu::BindGroup,
}

impl BloomResources {
    fn new(
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
        bgl: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let source_texture = source_view.texture();
        let source_extent = [source_texture.width(), source_texture.height()];
        let extents = bloom_pyramid_extents(source_extent[0], source_extent[1]);

        let downsample_targets = extents
            .iter()
            .enumerate()
            .map(|(index, extent)| {
                BloomTarget::new(device, *extent, &format!("Bloom downsample {index}"))
            })
            .collect::<Vec<_>>();
        let upsample_targets = extents
            .iter()
            .take(extents.len().saturating_sub(1))
            .enumerate()
            .map(|(index, extent)| {
                BloomTarget::new(device, *extent, &format!("Bloom upsample {index}"))
            })
            .collect::<Vec<_>>();
        let composite_target = BloomTarget::new(device, source_extent, "Bloom HDR composite");

        let downsample_uniforms = (0..downsample_targets.len())
            .map(|index| make_uniform_buffer(device, &format!("Bloom downsample uniform {index}")))
            .collect::<Vec<_>>();
        let upsample_uniforms = (0..upsample_targets.len())
            .map(|index| make_uniform_buffer(device, &format!("Bloom upsample uniform {index}")))
            .collect::<Vec<_>>();
        let composite_uniform = make_uniform_buffer(device, "Bloom composite uniform");

        let downsample_bind_groups = downsample_targets
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let primary = if index == 0 {
                    source_view
                } else {
                    &downsample_targets[index - 1].view
                };
                make_bind_group(
                    device,
                    bgl,
                    primary,
                    primary,
                    sampler,
                    &downsample_uniforms[index],
                    &format!("Bloom downsample BG {index}"),
                )
            })
            .collect::<Vec<_>>();

        let upsample_bind_groups = upsample_targets
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let lower = if index + 1 == downsample_targets.len() - 1 {
                    &downsample_targets[index + 1].view
                } else {
                    &upsample_targets[index + 1].view
                };
                make_bind_group(
                    device,
                    bgl,
                    &downsample_targets[index].view,
                    lower,
                    sampler,
                    &upsample_uniforms[index],
                    &format!("Bloom upsample BG {index}"),
                )
            })
            .collect::<Vec<_>>();

        let bloom_root = if upsample_targets.is_empty() {
            &downsample_targets[0].view
        } else {
            &upsample_targets[0].view
        };
        let composite_bind_group = make_bind_group(
            device,
            bgl,
            source_view,
            bloom_root,
            sampler,
            &composite_uniform,
            "Bloom composite BG",
        );

        Self {
            source_view: source_view.clone(),
            downsample_targets,
            upsample_targets,
            composite_target,
            downsample_uniforms,
            upsample_uniforms,
            composite_uniform,
            downsample_bind_groups,
            upsample_bind_groups,
            composite_bind_group,
        }
    }
}

pub(crate) struct BloomPass {
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    downsample_pipeline: wgpu::RenderPipeline,
    upsample_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    resources: BloomResources,
}

impl BloomPass {
    pub(crate) async fn new(
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
    ) -> Result<Self, wgpu::Error> {
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Bloom linear sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bgl = make_bind_group_layout(device);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Bloom shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bloom.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Bloom pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let downsample_pipeline = make_pipeline(
            device,
            &layout,
            &shader,
            "fs_downsample",
            "Bloom downsample pipeline",
        );
        let upsample_pipeline = make_pipeline(
            device,
            &layout,
            &shader,
            "fs_upsample",
            "Bloom upsample pipeline",
        );
        let composite_pipeline = make_pipeline(
            device,
            &layout,
            &shader,
            "fs_composite",
            "Bloom composite pipeline",
        );
        let resources = BloomResources::new(device, source_view, &bgl, &sampler);

        if let Some(error) = error_scope.pop().await {
            return Err(error);
        }

        Ok(Self {
            bgl,
            sampler,
            downsample_pipeline,
            upsample_pipeline,
            composite_pipeline,
            resources,
        })
    }

    pub(crate) fn update_source(
        &mut self,
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
    ) {
        self.resources = BloomResources::new(device, source_view, &self.bgl, &self.sampler);
    }

    pub(crate) fn source_view(&self) -> &wgpu::TextureView {
        &self.resources.source_view
    }

    pub(crate) fn output_view(&self) -> &wgpu::TextureView {
        &self.resources.composite_target.view
    }

    pub(crate) fn execute(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: &PostProcessSettings,
    ) {
        for (index, buffer) in self.resources.downsample_uniforms.iter().enumerate() {
            let uniform = BloomUniformData::downsample(settings, index);
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
        }
        let upsample_uniform = BloomUniformData::upsample(settings);
        for buffer in &self.resources.upsample_uniforms {
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&upsample_uniform));
        }
        let composite_uniform = BloomUniformData::composite(settings);
        queue.write_buffer(
            &self.resources.composite_uniform,
            0,
            bytemuck::bytes_of(&composite_uniform),
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Bloom encoder"),
        });

        for (index, target) in self.resources.downsample_targets.iter().enumerate() {
            run_fullscreen_pass(
                &mut encoder,
                &target.view,
                &self.downsample_pipeline,
                &self.resources.downsample_bind_groups[index],
                "Bloom downsample pass",
            );
        }

        for index in (0..self.resources.upsample_targets.len()).rev() {
            run_fullscreen_pass(
                &mut encoder,
                &self.resources.upsample_targets[index].view,
                &self.upsample_pipeline,
                &self.resources.upsample_bind_groups[index],
                "Bloom upsample pass",
            );
        }

        run_fullscreen_pass(
            &mut encoder,
            &self.resources.composite_target.view,
            &self.composite_pipeline,
            &self.resources.composite_bind_group,
            "Bloom HDR composite pass",
        );

        queue.submit(std::iter::once(encoder.finish()));
    }
}

fn make_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Bloom BGL"),
        entries: &[
            texture_layout_entry(0),
            texture_layout_entry(1),
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
    })
}

fn texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn make_uniform_buffer(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: std::mem::size_of::<BloomUniformData>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn make_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    primary: &wgpu::TextureView,
    secondary: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(primary),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(secondary),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fragment_entry: &str,
    label: &str,
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
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: BLOOM_FORMAT,
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
    })
}

fn run_fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    label: &str,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_pyramid_is_bounded_and_never_zero_sized() {
        let extents = bloom_pyramid_extents(1920, 1080);
        assert_eq!(extents[0], [960, 540]);
        assert!(extents.len() <= MAX_BLOOM_LEVELS);
        assert!(extents.iter().all(|extent| extent[0] > 0 && extent[1] > 0));
    }

    #[test]
    fn bloom_pyramid_reaches_one_by_one_for_small_targets() {
        assert_eq!(
            bloom_pyramid_extents(8, 4),
            vec![[4, 2], [2, 1], [1, 1]]
        );
        assert_eq!(bloom_pyramid_extents(1, 1), vec![[1, 1]]);
    }

    #[test]
    fn bloom_radius_maps_default_to_one_texel_filter_scale() {
        assert_eq!(bloom_filter_radius(4.0), 1.0);
        assert_eq!(bloom_filter_radius(0.0), 0.0);
        assert_eq!(bloom_filter_radius(-4.0), 0.0);
        assert_eq!(bloom_filter_radius(f32::INFINITY), 1.0);
    }

    #[test]
    fn bloom_pipelines_validate_when_a_gpu_adapter_is_available() {
        let instance = wgpu::Instance::default();
        let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
            Ok(context) => context,
            Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
            Err(error) => panic!("GPU device creation failed: {error}"),
        };
        let source = context.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("Bloom validation source"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: BLOOM_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());

        let mut bloom = pollster::block_on(BloomPass::new(context.device(), &source_view))
            .expect("multi-resolution bloom pipelines must validate");
        let settings = PostProcessSettings {
            bloom: crate::postprocess::BloomSettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let error_scope = context
            .device()
            .push_error_scope(wgpu::ErrorFilter::Validation);
        bloom.execute(context.device(), context.queue(), &settings);
        let execution_error = pollster::block_on(error_scope.pop());
        assert!(
            execution_error.is_none(),
            "multi-resolution bloom execution must validate: {execution_error:?}"
        );
    }
}
