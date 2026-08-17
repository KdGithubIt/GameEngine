//! GameEngine Launcher: the project picker that owns Editor process lifecycle.

mod fonts;
mod icon;
mod theme;
#[cfg(feature = "visual-validation")]
mod visual_capture;

use eframe::egui;
use engine_launcher::LauncherPreferences;
#[cfg(test)]
use engine_launcher::MAX_RECENT_PROJECTS;
use engine_project_lifecycle::{
    acquire_launcher, create_standard_project, editor_is_ready, editor_owner_metadata,
    inspect_project, launch_or_activate_editor, request_editor_close, EditorLaunchOutcome,
    LauncherRequest, LauncherSession, CURRENT_ENGINE_ASSOCIATION,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Height of the branded header band.
const HERO_HEIGHT: f32 = 132.0;
/// Width of the fixed action column beside the recent-project list.
const ACTIONS_WIDTH: f32 = 300.0;
/// Padding inside one recent-project row.
const ROW_PADDING: egui::Vec2 = egui::vec2(12.0, 9.0);
/// How long resolved recent-project data stays valid before disk is re-read.
///
/// Project identity and Editor ownership are owned by other processes, so the
/// list would otherwise keep showing state from the moment the window opened.
const RECENT_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);

/// How prominently a status line should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLevel {
    /// Progress or neutral information.
    Info,
    /// A request that completed as asked.
    Success,
    /// A request that could not be completed.
    Failure,
}

impl StatusLevel {
    fn color(self) -> egui::Color32 {
        match self {
            Self::Info => theme::TEXT_MUTED,
            Self::Success => theme::SUCCESS,
            Self::Failure => theme::DANGER,
        }
    }
}

/// The latest Launcher outcome, shown in the footer until it is replaced.
struct StatusMessage {
    level: StatusLevel,
    text: String,
}

impl StatusMessage {
    fn info(text: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Info,
            text: text.into(),
        }
    }

    fn success(text: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Success,
            text: text.into(),
        }
    }

    fn failure(text: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Failure,
            text: text.into(),
        }
    }
}

/// What the Launcher knows about a recent project's current state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectAvailability {
    /// The project opens cleanly and no Editor process holds it.
    Ready,
    /// An Editor process currently owns the project.
    OpenInEditor,
    /// The project cannot be opened; the message explains why.
    Unavailable(String),
}

impl ProjectAvailability {
    /// Returns the dot color that summarizes this state.
    fn color(&self) -> egui::Color32 {
        match self {
            Self::Ready => theme::TEXT_MUTED,
            Self::OpenInEditor => theme::SUCCESS,
            Self::Unavailable(_) => theme::WARNING,
        }
    }

