//! Timeline authoring and dependency-neutral runtime orchestration (ADR 0126).
//!
//! The concrete Animation, Camera, Transform, and Event domains remain owned
//! by their existing modules. This facade exposes the stable authoring model
//! and neutral evaluation requests; composition-layer adapters may translate
//! those requests into domain operations without moving domain logic into the
//! Timeline scheduler.

use crate::audio::{AudioAsset, AudioError, AudioSystem, AudioVoiceId, StereoGains};
use std::collections::{HashMap, HashSet};

pub use engine_authoring::timeline::{
    compile_timeline, CompiledAnimationPayload, CompiledAudioPayload, CompiledCameraCutPayload,
    CompiledEventPayload, CompiledTimelinePayload, CompiledTransformPayload, CompiledVfxPayload,
    TimelineAnimationClip, TimelineAudioClip,
    TimelineBindingResolver, TimelineBlend, TimelineCameraCutClip, TimelineClipRef,
    TimelineClipTiming, TimelineCompileError, TimelineDisplayRate, TimelineDocument,
    TimelineDocumentError, TimelineEventMarker, TimelineQuatKey, TimelineTrack, TimelineTrackKind,
    TimelineTrackType, TimelineTransformClip, TimelineTransformCurve, TimelineTransformSample,
    TimelineVec3Key, TimelineVfxClip, TIMELINE_FILE_SUFFIX, TIMELINE_SCHEMA_VERSION,
};
pub use engine_authoring::timeline_authoring::{
    TimelineAuthoringError, TimelineAuthoringMutation, TimelineAuthoringService,
    TimelineAuthoringSession, TimelineAuthoringSnapshot, TimelineAuthoringValidation,
    TimelineChange, TimelineCommand, TimelineCommitError, TimelineEditError, TimelineEditableClip,
    TimelineTransaction, TimelineTransactionCommit,
};
pub use engine_authoring::{TimelineClipId, TimelineId, TimelineMarkerId, TimelineTrackId};
pub use engine_timeline::{
    evaluate_with_adapter, CompiledEntry, CompiledSpan, CompiledTimeline, CompiledTimelineError,
    EvaluationDecision, EvaluationItem, EvaluationMode, EvaluationRequest, PlaybackRateError,
    SeekCapability, TimelineEvaluationAdapter, TimelinePlaybackState, TimelinePlayer, TimelineTick,
    TIMELINE_TICKS_PER_SECOND,
};

/// Owning-domain operations required by the concrete engine Timeline adapter.
///
/// This trait lives in the final composition crate rather than `engine-timeline`:
/// each method receives the original strongly typed payload plus the neutral
/// scheduler's local time and discontinuous-time decision. Implementations may
/// resolve stable authoring IDs to transient runtime handles, but must not
/// persist those handles back into Timeline data.
pub trait TimelineRuntimeHost {
    /// Runtime-domain error returned to the Timeline player host.
    type Error;

