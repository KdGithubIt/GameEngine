//! Production Timeline composition and transient multi-player runtime state (ADR 0126).
//!
//! Persisted Timeline data remains in `engine-authoring`; this module is the top-level
//! cross-domain fan-in that resolves stable IDs to live ECS/runtime state. Runtime entity,
//! animation, audio, and renderer handles stay process-local and are never serialized.

use crate::anim_graph::AnimGraphPlayer;
use crate::animation::Animator;
use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::audio::{
    AudioAsset, AudioEmitter, AudioSystem, AudioVoiceId, AudioVoiceSpatialSettings,
    SpatialAudioRuntime, StereoGains,
};
use crate::camera::{Camera3D, GameCameraSelectionOverride};
use crate::script_api::RuntimeEntityIdentity;
use crate::time::FixedTime;
use crate::transform::{GlobalTransform, Transform};
use crate::vfx::{VfxPlayer, VfxSourceIdentity};
use engine_authoring::{
    compile_timeline, sample_timeline_property, AssetId, CompiledTimelinePayload, EntityId,
    TimelineBinding, TimelineClipId, TimelineClipPayload, TimelineDocument, TimelineId,
    TimelinePropertyValue,
};
use engine_ecs::{Entity, Query, Res, ResMut};
pub use engine_timeline::{
    TimelineLoop, TimelinePlaybackState, TimelineTick, TIMELINE_TICKS_PER_SECOND,
};
use engine_timeline::{
    CompiledTimeline, EvaluationDecision, EvaluationMode, EvaluationRequest, PlaybackRate,
    PlaybackRateError, TimelineLoopError, TimelinePlayer,
};
use glam::{Quat, Vec3};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

/// Maximum retained runtime Timeline event records.
pub const MAX_TIMELINE_EVENTS: usize = 1024;
/// Maximum retained runtime diagnostics.
pub const MAX_TIMELINE_RUNTIME_DIAGNOSTICS: usize = 256;

/// Immutable prepared Timeline source ready to become one or more runtime players.
#[derive(Clone)]
pub struct PreparedTimeline {
    asset: AssetId,
    document_id: TimelineId,
    schedule: CompiledTimeline<CompiledTimelinePayload>,
}

impl PreparedTimeline {
    /// Stable Timeline asset ID.
    pub fn asset(&self) -> &AssetId {
        &self.asset
    }

    /// Stable semantic document ID.
    pub fn document_id(&self) -> &TimelineId {
        &self.document_id
    }

    /// Canonical duration of the compiled schedule.
    pub fn duration(&self) -> TimelineTick {
        self.schedule.duration()
    }
}

/// Failure while resolving a persisted Timeline into the production runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineRuntimeError {
    /// Stable asset is not registered in the active manifest.
    UnknownAsset(AssetId),
    /// Asset bytes could not be loaded below the configured asset root.
    AssetLoad(String),
    /// Persisted JSON did not deserialize as a Timeline document.
    Parse(String),
    /// Authoring validation/compilation rejected the document.
    Compile(Vec<String>),
    /// Runtime player ID is not active.
    UnknownPlayer(u64),
    /// Requested deterministic playback rate is invalid.
    InvalidRate,
    /// Requested loop range is empty or outside the compiled Timeline duration.
    InvalidLoop,
}

impl fmt::Display for TimelineRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAsset(asset) => write!(formatter, "Timeline asset `{}` is not registered", asset.as_str()),
            Self::AssetLoad(message) => write!(formatter, "Timeline asset could not be loaded: {message}"),
            Self::Parse(message) => write!(formatter, "Timeline JSON could not be parsed: {message}"),
            Self::Compile(diagnostics) => write!(formatter, "Timeline did not compile: {}", diagnostics.join("; ")),
            Self::UnknownPlayer(player) => write!(formatter, "Timeline player {player} does not exist"),
            Self::InvalidRate => formatter.write_str("Timeline playback rate is invalid"),
            Self::InvalidLoop => formatter.write_str("Timeline loop range is invalid"),
        }
    }
}

impl std::error::Error for TimelineRuntimeError {}

impl From<PlaybackRateError> for TimelineRuntimeError {
    fn from(_: PlaybackRateError) -> Self {
        Self::InvalidRate
    }
}

impl From<TimelineLoopError> for TimelineRuntimeError {
    fn from(_: TimelineLoopError) -> Self {
        Self::InvalidLoop
    }
}

/// Compiles an in-memory Timeline working copy through the same production schedule path.
///
/// Editor Sequencer preview uses this for unsaved source changes. Only the source read is
/// bypassed; validation, compilation, runtime composition, seek semantics, and domain
/// adapters remain identical to packaged Player playback.
pub fn prepare_timeline_document(
    asset: AssetId,
    document: &TimelineDocument,
) -> Result<PreparedTimeline, TimelineRuntimeError> {
    let compilation = compile_timeline(document);
    let schedule = compilation.schedule.ok_or_else(|| {
        TimelineRuntimeError::Compile(
            compilation
                .diagnostics
                .into_iter()
                .map(|diagnostic| format!("[{}] {}", diagnostic.code, diagnostic.message))
                .collect(),
        )
    })?;
    Ok(PreparedTimeline {
        asset,
        document_id: document.id.clone(),
        schedule,
    })
}

