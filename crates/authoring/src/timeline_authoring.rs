//! Shared transactional Timeline authoring service used by GUI/CLI/MCP adapters.

use crate::id::{TimelineClipId, TimelineMarkerId, TimelineTrackId};
use crate::timeline::{
    save_timeline, validate_timeline, TimelineBinding, TimelineClip, TimelineDiagnostic,
    TimelineDiagnosticSeverity, TimelineDocument, TimelineMarker, TimelineTrack,
};
use crate::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
use engine_timeline::TimelineTick;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Optimistic revision token.
pub type TimelineRevision = u64;

/// One typed Timeline mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TimelineAuthoringCommand {
    /// Add a typed track.
    AddTrack(TimelineTrack),
    /// Remove a track.
    RemoveTrack(TimelineTrackId),
    /// Change a track stable binding.
    SetBinding {
        /// Track.
        track: TimelineTrackId,
        /// Binding.
        binding: Option<TimelineBinding>,
    },
    /// Change persisted enabled state.
    SetTrackEnabled {
        /// Track.
        track: TimelineTrackId,
        /// Enabled.
        enabled: bool,
    },
    /// Add a typed clip to a track.
    AddClip {
        /// Track.
        track: TimelineTrackId,
        /// Clip.
        clip: TimelineClip,
    },
    /// Delete a clip.
    RemoveClip(TimelineClipId),
    /// Move a clip without changing duration.
    MoveClip {
        /// Clip.
        clip: TimelineClipId,
        /// New start.
        start: TimelineTick,
    },
    /// Resize a clip.
    ResizeClip {
        /// Clip.
        clip: TimelineClipId,
        /// New duration.
        duration: TimelineTick,
    },
    /// Add marker lane entry.
    AddMarker(TimelineMarker),
    /// Remove marker.
    RemoveMarker(TimelineMarkerId),
    /// Move marker.
    MoveMarker {
        /// Marker.
        marker: TimelineMarkerId,
        /// Tick.
        tick: TimelineTick,
    },
}

/// Preview result retained for the existing one-command Editor API.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringPreview {
    /// Resulting candidate.
    pub document: TimelineDocument,
    /// Pure validation diagnostics.
    pub diagnostics: Vec<TimelineDiagnostic>,
}

/// Immutable Timeline state returned by structured authoring inspection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringSnapshot {
    /// Logical content revision.
    pub revision: TimelineRevision,
    /// Monotonic live-session generation used for stale-branch rejection.
    pub generation: u64,
    /// Complete committed Timeline source document.
    pub document: TimelineDocument,
}

/// Structured Timeline validation result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringValidation {
    /// Logical content revision validated by this result.
    pub revision: TimelineRevision,
    /// Live-session generation validated by this result.
    pub generation: u64,
    /// Whether validation produced no blocking diagnostics.
    pub success: bool,
    /// Pure Timeline diagnostics.
    pub diagnostics: Vec<TimelineDiagnostic>,
}

/// Deterministic whole-document change produced by a Timeline command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringChange {
    /// Committed source before the batch.
    pub before: TimelineDocument,
    /// Candidate or committed source after the batch.
    pub after: TimelineDocument,
}

/// Result of previewing or applying one atomic Timeline command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringMutation {
    /// Whether the complete candidate passed Timeline validation.
    pub success: bool,
    /// Caller-observed base revision.
    pub base_revision: TimelineRevision,
    /// Caller-observed base generation.
    pub base_generation: u64,
    /// Current revision after the operation.
    pub revision: TimelineRevision,
    /// Current generation after the operation.
    pub generation: u64,
    /// Pure Timeline diagnostics for the candidate.
    pub diagnostics: Vec<TimelineDiagnostic>,
    /// Empty for a no-op, otherwise one deterministic before/after change.
    pub diff: Vec<TimelineAuthoringChange>,
}

