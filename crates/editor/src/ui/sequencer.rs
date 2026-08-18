//! ADR 0126 Timeline / Sequencer editor surface.
//!
//! Persisted changes are routed through `TimelineAuthoringService`; transient
//! preview mute/solo/lock and the editor transport clock are never serialized.

use super::*;
use engine::timeline::{SeekCapability, TimelineTick, TIMELINE_TICKS_PER_SECOND};
use engine_authoring::{
    TimelineAuthoringCommand, TimelineAuthoringService, TimelineBinding, TimelineClip,
    TimelineClipId, TimelineClipPayload, TimelineDocument, TimelineMarker, TimelineMarkerId,
    TimelinePropertyKey, TimelinePropertyValue, TimelineTrack, TimelineTrackId, TimelineTrackKind,
};
use std::collections::BTreeSet;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SequencerSelection {
    Track(TimelineTrackId),
    Clip(TimelineClipId),
    Marker(TimelineMarkerId),
}

pub(in crate::ui) struct TimelineSequencerState {
    relative_path: PathBuf,
    absolute_path: PathBuf,
    service: TimelineAuthoringService,
    saved_document: TimelineDocument,
    selection: Option<SequencerSelection>,
    add_track_search: String,
    playhead: TimelineTick,
    playing: bool,
    loop_preview: bool,
    pixels_per_second: f32,
    muted: BTreeSet<TimelineTrackId>,
    soloed: BTreeSet<TimelineTrackId>,
    locked: BTreeSet<TimelineTrackId>,
    last_frame: Instant,
    snap_feedback: Option<String>,
}

impl TimelineSequencerState {
    fn new(relative_path: PathBuf, absolute_path: PathBuf, document: TimelineDocument) -> Result<Self, String> {
        let service = TimelineAuthoringService::new(document.clone()).map_err(|error| error.to_string())?;
        Ok(Self {
            relative_path,
            absolute_path,
            service,
            saved_document: document,
            selection: None,
            add_track_search: String::new(),
            playhead: TimelineTick::ZERO,
            playing: false,
            loop_preview: false,
            pixels_per_second: 110.0,
            muted: BTreeSet::new(),
            soloed: BTreeSet::new(),
            locked: BTreeSet::new(),
            last_frame: Instant::now(),
            snap_feedback: None,
        })
    }

    fn document(&self) -> &TimelineDocument { self.service.document() }
    fn is_dirty(&self) -> bool { self.document() != &self.saved_document }

    fn apply(&mut self, command: TimelineAuthoringCommand) -> Result<(), String> {
        let revision = self.service.revision();
        self.service.apply(revision, command).map(|_| ()).map_err(|error| error.to_string())
    }

    fn save(&mut self) -> Result<(), String> {
        self.service.save(&self.absolute_path).map_err(|error| error.to_string())?;
        self.saved_document = self.document().clone();
        Ok(())
    }

    fn frame_ticks(&self) -> i64 {
        let rate = self.document().display_rate;
        if rate.numerator == 0 { return 1; }
        ((TIMELINE_TICKS_PER_SECOND as i128 * i128::from(rate.denominator)) / i128::from(rate.numerator)).max(1) as i64
    }

    fn advance_clock(&mut self) {
        let now = Instant::now();
        let delta = now.saturating_duration_since(self.last_frame);
        self.last_frame = now;
        if !self.playing { return; }
        let advance = (delta.as_secs_f64() * TIMELINE_TICKS_PER_SECOND as f64).round() as i64;
        let duration = self.document().duration;
        let next = self.playhead.saturating_add(advance);
        if next > duration {
            if self.loop_preview && duration > TimelineTick::ZERO {
                self.playhead = TimelineTick::new(next.get().rem_euclid(duration.get().max(1)));
            } else {
                self.playhead = duration;
                self.playing = false;
            }
        } else {
            self.playhead = next;
        }
    }

