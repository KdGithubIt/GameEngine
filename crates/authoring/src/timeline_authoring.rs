//! Shared transactional Timeline authoring service used by GUI/CLI/MCP adapters.

use crate::id::{TimelineClipId, TimelineMarkerId, TimelineTrackId};
use crate::timeline::{save_timeline, validate_timeline, TimelineBinding, TimelineClip, TimelineDiagnostic, TimelineDocument, TimelineMarker, TimelineTrack};
use engine_timeline::TimelineTick;
use std::fmt;
use std::path::Path;

/// Optimistic revision token.
pub type TimelineRevision = u64;

/// One typed Timeline mutation.
#[derive(Debug, Clone)]
pub enum TimelineAuthoringCommand {
    /// Add a typed track.
    AddTrack(TimelineTrack),
    /// Remove a track.
    RemoveTrack(TimelineTrackId),
    /// Change a track stable binding.
    SetBinding { /// Track.
        track: TimelineTrackId, /// Binding.
        binding: Option<TimelineBinding> },
    /// Change persisted enabled state.
    SetTrackEnabled { /// Track.
        track: TimelineTrackId, /// Enabled.
        enabled: bool },
    /// Add a typed clip to a track.
    AddClip { /// Track.
        track: TimelineTrackId, /// Clip.
        clip: TimelineClip },
    /// Delete a clip.
    RemoveClip(TimelineClipId),
    /// Move a clip without changing duration.
    MoveClip { /// Clip.
        clip: TimelineClipId, /// New start.
        start: TimelineTick },
    /// Resize a clip.
    ResizeClip { /// Clip.
        clip: TimelineClipId, /// New duration.
        duration: TimelineTick },
    /// Add marker lane entry.
    AddMarker(TimelineMarker),
    /// Remove marker.
    RemoveMarker(TimelineMarkerId),
    /// Move marker.
    MoveMarker { /// Marker.
        marker: TimelineMarkerId, /// Tick.
        tick: TimelineTick },
}

/// Preview result without committed mutation.
#[derive(Debug, Clone)]
pub struct TimelineAuthoringPreview {
    /// Resulting candidate.
    pub document: TimelineDocument,
    /// Pure validation diagnostics.
    pub diagnostics: Vec<TimelineDiagnostic>,
}

/// Authoring error.
#[derive(Debug)]
pub enum TimelineAuthoringError {
    /// Caller edited an obsolete revision.
    StaleRevision { /// Expected current revision.
        expected: TimelineRevision, /// Supplied revision.
        actual: TimelineRevision },
    /// Target stable ID no longer exists.
    MissingTarget(String),
    /// Mutation made the document invalid.
    Invalid(Vec<TimelineDiagnostic>),
    /// Persistence failed.
    Persist(String),
}
impl fmt::Display for TimelineAuthoringError { fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result { match self { Self::StaleRevision{expected,actual}=>write!(f,"stale Timeline revision {actual}; current is {expected}"), Self::MissingTarget(id)=>write!(f,"Timeline target {id} is missing"), Self::Invalid(d)=>write!(f,"Timeline mutation has {} validation diagnostics",d.len()), Self::Persist(e)=>f.write_str(e) } } }
impl std::error::Error for TimelineAuthoringError {}

/// Shared stateful authoring service with stale-revision rejection and session undo/redo.
#[derive(Debug, Clone)]
pub struct TimelineAuthoringService { document: TimelineDocument, revision: TimelineRevision, undo: Vec<TimelineDocument>, redo: Vec<TimelineDocument> }
impl TimelineAuthoringService {
    /// Starts a session around a current-format document.
    pub fn new(document: TimelineDocument) -> Result<Self, TimelineAuthoringError> { let diagnostics=validate_timeline(&document); if diagnostics.iter().any(|d|matches!(d.severity,crate::timeline::TimelineDiagnosticSeverity::Error)){return Err(TimelineAuthoringError::Invalid(diagnostics))} Ok(Self{document,revision:0,undo:Vec::new(),redo:Vec::new()}) }
    /// Current immutable document.
    pub const fn document(&self)->&TimelineDocument{&self.document}
    /// Current optimistic revision.
    pub const fn revision(&self)->TimelineRevision{self.revision}
    /// Validates a command against a clone without mutation.
    pub fn preview(&self, expected:TimelineRevision, command:&TimelineAuthoringCommand)->Result<TimelineAuthoringPreview,TimelineAuthoringError>{ self.check_revision(expected)?; let mut candidate=self.document.clone(); apply_command(&mut candidate,command)?; let diagnostics=validate_timeline(&candidate); Ok(TimelineAuthoringPreview{document:candidate,diagnostics}) }
    /// Atomically applies one command after validation.
    pub fn apply(&mut self, expected:TimelineRevision, command:TimelineAuthoringCommand)->Result<TimelineRevision,TimelineAuthoringError>{
        self.apply_transaction(expected, std::iter::once(command))
    }
    /// Atomically applies a granular command transaction with one undo boundary.
    ///
    /// Every command is evaluated against the candidate produced by the prior
    /// command. The committed document is replaced only after the complete
    /// candidate validates, so GUI drag gestures, CLI batches, and external
    /// authoring adapters share identical all-or-nothing semantics.
    pub fn apply_transaction(
        &mut self,
        expected: TimelineRevision,
        commands: impl IntoIterator<Item = TimelineAuthoringCommand>,
    ) -> Result<TimelineRevision, TimelineAuthoringError> {
        self.check_revision(expected)?;
        let mut candidate = self.document.clone();
        for command in commands {
            apply_command(&mut candidate, &command)?;
        }
        let diagnostics = validate_timeline(&candidate);
        if diagnostics.iter().any(|d| matches!(d.severity, crate::timeline::TimelineDiagnosticSeverity::Error)) {
            return Err(TimelineAuthoringError::Invalid(diagnostics));
        }
        self.undo.push(self.document.clone());
        self.document = candidate;
        self.redo.clear();
        self.revision = self.revision.wrapping_add(1);
        Ok(self.revision)
    }
    /// Undoes one committed transaction.
    pub fn undo(&mut self)->bool{ let Some(previous)=self.undo.pop()else{return false}; self.redo.push(std::mem::replace(&mut self.document,previous)); self.revision=self.revision.wrapping_add(1); true }
    /// Redoes one undone transaction.
    pub fn redo(&mut self)->bool{ let Some(next)=self.redo.pop()else{return false}; self.undo.push(std::mem::replace(&mut self.document,next)); self.revision=self.revision.wrapping_add(1); true }
    /// Persists the committed source of truth.
    pub fn save(&self,path:&Path)->Result<(),TimelineAuthoringError>{ save_timeline(path,&self.document).map_err(|e|TimelineAuthoringError::Persist(e.to_string())) }
    fn check_revision(&self,actual:TimelineRevision)->Result<(),TimelineAuthoringError>{ if actual==self.revision{Ok(())}else{Err(TimelineAuthoringError::StaleRevision{expected:self.revision,actual})} }
}