/// Shared Timeline authoring failure.
#[derive(Debug)]
pub enum TimelineAuthoringError {
    /// The host did not grant the required shared authoring permission.
    Permission(AuthoringPermissionError),
    /// Existing one-command caller supplied an obsolete revision.
    StaleRevision {
        /// Expected current revision.
        expected: TimelineRevision,
        /// Supplied revision.
        actual: TimelineRevision,
    },
    /// Structured caller supplied a revision/generation pair that is no longer live.
    Stale {
        /// Caller-observed revision.
        expected_revision: TimelineRevision,
        /// Caller-observed generation.
        expected_generation: u64,
        /// Current revision.
        actual_revision: TimelineRevision,
        /// Current generation.
        actual_generation: u64,
    },
    /// Target stable ID no longer exists.
    MissingTarget(String),
    /// Mutation made the document invalid.
    Invalid(Vec<TimelineDiagnostic>),
    /// Persistence failed.
    Persist(String),
}

impl TimelineAuthoringError {
    /// Returns the stable diagnostic-style code exposed to structured clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::StaleRevision { .. } | Self::Stale { .. } => "authoring.stale_revision",
            Self::MissingTarget(_) => "timeline.target_missing",
            Self::Invalid(_) => "timeline.invalid",
            Self::Persist(_) => "timeline.persist_failed",
        }
    }
}

impl fmt::Display for TimelineAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(error) => error.fmt(formatter),
            Self::StaleRevision { expected, actual } => {
                write!(formatter, "stale Timeline revision {actual}; current is {expected}")
            }
            Self::Stale {
                expected_revision,
                expected_generation,
                actual_revision,
                actual_generation,
            } => write!(
                formatter,
                "stale Timeline base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
            Self::MissingTarget(id) => write!(formatter, "Timeline target {id} is missing"),
            Self::Invalid(diagnostics) => write!(
                formatter,
                "Timeline mutation has {} validation diagnostics",
                diagnostics.len()
            ),
            Self::Persist(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for TimelineAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::StaleRevision { .. }
            | Self::Stale { .. }
            | Self::MissingTarget(_)
            | Self::Invalid(_)
            | Self::Persist(_) => None,
        }
    }
}

impl From<AuthoringPermissionError> for TimelineAuthoringError {
    fn from(error: AuthoringPermissionError) -> Self {
        Self::Permission(error)
    }
}

/// Shared stateful authoring service with stale-base rejection and session undo/redo.
#[derive(Debug, Clone)]
pub struct TimelineAuthoringService {
    document: TimelineDocument,
    revision: TimelineRevision,
    generation: u64,
    undo: Vec<TimelineDocument>,
    redo: Vec<TimelineDocument>,
}

impl TimelineAuthoringService {
    /// Starts a session around a current-format document.
    pub fn new(document: TimelineDocument) -> Result<Self, TimelineAuthoringError> {
        let diagnostics = validate_timeline(&document);
        if has_errors(&diagnostics) {
            return Err(TimelineAuthoringError::Invalid(diagnostics));
        }
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            document,
            revision: 0,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// Current immutable document.
    pub const fn document(&self) -> &TimelineDocument {
        &self.document
    }

    /// Current optimistic revision.
    pub const fn revision(&self) -> TimelineRevision {
        self.revision
    }

    /// Current live-session generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Inspects the committed Timeline through the shared permission boundary.
    pub fn inspect(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringSnapshot, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(TimelineAuthoringSnapshot {
            revision: self.revision,
            generation: self.generation,
            document: self.document.clone(),
        })
    }

    /// Validates the committed Timeline without mutation.
    pub fn validate(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringValidation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = validate_timeline(&self.document);
        Ok(TimelineAuthoringValidation {
            revision: self.revision,
            generation: self.generation,
            success: !has_errors(&diagnostics),
            diagnostics,
        })
    }

    /// Previews one atomic structured command batch without mutation or history changes.
    pub fn preview_commands(
        &self,
        permissions: &AuthoringPermissions,
        expected_revision: TimelineRevision,
        expected_generation: u64,
        commands: Vec<TimelineAuthoringCommand>,
    ) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        self.check_base(expected_revision, expected_generation)?;
        let candidate = candidate_from_commands(&self.document, commands)?;
        let diagnostics = validate_timeline(&candidate);
        let success = !has_errors(&diagnostics);
        Ok(TimelineAuthoringMutation {
            success,
            base_revision: expected_revision,
            base_generation: expected_generation,
            revision: self.revision,
            generation: self.generation,
            diagnostics,
            diff: document_diff(&self.document, &candidate),
        })
    }

