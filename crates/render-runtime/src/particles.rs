//! CPU particle simulation rendered through GPU instancing (Phase 49,
//! ADR 0044).
//!
//! [`ParticleEmitter`] is the legacy convenience API. ADR 0125 compiles its
//! public fields into the same backend-neutral VFX IR and [`VfxInstance`] used
//! by authored effects; particles remain transient runtime state rather than
//! ECS entities. [`particle_update_system`] runs on the frame schedule and the
//! render batch pass turns the compatibility particle view into instanced draws.

use engine_authoring::{
    CompiledVfxEffect, CompiledVfxEmitter, CompiledVfxOperation, VfxAttributeLayout,
    VfxCapabilityRequirements, VfxCurve, VfxCurveInterpolation, VfxCurveKey, VfxCurveKeyId,
    VfxEmitterId, VfxGradient, VfxGradientKey, VfxGradientKeyId, VfxModuleId,
    VfxModuleOperation, VfxRandomChannel, VfxScalarValue, VfxShape, VFX_SCHEMA_VERSION,
};
use engine_ecs::{Query, Res};
use glam::Vec3;

use crate::asset::Handle;
use crate::mesh::Mesh;
use crate::time::Time;
use crate::transform::GlobalTransform;
use crate::vfx::VfxInstance;

/// One live particle inside an emitter pool.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct Particle {
    /// Current world-space position consumed by the render adapter.
    pub position: Vec3,
    velocity: Vec3,
    age: f32,
    lifetime: f32,
}

impl Particle {
    /// Normalized age in `[0, 1]` used for color and size interpolation.
    #[doc(hidden)]
    pub fn life_factor(&self) -> f32 {
        if self.lifetime <= f32::EPSILON {
            1.0
        } else {
            (self.age / self.lifetime).clamp(0.0, 1.0)
        }
    }
}

/// Emits and simulates particles rendered as instanced meshes (ADR 0044).
///
/// Particles simulate in world space: each spawn captures the emitter
/// entity's [`GlobalTransform`] translation, so a moving emitter leaves a
/// trail. Attach an optional [`crate::material::Material`] to the emitter
/// entity to texture and tint all of its particles.
pub struct ParticleEmitter {
    /// Mesh drawn for every particle of this emitter.
    ///
    /// Held as a field, not a component, so the instancing batcher does not
    /// also draw the emitter entity itself.
    pub mesh: Handle<Mesh>,
    /// Particles spawned per second. `0.0` stops emission while existing
    /// particles keep simulating.
    pub spawn_rate: f32,
    /// Particle lifetime range in seconds `(min, max)`.
    pub lifetime: (f32, f32),
    /// Initial speed range along the emission cone `(min, max)`.
    pub initial_speed: (f32, f32),
    /// Base emission direction (normalized on use).
    pub direction: Vec3,
    /// Emission cone half-angle in radians (`0.0` = straight beam).
    pub spread: f32,
    /// World-space acceleration applied every frame.
    pub gravity: Vec3,
    /// RGBA color at spawn.
    pub start_color: [f32; 4],
    /// RGBA color at the end of a particle's life.
    pub end_color: [f32; 4],
    /// Uniform scale at spawn.
    pub start_size: f32,
    /// Uniform scale at the end of a particle's life.
    pub end_size: f32,
    /// Hard cap on the live particle pool.
    pub max_particles: usize,
    /// Seed for the emitter's deterministic random stream.
    pub seed: u32,
    /// Shared ADR 0125 runtime instance. The public simple-emitter fields are
    /// compiled into this instance before every simulation step.
    runtime: Option<VfxInstance>,
    /// Compatibility particle view consumed by the existing high-level render
    /// adapter. Simulation ownership remains in [`VfxInstance`].
    #[doc(hidden)]
    pub particles: Vec<Particle>,
}

impl ParticleEmitter {
    /// Creates an emitter with the given particle mesh and defaults suited
    /// to a small upward burst.
    pub fn new(mesh: Handle<Mesh>) -> Self {
        let seed = 0x51ED_5EED;
        Self {
            mesh,
            spawn_rate: 32.0,
            lifetime: (0.8, 1.6),
            initial_speed: (2.0, 4.0),
            direction: Vec3::Y,
            spread: 0.5,
            gravity: Vec3::new(0.0, -9.8, 0.0),
            start_color: [1.0, 0.9, 0.4, 1.0],
            end_color: [1.0, 0.2, 0.05, 1.0],
            start_size: 0.12,
            end_size: 0.02,
            max_particles: 1024,
            seed,
            runtime: None,
            particles: Vec::new(),
        }
    }

