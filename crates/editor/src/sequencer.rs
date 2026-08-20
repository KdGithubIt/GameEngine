//! Timeline / Sequencer authoring workspace (ADR 0126 §10).
//!
//! Every persisted edit here goes through the shared typed-document service, so
//! the Editor, CLI, and MCP apply the same validation and the same atomic
//! replacement semantics. Drawing code selects presentation only; it never
//! defines timeline semantics, which belong to the neutral runtime core.

use eframe::egui;
use engine_authoring::{
    AuthoringPermission, AuthoringPermissions, DisplayFrameRate, ProjectRoot, TimelineClip,
    TimelineClipId, TimelineClipPayload, TimelineDocument, TimelineMarker, TimelineMarkerId,
    TimelineProperty, TimelineTick, TimelineTrack, TimelineTrackId, TimelineTrackKind,
    replace_file_contents,
};
use engine_timeline::{
    CompiledTimeline, LoopRegion, TimelineEvaluation, TimelinePlayState, TimelinePlayer,
    TimelineSeek, TrackRegistry, compile_timeline,
};
use std::path::{Path, PathBuf};

mod curve_editor;

/// Ticks one Step control moves the playhead when no frame rate applies.
const STEP_FALLBACK_TICKS: i64 = 480;

/// One header control the user activated this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderAction {
    None,
    Save,
    Undo,
    Redo,
    Close,
}

/// One track control the user activated this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackAction {
    None,
    AddClip,
    ToggleEnabled,
    Delete,
}

/// Editor-owned Sequencer state.
pub(crate) struct SequencerState {
    open: Option<LoadedTimeline>,
    selected_track: usize,
    selected_clip: Option<TimelineClipId>,
    snap_to_frames: bool,
    preview_events: bool,
    registry: TrackRegistry,
    new_track_kind: TimelineTrackKind,
    status: Option<String>,
}

impl Default for SequencerState {
    fn default() -> Self {
        Self {
            open: None,
            selected_track: 0,
            selected_clip: None,
            snap_to_frames: true,
            preview_events: false,
            registry: TrackRegistry::default(),
            new_track_kind: TimelineTrackKind::Property,
            status: None,
        }
    }
}

struct LoadedTimeline {
    relative: PathBuf,
    path: PathBuf,
    document: TimelineDocument,
    undo: Vec<TimelineDocument>,
    redo: Vec<TimelineDocument>,
    dirty: bool,
    compiled: CompiledTimeline,
    player: TimelinePlayer,
    /// Ticks the last preview evaluation reported as its playhead.
    preview_tick: TimelineTick,
    /// One seek evaluation waiting for the Scene View preview bridge.
    ///
    /// Keeping this one-shot result preserves explicit Preview Events semantics;
    /// recomputing a paused sample on the next frame would correctly sample the
    /// pose but would discard the event the user deliberately opted into.
    pending_preview: Option<TimelineEvaluation>,
    /// Diagnostics from the most recent compile or validation.
    diagnostics: Vec<String>,
}

impl LoadedTimeline {
    fn open(project: &ProjectRoot, relative: &Path) -> Result<Self, String> {
        let relative_text = relative
            .to_str()
            .ok_or_else(|| "Timeline path contains non-UTF-8 characters".to_owned())?;
        let path = project
            .resolve_asset(relative_text)
            .map_err(|error| error.to_string())?;
        let json = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let document = TimelineDocument::from_json(&json).map_err(|error| error.to_string())?;
        let compiled = compile_timeline(&document).map_err(|error| error.to_string())?;
        Ok(Self {
            relative: relative.to_path_buf(),
            path,
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            compiled,
            player: TimelinePlayer::new(),
            preview_tick: TimelineTick::ZERO,
            pending_preview: None,
            diagnostics: Vec::new(),
        })
    }

    /// Applies one edit atomically, keeping the document valid or unchanged.
    ///
    /// A rejected edit leaves the previous document in place, which is the same
    /// contract the typed-document service gives CLI and MCP callers.
    fn edit(
        &mut self,
        permissions: &AuthoringPermissions,
        mutate: impl FnOnce(&mut TimelineDocument),
    ) -> Result<(), String> {
        if !permissions.contains(AuthoringPermission::ProjectDataWrite) {
            return Err("this session cannot write project data".to_owned());
        }
        let mut candidate = self.document.clone();
        mutate(&mut candidate);
        let errors = candidate.validate();
        if !errors.is_empty() {
            self.diagnostics = errors.clone();
            return Err(errors.join("; "));
        }
        let compiled = compile_timeline(&candidate).map_err(|error| error.to_string())?;
        self.undo
            .push(std::mem::replace(&mut self.document, candidate));
        if self.undo.len() > 64 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.compiled = compiled;
        self.pending_preview = None;
        if self.player.loop_region().is_some() {
            self.set_loop_enabled(true);
        }
        self.diagnostics.clear();
        self.dirty = true;
        Ok(())
    }

    fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        match compile_timeline(&previous) {
            Ok(compiled) => {
                let current = std::mem::replace(&mut self.document, previous);
                self.redo.push(current);
                if self.redo.len() > 64 {
                    self.redo.remove(0);
                }
                self.compiled = compiled;
                self.pending_preview = None;
                if self.player.loop_region().is_some() {
                    self.set_loop_enabled(true);
                }
                self.dirty = true;
                true
            }
            Err(_) => {
                self.undo.push(previous);
                false
            }
        }
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        match compile_timeline(&next) {
            Ok(compiled) => {
                let current = std::mem::replace(&mut self.document, next);
                self.undo.push(current);
                if self.undo.len() > 64 {
                    self.undo.remove(0);
                }
                self.compiled = compiled;
                self.pending_preview = None;
                if self.player.loop_region().is_some() {
                    self.set_loop_enabled(true);
                }
                self.dirty = true;
                true
            }
            Err(_) => {
                self.redo.push(next);
                false
            }
        }
    }

    fn save(&mut self) -> Result<(), String> {
        let json = self
            .document
            .to_canonical_json()
            .map_err(|error| error.to_string())?;
        replace_file_contents(&self.path, &json).map_err(|error| error.to_string())?;
        self.dirty = false;
        Ok(())
    }

    fn seek(&mut self, tick: TimelineTick, preview_events: bool) {
        let mode = if preview_events {
            TimelineSeek::PreviewEvents
        } else {
            TimelineSeek::Scrub
        };
        let evaluation = self.player.seek(&self.compiled, tick, mode);
        self.preview_tick = evaluation.tick;
        self.pending_preview = Some(evaluation);
    }

    fn set_loop_enabled(&mut self, enabled: bool) {
        let region = enabled.then_some(LoopRegion {
            start: TimelineTick::ZERO,
            end: self.document.duration,
            count: None,
        });
        let _ = self.player.set_loop_region(region);
    }

    fn advance_preview(&mut self, delta_seconds: f32) -> TimelineEvaluation {
        let evaluation = self
            .pending_preview
            .take()
            .unwrap_or_else(|| self.player.advance(&self.compiled, delta_seconds));
        self.preview_tick = evaluation.tick;
        evaluation
    }
}

