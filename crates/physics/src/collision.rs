//! Collision shapes, filters, events, and pure world-space queries.
//!
//! Gameplay filtering and render debug visualization are deliberately not
//! owned here. The final `engine` composition layer supplies those adapters.

use engine_ecs::Entity;
use engine_rig::transform::GlobalTransform;
use glam::Vec3;
use hashbrown::HashMap;
use rapier3d::parry::query::contact;
use rapier3d::prelude::{Pose, SharedShape};
use std::collections::BTreeMap;

/// A collision shape attached to an entity.
#[derive(Clone, Debug)]
pub enum Collider {
    /// An axis-aligned box.
    Aabb {
        /// Half the width, height, and depth.
        half_extents: Vec3,
    },
    /// A sphere.
    Sphere {
        /// Radius before global scale is applied.
        radius: f32,
    },
    /// A capsule whose core segment runs along local Y.
    CapsuleY {
        /// Half the core segment length, excluding caps.
        half_height: f32,
        /// Core/cap radius.
        radius: f32,
    },
}

impl Collider {
    /// Creates an AABB collider.
    pub fn aabb(half_extents: Vec3) -> Self {
        Self::Aabb { half_extents }
    }

    /// Creates a cube AABB with side length `half * 2`.
    pub fn aabb_cube(half: f32) -> Self {
        Self::aabb(Vec3::splat(half))
    }

    /// Creates a sphere collider.
    pub fn sphere(radius: f32) -> Self {
        Self::Sphere { radius }
    }

    /// Creates a Y-axis capsule collider.
    pub fn capsule_y(half_height: f32, radius: f32) -> Self {
        Self::CapsuleY {
            half_height,
            radius,
        }
    }

    /// Converts this primitive to its Rapier shape at `scale`.
    #[doc(hidden)]
    pub fn rapier_shape(&self, scale: Vec3) -> SharedShape {
        let scale = scale.abs();
        match self {
            Self::Aabb { half_extents } => {
                let half_extents = *half_extents * scale;
                SharedShape::cuboid(half_extents.x, half_extents.y, half_extents.z)
            }
            Self::Sphere { radius } => SharedShape::ball(radius * scale.max_element()),
            Self::CapsuleY {
                half_height,
                radius,
            } => SharedShape::capsule_y(
                half_height * scale.y,
                radius * scale.x.max(scale.z),
            ),
        }
    }

    /// Returns the world-space collision shape under `transform`.
    pub fn world_shape(&self, transform: &GlobalTransform) -> WorldShape {
        let (scale, _, translation) = transform.matrix().to_scale_rotation_translation();
        match self {
            Self::Aabb { half_extents } => WorldShape::Aabb(WorldAabb {
                center: translation,
                half_extents: *half_extents * scale,
            }),
            Self::Sphere { radius } => {
                let uniform_scale = scale.x.max(scale.y).max(scale.z);
                WorldShape::Sphere(WorldSphere {
                    center: translation,
                    radius: radius * uniform_scale,
                })
            }
            Self::CapsuleY {
                half_height,
                radius,
            } => {
                let scaled_half_height = half_height * scale.y;
                let scaled_radius = radius * scale.x.max(scale.z);
                WorldShape::CapsuleY(WorldCapsule {
                    segment_a: translation + Vec3::Y * scaled_half_height,
                    segment_b: translation - Vec3::Y * scaled_half_height,
                    radius: scaled_radius,
                })
            }
        }
    }

    /// Returns the enclosing world-space AABB.
    pub fn world_aabb(&self, transform: &GlobalTransform) -> WorldAabb {
        self.world_shape(transform).enclosing_aabb()
    }
}

/// A world-space collision primitive.
#[derive(Debug, Clone, Copy)]
pub enum WorldShape {
    /// AABB.
    Aabb(WorldAabb),
    /// Sphere.
    Sphere(WorldSphere),
    /// Y-axis capsule.
    CapsuleY(WorldCapsule),
}

