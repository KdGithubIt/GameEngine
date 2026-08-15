use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use engine::event_debug::{
    RuntimeEventTrace, RuntimeEventTraceKind, RUNTIME_EVENT_TRACE_PATH_ENV,
    RUNTIME_EVENT_TRACE_SCHEMA_VERSION,
};

struct RuntimeEventViewer {
    trace: RuntimeEventTrace,
    trace_path: PathBuf,
    trace_path_text: String,
    auto_refresh: bool,
    show_animation: bool,
    show_hits: bool,
    newest_first: bool,
    search: String,
    status: String,
    last_poll: Instant,
    last_modified: Option<SystemTime>,
    editor_process: Option<Child>,
}

impl Default for RuntimeEventViewer {
    fn default() -> Self {
        let trace_path = std::env::temp_dir().join("gameengine-runtime-events.json");
        Self {
            trace: empty_trace(),
            trace_path_text: trace_path.display().to_string(),
            trace_path,
            auto_refresh: true,
            show_animation: true,
            show_hits: true,
            newest_first: true,
            search: String::new(),
            status: "Launch Engine Editor from this viewer to start live capture".to_owned(),
            last_poll: Instant::now(),
            last_modified: None,
            editor_process: None,
        }
    }
}

impl RuntimeEventViewer {
    fn set_trace_path(&mut self, path: PathBuf) {
        self.trace_path_text = path.display().to_string();
        self.trace_path = path;
        self.last_modified = None;
        self.refresh(true);
    }

    fn choose_trace_path(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("GameEngine Runtime Event Trace", &["json"])
            .set_file_name("gameengine-runtime-events.json");
        if let Some(path) = dialog.save_file() {
            self.set_trace_path(path);
        }
    }

    fn open_existing_trace(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("GameEngine Runtime Event Trace", &["json"])
            .pick_file()
        else {
            return;
        };
        self.set_trace_path(path);
    }

    fn launch_editor(&mut self) {
        let editor = sibling_editor_executable();
        match Command::new(&editor)
            .env(RUNTIME_EVENT_TRACE_PATH_ENV, &self.trace_path)
            .spawn()
        {
            Ok(child) => {
                self.editor_process = Some(child);
                self.auto_refresh = true;
                self.status = format!(
                    "Launched {} with live capture to {}",
                    editor.display(),
                    self.trace_path.display()
                );
            }
            Err(error) => {
                self.status = format!("Could not launch {}: {error}", editor.display());
            }
        }
    }

