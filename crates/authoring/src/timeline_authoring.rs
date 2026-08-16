//! GUI-free transactional Timeline authoring service (ADR 0121, ADR 0126).
//!
//! Editor, CLI, MCP, tests, and future adapters can share this service without
//! importing any GUI toolkit. Preview is non-destructive, apply is atomic, and
//! the caller must present both revision and in-memory generation to prevent
//! stale/ABA writes across reloads.

use crate::access::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
use crate::diagnostic::Diagnostic;
use crate::id::{TimelineClipId, TimelineMarkerId, TimelineTrackId};
use crate::timeline::{
    TimelineAnimationClip, TimelineCameraCutClip, TimelineDisplayRate, TimelineDocument,
    TimelineEventMarker, TimelineTrack, TimelineTrackKind, TimelineTrackType,
    TimelineTransformClip,
};
use engine_timeline::TimelineTick;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Strongly typed interval clip accepted by [`TimelineCommand`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TimelineEditableClip {
    /// Animation Track clip.
    Animation(TimelineAnimationClip),
    /// Transform/Property Track clip.
    TransformProperty(TimelineTransformClip),
    /// Camera Cut Track clip.
    CameraCut(TimelineCameraCutClip),
}

impl TimelineEditableClip {
    /// Returns this clip's stable identity.
    pub fn id(&self) -> &TimelineClipId {
        match self {
            Self::Animation(clip) => &clip.timing.id,
            Self::TransformProperty(clip) => &clip.timing.id,
            Self::CameraCut(clip) => &clip.timing.id,
        }
    }

    /// Returns the required destination track family.
    pub const fn track_type(&self) -> TimelineTrackType {
        match self {
            Self::Animation(_) => TimelineTrackType::Animation,
            Self::TransformProperty(_) => TimelineTrackType::TransformProperty,
            Self::CameraCut(_) => TimelineTrackType::CameraCut,
        }
    }
}

/// Granular Timeline document edit command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TimelineCommand {
    /// Changes the human-readable document name.
    Rename {
        /// New document name.
        name: String,
    },
    /// Changes the exact Timeline duration.
    SetDuration {
        /// New inclusive final tick.
        duration_ticks: TimelineTick,
    },
    /// Changes frame/timecode presentation without changing canonical ticks.
    SetDisplayRate {
        /// New rational display frame rate.
        display_rate: TimelineDisplayRate,
    },
    /// Adds one complete typed track.
    AddTrack {
        /// Track to add.
        track: TimelineTrack,
    },
    /// Removes one track and its owned clips/markers.
    RemoveTrack {
        /// Stable track ID.
        track: TimelineTrackId,
    },
    /// Renames one track without changing its stable identity.
    SetTrackName {
        /// Stable track ID.
        track: TimelineTrackId,
        /// New human-readable name.
        name: String,
    },
    /// Enables or disables runtime compilation of one track.
    SetTrackEnabled {
        /// Stable track ID.
        track: TimelineTrackId,
        /// New persisted enabled state.
        enabled: bool,
    },
    /// Changes one track's persisted ordering key.
    SetTrackOrder {
        /// Stable track ID.
        track: TimelineTrackId,
        /// New ordering key.
        order: i32,
    },
    /// Adds one typed interval clip to a matching track family.
    AddClip {
        /// Destination track.
        track: TimelineTrackId,
        /// Strongly typed clip.
        clip: TimelineEditableClip,
    },
    /// Replaces one interval clip while preserving the addressed stable ID.
    ReplaceClip {
        /// Owning track.
        track: TimelineTrackId,
        /// Replacement typed clip. Its ID must match `clip`.
        clip: TimelineClipId,
        /// New clip data.
        replacement: TimelineEditableClip,
    },
    /// Removes one interval clip.
    RemoveClip {
        /// Owning track.
        track: TimelineTrackId,
        /// Stable clip ID.
        clip: TimelineClipId,
    },
    /// Adds one sequence-level Event marker to an Event Track.
    AddEventMarker {
        /// Destination Event Track.
        track: TimelineTrackId,
        /// Typed stable marker.
        marker: TimelineEventMarker,
    },
    /// Replaces one Event marker while preserving its stable ID.
    ReplaceEventMarker {
        /// Owning Event Track.
        track: TimelineTrackId,
        /// Stable marker ID being replaced.
        marker: TimelineMarkerId,
        /// Replacement marker data. Its ID must match `marker`.
        replacement: TimelineEventMarker,
    },
    /// Removes one Event marker.
    RemoveEventMarker {
        /// Owning Event Track.
        track: TimelineTrackId,
        /// Stable marker ID.
        marker: TimelineMarkerId,
    },
}

