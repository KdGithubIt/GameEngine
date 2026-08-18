//! Shared transactional Timeline authoring service used by GUI/CLI/MCP adapters.

use crate::id::{TimelineClipId, TimelineMarkerId, TimelineTrackId};
use crate::timeline::{
    save_timeline, validate_timeline, TimelineBinding, TimelineClip, TimelineClipPayload,
    TimelineDiagnostic, TimelineDiagnosticSeverity, TimelineDocument, TimelineMarker, TimelineTrack,
};
use crate::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
use engine_timeline::TimelineTick;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub type TimelineRevision = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TimelineAuthoringCommand {
    SetDuration(TimelineTick),
    AddTrack(TimelineTrack),
    RenameTrack { track: TimelineTrackId, name: String },
    RemoveTrack(TimelineTrackId),
    SetBinding { track: TimelineTrackId, binding: Option<TimelineBinding> },
    SetTrackEnabled { track: TimelineTrackId, enabled: bool },
    AddClip { track: TimelineTrackId, clip: TimelineClip },
    RemoveClip(TimelineClipId),
    RenameClip { clip: TimelineClipId, name: String },
    SetClipPayload { clip: TimelineClipId, payload: TimelineClipPayload },
    MoveClip { clip: TimelineClipId, start: TimelineTick },
    ResizeClip { clip: TimelineClipId, duration: TimelineTick },
    AddMarker(TimelineMarker),
    RemoveMarker(TimelineMarkerId),
    MoveMarker { marker: TimelineMarkerId, tick: TimelineTick },
    SetMarkerEvent { marker: TimelineMarkerId, event: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringPreview { pub document: TimelineDocument, pub diagnostics: Vec<TimelineDiagnostic> }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringSnapshot { pub revision: TimelineRevision, pub generation: u64, pub document: TimelineDocument }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringValidation { pub revision: TimelineRevision, pub generation: u64, pub success: bool, pub diagnostics: Vec<TimelineDiagnostic> }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringChange { pub before: TimelineDocument, pub after: TimelineDocument }
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringMutation { pub success: bool, pub base_revision: TimelineRevision, pub base_generation: u64, pub revision: TimelineRevision, pub generation: u64, pub diagnostics: Vec<TimelineDiagnostic>, pub diff: Vec<TimelineAuthoringChange> }

#[derive(Debug)]
pub enum TimelineAuthoringError {
    Permission(AuthoringPermissionError),
    StaleRevision { expected: TimelineRevision, actual: TimelineRevision },
    Stale { expected_revision: TimelineRevision, expected_generation: u64, actual_revision: TimelineRevision, actual_generation: u64 },
    MissingTarget(String),
    Invalid(Vec<TimelineDiagnostic>),
    Persist(String),
}
impl TimelineAuthoringError {
    pub fn code(&self) -> &'static str { match self { Self::Permission(error) => error.code(), Self::StaleRevision { .. } | Self::Stale { .. } => "authoring.stale_revision", Self::MissingTarget(_) => "timeline.target_missing", Self::Invalid(_) => "timeline.invalid", Self::Persist(_) => "timeline.persist_failed" } }
}
impl fmt::Display for TimelineAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(error) => error.fmt(formatter),
            Self::StaleRevision { expected, actual } => write!(formatter, "stale Timeline revision {actual}; current is {expected}"),
            Self::Stale { expected_revision, expected_generation, actual_revision, actual_generation } => write!(formatter, "stale Timeline base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"),
            Self::MissingTarget(id) => write!(formatter, "Timeline target {id} is missing"),
            Self::Invalid(diagnostics) => write!(formatter, "Timeline mutation has {} validation diagnostics", diagnostics.len()),
            Self::Persist(error) => formatter.write_str(error),
        }
    }
}
impl std::error::Error for TimelineAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { match self { Self::Permission(error) => Some(error), _ => None } }
}
impl From<AuthoringPermissionError> for TimelineAuthoringError { fn from(error: AuthoringPermissionError) -> Self { Self::Permission(error) } }

