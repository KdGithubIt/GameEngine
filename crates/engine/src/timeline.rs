//! Production Timeline composition and transient multi-player runtime state (ADR 0126).

use crate::anim_graph::AnimGraphPlayer;
use crate::animation::Animator;
use crate::asset::{AssetManifest, AssetServer, Assets};
use crate::audio::{AudioAsset, AudioEmitter, AudioSystem, AudioVoiceId, AudioVoiceSpatialSettings, SpatialAudioRuntime, StereoGains};
use crate::camera::{Camera3D, GameCameraSelectionOverride};
use crate::script_api::RuntimeEntityIdentity;
use crate::time::FixedTime;
use crate::transform::{GlobalTransform, Transform};
use crate::vfx::{VfxPlayer, VfxSourceIdentity};
use engine_authoring::{compile_timeline, sample_timeline_property, AssetId, CompiledTimelinePayload, EntityId, TimelineBinding, TimelineClipId, TimelineClipPayload, TimelineDocument, TimelineId, TimelinePropertyValue};
use engine_ecs::{Entity, Query, Res, ResMut};
pub use engine_timeline::{TimelineLoop, TimelinePlaybackState, TimelineTick, TIMELINE_TICKS_PER_SECOND};
use engine_timeline::{CompiledTimeline, EvaluationDecision, EvaluationMode, EvaluationRequest, PlaybackRate, PlaybackRateError, TimelineLoopError, TimelinePlayer};
use glam::{Quat, Vec3};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

pub const MAX_TIMELINE_EVENTS: usize = 1024;
pub const MAX_TIMELINE_RUNTIME_DIAGNOSTICS: usize = 256;