impl WorldShape {
    /// Returns the smallest AABB containing this shape.
    pub fn enclosing_aabb(&self) -> WorldAabb {
        match self {
            Self::Aabb(aabb) => *aabb,
            Self::Sphere(sphere) => WorldAabb {
                center: sphere.center,
                half_extents: Vec3::splat(sphere.radius),
            },
            Self::CapsuleY(capsule) => {
                let radius = Vec3::splat(capsule.radius);
                let min = capsule.segment_a.min(capsule.segment_b) - radius;
                let max = capsule.segment_a.max(capsule.segment_b) + radius;
                WorldAabb {
                    center: (min + max) * 0.5,
                    half_extents: (max - min) * 0.5,
                }
            }
        }
    }
}

/// An axis-aligned bounding box in world space.
#[derive(Debug, Clone, Copy)]
pub struct WorldAabb {
    /// Center position.
    pub center: Vec3,
    /// Half extents.
    pub half_extents: Vec3,
}

impl WorldAabb {
    /// Returns the minimum push-out separating this AABB from `other`.
    pub fn overlaps(&self, other: &WorldAabb) -> Option<PushOut> {
        let dx =
            (self.half_extents.x + other.half_extents.x) - (self.center.x - other.center.x).abs();
        let dy =
            (self.half_extents.y + other.half_extents.y) - (self.center.y - other.center.y).abs();
        let dz =
            (self.half_extents.z + other.half_extents.z) - (self.center.z - other.center.z).abs();
        if dx > 0.0 && dy > 0.0 && dz > 0.0 {
            Some(PushOut::minimum_axis(
                dx,
                dy,
                dz,
                self.center - other.center,
            ))
        } else {
            None
        }
    }

    fn min(&self) -> Vec3 {
        self.center - self.half_extents
    }

    fn max(&self) -> Vec3 {
        self.center + self.half_extents
    }
}

/// A sphere in world space.
#[derive(Debug, Clone, Copy)]
pub struct WorldSphere {
    /// Center position.
    pub center: Vec3,
    /// Radius.
    pub radius: f32,
}

/// A Y-axis capsule in world space.
#[derive(Debug, Clone, Copy)]
pub struct WorldCapsule {
    /// First end of the core segment.
    pub segment_a: Vec3,
    /// Second end of the core segment.
    pub segment_b: Vec3,
    /// Radius.
    pub radius: f32,
}

/// Minimum translation needed to separate two overlapping shapes.
#[derive(Debug, Clone, Copy)]
pub struct PushOut {
    /// Translation moving shape/entity A out of B.
    pub vector: Vec3,
}

impl PushOut {
    fn minimum_axis(dx: f32, dy: f32, dz: f32, a_minus_b: Vec3) -> Self {
        let (depth, axis) = if dx <= dy && dx <= dz {
            (dx, Vec3::X)
        } else if dy <= dz {
            (dy, Vec3::Y)
        } else {
            (dz, Vec3::Z)
        };
        let sign = if a_minus_b.dot(axis) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        Self {
            vector: axis * depth * sign,
        }
    }
}

/// Tests any pair of supported world shapes.
pub fn world_shapes_overlap(a: &WorldShape, b: &WorldShape) -> Option<PushOut> {
    let (pose_a, shape_a) = rapier_world_shape(a);
    let (pose_b, shape_b) = rapier_world_shape(b);
    let contact = contact(&pose_a, shape_a.as_ref(), &pose_b, shape_b.as_ref(), 0.0)
        .ok()
        .flatten()?;
    (contact.dist < 0.0).then(|| PushOut {
        vector: Vec3::new(contact.normal2.x, contact.normal2.y, contact.normal2.z)
            * -contact.dist,
    })
}