/// Deterministic semantic diff item produced by a Timeline transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineChange {
    /// Document name changed.
    DocumentRenamed,
    /// Document duration changed.
    DurationChanged,
    /// Display frame rate changed.
    DisplayRateChanged,
    /// Track was added.
    TrackAdded {
        /// Stable track ID.
        track: TimelineTrackId,
    },
    /// Track was removed.
    TrackRemoved {
        /// Stable track ID.
        track: TimelineTrackId,
    },
    /// Track metadata changed.
    TrackChanged {
        /// Stable track ID.
        track: TimelineTrackId,
    },
    /// Interval clip was added.
    ClipAdded {
        /// Owning track.
        track: TimelineTrackId,
        /// Stable clip ID.
        clip: TimelineClipId,
    },
    /// Interval clip was replaced.
    ClipChanged {
        /// Owning track.
        track: TimelineTrackId,
        /// Stable clip ID.
        clip: TimelineClipId,
    },
    /// Interval clip was removed.
    ClipRemoved {
        /// Owning track.
        track: TimelineTrackId,
        /// Stable clip ID.
        clip: TimelineClipId,
    },
    /// Event marker was added.
    MarkerAdded {
        /// Owning Event Track.
        track: TimelineTrackId,
        /// Stable marker ID.
        marker: TimelineMarkerId,
    },
    /// Event marker was replaced.
    MarkerChanged {
        /// Owning Event Track.
        track: TimelineTrackId,
        /// Stable marker ID.
        marker: TimelineMarkerId,
    },
    /// Event marker was removed.
    MarkerRemoved {
        /// Owning Event Track.
        track: TimelineTrackId,
        /// Stable marker ID.
        marker: TimelineMarkerId,
    },
}

/// One GUI-free atomic edit transaction over a Timeline document.
pub struct TimelineTransaction {
    working: TimelineDocument,
    changes: Vec<TimelineChange>,
}

impl TimelineTransaction {
    /// Begins a transaction from a committed document snapshot.
    pub fn begin(document: &TimelineDocument) -> Self {
        Self {
            working: document.clone(),
            changes: Vec::new(),
        }
    }

    /// Returns the transaction's current working document.
    pub fn document(&self) -> &TimelineDocument {
        &self.working
    }

    /// Returns the deterministic semantic changes accumulated so far.
    pub fn changes(&self) -> &[TimelineChange] {
        &self.changes
    }