#[derive(Clone)]
pub struct PreparedTimeline { asset: AssetId, document_id: TimelineId, schedule: CompiledTimeline<CompiledTimelinePayload> }
impl PreparedTimeline {
    pub fn asset(&self) -> &AssetId { &self.asset }
    pub fn document_id(&self) -> &TimelineId { &self.document_id }
    pub fn duration(&self) -> TimelineTick { self.schedule.duration() }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineRuntimeError { UnknownAsset(AssetId), AssetLoad(String), Parse(String), Compile(Vec<String>), UnknownPlayer(u64), InvalidRate, InvalidLoop }
impl fmt::Display for TimelineRuntimeError { fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { match self { Self::UnknownAsset(asset) => write!(formatter, "Timeline asset `{}` is not registered", asset.as_str()), Self::AssetLoad(message) => write!(formatter, "Timeline asset could not be loaded: {message}"), Self::Parse(message) => write!(formatter, "Timeline JSON could not be parsed: {message}"), Self::Compile(diagnostics) => write!(formatter, "Timeline did not compile: {}", diagnostics.join("; ")), Self::UnknownPlayer(player) => write!(formatter, "Timeline player {player} does not exist"), Self::InvalidRate => formatter.write_str("Timeline playback rate is invalid"), Self::InvalidLoop => formatter.write_str("Timeline loop range is invalid") } } }
impl std::error::Error for TimelineRuntimeError {}
impl From<PlaybackRateError> for TimelineRuntimeError { fn from(_: PlaybackRateError) -> Self { Self::InvalidRate } }
impl From<TimelineLoopError> for TimelineRuntimeError { fn from(_: TimelineLoopError) -> Self { Self::InvalidLoop } }

pub fn prepare_timeline_document(asset: AssetId, document: &TimelineDocument) -> Result<PreparedTimeline, TimelineRuntimeError> {
    let compilation = compile_timeline(document);
    let schedule = compilation.schedule.ok_or_else(|| TimelineRuntimeError::Compile(compilation.diagnostics.into_iter().map(|diagnostic| format!("[{}] {}", diagnostic.code, diagnostic.message)).collect()))?;
    Ok(PreparedTimeline { asset, document_id: document.id.clone(), schedule })
}
pub fn prepare_timeline_asset(asset: &AssetId, manifest: &AssetManifest, server: &AssetServer) -> Result<PreparedTimeline, TimelineRuntimeError> {
    let entry = manifest.get(asset).ok_or_else(|| TimelineRuntimeError::UnknownAsset(asset.clone()))?;
    let bytes = server.load_bytes(&entry.path).map_err(|error| TimelineRuntimeError::AssetLoad(error.to_string()))?;
    let document: TimelineDocument = serde_json::from_slice(&bytes).map_err(|error| TimelineRuntimeError::Parse(error.to_string()))?;
    let compilation = compile_timeline(&document);
    let schedule = compilation.schedule.ok_or_else(|| TimelineRuntimeError::Compile(compilation.diagnostics.into_iter().map(|diagnostic| format!("[{}] {}", diagnostic.code, diagnostic.message)).collect()))?;
    Ok(PreparedTimeline { asset: asset.clone(), document_id: document.id, schedule })
}

#[derive(Clone)]
struct ActiveTimelinePlayer { asset: AssetId, document_id: TimelineId, schedule: CompiledTimeline<CompiledTimelinePayload>, player: TimelinePlayer, priority: i32, pending: VecDeque<EvaluationRequest> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelinePlayerSnapshot { pub player_id: u64, pub asset: AssetId, pub document_id: TimelineId, pub state: TimelinePlaybackState, pub tick: TimelineTick, pub generation: u64, pub priority: i32, pub rate_numerator: i32, pub rate_denominator: u32 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEventRecord { pub source_sequence: u64, pub player_id: u64, pub asset: AssetId, pub document_id: TimelineId, pub tick: TimelineTick, pub name: String, pub payload: String }
#[derive(Debug, Default)]
pub struct TimelineEvents { events: VecDeque<TimelineEventRecord>, next_sequence: u64 }
impl TimelineEvents { pub fn iter(&self) -> impl Iterator<Item = &TimelineEventRecord> { self.events.iter() } pub(crate) fn push(&mut self, mut event: TimelineEventRecord) { if self.events.len() >= MAX_TIMELINE_EVENTS { self.events.pop_front(); } self.next_sequence = self.next_sequence.saturating_add(1).max(1); event.source_sequence = self.next_sequence; self.events.push_back(event); } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Priority { player_priority: i32, track_order: u32, item_order: u32, player_id: u64 }
#[derive(Clone)]
struct ActiveClip { priority: Priority, player_id: u64, playback_state: TimelinePlaybackState, discontinuous_seek: bool, clip_id: TimelineClipId, binding: Option<TimelineBinding>, source_offset: TimelineTick, local_tick: TimelineTick, payload: TimelineClipPayload }
struct PendingEvent { player_id: u64, asset: AssetId, document_id: TimelineId, tick: TimelineTick, name: String, payload: String }
#[derive(Default)]
struct CollectedFrame { active: Vec<ActiveClip>, events: Vec<PendingEvent> }
#[derive(Clone)]
struct DesiredTransform { priority: Priority, value: TimelinePropertyValue }
#[derive(Default)]
pub struct TimelineRuntime { players: BTreeMap<u64, ActiveTimelinePlayer>, clock_fraction: f64, animation_originals: HashMap<EntityId, Animator>, vfx_originals: HashMap<EntityId, VfxPlayer>, transform_originals: HashMap<(EntityId, String), TimelinePropertyValue>, desired_transforms: BTreeMap<(EntityId, String), DesiredTransform>, audio_voices: HashMap<(u64, TimelineClipId), AudioVoiceId>, audio_suppressed: HashSet<(u64, TimelineClipId)>, diagnostics: VecDeque<String> }
impl TimelineRuntime {
    pub fn start(&mut self, player_id: u64, prepared: PreparedTimeline) { self.start_with_priority(player_id, prepared, 0); }
    pub fn start_with_priority(&mut self, player_id: u64, prepared: PreparedTimeline, priority: i32) { let mut player = TimelinePlayer::new(prepared.schedule.duration()); player.play(); self.players.insert(player_id, ActiveTimelinePlayer { asset: prepared.asset, document_id: prepared.document_id, schedule: prepared.schedule, player, priority, pending: VecDeque::new() }); }
    pub fn pause(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> { self.player_mut(player_id)?.player.pause(); Ok(()) }
    pub fn resume(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> { self.player_mut(player_id)?.player.play(); Ok(()) }
    pub fn stop(&mut self, player_id: u64) -> Result<(), TimelineRuntimeError> { let active = self.player_mut(player_id)?; active.player.stop(); active.pending.clear(); Ok(()) }
    pub fn seek(&mut self, player_id: u64, tick: TimelineTick, preview_events: bool) -> Result<(), TimelineRuntimeError> { let active = self.player_mut(player_id)?; let request = active.player.seek(tick, preview_events); active.pending.push_back(request); Ok(()) }
    pub fn set_rate(&mut self, player_id: u64, numerator: i32, denominator: u32) -> Result<(), TimelineRuntimeError> { let rate = PlaybackRate::new(numerator, denominator)?; self.player_mut(player_id)?.player.set_rate(rate); Ok(()) }
    pub fn set_loop(&mut self, player_id: u64, range: Option<TimelineLoop>) -> Result<(), TimelineRuntimeError> { self.player_mut(player_id)?.player.set_loop(range)?; Ok(()) }
    pub fn snapshots(&self) -> Vec<TimelinePlayerSnapshot> { self.players.iter().map(|(&player_id, active)| { let rate = active.player.rate(); TimelinePlayerSnapshot { player_id, asset: active.asset.clone(), document_id: active.document_id.clone(), state: active.player.state(), tick: active.player.tick(), generation: active.player.generation(), priority: active.priority, rate_numerator: rate.numerator(), rate_denominator: rate.denominator() } }).collect() }
    pub fn diagnostics(&self) -> impl Iterator<Item = &str> { self.diagnostics.iter().map(String::as_str) }
    fn player_mut(&mut self, player_id: u64) -> Result<&mut ActiveTimelinePlayer, TimelineRuntimeError> { self.players.get_mut(&player_id).ok_or(TimelineRuntimeError::UnknownPlayer(player_id)) }
    fn warn(&mut self, message: impl Into<String>) { if self.diagnostics.len() >= MAX_TIMELINE_RUNTIME_DIAGNOSTICS { self.diagnostics.pop_front(); } let message = message.into(); log::warn!("timeline: {message}"); self.diagnostics.push_back(message); }
    fn host_ticks(&mut self, fixed_delta: f32) -> i64 { if !fixed_delta.is_finite() || fixed_delta <= 0.0 { return 0; } let exact = f64::from(fixed_delta) * TIMELINE_TICKS_PER_SECOND as f64 + self.clock_fraction; let whole = exact.floor(); self.clock_fraction = exact - whole; whole.clamp(0.0, i64::MAX as f64) as i64 }
    fn collect_frame(&mut self, fixed_delta: f32) -> CollectedFrame {
        let host_ticks = self.host_ticks(fixed_delta); let mut frame = CollectedFrame::default(); let mut suppress_audio = Vec::new();
        for (&player_id, active) in &mut self.players {
            let mut requests = active.pending.drain(..).collect::<Vec<_>>();
            if active.player.state() == TimelinePlaybackState::Playing { requests.extend(active.player.advance_ticks(host_ticks)); }
            let discontinuous_seek = requests.iter().any(|request| matches!(request.mode, EvaluationMode::Seek { .. }));
            for request in &requests {
                for item in active.schedule.evaluate(request) {
                    match item.entry().payload() {
                        CompiledTimelinePayload::Marker { event: name, .. } => frame.events.push(PendingEvent { player_id, asset: active.asset.clone(), document_id: active.document_id.clone(), tick: request.current_tick, name: name.clone(), payload: String::new() }),
                        CompiledTimelinePayload::Clip { payload: TimelineClipPayload::Event { name, payload }, .. } => frame.events.push(PendingEvent { player_id, asset: active.asset.clone(), document_id: active.document_id.clone(), tick: request.current_tick, name: name.clone(), payload: payload.clone() }),
                        CompiledTimelinePayload::Clip { clip_id, payload: TimelineClipPayload::Audio { .. }, .. } if item.decision() == EvaluationDecision::NonSeekable => suppress_audio.push((player_id, clip_id.clone())),
                        _ => {}
                    }
                }
            }
            if active.player.state() == TimelinePlaybackState::Stopped { continue; }
            let tick = active.player.tick();
            let hold = EvaluationRequest { previous_tick: tick, current_tick: tick, mode: EvaluationMode::Playback, generation: active.player.generation() };
            for item in active.schedule.evaluate(&hold) {
                let CompiledTimelinePayload::Clip { clip_id, binding, source_offset, payload, .. } = item.entry().payload() else { continue; };
                frame.active.push(ActiveClip { priority: Priority { player_priority: active.priority, track_order: item.entry().track_order(), item_order: item.entry().item_order(), player_id }, player_id, playback_state: active.player.state(), discontinuous_seek, clip_id: clip_id.clone(), binding: binding.clone(), source_offset: *source_offset, local_tick: item.local_tick(), payload: payload.clone() });
            }
        }
        self.audio_suppressed.extend(suppress_audio); frame
    }
}

/// Installs the resources required to evaluate Timeline inside an existing engine App.
///
/// Editor Scene View uses this on its persistent PreviewWorld. It intentionally installs no
/// parallel simulator and does not register systems itself; the caller places the production
/// Timeline systems in its existing pose pipeline so system ordering remains explicit.
pub fn ensure_timeline_preview_resources(app: &mut crate::App) {
    if app.world().get_resource::<TimelineRuntime>().is_none() {
        app.insert_resource(TimelineRuntime::default());
    }
    if app.world().get_resource::<TimelineEvents>().is_none() {
        app.insert_resource(TimelineEvents::default());
    }
    if app.world().get_resource::<SpatialAudioRuntime>().is_none() {
        app.insert_resource(SpatialAudioRuntime::default());
    }
    if app.world().get_resource::<GameCameraSelectionOverride>().is_none() {
        app.insert_resource(GameCameraSelectionOverride::default());
    }
}

/// Production Timeline prepare system, exposed for installation into the Editor PreviewWorld.
/// Runtime hosts normally receive this through `register_runtime_systems`.
pub fn timeline_prepare_system(
    fixed: Res<FixedTime>, mut runtime: ResMut<TimelineRuntime>, mut events: ResMut<TimelineEvents>, mut camera_override: ResMut<GameCameraSelectionOverride>,
    mut animators: Query<(&RuntimeEntityIdentity, &mut AnimGraphPlayer, &mut Animator)>, mut cameras: Query<(&RuntimeEntityIdentity, &Camera3D)>,
    mut vfx_players: Query<(&RuntimeEntityIdentity, &VfxSourceIdentity, &GlobalTransform, &mut VfxPlayer)>, mut emitters: Query<(&RuntimeEntityIdentity, &AudioEmitter)>,
    spatial_audio: Res<SpatialAudioRuntime>, manifest: Option<Res<AssetManifest>>, mut server: Option<ResMut<AssetServer>>, mut audio_assets: Option<ResMut<Assets<AudioAsset>>>, mut audio: Option<ResMut<AudioSystem>>,
) {
    let frame = runtime.collect_frame(fixed.fixed_delta);
    for event in frame.events { events.push(TimelineEventRecord { source_sequence: 0, player_id: event.player_id, asset: event.asset, document_id: event.document_id, tick: event.tick, name: event.name, payload: event.payload }); }
    let mut desired_animation = BTreeMap::<EntityId, &ActiveClip>::new(); let mut desired_vfx = BTreeMap::<EntityId, &ActiveClip>::new(); let mut desired_camera: Option<(EntityId, &ActiveClip)> = None; runtime.desired_transforms.clear(); let mut conflicts = BTreeSet::new();
    for active in &frame.active {
        let entity = match &active.binding { Some(TimelineBinding::Entity { entity }) => Some(entity.clone()), Some(TimelineBinding::Asset { .. }) | None => None };
        match &active.payload {
            TimelineClipPayload::Animation { .. } => { if let Some(entity) = entity { if let Some(current) = desired_animation.get(&entity) && current.priority.player_priority == active.priority.player_priority && current.player_id != active.player_id { conflicts.insert(format!("Animation target `{}` has equal Timeline player priority {} between players {} and {}", entity.as_str(), active.priority.player_priority, current.player_id, active.player_id)); } choose_clip(&mut desired_animation, entity, active); } }
            TimelineClipPayload::TransformProperty { property, keys } => { let Some(entity) = entity else { continue }; let sample_tick = active.source_offset.saturating_add(active.local_tick.get()); if let Some(value) = sample_timeline_property(keys, sample_tick) { let key = (entity, property.clone()); if let Some(current) = runtime.desired_transforms.get(&key) && current.priority.player_priority == active.priority.player_priority && current.priority.player_id != active.player_id { conflicts.insert(format!("property `{}` on entity `{}` has equal Timeline player priority {} between players {} and {}", property, key.0.as_str(), active.priority.player_priority, current.priority.player_id, active.player_id)); } let replace = runtime.desired_transforms.get(&key).is_none_or(|current| active.priority > current.priority); if replace { runtime.desired_transforms.insert(key, DesiredTransform { priority: active.priority, value }); } } }
            TimelineClipPayload::CameraCut => { if let Some(entity) = entity { if let Some((current_entity, current)) = desired_camera.as_ref() && current.priority.player_priority == active.priority.player_priority && current.player_id != active.player_id { conflicts.insert(format!("Camera Cut has equal Timeline player priority {} between player {} target `{}` and player {} target `{}`", active.priority.player_priority, current.player_id, current_entity.as_str(), active.player_id, entity.as_str())); } if desired_camera.as_ref().is_none_or(|(_, current)| active.priority > current.priority) { desired_camera = Some((entity, active)); } } }
            TimelineClipPayload::Vfx { .. } => { if let Some(entity) = entity { if let Some(current) = desired_vfx.get(&entity) && current.priority.player_priority == active.priority.player_priority && current.player_id != active.player_id { conflicts.insert(format!("VFX target `{}` has equal Timeline player priority {} between players {} and {}", entity.as_str(), active.priority.player_priority, current.player_id, active.player_id)); } choose_clip(&mut desired_vfx, entity, active); } }
            TimelineClipPayload::Audio { .. } | TimelineClipPayload::Event { .. } => {}
        }
    }
    for conflict in conflicts { runtime.warn(conflict); }
    apply_animation_overrides(&mut runtime, &desired_animation, &mut animators); apply_vfx_overrides(&mut runtime, &desired_vfx, &mut vfx_players); apply_camera_override(&mut runtime, desired_camera, &mut cameras, &mut camera_override);
    apply_audio(&mut runtime, &frame.active, &mut emitters, &spatial_audio, manifest.as_deref(), server.as_deref_mut(), audio_assets.as_deref_mut(), audio.as_deref_mut());
}

pub fn timeline_transform_system(mut runtime: ResMut<TimelineRuntime>, mut transforms: Query<(&RuntimeEntityIdentity, &mut Transform)>) {
    let desired = runtime.desired_transforms.clone(); let mut seen = BTreeSet::new();
    for (_, (identity, transform)) in transforms.iter_mut() {
        let entity_id = identity.authoring_id.clone(); seen.insert(entity_id.clone());
        let keys = desired.keys().filter(|(candidate, _)| candidate == &entity_id).cloned().collect::<Vec<_>>();
        for key in keys { let Some(target) = desired.get(&key) else { continue }; runtime.transform_originals.entry(key.clone()).or_insert_with(|| read_property(transform, &key.1)); apply_property(transform, &key.1, &target.value); }
        let stale = runtime.transform_originals.keys().filter(|(candidate, property)| candidate == &entity_id && !desired.contains_key(&(candidate.clone(), property.clone()))).cloned().collect::<Vec<_>>();
        for key in stale { if let Some(original) = runtime.transform_originals.remove(&key) { apply_property(transform, &key.1, &original); } }
    }
    runtime.transform_originals.retain(|(entity, _), _| seen.contains(entity));
}

fn choose_clip<'a>(map: &mut BTreeMap<EntityId, &'a ActiveClip>, key: EntityId, active: &'a ActiveClip) { if map.get(&key).is_none_or(|current| active.priority > current.priority) { map.insert(key, active); } }

fn apply_animation_overrides(runtime: &mut TimelineRuntime, desired: &BTreeMap<EntityId, &ActiveClip>, animators: &mut Query<(&RuntimeEntityIdentity, &mut AnimGraphPlayer, &mut Animator)>) {
    let mut seen = BTreeSet::new();
    for (_, (identity, graph, animator)) in animators.iter_mut() {
        let entity_id = identity.authoring_id.clone(); seen.insert(entity_id.clone());
        let Some(active) = desired.get(&entity_id).copied() else { if let Some(original) = runtime.animation_originals.remove(&entity_id) { *animator = original; } graph.set_timeline_overridden(false); continue; };
        let TimelineClipPayload::Animation { animation_set, motion_slot } = &active.payload else { continue };
        let Some(handle) = graph.timeline_clip_handle(animation_set, motion_slot) else { if let Some(original) = runtime.animation_originals.remove(&entity_id) { *animator = original; } graph.set_timeline_overridden(false); runtime.warn(format!("Animation binding `{}` / `{}` is not resolved on entity `{}`", animation_set.as_str(), motion_slot.as_str(), entity_id.as_str())); continue; };
        runtime.animation_originals.entry(entity_id.clone()).or_insert_with(|| animator.clone()); graph.set_timeline_overridden(true);
        let sample_tick = active.source_offset.saturating_add(active.local_tick.get()); animator.sample_timeline_pose(handle, sample_tick.to_seconds_f64() as f32, active.discontinuous_seek);
    }
    runtime.animation_originals.retain(|entity, _| seen.contains(entity));
}

fn apply_vfx_overrides(runtime: &mut TimelineRuntime, desired: &BTreeMap<EntityId, &ActiveClip>, players: &mut Query<(&RuntimeEntityIdentity, &VfxSourceIdentity, &GlobalTransform, &mut VfxPlayer)>) {
    let mut seen = BTreeSet::new();
    for (_, (identity, source, transform, player)) in players.iter_mut() {
        let entity_id = identity.authoring_id.clone(); seen.insert(entity_id.clone());
        let Some(active) = desired.get(&entity_id).copied() else { if let Some(original) = runtime.vfx_originals.remove(&entity_id) { *player = original; } else { player.set_external_clock(false); } continue; };
        let TimelineClipPayload::Vfx { effect, .. } = &active.payload else { continue };
        if &source.effect != effect { if let Some(original) = runtime.vfx_originals.remove(&entity_id) { *player = original; } else { player.set_external_clock(false); } runtime.warn(format!("VFX Timeline clip names effect `{}`, but entity `{}` owns `{}`", effect.as_str(), entity_id.as_str(), source.effect.as_str())); continue; }
        runtime.vfx_originals.entry(entity_id.clone()).or_insert_with(|| player.clone()); let sample_tick = active.source_offset.saturating_add(active.local_tick.get()); let origin = transform.matrix().col(3).truncate(); player.sample_external_time(sample_tick.to_seconds_f64() as f32, origin);
    }
    runtime.vfx_originals.retain(|entity, _| seen.contains(entity));
}

fn apply_camera_override(runtime: &mut TimelineRuntime, desired: Option<(EntityId, &ActiveClip)>, cameras: &mut Query<(&RuntimeEntityIdentity, &Camera3D)>, camera_override: &mut GameCameraSelectionOverride) {
    let Some((target, _)) = desired else { camera_override.clear(); return; };
    for (entity, (identity, _camera)) in cameras.iter_mut() { if identity.authoring_id == target { camera_override.set_target(entity); return; } }
    camera_override.clear(); runtime.warn(format!("Camera Cut target `{}` is not a live Camera3D entity", target.as_str()));
}

#[derive(Clone, Copy)]
struct EmitterSnapshot { entity: Entity, volume: f32, spatial_blend: f32, min_distance: f32, max_distance: f32, rolloff: crate::audio::AudioRolloffMode }

#[allow(clippy::too_many_arguments)]
fn apply_audio(runtime: &mut TimelineRuntime, active: &[ActiveClip], emitters: &mut Query<(&RuntimeEntityIdentity, &AudioEmitter)>, spatial: &SpatialAudioRuntime, manifest: Option<&AssetManifest>, mut server: Option<&mut AssetServer>, mut assets: Option<&mut Assets<AudioAsset>>, mut audio: Option<&mut AudioSystem>) {
    let mut emitter_map = BTreeMap::<EntityId, EmitterSnapshot>::new();
    for (entity, (identity, emitter)) in emitters.iter_mut() { emitter_map.insert(identity.authoring_id.clone(), EmitterSnapshot { entity, volume: emitter.volume, spatial_blend: emitter.spatial_blend, min_distance: emitter.min_distance, max_distance: emitter.max_distance, rolloff: emitter.rolloff }); }
    let mut selected = HashSet::new();
    for item in active {
        let TimelineClipPayload::Audio { clip, volume, looping } = &item.payload else { continue }; if item.playback_state != TimelinePlaybackState::Playing { continue; }
        let key = (item.player_id, item.clip_id.clone()); selected.insert(key.clone()); if runtime.audio_suppressed.contains(&key) { continue; } let Some(audio_system) = audio.as_deref_mut() else { continue; };
        let gains = match &item.binding {
            None => { let gain = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 0.0 }; StereoGains { left: gain, right: gain } },
            Some(TimelineBinding::Entity { entity }) => { let Some(emitter) = emitter_map.get(entity).copied() else { runtime.warn(format!("spatial Audio clip target `{}` has no production AudioEmitter", entity.as_str())); continue; }; let settings = AudioVoiceSpatialSettings { volume: (emitter.volume * *volume).clamp(0.0, 1.0), spatial_blend: emitter.spatial_blend, min_distance: emitter.min_distance, max_distance: emitter.max_distance, rolloff: emitter.rolloff }; let Some(gains) = spatial.timeline_gains(emitter.entity, settings) else { runtime.warn(format!("spatial Audio clip target `{}` has no propagated transform", entity.as_str())); continue; }; gains },
            Some(TimelineBinding::Asset { asset }) => { runtime.warn(format!("Audio clip has unsupported asset binding `{}`", asset.as_str())); continue; }
        };
        if let Some(&voice) = runtime.audio_voices.get(&key) { if let Err(error) = audio_system.update_voice(voice, gains) { runtime.audio_voices.remove(&key); runtime.audio_suppressed.insert(key); runtime.warn(format!("Timeline audio voice ended or failed: {error}")); } continue; }
        let (Some(manifest), Some(server), Some(assets)) = (manifest, server.as_deref_mut(), assets.as_deref_mut()) else { runtime.warn("Timeline audio host resources are unavailable"); continue; };
        let Some(entry) = manifest.get(clip) else { runtime.warn(format!("Audio asset `{}` is not registered", clip.as_str())); continue; };
        let handle = match server.load_audio(clip.clone(), &entry.path, assets) { Ok(handle) => handle, Err(error) => { runtime.warn(format!("Audio asset `{}` failed to load: {error}", clip.as_str())); continue; } };
        let Some(asset) = assets.get(&handle) else { runtime.warn(format!("Audio asset `{}` resolved to a missing runtime handle", clip.as_str())); continue; };
        match audio_system.start_voice(asset, gains, *looping) { Ok(voice) => { runtime.audio_voices.insert(key, voice); }, Err(error) => runtime.warn(format!("Timeline audio start failed: {error}")) }
    }
    let stale = runtime.audio_voices.keys().filter(|key| !selected.contains(*key)).cloned().collect::<Vec<_>>();
    if let Some(audio_system) = audio.as_deref_mut() { for key in stale { if let Some(voice) = runtime.audio_voices.remove(&key) && let Err(error) = audio_system.stop_voice(voice) { runtime.warn(format!("Timeline audio stop failed: {error}")); } } } else { runtime.audio_voices.clear(); }
    runtime.audio_suppressed.retain(|key| selected.contains(key));
}

fn read_property(transform: &Transform, property: &str) -> TimelinePropertyValue { match property { "engine.transform.rotation" => TimelinePropertyValue::Quat(transform.rotation.to_array().map(f64::from)), "engine.transform.scale" => TimelinePropertyValue::Vec3(transform.scale.to_array().map(f64::from)), _ => TimelinePropertyValue::Vec3(transform.translation.to_array().map(f64::from)) } }
fn apply_property(transform: &mut Transform, property: &str, value: &TimelinePropertyValue) { match (property, value) { ("engine.transform.translation", TimelinePropertyValue::Vec3(value)) => transform.translation = Vec3::from_array(value.map(|value| value as f32)), ("engine.transform.rotation", TimelinePropertyValue::Quat(value)) => transform.rotation = Quat::from_array(value.map(|value| value as f32)).normalize(), ("engine.transform.scale", TimelinePropertyValue::Vec3(value)) => transform.scale = Vec3::from_array(value.map(|value| value as f32)), _ => {} } }