impl SequencerState {
    /// Loads a deterministic Timeline for visual validation.
    ///
    /// Opening the workspace with no document proves only that the window
    /// starts. Reviewing tracks, clips, markers, and the playhead needs a
    /// document, and this fixture is compiled only for visual validation so no
    /// normal Editor launch can reach it.
    #[cfg(feature = "visual-validation")]
    pub(crate) fn prepare_visual_validation(
        &mut self,
        subject: Option<engine_authoring::EntityId>,
    ) {
        use engine_authoring::{
            EntityId, TimelineAudioAction, TimelineBinding, TimelineInterpolation, TimelineKey,
        };

        let camera = EntityId::generate();
        let subject = subject.unwrap_or_else(EntityId::generate);
        let mut document = TimelineDocument::new(TimelineTick::from_seconds(6.0));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::CameraCut,
            name: "Camera".to_owned(),
            enabled: true,
            binding: TimelineBinding::default(),
            clips: vec![
                TimelineClip {
                    id: TimelineClipId::generate(),
                    start: TimelineTick::ZERO,
                    end: TimelineTick::from_seconds(2.5),
                    payload: TimelineClipPayload::CameraCut {
                        camera: camera.clone(),
                    },
                },
                TimelineClip {
                    id: TimelineClipId::generate(),
                    start: TimelineTick::from_seconds(3.0),
                    end: TimelineTick::from_seconds(5.5),
                    payload: TimelineClipPayload::CameraCut { camera },
                },
            ],
        });
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Property,
            name: "Subject position".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(subject),
                asset: None,
            },
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::from_seconds(0.5),
                end: TimelineTick::from_seconds(4.0),
                payload: TimelineClipPayload::Property {
                    property: TimelineProperty::TranslationX,
                    keys: vec![
                        TimelineKey {
                            tick: TimelineTick::ZERO,
                            value: 0.0,
                            interpolation: TimelineInterpolation::Smooth,
                        },
                        TimelineKey {
                            tick: TimelineTick::from_seconds(3.5),
                            value: 3.0,
                            interpolation: TimelineInterpolation::Linear,
                        },
                    ],
                },
            }],
        });
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Event,
            name: "Sequence events".to_owned(),
            enabled: true,
            binding: TimelineBinding::default(),
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::from_seconds(2.0),
                end: TimelineTick::from_seconds(2.4),
                payload: TimelineClipPayload::Event {
                    event: "cutscene.door_opens".to_owned(),
                },
            }],
        });
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Audio,
            name: "Music".to_owned(),
            enabled: false,
            binding: TimelineBinding::default(),
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick::from_seconds(6.0),
                payload: TimelineClipPayload::Audio {
                    cue: engine_authoring::AssetId::generate(),
                    action: TimelineAudioAction::Play,
                    fade_ticks: TimelineTick::from_seconds(0.5),
                },
            }],
        });
        document.markers.push(TimelineMarker {
            id: TimelineMarkerId::generate(),
            tick: TimelineTick::from_seconds(2.0),
            name: "Door".to_owned(),
            event: "cutscene.door_opens".to_owned(),
        });
        document.markers.push(TimelineMarker {
            id: TimelineMarkerId::generate(),
            tick: TimelineTick::from_seconds(4.5),
            name: "Reveal".to_owned(),
            event: "cutscene.reveal".to_owned(),
        });

        let Ok(compiled) = compile_timeline(&document) else {
            self.status = Some("Timeline visual fixture failed to compile".to_owned());
            return;
        };
        let mut loaded = LoadedTimeline {
            relative: PathBuf::from("assets/cutscenes/intro.timeline.json"),
            path: PathBuf::from("assets/cutscenes/intro.timeline.json"),
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            compiled,
            player: TimelinePlayer::new(),
            preview_tick: TimelineTick::ZERO,
            pending_preview: None,
            diagnostics: Vec::new(),
        };
        loaded.set_loop_enabled(true);
        loaded.player.pause();
        loaded.seek(TimelineTick::from_seconds(2.0), false);
        self.selected_track = 1;
        self.selected_clip = loaded.document.tracks[1]
            .clips
            .first()
            .map(|clip| clip.id.clone());
        self.status = Some(
            "Scene View preview · Paused · Loop · 2.000s; gameplay events stay suppressed."
                .to_owned(),
        );
        self.open = Some(loaded);
    }

    /// Opens one Timeline document for editing.
    pub(crate) fn open_document(&mut self, project: &ProjectRoot, relative: &Path) {
        match LoadedTimeline::open(project, relative) {
            Ok(loaded) => {
                self.selected_track = 0;
                self.selected_clip = None;
                self.status = Some(format!("Opened {}", relative.display()));
                self.open = Some(loaded);
            }
            Err(error) => {
                self.status = Some(format!("Timeline open failed: {error}"));
                self.open = None;
            }
        }
    }

    /// Advances the Editor-owned preview clock and returns one runtime evaluation.
    pub(crate) fn advance_preview(&mut self, delta_seconds: f32) -> Option<TimelineEvaluation> {
        self.open
            .as_mut()
            .map(|loaded| loaded.advance_preview(delta_seconds.max(0.0)))
    }

    /// Returns whether the preview clock needs continuous Editor repaints.
    pub(crate) fn preview_is_playing(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|loaded| loaded.player.state() == TimelinePlayState::Playing)
    }

    /// Draws the Sequencer workspace.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, permissions: &AuthoringPermissions) {
        if self.open.is_none() {
            ui.label("Open a *.timeline.json asset to edit a sequence.");
            return;
        }
        // Header controls collect one action and apply it after drawing, so no
        // control mutates the open document while the header still borrows it.
        let mut action = HeaderAction::None;
        if let Some(loaded) = self.open.as_ref() {
            let title = loaded.relative.display().to_string();
            let dirty = loaded.dirty;
            let can_undo = !loaded.undo.is_empty();
            let can_redo = !loaded.redo.is_empty();
            ui.horizontal_wrapped(|ui| {
                ui.strong(title);
                if dirty {
                    ui.label("(unsaved)");
                }
                if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                    action = HeaderAction::Save;
                }
                if ui
                    .add_enabled(can_undo, egui::Button::new("Undo"))
                    .clicked()
                {
                    action = HeaderAction::Undo;
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("Redo"))
                    .clicked()
                {
                    action = HeaderAction::Redo;
                }
                if ui.button("Close").clicked() {
                    action = HeaderAction::Close;
                }
            });
        }
        match action {
            HeaderAction::None => {}
            HeaderAction::Save => {
                if let Some(loaded) = self.open.as_mut() {
                    self.status = Some(match loaded.save() {
                        Ok(()) => "Saved canonical Timeline".to_owned(),
                        Err(error) => format!("Timeline save failed: {error}"),
                    });
                }
            }
            HeaderAction::Undo => {
                if let Some(loaded) = self.open.as_mut()
                    && !loaded.undo()
                {
                    self.status = Some("Nothing to undo".to_owned());
                }
            }
            HeaderAction::Redo => {
                if let Some(loaded) = self.open.as_mut()
                    && !loaded.redo()
                {
                    self.status = Some("Nothing to redo".to_owned());
                }
            }
            HeaderAction::Close => {
                self.open = None;
                return;
            }
        }
        let Some(loaded) = self.open.as_mut() else {
            return;
        };

        transport_controls(ui, loaded, self.preview_events, self.snap_to_frames);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.snap_to_frames, "Snap to frames");
            ui.checkbox(&mut self.preview_events, "Preview events");
            ui.label(format!(
                "{}/{} fps",
                loaded.document.display_frame_rate.numerator,
                loaded.document.display_frame_rate.denominator
            ));
        });

        ui.separator();
        self.show_tracks(ui, permissions);
        ui.separator();
        self.show_markers(ui, permissions);

        if let Some(loaded) = self.open.as_ref()
            && !loaded.diagnostics.is_empty()
        {
            ui.colored_label(
                egui::Color32::from_rgb(220, 120, 120),
                loaded.diagnostics.join("; "),
            );
        }
        if let Some(status) = self.status.as_deref() {
            ui.small(status);
        }
        ui.small(
            "Scrubbing samples visual state and suppresses gameplay events unless Preview events is enabled.",
        );
    }

    fn show_tracks(&mut self, ui: &mut egui::Ui, permissions: &AuthoringPermissions) {
        let Some(loaded) = self.open.as_mut() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.strong("Add track");
            egui::ComboBox::from_id_salt("sequencer_add_track_kind")
                .selected_text(self.new_track_kind.label())
                .show_ui(ui, |ui| {
                    for kind in TimelineTrackKind::ALL {
                        ui.selectable_value(&mut self.new_track_kind, kind, kind.label());
                    }
                });
            if ui.button("Add").clicked() {
                let kind = self.new_track_kind;
                let result = loaded.edit(permissions, |document| {
                    document.tracks.push(TimelineTrack {
                        id: TimelineTrackId::generate(),
                        kind,
                        name: format!("{} {}", kind.label(), document.tracks.len() + 1),
                        enabled: true,
                        binding: engine_authoring::TimelineBinding::default(),
                        clips: Vec::new(),
                    });
                });
                self.status = Some(match result {
                    Ok(()) => format!("Added a {} track", kind.label()),
                    Err(error) => format!("Add track rejected: {error}"),
                });
            }
        });

        let track_count = loaded.document.tracks.len();
        if track_count == 0 {
            ui.label("This Timeline has no tracks yet.");
            return;
        }
        self.selected_track = self.selected_track.min(track_count - 1);
        let duration = loaded.document.duration.get().max(1);
        let playhead = loaded.preview_tick.get();
        let registry = &self.registry;
        let mut requested_seek = None;
        let mut selected_clip = self.selected_clip.clone();
        let mut selected_track = self.selected_track;

        for (index, track) in loaded.document.tracks.iter().enumerate() {
            ui.horizontal(|ui| {
                let label = format!("{} · {}", track.name, track.kind.label());
                ui.selectable_value(&mut selected_track, index, label);
                if let Some(descriptor) = registry.for_kind(track.kind) {
                    ui.small(descriptor.seek_policy.label());
                }
                if !track.enabled {
                    ui.small("disabled");
                }
                if track.binding.entity.is_none()
                    && registry
                        .for_kind(track.kind)
                        .is_some_and(|descriptor| descriptor.requires_entity_binding)
                {
                    ui.colored_label(egui::Color32::from_rgb(220, 160, 90), "unbound");
                }
            });
            let (rect, response) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, egui::Color32::from_gray(32));
            for clip in &track.clips {
                let start =
                    rect.left() + rect.width() * (clip.start.get() as f32 / duration as f32);
                let end = rect.left() + rect.width() * (clip.end.get() as f32 / duration as f32);
                let clip_rect = egui::Rect::from_min_max(
                    egui::pos2(start, rect.top() + 3.0),
                    egui::pos2(end.max(start + 2.0), rect.bottom() - 3.0),
                );
                let selected = selected_clip.as_ref() == Some(&clip.id);
                painter.rect_filled(
                    clip_rect,
                    2.0,
                    if selected {
                        egui::Color32::from_rgb(110, 150, 210)
                    } else {
                        egui::Color32::from_rgb(70, 90, 120)
                    },
                );
                if response.clicked()
                    && let Some(position) = response.interact_pointer_pos()
                    && clip_rect.contains(position)
                {
                    selected_clip = Some(clip.id.clone());
                    selected_track = index;
                }
            }
            let playhead_x = rect.left() + rect.width() * (playhead as f32 / duration as f32);
            painter.line_segment(
                [
                    egui::pos2(playhead_x, rect.top()),
                    egui::pos2(playhead_x, rect.bottom()),
                ],
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(230, 190, 90)),
            );
            if response.clicked()
                && let Some(position) = response.interact_pointer_pos()
                && !track.clips.iter().any(|clip| {
                    let start =
                        rect.left() + rect.width() * (clip.start.get() as f32 / duration as f32);
                    let end =
                        rect.left() + rect.width() * (clip.end.get() as f32 / duration as f32);
                    position.x >= start && position.x <= end
                })
            {
                let ratio = ((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                requested_seek = Some(TimelineTick((ratio * duration as f32) as i64));
            }
        }
        self.selected_track = selected_track;
        self.selected_clip = selected_clip;
        if let Some(tick) = requested_seek {
            let tick = if self.snap_to_frames {
                loaded
                    .document
                    .display_frame_rate
                    .snap(tick)
                    .unwrap_or(tick)
            } else {
                tick
            };
            loaded.seek(tick, self.preview_events);
        }

        self.show_track_editor(ui, permissions);
    }

    fn show_track_editor(&mut self, ui: &mut egui::Ui, permissions: &AuthoringPermissions) {
        let Some(loaded) = self.open.as_mut() else {
            return;
        };
        let Some(track) = loaded.document.tracks.get(self.selected_track) else {
            return;
        };
        let track_index = self.selected_track;
        let kind = track.kind;
        let name = track.name.clone();
        let duration = loaded.document.duration;
        let preview_tick = loaded.preview_tick;
        let mut track_action = TrackAction::None;
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("Track: {name}"));
            if ui.button("Add clip").clicked() {
                track_action = TrackAction::AddClip;
            }
            if ui.button("Toggle enabled").clicked() {
                track_action = TrackAction::ToggleEnabled;
            }
            if ui.button("Delete track").clicked() {
                track_action = TrackAction::Delete;
            }
        });
        match track_action {
            TrackAction::None => {}
            TrackAction::AddClip => {
                let start = preview_tick;
                let end = TimelineTick(
                    (start.get() + duration.get() / 8)
                        .min(duration.get())
                        .max(start.get() + 1),
                );
                let payload = default_payload(kind);
                let result = loaded.edit(permissions, |document| {
                    if let Some(track) = document.tracks.get_mut(track_index) {
                        track.clips.push(TimelineClip {
                            id: TimelineClipId::generate(),
                            start,
                            end,
                            payload,
                        });
                    }
                });
                self.status = Some(match result {
                    Ok(()) => "Added a clip".to_owned(),
                    Err(error) => format!("Add clip rejected: {error}"),
                });
            }
            TrackAction::ToggleEnabled => {
                let result = loaded.edit(permissions, |document| {
                    if let Some(track) = document.tracks.get_mut(track_index) {
                        track.enabled = !track.enabled;
                    }
                });
                if let Err(error) = result {
                    self.status = Some(format!("Track update rejected: {error}"));
                }
            }
            TrackAction::Delete => {
                let result = loaded.edit(permissions, |document| {
                    if track_index < document.tracks.len() {
                        document.tracks.remove(track_index);
                    }
                });
                self.status = Some(match result {
                    Ok(()) => "Deleted the track".to_owned(),
                    Err(error) => format!("Delete rejected: {error}"),
                });
            }
        }

        let Some(clip_id) = self.selected_clip.clone() else {
            ui.small("Select a clip to edit its interval.");
            return;
        };
        let Some(loaded) = self.open.as_mut() else {
            return;
        };
        let Some((clip_index, clip)) = loaded
            .document
            .tracks
            .get(track_index)
            .and_then(|track| {
                track
                    .clips
                    .iter()
                    .enumerate()
                    .find(|(_, clip)| clip.id == clip_id)
            })
            .map(|(index, clip)| (index, clip.clone()))
        else {
            return;
        };
        let mut start_seconds = clip.start.as_seconds();
        let mut end_seconds = clip.end.as_seconds();
        ui.horizontal_wrapped(|ui| {
            ui.label("Clip start (s)");
            let start_changed = ui
                .add(egui::DragValue::new(&mut start_seconds).speed(0.01_f32))
                .changed();
            ui.label("end (s)");
            let end_changed = ui
                .add(egui::DragValue::new(&mut end_seconds).speed(0.01_f32))
                .changed();
            if start_changed || end_changed {
                let start = TimelineTick::from_seconds(start_seconds);
                let end = TimelineTick::from_seconds(end_seconds);
                let result = loaded.edit(permissions, |document| {
                    if let Some(clip) = document
                        .tracks
                        .get_mut(track_index)
                        .and_then(|track| track.clips.get_mut(clip_index))
                    {
                        clip.start = start;
                        clip.end = end;
                    }
                });
                if let Err(error) = result {
                    self.status = Some(format!("Clip edit rejected: {error}"));
                }
            }
            if ui.button("Delete clip").clicked() {
                let result = loaded.edit(permissions, |document| {
                    if let Some(track) = document.tracks.get_mut(track_index) {
                        track.clips.retain(|candidate| candidate.id != clip_id);
                    }
                });
                self.status = Some(match result {
                    Ok(()) => "Deleted the clip".to_owned(),
                    Err(error) => format!("Delete rejected: {error}"),
                });
            }
        });

        curve_editor::show(
            ui,
            loaded,
            track_index,
            clip_index,
            permissions,
            self.snap_to_frames,
            &mut self.status,
        );
    }

    fn show_markers(&mut self, ui: &mut egui::Ui, permissions: &AuthoringPermissions) {
        let Some(loaded) = self.open.as_mut() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.strong("Markers");
            if ui.button("Add at playhead").clicked() {
                let tick = loaded.preview_tick;
                let result = loaded.edit(permissions, |document| {
                    document.markers.push(TimelineMarker {
                        id: TimelineMarkerId::generate(),
                        tick,
                        name: format!("Marker {}", document.markers.len() + 1),
                        event: format!("timeline.marker_{}", document.markers.len() + 1),
                    });
                });
                self.status = Some(match result {
                    Ok(()) => "Added a marker".to_owned(),
                    Err(error) => format!("Add marker rejected: {error}"),
                });
            }
        });
        let markers = loaded.document.markers.clone();
        for marker in markers {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} · {:.3}s · {}",
                    marker.name,
                    marker.tick.as_seconds(),
                    marker.event
                ));
                if ui.small_button("Go").clicked() {
                    loaded.seek(marker.tick, self.preview_events);
                }
                if ui.small_button("Delete").clicked() {
                    let id = marker.id.clone();
                    let result = loaded.edit(permissions, |document| {
                        document.markers.retain(|candidate| candidate.id != id);
                    });
                    if let Err(error) = result {
                        self.status = Some(format!("Delete rejected: {error}"));
                    }
                }
            });
        }
    }
}

