//! Timeline composition and runtime application (ADR 0126).
//!
//! `engine-timeline` owns time semantics and produces typed outputs. This
//! module is the composition layer that applies those outputs to the running
//! world: it writes supported transform properties, installs the transient
//! camera-selection override, and forwards sequence events to the ordinary
//! host event path. The neutral core stays free of every domain touched here.

mod audio_adapter;
mod vfx;

pub use engine_timeline::{
    ActiveClip, AdapterTokens, ClipTransition, CompiledClip, CompiledClipPayload, CompiledCurve,
    CompiledKey, CompiledMarker, CompiledTimeline, CompiledTrack, CurveInterpolation,
    DEFAULT_REPLAY_CHECKPOINT_INTERVAL_TICKS, DEFAULT_REPLAY_CHECKPOINT_LIMIT,
    DEFAULT_REPLAY_DEBOUNCE, DEFAULT_REPLAY_STEP_TICKS, FiredEvent, LoopRegion,
    ReplayCancellationToken, ReplayCheckpointCache, ReplayCheckpointConfigError,
    ReplayReconstruction, ReplayRequest, ReplayRequestController, TimelineCompileError,
    TimelineEvaluation, TimelinePlayState, TimelinePlayer, TimelineSeek, TimelineTrackOutput,
    TrackDescriptor, TrackRegistry, TrackSeekPolicy, VfxAction, compile_timeline,
};

use crate::anim_graph::AnimGraphPlayer;
use crate::animation::{Animator, AnimatorPlaybackSnapshot, AnimatorState};
use crate::script_api::RuntimeEntityIdentity;
use crate::time::FixedTime;
use crate::transform::Transform;
use engine_authoring::{
    AssetId, EntityId, MotionSlotId, TimelineProperty, TimelineTick, TimelineTrackId,
};
use engine_ecs::{Entity, World};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

/// Largest number of Timeline events retained for one fixed step.
///
/// Sequence events enter the same bounded host path as other gameplay events,
/// so a Timeline cannot grow an unbounded queue when nothing drains it.
pub const MAX_TIMELINE_EVENTS: usize = 256;

/// Runtime Timeline playback component.
///
/// The compiled schedule is shared; every field that differs between two
/// entities playing the same Timeline lives in the player beside it.
#[derive(Debug, Clone)]
pub struct TimelinePlayerComponent {
    /// Immutable compiled schedule shared with other players.
    pub timeline: Arc<CompiledTimeline>,
    /// Transient playback state owned by this entity.
    pub player: TimelinePlayer,
    /// Whether the component starts playing when the scene spawns.
    pub autoplay: bool,
    /// Adapter tokens kept between evaluations.
    pub tokens: AdapterTokens,
    animation_overrides: BTreeMap<TimelineTrackId, TimelineAnimationOverride>,
}

impl TimelinePlayerComponent {
    /// Creates a stopped player for one compiled Timeline.
    pub fn new(timeline: Arc<CompiledTimeline>) -> Self {
        Self {
            timeline,
            player: TimelinePlayer::new(),
            autoplay: false,
            tokens: AdapterTokens::default(),
            animation_overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct TimelineAnimationOverride {
    target: Entity,
    snapshot: AnimatorPlaybackSnapshot,
}

/// Runtime binding from authoring entity identity to a live runtime entity.
///
/// Timeline bindings are authored against stable identity, so the composition
/// layer resolves them here rather than persisting a runtime entity anywhere.
#[derive(Debug, Default)]
pub struct TimelineBindings {
    entities: BTreeMap<String, Entity>,
}

impl TimelineBindings {
    fn from_world(world: &mut World) -> Self {
        let mut bindings = Self::default();
        let Ok(query) = world.query::<&RuntimeEntityIdentity>() else {
            return bindings;
        };
        for (entity, identity) in query.iter() {
            bindings.bind(&identity.authoring_id, entity);
        }
        bindings
    }

    /// Records the runtime entity spawned for one authoring identity.
    pub fn bind(&mut self, authoring: &EntityId, entity: Entity) {
        self.entities
            .insert(authoring.as_stable_id().as_str().to_owned(), entity);
    }

    /// Resolves one authored binding to a live runtime entity.
    pub fn resolve(&self, authoring: &EntityId) -> Option<Entity> {
        self.entities
            .get(authoring.as_stable_id().as_str())
            .copied()
    }

    /// Drops every binding, as a scene change must.
    pub fn clear(&mut self) {
        self.entities.clear();
    }
}

/// Why one Timeline output could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineBindingDiagnostic {
    /// The authored target no longer resolves to a live entity.
    UnresolvedEntity {
        /// Authoring identity that failed to resolve.
        authoring: String,
    },
    /// The resolved entity does not carry the component the track writes.
    MissingComponent {
        /// Authoring identity that resolved.
        authoring: String,
        /// Component the track requires.
        component: &'static str,
    },
    /// The target Animation Controller is not using the Animation Set bound by the track.
    AnimationSetMismatch {
        /// Authoring identity that resolved.
        authoring: String,
        /// Animation Set required by the Timeline track.
        expected: AssetId,
        /// Animation Set currently resolved by the target controller, when known.
        actual: Option<AssetId>,
    },
    /// The bound Animation Set does not expose the authored motion slot.
    MissingMotionSlot {
        /// Authoring identity that resolved.
        authoring: String,
        /// Stable motion slot requested by the Timeline clip.
        motion_slot: MotionSlotId,
    },
}

/// Transient camera-selection override installed by an active Camera Cut clip.
///
/// The override never edits persisted camera priority or enabled state, and it
/// disappears as soon as the clip interval ends or the player stops.
#[derive(Debug, Default)]
pub struct TimelineCameraOverride {
    active: Option<TimelineCameraSelection>,
}

/// One camera selection contributed by a Timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineCameraSelection {
    /// Runtime entity carrying the selected camera.
    pub camera: Entity,
    /// Player entity whose Timeline installed the override.
    pub source: Entity,
}

impl TimelineCameraOverride {
    /// Currently installed override, if any.
    pub fn active(&self) -> Option<&TimelineCameraSelection> {
        self.active.as_ref()
    }

