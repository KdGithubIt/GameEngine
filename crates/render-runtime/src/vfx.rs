//! CPU reference runtime for compiled VFX effects (ADR 0125).
//!
//! The authoring crate owns persisted semantics and compilation. This module
//! owns only transient runtime state: particle pools, deterministic spawn
//! streams, preview replay, and budget counters. It consumes
//! [`engine_authoring::CompiledVfxEffect`] directly so CPU and future GPU
//! backends share one backend-neutral contract.

use std::collections::{BTreeMap, BTreeSet};

use engine_authoring::{
    CompiledVfxEffect, CompiledVfxEmitter, CompiledVfxOperation, VfxEmitterId, VfxModuleId,
    VfxModuleOperation, VfxScalarValue, VfxShape, VfxVectorValue,
};
use engine_ecs::{Query, Res};
use glam::Vec3;

use crate::asset::Handle;
use crate::material::Material;
use crate::mesh::Mesh;
use crate::time::Time;
use crate::transform::GlobalTransform;

/// Runtime render binding resolved from one authored Render module.
#[derive(Clone)]
pub struct VfxRenderBinding {
    /// Mesh used by this output. Billboard outputs resolve to the built-in quad.
    pub mesh: Handle<Mesh>,
    /// Runtime material used by this output.
    pub material: Material,
    /// Whether the mesh must face the active camera.
    pub billboard: bool,
}

/// Runtime-only bindings from stable Render module IDs to resolved render assets.
#[derive(Clone, Default)]
pub struct VfxRenderBindings {
    bindings: BTreeMap<VfxModuleId, VfxRenderBinding>,
}

impl VfxRenderBindings {
    /// Inserts or replaces one render-module binding.
    pub fn insert(&mut self, module: VfxModuleId, binding: VfxRenderBinding) {
        self.bindings.insert(module, binding);
    }

    /// Resolves the runtime binding for a compiled Render operation.
    pub fn get(&self, module: &VfxModuleId) -> Option<&VfxRenderBinding> {
        self.bindings.get(module)
    }
}

/// One presentation record derived from live simulation without GPU handles.
#[derive(Debug, Clone)]
pub struct VfxRenderParticle {
    /// Stable Render module that produces this presentation record.
    pub module: VfxModuleId,
    /// World-space particle position.
    pub position: Vec3,
    /// Linear RGBA particle color.
    pub color: [f32; 4],
    /// Uniform particle scale.
    pub size: f32,
    /// Authored particle rotation in radians.
    pub rotation: f32,
    /// UV scale XY and offset ZW for texture-sheet animation.
    pub uv_transform: [f32; 4],
}

/// Maximum number of particles one emitter may request in one runtime step.
///
/// This preserves the long-frame safety property of ADR 0044 while the
/// authored emitter/effect budgets remain the primary live-particle limits.
const MAX_SPAWNS_PER_STEP: u32 = 256;
/// Fixed replay step used by deterministic preview seeking.
pub const VFX_PREVIEW_STEP_SECONDS: f32 = 1.0 / 60.0;

/// One transient live particle produced by a compiled VFX emitter.
#[derive(Debug, Clone, PartialEq)]
pub struct VfxParticle {
    /// Current world-space position.
    pub position: Vec3,
    /// Current world-space velocity.
    pub velocity: Vec3,
    /// Current linear RGBA color after update modules.
    pub color: [f32; 4],
    /// Current uniform size after update modules.
    pub size: f32,
    /// Current rotation in radians after update modules.
    pub rotation: f32,
    /// Age in seconds.
    pub age: f32,
    /// Lifetime in seconds.
    pub lifetime: f32,
    base_size: f32,
    base_rotation: f32,
}

impl VfxParticle {
    /// Returns normalized age in `[0, 1]`.
    pub fn life_factor(&self) -> f32 {
        if self.lifetime <= f32::EPSILON {
            1.0
        } else {
            (self.age / self.lifetime).clamp(0.0, 1.0)
        }
    }
}

/// Runtime state and live pool for one compiled emitter.
#[derive(Debug, Clone)]
pub struct VfxEmitterRuntime {
    definition: CompiledVfxEmitter,
    particles: Vec<VfxParticle>,
    spawn_accumulator: f32,
    spawn_serial: u64,
    fired_bursts: BTreeSet<VfxModuleId>,
    dropped_particles: u64,
    spawned_particles: u64,
}

