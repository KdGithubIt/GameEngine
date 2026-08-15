//! Player-owned configuration and captured movement intent independent of input devices.

use std::collections::HashMap;

use engine_ecs::Entity;
use glam::Vec2;

/// Marks the entity controlled by the player.
#[derive(Debug, Clone, Copy)]
pub struct PlayerMarker;

/// Determines which world plane player movement is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePlane {
    /// Move along the XZ plane (standard 3D top-down or third-person).
    Xz,
    /// Move along the XY plane (2D side-scroller).
    Xy,
}

/// Configurable logical player movement settings.
///
/// Input capture, camera-relative projection, and physical motor integration
/// are adapters above this data contract.
#[derive(Debug, Clone)]
pub struct PlayerController {
    /// World units per second.
    pub move_speed: f32,
    /// Which plane movement is applied to.
    pub move_plane: MovePlane,
    /// Maximum velocity increase per second while movement is requested.
    pub acceleration: f32,
    /// Maximum velocity decrease per second after movement is released.
    pub deceleration: f32,
    /// Speed multiplier applied while the standard `sprint` action is held.
    pub sprint_multiplier: f32,
    /// Whether XZ movement follows the active Game View camera's planar basis.
    pub camera_relative: bool,
    /// Whether the entity turns its local -Z axis toward movement.
    pub face_movement: bool,
}

impl Default for PlayerController {
    fn default() -> Self {
        Self {
            move_speed: 3.0,
            move_plane: MovePlane::Xz,
            acceleration: 24.0,
            deceleration: 32.0,
            sprint_multiplier: 1.5,
            camera_relative: true,
            face_movement: true,
        }
    }
}

/// One rendered frame's logical player input, consumed by the fixed motor.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayerMovementIntent {
    /// Logical movement where X is right and Y is forward.
    pub movement: Vec2,
    /// Whether the standard `sprint` action is held.
    pub sprint_requested: bool,
    /// Whether the standard `dodge` action began during this frame.
    pub dodge_requested: bool,
    /// Monotonic input-capture generation that produced this intent.
    pub capture_generation: u64,
}

/// Latest movement intent for every player-controlled runtime entity.
///
/// Update-schedule input adapters capture transitions here so a short press is
/// not lost when a rendered frame happens to dispatch no fixed step.
#[derive(Debug, Clone, Default)]
pub struct PlayerMovementIntents {
    intents: HashMap<Entity, PlayerMovementIntent>,
    capture_count: u64,
}

impl PlayerMovementIntents {
    /// Returns the latest intent captured for `entity`.
    pub fn get(&self, entity: Entity) -> Option<PlayerMovementIntent> {
        self.intents.get(&entity).copied()
    }

    /// Returns the generation to use for the next rendered-frame capture.
    #[doc(hidden)]
    pub fn next_capture_generation(&self) -> u64 {
        self.capture_count.saturating_add(1)
    }

    /// Replaces a complete rendered-frame capture atomically.
    #[doc(hidden)]
    pub fn replace_capture(
        &mut self,
        capture_count: u64,
        intents: HashMap<Entity, PlayerMovementIntent>,
    ) {
        self.capture_count = capture_count;
        self.intents = intents;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_controller_preserves_third_person_movement_policy() {
        let controller = PlayerController::default();
        assert_eq!(controller.move_plane, MovePlane::Xz);
        assert!(controller.camera_relative);
        assert!(controller.face_movement);
        assert!(controller.move_speed > 0.0);
        assert!(controller.acceleration > 0.0);
        assert!(controller.deceleration > 0.0);
    }

    #[test]
    fn replace_capture_updates_generation_and_entity_intent() {
        let entity = Entity::from_raw(3, 0);
        let intent = PlayerMovementIntent {
            movement: Vec2::new(1.0, -0.5),
            sprint_requested: true,
            dodge_requested: false,
            capture_generation: 4,
        };
        let mut intents = PlayerMovementIntents::default();
        intents.replace_capture(4, HashMap::from([(entity, intent)]));

        assert_eq!(intents.next_capture_generation(), 5);
        assert_eq!(intents.get(entity), Some(intent));
    }
}