/// Converts a public world shape into Rapier pose/shape data.
#[doc(hidden)]
pub fn rapier_world_shape(shape: &WorldShape) -> (Pose, SharedShape) {
    match shape {
        WorldShape::Aabb(aabb) => (
            Pose::translation(aabb.center.x, aabb.center.y, aabb.center.z),
            SharedShape::cuboid(
                aabb.half_extents.x,
                aabb.half_extents.y,
                aabb.half_extents.z,
            ),
        ),
        WorldShape::Sphere(sphere) => (
            Pose::translation(sphere.center.x, sphere.center.y, sphere.center.z),
            SharedShape::ball(sphere.radius),
        ),
        WorldShape::CapsuleY(capsule) => {
            let center = (capsule.segment_a + capsule.segment_b) * 0.5;
            let half_height = capsule.segment_a.distance(capsule.segment_b) * 0.5;
            (
                Pose::translation(center.x, center.y, center.z),
                SharedShape::capsule_y(half_height, capsule.radius),
            )
        }
    }
}

/// Collision layer membership and mask bitmasks.
#[derive(Debug, Clone, PartialEq)]
pub struct CollisionLayers {
    /// Layers this collider belongs to.
    pub membership: u32,
    /// Layers this collider tests against.
    pub mask: u32,
}

impl Default for CollisionLayers {
    fn default() -> Self {
        Self {
            membership: 1,
            mask: u32::MAX,
        }
    }
}

/// Returns whether two collision-layer filters permit testing.
pub fn should_collide(a: &CollisionLayers, b: &CollisionLayers) -> bool {
    (a.membership & b.mask) != 0 && (b.membership & a.mask) != 0
}

/// Marker for overlaps that report events without push-out.
#[derive(Debug, Clone, Copy)]
pub struct TriggerVolume;

/// Whether a physics body participates in push-out resolution.
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicsBody {
    /// Never moved by collision correction.
    Static,
    /// Moved by collision correction but not gravity integration.
    Kinematic,
    /// Driven by velocity/gravity integration.
    Dynamic,
}

/// One overlap detected during a fixed step.
#[derive(Debug, Clone)]
pub struct CollisionEvent {
    /// First entity.
    pub entity_a: Entity,
    /// Second entity.
    pub entity_b: Entity,
    /// Vector moving A out of B.
    pub push_out: Vec3,
    /// Whether either side is a trigger.
    pub is_trigger: bool,
}

/// Collision pair lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionPhase {
    /// Newly overlapping.
    Enter,
    /// Still overlapping.
    Stay,
    /// No longer overlapping.
    Exit,
}

/// One collision event with lifecycle phase.
#[derive(Debug, Clone)]
pub struct CollisionTransition {
    /// Pair lifecycle phase.
    pub phase: CollisionPhase,
    /// Contact geometry.
    pub contact: CollisionEvent,
}

/// Broad-/narrow-phase counters from the latest detection pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CollisionStats {
    /// Number of collider proxies.
    pub proxy_count: usize,
    /// Broad-phase candidate pairs.
    pub candidate_pair_count: usize,
    /// Pairs sent to exact testing.
    pub narrow_phase_count: usize,
    /// Overlapping pairs.
    pub contact_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CollisionPair(Entity, Entity);

impl CollisionPair {
    fn new(entity_a: Entity, entity_b: Entity) -> (Self, bool) {
        if entity_a <= entity_b {
            (Self(entity_a, entity_b), false)
        } else {
            (Self(entity_b, entity_a), true)
        }
    }
}

/// Resource containing collision events from the latest fixed step.
#[derive(Default)]
pub struct CollisionEvents {
    events: Vec<CollisionEvent>,
    transitions: Vec<CollisionTransition>,
    previous_pairs: BTreeMap<CollisionPair, CollisionEvent>,
    previous_shapes: BTreeMap<Entity, WorldShape>,
    generation: u64,
}

impl CollisionEvents {
    /// Iterates current overlap events.
    pub fn iter(&self) -> impl Iterator<Item = &CollisionEvent> {
        self.events.iter()
    }

    /// Iterates enter/stay/exit transitions.
    pub fn transitions(&self) -> impl Iterator<Item = &CollisionTransition> {
        self.transitions.iter()
    }