    /// Returns the trailing row label, for states worth naming.
    fn badge(&self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::OpenInEditor => Some("Editor running"),
            Self::Unavailable(_) => Some("Unavailable"),
        }
    }

    /// Returns the explanation appended to the row's hover text.
    fn detail(&self) -> Option<&str> {
        match self {
            Self::Ready | Self::OpenInEditor => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

/// One resolved row of the recent-project list.
#[derive(Debug, Clone)]
struct RecentProject {
    path: PathBuf,
    /// The `name` from `project.json`, or the folder name when it is unreadable.
    name: String,
    availability: ProjectAvailability,
}

impl RecentProject {
    /// Reads the current on-disk state of one remembered project location.
    fn resolve(path: &Path) -> Self {
        let folder_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let (name, availability) = match inspect_project(path) {
            Ok(project) => {
                let configured = project.config().name.trim().to_owned();
                let availability = match editor_owner_metadata(path) {
                    Ok(Some(_)) => ProjectAvailability::OpenInEditor,
                    Ok(None) => ProjectAvailability::Ready,
                    Err(error) => ProjectAvailability::Unavailable(error.to_string()),
                };
                let name = if configured.is_empty() {
                    folder_name
                } else {
                    configured
                };
                (name, availability)
            }
            Err(error) => (
                folder_name,
                ProjectAvailability::Unavailable(error.to_string()),
            ),
        };
        Self {
            path: path.to_path_buf(),
            name,
            availability,
        }
    }
}

/// Presentation data for the recent-project list, refreshed on an interval.
#[derive(Default)]
struct RecentProjectList {
    entries: Vec<RecentProject>,
    resolved_at: Option<Instant>,
}

impl RecentProjectList {
    /// Re-reads disk state when the cache aged out or no longer matches
    /// `paths`, which happens as soon as a project is opened or forgotten.
    fn refresh_if_stale(&mut self, paths: &[PathBuf]) {
        let is_aged = self
            .resolved_at
            .is_none_or(|resolved_at| resolved_at.elapsed() >= RECENT_REFRESH_INTERVAL);
        let matches_paths = self.entries.len() == paths.len()
            && self
                .entries
                .iter()
                .zip(paths)
                .all(|(entry, path)| &entry.path == path);
        if is_aged || !matches_paths {
            self.entries = paths.iter().map(|path| RecentProject::resolve(path)).collect();
            self.resolved_at = Some(Instant::now());
        }
    }
}

/// What a recent-project row asked the Launcher to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowAction {
    None,
    Open,
    Forget,
}

struct PendingSwitch {
    source: PathBuf,
    target: PathBuf,
}

struct LauncherApp {
    session: LauncherSession,
    preferences: LauncherPreferences,
    recent: RecentProjectList,
    new_project_name: String,
    status: Option<StatusMessage>,
    switch_from: Option<PathBuf>,
    pending_switch: Option<PendingSwitch>,
    #[cfg(feature = "visual-validation")]
    visual_capture: visual_capture::VisualCapture,
}

impl LauncherApp {
    fn new(session: LauncherSession) -> Self {
        Self {
            session,
            preferences: LauncherPreferences::load(),
            recent: RecentProjectList::default(),
            new_project_name: "NewGame".to_owned(),
            status: None,
            switch_from: None,
            pending_switch: None,
            #[cfg(feature = "visual-validation")]
            visual_capture: visual_capture::VisualCapture::from_environment(),
        }
    }

    fn apply_request(&mut self, request: LauncherRequest, context: &egui::Context) {
        self.switch_from = request.switch_from;
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn open_project(&mut self, path: PathBuf) {
        match launch_or_activate_editor(&path) {
            Ok(launch) => {
                self.preferences.push_recent(&launch.canonical_project);
                let verb = match launch.outcome {
                    EditorLaunchOutcome::Spawned => "Started",
                    EditorLaunchOutcome::Activated => "Activated",
                };
                self.status = Some(StatusMessage::success(format!(
                    "{verb} Editor for {}",
                    launch.canonical_project.display()
                )));

                if let Some(source) = self.switch_from.take()
                    && source != launch.canonical_project
                {
                    self.pending_switch = Some(PendingSwitch {
                        source,
                        target: launch.canonical_project,
                    });
                }
            }
            Err(error) => {
                self.status = Some(StatusMessage::failure(format!(
                    "Could not open project: {error}"
                )));
            }
        }
    }

    fn create_project(&mut self) {
        let name = self.new_project_name.trim();
        if name.is_empty() {
            self.status = Some(StatusMessage::failure("Enter a project name first."));
            return;
        }
        let mut dialog = rfd::FileDialog::new();
        if let Some(parent) = self.preferences.new_project_parent.as_deref()
            && parent.is_dir()
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(parent) = dialog.pick_folder() else {
            return;
        };
        self.preferences.remember_new_project_parent(&parent);
        let final_path = parent.join(name);
        match create_standard_project(&final_path, name) {
            Ok(project) => {
                let path = project.path().to_path_buf();
                self.preferences.push_recent(&path);
                self.status = Some(StatusMessage::success(format!("Created {}", path.display())));
                self.open_project(path);
            }
            Err(error) => {
                self.status = Some(StatusMessage::failure(format!(
                    "Could not create project: {error}"
                )));
            }
        }
    }

    fn poll_switch(&mut self, context: &egui::Context) {
        let Some(pending) = self.pending_switch.as_ref() else {
            return;
        };
        context.request_repaint_after(std::time::Duration::from_millis(100));
        match editor_is_ready(&pending.target) {
            Ok(true) => {
                let source = pending.source.clone();
                let target = pending.target.clone();
                self.pending_switch = None;
                match request_editor_close(&source) {
                    Ok(()) => {
                        self.status = Some(StatusMessage::success(format!(
                            "Switched from {} to {}",
                            source.display(),
                            target.display()
                        )));
                    }
                    Err(error) => {
                        self.status = Some(StatusMessage::failure(format!(
                            "Target Editor is ready, but source close failed: {error}"
                        )));
                    }
                }
            }
            Ok(false) => {}
            Err(error) => {
                self.pending_switch = None;
                self.status = Some(StatusMessage::failure(format!(
                    "Target Editor readiness check failed: {error}"
                )));
            }
        }
    }

    /// Draws the branded header band.
    fn hero(&self, ui: &mut egui::Ui) {
        let band = ui.max_rect();
        theme::paint_hero_background(ui.painter(), band);
        ui.allocate_ui_with_layout(
            band.size(),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(28.0);
                let (mark, _) = ui.allocate_exact_size(egui::vec2(62.0, 62.0), egui::Sense::hover());
                theme::paint_engine_mark(ui.painter(), mark.center(), 28.0);
                ui.add_space(20.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("GameEngine")
                            .size(30.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(
                            egui::RichText::new("Launcher")
                                .strong()
                                .color(theme::ACCENT_TEXT),
                        );
                        ui.label(egui::RichText::new("·").color(theme::TEXT_MUTED));
                        ui.label(
                            egui::RichText::new(format!("Engine {CURRENT_ENGINE_ASSOCIATION}"))
                                .color(theme::TEXT_MUTED),
                        );
                    });
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Open a project in the Editor, or scaffold a new one.",
                        )
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                });
            },
        );
    }

    /// Draws the fixed column holding the create and open actions.
    fn actions_column(&mut self, ui: &mut egui::Ui) {
        theme::card_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::section_caption(ui, "New project");
            theme::section_rule(ui);
            ui.add(
                egui::TextEdit::singleline(&mut self.new_project_name)
                    .hint_text("Project name")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(8, 6)),
            );
            ui.add_space(2.0);
            if accent_button(ui, "Create project").clicked() {
                self.create_project();
            }
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("The parent folder is chosen after Create.")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        });

        ui.add_space(12.0);

        theme::card_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            theme::section_caption(ui, "Open project");
            theme::section_rule(ui);
            let open = ui.add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new("Open project folder…"),
            );
            if open.clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.open_project(path);
            }
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Pick any folder that contains project.json.")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        });
    }

    /// Draws the recent-project list and applies the row the user activated.
    fn recent_column(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            theme::section_caption(ui, "Recent projects");
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.label(
                        egui::RichText::new(format!("{}", self.recent.entries.len()))
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                },
            );
        });
        theme::section_rule(ui);

        if self.recent.entries.is_empty() {
            empty_recent_list(ui);
            return;
        }

        let mut activated = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, project) in self.recent.entries.iter().enumerate() {
                    match recent_project_row(ui, index, project) {
                        RowAction::None => {}
                        action => activated = Some((action, project.path.clone())),
                    }
                    ui.add_space(8.0);
                }
            });

        match activated {
            Some((RowAction::Open, path)) => self.open_project(path),
            Some((RowAction::Forget, path)) => {
                self.preferences.remove_recent(&path);
                self.status = Some(StatusMessage::info(format!(
                    "Removed {} from recent projects",
                    path.display()
                )));
            }
            Some((RowAction::None, _)) | None => {}
        }
    }

    /// Draws the pending-switch banner, when a switch is in progress.
    fn switch_banner(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.switch_from.clone() else {
            return;
        };
        egui::Frame::NONE
            .fill(egui::Color32::from_rgb(33, 44, 61))
            .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT))
            .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Project switch")
                            .strong()
                            .color(theme::ACCENT_TEXT),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.button("Cancel").clicked() {
                                self.switch_from = None;
                                self.pending_switch = None;
                            }
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} stays open until the new project is ready.",
                                    source.display()
                                ))
                                .small()
                                .color(theme::TEXT_MUTED),
                            );
                        },
                    );
                });
            });
        ui.add_space(8.0);
    }

    /// Draws the footer status line.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // The bottom panel shares the card fill, so a hairline marks where the
        // content area ends.
        let bar = ui.max_rect().expand2(egui::vec2(18.0, 10.0));
        ui.painter().hline(
            bar.x_range(),
            bar.top() + 0.5,
            egui::Stroke::new(1.0_f32, theme::BORDER),
        );

        self.switch_banner(ui);

        ui.horizontal(|ui| {
            match &self.status {
                Some(status) => {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot.center(), 4.0, status.level.color());
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    ui.label(egui::RichText::new(&status.text).color(status.level.color()))
                        .on_hover_text(&status.text);
                }
                None => {
                    ui.label(
                        egui::RichText::new("Ready.")
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                }
            }

            if let Some(pending) = &self.pending_switch {
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(format!(
                                "Waiting for {}…",
                                display_name(&pending.target)
                            ))
                            .small()
                            .color(theme::TEXT_MUTED),
                        );
                    },
                );
            }
        });
    }
}

