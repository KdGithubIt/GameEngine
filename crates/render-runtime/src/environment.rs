use std::sync::{Arc, Weak};

use bytemuck::{Pod, Zeroable};

use crate::material::Texture;
use crate::shadow::EnvironmentLighting;

const DIFFUSE_WIDTH: u32 = 32;
const DIFFUSE_HEIGHT: u32 = 16;
const SPECULAR_WIDTH: u32 = 128;
const SPECULAR_HEIGHT: u32 = 64;
const SPECULAR_MIP_LEVELS: u32 = 8;
const BRDF_LUT_SIZE: u32 = 128;
const ENVIRONMENT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EnvironmentUniform {
    diffuse_color: [f32; 3],
    intensity: f32,
    /// x=diffuse enabled, y=has diffuse texture, z=specular enabled, w=has skybox.
    params: [f32; 4],
}

impl EnvironmentUniform {
    fn from_settings(
        settings: &EnvironmentLighting,
        has_diffuse_texture: bool,
        has_skybox: bool,
    ) -> Self {
        let finite = |value: f32, fallback: f32| {
            if value.is_finite() {
                value
            } else {
                fallback
            }
        };
        Self {
            diffuse_color: [
                finite(settings.diffuse_color.x, 1.0).max(0.0),
                finite(settings.diffuse_color.y, 1.0).max(0.0),
                finite(settings.diffuse_color.z, 1.0).max(0.0),
            ],
            intensity: finite(settings.intensity, 0.0).max(0.0),
            params: [
                flag(settings.diffuse_ibl_enabled),
                flag(has_diffuse_texture),
                flag(has_skybox),
                flag(has_skybox),
            ],
        }
    }
}