    /// Returns the producer generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Starts a new collision detection pass.
    #[doc(hidden)]
    pub fn begin_detection(&mut self) {
        self.events.clear();
        self.transitions.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Adds one current-step event.
    #[doc(hidden)]
    pub fn push_event(&mut self, event: CollisionEvent) {
        self.events.push(event);
    }

    /// Returns the previous fixed-step shape for an entity.
    #[doc(hidden)]
    pub fn previous_shape(&self, entity: Entity) -> Option<WorldShape> {
        self.previous_shapes.get(&entity).copied()
    }

    /// Completes transitions and stores the shape snapshot for the next step.
    #[doc(hidden)]
    pub fn finish_detection(
        &mut self,
        shapes: impl IntoIterator<Item = (Entity, WorldShape)>,
    ) {
        let mut current_pairs = BTreeMap::new();
        for event in &self.events {
            let (pair, reversed) = CollisionPair::new(event.entity_a, event.entity_b);
            let canonical = if reversed {
                CollisionEvent {
                    entity_a: event.entity_b,
                    entity_b: event.entity_a,
                    push_out: -event.push_out,
                    is_trigger: event.is_trigger,
                }
            } else {
                event.clone()
            };
            let phase = if self.previous_pairs.contains_key(&pair) {
                CollisionPhase::Stay
            } else {
                CollisionPhase::Enter
            };
            self.transitions.push(CollisionTransition {
                phase,
                contact: canonical.clone(),
            });
            current_pairs.insert(pair, canonical);
        }
        for (pair, previous) in &self.previous_pairs {
            if !current_pairs.contains_key(pair) {
                self.transitions.push(CollisionTransition {
                    phase: CollisionPhase::Exit,
                    contact: previous.clone(),
                });
            }
        }
        self.previous_pairs = current_pairs;
        self.previous_shapes.clear();
        self.previous_shapes.extend(shapes);
    }

    /// Replaces events in tests while preserving transition behavior.
    #[doc(hidden)]
    pub fn replace_for_test(&mut self, events: Vec<CollisionEvent>) {
        self.begin_detection();
        self.events = events;
        self.finish_detection(std::iter::empty());
    }
}

/// One entity's view of one collision event.
#[derive(Debug, Clone, Copy)]
pub struct CollisionInfo {
    /// Other entity.
    pub other: Entity,
    /// Push vector from this entity's perspective.
    pub push: Vec3,
    /// Whether either side is a trigger.
    pub is_trigger: bool,
}

/// Builds per-entity collision snapshots.
pub fn collisions_by_entity(events: &CollisionEvents) -> HashMap<Entity, Vec<CollisionInfo>> {
    let mut map = HashMap::new();
    for event in events.iter() {
        map.entry(event.entity_a)
            .or_insert_with(Vec::new)
            .push(CollisionInfo {
                other: event.entity_b,
                push: event.push_out,
                is_trigger: event.is_trigger,
            });
        map.entry(event.entity_b)
            .or_insert_with(Vec::new)
            .push(CollisionInfo {
                other: event.entity_a,
                push: -event.push_out,
                is_trigger: event.is_trigger,
            });
    }
    map
}

/// Collects enclosing AABBs for static, non-trigger colliders.
pub fn static_obstacle_aabbs<'a>(
    colliders: impl Iterator<
        Item = (
            &'a Collider,
            &'a PhysicsBody,
            &'a GlobalTransform,
            Option<&'a TriggerVolume>,
        ),
    >,
) -> Vec<WorldAabb> {
    colliders
        .filter(|(_, body, _, trigger)| **body == PhysicsBody::Static && trigger.is_none())
        .map(|(collider, _, transform, _)| collider.world_aabb(transform))
        .collect()
}

/// Returns the earliest segment parameter entering any static obstacle AABB.
pub fn segment_blocked_by_static(aabbs: &[WorldAabb], from: Vec3, to: Vec3) -> Option<f32> {
    let mut closest = None;
    for aabb in aabbs {
        if let Some(t) = segment_vs_aabb(from, to, aabb) {
            closest = Some(closest.map_or(t, |current: f32| current.min(t)));
        }
    }
    closest
}