impl VfxEmitterRuntime {
    fn new(definition: CompiledVfxEmitter) -> Self {
        Self {
            definition,
            particles: Vec::new(),
            spawn_accumulator: 0.0,
            spawn_serial: 0,
            fired_bursts: BTreeSet::new(),
            dropped_particles: 0,
            spawned_particles: 0,
        }
    }

    /// Stable source emitter ID retained from the authoring document.
    pub fn source(&self) -> &VfxEmitterId {
        &self.definition.source
    }

    /// Human-readable source emitter name retained for diagnostics.
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    /// Live particles in deterministic pool order.
    pub fn particles(&self) -> &[VfxParticle] {
        &self.particles
    }

    /// Number of currently live particles.
    pub fn live_count(&self) -> usize {
        self.particles.len()
    }

    /// Particles refused by frame or authored budgets since the last restart.
    pub fn dropped_particles(&self) -> u64 {
        self.dropped_particles
    }

    /// Particles accepted into this emitter pool since the last restart.
    pub fn spawned_particles(&self) -> u64 {
        self.spawned_particles
    }

    /// Backend-neutral render operations associated with this emitter.
    pub fn render_operations(&self) -> &[CompiledVfxOperation] {
        &self.definition.render_operations
    }

    /// Axis-aligned world-space bounds of the current live pool.
    pub fn live_bounds(&self) -> Option<(Vec3, Vec3)> {
        let first = self.particles.first()?.position;
        let mut min = first;
        let mut max = first;
        for particle in &self.particles[1..] {
            min = min.min(particle.position);
            max = max.max(particle.position);
        }
        Some((min, max))
    }
}

/// Runtime backend currently executing an effect instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VfxRuntimeBackend {
    /// Deterministic CPU reference backend shared by packaged/editor runtime.
    #[default]
    CpuReference,
}

/// Aggregate counters exposed to Editor preview and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VfxRuntimeStats {
    /// Current live particles across all emitters.
    pub live_particles: usize,
    /// Particles accepted into live pools since the last restart.
    pub spawned_particles: u64,
    /// Particles refused by budgets or the per-step safety cap.
    pub dropped_particles: u64,
    /// Runtime backend used by this instance.
    pub backend: VfxRuntimeBackend,
}

/// Deterministic CPU reference instance for one compiled VFX effect.
#[derive(Debug, Clone)]
pub struct VfxInstance {
    effect: CompiledVfxEffect,
    emitters: Vec<VfxEmitterRuntime>,
    seed_override: Option<u32>,
    active_seed: u32,
    elapsed_seconds: f32,
}

impl VfxInstance {
    /// Creates a stopped-at-zero runtime instance from backend-neutral IR.
    ///
    /// `seed_override` implements the instance-level deterministic preview and
    /// `engine.vfx_player` override contract without mutating the asset.
    pub fn new(effect: CompiledVfxEffect, seed_override: Option<u32>) -> Self {
        let active_seed = seed_override.unwrap_or(effect.seed);
        let emitters = effect
            .emitters
            .iter()
            .cloned()
            .map(VfxEmitterRuntime::new)
            .collect();
        Self {
            effect,
            emitters,
            seed_override,
            active_seed,
            elapsed_seconds: 0.0,
        }
    }

    /// Current backend-neutral effect definition.
    pub fn effect(&self) -> &CompiledVfxEffect {
        &self.effect
    }

    /// Current runtime emitter pools in compiled document order.
    pub fn emitters(&self) -> &[VfxEmitterRuntime] {
        &self.emitters
    }

    /// Current simulated time in seconds.
    pub fn elapsed_seconds(&self) -> f32 {
        self.elapsed_seconds
    }

    /// Current seed override, if one was supplied for this instance.
    pub fn seed_override(&self) -> Option<u32> {
        self.seed_override
    }

