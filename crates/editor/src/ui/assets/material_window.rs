//! Material editor window, material persistence, and texture previews.
//!
//! Material edits are continuous, so writes are debounced through a pending
//! save queue instead of touching the file on every frame.

use crate::ui::*;

impl EditorApp {
    pub(in crate::ui) fn show_material_editor_window(&mut self, context: &egui::Context) {
        self.flush_pending_material_saves(context);
        self.flush_material_scene_preview_refresh(context);
        if !self.show_material_editor {
            return;
        }
        let texture_choices = self.material_texture_choices();
        self.refresh_material_texture_preview(context);
        let mut open = self.show_material_editor;
        let mut changed = false;
        let mut reimport_preview = false;
        egui::Window::new("Material Editor")
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                changed =
                    show_material_editor_panel(
                        &mut self.material_editor,
                        ui,
                        texture_choices.as_slice(),
                    );
                ui.separator();
                ui.heading("Preview");
                if let Some(material) = self.material_editor.active_material() {
                    show_material_preview(ui, material, self.material_texture_preview.as_ref());
                }
                reimport_preview = ui
                    .button("Reimport textures")
                    .on_hover_text("Decode the registered source files again and refresh Scene View diagnostics")
                    .clicked();
            });
        self.show_material_editor = open;
        if changed {
            self.queue_active_material_save(context);
        }
        if !self.show_material_editor {
            self.flush_all_pending_material_saves();
            self.flush_material_scene_preview_refresh(context);
        }
        if reimport_preview {
            self.material_preview_asset = None;
            self.refresh_material_texture_preview(context);
            self.refresh_scene_problems();
            context.request_repaint();
        }
    }

    /// Reuses texture picker choices while neither the project nor manifest
    /// changed, avoiding a project-wide asset scan on every color-drag frame.
    fn material_texture_choices(&mut self) -> Arc<Vec<(AssetId, String)>> {
        let assets_root = self.project_root.as_ref().map(ProjectRoot::assets_root);
        let manifest_revision = self.asset_manifest.revision();
        if let Some(cache) = &self.material_texture_choices_cache
            && cache.manifest_revision == manifest_revision
            && cache.assets_root == assets_root
        {
            return Arc::clone(&cache.choices);
        }
        let choices = Arc::new(
            asset_choices_for_kind(
                engine::AssetKind::Texture,
                &self.asset_manifest,
                assets_root.as_deref(),
            )
            .into_iter()
            .map(|choice| (choice.id, choice.label))
            .collect(),
        );
        self.material_texture_choices_cache = Some(MaterialTextureChoicesCache {
            manifest_revision,
            assets_root,
            choices: Arc::clone(&choices),
        });
        choices
    }

    /// Queues the latest active value and replaces any older value for the
    /// same path while a slider or color picker is still moving.
    pub(in crate::ui) fn queue_active_material_save(&mut self, context: &egui::Context) {
        let Some(relative_path) = self.material_editor.active.clone() else {
            return;
        };
        let Some(material) = self.material_editor.active_material().cloned() else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(120);
        self.pending_material_saves.insert(
            relative_path,
            PendingMaterialSave { material, deadline },
        );
        context.request_repaint_after(std::time::Duration::from_millis(120));
    }

    /// Writes each Material once after its continuous edit reaches a quiet
    /// period, avoiding synchronous file replacement and fsync every frame.
    pub(in crate::ui) fn flush_pending_material_saves(&mut self, context: &egui::Context) {
        let now = std::time::Instant::now();
        let due = self
            .pending_material_saves
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in due {
            if let Some(pending) = self.pending_material_saves.remove(&path) {
                self.persist_material(path, pending.material);
            }
        }
        if let Some(next) = self
            .pending_material_saves
            .values()
            .map(|pending| pending.deadline)
            .min()
        {
            context.request_repaint_after(next.saturating_duration_since(now));
        }
    }

    /// Persists every queued value before closing its editor or replacing the
    /// project that owns its relative path.
    pub(in crate::ui) fn flush_all_pending_material_saves(&mut self) {
        let pending = std::mem::take(&mut self.pending_material_saves);
        for (path, pending) in pending {
            self.persist_material(path, pending.material);
        }
    }

    /// Applies one deferred Scene View rebuild after continuous Material edits
    /// have been quiet long enough to avoid rebuilding on every drag frame.
    fn flush_material_scene_preview_refresh(&mut self, context: &egui::Context) {
        let Some(deadline) = self.material_scene_preview_deadline else {
            return;
        };
        let now = std::time::Instant::now();
        if now < deadline {
            context.request_repaint_after(deadline.saturating_duration_since(now));
            return;
        }
        self.material_scene_preview_deadline = None;
        self.scene_view.invalidate_asset_preview();
        context.request_repaint();
    }

    fn refresh_material_texture_preview(&mut self, context: &egui::Context) {
        let selected = self
            .material_editor
            .active_material()
            .and_then(|material| material.base_color_texture.clone());
        if selected == self.material_preview_asset {
            return;
        }
        self.material_preview_asset = selected.clone();
        self.material_texture_preview = selected.and_then(|asset| {
            let project = self.project_root.as_ref()?;
            let entry = self.asset_manifest.get(&asset)?;
            let path = project.assets_root().join(&entry.path);
            load_texture_preview(context, &path, PathBuf::from(&entry.path)).ok()
        });
    }

    pub(in crate::ui) fn show_texture_preview_window(&mut self, context: &egui::Context) {
        let Some(preview) = &self.texture_preview else {
            return;
        };
        let mut open = true;
        let mut reimport = false;
        egui::Window::new("Texture Preview")
            .open(&mut open)
            .default_width(360.0)
            .show(context, |ui| {
                ui.strong(preview.relative_path.display().to_string());
                ui.label(format!(
                    "{} × {} px",
                    preview.dimensions[0], preview.dimensions[1]
                ));
                let available = ui.available_width().min(320.0);
                ui.add(
                    egui::Image::new((preview.texture.id(), egui::vec2(available, available)))
                        .maintain_aspect_ratio(true),
                );
                reimport = ui.button("Reimport").clicked();
            });
        if !open {
            self.texture_preview = None;
            return;
        }
        if reimport {
            let relative = preview.relative_path.clone();
            let result = self
                .project_root
                .as_ref()
                .map(|project| project.assets_root().join(&relative))
                .ok_or_else(|| "no project is open".to_owned())
                .and_then(|path| load_texture_preview(context, &path, relative));
            match result {
                Ok(preview) => {
                    self.texture_preview = Some(preview);
                    self.material_preview_asset = None;
                    self.refresh_scene_problems();
                    context.request_repaint();
                }
                Err(error) => self
                    .session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.texture_reimport_failed",
                        format!("texture reimport failed: {error}"),
                    )),
            }
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn save_active_material(&mut self) {
        let Some(relative_path) = self.material_editor.active.clone() else {
            return;
        };
        let Some(material) = self.material_editor.active_material().cloned() else {
            return;
        };
        self.persist_material(relative_path, material);
    }

    /// Persists one captured Material value and schedules one preview rebuild.
    fn persist_material(
        &mut self,
        relative_path: PathBuf,
        material: engine_authoring::MaterialAsset,
    ) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let relative = relative_path.to_string_lossy();
        let result = material
            .validate()
            .map_err(|error| error.to_string())
            .and_then(|()| material.to_json().map_err(|error| error.to_string()))
            .and_then(|json| {
                project
                    .resolve_asset_for_write(&relative)
                    .map_err(|error| error.to_string())
                    .and_then(|path| {
                        replace_file_contents(&path, &json).map_err(|error| error.to_string())
                    })
            });
        match result {
            Ok(()) => {
                self.material_scene_preview_deadline = Some(std::time::Instant::now());
                self.refresh_scene_problems();
            }
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.material_save_failed",
                    format!("failed to save {}: {error}", relative_path.display()),
                )),
        }
    }

}