    /// Resets the random stream to `seed` and clears the pool.
    ///
    /// Two emitters with equal configuration and seed produce identical
    /// particle streams after a reset.
    pub fn reset(&mut self) {
        self.runtime = None;
        self.particles.clear();
    }

    /// Number of currently live particles.
    pub fn live_count(&self) -> usize {
        self.particles.len()
    }

    /// Returns an axis-aligned world-space bound of the current live pool.
    /// Editor debug drawing uses this instead of exposing particle storage.
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

    /// Rebuilds a deterministic editor preview at `elapsed_seconds`.
    ///
    /// Scene View reconstructs its runtime world whenever authoring data
    /// changes. Replaying fixed simulation steps makes preview and Restart
    /// deterministic without persisting editor-only time into the scene.
    pub fn simulate_preview(&mut self, elapsed_seconds: f32, origin: Vec3) {
        self.reset();
        let mut remaining = elapsed_seconds.clamp(0.0, 5.0);
        const PREVIEW_STEP: f32 = 1.0 / 60.0;
        while remaining > 0.0 {
            let step = remaining.min(PREVIEW_STEP);
            self.step(step, origin);
            remaining -= step;
        }
    }

    /// Interpolated RGBA color for one particle.
    #[doc(hidden)]
    pub fn color_at(&self, factor: f32) -> [f32; 4] {
        let mut color = [0.0; 4];
        for (slot, (start, end)) in color
            .iter_mut()
            .zip(self.start_color.iter().zip(self.end_color.iter()))
        {
            *slot = start + (end - start) * factor;
        }
        color
    }

    /// Interpolated uniform scale for one particle.
    #[doc(hidden)]
    pub fn size_at(&self, factor: f32) -> f32 {
        self.start_size + (self.end_size - self.start_size) * factor
    }

    /// Compiles the convenience API into the backend-neutral ADR 0125 IR.
    ///
    /// Stable source/module IDs intentionally do not change as public fields
    /// are edited. This lets [`VfxInstance::reconfigure`] preserve the live pool
    /// and keeps shape random channels deterministic across property updates.
    fn compiled_effect(&self) -> CompiledVfxEffect {
        let max_particles = u32::try_from(self.max_particles).unwrap_or(u32::MAX);
        let spawn_operations = vec![
            compiled_operation(
                1,
                VfxModuleOperation::SpawnRate {
                    particles_per_second: self.spawn_rate,
                },
            ),
            compiled_operation(
                2,
                VfxModuleOperation::Shape {
                    shape: VfxShape::Cone {
                        direction: self.direction.to_array(),
                        angle_radians: self.spread,
                        radius: 0.0,
                    },
                },
            ),
            compiled_operation(
                3,
                VfxModuleOperation::Lifetime {
                    value: VfxScalarValue::Range {
                        min: self.lifetime.0,
                        max: self.lifetime.1,
                        channel: VfxRandomChannel::new(0),
                    },
                },
            ),
            compiled_operation(
                4,
                VfxModuleOperation::InitialSpeed {
                    value: VfxScalarValue::Range {
                        min: self.initial_speed.0,
                        max: self.initial_speed.1,
                        channel: VfxRandomChannel::new(1),
                    },
                },
            ),
            compiled_operation(
                5,
                VfxModuleOperation::InitialColor {
                    color: self.start_color,
                },
            ),
            compiled_operation(
                6,
                VfxModuleOperation::InitialSize {
                    value: VfxScalarValue::Constant { value: 1.0 },
                },
            ),
        ];
        let update_operations = vec![
            compiled_operation(
                7,
                VfxModuleOperation::Force {
                    acceleration: self.gravity.to_array(),
                },
            ),
            compiled_operation(
                8,
                VfxModuleOperation::ColorOverLife {
                    gradient: simple_color_gradient(self.start_color, self.end_color),
                },
            ),
            compiled_operation(
                9,
                VfxModuleOperation::SizeOverLife {
                    curve: simple_size_curve(self.start_size, self.end_size),
                },
            ),
        ];

        CompiledVfxEffect {
            source_schema_version: VFX_SCHEMA_VERSION,
            seed: self.seed,
            max_particles,
            emitters: vec![CompiledVfxEmitter {
                source: simple_emitter_id(),
                name: "ParticleEmitter".to_owned(),
                max_particles,
                attribute_layout: VfxAttributeLayout {
                    velocity: true,
                    color: true,
                    size: true,
                    rotation: false,
                },
                spawn_operations,
                update_operations,
                // The legacy convenience API keeps its mesh Handle in the
                // render adapter; only simulation semantics are compiled here.
                render_operations: Vec::new(),
                estimated_capacity: max_particles,
            }],
            capabilities: VfxCapabilityRequirements::default(),
        }
    }