    /// Aggregate live/spawned/dropped counters and backend identity.
    pub fn stats(&self) -> VfxRuntimeStats {
        VfxRuntimeStats {
            live_particles: self.emitters.iter().map(VfxEmitterRuntime::live_count).sum(),
            spawned_particles: self
                .emitters
                .iter()
                .map(VfxEmitterRuntime::spawned_particles)
                .sum(),
            dropped_particles: self
                .emitters
                .iter()
                .map(VfxEmitterRuntime::dropped_particles)
                .sum(),
            backend: VfxRuntimeBackend::CpuReference,
        }
    }

    /// Returns `true` when no live particles remain and no future emission is pending.
    pub fn is_complete(&self) -> bool {
        if self.stats().live_particles != 0 {
            return false;
        }
        self.emitters.iter().all(|runtime| {
            runtime.definition.spawn_operations.iter().all(|operation| match &operation.operation {
                VfxModuleOperation::SpawnRate {
                    particles_per_second,
                } => *particles_per_second <= 0.0,
                VfxModuleOperation::Burst { .. } => {
                    runtime.fired_bursts.contains(&operation.source_module)
                }
                _ => true,
            })
        })
    }

    /// Restarts the effect from time zero and resets deterministic streams.
    pub fn restart(&mut self) {
        self.active_seed = self.seed_override.unwrap_or(self.effect.seed);
        self.elapsed_seconds = 0.0;
        self.emitters = self
            .effect
            .emitters
            .iter()
            .cloned()
            .map(VfxEmitterRuntime::new)
            .collect();
    }

    /// Replaces compiled definitions while preserving compatible transient pools.
    ///
    /// Emitters are matched by stable source ID. This is used by affected VFX
    /// recompilation and by the simple-emitter adapter when public properties
    /// change. Removed emitters disappear; newly added emitters start empty.
    pub fn reconfigure(&mut self, effect: CompiledVfxEffect) {
        let mut previous = std::mem::take(&mut self.emitters);
        let mut emitters = Vec::with_capacity(effect.emitters.len());
        for definition in effect.emitters.iter().cloned() {
            if let Some(index) = previous
                .iter()
                .position(|runtime| runtime.definition.source == definition.source)
            {
                let mut runtime = previous.remove(index);
                runtime.definition = definition;
                runtime
                    .particles
                    .truncate(runtime.definition.max_particles as usize);
                emitters.push(runtime);
            } else {
                emitters.push(VfxEmitterRuntime::new(definition));
            }
        }
        self.effect = effect;
        self.emitters = emitters;
        self.enforce_effect_cap();
    }

    /// Advances the effect by `dt` seconds using `origin` as emitter world origin.
    ///
    /// Non-finite or non-positive deltas are ignored. Particle simulation is
    /// visual/runtime state and intentionally remains outside persisted ECS data.
    pub fn step(&mut self, dt: f32, origin: Vec3) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let end_time = self.elapsed_seconds + dt;

        for runtime in &mut self.emitters {
            update_particles(runtime, dt);
        }

        let mut remaining_effect_capacity = (self.effect.max_particles as usize)
            .saturating_sub(self.emitters.iter().map(VfxEmitterRuntime::live_count).sum());

        for (emitter_index, runtime) in self.emitters.iter_mut().enumerate() {
            let requested = requested_spawns(runtime, dt, end_time);
            let accepted_by_step = requested.min(MAX_SPAWNS_PER_STEP);
            runtime.dropped_particles = runtime
                .dropped_particles
                .saturating_add(u64::from(requested.saturating_sub(accepted_by_step)));

            for _ in 0..accepted_by_step {
                if runtime.particles.len() >= runtime.definition.max_particles as usize
                    || remaining_effect_capacity == 0
                {
                    runtime.dropped_particles = runtime.dropped_particles.saturating_add(1);
                    continue;
                }
                let particle = spawn_particle(
                    &runtime.definition,
                    self.active_seed,
                    emitter_index as u32,
                    runtime.spawn_serial,
                    origin,
                );
                runtime.spawn_serial = runtime.spawn_serial.wrapping_add(1);
                runtime.particles.push(particle);
                runtime.spawned_particles = runtime.spawned_particles.saturating_add(1);
                remaining_effect_capacity -= 1;
            }
        }