pub(in crate::ui) struct TexturePreview {
    pub(in crate::ui) relative_path: PathBuf,
    pub(in crate::ui) dimensions: [usize; 2],
    pub(in crate::ui) texture: egui::TextureHandle,
}

pub(in crate::ui) fn load_texture_preview(
    context: &egui::Context,
    source: &Path,
    relative_path: PathBuf,
) -> Result<TexturePreview, String> {
    let bytes = fs::read(source).map_err(|error| error.to_string())?;
    let decoded = engine::DecodedTexture::from_bytes(&bytes, source.display().to_string())
        .map_err(|error| error.to_string())?;
    if decoded.width > engine::MAX_TEXTURE_DIMENSION
        || decoded.height > engine::MAX_TEXTURE_DIMENSION
    {
        return Err(format!(
            "{}x{} exceeds the {} px renderer limit",
            decoded.width,
            decoded.height,
            engine::MAX_TEXTURE_DIMENSION
        ));
    }
    let dimensions = [decoded.width as usize, decoded.height as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(dimensions, &decoded.rgba8);
    let texture = context.load_texture(
        format!("asset_preview:{}", relative_path.display()),
        color_image,
        egui::TextureOptions::LINEAR,
    );
    Ok(TexturePreview {
        relative_path,
        dimensions,
        texture,
    })
}

fn show_material_preview(
    ui: &mut egui::Ui,
    material: &engine_authoring::MaterialAsset,
    texture: Option<&TexturePreview>,
) {
    let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let tint = egui::Color32::from_rgba_unmultiplied(
        to_byte(material.base_color.r),
        to_byte(material.base_color.g),
        to_byte(material.base_color.b),
        to_byte(material.base_color.a),
    );
    let size = egui::vec2(180.0, 180.0);
    if let Some(texture) = texture {
        ui.add(egui::Image::new((texture.texture.id(), size)).tint(tint));
        ui.small(format!(
            "Base texture: {} × {} px",
            texture.dimensions[0], texture.dimensions[1]
        ));
    } else {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter().rect_filled(rect, 6.0, tint);
        ui.painter().rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0_f32, egui::Color32::GRAY),
            egui::StrokeKind::Inside,
        );
        ui.small("Base color (no base texture)");
    }
    ui.small(format!(
        "roughness {:.2}  metallic {:.2}  {:?} / {:?} / {:?}",
        material.roughness,
        material.metallic,
        material.alpha_mode,
        material.cull_mode,
        material.shading_model
    ));
}