    /// Applies one typed granular edit to the working copy.
    ///
    /// # Errors
    ///
    /// Returns a stable edit error without changing the working copy when the
    /// addressed object is missing, duplicated, or the clip/track family is
    /// incompatible.
    pub fn apply(&mut self, command: TimelineCommand) -> Result<(), TimelineEditError> {
        match command {
            TimelineCommand::Rename { name } => {
                self.working.name = name;
                self.changes.push(TimelineChange::DocumentRenamed);
            }
            TimelineCommand::SetDuration { duration_ticks } => {
                self.working.duration_ticks = duration_ticks;
                self.changes.push(TimelineChange::DurationChanged);
            }
            TimelineCommand::SetDisplayRate { display_rate } => {
                self.working.display_rate = display_rate;
                self.changes.push(TimelineChange::DisplayRateChanged);
            }
            TimelineCommand::AddTrack { track } => {
                if self.working.tracks.iter().any(|candidate| candidate.id == track.id) {
                    return Err(TimelineEditError::DuplicateTrackId(track.id));
                }
                let id = track.id.clone();
                self.working.tracks.push(track);
                self.changes.push(TimelineChange::TrackAdded { track: id });
            }
            TimelineCommand::RemoveTrack { track } => {
                let index = track_index(&self.working, &track)?;
                self.working.tracks.remove(index);
                self.changes.push(TimelineChange::TrackRemoved { track });
            }
            TimelineCommand::SetTrackName { track, name } => {
                track_mut(&mut self.working, &track)?.name = name;
                self.changes.push(TimelineChange::TrackChanged { track });
            }
            TimelineCommand::SetTrackEnabled { track, enabled } => {
                track_mut(&mut self.working, &track)?.enabled = enabled;
                self.changes.push(TimelineChange::TrackChanged { track });
            }
            TimelineCommand::SetTrackOrder { track, order } => {
                track_mut(&mut self.working, &track)?.order = order;
                self.changes.push(TimelineChange::TrackChanged { track });
            }
            TimelineCommand::AddClip { track, clip } => {
                ensure_clip_id_absent(&self.working, clip.id())?;
                let id = clip.id().clone();
                insert_clip(track_mut(&mut self.working, &track)?, clip)?;
                self.changes.push(TimelineChange::ClipAdded { track, clip: id });
            }
            TimelineCommand::ReplaceClip {
                track,
                clip,
                replacement,
            } => {
                if replacement.id() != &clip {
                    return Err(TimelineEditError::ReplacementClipIdMismatch {
                        expected: clip,
                        actual: replacement.id().clone(),
                    });
                }
                replace_clip(track_mut(&mut self.working, &track)?, &clip, replacement)?;
                self.changes.push(TimelineChange::ClipChanged { track, clip });
            }
            TimelineCommand::RemoveClip { track, clip } => {
                remove_clip(track_mut(&mut self.working, &track)?, &clip)?;
                self.changes.push(TimelineChange::ClipRemoved { track, clip });
            }
            TimelineCommand::AddEventMarker { track, marker } => {
                ensure_marker_id_absent(&self.working, &marker.id)?;
                let marker_id = marker.id.clone();
                insert_marker(track_mut(&mut self.working, &track)?, marker)?;
                self.changes.push(TimelineChange::MarkerAdded {
                    track,
                    marker: marker_id,
                });
            }
            TimelineCommand::ReplaceEventMarker {
                track,
                marker,
                replacement,
            } => {
                if replacement.id != marker {
                    return Err(TimelineEditError::ReplacementMarkerIdMismatch {
                        expected: marker,
                        actual: replacement.id.clone(),
                    });
                }
                replace_marker(track_mut(&mut self.working, &track)?, &marker, replacement)?;
                self.changes.push(TimelineChange::MarkerChanged { track, marker });
            }
            TimelineCommand::RemoveEventMarker { track, marker } => {
                remove_marker(track_mut(&mut self.working, &track)?, &marker)?;
                self.changes.push(TimelineChange::MarkerRemoved { track, marker });
            }
        }
        Ok(())
    }

    /// Finalizes this transaction after whole-document validation.
    ///
    /// # Errors
    ///
    /// Returns deterministic blocking diagnostics and no document when the
    /// final working copy is invalid.
    pub fn commit(self) -> Result<TimelineTransactionCommit, TimelineCommitError> {
        let diagnostics = self.working.validate();
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(TimelineCommitError { diagnostics });
        }
        Ok(TimelineTransactionCommit {
            document: self.working,
            changes: self.changes,
            diagnostics,
        })
    }
}

/// Successful transaction output consumed by preview/apply services.
pub struct TimelineTransactionCommit {
    /// Validated replacement document.
    pub document: TimelineDocument,
    /// Deterministic semantic changes.
    pub changes: Vec<TimelineChange>,
    /// Non-blocking diagnostics retained after validation.
    pub diagnostics: Vec<Diagnostic>,
}

/// Whole-document Timeline transaction validation failure.
#[derive(Debug)]
pub struct TimelineCommitError {
    /// Deterministic diagnostics; at least one is blocking.
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for TimelineCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Timeline transaction failed with {} validation diagnostic(s)",
            self.diagnostics.len()
        )
    }
}

impl std::error::Error for TimelineCommitError {}

/// Stable granular Timeline edit failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineEditError {
    /// Track ID does not exist.
    TrackNotFound(TimelineTrackId),
    /// New track duplicates an existing stable ID.
    DuplicateTrackId(TimelineTrackId),
    /// New clip duplicates an existing stable ID anywhere in the document.
    DuplicateClipId(TimelineClipId),
    /// New marker duplicates an existing stable ID anywhere in the document.
    DuplicateMarkerId(TimelineMarkerId),
    /// Clip ID does not exist on the addressed track.
    ClipNotFound(TimelineClipId),
    /// Marker ID does not exist on the addressed Event Track.
    MarkerNotFound(TimelineMarkerId),
    /// A typed clip was sent to the wrong track family.
    TrackTypeMismatch {
        /// Actual destination track family.
        track_type: TimelineTrackType,
        /// Family required by the payload.
        payload_type: TimelineTrackType,
    },
    /// Marker command addressed a non-Event track.
    MarkerRequiresEventTrack {
        /// Actual destination family.
        track_type: TimelineTrackType,
    },
    /// Replacement clip attempted to change stable identity.
    ReplacementClipIdMismatch {
        /// Addressed stable ID.
        expected: TimelineClipId,
        /// ID carried by replacement payload.
        actual: TimelineClipId,
    },
    /// Replacement marker attempted to change stable identity.
    ReplacementMarkerIdMismatch {
        /// Addressed stable ID.
        expected: TimelineMarkerId,
        /// ID carried by replacement payload.
        actual: TimelineMarkerId,
    },
}