        self.elapsed_seconds = end_time;
    }

    /// Rebuilds deterministic preview state at an arbitrary non-negative time.
    ///
    /// Seeking backwards is implemented as Restart + fixed-step replay. The
    /// same path is also used for forward seeks so preview results do not
    /// depend on the Editor frame rate.
    pub fn seek_preview(&mut self, elapsed_seconds: f32, origin: Vec3) {
        self.restart();
        if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return;
        }
        let mut remaining = elapsed_seconds;
        while remaining > 0.0 {
            let step = remaining.min(VFX_PREVIEW_STEP_SECONDS);
            self.step(step, origin);
            remaining -= step;
        }
    }

    fn enforce_effect_cap(&mut self) {
        let mut remaining = self.effect.max_particles as usize;
        for runtime in &mut self.emitters {
            if runtime.particles.len() <= remaining {
                remaining -= runtime.particles.len();
                continue;
            }
            let removed = runtime.particles.len() - remaining;
            runtime.particles.truncate(remaining);
            runtime.dropped_particles = runtime.dropped_particles.saturating_add(removed as u64);
            remaining = 0;
        }
    }
}

/// Playback completion policy used by scene VFX players.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VfxRestartPolicy {
    /// Leave the completed effect stopped until explicitly restarted.
    #[default]
    Manual,
    /// Restart automatically when all finite emission and live particles finish.
    OnComplete,
}

/// Transient scene runtime component that plays one compiled VFX effect.
#[derive(Debug, Clone)]
pub struct VfxPlayer {
    instance: VfxInstance,
    playing: bool,
    /// Starts playback when the runtime component is created.
    pub autoplay: bool,
    /// Restarts immediately after a finite effect completes.
    pub looping: bool,
    /// Completion policy used when `looping` is disabled.
    pub restart_policy: VfxRestartPolicy,
    /// Per-instance playback time multiplier.
    pub time_scale: f32,
    /// Named scalar instance overrides reserved by the scene contract.
    pub parameter_overrides: BTreeMap<String, f32>,
}

impl VfxPlayer {
    /// Creates one runtime player from already compiled effect IR.
    pub fn new(
        effect: CompiledVfxEffect,
        autoplay: bool,
        looping: bool,
        restart_policy: VfxRestartPolicy,
        time_scale: f32,
        seed_override: Option<u32>,
        parameter_overrides: BTreeMap<String, f32>,
    ) -> Self {
        Self {
            instance: VfxInstance::new(effect, seed_override),
            playing: autoplay,
            autoplay,
            looping,
            restart_policy,
            time_scale: if time_scale.is_finite() { time_scale.max(0.0) } else { 1.0 },
            parameter_overrides,
        }
    }

    /// Shared backend-neutral runtime instance.
    pub fn instance(&self) -> &VfxInstance {
        &self.instance
    }

    /// Mutable runtime instance for affected recompilation and preview tools.
    pub fn instance_mut(&mut self) -> &mut VfxInstance {
        &mut self.instance
    }

    /// Whether simulation currently advances.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Extracts backend-neutral presentation records for every live Render output.
    pub fn render_particles(&self) -> Vec<VfxRenderParticle> {
        let mut output = Vec::new();
        for emitter in self.instance.emitters() {
            for operation in emitter.render_operations() {
                let (VfxModuleOperation::Billboard { texture_sheet, .. }
                | VfxModuleOperation::Mesh { texture_sheet, .. }) = &operation.operation
                else {
                    continue;
                };
                for particle in emitter.particles() {
                    output.push(VfxRenderParticle {
                        module: operation.source_module.clone(),
                        position: particle.position,
                        color: particle.color,
                        size: particle.size,
                        rotation: particle.rotation,
                        uv_transform: texture_sheet
                            .as_ref()
                            .map_or([1.0, 1.0, 0.0, 0.0], |sheet| {
                                texture_sheet_uv(sheet, particle.life_factor())
                            }),
                    });
                }
            }
        }
        output
    }

    /// Resumes playback without resetting transient state.
    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Pauses playback without discarding transient state.
    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Restarts deterministic state and resumes playback.
    pub fn restart(&mut self) {
        self.instance.restart();
        self.playing = true;
    }

    /// Advances playback and applies the configured completion policy.
    pub fn step(&mut self, dt: f32, origin: Vec3) {
        if !self.playing {
            return;
        }
        self.instance.step(dt * self.time_scale, origin);
        if self.instance.is_complete() {
            if self.looping || self.restart_policy == VfxRestartPolicy::OnComplete {
                self.instance.restart();
            } else {
                self.playing = false;
            }
        }
    }
}

