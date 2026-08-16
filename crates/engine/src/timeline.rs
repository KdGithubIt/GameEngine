//! Timeline authoring and dependency-neutral runtime orchestration (ADR 0126).
//!
//! The concrete Animation, Camera, Transform, and Event domains remain owned
//! by their existing modules. This facade exposes the stable authoring model
//! and neutral evaluation requests; composition-layer adapters may translate
//! those requests into domain operations without moving domain logic into the
//! Timeline scheduler.

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