impl fmt::Display for TimelineEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TrackNotFound(id) => write!(formatter, "Timeline track `{id}` was not found"),
            Self::DuplicateTrackId(id) => write!(formatter, "Timeline track `{id}` already exists"),
            Self::DuplicateClipId(id) => write!(formatter, "Timeline clip `{id}` already exists"),
            Self::DuplicateMarkerId(id) => write!(formatter, "Timeline marker `{id}` already exists"),
            Self::ClipNotFound(id) => write!(formatter, "Timeline clip `{id}` was not found"),
            Self::MarkerNotFound(id) => write!(formatter, "Timeline marker `{id}` was not found"),
            Self::TrackTypeMismatch {
                track_type,
                payload_type,
            } => write!(
                formatter,
                "Timeline payload type {payload_type:?} cannot be inserted into {track_type:?} track"
            ),
            Self::MarkerRequiresEventTrack { track_type } => write!(
                formatter,
                "Timeline marker command requires Event track, found {track_type:?}"
            ),
            Self::ReplacementClipIdMismatch { expected, actual } => write!(
                formatter,
                "replacement Timeline clip ID `{actual}` does not match addressed ID `{expected}`"
            ),
            Self::ReplacementMarkerIdMismatch { expected, actual } => write!(
                formatter,
                "replacement Timeline marker ID `{actual}` does not match addressed ID `{expected}`"
            ),
        }
    }
}

impl std::error::Error for TimelineEditError {}

/// Live Timeline authoring state used by structured adapters.
///
/// Generation is process-local and intentionally not serialized. Reopening the
/// same file receives a new generation even when its logical revision is zero,
/// so stale previews cannot apply across reload/undo ABA cycles.
pub struct TimelineAuthoringSession {
    document: TimelineDocument,
    generation: u64,
    revision: u64,
}

impl TimelineAuthoringSession {
    /// Creates a live session around one committed Timeline document.
    pub fn new(document: TimelineDocument) -> Self {
        Self {
            document,
            generation: next_timeline_generation(),
            revision: 0,
        }
    }

    /// Returns the committed Timeline document.
    pub fn document(&self) -> &TimelineDocument {
        &self.document
    }

    fn commit(&mut self, document: TimelineDocument) {
        self.document = document;
        self.revision = self.revision.saturating_add(1);
    }
}

/// Immutable committed Timeline snapshot returned to adapters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringSnapshot {
    /// Logical content revision.
    pub revision: u64,
    /// Process-local generation protecting against stale reload/undo bases.
    pub generation: u64,
    /// Complete committed current-format document.
    pub document: TimelineDocument,
}

/// Whole-document Timeline validation result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringValidation {
    /// Revision validated by this result.
    pub revision: u64,
    /// Generation validated by this result.
    pub generation: u64,
    /// Whether no blocking diagnostic was produced.
    pub success: bool,
    /// Deterministic structural diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of previewing or applying one atomic Timeline command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineAuthoringMutation {
    /// Whether the complete batch validated successfully.
    pub success: bool,
    /// Revision supplied by the caller.
    pub base_revision: u64,
    /// Generation supplied by the caller.
    pub base_generation: u64,
    /// Current committed revision after this operation.
    pub revision: u64,
    /// Current generation after this operation.
    pub generation: u64,
    /// Structured command/whole-document diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Deterministic semantic changes proposed or committed.
    pub diff: Vec<TimelineChange>,
}

/// Shared GUI-free Timeline authoring service failure.
#[derive(Debug)]
pub enum TimelineAuthoringError {
    /// Shared authoring permission failure.
    Permission(AuthoringPermissionError),
    /// Caller revision/generation no longer names the live document state.
    Stale {
        /// Revision supplied by caller.
        expected_revision: u64,
        /// Generation supplied by caller.
        expected_generation: u64,
        /// Current live revision.
        actual_revision: u64,
        /// Current live generation.
        actual_generation: u64,
    },
}

