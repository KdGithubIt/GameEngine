//! Runtime attack-hitbox metadata owned by validated gameplay commands.

use engine_ecs::Entity;
use glam::Vec3;
use std::collections::BTreeSet;

/// Trigger-collider metadata for one attack volume.
#[derive(Debug, Clone)]
pub struct AttackHitbox {
    /// Entity responsible for the attack.
    pub owner: Entity,
    /// Gameplay team used by later hit filtering.
    pub team: i32,
    /// Project-defined damage magnitude.
    pub damage: f32,
    /// Whether one activation may affect each target at most once.
    pub one_hit_per_target: bool,
    /// Whether collision detection should include this attack volume.
    pub enabled: bool,
    /// Monotonic activation counter used by hit-result processing.
    pub activation: u64,
    /// World-space velocity added to an accepted target hit.
    pub knockback: Vec3,
    /// Targets already observed during the current activation.
    #[doc(hidden)]
    pub hit_entities: BTreeSet<Entity>,
}

impl AttackHitbox {
    /// Creates hitbox metadata for the validated scripting/gameplay bridge.
    #[doc(hidden)]
    pub fn new(
        owner: Entity,
        team: i32,
        damage: f32,
        one_hit_per_target: bool,
        enabled: bool,
    ) -> Self {
        Self {
            owner,
            team,
            damage,
            one_hit_per_target,
            enabled,
            activation: u64::from(enabled),
            knockback: Vec3::ZERO,
            hit_entities: BTreeSet::new(),
        }
    }

    /// Applies a validated knockback vector while constructing a hitbox.
    #[doc(hidden)]
    pub fn with_knockback(mut self, knockback: Vec3) -> Self {
        self.knockback = knockback;
        self
    }

    /// Applies an enabled-state transition and resets one-hit history on activation.
    #[doc(hidden)]
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            self.activation = self.activation.saturating_add(1);
            self.hit_entities.clear();
        }
        self.enabled = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactivation_advances_generation_and_clears_hit_history() {
        let owner = Entity::from_raw(1, 0);
        let target = Entity::from_raw(2, 0);
        let mut hitbox = AttackHitbox::new(owner, 1, 10.0, true, true);
        hitbox.hit_entities.insert(target);

        hitbox.set_enabled(false);
        hitbox.set_enabled(true);

        assert_eq!(hitbox.activation, 2);
        assert!(hitbox.hit_entities.is_empty());
    }
}