/// Advances every scene [`VfxPlayer`] from the frame clock.
pub fn vfx_update_system(time: Res<'_, Time>, mut query: Query<'_, (&mut VfxPlayer, &GlobalTransform)>) {
    let dt = time.delta_seconds();
    for (player, transform) in &mut query {
        player.step(dt, transform.translation());
    }
}

fn texture_sheet_uv(sheet: &engine_authoring::VfxTextureSheet, life: f32) -> [f32; 4] {
    let columns = sheet.columns.max(1);
    let rows = sheet.rows.max(1);
    let frame_count = columns.saturating_mul(rows).max(1);
    let normalized = sheet.frame_over_life.evaluate(life).clamp(0.0, 1.0);
    let frame = ((normalized * frame_count as f32).floor() as u32).min(frame_count - 1);
    let column = frame % columns;
    let row = frame / columns;
    let scale_x = 1.0 / columns as f32;
    let scale_y = 1.0 / rows as f32;
    [
        scale_x,
        scale_y,
        column as f32 * scale_x,
        row as f32 * scale_y,
    ]
}

fn requested_spawns(runtime: &mut VfxEmitterRuntime, dt: f32, end_time: f32) -> u32 {
    let mut rate = 0.0_f32;
    let mut bursts = 0_u64;
    for operation in &runtime.definition.spawn_operations {
        match &operation.operation {
            VfxModuleOperation::SpawnRate {
                particles_per_second,
            } => rate += (*particles_per_second).max(0.0),
            VfxModuleOperation::Burst { time, count }
                if *time <= end_time && !runtime.fired_bursts.contains(&operation.source_module) =>
            {
                runtime.fired_bursts.insert(operation.source_module.clone());
                bursts = bursts.saturating_add(u64::from(*count));
            }
            _ => {}
        }
    }

    let continuous = if rate > 0.0 {
        runtime.spawn_accumulator += rate * dt;
        let continuous = runtime.spawn_accumulator.floor().max(0.0) as u64;
        runtime.spawn_accumulator -= continuous as f32;
        continuous
    } else {
        runtime.spawn_accumulator = 0.0;
        0
    };
    continuous
        .saturating_add(bursts)
        .min(u64::from(u32::MAX)) as u32
}

fn update_particles(runtime: &mut VfxEmitterRuntime, dt: f32) {
    for particle in &mut runtime.particles {
        particle.age += dt;
        let factor = particle.life_factor();
        for operation in &runtime.definition.update_operations {
            match &operation.operation {
                VfxModuleOperation::Force { acceleration } => {
                    particle.velocity += Vec3::from_array(*acceleration) * dt;
                }
                VfxModuleOperation::Drag { coefficient } => {
                    particle.velocity /= 1.0 + (*coefficient).max(0.0) * dt;
                }
                VfxModuleOperation::ColorOverLife { gradient } => {
                    particle.color = gradient.evaluate(factor);
                }
                VfxModuleOperation::SizeOverLife { curve } => {
                    particle.size = particle.base_size * curve.evaluate(factor);
                }
                VfxModuleOperation::RotationOverLife { curve } => {
                    particle.rotation = particle.base_rotation + curve.evaluate(factor);
                }
                _ => {}
            }
        }
        particle.position += particle.velocity * dt;
    }
    runtime
        .particles
        .retain(|particle| particle.age < particle.lifetime);
}