impl TimelineAuthoringError {
    /// Returns a stable diagnostic-style adapter error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::Stale { .. } => "authoring.stale_revision",
        }
    }
}

impl fmt::Display for TimelineAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(error) => error.fmt(formatter),
            Self::Stale {
                expected_revision,
                expected_generation,
                actual_revision,
                actual_generation,
            } => write!(
                formatter,
                "stale Timeline base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
        }
    }
}

impl std::error::Error for TimelineAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Stale { .. } => None,
        }
    }
}

impl From<AuthoringPermissionError> for TimelineAuthoringError {
    fn from(value: AuthoringPermissionError) -> Self {
        Self::Permission(value)
    }
}

/// Stateless GUI-free Timeline authoring behavior shared by all adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct TimelineAuthoringService;

impl TimelineAuthoringService {
    /// Creates the stateless service.
    pub const fn new() -> Self {
        Self
    }

    /// Inspects committed Timeline state.
    ///
    /// # Errors
    ///
    /// Returns a permission error when read access is absent.
    pub fn inspect(
        &self,
        session: &TimelineAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringSnapshot, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(TimelineAuthoringSnapshot {
            revision: session.revision,
            generation: session.generation,
            document: session.document.clone(),
        })
    }

    /// Validates the committed Timeline without mutation.
    ///
    /// # Errors
    ///
    /// Returns a permission error when read access is absent.
    pub fn validate(
        &self,
        session: &TimelineAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringValidation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = session.document.validate();
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        Ok(TimelineAuthoringValidation {
            revision: session.revision,
            generation: session.generation,
            success,
            diagnostics,
        })
    }

    /// Previews one atomic command batch without mutating live state.
    ///
    /// # Errors
    ///
    /// Returns a permission or stale revision/generation error.
    pub fn preview(
        &self,
        session: &TimelineAuthoringSession,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<TimelineCommand>,
    ) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        ensure_current(session, expected_revision, expected_generation)?;
        Ok(evaluate(&session.document, commands).mutation(
            expected_revision,
            expected_generation,
            false,
        ))
    }

    /// Applies one atomic command batch to live Timeline state.
    ///
    /// Invalid commands/final validation leave the committed document
    /// unchanged. A successful empty/no-op batch does not advance revision.
    ///
    /// # Errors
    ///
    /// Returns a permission or stale revision/generation error.
    pub fn apply(
        &self,
        session: &mut TimelineAuthoringSession,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<TimelineCommand>,
    ) -> Result<TimelineAuthoringMutation, TimelineAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        ensure_current(session, expected_revision, expected_generation)?;
        let evaluated = evaluate(&session.document, commands);
        match evaluated {
            EvaluatedTimelineMutation::Accepted {
                diagnostics,
                diff,
                document,
            } => {
                let committed = !diff.is_empty();
                if committed {
                    session.commit(*document);
                }
                Ok(TimelineAuthoringMutation {
                    success: true,
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: session.revision,
                    generation: session.generation,
                    diagnostics,
                    diff,
                })
            }
            EvaluatedTimelineMutation::Rejected { diagnostics, diff } => {
                Ok(TimelineAuthoringMutation {
                    success: false,
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: expected_revision,
                    generation: expected_generation,
                    diagnostics,
                    diff,
                })
            }
        }
    }
}

enum EvaluatedTimelineMutation {
    Accepted {
        diagnostics: Vec<Diagnostic>,
        diff: Vec<TimelineChange>,
        document: Box<TimelineDocument>,
    },
    Rejected {
        diagnostics: Vec<Diagnostic>,
        diff: Vec<TimelineChange>,
    },
}

impl EvaluatedTimelineMutation {
    fn mutation(
        &self,
        base_revision: u64,
        base_generation: u64,
        committed: bool,
    ) -> TimelineAuthoringMutation {
        match self {
            Self::Accepted {
                diagnostics, diff, ..
            } => TimelineAuthoringMutation {
                success: true,
                base_revision,
                base_generation,
                revision: if committed && !diff.is_empty() {
                    base_revision.saturating_add(1)
                } else {
                    base_revision
                },
                generation: base_generation,
                diagnostics: diagnostics.clone(),
                diff: diff.clone(),
            },
            Self::Rejected { diagnostics, diff } => TimelineAuthoringMutation {
                success: false,
                base_revision,
                base_generation,
                revision: base_revision,
                generation: base_generation,
                diagnostics: diagnostics.clone(),
                diff: diff.clone(),
            },
        }
    }
}