    fn refresh(&mut self, force: bool) {
        let metadata = match fs::metadata(&self.trace_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                if force {
                    self.status =
                        format!("Waiting for trace {}: {error}", self.trace_path.display());
                }
                return;
            }
        };
        let modified = metadata.modified().ok();
        if !force && modified.is_some() && modified == self.last_modified {
            return;
        }
        match fs::read_to_string(&self.trace_path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                RuntimeEventTrace::from_json_str(&json).map_err(|error| error.to_string())
            }) {
            Ok(trace) => {
                self.trace = trace;
                self.last_modified = modified;
                self.status = format!(
                    "Loaded {} events through fixed step {}",
                    self.trace.entries.len(),
                    self.trace.latest_fixed_step
                );
            }
            Err(error) => self.status = format!("Trace refresh failed: {error}"),
        }
    }

    fn delete_trace(&mut self) {
        match fs::remove_file(&self.trace_path) {
            Ok(()) => {
                self.trace = empty_trace();
                self.last_modified = None;
                self.status = format!("Deleted {}", self.trace_path.display());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.trace = empty_trace();
                self.last_modified = None;
                self.status = "Trace was already empty".to_owned();
            }
            Err(error) => self.status = format!("Could not delete trace: {error}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Launch Engine Editor with Live Capture")
                .clicked()
            {
                self.launch_editor();
            }
            if ui.button("Open Trace...").clicked() {
                self.open_existing_trace();
            }
            if ui.button("Choose Capture Path...").clicked() {
                self.choose_trace_path();
            }
            if ui.button("Refresh Now").clicked() {
                self.refresh(true);
            }
            if ui.button("Delete Trace").clicked() {
                self.delete_trace();
            }
            ui.separator();
            ui.checkbox(&mut self.auto_refresh, "Live refresh");
            ui.checkbox(&mut self.newest_first, "Newest first");
        });
        ui.horizontal(|ui| {
            ui.label("Trace path");
            let response = ui.text_edit_singleline(&mut self.trace_path_text);
            if response.lost_focus() && response.changed() {
                self.set_trace_path(PathBuf::from(self.trace_path_text.trim()));
            }
        });
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.show_animation, "Animation Events");
            ui.checkbox(&mut self.show_hits, "Combat Hits");
            ui.separator();
            ui.label("Search");
            ui.text_edit_singleline(&mut self.search);
            ui.separator();
            ui.label(format!("Retained: {}", self.trace.entries.len()));
            ui.label(format!(
                "Latest fixed step: {}",
                self.trace.latest_fixed_step
            ));
        });
    }

    fn timeline(&self, ui: &mut egui::Ui) {
        let search = self.search.to_ascii_lowercase();
        let entries = self
            .trace
            .entries
            .iter()
            .filter(|entry| match &entry.kind {
                RuntimeEventTraceKind::Animation { .. } => self.show_animation,
                RuntimeEventTraceKind::Hit { .. } => self.show_hits,
            })
            .filter(|entry| search.is_empty() || event_search_text(entry).contains(&search));
        let mut entries = entries.collect::<Vec<_>>();
        if self.newest_first {
            entries.reverse();
        }

        egui::ScrollArea::both().show(ui, |ui| {
            egui::Grid::new("runtime_event_timeline")
                .num_columns(7)
                .striped(true)
                .min_col_width(80.0)
                .show(ui, |ui| {
                    ui.strong("Sequence");
                    ui.strong("Fixed Step");
                    ui.strong("Type");
                    ui.strong("Source");
                    ui.strong("Target");
                    ui.strong("Value");
                    ui.strong("Detail");
                    ui.end_row();

                    for entry in entries {
                        ui.monospace(entry.sequence.to_string());
                        ui.monospace(entry.fixed_step.to_string());
                        match &entry.kind {
                            RuntimeEventTraceKind::Animation {
                                entity,
                                name,
                                clip_time,
                            } => {
                                ui.label("Animation");
                                ui.monospace(entity_label(entity.id, entity.generation));
                                ui.label("—");
                                ui.monospace(format!("{clip_time:.3}s"));
                                ui.label(name);
                            }
                            RuntimeEventTraceKind::Hit {
                                attacker,
                                hitbox,
                                target,
                                damage,
                                remaining_health,
                                activation,
                            } => {
                                ui.label("Hit");
                                ui.monospace(entity_label(attacker.id, attacker.generation));
                                ui.monospace(entity_label(target.id, target.generation));
                                ui.monospace(format!("-{damage:.2} HP"));
                                ui.label(format!(
                                    "health {remaining_health:.2}, hitbox {}, activation {activation}",
                                    entity_label(hitbox.id, hitbox.generation)
                                ));
                            }
                        }
                        ui.end_row();
                    }
                });
        });
    }
}

impl eframe::App for RuntimeEventViewer {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(child) = &mut self.editor_process
            && let Ok(Some(status)) = child.try_wait() {
                self.status = format!("Engine Editor exited with {status}");
                self.editor_process = None;
            }
        if self.auto_refresh && self.last_poll.elapsed() >= Duration::from_millis(250) {
            self.last_poll = Instant::now();
            self.refresh(false);
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("runtime_event_toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::top("runtime_event_filters")
            .exact_size(36.0)
            .show_inside(ui, |ui| self.filters(ui));
        egui::Panel::bottom("runtime_event_status")
            .exact_size(28.0)
            .show_inside(ui, |ui| {
                ui.label(&self.status);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| self.timeline(ui));
    }
}

fn empty_trace() -> RuntimeEventTrace {
    RuntimeEventTrace {
        schema_version: RUNTIME_EVENT_TRACE_SCHEMA_VERSION,
        latest_fixed_step: 0,
        entries: Vec::new(),
    }
}

fn sibling_editor_executable() -> PathBuf {
    let filename = if cfg!(windows) {
        "engine-editor.exe"
    } else {
        "engine-editor"
    };
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(filename)
}

fn entity_label(id: u32, generation: u32) -> String {
    format!("{id}v{generation}")
}

fn event_search_text(entry: &engine::event_debug::RuntimeEventTraceEntry) -> String {
    match &entry.kind {
        RuntimeEventTraceKind::Animation {
            entity,
            name,
            clip_time,
        } => format!(
            "animation {} {} {clip_time}",
            entity_label(entity.id, entity.generation),
            name
        )
        .to_ascii_lowercase(),
        RuntimeEventTraceKind::Hit {
            attacker,
            hitbox,
            target,
            damage,
            remaining_health,
            activation,
        } => format!(
            "hit {} {} {} {damage} {remaining_health} {activation}",
            entity_label(attacker.id, attacker.generation),
            entity_label(hitbox.id, hitbox.generation),
            entity_label(target.id, target.generation)
        )
        .to_ascii_lowercase(),
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1380.0, 820.0])
            .with_min_inner_size([980.0, 620.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Runtime Event Timeline",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(RuntimeEventViewer::default()))
        }),
    )
}