/// Returns whether swept enclosing AABBs could overlap between two samples.
#[doc(hidden)]
pub fn swept_shapes_overlap(
    previous_a: WorldShape,
    current_a: WorldShape,
    previous_b: WorldShape,
    current_b: WorldShape,
) -> bool {
    let previous_aabb_a = previous_a.enclosing_aabb();
    let current_aabb_a = current_a.enclosing_aabb();
    let previous_aabb_b = previous_b.enclosing_aabb();
    let current_aabb_b = current_b.enclosing_aabb();
    let relative_from = previous_aabb_a.center - previous_aabb_b.center;
    let relative_to = current_aabb_a.center - current_aabb_b.center;
    let combined_half_extents = previous_aabb_a
        .half_extents
        .max(current_aabb_a.half_extents)
        + previous_aabb_b
            .half_extents
            .max(current_aabb_b.half_extents);
    segment_vs_aabb(
        relative_from,
        relative_to,
        &WorldAabb {
            center: Vec3::ZERO,
            half_extents: combined_half_extents,
        },
    )
    .is_some()
}

fn slab_intersect(
    origin: f32,
    delta: f32,
    lo: f32,
    hi: f32,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, f32)> {
    if delta.abs() < f32::EPSILON {
        return (origin >= lo && origin <= hi).then_some((t_min, t_max));
    }
    let inv_delta = 1.0 / delta;
    let raw_a = (lo - origin) * inv_delta;
    let raw_b = (hi - origin) * inv_delta;
    let (near, far) = if raw_a <= raw_b {
        (raw_a, raw_b)
    } else {
        (raw_b, raw_a)
    };
    let new_min = t_min.max(near);
    let new_max = t_max.min(far);
    (new_min <= new_max).then_some((new_min, new_max))
}

fn segment_vs_aabb(from: Vec3, to: Vec3, aabb: &WorldAabb) -> Option<f32> {
    let direction = to - from;
    if direction.length_squared() <= f32::EPSILON {
        return None;
    }
    let min = aabb.min();
    let max = aabb.max();
    let (t_min, t_max) = slab_intersect(from.x, direction.x, min.x, max.x, 0.0, 1.0)?;
    let (t_min, t_max) = slab_intersect(from.y, direction.y, min.y, max.y, t_min, t_max)?;
    let (t_min, _) = slab_intersect(from.z, direction.z, min.z, max.z, t_min, t_max)?;
    Some(t_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_require_both_masks_to_match() {
        let a = CollisionLayers {
            membership: 0b01,
            mask: 0b10,
        };
        let b = CollisionLayers {
            membership: 0b10,
            mask: 0b01,
        };
        assert!(should_collide(&a, &b));
        let blocked = CollisionLayers {
            membership: 0b10,
            mask: 0,
        };
        assert!(!should_collide(&a, &blocked));
    }

    #[test]
    fn segment_query_reports_nearest_hit() {
        let obstacles = [
            WorldAabb {
                center: Vec3::new(5.0, 0.0, 0.0),
                half_extents: Vec3::ONE,
            },
            WorldAabb {
                center: Vec3::new(2.0, 0.0, 0.0),
                half_extents: Vec3::splat(0.5),
            },
        ];
        let hit = segment_blocked_by_static(&obstacles, Vec3::ZERO, Vec3::X * 10.0)
            .expect("segment must hit");
        assert!((hit - 0.15).abs() < 1.0e-4);
    }

    #[test]
    fn event_snapshots_are_bidirectional() {
        let mut events = CollisionEvents::default();
        events.replace_for_test(vec![CollisionEvent {
            entity_a: Entity::from_raw(1, 0),
            entity_b: Entity::from_raw(2, 0),
            push_out: Vec3::X,
            is_trigger: false,
        }]);
        let by_entity = collisions_by_entity(&events);
        assert_eq!(by_entity[&Entity::from_raw(1, 0)][0].push, Vec3::X);
        assert_eq!(by_entity[&Entity::from_raw(2, 0)][0].push, -Vec3::X);
    }
}
