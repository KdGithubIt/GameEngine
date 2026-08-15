//! Deterministic combat-contact primitives built on fixed-step collision data.

use std::collections::BTreeMap;

use engine_core::time::FixedTime;
use engine_ecs::{Entity, Query, Res, ResMut};
use engine_physics::character_controller::KinematicCharacterController;
use engine_physics::collision::CollisionEvents;
use glam::Vec3;

use crate::hitbox::AttackHitbox;

/// Authorable health, team, and invulnerability state for a combat target.
#[derive(Debug, Clone)]
pub struct DamageReceiver {
    /// Faction identifier compared with an attack hitbox's team.
    pub team: i32,
    /// Current hit points. The combat system clamps this to zero.
    pub health: f32,
    /// Upper bound used by gameplay healing and editor inspection.
    pub max_health: f32,
    /// Duration of invulnerability started after an accepted hit.
    pub invulnerability_seconds: f32,
    /// Fixed-step countdown remaining before another hit is accepted.
    pub invulnerability_remaining: f32,
}

impl Default for DamageReceiver {
    fn default() -> Self {
        Self {
            team: 0,
            health: 100.0,
            max_health: 100.0,
            invulnerability_seconds: 0.1,
            invulnerability_remaining: 0.0,
        }
    }
}

/// Immutable record of one damage application in the latest fixed step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HitResult {
    /// Entity that owns the attack.
    pub attacker: Entity,
    /// Trigger entity that produced the contact.
    pub hitbox: Entity,
    /// Entity whose [`DamageReceiver`] was changed.
    pub target: Entity,
    /// Damage removed from the target this step.
    pub damage: f32,
    /// Target health after damage was applied.
    pub remaining_health: f32,
    /// Hitbox activation generation for combo correlation.
    pub activation: u64,
}

/// Bounded-by-collider-count hit records produced by the latest fixed step.
#[derive(Debug, Default)]
pub struct HitResults {
    results: Vec<HitResult>,
    generation: u64,
}

impl HitResults {
    /// Iterates accepted hits in stable hitbox/entity order.
    pub fn iter(&self) -> impl Iterator<Item = &HitResult> {
        self.results.iter()
    }

    /// Producer generation used by host event bridges to avoid duplicates.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn begin_step(&mut self) {
        self.results.clear();
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Deferred velocity change created by one accepted hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnockbackRequest {
    /// Character that receives the velocity impulse.
    pub target: Entity,
    /// World-space velocity added at the end of combat contact processing.
    pub velocity: Vec3,
}

/// Fixed-step queue of knockback requests.
#[derive(Debug, Default)]
pub struct KnockbackRequests {
    requests: Vec<KnockbackRequest>,
}

/// Converts current attack-trigger contacts into damage and knockback.
pub fn combat_contact_system(
    fixed_time: Res<FixedTime>,
    collisions: Res<CollisionEvents>,
    mut hitboxes: Query<&mut AttackHitbox>,
    mut receivers: Query<&mut DamageReceiver>,
    mut results: ResMut<HitResults>,
    mut knockback: ResMut<KnockbackRequests>,
) {
    results.begin_step();
    knockback.requests.clear();

    for (_, receiver) in receivers.iter_mut() {
        receiver.invulnerability_remaining =
            (receiver.invulnerability_remaining - fixed_time.fixed_delta).max(0.0);
    }

    let mut contacts = BTreeMap::<Entity, Vec<Entity>>::new();
    for collision in collisions.iter() {
        contacts
            .entry(collision.entity_a)
            .or_default()
            .push(collision.entity_b);
        contacts
            .entry(collision.entity_b)
            .or_default()
            .push(collision.entity_a);
    }
    for targets in contacts.values_mut() {
        targets.sort_unstable();
        targets.dedup();
    }

    for (hitbox_entity, hitbox) in hitboxes.iter_mut() {
        if !hitbox.enabled {
            continue;
        }
        let Some(targets) = contacts.get(&hitbox_entity) else {
            continue;
        };
        for target in targets {
            if *target == hitbox.owner
                || (hitbox.one_hit_per_target && hitbox.hit_entities.contains(target))
            {
                continue;
            }
            let Some((_, receiver)) = receivers.iter_mut().find(|(entity, _)| *entity == *target)
            else {
                continue;
            };
            if receiver.team == hitbox.team || receiver.invulnerability_remaining > 0.0 {
                continue;
            }

            receiver.health = (receiver.health - hitbox.damage).max(0.0);
            receiver.invulnerability_remaining = receiver.invulnerability_seconds.max(0.0);
            hitbox.hit_entities.insert(*target);
            results.results.push(HitResult {
                attacker: hitbox.owner,
                hitbox: hitbox_entity,
                target: *target,
                damage: hitbox.damage,
                remaining_health: receiver.health,
                activation: hitbox.activation,
            });
            if hitbox.knockback.length_squared() > 0.0 {
                knockback.requests.push(KnockbackRequest {
                    target: *target,
                    velocity: hitbox.knockback,
                });
            }
        }
    }
}

/// Applies queued hit impulses to kinematic character velocity.
pub fn apply_knockback_system(
    mut requests: ResMut<KnockbackRequests>,
    mut controllers: Query<&mut KinematicCharacterController>,
) {
    if requests.requests.is_empty() {
        return;
    }
    let mut combined = BTreeMap::<Entity, Vec3>::new();
    for request in requests.requests.drain(..) {
        *combined.entry(request.target).or_default() += request.velocity;
    }
    for (entity, controller) in controllers.iter_mut() {
        if let Some(velocity) = combined.get(&entity) {
            controller.velocity += *velocity;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::App;
    use engine_physics::collision::CollisionEvent;

    #[test]
    fn one_activation_damages_target_once_and_reactivation_allows_another_hit() {
        let mut app = App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(CollisionEvents::default());
        app.insert_resource(HitResults::default());
        app.insert_resource(KnockbackRequests::default());
        let owner = app.world_mut().spawn().expect("owner");
        let hitbox_entity = app
            .world_mut()
            .spawn_with(AttackHitbox::new(owner, 1, 25.0, true, true))
            .expect("hitbox");
        let target = app
            .world_mut()
            .spawn_with(DamageReceiver {
                team: 2,
                invulnerability_seconds: 0.0,
                ..DamageReceiver::default()
            })
            .expect("target");
        app.world_mut()
            .get_resource_mut::<CollisionEvents>()
            .expect("events")
            .replace_for_test(vec![CollisionEvent {
                entity_a: hitbox_entity,
                entity_b: target,
                push_out: Vec3::ZERO,
                is_trigger: true,
            }]);
        app.add_fixed_system(combat_contact_system);

        app.run_fixed_update().expect("first hit");
        app.run_fixed_update().expect("same activation stay");
        assert_eq!(
            app.world()
                .get_component::<DamageReceiver>(target)
                .expect("receiver")
                .health,
            75.0
        );

        app.world_mut()
            .get_component_mut::<AttackHitbox>(hitbox_entity)
            .expect("hitbox")
            .set_enabled(false);
        app.world_mut()
            .get_component_mut::<AttackHitbox>(hitbox_entity)
            .expect("hitbox")
            .set_enabled(true);
        app.run_fixed_update().expect("reactivated hit");
        assert_eq!(
            app.world()
                .get_component::<DamageReceiver>(target)
                .expect("receiver")
                .health,
            50.0
        );
    }
}