    /// Advances the pool by `dt` seconds, spawning from `origin`.
    fn step(&mut self, dt: f32, origin: Vec3) {
        let definition = self.compiled_effect();
        match &mut self.runtime {
            Some(runtime) => runtime.reconfigure(definition),
            None => self.runtime = Some(VfxInstance::new(definition, None)),
        }
        let runtime = self
            .runtime
            .as_mut()
            .expect("ParticleEmitter runtime is initialized above");
        runtime.step(dt, origin);
        self.particles.clear();
        if let Some(emitter) = runtime.emitters().first() {
            self.particles.extend(emitter.particles().iter().map(|particle| Particle {
                position: particle.position,
                velocity: particle.velocity,
                age: particle.age,
                lifetime: particle.lifetime,
            }));
        }
    }
}

fn simple_emitter_id() -> VfxEmitterId {
    VfxEmitterId::try_new("vfxemitter_00000000000000000000000000")
        .expect("hard-coded simple VFX emitter ID must be valid")
}

fn simple_module_id(index: u8) -> VfxModuleId {
    VfxModuleId::try_new(format!("vfxmodule_0000000000000000000000000{index:X}"))
        .expect("hard-coded simple VFX module ID must be valid")
}

fn compiled_operation(index: u8, operation: VfxModuleOperation) -> CompiledVfxOperation {
    CompiledVfxOperation {
        source_module: simple_module_id(index),
        operation,
    }
}

fn simple_curve_key_id(index: u8) -> VfxCurveKeyId {
    VfxCurveKeyId::try_new(format!("vfxkey_0000000000000000000000000{index:X}"))
        .expect("hard-coded simple VFX curve key ID must be valid")
}

fn simple_gradient_key_id(index: u8) -> VfxGradientKeyId {
    VfxGradientKeyId::try_new(format!("vfxgradient_0000000000000000000000000{index:X}"))
        .expect("hard-coded simple VFX gradient key ID must be valid")
}

fn simple_size_curve(start: f32, end: f32) -> VfxCurve {
    VfxCurve {
        keys: vec![
            VfxCurveKey {
                id: simple_curve_key_id(0),
                time: 0.0,
                value: start,
                interpolation: VfxCurveInterpolation::Linear,
            },
            VfxCurveKey {
                id: simple_curve_key_id(1),
                time: 1.0,
                value: end,
                interpolation: VfxCurveInterpolation::Linear,
            },
        ],
    }
}

fn simple_color_gradient(start: [f32; 4], end: [f32; 4]) -> VfxGradient {
    VfxGradient {
        keys: vec![
            VfxGradientKey {
                id: simple_gradient_key_id(0),
                time: 0.0,
                color: start,
            },
            VfxGradientKey {
                id: simple_gradient_key_id(1),
                time: 1.0,
                color: end,
            },
        ],
    }
}