fn spawn_particle(
    definition: &CompiledVfxEmitter,
    effect_seed: u32,
    emitter_index: u32,
    spawn_serial: u64,
    origin: Vec3,
) -> VfxParticle {
    let mut position = origin;
    let mut direction = Vec3::Y;
    let mut lifetime = 1.0_f32;
    let mut velocity = Vec3::ZERO;
    let mut color = [1.0; 4];
    let mut size = 1.0_f32;
    let mut rotation = 0.0_f32;

    for operation in &definition.spawn_operations {
        match &operation.operation {
            VfxModuleOperation::Shape { shape } => {
                let sampled = sample_shape(
                    shape,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                    module_salt(&operation.source_module),
                );
                position = origin + sampled.0;
                direction = sampled.1;
            }
            VfxModuleOperation::Lifetime { value } => {
                lifetime = sample_scalar(
                    value,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                    0,
                )
                .max(0.01);
            }
            VfxModuleOperation::InitialSpeed { value } => {
                let speed = sample_scalar(
                    value,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                    0,
                );
                velocity = direction.normalize_or_zero() * speed;
            }
            VfxModuleOperation::InitialVelocity { value } => {
                velocity = Vec3::from_array(sample_vector(
                    value,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                ));
            }
            VfxModuleOperation::InitialColor { color: initial } => color = *initial,
            VfxModuleOperation::InitialSize { value } => {
                size = sample_scalar(
                    value,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                    0,
                )
                .max(0.0);
            }
            VfxModuleOperation::InitialRotation { value } => {
                rotation = sample_scalar(
                    value,
                    effect_seed,
                    emitter_index,
                    spawn_serial,
                    0,
                );
            }
            _ => {}
        }
    }

    VfxParticle {
        position,
        velocity,
        color,
        size,
        rotation,
        age: 0.0,
        lifetime,
        base_size: size,
        base_rotation: rotation,
    }
}

fn sample_scalar(
    value: &VfxScalarValue,
    seed: u32,
    emitter_index: u32,
    spawn_serial: u64,
    lane: u32,
) -> f32 {
    match value {
        VfxScalarValue::Constant { value } => *value,
        VfxScalarValue::Range { min, max, channel } => {
            min + (max - min) * random_unit(seed, emitter_index, spawn_serial, channel.index(), lane)
        }
    }
}

fn sample_vector(
    value: &VfxVectorValue,
    seed: u32,
    emitter_index: u32,
    spawn_serial: u64,
) -> [f32; 3] {
    match value {
        VfxVectorValue::Constant { value } => *value,
        VfxVectorValue::Range { min, max, channel } => std::array::from_fn(|lane| {
            min[lane]
                + (max[lane] - min[lane])
                    * random_unit(
                        seed,
                        emitter_index,
                        spawn_serial,
                        channel.index(),
                        lane as u32,
                    )
        }),
    }
}

fn sample_shape(
    shape: &VfxShape,
    seed: u32,
    emitter_index: u32,
    spawn_serial: u64,
    salt: u32,
) -> (Vec3, Vec3) {
    let sample = |lane| random_unit(seed, emitter_index, spawn_serial, salt, lane);
    match shape {
        VfxShape::Point => (Vec3::ZERO, Vec3::Y),
        VfxShape::Box { half_extents } => {
            let extent = Vec3::from_array(*half_extents);
            let offset = Vec3::new(
                sample(0) * 2.0 - 1.0,
                sample(1) * 2.0 - 1.0,
                sample(2) * 2.0 - 1.0,
            ) * extent;
            let direction = if offset.length_squared() > f32::EPSILON {
                offset.normalize()
            } else {
                Vec3::Y
            };
            (offset, direction)
        }
        VfxShape::Sphere { radius } => {
            let z = sample(0) * 2.0 - 1.0;
            let angle = sample(1) * std::f32::consts::TAU;
            let radial = (1.0 - z * z).max(0.0).sqrt();
            let direction = Vec3::new(radial * angle.cos(), z, radial * angle.sin());
            let distance = sample(2).cbrt() * *radius;
            (direction * distance, direction)
        }
        VfxShape::Cone {
            direction,
            angle_radians,
            radius,
        } => {
            let axis = Vec3::from_array(*direction).normalize_or_zero();
            let axis = if axis == Vec3::ZERO { Vec3::Y } else { axis };
            let helper = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
            let tangent = axis.cross(helper).normalize();
            let bitangent = axis.cross(tangent);
            let around = sample(0) * std::f32::consts::TAU;
            let tilt = sample(1) * *angle_radians;
            let radial_dir = tangent * around.cos() + bitangent * around.sin();
            let spawn_direction =
                (axis * tilt.cos() + radial_dir * tilt.sin()).normalize_or_zero();
            let disk_radius = sample(2).sqrt() * *radius;
            (radial_dir * disk_radius, spawn_direction)
        }
    }
}