#[derive(Debug, Clone)]
pub struct TimelineAuthoringService { document: TimelineDocument, revision: TimelineRevision, generation: u64, undo: Vec<TimelineDocument>, redo: Vec<TimelineDocument> }
impl TimelineAuthoringService {
    pub fn new(document: TimelineDocument) -> Result<Self, TimelineAuthoringError> {
        let diagnostics = validate_timeline(&document);
        if has_errors(&diagnostics) { return Err(TimelineAuthoringError::Invalid(diagnostics)); }
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        Ok(Self { document, revision: 0, generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed), undo: Vec::new(), redo: Vec::new() })
    }
    pub const fn document(&self) -> &TimelineDocument { &self.document }
    pub const fn revision(&self) -> TimelineRevision { self.revision }
    pub const fn generation(&self) -> u64 { self.generation }
    pub fn inspect(&self, permissions: &AuthoringPermissions) -> Result<TimelineAuthoringSnapshot, TimelineAuthoringError> { permissions.require(AuthoringPermission::Read)?; Ok(TimelineAuthoringSnapshot { revision: self.revision, generation: self.generation, document: self.document.clone() }) }
    pub fn validate(&self, permissions: &AuthoringPermissions) -> Result<TimelineAuthoringValidation, TimelineAuthoringError> { permissions.require(AuthoringPermission::Read)?; let diagnostics = validate_timeline(&self.document); Ok(TimelineAuthoringValidation { revision: self.revision, generation: self.generation, success: !has_errors(&diagnostics), diagnostics }) }
    pub fn preview_commands(&self, permissions: &AuthoringPermissions, expected_revision: TimelineRevision, expected_generation: u64, commands: Vec<TimelineAuthoringCommand>) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?; self.check_base(expected_revision, expected_generation)?; let candidate = candidate_from_commands(&self.document, commands)?; let diagnostics = validate_timeline(&candidate); let success = !has_errors(&diagnostics); Ok(TimelineAuthoringMutation { success, base_revision: expected_revision, base_generation: expected_generation, revision: self.revision, generation: self.generation, diagnostics, diff: document_diff(&self.document, &candidate) })
    }
    pub fn apply_commands(&mut self, permissions: &AuthoringPermissions, expected_revision: TimelineRevision, expected_generation: u64, commands: Vec<TimelineAuthoringCommand>) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?; self.check_base(expected_revision, expected_generation)?; let candidate = candidate_from_commands(&self.document, commands)?; let diagnostics = validate_timeline(&candidate); let success = !has_errors(&diagnostics); let diff = document_diff(&self.document, &candidate);
        if !success { return Ok(TimelineAuthoringMutation { success: false, base_revision: expected_revision, base_generation: expected_generation, revision: self.revision, generation: self.generation, diagnostics, diff }); }
        if !diff.is_empty() { self.undo.push(self.document.clone()); self.document = candidate; self.redo.clear(); self.advance_live_state(); }
        Ok(TimelineAuthoringMutation { success: true, base_revision: expected_revision, base_generation: expected_generation, revision: self.revision, generation: self.generation, diagnostics, diff })
    }
    pub fn preview(&self, expected: TimelineRevision, command: &TimelineAuthoringCommand) -> Result<TimelineAuthoringPreview, TimelineAuthoringError> { self.check_revision(expected)?; let mut candidate = self.document.clone(); apply_command(&mut candidate, command)?; let diagnostics = validate_timeline(&candidate); Ok(TimelineAuthoringPreview { document: candidate, diagnostics }) }
    pub fn apply(&mut self, expected: TimelineRevision, command: TimelineAuthoringCommand) -> Result<TimelineRevision, TimelineAuthoringError> { self.apply_transaction(expected, std::iter::once(command)) }
    pub fn apply_transaction(&mut self, expected: TimelineRevision, commands: impl IntoIterator<Item = TimelineAuthoringCommand>) -> Result<TimelineRevision, TimelineAuthoringError> { self.check_revision(expected)?; let candidate = candidate_from_commands(&self.document, commands)?; let diagnostics = validate_timeline(&candidate); if has_errors(&diagnostics) { return Err(TimelineAuthoringError::Invalid(diagnostics)); } self.undo.push(self.document.clone()); self.document = candidate; self.redo.clear(); self.advance_live_state(); Ok(self.revision) }
    pub fn undo(&mut self) -> bool { let Some(previous) = self.undo.pop() else { return false; }; self.redo.push(std::mem::replace(&mut self.document, previous)); self.advance_live_state(); true }
    pub fn redo(&mut self) -> bool { let Some(next) = self.redo.pop() else { return false; }; self.undo.push(std::mem::replace(&mut self.document, next)); self.advance_live_state(); true }
    pub fn save(&self, path: &Path) -> Result<(), TimelineAuthoringError> { save_timeline(path, &self.document).map_err(|error| TimelineAuthoringError::Persist(error.to_string())) }
    fn advance_live_state(&mut self) { self.revision = self.revision.wrapping_add(1); self.generation = self.generation.wrapping_add(1); }
    fn check_revision(&self, actual: TimelineRevision) -> Result<(), TimelineAuthoringError> { if actual == self.revision { Ok(()) } else { Err(TimelineAuthoringError::StaleRevision { expected: self.revision, actual }) } }
    fn check_base(&self, expected_revision: TimelineRevision, expected_generation: u64) -> Result<(), TimelineAuthoringError> { if expected_revision == self.revision && expected_generation == self.generation { return Ok(()); } Err(TimelineAuthoringError::Stale { expected_revision, expected_generation, actual_revision: self.revision, actual_generation: self.generation }) }
}