fn transport_controls(
    ui: &mut egui::Ui,
    loaded: &mut LoadedTimeline,
    preview_events: bool,
    snap_to_frames: bool,
) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Play").clicked() {
            loaded.player.play();
        }
        if ui.button("Pause").clicked() {
            loaded.player.pause();
        }
        if ui.button("Stop").clicked() {
            loaded.player.stop();
            loaded.pending_preview = None;
            loaded.preview_tick = TimelineTick::ZERO;
        }
        if ui.button("Step −").clicked() {
            let tick = step_tick(loaded, -1, snap_to_frames);
            loaded.seek(tick, preview_events);
        }
        if ui.button("Step +").clicked() {
            let tick = step_tick(loaded, 1, snap_to_frames);
            loaded.seek(tick, preview_events);
        }
        let mut loop_enabled = loaded.player.loop_region().is_some();
        if ui.checkbox(&mut loop_enabled, "Loop").changed() {
            loaded.set_loop_enabled(loop_enabled);
        }
        let state = match loaded.player.state() {
            TimelinePlayState::Playing => "Playing",
            TimelinePlayState::Paused => "Paused",
            TimelinePlayState::Stopped => "Stopped",
        };
        ui.label(state);
        ui.label(format!(
            "{:.3}s / {:.3}s",
            loaded.preview_tick.as_seconds(),
            loaded.document.duration.as_seconds()
        ));
        if let Some(frame) = loaded
            .document
            .display_frame_rate
            .frame_of_tick(loaded.preview_tick)
        {
            ui.label(format!("frame {frame}"));
        }
    });
}

