use engine_render_runtime::{
    postprocess::{BloomSettings, PostProcessSettings, ToneMapOperator},
    renderer::ToneMapPass,
};

const WIDTH: u32 = 32;
const HEIGHT: u32 = 32;
const BYTES_PER_PIXEL_RGBA16F: usize = 8;
const BLACK_RGBA16F: [u8; BYTES_PER_PIXEL_RGBA16F] = [0, 0, 0, 0, 0, 0, 0, 0x3c];
const BRIGHT_RGBA16F: [u8; BYTES_PER_PIXEL_RGBA16F] =
    [0, 0x40, 0, 0x40, 0, 0x40, 0, 0x3c];

fn impulse_hdr_pixels() -> Vec<u8> {
    let mut pixels = vec![0_u8; WIDTH as usize * HEIGHT as usize * BYTES_PER_PIXEL_RGBA16F];

    for pixel in pixels.chunks_exact_mut(BYTES_PER_PIXEL_RGBA16F) {
        pixel.copy_from_slice(&BLACK_RGBA16F);
    }

    for y in 14..18 {
        for x in 14..18 {
            let offset = ((y * WIDTH + x) as usize) * BYTES_PER_PIXEL_RGBA16F;
            pixels[offset..offset + BYTES_PER_PIXEL_RGBA16F].copy_from_slice(&BRIGHT_RGBA16F);
        }
    }

    pixels
}

fn readback_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<[u8; 4]> {
    const BYTES_PER_PIXEL: u32 = 4;
    let unpadded_bytes_per_row = WIDTH * BYTES_PER_PIXEL;
    let padded_bytes_per_row = unpadded_bytes_per_row
        .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Bloom spread readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Bloom spread readback encoder"),
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
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
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
        .expect("Bloom spread readback device poll must succeed");
    receiver
        .recv()
        .expect("Bloom spread readback callback must run")
        .expect("Bloom spread readback buffer must map");

    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for y in 0..HEIGHT as usize {
        let row_start = y * padded_bytes_per_row as usize;
        for x in 0..WIDTH as usize {
            let offset = row_start + x * BYTES_PER_PIXEL as usize;
            pixels.push([
                mapped[offset],
                mapped[offset + 1],
                mapped[offset + 2],
                mapped[offset + 3],
            ]);
        }
    }
    drop(mapped);
    buffer.unmap();
    pixels
}

#[test]
fn bloom_spreads_bright_source_into_neighboring_pixels_when_a_gpu_adapter_is_available() {
    let instance = wgpu::Instance::default();
    let context = match pollster::block_on(engine_renderer::GpuContext::new(&instance, None)) {
        Ok(context) => context,
        Err(engine_renderer::GpuContextError::AdapterUnavailable) => return,
        Err(error) => panic!("GPU device creation failed: {error}"),
    };
    let device = context.device();
    let queue = context.queue();

    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Bloom spread HDR source"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &impulse_hdr_pixels(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(WIDTH * BYTES_PER_PIXEL_RGBA16F as u32),
            rows_per_image: Some(HEIGHT),
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());

    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Bloom spread output"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let mut tone_map = pollster::block_on(ToneMapPass::new(
        device,
        wgpu::TextureFormat::Rgba8Unorm,
        &source_view,
    ))
    .expect("tone-map and Bloom pipelines must validate");

    let bloom = BloomSettings {
        enabled: false,
        threshold: 0.0,
        intensity: 64.0,
        radius: 8.0,
    };
    let without_bloom = PostProcessSettings {
        tone_map: ToneMapOperator::Reinhard,
        bloom,
        ..PostProcessSettings::default()
    };

    tone_map.execute(device, queue, &output_view, &without_bloom);
    let baseline = readback_image(device, queue, &output);

    let with_bloom = PostProcessSettings {
        bloom: BloomSettings {
            enabled: true,
            ..bloom
        },
        ..without_bloom
    };
    tone_map.execute(device, queue, &output_view, &with_bloom);
    let bloomed = readback_image(device, queue, &output);

    tone_map.execute(device, queue, &output_view, &without_bloom);
    let restored = readback_image(device, queue, &output);

    assert_eq!(
        baseline, restored,
        "disabling Bloom must remove the halo again"
    );

    let mut spread_pixels = 0_usize;
    let mut max_channel_delta = 0_u8;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if (14..18).contains(&x) && (14..18).contains(&y) {
                continue;
            }
            let index = (y * WIDTH + x) as usize;
            let baseline_pixel = baseline[index];
            let bloomed_pixel = bloomed[index];
            let mut pixel_spread = false;
            for channel in 0..3 {
                let delta = bloomed_pixel[channel].saturating_sub(baseline_pixel[channel]);
                max_channel_delta = max_channel_delta.max(delta);
                pixel_spread |= delta > 0;
            }
            spread_pixels += usize::from(pixel_spread);
        }
    }

    assert!(
        spread_pixels > 0,
        "Bloom must spread the bright source into at least one originally dark pixel: spread_pixels={spread_pixels}, max_channel_delta={max_channel_delta}"
    );
}