fn candidate_from_commands(document: &TimelineDocument, commands: impl IntoIterator<Item = TimelineAuthoringCommand>) -> Result<TimelineDocument, TimelineAuthoringError> { let mut candidate = document.clone(); for command in commands { apply_command(&mut candidate, &command)?; } Ok(candidate) }
fn document_diff(before: &TimelineDocument, after: &TimelineDocument) -> Vec<TimelineAuthoringChange> { if before == after { Vec::new() } else { vec![TimelineAuthoringChange { before: before.clone(), after: after.clone() }] } }
fn has_errors(diagnostics: &[TimelineDiagnostic]) -> bool { diagnostics.iter().any(|diagnostic| diagnostic.severity == TimelineDiagnosticSeverity::Error) }
fn apply_command(doc: &mut TimelineDocument, command: &TimelineAuthoringCommand) -> Result<(), TimelineAuthoringError> {
    match command {
        TimelineAuthoringCommand::SetDuration(duration) => doc.duration = *duration,
        TimelineAuthoringCommand::AddTrack(track) => doc.tracks.push(track.clone()),
        TimelineAuthoringCommand::RenameTrack { track, name } => find_track_mut(doc, track)?.name = name.clone(),
        TimelineAuthoringCommand::RemoveTrack(id) => { let before = doc.tracks.len(); doc.tracks.retain(|value| &value.id != id); if before == doc.tracks.len() { return Err(TimelineAuthoringError::MissingTarget(id.to_string())); } }
        TimelineAuthoringCommand::SetBinding { track, binding } => find_track_mut(doc, track)?.binding = binding.clone(),
        TimelineAuthoringCommand::SetTrackEnabled { track, enabled } => find_track_mut(doc, track)?.enabled = *enabled,
        TimelineAuthoringCommand::AddClip { track, clip } => find_track_mut(doc, track)?.clips.push(clip.clone()),
        TimelineAuthoringCommand::RemoveClip(id) => { let mut found = false; for track in &mut doc.tracks { let before = track.clips.len(); track.clips.retain(|value| &value.id != id); found |= before != track.clips.len(); } if !found { return Err(TimelineAuthoringError::MissingTarget(id.to_string())); } }
        TimelineAuthoringCommand::RenameClip { clip, name } => find_clip_mut(doc, clip)?.name = name.clone(),
        TimelineAuthoringCommand::SetClipPayload { clip, payload } => find_clip_mut(doc, clip)?.payload = payload.clone(),
        TimelineAuthoringCommand::MoveClip { clip, start } => find_clip_mut(doc, clip)?.start = *start,
        TimelineAuthoringCommand::ResizeClip { clip, duration } => find_clip_mut(doc, clip)?.duration = *duration,
        TimelineAuthoringCommand::AddMarker(marker) => doc.markers.push(marker.clone()),
        TimelineAuthoringCommand::RemoveMarker(id) => { let before = doc.markers.len(); doc.markers.retain(|marker| &marker.id != id); if before == doc.markers.len() { return Err(TimelineAuthoringError::MissingTarget(id.to_string())); } }
        TimelineAuthoringCommand::MoveMarker { marker, tick } => { let item = doc.markers.iter_mut().find(|item| &item.id == marker).ok_or_else(|| TimelineAuthoringError::MissingTarget(marker.to_string()))?; item.tick = *tick; }
        TimelineAuthoringCommand::SetMarkerEvent { marker, event } => { let item = doc.markers.iter_mut().find(|item| &item.id == marker).ok_or_else(|| TimelineAuthoringError::MissingTarget(marker.to_string()))?; item.event = event.clone(); }
    }
    Ok(())
}
fn find_track_mut<'a>(doc: &'a mut TimelineDocument, id: &TimelineTrackId) -> Result<&'a mut TimelineTrack, TimelineAuthoringError> { doc.tracks.iter_mut().find(|value| &value.id == id).ok_or_else(|| TimelineAuthoringError::MissingTarget(id.to_string())) }
fn find_clip_mut<'a>(doc: &'a mut TimelineDocument, id: &TimelineClipId) -> Result<&'a mut TimelineClip, TimelineAuthoringError> { for track in &mut doc.tracks { if let Some(index) = track.clips.iter().position(|value| &value.id == id) { return Ok(&mut track.clips[index]); } } Err(TimelineAuthoringError::MissingTarget(id.to_string())) }