/// Loads and compiles a stable Timeline asset through the production asset boundary.
///
/// The returned schedule is the same immutable `engine-timeline` representation used by
/// Editor preview and packaged Player playback.
pub fn prepare_timeline_asset(
    asset: &AssetId,
    manifest: &AssetManifest,
    server: &AssetServer,
) -> Result<PreparedTimeline, TimelineRuntimeError> {
    let entry = manifest
        .get(asset)
        .ok_or_else(|| TimelineRuntimeError::UnknownAsset(asset.clone()))?;
    let bytes = server
        .load_bytes(&entry.path)
        .map_err(|error| TimelineRuntimeError::AssetLoad(error.to_string()))?;
    let document: TimelineDocument = serde_json::from_slice(&bytes)
        .map_err(|error| TimelineRuntimeError::Parse(error.to_string()))?;
    let compilation = compile_timeline(&document);
    let schedule = compilation.schedule.ok_or_else(|| {
        TimelineRuntimeError::Compile(
            compilation
                .diagnostics
                .into_iter()
                .map(|diagnostic| format!("[{}] {}", diagnostic.code, diagnostic.message))
                .collect(),
        )
    })?;
    Ok(PreparedTimeline {
        asset: asset.clone(),
        document_id: document.id,
        schedule,
    })
}

#[derive(Clone)]
struct ActiveTimelinePlayer {
    asset: AssetId,
    document_id: TimelineId,
    schedule: CompiledTimeline<CompiledTimelinePayload>,
    player: TimelinePlayer,
    priority: i32,
    pending: VecDeque<EvaluationRequest>,
}

/// Copied Project-Rust/Editor-safe state for one runtime Timeline player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelinePlayerSnapshot {
    /// Caller-owned logical player ID.
    pub player_id: u64,
    /// Stable Timeline asset being played.
    pub asset: AssetId,
    /// Stable Timeline document identity.
    pub document_id: TimelineId,
    /// Current playback state.
    pub state: TimelinePlaybackState,
    /// Exact current canonical tick.
    pub tick: TimelineTick,
    /// Monotonic discontinuity generation.
    pub generation: u64,
    /// Explicit cross-player composition priority. Higher values win.
    pub priority: i32,
    /// Rational rate numerator.
    pub rate_numerator: i32,
    /// Rational rate denominator.
    pub rate_denominator: u32,
}

/// One bounded sequence-level event emitted by Timeline evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEventRecord {
    /// Monotonic runtime source sequence.
    pub source_sequence: u64,
    /// Logical player that emitted the event.
    pub player_id: u64,
    /// Stable Timeline asset.
    pub asset: AssetId,
    /// Stable semantic Timeline document identity.
    pub document_id: TimelineId,
    /// Canonical event tick.
    pub tick: TimelineTick,
    /// Stable bounded event name.
    pub name: String,
    /// Bounded serialized payload. Marker events use an empty payload.
    pub payload: String,
}

/// Bounded event history shared by Editor inspection and Project Rust subscribers.
#[derive(Debug, Default)]
pub struct TimelineEvents {
    events: VecDeque<TimelineEventRecord>,
    next_sequence: u64,
}

impl TimelineEvents {
    /// Iterates retained events in source order.
    pub fn iter(&self) -> impl Iterator<Item = &TimelineEventRecord> {
        self.events.iter()
    }

