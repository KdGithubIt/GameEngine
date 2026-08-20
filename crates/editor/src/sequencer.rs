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
    CompiledTimeline, TimelinePlayer, TimelineSeek, TrackRegistry, compile_timeline,
};
use std::path::{Path, PathBuf};

/// Ticks one Step control moves the playhead when no frame rate applies.
const STEP_FALLBACK_TICKS: i64 = 480;

/// One header control the user activated this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderAction {
    None,
    Save,
    Undo,
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
    dirty: bool,
    compiled: CompiledTimeline,
    player: TimelinePlayer,
    /// Ticks the last preview evaluation reported as its playhead.
    preview_tick: TimelineTick,
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
            dirty: false,
            compiled,
            player: TimelinePlayer::new(),
            preview_tick: TimelineTick::ZERO,
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
        self.compiled = compiled;
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
                self.document = previous;
                self.compiled = compiled;
                self.dirty = true;
                true
            }
            Err(_) => false,
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
    }
}

impl SequencerState {
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
            ui.horizontal_wrapped(|ui| {
                ui.strong(title);
                if dirty {
                    ui.label("(unsaved)");
                }
                if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                    action = HeaderAction::Save;
                }
                if ui.button("Undo").clicked() {
                    action = HeaderAction::Undo;
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
            motion_slot: "motion".to_owned(),
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
            dirty: false,
            compiled,
            player: TimelinePlayer::new(),
            preview_tick: TimelineTick::ZERO,
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
    fn undo_restores_the_previous_document_and_recompiles_it() {
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
    fn a_default_payload_matches_the_track_kind_it_was_created_for() {
        for kind in TimelineTrackKind::ALL {
            assert_eq!(default_payload(kind).kind(), kind);
        }
    }
}