    /// Clears the override, as a stop, clip exit, or binding failure must.
    pub fn clear(&mut self) {
        self.active = None;
    }

    /// Installs one override, resolving ties by player entity order.
    ///
    /// Two Timelines cutting at the same instant are resolved deterministically
    /// by player entity rather than by evaluation order, and the loser is
    /// reported so the conflict is visible instead of silent.
    pub fn install(
        &mut self,
        selection: TimelineCameraSelection,
    ) -> Option<TimelineCameraSelection> {
        match self.active.as_ref() {
            Some(current) if current.source <= selection.source => Some(selection),
            _ => self.active.replace(selection),
        }
    }
}

/// Bounded queue of sequence events produced by Timeline evaluation.
#[derive(Debug, Default)]
pub struct TimelineEvents {
    events: Vec<TimelineEventRecord>,
    dropped: u64,
}

/// One sequence event with its source Timeline identity.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEventRecord {
    /// Stable event name authored on the marker or Event clip.
    pub event: String,
    /// Runtime entity whose Timeline produced the event.
    pub source: Entity,
    /// Tick the event was produced at.
    pub tick: TimelineTick,
}

impl TimelineEvents {
    /// Records one event, dropping it when the bounded queue is full.
    pub fn push(&mut self, record: TimelineEventRecord) {
        if self.events.len() >= MAX_TIMELINE_EVENTS {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.events.push(record);
    }

    /// Copied view of the events produced this step.
    pub fn events(&self) -> &[TimelineEventRecord] {
        &self.events
    }

    /// Events dropped because the bounded queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Clears the queue at the start of a step.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Diagnostics produced by the most recent Timeline step.
#[derive(Debug, Default)]
pub struct TimelineDiagnostics {
    entries: Vec<TimelineBindingDiagnostic>,
}

impl TimelineDiagnostics {
    /// Diagnostics from the most recent step.
    pub fn iter(&self) -> impl Iterator<Item = &TimelineBindingDiagnostic> {
        self.entries.iter()
    }

    /// Records one diagnostic.
    pub fn push(&mut self, diagnostic: TimelineBindingDiagnostic) {
        self.entries.push(diagnostic);
    }

    /// Clears diagnostics at the start of a step.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Applies one property output to a transform.
///
/// The supported property set is closed by design: a Timeline writes exactly
/// the fields it declares, never arbitrary component memory.
pub fn apply_property(transform: &mut Transform, property: TimelineProperty, value: f32) {
    match property {
        TimelineProperty::TranslationX => transform.translation.x = value,
        TimelineProperty::TranslationY => transform.translation.y = value,
        TimelineProperty::TranslationZ => transform.translation.z = value,
        TimelineProperty::RotationX => set_euler(transform, 0, value),
        TimelineProperty::RotationY => set_euler(transform, 1, value),
        TimelineProperty::RotationZ => set_euler(transform, 2, value),
        TimelineProperty::ScaleX => transform.scale.x = value,
        TimelineProperty::ScaleY => transform.scale.y = value,
        TimelineProperty::ScaleZ => transform.scale.z = value,
    }
}

fn set_euler(transform: &mut Transform, axis: usize, degrees: f32) {
    let (x, y, z) = transform.rotation.to_euler(glam::EulerRot::XYZ);
    let mut angles = [x, y, z];
    angles[axis] = degrees.to_radians();
    transform.rotation =
        glam::Quat::from_euler(glam::EulerRot::XYZ, angles[0], angles[1], angles[2]);
}

/// Advances every Timeline player and applies its evaluation.
///
/// Ordering is deterministic: players are advanced in entity order, and each
/// player applies its own evaluation in compiled track order.
pub fn advance_timelines(
    delta_seconds: f32,
    world: &mut World,
    bindings: &TimelineBindings,
    camera_override: &mut TimelineCameraOverride,
    events: &mut TimelineEvents,
    diagnostics: &mut TimelineDiagnostics,
) {
    events.clear();
    diagnostics.clear();
    camera_override.clear();

    let Ok(query) = world.query::<&TimelinePlayerComponent>() else {
        return;
    };
    let players = query.iter().map(|(entity, _)| entity).collect::<Vec<_>>();
    for entity in players.iter().copied() {
        let (timeline, evaluation, state, generation, previous_tick, pending_vfx_seek, mut tokens) = {
            let Some(component) = world.get_component_mut::<TimelinePlayerComponent>(entity) else {
                continue;
            };
            if component.autoplay && component.player.state() == TimelinePlayState::Stopped {
                component.player.play();
            }
            let pending_vfx_seek = vfx::pending_seek(component);
            let timeline = Arc::clone(&component.timeline);
            let evaluation = component.player.advance(&timeline, delta_seconds);
            let state = component.player.state();
            let generation = component.player.generation();
            let previous_tick = component.player.previous_tick();
            let tokens = std::mem::take(&mut component.tokens);
            (
                timeline,
                evaluation,
                state,
                generation,
                previous_tick,
                pending_vfx_seek,
                tokens,
            )
        };

        let audio_input = audio_adapter::AudioEvaluationInput::new(
            entity,
            &timeline,
            &evaluation,
            state,
            generation,
        );
        audio_adapter::apply_audio_evaluation(audio_input, world, bindings, diagnostics);

        if let Some(seek) = pending_vfx_seek {
            vfx::apply_seek(&timeline, seek, world, bindings, &mut tokens, diagnostics);
            vfx::mark_seek_applied(&mut tokens, seek);
        }
        vfx::apply_evaluation(
            &timeline,
            &evaluation,
            previous_tick,
            world,
            bindings,
            &mut tokens,
            diagnostics,
        );

        apply_evaluation(
            entity,
            &evaluation,
            world,
            bindings,
            camera_override,
            events,
            diagnostics,
        );
        if let Some(component) = world.get_component_mut::<TimelinePlayerComponent>(entity) {
            component.tokens = tokens;
        }
    }
    audio_adapter::cleanup_stale_sources(world, &players);
}

/// Shared fixed-step Timeline bridge used by Editor Play and packaged Player.
///
/// Timeline outputs can target arbitrary authoring identities and several
/// runtime domains, so the final `engine` composition layer owns this
/// exclusive bridge rather than forcing those dependencies into
/// `engine-timeline`. Stable bindings are rebuilt from the runtime identities
/// produced by scene conversion; runtime entity handles are never persisted.
pub fn timeline_fixed_system(world: &mut World) -> Result<(), Infallible> {
    let delta_seconds = world
        .get_resource::<FixedTime>()
        .map_or(0.0, |time| time.fixed_delta);
    let bindings = TimelineBindings::from_world(world);
    let mut camera_override = world
        .remove_resource::<TimelineCameraOverride>()
        .unwrap_or_default();
    let mut events = world
        .remove_resource::<TimelineEvents>()
        .unwrap_or_default();
    let mut diagnostics = world
        .remove_resource::<TimelineDiagnostics>()
        .unwrap_or_default();

    advance_timelines(
        delta_seconds,
        world,
        &bindings,
        &mut camera_override,
        &mut events,
        &mut diagnostics,
    );

    world.insert_resource(camera_override);
    world.insert_resource(events);
    world.insert_resource(diagnostics);
    Ok(())
}

/// Applies one already-computed evaluation to the world.
pub fn apply_evaluation(
    source: Entity,
    evaluation: &TimelineEvaluation,
    world: &mut World,
    bindings: &TimelineBindings,
    camera_override: &mut TimelineCameraOverride,
    events: &mut TimelineEvents,
    diagnostics: &mut TimelineDiagnostics,
) {
    let mut animation_overrides = world
        .get_component_mut::<TimelinePlayerComponent>(source)
        .map(|component| std::mem::take(&mut component.animation_overrides))
        .unwrap_or_default();

    // Restore in reverse track order so overlapping animation tracks unwind in
    // the opposite order they were applied. A later track may have captured
    // the earlier track's override as its suspended state.
    for exited in evaluation.exited.iter().rev() {
        if let Some(override_state) = animation_overrides.remove(&exited.track) {
            restore_animation_override(world, override_state);
        }
    }

    for fired in &evaluation.events {
        events.push(TimelineEventRecord {
            event: fired.event.clone(),
            source,
            tick: evaluation.tick,
        });
    }
    for active in &evaluation.active {
        match &active.output {
            TimelineTrackOutput::Property {
                entity,
                property,
                value,
            } => {
                let Some(authoring) = entity else {
                    continue;
                };
                let Some(target) = bindings.resolve(authoring) else {
                    diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                    });
                    continue;
                };
                match world.get_component_mut::<Transform>(target) {
                    Some(transform) => apply_property(transform, *property, *value),
                    None => diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                        component: "Transform",
                    }),
                }
            }
            TimelineTrackOutput::CameraCut { camera, .. } => {
                let Some(target) = bindings.resolve(camera) else {
                    diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
                        authoring: camera.as_stable_id().as_str().to_owned(),
                    });
                    continue;
                };
                camera_override.install(TimelineCameraSelection {
                    camera: target,
                    source,
                });
            }
            TimelineTrackOutput::Animation {
                entity,
                animation_set,
                motion_slot,
                speed,
                looping,
            } => {
                let Some(authoring) = entity else {
                    continue;
                };
                let Some(target) = bindings.resolve(authoring) else {
                    diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                    });
                    continue;
                };
                let Some(animation_set) = animation_set else {
                    continue;
                };
                let Some(graph_player) = world.get_component::<AnimGraphPlayer>(target) else {
                    diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                        component: "AnimGraphPlayer",
                    });
                    continue;
                };
                let actual_set = graph_player
                    .debug_source()
                    .map(|source| source.animation_set_asset.clone());
                if actual_set.as_ref() != Some(animation_set) {
                    diagnostics.push(TimelineBindingDiagnostic::AnimationSetMismatch {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                        expected: animation_set.clone(),
                        actual: actual_set,
                    });
                    continue;
                }
                let Some(clip) = graph_player.clip_handle(motion_slot.as_str()) else {
                    diagnostics.push(TimelineBindingDiagnostic::MissingMotionSlot {
                        authoring: authoring.as_stable_id().as_str().to_owned(),
                        motion_slot: motion_slot.clone(),
                    });
                    continue;
                };
                let raw_time = (active.offset.max(0) as f32
                    / CompiledTimeline::ticks_per_second() as f32)
                    * *speed;
                let sample_time = world
                    .get_resource::<crate::asset::Assets<crate::animation::AnimationClip>>()
                    .and_then(|clips| clips.get(&clip))
                    .map_or(raw_time, |clip_asset| {
                        let duration = clip_asset.duration.max(0.0);
                        if *looping && duration > f32::EPSILON {
                            raw_time.rem_euclid(duration)
                        } else {
                            raw_time.clamp(0.0, duration)
                        }
                    });
                let new_override = !animation_overrides.contains_key(&active.track);
                {
                    let Some(animator) = world.get_component_mut::<Animator>(target) else {
                        diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
                            authoring: authoring.as_stable_id().as_str().to_owned(),
                            component: "Animator",
                        });
                        continue;
                    };

                    if new_override {
                        animation_overrides.insert(
                            active.track.clone(),
                            TimelineAnimationOverride {
                                target,
                                snapshot: animator.playback_snapshot(),
                            },
                        );
                    }
                    if animator.clip != clip || animator.is_fading() {
                        animator.crossfade_to(clip, 0.0);
                    }
                    animator.state = AnimatorState::Playing;
                    animator.time = sample_time;
                    animator.looping = *looping;
                    // Timeline owns this clock while the clip is active. The
                    // animation system still samples the pose later in this same
                    // fixed step, but a zero multiplier prevents a second advance.
                    animator.playback_speed = 0.0;
                }
                if new_override
                    && let Some(graph_player) = world.get_component_mut::<AnimGraphPlayer>(target)
                {
                    graph_player.begin_external_override();
                }
            }
            // Audio and VFX are stateful domain adapters applied before this
            // world-output pass. Their typed outputs remain visible to the
            // neutral evaluator, while composition ownership stays here.
            TimelineTrackOutput::Audio { .. }
            | TimelineTrackOutput::Vfx { .. }
            | TimelineTrackOutput::Event => {}
        }
    }

    if let Some(component) = world.get_component_mut::<TimelinePlayerComponent>(source) {
        component.animation_overrides = animation_overrides;
    } else {
        for override_state in animation_overrides.into_values().rev() {
            restore_animation_override(world, override_state);
        }
    }
}