impl eframe::App for LauncherApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(request) = self.session.take_request() {
            self.apply_request(request, context);
        }
        self.poll_switch(context);
        // Editor ownership is written by other processes, so the row badges
        // only stay honest if the window wakes up without user input.
        context.request_repaint_after(RECENT_REFRESH_INTERVAL);
        #[cfg(feature = "visual-validation")]
        self.visual_capture.update(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.recent
            .refresh_if_stale(&self.preferences.recent_projects);

        egui::Panel::top("launcher_hero")
            .exact_size(HERO_HEIGHT)
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.hero(ui));

        egui::Panel::bottom("launcher_status")
            .frame(
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(18, 10)),
            )
            .show_inside(ui, |ui| self.status_bar(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::BACKGROUND)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show_inside(ui, |ui| {
                egui::Panel::left("launcher_actions")
                    .exact_size(ACTIONS_WIDTH)
                    .resizable(false)
                    .frame(egui::Frame::NONE)
                    .show_inside(ui, |ui| self.actions_column(ui));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                        left: 18,
                        ..egui::Margin::ZERO
                    }))
                    .show_inside(ui, |ui| self.recent_column(ui));
            });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BACKGROUND.to_normalized_gamma_f32()
    }
}

/// Adds a filled primary-action button that spans the available width.
fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = theme::ACCENT;
            widgets.inactive.bg_stroke = egui::Stroke::NONE;
            widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
            widgets.hovered.weak_bg_fill = theme::ACCENT_HOVERED;
            widgets.hovered.bg_stroke = egui::Stroke::NONE;
            widgets.active.weak_bg_fill = theme::ACCENT_TEXT;
            widgets.active.bg_stroke = egui::Stroke::NONE;
            widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, theme::BACKGROUND);
        }
        ui.add_sized(
            [ui.available_width(), 34.0],
            egui::Button::new(egui::RichText::new(text).strong()),
        )
    })
    .inner
}