/// Advances every [`ParticleEmitter`] by the frame delta.
///
/// Registered by the built-in frame schedule after transform propagation so
/// spawn positions use the emitter's current world transform.
pub fn particle_update_system(
    time: Res<Time>,
    mut emitters: Query<(&mut ParticleEmitter, &GlobalTransform)>,
) {
    let dt = time.delta_seconds;
    if dt <= 0.0 {
        return;
    }
    for (_, (emitter, global)) in emitters.iter_mut() {
        let origin = global.matrix().col(3).truncate();
        emitter.step(dt, origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::Assets;

    fn test_emitter() -> ParticleEmitter {
        let mut meshes = Assets::<Mesh>::default();
        ParticleEmitter::new(meshes.add(Mesh::cube()))
    }

    #[test]
    fn emitter_spawns_at_configured_rate() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 10.0;
        emitter.lifetime = (10.0, 10.0);

        emitter.step(1.0, Vec3::ZERO);

        assert_eq!(emitter.live_count(), 10);
    }

    #[test]
    fn particles_expire_after_lifetime() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 10.0;
        emitter.lifetime = (0.5, 0.5);

        emitter.step(1.0, Vec3::ZERO);
        assert!(emitter.live_count() > 0);
        emitter.spawn_rate = 0.0;
        emitter.step(1.0, Vec3::ZERO);

        assert_eq!(emitter.live_count(), 0);
    }

    #[test]
    fn pool_respects_max_particles_cap() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 100.0;
        emitter.lifetime = (100.0, 100.0);
        emitter.max_particles = 7;

        emitter.step(1.0, Vec3::ZERO);

        assert_eq!(emitter.live_count(), 7);
    }

    #[test]
    fn fractional_spawn_rate_accumulates_across_frames() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 0.5;
        emitter.lifetime = (100.0, 100.0);

        emitter.step(1.0, Vec3::ZERO);
        assert_eq!(emitter.live_count(), 0, "0.5 accumulated, below 1");
        emitter.step(1.0, Vec3::ZERO);
        assert_eq!(emitter.live_count(), 1, "accumulator must carry over");
    }

    #[test]
    fn equal_seeds_produce_identical_streams() {
        let mut first = test_emitter();
        let mut second = test_emitter();
        for emitter in [&mut first, &mut second] {
            emitter.seed = 42;
            emitter.reset();
            emitter.spawn_rate = 25.0;
            emitter.step(0.5, Vec3::ZERO);
            emitter.step(0.5, Vec3::ZERO);
        }

        assert_eq!(first.live_count(), second.live_count());
        for (a, b) in first.particles.iter().zip(&second.particles) {
            assert_eq!(a.position, b.position);
            assert_eq!(a.velocity, b.velocity);
            assert_eq!(a.lifetime, b.lifetime);
        }
    }

    #[test]
    fn long_frame_spawn_burst_is_capped() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 100_000.0;
        emitter.lifetime = (100.0, 100.0);
        emitter.max_particles = 100_000;

        emitter.step(1.0, Vec3::ZERO);

        assert_eq!(emitter.live_count(), 256);
    }

    #[test]
    fn zero_lifetime_and_direction_do_not_panic() {
        let mut emitter = test_emitter();
        emitter.lifetime = (0.0, 0.0);
        emitter.direction = Vec3::ZERO;
        emitter.spawn_rate = 5.0;

        emitter.step(1.0, Vec3::ZERO);

        assert!(emitter.live_count() <= 5);
    }

    #[test]
    fn color_and_size_interpolate_over_life() {
        let emitter = test_emitter();
        let start = emitter.color_at(0.0);
        let end = emitter.color_at(1.0);
        for (actual, expected) in start.iter().zip(emitter.start_color) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
        for (actual, expected) in end.iter().zip(emitter.end_color) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
        assert!((emitter.size_at(0.0) - emitter.start_size).abs() < 1.0e-6);
        assert!((emitter.size_at(1.0) - emitter.end_size).abs() < 1.0e-6);
    }

    #[test]
    fn deterministic_preview_reports_live_bounds_and_restarts() {
        let mut emitter = test_emitter();
        emitter.spawn_rate = 20.0;
        emitter.lifetime = (2.0, 2.0);
        emitter.simulate_preview(0.5, Vec3::new(3.0, 4.0, 5.0));
        let first_count = emitter.live_count();
        let first_bounds = emitter.live_bounds().expect("preview particles");

        emitter.simulate_preview(0.5, Vec3::new(3.0, 4.0, 5.0));
        assert_eq!(emitter.live_count(), first_count);
        assert_eq!(emitter.live_bounds(), Some(first_bounds));
    }

    #[test]
    fn simple_emitter_uses_shared_vfx_reference_runtime() {
        let mut emitter = test_emitter();
        emitter.seed = 77;
        emitter.spawn_rate = 16.0;
        emitter.lifetime = (1.5, 2.0);
        emitter.initial_speed = (3.0, 5.0);
        emitter.direction = Vec3::new(1.0, 2.0, 3.0);
        emitter.spread = 0.35;
        emitter.gravity = Vec3::new(0.0, -4.0, 0.0);
        emitter.max_particles = 128;
        emitter.reset();

        let definition = emitter.compiled_effect();
        let mut direct = VfxInstance::new(definition, None);
        let origin = Vec3::new(4.0, 5.0, 6.0);
        for _ in 0..30 {
            emitter.step(1.0 / 60.0, origin);
            direct.step(1.0 / 60.0, origin);
        }

        let direct_particles = direct.emitters()[0].particles();
        assert_eq!(emitter.particles.len(), direct_particles.len());
        for (simple, shared) in emitter.particles.iter().zip(direct_particles) {
            assert_eq!(simple.position, shared.position);
            assert_eq!(simple.velocity, shared.velocity);
            assert_eq!(simple.age, shared.age);
            assert_eq!(simple.lifetime, shared.lifetime);
        }
    }

    #[test]
    fn simple_emitter_compilation_is_deterministic() {
        let emitter = test_emitter();
        assert_eq!(emitter.compiled_effect(), emitter.compiled_effect());
    }
}