fn restore_animation_override(world: &mut World, override_state: TimelineAnimationOverride) {
    if let Some(animator) = world.get_component_mut::<Animator>(override_state.target) {
        animator.restore_playback_snapshot(override_state.snapshot);
    }
    if let Some(graph_player) = world.get_component_mut::<AnimGraphPlayer>(override_state.target) {
        graph_player.end_external_override();
    }
}

/// Copied Timeline state a project system may read.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePlayerView {
    /// Current playhead position.
    pub tick: TimelineTick,
    /// Sequence duration.
    pub duration: TimelineTick,
    /// Whether the player is advancing.
    pub playing: bool,
    /// Playback rate multiplier.
    pub rate: f32,
    /// Completed loop repetitions.
    pub loops_completed: u32,
}

/// Reads one copied Timeline view.
///
/// Project code receives copied state, never a mutable player or an adapter
/// reference, exactly as the ADR 0052 command and view family requires.
pub fn timeline_view(world: &World, entity: Entity) -> Option<TimelinePlayerView> {
    let component = world.get_component::<TimelinePlayerComponent>(entity)?;
    Some(TimelinePlayerView {
        tick: component.player.tick(),
        duration: component.timeline.duration,
        playing: component.player.state() == TimelinePlayState::Playing,
        rate: component.player.rate(),
        loops_completed: component.player.loops_completed(),
    })
}