/// Draws the placeholder shown before any project has been opened.
fn empty_recent_list(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.add_space(22.0);
            theme::paint_engine_mark(
                ui.painter(),
                ui.cursor().center_top() + egui::vec2(0.0, 20.0),
                18.0,
            );
            ui.add_space(48.0);
            ui.label(
                egui::RichText::new("No projects opened yet")
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Projects appear here after you create or open one.")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(22.0);
        });
    });
}

/// Draws one recent-project row and reports what the user asked for.
fn recent_project_row(ui: &mut egui::Ui, index: usize, project: &RecentProject) -> RowAction {
    let mut action = RowAction::None;
    let row = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id_salt(("recent_project", index))
                .sense(egui::Sense::click()),
            |ui| {
                let state = ui.response();
                // The background spans the finished row, so its shape is
                // reserved now and written once the height is known.
                let background = ui.painter().add(egui::Shape::Noop);
                ui.set_min_width(ui.available_width());
                ui.add_space(ROW_PADDING.y);
                ui.horizontal(|ui| {
                    ui.add_space(ROW_PADDING.x);
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot.center(), 4.0, project.availability.color());
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(
                            egui::RichText::new(&project.name)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            egui::RichText::new(project.path.display().to_string())
                                .small()
                                .color(theme::TEXT_MUTED),
                        );
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.add_space(ROW_PADDING.x - 6.0);
                            let forget = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("×")
                                            .size(17.0)
                                            .color(theme::TEXT_MUTED),
                                    )
                                    .frame(false)
                                    .min_size(egui::vec2(22.0, 22.0)),
                                )
                                .on_hover_text("Remove from recent projects");
                            if forget.clicked() {
                                action = RowAction::Forget;
                            }
                            if let Some(badge) = project.availability.badge() {
                                ui.label(
                                    egui::RichText::new(badge)
                                        .small()
                                        .color(project.availability.color()),
                                );
                            }
                        },
                    );
                });
                ui.add_space(ROW_PADDING.y);
                ui.painter()
                    .set(background, row_background(ui.min_rect(), &state));
            },
        )
        .response;

    let mut hover = project.path.display().to_string();
    if let Some(detail) = project.availability.detail() {
        hover.push('\n');
        hover.push_str(detail);
    }
    let row = row
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover);
    if action == RowAction::None && row.clicked() {
        action = RowAction::Open;
    }
    action
}

/// Returns the card shape drawn behind a recent-project row.
fn row_background(rect: egui::Rect, state: &egui::Response) -> egui::Shape {
    let (fill, stroke) = if state.is_pointer_button_down_on() {
        (theme::SURFACE_ACTIVE, theme::ACCENT_TEXT)
    } else if state.hovered() {
        (theme::SURFACE_HOVERED, theme::ACCENT)
    } else {
        (theme::SURFACE, theme::BORDER)
    };
    egui::Shape::Rect(egui::epaint::RectShape::new(
        rect,
        egui::CornerRadius::same(theme::CORNER_RADIUS + 2),
        fill,
        egui::Stroke::new(1.0_f32, stroke),
        egui::StrokeKind::Inside,
    ))
}