fn evaluate(document: &TimelineDocument, commands: Vec<TimelineCommand>) -> EvaluatedTimelineMutation {
    let mut transaction = TimelineTransaction::begin(document);
    for command in commands {
        if let Err(error) = transaction.apply(command) {
            return EvaluatedTimelineMutation::Rejected {
                diagnostics: vec![diagnostic_for_edit_error(&error)],
                diff: transaction.changes().to_vec(),
            };
        }
    }
    let diff = transaction.changes().to_vec();
    match transaction.commit() {
        Ok(commit) => EvaluatedTimelineMutation::Accepted {
            diagnostics: commit.diagnostics,
            diff: commit.changes,
            document: Box::new(commit.document),
        },
        Err(error) => EvaluatedTimelineMutation::Rejected {
            diagnostics: error.diagnostics,
            diff,
        },
    }
}

fn diagnostic_for_edit_error(error: &TimelineEditError) -> Diagnostic {
    let code = match error {
        TimelineEditError::TrackNotFound(_) => "timeline.track_not_found",
        TimelineEditError::DuplicateTrackId(_) => "timeline.duplicate_track_id",
        TimelineEditError::DuplicateClipId(_) => "timeline.duplicate_clip_id",
        TimelineEditError::DuplicateMarkerId(_) => "timeline.duplicate_marker_id",
        TimelineEditError::ClipNotFound(_) => "timeline.clip_not_found",
        TimelineEditError::MarkerNotFound(_) => "timeline.marker_not_found",
        TimelineEditError::TrackTypeMismatch { .. } => "timeline.track_type_mismatch",
        TimelineEditError::MarkerRequiresEventTrack { .. } => {
            "timeline.marker_requires_event_track"
        }
        TimelineEditError::ReplacementClipIdMismatch { .. } => {
            "timeline.replacement_clip_id_mismatch"
        }
        TimelineEditError::ReplacementMarkerIdMismatch { .. } => {
            "timeline.replacement_marker_id_mismatch"
        }
    };
    Diagnostic::error(code, error.to_string())
}

fn ensure_current(
    session: &TimelineAuthoringSession,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), TimelineAuthoringError> {
    if session.revision == expected_revision && session.generation == expected_generation {
        return Ok(());
    }
    Err(TimelineAuthoringError::Stale {
        expected_revision,
        expected_generation,
        actual_revision: session.revision,
        actual_generation: session.generation,
    })
}

fn next_timeline_generation() -> u64 {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn track_index(
    document: &TimelineDocument,
    id: &TimelineTrackId,
) -> Result<usize, TimelineEditError> {
    document
        .tracks
        .iter()
        .position(|track| &track.id == id)
        .ok_or_else(|| TimelineEditError::TrackNotFound(id.clone()))
}

fn track_mut<'a>(
    document: &'a mut TimelineDocument,
    id: &TimelineTrackId,
) -> Result<&'a mut TimelineTrack, TimelineEditError> {
    let index = track_index(document, id)?;
    Ok(&mut document.tracks[index])
}

fn ensure_clip_id_absent(
    document: &TimelineDocument,
    id: &TimelineClipId,
) -> Result<(), TimelineEditError> {
    if document
        .tracks
        .iter()
        .any(|track| track.kind.clip(id).is_some())
    {
        Err(TimelineEditError::DuplicateClipId(id.clone()))
    } else {
        Ok(())
    }
}

fn ensure_marker_id_absent(
    document: &TimelineDocument,
    id: &TimelineMarkerId,
) -> Result<(), TimelineEditError> {
    let exists = document.tracks.iter().any(|track| match &track.kind {
        TimelineTrackKind::Event { markers } => markers.iter().any(|marker| &marker.id == id),
        _ => false,
    });
    if exists {
        Err(TimelineEditError::DuplicateMarkerId(id.clone()))
    } else {
        Ok(())
    }
}

fn insert_clip(track: &mut TimelineTrack, clip: TimelineEditableClip) -> Result<(), TimelineEditError> {
    let track_type = track.kind.track_type();
    let payload_type = clip.track_type();
    match (&mut track.kind, clip) {
        (TimelineTrackKind::Animation { clips }, TimelineEditableClip::Animation(clip)) => {
            clips.push(clip)
        }
        (
            TimelineTrackKind::TransformProperty { clips },
            TimelineEditableClip::TransformProperty(clip),
        ) => clips.push(clip),
        (TimelineTrackKind::CameraCut { clips }, TimelineEditableClip::CameraCut(clip)) => {
            clips.push(clip)
        }
        _ => {
            return Err(TimelineEditError::TrackTypeMismatch {
                track_type,
                payload_type,
            })
        }
    }
    Ok(())
}