/// One deferred Timeline control a project system may request.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineControl {
    /// Starts or resumes playback.
    Play,
    /// Holds the playhead.
    Pause,
    /// Resets the playhead and clears loop progress.
    Stop,
    /// Moves the playhead to an exact tick.
    Seek {
        /// Target tick.
        tick: TimelineTick,
    },
    /// Sets the playback rate multiplier.
    SetRate {
        /// Non-negative finite rate.
        rate: f32,
    },
}

/// Why one deferred Timeline control was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineControlError {
    /// The target entity carries no Timeline player.
    MissingPlayer,
    /// The requested rate is not a finite non-negative multiplier.
    InvalidRate,
}

/// Applies one deferred Timeline control to a live player.
pub fn apply_timeline_control(
    world: &mut World,
    entity: Entity,
    control: TimelineControl,
) -> Result<(), TimelineControlError> {
    let mut animation_overrides = Vec::new();
    {
        let Some(component) = world.get_component_mut::<TimelinePlayerComponent>(entity) else {
            return Err(TimelineControlError::MissingPlayer);
        };
        match control {
            TimelineControl::Play => component.player.play(),
            TimelineControl::Pause => component.player.pause(),
            TimelineControl::Stop => {
                component.player.stop();
                component.tokens.clear();
                vfx::mark_seek(component);
                animation_overrides = std::mem::take(&mut component.animation_overrides)
                    .into_values()
                    .rev()
                    .collect();
            }
            TimelineControl::Seek { tick } => {
                // A discontinuous Timeline seek abandons the previous adapter
                // interval. Restore the graph-owned Animator first; the next
                // evaluation captures a fresh snapshot if the seek lands
                // inside another Animation clip.
                animation_overrides = std::mem::take(&mut component.animation_overrides)
                    .into_values()
                    .rev()
                    .collect();
                let timeline = Arc::clone(&component.timeline);
                component
                    .player
                    .seek(&timeline, tick, TimelineSeek::Playback);
                vfx::mark_seek(component);
            }
            TimelineControl::SetRate { rate } => {
                if !component.player.set_rate(rate) {
                    return Err(TimelineControlError::InvalidRate);
                }
            }
        }
    }
    for override_state in animation_overrides {
        restore_animation_override(world, override_state);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::AnimationClip;
    use crate::asset::Assets;
    use engine_authoring::{
        TimelineBinding, TimelineClip, TimelineClipId, TimelineClipPayload, TimelineDocument,
        TimelineInterpolation, TimelineKey, TimelineMarker, TimelineMarkerId, TimelineTrack,
        TimelineTrackId, TimelineTrackKind,
    };

    fn ramp_document(entity: &EntityId) -> TimelineDocument {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Property,
            name: "Move".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(entity.clone()),
                asset: None,
            },
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick(24_000),
                payload: TimelineClipPayload::Property {
                    property: TimelineProperty::TranslationX,
                    keys: vec![
                        TimelineKey {
                            tick: TimelineTick::ZERO,
                            value: 0.0,
                            interpolation: TimelineInterpolation::Linear,
                        },
                        TimelineKey {
                            tick: TimelineTick(24_000),
                            value: 10.0,
                            interpolation: TimelineInterpolation::Linear,
                        },
                    ],
                },
            }],
        });
        document.markers.push(TimelineMarker {
            id: TimelineMarkerId::generate(),
            tick: TimelineTick(12_000),
            name: "beat".to_owned(),
            event: "cutscene.beat".to_owned(),
        });
        document
    }

    fn camera_document(camera: &EntityId) -> TimelineDocument {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::CameraCut,
            name: "Cut".to_owned(),
            enabled: true,
            binding: TimelineBinding::default(),
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick(24_000),
                payload: TimelineClipPayload::CameraCut {
                    camera: camera.clone(),
                },
            }],
        });
        document
    }

    fn animation_document(
        entity: &EntityId,
        animation_set: &AssetId,
        motion_slot: &MotionSlotId,
    ) -> TimelineDocument {
        let mut document = TimelineDocument::new(TimelineTick(48_000));
        document.tracks.push(TimelineTrack {
            id: TimelineTrackId::generate(),
            kind: TimelineTrackKind::Animation,
            name: "Motion".to_owned(),
            enabled: true,
            binding: TimelineBinding {
                entity: Some(entity.clone()),
                asset: Some(animation_set.clone()),
            },
            clips: vec![TimelineClip {
                id: TimelineClipId::generate(),
                start: TimelineTick::ZERO,
                end: TimelineTick(48_000),
                payload: TimelineClipPayload::Animation {
                    motion_slot: motion_slot.as_str().to_owned(),
                    speed: 2.0,
                    looping: false,
                },
            }],
        });
        document
    }

    #[test]
    fn a_property_track_writes_only_the_field_it_declares() {
        let mut world = World::new();
        let target = world.spawn().expect("entity");
        world
            .add_component(target, Transform::default())
            .expect("transform");
        let authoring = EntityId::generate();
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring, target);

        let timeline = Arc::new(compile_timeline(&ramp_document(&authoring)).expect("compile"));
        let player = world.spawn().expect("entity");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(player, component).expect("player");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.25,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );

        let transform = world
            .get_component::<Transform>(target)
            .expect("target transform");
        assert!((transform.translation.x - 5.0).abs() < 0.001);
        assert_eq!(transform.translation.y, 0.0);
        assert_eq!(transform.scale, glam::Vec3::ONE);
        assert!(diagnostics.iter().next().is_none());
    }

    #[test]
    fn animation_track_samples_motion_slot_time_and_restores_graph_playback() {
        let mut world = World::new();
        let target = world.spawn().expect("target");
        let authoring = EntityId::generate();
        let animation_set = AssetId::generate();
        let motion_slot = MotionSlotId::generate();
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring, target);

        let mut clips = Assets::<AnimationClip>::default();
        let idle = clips.add(AnimationClip {
            duration: 4.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let walk = clips.add(AnimationClip {
            duration: 2.0,
            channels: Vec::new(),
            morph_channels: Vec::new(),
            events: Vec::new(),
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        });
        let mut animator = Animator::playing(idle);
        animator.time = 0.75;
        animator.set_looping(true);
        assert!(animator.set_playback_speed(1.25));
        world.add_component(target, animator).expect("animator");

        let graph = engine_authoring::CompiledAnimGraph {
            states: Vec::new(),
            transitions: Vec::new(),
            entry_state: 0,
            compile_warnings: Vec::new(),
        };
        let mut motion_slots = BTreeMap::new();
        motion_slots.insert(motion_slot.as_str().to_owned(), walk);
        let mut graph_player = AnimGraphPlayer::new(graph, motion_slots);
        graph_player.set_debug_source(crate::anim_graph::AnimationGraphDebugSource {
            graph_asset: AssetId::generate(),
            graph_id: engine_authoring::GraphId::generate(),
            animation_set_asset: animation_set.clone(),
            transition_edges: Vec::new(),
            motion_bindings: BTreeMap::new(),
        });
        world
            .add_component(target, graph_player)
            .expect("graph player");

        let timeline = Arc::new(
            compile_timeline(&animation_document(
                &authoring,
                &animation_set,
                &motion_slot,
            ))
            .expect("compile"),
        );
        let player = world.spawn().expect("player");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world
            .add_component(player, component)
            .expect("player component");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.25,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );

        let animator = world.get_component::<Animator>(target).expect("animator");
        assert_eq!(animator.clip, walk);
        assert!((animator.time - 0.5).abs() < 1.0e-6);
        assert_eq!(animator.playback_speed, 0.0);
        assert!(!animator.looping);
        assert!(diagnostics.iter().next().is_none());
        assert!(
            world
                .get_component::<AnimGraphPlayer>(target)
                .expect("graph player")
                .external_override_active()
        );

        apply_timeline_control(
            &mut world,
            player,
            TimelineControl::Seek {
                tick: TimelineTick(36_000),
            },
        )
        .expect("seek");
        advance_timelines(
            0.0,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );

        let animator = world
            .get_component::<Animator>(target)
            .expect("seeked animator");
        assert_eq!(animator.clip, walk);
        assert!((animator.time - 1.5).abs() < 1.0e-6);
        assert!(
            world
                .get_component::<AnimGraphPlayer>(target)
                .expect("graph player")
                .external_override_active()
        );

        advance_timelines(
            0.25,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(
            timeline_view(&world, player).expect("timeline view").tick,
            TimelineTick(48_000)
        );
        let animator = world
            .get_component::<Animator>(target)
            .expect("restored animator");
        assert_eq!(animator.clip, idle);
        assert!((animator.time - 0.75).abs() < 1.0e-6);
        assert!((animator.playback_speed - 1.25).abs() < 1.0e-6);
        assert!(animator.looping);
        assert_eq!(animator.state, AnimatorState::Playing);
        assert!(
            !world
                .get_component::<AnimGraphPlayer>(target)
                .expect("graph player")
                .external_override_active()
        );
    }

    #[test]
    fn a_marker_crossing_reaches_the_bounded_host_event_queue() {
        let mut world = World::new();
        let target = world.spawn().expect("entity");
        world
            .add_component(target, Transform::default())
            .expect("transform");
        let authoring = EntityId::generate();
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring, target);
        let timeline = Arc::new(compile_timeline(&ramp_document(&authoring)).expect("compile"));
        let player = world.spawn().expect("entity");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(player, component).expect("player");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.5,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(events.events().len(), 1);
        assert_eq!(events.events()[0].event, "cutscene.beat");
        assert_eq!(events.events()[0].source, player);

        // A second step past the marker does not repeat it.
        advance_timelines(
            0.1,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert!(events.events().is_empty());
    }

    #[test]
    fn an_unresolved_binding_reports_a_diagnostic_instead_of_writing_elsewhere() {
        let mut world = World::new();
        let authoring = EntityId::generate();
        let bindings = TimelineBindings::default();
        let timeline = Arc::new(compile_timeline(&ramp_document(&authoring)).expect("compile"));
        let player = world.spawn().expect("entity");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(player, component).expect("player");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.25,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert!(matches!(
            diagnostics.iter().next(),
            Some(TimelineBindingDiagnostic::UnresolvedEntity { .. })
        ));
    }

    #[test]
    fn a_camera_cut_installs_a_transient_override_that_clears_on_stop() {
        let mut world = World::new();
        let camera_entity = world.spawn().expect("entity");
        let authoring_camera = EntityId::generate();
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring_camera, camera_entity);
        let timeline =
            Arc::new(compile_timeline(&camera_document(&authoring_camera)).expect("compile"));
        let player = world.spawn().expect("entity");
        let mut component = TimelinePlayerComponent::new(timeline);
        component.autoplay = true;
        world.add_component(player, component).expect("player");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.1,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert_eq!(
            camera_override.active().map(|selection| selection.camera),
            Some(camera_entity)
        );

        // Leaving the clip interval drops the override without touching any
        // persisted camera state.
        advance_timelines(
            0.5,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert!(camera_override.active().is_none());

        // Stopping does the same for a player that is not set to autoplay.
        if let Some(component) = world.get_component_mut::<TimelinePlayerComponent>(player) {
            component.autoplay = false;
        }
        apply_timeline_control(&mut world, player, TimelineControl::Stop).expect("stop");
        advance_timelines(
            0.1,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );
        assert!(camera_override.active().is_none());
    }

    #[test]
    fn conflicting_camera_cuts_resolve_by_player_entity_rather_than_query_order() {
        let mut world = World::new();
        let first_camera = world.spawn().expect("entity");
        let second_camera = world.spawn().expect("entity");
        let first_player = world.spawn().expect("entity");
        let second_player = world.spawn().expect("entity");

        let mut camera_override = TimelineCameraOverride::default();
        assert!(
            camera_override
                .install(TimelineCameraSelection {
                    camera: second_camera,
                    source: second_player,
                })
                .is_none()
        );
        // The lower player entity wins deterministically, and the losing
        // selection is returned rather than silently discarded.
        let displaced = camera_override.install(TimelineCameraSelection {
            camera: first_camera,
            source: first_player,
        });
        assert_eq!(
            camera_override.active().map(|selection| selection.camera),
            Some(first_camera)
        );
        assert_eq!(
            displaced.map(|selection| selection.camera),
            Some(second_camera)
        );
    }

    #[test]
    fn project_control_rejects_an_invalid_rate_and_a_missing_player() {
        let mut world = World::new();
        let authoring = EntityId::generate();
        let timeline = Arc::new(compile_timeline(&ramp_document(&authoring)).expect("compile"));
        let player = world.spawn().expect("entity");
        world
            .add_component(player, TimelinePlayerComponent::new(timeline))
            .expect("player");
        let stranger = world.spawn().expect("entity");

        assert_eq!(
            apply_timeline_control(&mut world, stranger, TimelineControl::Play),
            Err(TimelineControlError::MissingPlayer)
        );
        assert_eq!(
            apply_timeline_control(&mut world, player, TimelineControl::SetRate { rate: -1.0 }),
            Err(TimelineControlError::InvalidRate)
        );
        apply_timeline_control(&mut world, player, TimelineControl::SetRate { rate: 2.0 })
            .expect("valid rate");
        let view = timeline_view(&world, player).expect("view");
        assert_eq!(view.rate, 2.0);
        assert!(!view.playing);
    }

    #[test]
    fn two_players_of_one_timeline_keep_independent_state() {
        let mut world = World::new();
        let authoring = EntityId::generate();
        let target = world.spawn().expect("entity");
        world
            .add_component(target, Transform::default())
            .expect("transform");
        let mut bindings = TimelineBindings::default();
        bindings.bind(&authoring, target);
        let timeline = Arc::new(compile_timeline(&ramp_document(&authoring)).expect("compile"));

        let first = world.spawn().expect("entity");
        let second = world.spawn().expect("entity");
        let mut component = TimelinePlayerComponent::new(Arc::clone(&timeline));
        component.autoplay = true;
        world.add_component(first, component).expect("first");
        world
            .add_component(second, TimelinePlayerComponent::new(timeline))
            .expect("second");

        let mut camera_override = TimelineCameraOverride::default();
        let mut events = TimelineEvents::default();
        let mut diagnostics = TimelineDiagnostics::default();
        advance_timelines(
            0.25,
            &mut world,
            &bindings,
            &mut camera_override,
            &mut events,
            &mut diagnostics,
        );

        assert_eq!(
            timeline_view(&world, first).expect("first view").tick,
            TimelineTick(12_000)
        );
        assert_eq!(
            timeline_view(&world, second).expect("second view").tick,
            TimelineTick::ZERO
        );
    }

    #[test]
    fn runtime_bindings_resolve_scene_identity_without_persisting_runtime_handles() {
        let mut world = World::new();
        let target = world.spawn().expect("target");
        let authoring = EntityId::generate();
        world
            .add_component(
                target,
                RuntimeEntityIdentity {
                    authoring_id: authoring.clone(),
                    name: "Target".to_owned(),
                },
            )
            .expect("identity");

        let bindings = TimelineBindings::from_world(&mut world);
        assert_eq!(bindings.resolve(&authoring), Some(target));
    }

    #[test]
    fn the_event_queue_is_bounded_and_reports_what_it_dropped() {
        let mut events = TimelineEvents::default();
        let source = World::new().spawn().expect("entity");
        for index in 0..(MAX_TIMELINE_EVENTS + 5) {
            events.push(TimelineEventRecord {
                event: format!("event_{index}"),
                source,
                tick: TimelineTick::ZERO,
            });
        }
        assert_eq!(events.events().len(), MAX_TIMELINE_EVENTS);
        assert_eq!(events.dropped(), 5);
    }
}