fn step_tick(loaded: &LoadedTimeline, direction: i64, snap_to_frames: bool) -> TimelineTick {
    let rate: DisplayFrameRate = loaded.document.display_frame_rate;
    if snap_to_frames
        && let Some(frame) = rate.frame_of_tick(loaded.preview_tick)
        && let Some(tick) = rate.tick_of_frame(frame + direction)
    {
        return tick;
    }
    TimelineTick(loaded.preview_tick.get() + direction * STEP_FALLBACK_TICKS)
}

/// Default payload used when a clip is added to a track of one kind.
fn default_payload(kind: TimelineTrackKind) -> TimelineClipPayload {
    match kind {
        TimelineTrackKind::Event => TimelineClipPayload::Event {
            event: "timeline.event".to_owned(),
        },
        TimelineTrackKind::CameraCut => TimelineClipPayload::CameraCut {
            camera: engine_authoring::EntityId::generate(),
        },
        TimelineTrackKind::Animation => TimelineClipPayload::Animation {
            motion_slot: engine_authoring::MotionSlotId::generate()
                .as_str()
                .to_owned(),
            speed: 1.0,
            looping: false,
        },
        TimelineTrackKind::Property => TimelineClipPayload::Property {
            property: TimelineProperty::TranslationX,
            keys: vec![engine_authoring::TimelineKey {
                tick: TimelineTick::ZERO,
                value: 0.0,
                interpolation: engine_authoring::TimelineInterpolation::Linear,
            }],
        },
        TimelineTrackKind::Audio => TimelineClipPayload::Audio {
            cue: engine_authoring::AssetId::generate(),
            action: engine_authoring::TimelineAudioAction::Play,
            fade_ticks: TimelineTick::ZERO,
        },
        TimelineTrackKind::Vfx => TimelineClipPayload::Vfx {
            effect: engine_authoring::AssetId::generate(),
            action: engine_authoring::TimelineVfxAction::Play,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only().with(AuthoringPermission::ProjectDataWrite)
    }

    fn loaded_timeline() -> LoadedTimeline {
        let document = TimelineDocument::new(TimelineTick(48_000));
        let compiled = compile_timeline(&document).expect("compile");
        LoadedTimeline {
            relative: PathBuf::from("cutscene.timeline.json"),
            path: PathBuf::from("cutscene.timeline.json"),
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            compiled,
            player: TimelinePlayer::new(),
            preview_tick: TimelineTick::ZERO,
            pending_preview: None,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn a_rejected_edit_leaves_the_document_and_the_compiled_schedule_unchanged() {
        let mut loaded = loaded_timeline();
        loaded
            .edit(&writable(), |document| {
                document.tracks.push(TimelineTrack {
                    id: TimelineTrackId::generate(),
                    kind: TimelineTrackKind::Event,
                    name: "Events".to_owned(),
                    enabled: true,
                    binding: engine_authoring::TimelineBinding::default(),
                    clips: Vec::new(),
                });
            })
            .expect("valid edit");
        let before = loaded.document.clone();

        let error = loaded
            .edit(&writable(), |document| {
                // A clip that ends before it starts is rejected by validation.
                document.tracks[0].clips.push(TimelineClip {
                    id: TimelineClipId::generate(),
                    start: TimelineTick(1_000),
                    end: TimelineTick(500),
                    payload: TimelineClipPayload::Event {
                        event: "boom".to_owned(),
                    },
                });
            })
            .expect_err("invalid edit");
        assert!(error.contains("must end after it starts"));
        assert_eq!(loaded.document, before);
        assert_eq!(loaded.compiled.tracks.len(), 1);
    }

    #[test]
    fn a_read_only_session_cannot_edit_the_document() {
        let mut loaded = loaded_timeline();
        let error = loaded
            .edit(&AuthoringPermissions::read_only(), |document| {
                document.duration = TimelineTick(1);
            })
            .expect_err("read-only session");
        assert!(error.contains("cannot write project data"));
        assert_eq!(loaded.document.duration, TimelineTick(48_000));
    }

    #[test]
    fn undo_and_redo_restore_the_document_and_recompile_it() {
        let mut loaded = loaded_timeline();
        loaded
            .edit(&writable(), |document| {
                document.tracks.push(TimelineTrack {
                    id: TimelineTrackId::generate(),
                    kind: TimelineTrackKind::Event,
                    name: "Events".to_owned(),
                    enabled: true,
                    binding: engine_authoring::TimelineBinding::default(),
                    clips: Vec::new(),
                });
            })
            .expect("valid edit");
        assert_eq!(loaded.compiled.tracks.len(), 1);

        assert!(loaded.undo());
        assert!(loaded.document.tracks.is_empty());
        assert!(loaded.compiled.tracks.is_empty());
        assert!(!loaded.undo());

        assert!(loaded.redo());
        assert_eq!(loaded.document.tracks.len(), 1);
        assert_eq!(loaded.compiled.tracks.len(), 1);
        assert!(!loaded.redo());
    }

    #[test]
    fn a_new_edit_after_undo_clears_redo_history() {
        let mut loaded = loaded_timeline();
        loaded
            .edit(&writable(), |document| {
                document.duration = TimelineTick(24_000);
            })
            .expect("valid edit");
        assert!(loaded.undo());
        assert_eq!(loaded.redo.len(), 1);

        loaded
            .edit(&writable(), |document| {
                document.duration = TimelineTick(36_000);
            })
            .expect("replacement edit");
        assert!(loaded.redo.is_empty());
        assert!(!loaded.redo());
    }

    #[test]
    fn curve_edit_save_and_reopen_preserve_ticks_and_stable_ids() {
        let mut loaded = loaded_timeline();
        let track_id = TimelineTrackId::generate();
        let clip_id = TimelineClipId::generate();
        let entity = engine_authoring::EntityId::generate();
        loaded
            .edit(&writable(), |document| {
                document.tracks.push(TimelineTrack {
                    id: track_id.clone(),
                    kind: TimelineTrackKind::Property,
                    name: "Transform X".to_owned(),
                    enabled: true,
                    binding: engine_authoring::TimelineBinding {
                        entity: Some(entity),
                        asset: None,
                    },
                    clips: vec![TimelineClip {
                        id: clip_id.clone(),
                        start: TimelineTick(1_000),
                        end: TimelineTick(20_000),
                        payload: TimelineClipPayload::Property {
                            property: TimelineProperty::TranslationX,
                            keys: vec![
                                engine_authoring::TimelineKey {
                                    tick: TimelineTick(123),
                                    value: 1.25,
                                    interpolation: engine_authoring::TimelineInterpolation::Smooth,
                                },
                                engine_authoring::TimelineKey {
                                    tick: TimelineTick(9_876),
                                    value: -3.5,
                                    interpolation: engine_authoring::TimelineInterpolation::Linear,
                                },
                            ],
                        },
                    }],
                });
            })
            .expect("valid curve edit");

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "gameengine-sequencer-{}-{unique}.timeline.json",
            std::process::id()
        ));
        loaded.path = path.clone();
        let expected = loaded.document.clone();
        loaded.save().expect("save Timeline");

        let saved = std::fs::read_to_string(&path).expect("read saved Timeline");
        let reopened = TimelineDocument::from_json(&saved).expect("reopen Timeline");
        assert_eq!(reopened, expected);
        let track = reopened.track(&track_id).expect("stable track id");
        assert_eq!(track.clips[0].id, clip_id);
        let TimelineClipPayload::Property { keys, .. } = &track.clips[0].payload else {
            panic!("saved clip must remain a Property payload");
        };
        assert_eq!(keys[0].tick, TimelineTick(123));
        assert_eq!(keys[1].tick, TimelineTick(9_876));

        std::fs::remove_file(path).expect("remove temporary Timeline");
    }

    #[test]
    fn scrubbing_suppresses_events_and_event_preview_opts_in() {
        let mut loaded = loaded_timeline();
        loaded
            .edit(&writable(), |document| {
                document.markers.push(TimelineMarker {
                    id: TimelineMarkerId::generate(),
                    tick: TimelineTick(1_000),
                    name: "hit".to_owned(),
                    event: "hit".to_owned(),
                });
            })
            .expect("valid edit");

        loaded.seek(TimelineTick(1_000), false);
        assert_eq!(loaded.preview_tick, TimelineTick(1_000));
        let scrub = loaded
            .player
            .seek(&loaded.compiled, TimelineTick(1_000), TimelineSeek::Scrub);
        assert!(scrub.events.is_empty());
        let preview = loaded.player.seek(
            &loaded.compiled,
            TimelineTick(1_000),
            TimelineSeek::PreviewEvents,
        );
        assert_eq!(preview.events.len(), 1);
    }

    #[test]
    fn preview_clock_plays_pauses_and_loops_without_losing_the_playhead() {
        let mut loaded = loaded_timeline();
        loaded.set_loop_enabled(true);
        loaded.player.play();

        let evaluation = loaded.advance_preview(1.25);
        assert_eq!(evaluation.tick, TimelineTick(12_000));
        assert_eq!(loaded.player.loops_completed(), 1);
        assert_eq!(loaded.player.state(), TimelinePlayState::Playing);

        loaded.player.pause();
        let paused = loaded.advance_preview(0.5);
        assert_eq!(paused.tick, TimelineTick(12_000));
        assert_eq!(loaded.preview_tick, TimelineTick(12_000));
    }

    #[test]
    fn explicit_event_preview_reaches_the_next_scene_view_sample_once() {
        let mut loaded = loaded_timeline();
        loaded
            .edit(&writable(), |document| {
                document.markers.push(TimelineMarker {
                    id: TimelineMarkerId::generate(),
                    tick: TimelineTick(1_000),
                    name: "hit".to_owned(),
                    event: "hit".to_owned(),
                });
            })
            .expect("valid edit");

        loaded.seek(TimelineTick(1_000), true);
        let preview = loaded.advance_preview(0.0);
        assert_eq!(preview.events.len(), 1);
        let held = loaded.advance_preview(0.0);
        assert!(held.events.is_empty());
    }

    #[test]
    fn a_default_payload_matches_the_track_kind_it_was_created_for() {
        for kind in TimelineTrackKind::ALL {
            assert_eq!(default_payload(kind).kind(), kind);
        }
    }
}
