//! Kinematic character-controller state without a concrete collision solver.

use glam::Vec3;

/// State and tuning for the engine kinematic character controller.
///
/// The default `solver` feature additionally exposes the movement solver. This
/// contract-only definition keeps gameplay components and command payloads
/// available without compiling the Rapier backend.
#[derive(Debug, Clone)]
pub struct KinematicCharacterController {
    /// Current linear velocity, integrated into translation each fixed step.
    pub velocity: Vec3,
    /// Multiplier applied to the world gravity acceleration.
    pub gravity_scale: f32,
    /// Whether the previous resolve pass found walkable ground.
    pub grounded: bool,
    /// Maximum number of push-out resolve passes per fixed step.
    pub max_resolve_iterations: u32,
    /// Steepest contact normal treated as walkable ground, in degrees.
    pub slope_limit_degrees: f32,
    /// Maximum vertical ledge height that horizontal motion may climb.
    pub step_offset: f32,
    /// Distance used to retain contact with descending ground.
    pub ground_snap_distance: f32,
    /// Small separation margin used for motion subdivision and contact tests.
    pub skin_width: f32,
}

impl Default for KinematicCharacterController {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            gravity_scale: 1.0,
            grounded: false,
            max_resolve_iterations: 3,
            slope_limit_degrees: 50.0,
            step_offset: 0.3,
            ground_snap_distance: 0.15,
            skin_width: 0.02,
        }
    }
}