fn random_unit(seed: u32, emitter_index: u32, serial: u64, channel: u32, lane: u32) -> f32 {
    let mut value = u64::from(seed)
        ^ (u64::from(emitter_index).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        ^ serial.wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ (u64::from(channel).wrapping_mul(0x94D0_49BB_1331_11EB))
        ^ (u64::from(lane).wrapping_mul(0xD6E8_FEB8_6659_FD93));
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    ((value >> 40) as u32) as f32 / (1_u32 << 24) as f32
}

fn module_salt(module: &VfxModuleId) -> u32 {
    let mut hash = 0x811C_9DC5_u32;
    for byte in module.as_str().bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        CompiledVfxOperation, VfxAttributeLayout, VfxCapabilityRequirements, VfxCurve,
        VfxCurveInterpolation, VfxCurveKey, VfxCurveKeyId, VfxGradient, VfxModuleOperation,
        VfxRandomChannel,
    };

    fn emitter_id() -> VfxEmitterId {
        VfxEmitterId::try_new("vfxemitter_00000000000000000000000000")
            .expect("test emitter ID")
    }

    fn module_id(index: u8) -> VfxModuleId {
        VfxModuleId::try_new(format!("vfxmodule_0000000000000000000000000{index:X}"))
            .expect("test module ID")
    }

    fn operation(index: u8, operation: VfxModuleOperation) -> CompiledVfxOperation {
        CompiledVfxOperation {
            source_module: module_id(index),
            operation,
        }
    }

    fn effect(max_particles: u32) -> CompiledVfxEffect {
        CompiledVfxEffect {
            source_schema_version: 1,
            seed: 42,
            max_particles,
            emitters: vec![CompiledVfxEmitter {
                source: emitter_id(),
                name: "test".to_owned(),
                max_particles,
                attribute_layout: VfxAttributeLayout {
                    velocity: true,
                    color: true,
                    size: true,
                    rotation: false,
                },
                spawn_operations: vec![
                    operation(
                        1,
                        VfxModuleOperation::SpawnRate {
                            particles_per_second: 20.0,
                        },
                    ),
                    operation(
                        2,
                        VfxModuleOperation::Shape {
                            shape: VfxShape::Cone {
                                direction: [0.0, 1.0, 0.0],
                                angle_radians: 0.5,
                                radius: 0.0,
                            },
                        },
                    ),
                    operation(
                        3,
                        VfxModuleOperation::Lifetime {
                            value: VfxScalarValue::Range {
                                min: 1.0,
                                max: 2.0,
                                channel: VfxRandomChannel::new(0),
                            },
                        },
                    ),
                    operation(
                        4,
                        VfxModuleOperation::InitialSpeed {
                            value: VfxScalarValue::Range {
                                min: 2.0,
                                max: 4.0,
                                channel: VfxRandomChannel::new(1),
                            },
                        },
                    ),
                    operation(
                        5,
                        VfxModuleOperation::InitialColor {
                            color: [1.0, 0.5, 0.25, 1.0],
                        },
                    ),
                    operation(
                        6,
                        VfxModuleOperation::InitialSize {
                            value: VfxScalarValue::Constant { value: 2.0 },
                        },
                    ),
                ],
                update_operations: vec![
                    operation(
                        7,
                        VfxModuleOperation::Force {
                            acceleration: [0.0, -9.8, 0.0],
                        },
                    ),
                    operation(
                        8,
                        VfxModuleOperation::ColorOverLife {
                            gradient: VfxGradient::linear(
                                [1.0, 0.5, 0.25, 1.0],
                                [0.0, 0.0, 0.0, 0.0],
                            ),
                        },
                    ),
                    operation(
                        9,
                        VfxModuleOperation::SizeOverLife {
                            curve: VfxCurve {
                                keys: vec![
                                    VfxCurveKey {
                                        id: VfxCurveKeyId::generate(),
                                        time: 0.0,
                                        value: 1.0,
                                        interpolation: VfxCurveInterpolation::Linear,
                                    },
                                    VfxCurveKey {
                                        id: VfxCurveKeyId::generate(),
                                        time: 1.0,
                                        value: 0.0,
                                        interpolation: VfxCurveInterpolation::Linear,
                                    },
                                ],
                            },
                        },
                    ),
                ],
                render_operations: Vec::new(),
                estimated_capacity: max_particles,
            }],
            capabilities: VfxCapabilityRequirements::default(),
        }
    }

    #[test]
    fn equal_seeds_produce_identical_reference_simulation() {
        let definition = effect(128);
        let mut first = VfxInstance::new(definition.clone(), None);
        let mut second = VfxInstance::new(definition, None);
        for _ in 0..60 {
            first.step(1.0 / 60.0, Vec3::new(1.0, 2.0, 3.0));
            second.step(1.0 / 60.0, Vec3::new(1.0, 2.0, 3.0));
        }
        assert_eq!(first.stats(), second.stats());
        assert_eq!(first.emitters()[0].particles(), second.emitters()[0].particles());
    }

    #[test]
    fn effect_and_emitter_caps_report_dropped_particles() {
        let mut definition = effect(3);
        definition.emitters[0].max_particles = 2;
        let mut instance = VfxInstance::new(definition, None);
        instance.step(1.0, Vec3::ZERO);
        assert_eq!(instance.stats().live_particles, 2);
        assert!(instance.stats().dropped_particles >= 18);
    }

    #[test]
    fn runtime_stats_report_spawn_count_and_backend() {
        let mut instance = VfxInstance::new(effect(128), None);
        instance.step(0.5, Vec3::ZERO);
        let stats = instance.stats();
        assert_eq!(stats.live_particles, 10);
        assert_eq!(stats.spawned_particles, 10);
        assert_eq!(stats.dropped_particles, 0);
        assert_eq!(stats.backend, VfxRuntimeBackend::CpuReference);
    }

    #[test]
    fn backward_preview_seek_restarts_and_replays_deterministically() {
        let mut instance = VfxInstance::new(effect(128), Some(7));
        instance.seek_preview(1.0, Vec3::ZERO);
        let first = instance.emitters()[0].particles().to_vec();
        instance.seek_preview(2.0, Vec3::ZERO);
        instance.seek_preview(1.0, Vec3::ZERO);
        assert_eq!(instance.emitters()[0].particles(), first.as_slice());
    }

    #[test]
    fn reconfigure_preserves_pool_for_same_stable_emitter() {
        let mut definition = effect(128);
        let mut instance = VfxInstance::new(definition.clone(), None);
        instance.step(0.5, Vec3::ZERO);
        let live_before = instance.stats().live_particles;
        definition.emitters[0].spawn_operations[0].operation = VfxModuleOperation::SpawnRate {
            particles_per_second: 0.0,
        };
        instance.reconfigure(definition);
        assert_eq!(instance.stats().live_particles, live_before);
        instance.step(0.1, Vec3::ZERO);
        assert!(instance.stats().live_particles <= live_before);
    }

    #[test]
    fn burst_at_zero_fires_once() {
        let mut definition = effect(128);
        definition.emitters[0].spawn_operations = vec![operation(
            1,
            VfxModuleOperation::Burst { time: 0.0, count: 4 },
        )];
        let mut instance = VfxInstance::new(definition, None);
        instance.step(0.1, Vec3::ZERO);
        assert_eq!(instance.stats().live_particles, 4);
        instance.step(0.1, Vec3::ZERO);
        assert_eq!(instance.stats().live_particles, 4);
    }

    #[test]
    fn player_pause_restart_and_loop_use_reference_instance() {
        let mut definition = effect(32);
        definition.emitters[0].spawn_operations = vec![operation(
            1,
            VfxModuleOperation::Burst { time: 0.0, count: 1 },
        )];
        definition.emitters[0].update_operations.clear();
        definition.emitters[0].spawn_operations.push(operation(
            2,
            VfxModuleOperation::Lifetime {
                value: VfxScalarValue::Constant { value: 0.1 },
            },
        ));
        let mut player = VfxPlayer::new(
            definition,
            false,
            true,
            VfxRestartPolicy::Manual,
            1.0,
            Some(9),
            BTreeMap::new(),
        );
        assert!(!player.is_playing());
        player.play();
        player.step(0.05, Vec3::ZERO);
        assert!(player.is_playing());
        player.pause();
        assert!(!player.is_playing());
        player.restart();
        assert!(player.is_playing());
    }
}