fn flag(enabled: bool) -> f32 {
    f32::from(u8::from(enabled))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BakeUniform {
    roughness: f32,
    _padding: [f32; 3],
}

impl BakeUniform {
    const fn new(roughness: f32) -> Self {
        Self {
            roughness,
            _padding: [0.0; 3],
        }
    }
}

/// Renderer-owned environment resources derived from runtime texture assets.
pub(crate) struct EnvironmentGpuState {
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    fallback: Texture,
    _brdf_texture: wgpu::Texture,
    brdf_view: wgpu::TextureView,
    _diffuse_texture: Option<wgpu::Texture>,
    diffuse_view: Option<wgpu::TextureView>,
    _specular_texture: Option<wgpu::Texture>,
    specular_view: Option<wgpu::TextureView>,
    source_bgl: wgpu::BindGroupLayout,
    diffuse_pipeline: wgpu::RenderPipeline,
    specular_pipeline: wgpu::RenderPipeline,
    skybox_source: Option<Weak<Texture>>,
    diffuse_override_source: Option<Weak<Texture>>,
    has_skybox: bool,
}

impl EnvironmentGpuState {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use wgpu::util::DeviceExt;

        let fallback = Texture::solid_rgba(
            device,
            queue,
            [0, 0, 0, 255],
            "environment_black_fallback",
            false,
        )
        .expect("one-pixel environment fallback must fit every valid GPU device");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Environment sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Environment IBL BGL"),
                entries: &[
                    texture_layout_entry(0),
                    texture_layout_entry(1),
                    texture_layout_entry(2),
                    texture_layout_entry(3),
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
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
        let source_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Environment precompute source BGL"),
            entries: &[
                texture_layout_entry(0),
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Environment precompute shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/environment_precompute.wgsl").into(),
            ),
        });
        let source_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Environment precompute pipeline layout"),
                bind_group_layouts: &[Some(&source_bgl)],
                immediate_size: 0,
            });
        let diffuse_pipeline = make_precompute_pipeline(
            device,
            "Environment diffuse irradiance pipeline",
            &source_pipeline_layout,
            &shader,
            "fs_diffuse",
        );
        let specular_pipeline = make_precompute_pipeline(
            device,
            "Environment GGX prefilter pipeline",
            &source_pipeline_layout,
            &shader,
            "fs_specular",
        );
        let brdf_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Environment BRDF integration pipeline layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let brdf_pipeline = make_precompute_pipeline(
            device,
            "Environment BRDF integration pipeline",
            &brdf_pipeline_layout,
            &shader,
            "fs_brdf",
        );

        let brdf_texture = make_render_texture(
            device,
            "Environment BRDF integration LUT",
            BRDF_LUT_SIZE,
            BRDF_LUT_SIZE,
            1,
        );
        let brdf_view = brdf_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Environment BRDF integration encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Environment BRDF integration pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &brdf_view,
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
            pass.set_pipeline(&brdf_pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));

        let uniform =
            EnvironmentUniform::from_settings(&EnvironmentLighting::default(), false, false);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Environment IBL uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = make_environment_bind_group(
            device,
            &bind_group_layout,
            &fallback.view,
            &fallback.view,
            &fallback.view,
            &brdf_view,
            &sampler,
            &uniform_buffer,
        );

        Self {
            bind_group_layout,
            bind_group,
            uniform_buffer,
            sampler,
            fallback,
            _brdf_texture: brdf_texture,
            brdf_view,
            _diffuse_texture: None,
            diffuse_view: None,
            _specular_texture: None,
            specular_view: None,
            source_bgl,
            diffuse_pipeline,
            specular_pipeline,
            skybox_source: None,
            diffuse_override_source: None,
            has_skybox: false,
        }
    }

    pub(crate) fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn has_skybox(&self) -> bool {
        self.has_skybox
    }

    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        settings: &EnvironmentLighting,
        skybox: Option<&Arc<Texture>>,
        diffuse_override: Option<&Arc<Texture>>,
    ) {
        let sky_changed = !weak_matches(&self.skybox_source, skybox);
        let diffuse_changed = !weak_matches(&self.diffuse_override_source, diffuse_override);

        if sky_changed {
            self.skybox_source = skybox.map(Arc::downgrade);
            self.has_skybox = skybox.is_some();
            if let Some(source) = skybox {
                let (diffuse_texture, diffuse_view, specular_texture, specular_view) =
                    self.precompute_environment(device, queue, source);
                self._diffuse_texture = Some(diffuse_texture);
                self.diffuse_view = Some(diffuse_view);
                self._specular_texture = Some(specular_texture);
                self.specular_view = Some(specular_view);
            } else {
                self._diffuse_texture = None;
                self.diffuse_view = None;
                self._specular_texture = None;
                self.specular_view = None;
            }
        }
        if diffuse_changed {
            self.diffuse_override_source = diffuse_override.map(Arc::downgrade);
        }

        let has_diffuse_texture = diffuse_override.is_some() || self.diffuse_view.is_some();
        let uniform =
            EnvironmentUniform::from_settings(settings, has_diffuse_texture, self.has_skybox);
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        if sky_changed || diffuse_changed {
            let source_view = skybox
                .map(|texture| &texture.view)
                .unwrap_or(&self.fallback.view);
            let diffuse_view = diffuse_override
                .map(|texture| &texture.view)
                .or(self.diffuse_view.as_ref())
                .unwrap_or(&self.fallback.view);
            let specular_view = self
                .specular_view
                .as_ref()
                .unwrap_or(&self.fallback.view);
            self.bind_group = make_environment_bind_group(
                device,
                &self.bind_group_layout,
                source_view,
                diffuse_view,
                specular_view,
                &self.brdf_view,
                &self.sampler,
                &self.uniform_buffer,
            );
        }
    }

    fn precompute_environment(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &Texture,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::Texture, wgpu::TextureView) {
        use wgpu::util::DeviceExt;

        let diffuse_texture = make_render_texture(
            device,
            "Environment diffuse irradiance",
            DIFFUSE_WIDTH,
            DIFFUSE_HEIGHT,
            1,
        );
        let diffuse_view = diffuse_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let specular_texture = make_render_texture(
            device,
            "Environment prefiltered specular",
            SPECULAR_WIDTH,
            SPECULAR_HEIGHT,
            SPECULAR_MIP_LEVELS,
        );
        let specular_view = specular_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Environment precompute encoder"),
        });

        let diffuse_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Environment diffuse bake uniform"),
            contents: bytemuck::bytes_of(&BakeUniform::new(1.0)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let diffuse_bind_group = make_precompute_bind_group(
            device,
            &self.source_bgl,
            source,
            &self.sampler,
            &diffuse_uniform,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Environment diffuse irradiance pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &diffuse_view,
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
            pass.set_pipeline(&self.diffuse_pipeline);
            pass.set_bind_group(0, &diffuse_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        for mip_level in 0..SPECULAR_MIP_LEVELS {
            let roughness = mip_level as f32 / (SPECULAR_MIP_LEVELS - 1) as f32;
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Environment specular bake uniform"),
                contents: bytemuck::bytes_of(&BakeUniform::new(roughness)),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = make_precompute_bind_group(
                device,
                &self.source_bgl,
                source,
                &self.sampler,
                &uniform,
            );
            let mip_view = specular_texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("Environment specular mip view"),
                base_mip_level: mip_level,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Environment specular prefilter pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &mip_view,
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
            pass.set_pipeline(&self.specular_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        queue.submit(Some(encoder.finish()));
        (diffuse_texture, diffuse_view, specular_texture, specular_view)
    }
}

fn texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            view_dimension: wgpu::TextureViewDimension::D2,
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
        },
        count: None,
    }
}

fn weak_matches(cached: &Option<Weak<Texture>>, current: Option<&Arc<Texture>>) -> bool {
    match (cached, current) {
        (None, None) => true,
        (Some(cached), Some(current)) => cached.ptr_eq(&Arc::downgrade(current)),
        _ => false,
    }
}

fn make_render_texture(
    device: &wgpu::Device,
    label: &str,
    width: u32,
    height: u32,
    mip_level_count: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ENVIRONMENT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn make_precompute_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fragment_entry: &str,
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
                format: ENVIRONMENT_FORMAT,
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

fn make_precompute_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &Texture,
    sampler: &wgpu::Sampler,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Environment precompute BG"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source.view),
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

#[allow(clippy::too_many_arguments)]
fn make_environment_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    source: &wgpu::TextureView,
    diffuse: &wgpu::TextureView,
    specular: &wgpu::TextureView,
    brdf: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Environment IBL BG"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(diffuse),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(specular),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(brdf),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: uniform_buffer.as_entire_binding(),
            },
        ],
    })
}
