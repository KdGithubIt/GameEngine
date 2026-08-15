//! CPU particle simulation rendered through GPU instancing (Phase 49,
//! ADR 0044).
//!
//! A [`ParticleEmitter`] owns its particle pool; particles are never ECS
//! entities. [`particle_update_system`] runs on the frame schedule and the
//! render batch pass turns live particles into instanced draws of the
//! emitter's mesh.

use engine_ecs::{Query, Res};
use glam::Vec3;

use crate::asset::Handle;
use crate::mesh::Mesh;
use crate::time::Time;
use crate::transform::GlobalTransform;

/// Upper bound on particles spawned in a single frame per emitter.
///
/// Protects against a burst after a long frame (breakpoints, window drags)
/// turning the accumulated spawn debt into one giant emission.
const MAX_SPAWNS_PER_FRAME: u32 = 256;

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

/// A deterministic xorshift32 generator (ADR 0044: no `rand` dependency).
#[derive(Debug, Clone)]
struct XorShift32(u32);

impl XorShift32 {
    fn new(seed: u32) -> Self {
        // Zero is a fixed point of xorshift; nudge it to a valid state.
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Returns a uniform value in `[0, 1)`.
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Returns a uniform value in `[low, high]`.
    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.next_f32()
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
    rng: XorShift32,
    spawn_accumulator: f32,
    /// Live particle storage consumed by the high-level render adapter.
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
            rng: XorShift32::new(seed),
            spawn_accumulator: 0.0,
            particles: Vec::new(),
        }
    }

    /// Resets the random stream to `seed` and clears the pool.
    ///
    /// Two emitters with equal configuration and seed produce identical
    /// particle streams after a reset.
    pub fn reset(&mut self) {
        self.rng = XorShift32::new(self.seed);
        self.spawn_accumulator = 0.0;
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

    /// Advances the pool by `dt` seconds, spawning from `origin`.
    fn step(&mut self, dt: f32, origin: Vec3) {
        // Age and integrate existing particles, dropping expired ones.
        let gravity = self.gravity;
        for particle in &mut self.particles {
            particle.age += dt;
            particle.velocity += gravity * dt;
            let velocity = particle.velocity;
            particle.position += velocity * dt;
        }
        self.particles
            .retain(|particle| particle.age < particle.lifetime);

        if self.spawn_rate <= 0.0 {
            self.spawn_accumulator = 0.0;
            return;
        }
        self.spawn_accumulator += self.spawn_rate * dt;
        let mut to_spawn = self.spawn_accumulator.floor() as u32;
        self.spawn_accumulator -= to_spawn as f32;
        to_spawn = to_spawn.min(MAX_SPAWNS_PER_FRAME);

        for _ in 0..to_spawn {
            if self.particles.len() >= self.max_particles {
                break;
            }
            let particle = self.spawn_one(origin);
            self.particles.push(particle);
        }
    }

    /// Creates one particle at `origin` using the emitter's random stream.
    fn spawn_one(&mut self, origin: Vec3) -> Particle {
        let lifetime = self.rng.range(self.lifetime.0, self.lifetime.1).max(0.01);
        let speed = self.rng.range(self.initial_speed.0, self.initial_speed.1);

        // Build an orthonormal basis around the emission direction and tilt
        // by a random angle within the cone.
        let axis = self.direction.normalize_or_zero();
        let axis = if axis == Vec3::ZERO { Vec3::Y } else { axis };
        let ortho = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
        let tangent = axis.cross(ortho).normalize();
        let bitangent = axis.cross(tangent);
        let tilt = self.rng.range(0.0, self.spread);
        let around = self.rng.range(0.0, std::f32::consts::TAU);
        let direction = (axis * tilt.cos()
            + (tangent * around.cos() + bitangent * around.sin()) * tilt.sin())
        .normalize_or_zero();

        Particle {
            position: origin,
            velocity: direction * speed,
            age: 0.0,
            lifetime,
        }
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

        assert_eq!(emitter.live_count(), MAX_SPAWNS_PER_FRAME as usize);
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
}