    /// Applies one atomic structured command batch through the shared permission boundary.
    pub fn apply_commands(
        &mut self,
        permissions: &AuthoringPermissions,
        expected_revision: TimelineRevision,
        expected_generation: u64,
        commands: Vec<TimelineAuthoringCommand>,
    ) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        self.check_base(expected_revision, expected_generation)?;
        let candidate = candidate_from_commands(&self.document, commands)?;
        let diagnostics = validate_timeline(&candidate);
        let success = !has_errors(&diagnostics);
        let diff = document_diff(&self.document, &candidate);
        if !success {
            return Ok(TimelineAuthoringMutation {
                success: false,
                base_revision: expected_revision,
                base_generation: expected_generation,
                revision: self.revision,
                generation: self.generation,
                diagnostics,
                diff,
            });
        }
        if !diff.is_empty() {
            self.undo.push(self.document.clone());
            self.document = candidate;
            self.redo.clear();
            self.advance_live_state();
        }
        Ok(TimelineAuthoringMutation {
            success: true,
            base_revision: expected_revision,
            base_generation: expected_generation,
            revision: self.revision,
            generation: self.generation,
            diagnostics,
            diff,
        })
    }

    /// Validates one command against a clone without mutation.
    pub fn preview(
        &self,
        expected: TimelineRevision,
        command: &TimelineAuthoringCommand,
    ) -> Result<TimelineAuthoringPreview, TimelineAuthoringError> {
        self.check_revision(expected)?;
        let mut candidate = self.document.clone();
        apply_command(&mut candidate, command)?;
        let diagnostics = validate_timeline(&candidate);
        Ok(TimelineAuthoringPreview {
            document: candidate,
            diagnostics,
        })
    }

    /// Atomically applies one command after validation.
    pub fn apply(
        &mut self,
        expected: TimelineRevision,
        command: TimelineAuthoringCommand,
    ) -> Result<TimelineRevision, TimelineAuthoringError> {
        self.apply_transaction(expected, std::iter::once(command))
    }

    /// Atomically applies a granular command transaction with one undo boundary.
    ///
    /// This compatibility entry point is retained for existing Editor code. New
    /// structured adapters use [`Self::apply_commands`] so permissions and the
    /// generation token are enforced at the shared boundary.
    pub fn apply_transaction(
        &mut self,
        expected: TimelineRevision,
        commands: impl IntoIterator<Item = TimelineAuthoringCommand>,
    ) -> Result<TimelineRevision, TimelineAuthoringError> {
        self.check_revision(expected)?;
        let candidate = candidate_from_commands(&self.document, commands)?;
        let diagnostics = validate_timeline(&candidate);
        if has_errors(&diagnostics) {
            return Err(TimelineAuthoringError::Invalid(diagnostics));
        }
        self.undo.push(self.document.clone());
        self.document = candidate;
        self.redo.clear();
        self.advance_live_state();
        Ok(self.revision)
    }

    /// Undoes one committed transaction.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.document, previous));
        self.advance_live_state();
        true
    }

    /// Redoes one undone transaction.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo
            .push(std::mem::replace(&mut self.document, next));
        self.advance_live_state();
        true
    }

    /// Persists the committed source of truth.
    pub fn save(&self, path: &Path) -> Result<(), TimelineAuthoringError> {
        save_timeline(path, &self.document)
            .map_err(|error| TimelineAuthoringError::Persist(error.to_string()))
    }

    fn advance_live_state(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
    }

    fn check_revision(&self, actual: TimelineRevision) -> Result<(), TimelineAuthoringError> {
        if actual == self.revision {
            Ok(())
        } else {
            Err(TimelineAuthoringError::StaleRevision {
                expected: self.revision,
                actual,
            })
        }
    }

    fn check_base(
        &self,
        expected_revision: TimelineRevision,
        expected_generation: u64,
    ) -> Result<(), TimelineAuthoringError> {
        if expected_revision == self.revision && expected_generation == self.generation {
            return Ok(());
        }
        Err(TimelineAuthoringError::Stale {
            expected_revision,
            expected_generation,
            actual_revision: self.revision,
            actual_generation: self.generation,
        })
    }
}