fn replace_clip(
    track: &mut TimelineTrack,
    id: &TimelineClipId,
    replacement: TimelineEditableClip,
) -> Result<(), TimelineEditError> {
    let track_type = track.kind.track_type();
    let payload_type = replacement.track_type();
    match (&mut track.kind, replacement) {
        (TimelineTrackKind::Animation { clips }, TimelineEditableClip::Animation(replacement)) => {
            replace_by_id(clips, id, replacement, |clip| &clip.timing.id)
        }
        (
            TimelineTrackKind::TransformProperty { clips },
            TimelineEditableClip::TransformProperty(replacement),
        ) => replace_by_id(clips, id, replacement, |clip| &clip.timing.id),
        (TimelineTrackKind::CameraCut { clips }, TimelineEditableClip::CameraCut(replacement)) => {
            replace_by_id(clips, id, replacement, |clip| &clip.timing.id)
        }
        _ => Err(TimelineEditError::TrackTypeMismatch {
            track_type,
            payload_type,
        }),
    }
}

fn replace_by_id<T>(
    items: &mut [T],
    id: &TimelineClipId,
    replacement: T,
    id_of: impl Fn(&T) -> &TimelineClipId,
) -> Result<(), TimelineEditError> {
    let Some(index) = items.iter().position(|item| id_of(item) == id) else {
        return Err(TimelineEditError::ClipNotFound(id.clone()));
    };
    items[index] = replacement;
    Ok(())
}

fn remove_clip(track: &mut TimelineTrack, id: &TimelineClipId) -> Result<(), TimelineEditError> {
    match &mut track.kind {
        TimelineTrackKind::Animation { clips } => remove_by_id(clips, id, |clip| &clip.timing.id),
        TimelineTrackKind::TransformProperty { clips } => {
            remove_by_id(clips, id, |clip| &clip.timing.id)
        }
        TimelineTrackKind::CameraCut { clips } => remove_by_id(clips, id, |clip| &clip.timing.id),
        TimelineTrackKind::Event { .. } => Err(TimelineEditError::ClipNotFound(id.clone())),
    }
}

fn remove_by_id<T>(
    items: &mut Vec<T>,
    id: &TimelineClipId,
    id_of: impl Fn(&T) -> &TimelineClipId,
) -> Result<(), TimelineEditError> {
    let Some(index) = items.iter().position(|item| id_of(item) == id) else {
        return Err(TimelineEditError::ClipNotFound(id.clone()));
    };
    items.remove(index);
    Ok(())
}

fn insert_marker(
    track: &mut TimelineTrack,
    marker: TimelineEventMarker,
) -> Result<(), TimelineEditError> {
    match &mut track.kind {
        TimelineTrackKind::Event { markers } => {
            markers.push(marker);
            Ok(())
        }
        other => Err(TimelineEditError::MarkerRequiresEventTrack {
            track_type: other.track_type(),
        }),
    }
}

fn replace_marker(
    track: &mut TimelineTrack,
    id: &TimelineMarkerId,
    replacement: TimelineEventMarker,
) -> Result<(), TimelineEditError> {
    match &mut track.kind {
        TimelineTrackKind::Event { markers } => {
            let Some(index) = markers.iter().position(|marker| &marker.id == id) else {
                return Err(TimelineEditError::MarkerNotFound(id.clone()));
            };
            markers[index] = replacement;
            Ok(())
        }
        other => Err(TimelineEditError::MarkerRequiresEventTrack {
            track_type: other.track_type(),
        }),
    }
}

