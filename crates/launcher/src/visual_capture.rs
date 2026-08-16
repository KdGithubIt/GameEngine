use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

pub(crate) struct VisualCapture {
    path: Option<PathBuf>,
    requested_at: Option<Instant>,
}

impl VisualCapture {
    pub(crate) fn from_environment() -> Self {
        Self {
            path: std::env::var_os("GAMEENGINE_LAUNCHER_SCREENSHOT_TO").map(PathBuf::from),
            requested_at: None,
        }
    }

    pub(crate) fn update(&mut self, context: &egui::Context) {
        let Some(path) = self.path.clone() else {
            return;
        };

        let screenshot = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = screenshot {
            if let Err(error) = write_png(&path, image.as_ref()) {
                let _ = std::fs::remove_file(&path);
                eprintln!("[launcher.visual_validation_capture_failed] {error}");
            }
            self.path = None;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        match self.requested_at {
            None => {
                self.requested_at = Some(Instant::now());
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::UserData::default(),
                ));
                context.request_repaint();
            }
            Some(requested_at) if requested_at.elapsed() >= CAPTURE_TIMEOUT => {
                let _ = std::fs::remove_file(&path);
                eprintln!(
                    "[launcher.visual_validation_capture_failed] screenshot event was not returned"
                );
                self.path = None;
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(_) => context.request_repaint(),
        }
    }
}

fn write_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return Err("screenshot image has zero extent".to_owned());
    }
    let expected_pixels = width
        .checked_mul(height)
        .ok_or_else(|| "screenshot dimensions overflow pixel count".to_owned())?;
    if image.pixels.len() != expected_pixels {
        return Err("screenshot pixel count does not match its dimensions".to_owned());
    }

    let row_bytes = width
        .checked_mul(4)
        .ok_or_else(|| "screenshot row size overflowed".to_owned())?;
    let raw_capacity = row_bytes
        .checked_add(1)
        .and_then(|stride| stride.checked_mul(height))
        .ok_or_else(|| "screenshot PNG buffer size overflowed".to_owned())?;
    let mut raw = Vec::with_capacity(raw_capacity);
    for row in image.pixels.chunks_exact(width) {
        raw.push(0); // PNG filter type: None.
        raw.extend(row.iter().flat_map(|pixel| pixel.to_array()));
    }

    let width = u32::try_from(width).map_err(|_| "screenshot width exceeds PNG limits")?;
    let height = u32::try_from(height).map_err(|_| "screenshot height exceeds PNG limits")?;
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut encoded = Vec::with_capacity(raw.len().saturating_add(128));
    encoded.extend_from_slice(PNG_SIGNATURE);
    append_png_chunk(&mut encoded, *b"IHDR", &ihdr)?;
    append_png_chunk(&mut encoded, *b"IDAT", &zlib_store(&raw))?;
    append_png_chunk(&mut encoded, *b"IEND", &[])?;
    std::fs::write(path, encoded).map_err(|error| error.to_string())
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(data.len().saturating_add(data.len() / 65_535 * 5 + 16));
    encoded.extend_from_slice(&[0x78, 0x01]);

    let mut offset = 0;
    while offset < data.len() {
        let block_len = (data.len() - offset).min(u16::MAX as usize);
        let end = offset + block_len;
        encoded.push(u8::from(end == data.len()));
        let block_len = block_len as u16;
        encoded.extend_from_slice(&block_len.to_le_bytes());
        encoded.extend_from_slice(&(!block_len).to_le_bytes());
        encoded.extend_from_slice(&data[offset..end]);
        offset = end;
    }
    if data.is_empty() {
        encoded.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    encoded.extend_from_slice(&adler32(data).to_be_bytes());
    encoded
}

fn append_png_chunk(output: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> Result<(), String> {
    let length = u32::try_from(data.len()).map_err(|_| "PNG chunk exceeds format limits")?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&crc32(&kind, data).to_be_bytes());
    Ok(())
}

fn adler32(data: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

fn crc32(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in kind.iter().chain(data) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