fn candidate_from_commands(
    document: &TimelineDocument,
    commands: impl IntoIterator<Item = TimelineAuthoringCommand>,
) -> Result<TimelineDocument, TimelineAuthoringError> {
    let mut candidate = document.clone();
    for command in commands {
        apply_command(&mut candidate, &command)?;
    }
    Ok(candidate)
}

fn document_diff(
    before: &TimelineDocument,
    after: &TimelineDocument,
) -> Vec<TimelineAuthoringChange> {
    if before == after {
        Vec::new()
    } else {
        vec![TimelineAuthoringChange {
            before: before.clone(),
            after: after.clone(),
        }]
    }
}

fn has_errors(diagnostics: &[TimelineDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == TimelineDiagnosticSeverity::Error)
}

fn apply_command(
    doc: &mut TimelineDocument,
    command: &TimelineAuthoringCommand,
) -> Result<(), TimelineAuthoringError> {
    match command {
        TimelineAuthoringCommand::AddTrack(track) => doc.tracks.push(track.clone()),
        TimelineAuthoringCommand::RemoveTrack(id) => {
            let before = doc.tracks.len();
            doc.tracks.retain(|value| &value.id != id);
            if before == doc.tracks.len() {
                return Err(TimelineAuthoringError::MissingTarget(id.to_string()));
            }
        }
        TimelineAuthoringCommand::SetBinding { track, binding } => {
            find_track_mut(doc, track)?.binding = binding.clone();
        }
        TimelineAuthoringCommand::SetTrackEnabled { track, enabled } => {
            find_track_mut(doc, track)?.enabled = *enabled;
        }
        TimelineAuthoringCommand::AddClip { track, clip } => {
            find_track_mut(doc, track)?.clips.push(clip.clone());
        }
        TimelineAuthoringCommand::RemoveClip(id) => {
            let mut found = false;
            for track in &mut doc.tracks {
                let before = track.clips.len();
                track.clips.retain(|value| &value.id != id);
                found |= before != track.clips.len();
            }
            if !found {
                return Err(TimelineAuthoringError::MissingTarget(id.to_string()));
            }
        }
        TimelineAuthoringCommand::MoveClip { clip, start } => {
            find_clip_mut(doc, clip)?.start = *start;
        }
        TimelineAuthoringCommand::ResizeClip { clip, duration } => {
            find_clip_mut(doc, clip)?.duration = *duration;
        }
        TimelineAuthoringCommand::AddMarker(marker) => doc.markers.push(marker.clone()),
        TimelineAuthoringCommand::RemoveMarker(id) => {
            let before = doc.markers.len();
            doc.markers.retain(|marker| &marker.id != id);
            if before == doc.markers.len() {
                return Err(TimelineAuthoringError::MissingTarget(id.to_string()));
            }
        }
        TimelineAuthoringCommand::MoveMarker { marker, tick } => {
            let item = doc
                .markers
                .iter_mut()
                .find(|item| &item.id == marker)
                .ok_or_else(|| TimelineAuthoringError::MissingTarget(marker.to_string()))?;
            item.tick = *tick;
        }
    }
    Ok(())
}