fn remove_marker(
    track: &mut TimelineTrack,
    id: &TimelineMarkerId,
) -> Result<(), TimelineEditError> {
    match &mut track.kind {
        TimelineTrackKind::Event { markers } => {
            let Some(index) = markers.iter().position(|marker| &marker.id == id) else {
                return Err(TimelineEditError::MarkerNotFound(id.clone()));
            };
            markers.remove(index);
            Ok(())
        }
        other => Err(TimelineEditError::MarkerRequiresEventTrack {
            track_type: other.track_type(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{EntityId, TimelineId};
    use crate::timeline::{TimelineCameraCutClip, TimelineClipTiming};

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    fn document() -> TimelineDocument {
        TimelineDocument::new(TimelineId::generate(), "sequence", TimelineTick::new(1_000))
    }

    fn camera_track() -> TimelineTrack {
        TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "camera".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::CameraCut { clips: Vec::new() },
        }
    }

    fn camera_clip(id: TimelineClipId) -> TimelineEditableClip {
        TimelineEditableClip::CameraCut(TimelineCameraCutClip {
            timing: TimelineClipTiming {
                id,
                start_tick: TimelineTick::new(10),
                end_tick: TimelineTick::new(100),
                blend: Default::default(),
            },
            camera: EntityId::generate(),
            override_priority: 7,
        })
    }

    #[test]
    fn preview_is_non_destructive_and_apply_is_one_revision() {
        let service = TimelineAuthoringService::new();
        let mut session = TimelineAuthoringSession::new(document());
        let base = service.inspect(&session, &writable()).expect("inspect");
        let track = camera_track();
        let track_id = track.id.clone();
        let command = TimelineCommand::AddTrack { track };

        let preview = service
            .preview(
                &session,
                &writable(),
                base.revision,
                base.generation,
                vec![command.clone()],
            )
            .expect("preview");
        assert!(preview.success);
        assert_eq!(preview.diff.len(), 1);
        assert!(session.document().track(&track_id).is_none());

        let applied = service
            .apply(
                &mut session,
                &writable(),
                base.revision,
                base.generation,
                vec![command],
            )
            .expect("apply");
        assert!(applied.success);
        assert_eq!(applied.revision, base.revision + 1);
        assert_eq!(applied.generation, base.generation);
        assert!(session.document().track(&track_id).is_some());
    }

    #[test]
    fn command_batch_is_atomic_on_final_validation_failure() {
        let service = TimelineAuthoringService::new();
        let mut session = TimelineAuthoringSession::new(document());
        let base = service.inspect(&session, &writable()).expect("inspect");
        let result = service
            .apply(
                &mut session,
                &writable(),
                base.revision,
                base.generation,
                vec![TimelineCommand::SetDuration {
                    duration_ticks: TimelineTick::new(-1),
                }],
            )
            .expect("validation is structured result");
        assert!(!result.success);
        assert_eq!(session.document().duration_ticks, TimelineTick::new(1_000));
        assert_eq!(result.revision, base.revision);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "timeline.negative_duration"));
    }

    #[test]
    fn stale_generation_rejects_reopened_document() {
        let service = TimelineAuthoringService::new();
        let session = TimelineAuthoringSession::new(document());
        let base = service.inspect(&session, &writable()).expect("inspect");
        let mut reopened = TimelineAuthoringSession::new(session.document().clone());
        let error = service
            .apply(
                &mut reopened,
                &writable(),
                base.revision,
                base.generation,
                Vec::new(),
            )
            .expect_err("reopened generation must reject stale base");
        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn clip_commands_preserve_stable_identity_and_track_type() {
        let mut transaction = TimelineTransaction::begin(&document());
        let track = camera_track();
        let track_id = track.id.clone();
        transaction
            .apply(TimelineCommand::AddTrack { track })
            .expect("add track");
        let clip_id = TimelineClipId::generate();
        transaction
            .apply(TimelineCommand::AddClip {
                track: track_id.clone(),
                clip: camera_clip(clip_id.clone()),
            })
            .expect("add camera clip");
        let replacement = camera_clip(clip_id.clone());
        transaction
            .apply(TimelineCommand::ReplaceClip {
                track: track_id.clone(),
                clip: clip_id.clone(),
                replacement,
            })
            .expect("replace same ID");
        let commit = transaction.commit().expect("valid commit");
        assert!(commit.document.track(&track_id).unwrap().kind.clip(&clip_id).is_some());
    }

    #[test]
    fn wrong_track_family_rejects_typed_clip_without_mutating() {
        let mut base = document();
        let event_track = TimelineTrack {
            id: TimelineTrackId::generate(),
            name: "events".into(),
            enabled: true,
            order: 0,
            kind: TimelineTrackKind::Event { markers: Vec::new() },
        };
        let track_id = event_track.id.clone();
        base.tracks.push(event_track);
        let mut transaction = TimelineTransaction::begin(&base);
        let error = transaction
            .apply(TimelineCommand::AddClip {
                track: track_id,
                clip: camera_clip(TimelineClipId::generate()),
            })
            .expect_err("camera clip cannot enter event track");
        assert!(matches!(error, TimelineEditError::TrackTypeMismatch { .. }));
        assert!(transaction.changes().is_empty());
    }
}