fn apply_command(doc:&mut TimelineDocument,command:&TimelineAuthoringCommand)->Result<(),TimelineAuthoringError>{ match command { TimelineAuthoringCommand::AddTrack(track)=>doc.tracks.push(track.clone()), TimelineAuthoringCommand::RemoveTrack(id)=>{let before=doc.tracks.len();doc.tracks.retain(|v|&v.id!=id);if before==doc.tracks.len(){return Err(TimelineAuthoringError::MissingTarget(id.to_string()))}}, TimelineAuthoringCommand::SetBinding{track,binding}=>find_track_mut(doc,track)?.binding=binding.clone(), TimelineAuthoringCommand::SetTrackEnabled{track,enabled}=>find_track_mut(doc,track)?.enabled=*enabled, TimelineAuthoringCommand::AddClip{track,clip}=>find_track_mut(doc,track)?.clips.push(clip.clone()), TimelineAuthoringCommand::RemoveClip(id)=>{let mut found=false;for track in &mut doc.tracks{let before=track.clips.len();track.clips.retain(|v|&v.id!=id);found|=before!=track.clips.len();}if !found{return Err(TimelineAuthoringError::MissingTarget(id.to_string()))}}, TimelineAuthoringCommand::MoveClip{clip,start}=>find_clip_mut(doc,clip)?.start=*start, TimelineAuthoringCommand::ResizeClip{clip,duration}=>find_clip_mut(doc,clip)?.duration=*duration, TimelineAuthoringCommand::AddMarker(marker)=>doc.markers.push(marker.clone()), TimelineAuthoringCommand::RemoveMarker(id)=>{let before=doc.markers.len();doc.markers.retain(|m|&m.id!=id);if before==doc.markers.len(){return Err(TimelineAuthoringError::MissingTarget(id.to_string()))}}, TimelineAuthoringCommand::MoveMarker{marker,tick}=>{let item=doc.markers.iter_mut().find(|m|&m.id==marker).ok_or_else(||TimelineAuthoringError::MissingTarget(marker.to_string()))?;item.tick=*tick;} } Ok(()) }
fn find_track_mut<'a>(doc:&'a mut TimelineDocument,id:&TimelineTrackId)->Result<&'a mut TimelineTrack,TimelineAuthoringError>{doc.tracks.iter_mut().find(|v|&v.id==id).ok_or_else(||TimelineAuthoringError::MissingTarget(id.to_string()))}
fn find_clip_mut<'a>(doc:&'a mut TimelineDocument,id:&TimelineClipId)->Result<&'a mut TimelineClip,TimelineAuthoringError>{for track in &mut doc.tracks{if let Some(index)=track.clips.iter().position(|v|&v.id==id){return Ok(&mut track.clips[index])}}Err(TimelineAuthoringError::MissingTarget(id.to_string()))}

#[cfg(test)] mod tests { use super::*; use crate::timeline::*; #[test] fn stale_revision_rejected_and_undo_redo_work(){let doc=TimelineDocument::new("x",TimelineTick::new(100));let mut service=TimelineAuthoringService::new(doc).unwrap();let track=TimelineTrack{id:TimelineTrackId::generate(),name:"event".into(),kind:TimelineTrackKind::Event,enabled:true,binding:None,clips:Vec::new()};service.apply(0,TimelineAuthoringCommand::AddTrack(track)).unwrap();assert!(matches!(service.apply(0,TimelineAuthoringCommand::AddMarker(TimelineMarker{id:TimelineMarkerId::generate(),name:"m".into(),tick:TimelineTick::ZERO,event:None})),Err(TimelineAuthoringError::StaleRevision{..})));assert!(service.undo());assert!(service.document().tracks.is_empty());assert!(service.redo());assert_eq!(service.document().tracks.len(),1);} }