    /// Applies or samples one Animation Set motion-slot clip.
    fn animation(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledAnimationPayload,
        local_tick: TimelineTick,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;

    /// Applies one typed Transform property sample.
    fn transform_property(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledTransformPayload,
        local_tick: TimelineTick,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;

    /// Installs or updates one transient game-camera override.
    fn camera_cut(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledCameraCutPayload,
        local_tick: TimelineTick,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;

    /// Applies one ADR 0122 tracked-audio clip evaluation.
    fn audio(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledAudioPayload,
        local_tick: TimelineTick,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;

    /// Applies or reconstructs one ADR 0125 VFX clip evaluation.
    fn vfx(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledVfxPayload,
        local_tick: TimelineTick,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;

    /// Emits one exact sequence-level Event marker selected by the scheduler.
    fn event(
        &mut self,
        request: &EvaluationRequest,
        payload: &CompiledEventPayload,
        decision: EvaluationDecision,
    ) -> Result<(), Self::Error>;
}

/// Concrete typed adapter from the neutral scheduler into engine-owned domains.
pub struct EngineTimelineAdapter<'a, H> {
    host: &'a mut H,
}

impl<'a, H> EngineTimelineAdapter<'a, H> {
    /// Borrows the composition host for one evaluation pass.
    pub fn new(host: &'a mut H) -> Self {
        Self { host }
    }
}

impl<H> TimelineEvaluationAdapter<CompiledTimelinePayload> for EngineTimelineAdapter<'_, H>
where
    H: TimelineRuntimeHost,
{
    type Error = H::Error;

    fn apply(
        &mut self,
        request: &EvaluationRequest,
        item: EvaluationItem<'_, CompiledTimelinePayload>,
    ) -> Result<(), Self::Error> {
        let local_tick = item.local_tick();
        let decision = item.decision();
        match item.entry().payload() {
            CompiledTimelinePayload::Animation(payload) => {
                self.host.animation(request, payload, local_tick, decision)
            }
            CompiledTimelinePayload::TransformProperty(payload) => {
                self.host
                    .transform_property(request, payload, local_tick, decision)
            }
            CompiledTimelinePayload::CameraCut(payload) => {
                self.host.camera_cut(request, payload, local_tick, decision)
            }
            CompiledTimelinePayload::Audio(payload) => {
                self.host.audio(request, payload, local_tick, decision)
            }
            CompiledTimelinePayload::Vfx(payload) => {
                self.host.vfx(request, payload, local_tick, decision)
            }
            CompiledTimelinePayload::Event(payload) => self.host.event(request, payload, decision),
        }
    }
}

/// Evaluates a compiled engine Timeline and dispatches selected payloads to
/// their owning domains through [`TimelineRuntimeHost`].
pub fn evaluate_engine_timeline<H>(
    timeline: &CompiledTimeline<CompiledTimelinePayload>,
    request: &EvaluationRequest,
    host: &mut H,
) -> Result<(), H::Error>
where
    H: TimelineRuntimeHost,
{
    let mut adapter = EngineTimelineAdapter::new(host);
    evaluate_with_adapter(timeline, request, &mut adapter)
}

/// Transient tracked-voice state for Audio Track clips.
///
/// Stable Timeline clip IDs are process-local lookup keys only. The
/// [`AudioVoiceId`] values remain runtime-only and are never written back to
/// authoring data. One instance belongs to one Timeline player/runtime host.
#[derive(Default)]
pub struct TimelineAudioVoices {
    active: HashMap<TimelineClipId, AudioVoiceId>,
    suppressed_until_exit: HashSet<TimelineClipId>,
    selected_this_pass: HashSet<TimelineClipId>,
}

impl TimelineAudioVoices {
    /// Creates empty tracked-voice state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begins one complete Timeline evaluation pass.
    ///
    /// Call this before [`evaluate_engine_timeline`], then call
    /// [`Self::finish_pass`] after every selected item has been dispatched.
    pub fn begin_pass(&mut self) {
        self.selected_this_pass.clear();
    }

    /// Applies one selected Audio Track clip through ADR 0122's tracked voice API.
    ///
    /// Normal playback starts the voice once and then updates its gains while
    /// the clip remains active. A naturally completed voice or discontinuous
    /// `NonSeekable` result suppresses that clip until it leaves the selected
    /// interval, preventing a one-shot from being restarted every frame.
    ///
    /// # Errors
    ///
    /// Returns the underlying platform-audio failure from start, update, or stop.
    pub fn apply_selected(
        &mut self,
        payload: &CompiledAudioPayload,
        decision: EvaluationDecision,
        asset: &AudioAsset,
        gains: StereoGains,
        audio: &mut AudioSystem,
    ) -> Result<(), AudioError> {
        self.selected_this_pass.insert(payload.clip.clone());
        match decision {
            EvaluationDecision::Apply => {
                if self.suppressed_until_exit.contains(&payload.clip) {
                    return Ok(());
                }
                if let Some(&voice) = self.active.get(&payload.clip) {
                    audio.update_voice(voice, gains)
                } else {
                    let voice = audio.start_voice(asset, gains, payload.looping)?;
                    self.active.insert(payload.clip.clone(), voice);
                    Ok(())
                }
            }
            EvaluationDecision::NonSeekable | EvaluationDecision::ReplayRequired { .. } => {
                self.stop_clip(&payload.clip, audio)?;
                self.suppressed_until_exit.insert(payload.clip.clone());
                Ok(())
            }
        }
    }

    /// Stops voices whose clips were not selected by the just-completed pass.
    ///
    /// This is the interval-exit path: neutral Timeline evaluation reports only
    /// entries active at the current tick, so stale tracked voices are retired
    /// here rather than by inventing interval-end events in `engine-timeline`.
    /// Suppression is also cleared only after interval exit, allowing a later
    /// loop/re-entry to start the clip again exactly once.
    ///
    /// # Errors
    ///
    /// Returns the first platform-audio stop failure. Successfully stopped
    /// voices are removed before an error is returned.
    pub fn finish_pass(&mut self, audio: &mut AudioSystem) -> Result<(), AudioError> {
        let stale = self
            .active
            .keys()
            .filter(|clip| !self.selected_this_pass.contains(*clip))
            .cloned()
            .collect::<Vec<_>>();
        for clip in stale {
            self.stop_clip(&clip, audio)?;
        }
        self.suppressed_until_exit
            .retain(|clip| self.selected_this_pass.contains(clip));
        Ok(())
    }

    /// Retires naturally completed voices and suppresses their clips until exit.
    pub fn drain_completed(&mut self, audio: &mut AudioSystem) {
        let completed = audio
            .drain_completed_voices()
            .into_iter()
            .collect::<HashSet<_>>();
        if completed.is_empty() {
            return;
        }
        let completed_clips = self
            .active
            .iter()
            .filter(|(_, voice)| completed.contains(voice))
            .map(|(clip, _)| clip.clone())
            .collect::<Vec<_>>();
        for clip in completed_clips {
            self.active.remove(&clip);
            self.suppressed_until_exit.insert(clip);
        }
    }

    /// Stops every Timeline-owned tracked voice, for example when the player is
    /// stopped, rebound to another compiled schedule, or destroyed.
    ///
    /// # Errors
    ///
    /// Returns the first platform-audio stop failure after removing every voice
    /// stopped before that failure.
    pub fn stop_all(&mut self, audio: &mut AudioSystem) -> Result<(), AudioError> {
        let clips = self.active.keys().cloned().collect::<Vec<_>>();
        for clip in clips {
            self.stop_clip(&clip, audio)?;
        }
        self.suppressed_until_exit.clear();
        self.selected_this_pass.clear();
        Ok(())
    }

    /// Returns the number of currently tracked Timeline-owned voices.
    pub fn active_voice_count(&self) -> usize {
        self.active.len()
    }

    fn stop_clip(
        &mut self,
        clip: &TimelineClipId,
        audio: &mut AudioSystem,
    ) -> Result<(), AudioError> {
        let Some(voice) = self.active.get(clip).copied() else {
            return Ok(());
        };
        audio.stop_voice(voice)?;
        self.active.remove(clip);
        Ok(())
    }
}