    fn snap(&mut self, proposed: TimelineTick) -> TimelineTick {
        let document = self.document().clone();
        let threshold = ((TIMELINE_TICKS_PER_SECOND as f32 * 7.0) / self.pixels_per_second.max(1.0)).round().max(1.0) as u64;
        let frame = self.frame_ticks();
        let nearest_frame = TimelineTick::new((proposed.get() as f64 / frame as f64).round() as i64 * frame);
        let mut candidates = vec![(nearest_frame, "frame".to_owned()), (self.playhead, "playhead".to_owned())];
        for marker in &document.markers { candidates.push((marker.tick, format!("marker {}", marker.name))); }
        for track in &document.tracks {
            for clip in &track.clips {
                candidates.push((clip.start, format!("{} start", clip.name)));
                candidates.push((clip.end(), format!("{} end", clip.name)));
            }
        }
        let proposed = proposed.clamp(TimelineTick::ZERO, document.duration);
        if let Some((_, tick, label)) = candidates.into_iter()
            .map(|(tick, label)| ((tick.get() - proposed.get()).unsigned_abs(), tick, label))
            .filter(|(distance, _, _)| *distance <= threshold)
            .min_by_key(|(distance, _, _)| *distance)
        {
            self.snap_feedback = Some(format!("Snapped to {label}"));
            tick.clamp(TimelineTick::ZERO, document.duration)
        } else {
            self.snap_feedback = None;
            proposed
        }
    }
}

fn track_label(kind: TimelineTrackKind) -> &'static str {
    engine_authoring::timeline_track_registry().iter().find(|descriptor| descriptor.kind == kind).map(|descriptor| descriptor.label).unwrap_or("Track")
}

fn seek_badge(kind: TimelineTrackKind) -> Option<&'static str> {
    match kind.seek_capability() {
        SeekCapability::ReplayRequired => Some("ReplayRequired"),
        SeekCapability::NonSeekable => Some("NonSeekable"),
        SeekCapability::Stateless | SeekCapability::Seekable => None,
    }
}

fn seconds(tick: TimelineTick) -> f64 { tick.to_seconds_f64() }
fn tick(value: f64) -> TimelineTick { TimelineTick::new((value.max(0.0) * TIMELINE_TICKS_PER_SECOND as f64).round() as i64) }

fn new_track(kind: TimelineTrackKind) -> TimelineTrack {
    TimelineTrack { id: TimelineTrackId::generate(), name: track_label(kind).to_owned(), kind, enabled: true, binding: None, clips: Vec::new() }
}

fn direct_clip(kind: TimelineTrackKind, start: TimelineTick) -> Option<TimelineClip> {
    let duration = TimelineTick::new(TIMELINE_TICKS_PER_SECOND);
    let payload = match kind {
        TimelineTrackKind::TransformProperty => TimelineClipPayload::TransformProperty {
            property: "engine.transform.translation".to_owned(),
            keys: vec![
                TimelinePropertyKey { tick: TimelineTick::ZERO, value: TimelinePropertyValue::Vec3([0.0; 3]) },
                TimelinePropertyKey { tick: duration, value: TimelinePropertyValue::Vec3([0.0; 3]) },
            ],
        },
        TimelineTrackKind::CameraCut => TimelineClipPayload::CameraCut,
        TimelineTrackKind::Event => TimelineClipPayload::Event { name: "timeline.event".to_owned(), payload: "{}".to_owned() },
        TimelineTrackKind::Animation | TimelineTrackKind::Audio | TimelineTrackKind::Vfx => return None,
    };
    Some(TimelineClip { id: TimelineClipId::generate(), name: track_label(kind).to_owned(), start, duration, source_offset: TimelineTick::ZERO, payload })
}

fn curve_value_ui(ui: &mut egui::Ui, value: &mut TimelinePropertyValue) -> bool {
    match value {
        TimelinePropertyValue::Bool(value) => ui.checkbox(value, "value").changed(),
        TimelinePropertyValue::Number(value) => ui.add(egui::DragValue::new(value).speed(0.01)).changed(),
        TimelinePropertyValue::Vec3(value) => {
            let mut changed = false;
            ui.horizontal(|ui| for component in value { changed |= ui.add(egui::DragValue::new(component).speed(0.01)).changed(); });
            changed
        }
        TimelinePropertyValue::Quat(value) => {
            let mut changed = false;
            ui.horizontal(|ui| for component in value { changed |= ui.add(egui::DragValue::new(component).speed(0.01)).changed(); });
            changed
        }
    }
}

