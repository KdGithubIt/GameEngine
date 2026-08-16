use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

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
    let file = File::create(path).map_err(|error| error.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    let rgba = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect::<Vec<_>>();
    writer
        .write_image_data(&rgba)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())
}