/// Returns the folder name of `path`, falling back to the whole path.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn parse_switch_from() -> Result<Option<PathBuf>, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut switch_from = None;
    while let Some(argument) = arguments.next() {
        if argument == "--switch-from" {
            let path = arguments
                .next()
                .ok_or_else(|| "--switch-from requires a project path".to_owned())?;
            switch_from = Some(PathBuf::from(path));
        } else {
            return Err(format!(
                "unknown Launcher argument `{}`",
                argument.to_string_lossy()
            ));
        }
    }
    Ok(switch_from)
}

fn run() -> Result<(), String> {
    let switch_from = parse_switch_from()?;
    let Some(session) = acquire_launcher(switch_from).map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([940.0, 660.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(icon::launcher_icon()),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Launcher",
        options,
        Box::new(move |creation_context| {
            fonts::install_launcher_fonts(&creation_context.egui_ctx);
            theme::apply_launcher_style(&creation_context.egui_ctx);
            Ok(Box::new(LauncherApp::new(session)))
        }),
    )
    .map_err(|error| error.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[launcher.startup_failed] {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Preference files written before the project-parent setting existed must
    /// remain readable after the setting is introduced.
    #[test]
    fn legacy_preferences_without_new_project_parent_remain_readable() {
        let preferences: LauncherPreferences =
            serde_json::from_str(r#"{"recent_projects":[]}"#).expect("legacy preferences parse");

        assert!(preferences.new_project_parent.is_none());
    }

    /// The selected parent is persisted as Launcher user state so a later
    /// create dialog can start from the same location.
    #[test]
    fn new_project_parent_round_trips_through_preferences_json() {
        let expected = PathBuf::from("D:/GameProjects");
        let preferences = LauncherPreferences {
            new_project_parent: Some(expected.clone()),
            ..LauncherPreferences::default()
        };

        let json = serde_json::to_string(&preferences).expect("serialize preferences");
        let decoded: LauncherPreferences =
            serde_json::from_str(&json).expect("deserialize preferences");

        assert_eq!(decoded.new_project_parent, Some(expected));
    }

    /// Reopening a remembered project must move it to the front instead of
    /// adding a second row for the same location.
    #[test]
    fn recording_a_known_project_moves_it_to_the_front_without_duplicating_it() {
        let mut preferences = LauncherPreferences::default();
        preferences.record_recent(Path::new("/projects/alpha"));
        preferences.record_recent(Path::new("/projects/beta"));
        preferences.record_recent(Path::new("/projects/alpha"));

        assert_eq!(
            preferences.recent_projects,
            vec![
                PathBuf::from("/projects/alpha"),
                PathBuf::from("/projects/beta"),
            ]
        );
    }

    /// The list is capped so the column cannot grow without bound.
    #[test]
    fn recording_more_projects_than_the_limit_drops_the_oldest() {
        let mut preferences = LauncherPreferences::default();
        for index in 0..MAX_RECENT_PROJECTS + 3 {
            preferences.record_recent(&PathBuf::from(format!("/projects/{index}")));
        }

        assert_eq!(preferences.recent_projects.len(), MAX_RECENT_PROJECTS);
        assert_eq!(
            preferences.recent_projects.first(),
            Some(&PathBuf::from(format!(
                "/projects/{}",
                MAX_RECENT_PROJECTS + 2
            )))
        );
    }

    /// Removing one row must not disturb the order of the remaining rows.
    #[test]
    fn forgetting_a_project_leaves_the_other_entries_in_order() {
        let mut preferences = LauncherPreferences::default();
        preferences.record_recent(Path::new("/projects/alpha"));
        preferences.record_recent(Path::new("/projects/beta"));
        preferences.record_recent(Path::new("/projects/gamma"));

        preferences.forget_recent(Path::new("/projects/beta"));

        assert_eq!(
            preferences.recent_projects,
            vec![
                PathBuf::from("/projects/gamma"),
                PathBuf::from("/projects/alpha"),
            ]
        );
    }

    /// A list resolved from an empty set of paths must be rebuilt as soon as a
    /// project is remembered, without waiting for the refresh interval.
    #[test]
    fn a_changed_path_list_refreshes_immediately() {
        let mut recent = RecentProjectList::default();
        recent.refresh_if_stale(&[]);
        let resolved_at = recent.resolved_at;

        recent.refresh_if_stale(&[PathBuf::from("/projects/alpha")]);

        assert_eq!(recent.entries.len(), 1);
        assert_eq!(recent.entries[0].path, PathBuf::from("/projects/alpha"));
        assert!(recent.resolved_at > resolved_at);
    }
}