impl EditorApp {
    pub(in crate::ui) fn open_timeline_sequencer(&mut self, relative_path: PathBuf, absolute_path: PathBuf) {
        match engine_authoring::load_timeline(&absolute_path)
            .map_err(|error| error.to_string())
            .and_then(|document| TimelineSequencerState::new(relative_path, absolute_path.clone(), document))
        {
            Ok(state) => self.timeline_sequencer = Some(state),
            Err(error) => self.report_error("editor.timeline_open_failed", format!("failed to open {}: {error}", absolute_path.display())),
        }
    }

    pub(in crate::ui) fn show_timeline_sequencer_window(&mut self, context: &egui::Context) {
        let Some(read_only) = self.timeline_sequencer.as_ref() else { return; };
        let title = format!("Sequencer: {}{}", read_only.relative_path.display(), if read_only.is_dirty() { " *" } else { "" });
        let scene = self.session.scene().cloned();
        let selected_scene_entity = self.selected_entity.clone();
        let mut open = true;
        let mut focus = None;
        let mut reported_error = None;

        {
            let state = self.timeline_sequencer.as_mut().expect("checked above");
            state.advance_clock();
            if state.playing { context.request_repaint(); }
            egui::Window::new(title)
                .id(egui::Id::new("timeline_sequencer_window"))
                .open(&mut open)
                .default_width(1120.0)
                .default_height(700.0)
                .resizable(true)
                .show(context, |ui| {
                    let mut command = None;
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Undo").clicked() { state.service.undo(); }
                        if ui.button("Redo").clicked() { state.service.redo(); }
                        if ui.add_enabled(state.is_dirty(), egui::Button::new("Save")).clicked() {
                            if let Err(error) = state.save() { reported_error = Some(error); }
                        }
                        ui.separator();
                        if ui.button("Play").clicked() { state.playing = true; state.last_frame = Instant::now(); }
                        if ui.button("Pause").clicked() { state.playing = false; }
                        if ui.button("Stop").clicked() { state.playing = false; state.playhead = TimelineTick::ZERO; }
                        if ui.button("Step").clicked() {
                            state.playing = false;
                            state.playhead = state.playhead.saturating_add(state.frame_ticks()).clamp(TimelineTick::ZERO, state.document().duration);
                        }
                        ui.checkbox(&mut state.loop_preview, "Loop");
                        let mut current = seconds(state.playhead);
                        if ui.add(egui::DragValue::new(&mut current).range(0.0..=seconds(state.document().duration)).speed(0.01).suffix(" s")).changed() {
                            state.playing = false;
                            state.playhead = state.snap(tick(current));
                        }
                        ui.monospace(format!("tick {}", state.playhead.get()));
                        ui.add(egui::Slider::new(&mut state.pixels_per_second, 40.0..=360.0).text("Zoom"));
                    });
                    if let Some(feedback) = &state.snap_feedback { ui.colored_label(egui::Color32::LIGHT_BLUE, feedback); }
                    ui.separator();

                    ui.horizontal_wrapped(|ui| {
                        ui.strong("Add Track");
                        ui.add(egui::TextEdit::singleline(&mut state.add_track_search).hint_text("Search track types...").desired_width(180.0));
                        let search = state.add_track_search.to_ascii_lowercase();
                        for descriptor in engine_authoring::timeline_track_registry() {
                            if (search.is_empty() || descriptor.label.to_ascii_lowercase().contains(&search)) && ui.small_button(descriptor.label).clicked() {
                                command = Some(TimelineAuthoringCommand::AddTrack(new_track(descriptor.kind)));
                            }
                        }
                    });
                    ui.separator();

                    let document = state.document().clone();
                    egui::ScrollArea::both().id_salt("timeline_lanes").auto_shrink([false, false]).show(ui, |ui| {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong("Markers / Events");
                                if ui.small_button("+ Marker").clicked() {
                                    command = Some(TimelineAuthoringCommand::AddMarker(TimelineMarker { id: TimelineMarkerId::generate(), name: "Marker".to_owned(), tick: state.playhead, event: None }));
                                }
                            });
                            ui.horizontal_wrapped(|ui| for marker in &document.markers {
                                let selected = state.selection.as_ref() == Some(&SequencerSelection::Marker(marker.id.clone()));
                                if ui.selectable_label(selected, format!("◆ {} {:.3}s", marker.name, seconds(marker.tick))).clicked() { state.selection = Some(SequencerSelection::Marker(marker.id.clone())); }
                            });
                        });