fn find_track_mut<'a>(
    doc: &'a mut TimelineDocument,
    id: &TimelineTrackId,
) -> Result<&'a mut TimelineTrack, TimelineAuthoringError> {
    doc.tracks
        .iter_mut()
        .find(|value| &value.id == id)
        .ok_or_else(|| TimelineAuthoringError::MissingTarget(id.to_string()))
}

fn find_clip_mut<'a>(
    doc: &'a mut TimelineDocument,
    id: &TimelineClipId,
) -> Result<&'a mut TimelineClip, TimelineAuthoringError> {
    for track in &mut doc.tracks {
        if let Some(index) = track.clips.iter().position(|value| &value.id == id) {
            return Ok(&mut track.clips[index]);
        }
    }
    Err(TimelineAuthoringError::MissingTarget(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::*;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    fn event_track() -> TimelineTrack {
        TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "event".into(),
            kind: TimelineTrackKind::Event,
            enabled: true,
            binding: None,
            clips: Vec::new(),
        }
    }

    #[test]
    fn stale_revision_rejected_and_undo_redo_work() {
        let doc = TimelineDocument::new("x", TimelineTick::new(100));
        let mut service = TimelineAuthoringService::new(doc).unwrap();
        service
            .apply(0, TimelineAuthoringCommand::AddTrack(event_track()))
            .unwrap();
        assert!(matches!(
            service.apply(
                0,
                TimelineAuthoringCommand::AddMarker(TimelineMarker {
                    id: TimelineMarkerId::generate(),
                    name: "m".into(),
                    tick: TimelineTick::ZERO,
                    event: None,
                })
            ),
            Err(TimelineAuthoringError::StaleRevision { .. })
        ));
        assert!(service.undo());
        assert!(service.document().tracks.is_empty());
        assert!(service.redo());
        assert_eq!(service.document().tracks.len(), 1);
    }

    #[test]
    fn structured_preview_is_non_destructive_and_apply_advances_live_base() {
        let doc = TimelineDocument::new("x", TimelineTick::new(100));
        let mut service = TimelineAuthoringService::new(doc).unwrap();
        let base = service.inspect(&writable()).unwrap();
        let command = TimelineAuthoringCommand::AddTrack(event_track());

        let preview = service
            .preview_commands(
                &writable(),
                base.revision,
                base.generation,
                vec![command.clone()],
            )
            .unwrap();
        assert!(preview.success);
        assert_eq!(preview.diff.len(), 1);
        assert!(service.document().tracks.is_empty());

        let applied = service
            .apply_commands(
                &writable(),
                base.revision,
                base.generation,
                vec![command],
            )
            .unwrap();
        assert!(applied.success);
        assert_eq!(applied.revision, base.revision + 1);
        assert_ne!(applied.generation, base.generation);
        assert_eq!(service.document().tracks.len(), 1);
    }

    #[test]
    fn structured_apply_rejects_stale_generation_and_missing_permission() {
        let doc = TimelineDocument::new("x", TimelineTick::new(100));
        let mut service = TimelineAuthoringService::new(doc).unwrap();
        let base = service.inspect(&writable()).unwrap();

        let denied = service.apply_commands(
            &AuthoringPermissions::read_only(),
            base.revision,
            base.generation,
            vec![TimelineAuthoringCommand::AddTrack(event_track())],
        );
        assert!(matches!(
            denied,
            Err(TimelineAuthoringError::Permission(_))
        ));

        service
            .apply_commands(
                &writable(),
                base.revision,
                base.generation,
                vec![TimelineAuthoringCommand::AddTrack(event_track())],
            )
            .unwrap();
        let stale = service.apply_commands(
            &writable(),
            base.revision,
            base.generation,
            Vec::new(),
        );
        assert!(matches!(stale, Err(TimelineAuthoringError::Stale { .. })));
    }
}