    fn push(&mut self, mut event: TimelineEventRecord) {
        if self.events.len() >= MAX_TIMELINE_EVENTS {
            self.events.pop_front();
        }
        self.next_sequence = self.next_sequence.saturating_add(1).max(1);
        event.source_sequence = self.next_sequence;
        self.events.push_back(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Priority {
    player_priority: i32,
    track_order: u32,
    item_order: u32,
    player_id: u64,
}

#[derive(Clone)]
struct ActiveClip {
    priority: Priority,
    player_id: u64,
    playback_state: TimelinePlaybackState,
    discontinuous_seek: bool,
    clip_id: TimelineClipId,
    binding: Option<TimelineBinding>,
    source_offset: TimelineTick,
    local_tick: TimelineTick,
    payload: TimelineClipPayload,
}

struct PendingEvent {
    player_id: u64,
    asset: AssetId,
    document_id: TimelineId,
    tick: TimelineTick,
    name: String,
    payload: String,
}

#[derive(Default)]
struct CollectedFrame {
    active: Vec<ActiveClip>,
    events: Vec<PendingEvent>,
}

#[derive(Clone)]
struct DesiredTransform {
    priority: Priority,
    value: TimelinePropertyValue,
}

/// Multi-player transient Timeline runtime.
///
/// Every override snapshot and backend voice ID is runtime-only. Stable persisted data is
/// never rewritten when a player starts, seeks, or stops.
#[derive(Default)]
pub struct TimelineRuntime {
    players: BTreeMap<u64, ActiveTimelinePlayer>,
    clock_fraction: f64,
    animation_originals: HashMap<EntityId, Animator>,
    vfx_originals: HashMap<EntityId, VfxPlayer>,
    transform_originals: HashMap<(EntityId, String), TimelinePropertyValue>,
    desired_transforms: BTreeMap<(EntityId, String), DesiredTransform>,
    audio_voices: HashMap<(u64, TimelineClipId), AudioVoiceId>,
    audio_suppressed: HashSet<(u64, TimelineClipId)>,
    diagnostics: VecDeque<String>,
}

impl TimelineRuntime {
    /// Starts or replaces a logical player at the default composition priority.
    pub fn start(&mut self, player_id: u64, prepared: PreparedTimeline) {
        self.start_with_priority(player_id, prepared, 0);
    }

    /// Starts or replaces a logical player with an explicit cross-player priority.
    ///
    /// Higher priorities win when independent Timeline players target the same runtime
    /// domain. Equal priorities remain deterministic through track/item order and logical
    /// player ID, and the runtime records a conflict diagnostic instead of silently
    /// pretending the authoring intent was unambiguous.
    pub fn start_with_priority(
        &mut self,
        player_id: u64,
        prepared: PreparedTimeline,
        priority: i32,
    ) {
        let mut player = TimelinePlayer::new(prepared.schedule.duration());
        player.play();
        self.players.insert(
            player_id,
            ActiveTimelinePlayer {
                asset: prepared.asset,
                document_id: prepared.document_id,
                schedule: prepared.schedule,
                player,
                priority,
                pending: VecDeque::new(),
            },
        );
    }

    /// Pauses one player without changing its exact playhead.
    pub fn pause(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> {
        self.player_mut(player_id)?.player.pause();
        Ok(())
    }

    /// Resumes one player.
    pub fn resume(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> {
        self.player_mut(player_id)?.player.play();
        Ok(())
    }

    /// Stops one player and releases its transient overrides on the next fixed pass.
    pub fn stop(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> {
        let active = self.player_mut(player_id)?;
        active.player.stop();
        active.pending.clear();
        Ok(())
    }

    /// Performs an exact discontinuous seek. Side-effect events remain suppressed unless
    /// `preview_events` is explicitly enabled.
    pub fn seek(
        &mut self,
        player_id: u64,
        tick: TimelineTick,
        preview_events: bool,
    ) -> Result<(), TimelineRuntimeError> {
        let active = self.player_mut(player_id)?;
        let request = active.player.seek(tick, preview_events);
        active.pending.push_back(request);
        Ok(())
    }

    /// Sets a bounded deterministic rational playback rate.
    pub fn set_rate(
        &mut self,
        player_id: u64,
        numerator: i32,
        denominator: u32,
    ) -> Result<(), TimelineRuntimeError> {
        let rate = PlaybackRate::new(numerator, denominator)?;
        self.player_mut(player_id)?.player.set_rate(rate);
        Ok(())
    }

    /// Configures the neutral player's exact half-open loop range.
    ///
    /// Loop crossings remain owned by `engine-timeline`, which splits the end/start
    /// transition so point events keep the same exact crossing semantics in Editor
    /// preview and packaged Player playback.
    pub fn set_loop(
        &mut self,
        player_id: u64,
        range: Option<TimelineLoop>,
    ) -> Result<(), TimelineRuntimeError> {
        self.player_mut(player_id)?.player.set_loop(range)?;
        Ok(())
    }

    /// Returns copied runtime player state in deterministic logical-ID order.
    pub fn snapshots(&self) -> Vec<TimelinePlayerSnapshot> {
        self.players
            .iter()
            .map(|(&player_id, active)| {
                let rate = active.player.rate();
                TimelinePlayerSnapshot {
                    player_id,
                    asset: active.asset.clone(),
                    document_id: active.document_id.clone(),
                    state: active.player.state(),
                    tick: active.player.tick(),
                    generation: active.player.generation(),
                    priority: active.priority,
                    rate_numerator: rate.numerator(),
                    rate_denominator: rate.denominator(),
                }
            })
            .collect()
    }

    /// Iterates recent runtime binding/asset diagnostics.
    pub fn diagnostics(&self) -> impl Iterator<Item = &str> {
        self.diagnostics.iter().map(String::as_str)
    }

    fn player_mut(&mut self, player_id: u64) -> Result<&mut ActiveTimelinePlayer, TimelineRuntimeError> {
        self.players
            .get_mut(&player_id)
            .ok_or(TimelineRuntimeError::UnknownPlayer(player_id))
    }

    fn warn(&mut self, message: impl Into<String>) {
        if self.diagnostics.len() >= MAX_TIMELINE_RUNTIME_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        let message = message.into();
        log::warn!("timeline: {message}");
        self.diagnostics.push_back(message);
    }

    fn host_ticks(&mut self, fixed_delta: f32) -> i64 {
        if !fixed_delta.is_finite() || fixed_delta <= 0.0 {
            return 0;
        }
        let exact = f64::from(fixed_delta) * TIMELINE_TICKS_PER_SECOND as f64
            + self.clock_fraction;
        let whole = exact.floor();
        self.clock_fraction = exact - whole;
        whole.clamp(0.0, i64::MAX as f64) as i64
    }

    fn collect_frame(&mut self, fixed_delta: f32) -> CollectedFrame {
        let host_ticks = self.host_ticks(fixed_delta);
        let mut frame = CollectedFrame::default();
        let mut suppress_audio = Vec::new();

        for (&player_id, active) in &mut self.players {
            let mut requests = active.pending.drain(..).collect::<Vec<_>>();
            if active.player.state() == TimelinePlaybackState::Playing {
                requests.extend(active.player.advance_ticks(host_ticks));
            }
            let discontinuous_seek = requests
                .iter()
                .any(|request| matches!(request.mode, EvaluationMode::Seek { .. }));

            for request in &requests {
                for item in active.schedule.evaluate(request) {
                    match item.entry().payload() {
                        CompiledTimelinePayload::Marker { event: Some(name), .. } => {
                            frame.events.push(PendingEvent {
                                player_id,
                                asset: active.asset.clone(),
                                document_id: active.document_id.clone(),
                                tick: request.current_tick,
                                name: name.clone(),
                                payload: String::new(),
                            });
                        }
                        CompiledTimelinePayload::Clip {
                            payload: TimelineClipPayload::Event { name, payload },
                            ..
                        } => {
                            frame.events.push(PendingEvent {
                                player_id,
                                asset: active.asset.clone(),
                                document_id: active.document_id.clone(),
                                tick: request.current_tick,
                                name: name.clone(),
                                payload: payload.clone(),
                            });
                        }
                        CompiledTimelinePayload::Clip {
                            clip_id,
                            payload: TimelineClipPayload::Audio { .. },
                            ..
                        } if item.decision() == EvaluationDecision::NonSeekable => {
                            suppress_audio.push((player_id, clip_id.clone()));
                        }
                        _ => {}
                    }
                }
            }

            if active.player.state() == TimelinePlaybackState::Stopped {
                continue;
            }
            let tick = active.player.tick();
            let hold = EvaluationRequest {
                previous_tick: tick,
                current_tick: tick,
                mode: EvaluationMode::Playback,
                generation: active.player.generation(),
            };
            for item in active.schedule.evaluate(&hold) {
                let CompiledTimelinePayload::Clip {
                    clip_id,
                    binding,
                    source_offset,
                    payload,
                    ..
                } = item.entry().payload()
                else {
                    continue;
                };
                frame.active.push(ActiveClip {
                    priority: Priority {
                        player_priority: active.priority,
                        track_order: item.entry().track_order(),
                        item_order: item.entry().item_order(),
                        player_id,
                    },
                    player_id,
                    playback_state: active.player.state(),
                    discontinuous_seek,
                    clip_id: clip_id.clone(),
                    binding: binding.clone(),
                    source_offset: *source_offset,
                    local_tick: item.local_tick(),
                    payload: payload.clone(),
                });
            }
        }

        self.audio_suppressed.extend(suppress_audio);
        frame
    }
}

/// Advances every Timeline player, applies domain-owned runtime state, and emits exact events.
///
/// Register this after Animation Graph state selection and before skeletal animation sampling.
pub(crate) fn timeline_prepare_system(
    fixed: Res<FixedTime>,
    mut runtime: ResMut<TimelineRuntime>,
    mut events: ResMut<TimelineEvents>,
    mut camera_override: ResMut<GameCameraSelectionOverride>,
    mut animators: Query<(&RuntimeEntityIdentity, &mut AnimGraphPlayer, &mut Animator)>,
    mut cameras: Query<(&RuntimeEntityIdentity, &Camera3D)>,
    mut vfx_players: Query<(
        &RuntimeEntityIdentity,
        &VfxSourceIdentity,
        &GlobalTransform,
        &mut VfxPlayer,
    )>,
    mut emitters: Query<(&RuntimeEntityIdentity, &AudioEmitter)>,
    spatial_audio: Res<SpatialAudioRuntime>,
    manifest: Option<Res<AssetManifest>>,
    mut server: Option<ResMut<AssetServer>>,
    mut audio_assets: Option<ResMut<Assets<AudioAsset>>>,
    mut audio: Option<ResMut<AudioSystem>>,
) {
    let frame = runtime.collect_frame(fixed.fixed_delta);
    for event in frame.events {
        events.push(TimelineEventRecord {
            source_sequence: 0,
            player_id: event.player_id,
            asset: event.asset,
            document_id: event.document_id,
            tick: event.tick,
            name: event.name,
            payload: event.payload,
        });
    }

    let mut desired_animation = BTreeMap::<EntityId, &ActiveClip>::new();
    let mut desired_vfx = BTreeMap::<EntityId, &ActiveClip>::new();
    let mut desired_camera: Option<(EntityId, &ActiveClip)> = None;
    runtime.desired_transforms.clear();
    let mut conflicts = BTreeSet::new();

    for active in &frame.active {
        let entity = match &active.binding {
            Some(TimelineBinding::Entity { entity }) => Some(entity.clone()),
            Some(TimelineBinding::Asset { .. }) | None => None,
        };
        match &active.payload {
            TimelineClipPayload::Animation { .. } => {
                if let Some(entity) = entity {
                    if let Some(current) = desired_animation.get(&entity)
                        && current.priority.player_priority == active.priority.player_priority
                        && current.player_id != active.player_id
                    {
                        conflicts.insert(format!(
                            "Animation target `{}` has equal Timeline player priority {} between players {} and {}",
                            entity.as_str(),
                            active.priority.player_priority,
                            current.player_id,
                            active.player_id
                        ));
                    }
                    choose_clip(&mut desired_animation, entity, active);
                }
            }
            TimelineClipPayload::TransformProperty { property, keys } => {
                let Some(entity) = entity else { continue };
                let sample_tick = active
                    .source_offset
                    .saturating_add(active.local_tick.get());
                if let Some(value) = sample_timeline_property(keys, sample_tick) {
                    let key = (entity, property.clone());
                    if let Some(current) = runtime.desired_transforms.get(&key)
                        && current.priority.player_priority == active.priority.player_priority
                        && current.priority.player_id != active.player_id
                    {
                        conflicts.insert(format!(
                            "property `{}` on entity `{}` has equal Timeline player priority {} between players {} and {}",
                            property,
                            key.0.as_str(),
                            active.priority.player_priority,
                            current.priority.player_id,
                            active.player_id
                        ));
                    }
                    let replace = runtime
                        .desired_transforms
                        .get(&key)
                        .is_none_or(|current| active.priority > current.priority);
                    if replace {
                        runtime.desired_transforms.insert(
                            key,
                            DesiredTransform {
                                priority: active.priority,
                                value,
                            },
                        );
                    }
                }
            }
            TimelineClipPayload::CameraCut => {
                if let Some(entity) = entity {
                    if let Some((current_entity, current)) = desired_camera.as_ref()
                        && current.priority.player_priority == active.priority.player_priority
                        && current.player_id != active.player_id
                    {
                        conflicts.insert(format!(
                            "Camera Cut has equal Timeline player priority {} between player {} target `{}` and player {} target `{}`",
                            active.priority.player_priority,
                            current.player_id,
                            current_entity.as_str(),
                            active.player_id,
                            entity.as_str()
                        ));
                    }
                    let replace = desired_camera
                        .as_ref()
                        .is_none_or(|(_, current)| active.priority > current.priority);
                    if replace {
                        desired_camera = Some((entity, active));
                    }
                }
            }
            TimelineClipPayload::Vfx { .. } => {
                if let Some(entity) = entity {
                    if let Some(current) = desired_vfx.get(&entity)
                        && current.priority.player_priority == active.priority.player_priority
                        && current.player_id != active.player_id
                    {
                        conflicts.insert(format!(
                            "VFX target `{}` has equal Timeline player priority {} between players {} and {}",
                            entity.as_str(),
                            active.priority.player_priority,
                            current.player_id,
                            active.player_id
                        ));
                    }
                    choose_clip(&mut desired_vfx, entity, active);
                }
            }
            TimelineClipPayload::Audio { .. } | TimelineClipPayload::Event { .. } => {}
        }
    }

    for conflict in conflicts {
        runtime.warn(conflict);
    }

    apply_animation_overrides(&mut runtime, &desired_animation, &mut animators);
    apply_vfx_overrides(&mut runtime, &desired_vfx, &mut vfx_players);
    apply_camera_override(&mut runtime, desired_camera, &mut cameras, &mut camera_override);
    apply_audio(
        &mut runtime,
        &frame.active,
        &mut emitters,
        &spatial_audio,
        manifest.as_deref(),
        server.as_deref_mut(),
        audio_assets.as_deref_mut(),
        audio.as_deref_mut(),
    );
}

/// Applies Transform/Property clips after skeletal animation so Timeline property tracks have
/// an explicit deterministic priority over animation channels targeting the same Transform.
pub(crate) fn timeline_transform_system(
    mut runtime: ResMut<TimelineRuntime>,
    mut transforms: Query<(&RuntimeEntityIdentity, &mut Transform)>,
) {
    let desired = runtime.desired_transforms.clone();
    let mut seen = BTreeSet::new();
    for (_, (identity, transform)) in transforms.iter_mut() {
        let entity_id = identity.authoring_id.clone();
        seen.insert(entity_id.clone());
        let keys = desired
            .keys()
            .filter(|(candidate, _)| candidate == &entity_id)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let Some(target) = desired.get(&key) else { continue };
            runtime
                .transform_originals
                .entry(key.clone())
                .or_insert_with(|| read_property(transform, &key.1));
            apply_property(transform, &key.1, &target.value);
        }

        let stale = runtime
            .transform_originals
            .keys()
            .filter(|(candidate, property)| {
                candidate == &entity_id
                    && !desired.contains_key(&(candidate.clone(), property.clone()))
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(original) = runtime.transform_originals.remove(&key) {
                apply_property(transform, &key.1, &original);
            }
        }
    }
    runtime
        .transform_originals
        .retain(|(entity, _), _| seen.contains(entity));
}

fn choose_clip<'a>(map: &mut BTreeMap<EntityId, &'a ActiveClip>, key: EntityId, active: &'a ActiveClip) {
    let replace = map
        .get(&key)
        .is_none_or(|current| active.priority > current.priority);
    if replace {
        map.insert(key, active);
    }
}

fn apply_animation_overrides(
    runtime: &mut TimelineRuntime,
    desired: &BTreeMap<EntityId, &ActiveClip>,
    animators: &mut Query<(&RuntimeEntityIdentity, &mut AnimGraphPlayer, &mut Animator)>,
) {
    let mut seen = BTreeSet::new();
    for (_, (identity, graph, animator)) in animators.iter_mut() {
        let entity_id = identity.authoring_id.clone();
        seen.insert(entity_id.clone());
        let Some(active) = desired.get(&entity_id).copied() else {
            if let Some(original) = runtime.animation_originals.remove(&entity_id) {
                *animator = original;
            }
            graph.set_timeline_overridden(false);
            continue;
        };
        let TimelineClipPayload::Animation {
            animation_set,
            motion_slot,
        } = &active.payload
        else {
            continue;
        };
        let Some(handle) = graph.timeline_clip_handle(animation_set, motion_slot) else {
            if let Some(original) = runtime.animation_originals.remove(&entity_id) {
                *animator = original;
            }
            graph.set_timeline_overridden(false);
            runtime.warn(format!(
                "Animation binding `{}` / `{}` is not resolved on entity `{}`",
                animation_set.as_str(),
                motion_slot.as_str(),
                entity_id.as_str()
            ));
            continue;
        };
        runtime
            .animation_originals
            .entry(entity_id.clone())
            .or_insert_with(|| animator.clone());
        graph.set_timeline_overridden(true);
        let sample_tick = active
            .source_offset
            .saturating_add(active.local_tick.get());
        animator.sample_timeline_pose(
            handle,
            sample_tick.to_seconds_f64() as f32,
            active.discontinuous_seek,
        );
    }
    runtime.animation_originals.retain(|entity, _| seen.contains(entity));
}

fn apply_vfx_overrides(
    runtime: &mut TimelineRuntime,
    desired: &BTreeMap<EntityId, &ActiveClip>,
    players: &mut Query<(
        &RuntimeEntityIdentity,
        &VfxSourceIdentity,
        &GlobalTransform,
        &mut VfxPlayer,
    )>,
) {
    let mut seen = BTreeSet::new();
    for (_, (identity, source, transform, player)) in players.iter_mut() {
        let entity_id = identity.authoring_id.clone();
        seen.insert(entity_id.clone());
        let Some(active) = desired.get(&entity_id).copied() else {
            if let Some(original) = runtime.vfx_originals.remove(&entity_id) {
                *player = original;
            } else {
                player.set_external_clock(false);
            }
            continue;
        };
        let TimelineClipPayload::Vfx { effect, .. } = &active.payload else { continue };
        if &source.effect != effect {
            if let Some(original) = runtime.vfx_originals.remove(&entity_id) {
                *player = original;
            } else {
                player.set_external_clock(false);
            }
            runtime.warn(format!(
                "VFX Timeline clip names effect `{}`, but entity `{}` owns `{}`",
                effect.as_str(),
                entity_id.as_str(),
                source.effect.as_str()
            ));
            continue;
        }
        runtime
            .vfx_originals
            .entry(entity_id.clone())
            .or_insert_with(|| player.clone());
        let sample_tick = active
            .source_offset
            .saturating_add(active.local_tick.get());
        let origin = transform.matrix().col(3).truncate();
        player.sample_external_time(sample_tick.to_seconds_f64() as f32, origin);
    }
    runtime.vfx_originals.retain(|entity, _| seen.contains(entity));
}

fn apply_camera_override(
    runtime: &mut TimelineRuntime,
    desired: Option<(EntityId, &ActiveClip)>,
    cameras: &mut Query<(&RuntimeEntityIdentity, &Camera3D)>,
    camera_override: &mut GameCameraSelectionOverride,
) {
    let Some((target, _)) = desired else {
        camera_override.clear();
        return;
    };
    for (entity, (identity, _camera)) in cameras.iter_mut() {
        if identity.authoring_id == target {
            camera_override.set_target(entity);
            return;
        }
    }
    camera_override.clear();
    runtime.warn(format!(
        "Camera Cut target `{}` is not a live Camera3D entity",
        target.as_str()
    ));
}

#[derive(Clone, Copy)]
struct EmitterSnapshot {
    entity: Entity,
    volume: f32,
    spatial_blend: f32,
    min_distance: f32,
    max_distance: f32,
    rolloff: crate::audio::AudioRolloffMode,
}

#[allow(clippy::too_many_arguments)]
fn apply_audio(
    runtime: &mut TimelineRuntime,
    active: &[ActiveClip],
    emitters: &mut Query<(&RuntimeEntityIdentity, &AudioEmitter)>,
    spatial: &SpatialAudioRuntime,
    manifest: Option<&AssetManifest>,
    mut server: Option<&mut AssetServer>,
    mut assets: Option<&mut Assets<AudioAsset>>,
    mut audio: Option<&mut AudioSystem>,
) {
    let mut emitter_map = BTreeMap::<EntityId, EmitterSnapshot>::new();
    for (entity, (identity, emitter)) in emitters.iter_mut() {
        emitter_map.insert(
            identity.authoring_id.clone(),
            EmitterSnapshot {
                entity,
                volume: emitter.volume,
                spatial_blend: emitter.spatial_blend,
                min_distance: emitter.min_distance,
                max_distance: emitter.max_distance,
                rolloff: emitter.rolloff,
            },
        );
    }

    let mut selected = HashSet::new();
    for item in active {
        let TimelineClipPayload::Audio {
            clip,
            volume,
            looping,
        } = &item.payload
        else {
            continue;
        };
        if item.playback_state != TimelinePlaybackState::Playing {
            continue;
        }
        let key = (item.player_id, item.clip_id.clone());
        selected.insert(key.clone());
        if runtime.audio_suppressed.contains(&key) {
            continue;
        }
        let Some(audio_system) = audio.as_deref_mut() else {
            continue;
        };
        let gains = match &item.binding {
            None => {
                let gain = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 0.0 };
                StereoGains { left: gain, right: gain }
            }
            Some(TimelineBinding::Entity { entity }) => {
                let Some(emitter) = emitter_map.get(entity).copied() else {
                    runtime.warn(format!(
                        "spatial Audio clip target `{}` has no production AudioEmitter",
                        entity.as_str()
                    ));
                    continue;
                };
                let settings = AudioVoiceSpatialSettings {
                    volume: (emitter.volume * *volume).clamp(0.0, 1.0),
                    spatial_blend: emitter.spatial_blend,
                    min_distance: emitter.min_distance,
                    max_distance: emitter.max_distance,
                    rolloff: emitter.rolloff,
                };
                let Some(gains) = spatial.timeline_gains(emitter.entity, settings) else {
                    runtime.warn(format!(
                        "spatial Audio clip target `{}` has no propagated transform",
                        entity.as_str()
                    ));
                    continue;
                };
                gains
            }
            Some(TimelineBinding::Asset { asset }) => {
                runtime.warn(format!(
                    "Audio clip has unsupported asset binding `{}`",
                    asset.as_str()
                ));
                continue;
            }
        };

        if let Some(&voice) = runtime.audio_voices.get(&key) {
            if let Err(error) = audio_system.update_voice(voice, gains) {
                runtime.audio_voices.remove(&key);
                runtime.audio_suppressed.insert(key);
                runtime.warn(format!("Timeline audio voice ended or failed: {error}"));
            }
            continue;
        }

        let (Some(manifest), Some(server), Some(assets)) =
            (manifest, server.as_deref_mut(), assets.as_deref_mut())
        else {
            runtime.warn("Timeline audio host resources are unavailable");
            continue;
        };
        let Some(entry) = manifest.get(clip) else {
            runtime.warn(format!("Audio asset `{}` is not registered", clip.as_str()));
            continue;
        };
        let handle = match server.load_audio(clip.clone(), &entry.path, assets) {
            Ok(handle) => handle,
            Err(error) => {
                runtime.warn(format!("Audio asset `{}` failed to load: {error}", clip.as_str()));
                continue;
            }
        };
        let Some(asset) = assets.get(&handle) else {
            runtime.warn(format!("Audio asset `{}` resolved to a missing runtime handle", clip.as_str()));
            continue;
        };
        match audio_system.start_voice(asset, gains, *looping) {
            Ok(voice) => {
                runtime.audio_voices.insert(key, voice);
            }
            Err(error) => runtime.warn(format!("Timeline audio start failed: {error}")),
        }
    }

    let stale = runtime
        .audio_voices
        .keys()
        .filter(|key| !selected.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(audio_system) = audio.as_deref_mut() {
        for key in stale {
            if let Some(voice) = runtime.audio_voices.remove(&key)
                && let Err(error) = audio_system.stop_voice(voice)
            {
                runtime.warn(format!("Timeline audio stop failed: {error}"));
            }
        }
    } else {
        runtime.audio_voices.clear();
    }
    runtime
        .audio_suppressed
        .retain(|key| selected.contains(key));
}

fn read_property(transform: &Transform, property: &str) -> TimelinePropertyValue {
    match property {
        "engine.transform.rotation" => {
            let value = transform.rotation.to_array();
            TimelinePropertyValue::Quat(value.map(f64::from))
        }
        "engine.transform.scale" => {
            TimelinePropertyValue::Vec3(transform.scale.to_array().map(f64::from))
        }
        _ => TimelinePropertyValue::Vec3(transform.translation.to_array().map(f64::from)),
    }
}

fn apply_property(transform: &mut Transform, property: &str, value: &TimelinePropertyValue) {
    match (property, value) {
        ("engine.transform.translation", TimelinePropertyValue::Vec3(value)) => {
            transform.translation = Vec3::from_array(value.map(|value| value as f32));
        }
        ("engine.transform.rotation", TimelinePropertyValue::Quat(value)) => {
            transform.rotation = Quat::from_array(value.map(|value| value as f32)).normalize();
        }
        ("engine.transform.scale", TimelinePropertyValue::Vec3(value)) => {
            transform.scale = Vec3::from_array(value.map(|value| value as f32));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn two_players_keep_independent_ticks_and_stable_ids() {
        let document = TimelineDocument::new("test", TimelineTick::new(10_000));
        let schedule = compile_timeline(&document).schedule.unwrap();
        let prepared = PreparedTimeline {
            asset: AssetId::generate(),
            document_id: document.id.clone(),
            schedule,
        };
        let mut runtime = TimelineRuntime::default();
        runtime.start(1, prepared.clone());
        runtime.start(2, prepared);
        runtime.player_mut(1).unwrap().player.advance_ticks(800);
        let snapshots = runtime.snapshots();
        assert_eq!(snapshots[0].tick, TimelineTick::new(800));
        assert_eq!(snapshots[1].tick, TimelineTick::ZERO);
        assert_eq!(snapshots[0].document_id, snapshots[1].document_id);
    }

    #[test]
    fn timeline_event_records_keep_stable_asset_and_document_identity() {
        let asset = AssetId::generate();
        let document_id = TimelineId::generate();
        let mut events = TimelineEvents::default();
        events.push(TimelineEventRecord {
            source_sequence: 0,
            player_id: u64::MAX,
            asset: asset.clone(),
            document_id: document_id.clone(),
            tick: TimelineTick::new(48_000),
            name: "sequence.event".to_owned(),
            payload: "payload".to_owned(),
        });
        let event = events.iter().next().unwrap();
        assert_eq!(event.asset, asset);
        assert_eq!(event.document_id, document_id);
        assert_eq!(event.player_id, u64::MAX);
        assert_eq!(event.source_sequence, 1);
    }

    #[test]
    fn explicit_player_priority_wins_before_logical_id_tiebreaker() {
        let entity = EntityId::generate();
        let clip = |player_id, player_priority| ActiveClip {
            priority: Priority {
                player_priority,
                track_order: 0,
                item_order: 0,
                player_id,
            },
            player_id,
            playback_state: TimelinePlaybackState::Playing,
            discontinuous_seek: false,
            clip_id: TimelineClipId::generate(),
            binding: Some(TimelineBinding::Entity { entity: entity.clone() }),
            source_offset: TimelineTick::ZERO,
            local_tick: TimelineTick::ZERO,
            payload: TimelineClipPayload::CameraCut,
        };
        let high_id_low_priority = clip(99, 1);
        let low_id_high_priority = clip(2, 10);
        let mut selected = BTreeMap::new();
        choose_clip(&mut selected, entity.clone(), &high_id_low_priority);
        choose_clip(&mut selected, entity.clone(), &low_id_high_priority);
        assert_eq!(selected[&entity].player_id, 2);
    }

    #[test]
    fn higher_logical_player_id_is_the_deterministic_override_tiebreaker() {
        let entity = EntityId::generate();
        let clip = |player_id| ActiveClip {
            priority: Priority {
                player_priority: 0,
                track_order: 0,
                item_order: 0,
                player_id,
            },
            player_id,
            playback_state: TimelinePlaybackState::Playing,
            discontinuous_seek: false,
            clip_id: TimelineClipId::generate(),
            binding: Some(TimelineBinding::Entity { entity: entity.clone() }),
            source_offset: TimelineTick::ZERO,
            local_tick: TimelineTick::ZERO,
            payload: TimelineClipPayload::CameraCut,
        };
        let low = clip(4);
        let high = clip(9);
        let mut selected = BTreeMap::new();
        choose_clip(&mut selected, entity.clone(), &low);
        choose_clip(&mut selected, entity.clone(), &high);
        assert_eq!(selected[&entity].player_id, 9);
    }
}