                        for track in &document.tracks {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    let selected = state.selection.as_ref() == Some(&SequencerSelection::Track(track.id.clone()));
                                    if ui.selectable_label(selected, format!("{} [{}]", track.name, track_label(track.kind))).clicked() { state.selection = Some(SequencerSelection::Track(track.id.clone())); }
                                    let mut enabled = track.enabled;
                                    if ui.checkbox(&mut enabled, "Enabled").changed() { command = Some(TimelineAuthoringCommand::SetTrackEnabled { track: track.id.clone(), enabled }); }
                                    for (label, set, hint) in [("M", &mut state.muted, "Preview mute"), ("S", &mut state.soloed, "Preview solo"), ("L", &mut state.locked, "Edit lock")] {
                                        if ui.selectable_label(set.contains(&track.id), label).on_hover_text(hint).clicked() { if !set.remove(&track.id) { set.insert(track.id.clone()); } }
                                    }
                                    if let Some(badge) = seek_badge(track.kind) { ui.colored_label(egui::Color32::YELLOW, badge); }
                                });
                                match &track.binding {
                                    Some(TimelineBinding::Entity { entity }) => {
                                        let exists = scene.as_ref().is_some_and(|scene| scene.entity(entity).is_some());
                                        ui.horizontal(|ui| {
                                            ui.small(format!("Binding: entity {}", entity.as_str()));
                                            if exists && ui.small_button("Focus").clicked() { focus = Some(entity.clone()); }
                                            if !exists { ui.colored_label(egui::Color32::from_rgb(230, 92, 92), "Missing binding"); }
                                        });
                                    }
                                    Some(TimelineBinding::Asset { asset }) => { ui.small(format!("Binding: asset {}", asset.as_str())); }
                                    None => { ui.colored_label(egui::Color32::YELLOW, "Binding: Unassigned"); }
                                }
                                ui.horizontal_wrapped(|ui| {
                                    for clip in &track.clips {
                                        let selected = state.selection.as_ref() == Some(&SequencerSelection::Clip(clip.id.clone()));
                                        if ui.selectable_label(selected, format!("{} {:.2}–{:.2}s", clip.name, seconds(clip.start), seconds(clip.end()))).clicked() { state.selection = Some(SequencerSelection::Clip(clip.id.clone())); }
                                    }
                                    let can_add = direct_clip(track.kind, state.playhead).is_some();
                                    if ui.add_enabled(can_add && !state.locked.contains(&track.id), egui::Button::new("+ Clip")).on_disabled_hover_text("Animation, Audio and VFX clips require a typed asset reference").clicked() {
                                        if let Some(clip) = direct_clip(track.kind, state.playhead) { command = Some(TimelineAuthoringCommand::AddClip { track: track.id.clone(), clip }); }
                                    }
                                });
                            });
                        }
                    });

                    ui.separator();
                    ui.strong("Inspector");
                    match state.selection.clone() {
                        Some(SequencerSelection::Track(track_id)) => if let Some(track) = document.tracks.iter().find(|track| track.id == track_id) {
                            ui.label(format!("{} / {}", track.name, track_label(track.kind)));
                            ui.horizontal(|ui| {
                                if let Some(entity) = selected_scene_entity.clone() { if ui.button("Bind Selected Entity").clicked() { command = Some(TimelineAuthoringCommand::SetBinding { track: track.id.clone(), binding: Some(TimelineBinding::Entity { entity }) }); } }
                                if ui.button("Clear Binding").clicked() { command = Some(TimelineAuthoringCommand::SetBinding { track: track.id.clone(), binding: None }); }
                                if ui.button("Delete Track").clicked() { command = Some(TimelineAuthoringCommand::RemoveTrack(track.id.clone())); state.selection = None; }
                            });
                        },
                        Some(SequencerSelection::Clip(clip_id)) => if let Some((track, clip)) = document.tracks.iter().find_map(|track| track.clips.iter().find(|clip| clip.id == clip_id).map(|clip| (track, clip))) {
                            ui.label(format!("{} / {}", track.name, clip.name));
                            let mut start = seconds(clip.start);
                            if ui.add(egui::DragValue::new(&mut start).range(0.0..=seconds(document.duration)).speed(0.01).prefix("Start ")).changed() { let start = state.snap(tick(start)); command = Some(TimelineAuthoringCommand::MoveClip { clip: clip.id.clone(), start }); }
                            let mut duration = seconds(clip.duration);
                            if ui.add(egui::DragValue::new(&mut duration).range(1.0 / TIMELINE_TICKS_PER_SECOND as f64..=seconds(document.duration)).speed(0.01).prefix("Duration ")).changed() { command = Some(TimelineAuthoringCommand::ResizeClip { clip: clip.id.clone(), duration: tick(duration) }); }
                            ui.horizontal(|ui| {
                                if ui.button("Duplicate").clicked() { let mut copy = clip.clone(); copy.id = TimelineClipId::generate(); copy.name = format!("{} Copy", clip.name); copy.start = state.snap(clip.end()); command = Some(TimelineAuthoringCommand::AddClip { track: track.id.clone(), clip: copy }); }
                                if ui.button("Delete Clip").clicked() { command = Some(TimelineAuthoringCommand::RemoveClip(clip.id.clone())); state.selection = None; }
                            });
                            if let TimelineClipPayload::TransformProperty { property, keys } = &clip.payload {
                                ui.separator(); ui.strong(format!("Curve: {property}"));
                                let mut edited = keys.clone(); let mut changed = false;
                                for key in &mut edited { ui.horizontal(|ui| { let mut local = seconds(key.tick); if ui.add(egui::DragValue::new(&mut local).range(0.0..=seconds(clip.duration)).speed(0.01).suffix(" s")).changed() { key.tick = tick(local).clamp(TimelineTick::ZERO, clip.duration); changed = true; } changed |= curve_value_ui(ui, &mut key.value); }); }
                                if changed { edited.sort_by_key(|key| key.tick); command = Some(TimelineAuthoringCommand::SetClipPayload { clip: clip.id.clone(), payload: TimelineClipPayload::TransformProperty { property: property.clone(), keys: edited } }); }
                            }
                        },
                        Some(SequencerSelection::Marker(marker_id)) => if let Some(marker) = document.markers.iter().find(|marker| marker.id == marker_id) {
                            ui.label(&marker.name); let mut at = seconds(marker.tick);
                            if ui.add(egui::DragValue::new(&mut at).range(0.0..=seconds(document.duration)).speed(0.01).prefix("Time ")).changed() { let at = state.snap(tick(at)); command = Some(TimelineAuthoringCommand::MoveMarker { marker: marker.id.clone(), tick: at }); }
                            if ui.button("Delete Marker").clicked() { command = Some(TimelineAuthoringCommand::RemoveMarker(marker.id.clone())); state.selection = None; }
                        },
                        None => { ui.small("Select a track, clip, or marker. Persisted edits use TimelineAuthoringCommand."); }
                    }
                    if let Some(command) = command { if let Err(error) = state.apply(command) { reported_error = Some(error); } }
                });
        }

        if let Some(message) = reported_error { self.report_error("editor.timeline_edit_failed", message); }
        if let Some(entity) = focus {
            if let Some(scene) = self.session.scene() {
                self.selected_entity = Some(entity.clone());
                self.selected_entities.clear();
                self.selected_entities.insert(entity.clone());
                let _ = self.scene_view.focus_entity(scene, &entity);
            }
        }
        if !open { self.timeline_sequencer = None; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_clip_is_directly_authorable() {
        let clip = direct_clip(TimelineTrackKind::TransformProperty, TimelineTick::ZERO).expect("property clip");
        assert_eq!(clip.duration, TimelineTick::new(TIMELINE_TICKS_PER_SECOND));
        assert!(matches!(clip.payload, TimelineClipPayload::TransformProperty { .. }));
    }

    #[test]
    fn asset_backed_clips_require_typed_references() {
        assert!(direct_clip(TimelineTrackKind::Animation, TimelineTick::ZERO).is_none());
        assert!(direct_clip(TimelineTrackKind::Audio, TimelineTick::ZERO).is_none());
        assert!(direct_clip(TimelineTrackKind::Vfx, TimelineTick::ZERO).is_none());
    }
}
