//! Engine-owned Native 2D physics contracts and deterministic reference solver (ADR 0127).
//!
//! The 2D world is explicit and independent from the existing Rapier 3D world.

use glam::{Mat4, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 2D rigid-body simulation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RigidBodyMode2d {
    /// Body never moves under simulation.
    Fixed,
    /// Body is integrated by the dedicated 2D world.
    Dynamic,
    /// Body pose is driven explicitly by gameplay code.
    Kinematic,
}

/// 2D collision filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionFilter2d {
    /// Layer bits this collider belongs to.
    pub memberships: u32,
    /// Layer bits this collider may interact with.
    pub mask: u32,
}

impl Default for CollisionFilter2d {
    fn default() -> Self {
        Self {
            memberships: 1,
            mask: u32::MAX,
        }
    }
}

impl CollisionFilter2d {
    /// Returns whether both filters mutually permit contact.
    pub fn permits(self, other: Self) -> bool {
        self.memberships & other.mask != 0 && other.memberships & self.mask != 0
    }
}

/// Supported first-release collider geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColliderShape2d {
    /// Axis-aligned box in the body's local 2D plane.
    Box {
        /// Positive local X/Y half extents.
        half_extents: [f32; 2],
    },
    /// Circle centered on the body origin.
    Circle {
        /// Positive local radius.
        radius: f32,
    },
    /// Y-oriented capsule centered on the body origin.
    Capsule {
        /// Half length of the capsule's center segment.
        half_height: f32,
        /// Positive capsule radius.
        radius: f32,
    },
    /// Simple polygon in local XY coordinates.
    Polygon {
        /// Ordered finite polygon vertices.
        points: Vec<[f32; 2]>,
    },
}

/// Global gravitational acceleration for the dedicated Native 2D world.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gravity2d(pub Vec2);

impl Default for Gravity2d {
    fn default() -> Self {
        Self(Vec2::new(0.0, -9.81))
    }
}

/// Persistable rigid-body contract.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RigidBody2d {
    /// Simulation ownership mode.
    pub mode: RigidBodyMode2d,
    /// Linear velocity in world XY units per second.
    pub velocity: [f32; 2],
    /// Z-axis angular velocity in radians per second.
    pub angular_velocity: f32,
    /// Multiplier applied to project 2D gravity.
    pub gravity_scale: f32,
    /// Whether continuous-collision substepping is enabled.
    pub continuous: bool,
}

impl Default for RigidBody2d {
    fn default() -> Self {
        Self {
            mode: RigidBodyMode2d::Fixed,
            velocity: [0.0, 0.0],
            angular_velocity: 0.0,
            gravity_scale: 1.0,
            continuous: false,
        }
    }
}

/// Persistable collider contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collider2d {
    /// Backend-neutral collision geometry.
    pub shape: ColliderShape2d,
    /// Whether contacts are reported without solid blocking.
    pub sensor: bool,
    /// Tangential friction coefficient.
    pub friction: f32,
    /// Normal restitution coefficient.
    pub restitution: f32,
    /// Layer membership and interaction mask.
    pub filter: CollisionFilter2d,
    /// Whether the shared one-way platform policy applies.
    pub one_way: bool,
}

impl Default for Collider2d {
    fn default() -> Self {
        Self {
            shape: ColliderShape2d::Box {
                half_extents: [0.5, 0.5],
            },
            sensor: false,
            friction: 0.5,
            restitution: 0.0,
            filter: CollisionFilter2d::default(),
            one_way: false,
        }
    }
}

/// Backend-neutral joint contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Joint2d {
    /// Preserves the current relative pose to another body.
    Fixed {
        /// Stable runtime entity key of the connected body.
        other: u64,
    },
    /// Maintains a target center-to-center distance.
    Distance {
        /// Stable runtime entity key of the connected body.
        other: u64,
        /// Target distance in world units.
        distance: f32,
    },
    /// Keeps two local anchors coincident while allowing rotation.
    Revolute {
        /// Stable runtime entity key of the connected body.
        other: u64,
        /// Local XY anchor on this body.
        anchor: [f32; 2],
    },
    /// Constrains relative translation to one axis and bounded travel.
    Prismatic {
        /// Stable runtime entity key of the connected body.
        other: u64,
        /// Local normalized translation axis.
        axis: [f32; 2],
        /// Minimum and maximum allowed travel.
        limits: [f32; 2],
    },
}

/// Resolved planar pose used by the 2D world.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanarPose2d {
    /// World XY translation.
    pub translation: Vec2,
    /// World Z rotation in radians.
    pub rotation: f32,
    /// World XY scale.
    pub scale: Vec2,
}

/// Structured transform projection diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanarPoseError {
    /// Transform contains a non-finite component.
    NonFinite,
    /// Rotation tilts the local Z axis out of the gameplay plane.
    NonPlanarRotation,
    /// Scale cannot be represented by the 2D physics contract.
    UnsupportedScale,
}

/// Projects the normal Transform world matrix into XY plus Z rotation.
pub fn project_planar_transform(matrix: Mat4) -> Result<PlanarPose2d, PlanarPoseError> {
    if !matrix.is_finite() {
        return Err(PlanarPoseError::NonFinite);
    }
    let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
    if !scale.is_finite()
        || scale.x.abs() < f32::EPSILON
        || scale.y.abs() < f32::EPSILON
        || (scale.z.abs() - 1.0).abs() > 1.0e-3
    {
        return Err(PlanarPoseError::UnsupportedScale);
    }
    let z = rotation * Vec3::Z;
    if z.dot(Vec3::Z).abs() < 0.999 {
        return Err(PlanarPoseError::NonPlanarRotation);
    }
    let (_, _, angle) = rotation.to_euler(glam::EulerRot::XYZ);
    Ok(PlanarPose2d {
        translation: translation.truncate(),
        rotation: angle,
        scale: scale.truncate(),
    })
}

/// One entity in the dedicated 2D world.
#[derive(Debug, Clone)]
pub struct BodyEntry2d {
    /// Stable runtime entity key.
    pub entity: u64,
    /// Current resolved planar pose.
    pub pose: PlanarPose2d,
    /// Rigid-body simulation state.
    pub body: RigidBody2d,
    /// Collider used for contacts and queries.
    pub collider: Collider2d,
}

/// Contact/trigger transition phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactPhase2d {
    /// Pair became overlapping this fixed step.
    Enter,
    /// Pair remained overlapping from the prior fixed step.
    Stay,
    /// Pair stopped overlapping this fixed step.
    Exit,
}

/// Fixed-step 2D contact event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactEvent2d {
    /// Lower deterministic entity key.
    pub a: u64,
    /// Higher deterministic entity key.
    pub b: u64,
    /// Whether either participating collider is a sensor.
    pub sensor: bool,
    /// Transition phase for this pair.
    pub phase: ContactPhase2d,
}

/// Ray or shape query result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryHit2d {
    /// Stable runtime entity key that was hit.
    pub entity: u64,
    /// World XY hit point.
    pub point: Vec2,
    /// World XY outward surface normal.
    pub normal: Vec2,
    /// Distance from query origin to the hit.
    pub distance: f32,
}

#[derive(Debug, Clone)]
struct JointConstraint2d {
    joint: Joint2d,
    fixed_offset: Vec2,
}

/// Dedicated deterministic 2D world. No 3D solver state is accepted or consulted.
#[derive(Debug, Default)]
pub struct PhysicsWorld2d {
    bodies: BTreeMap<u64, BodyEntry2d>,
    joints: BTreeMap<u64, Vec<JointConstraint2d>>,
    previous: BTreeMap<(u64, u64), bool>,
}

impl PhysicsWorld2d {
    /// Inserts a new body or replaces the body with the same stable runtime key.
    pub fn upsert(&mut self, entry: BodyEntry2d) {
        self.bodies.insert(entry.entity, entry);
    }

    /// Removes a body and retained state involving it.
    pub fn remove(&mut self, entity: u64) {
        self.bodies.remove(&entity);
        self.joints.remove(&entity);
        self.joints.values_mut().for_each(|constraints| {
            constraints.retain(|constraint| joint_other(&constraint.joint) != entity);
        });
        self.previous
            .retain(|(a, b), _| *a != entity && *b != entity);
    }

    /// Removes every body whose stable key is absent from `active`.
    pub fn retain_entities(&mut self, active: &BTreeSet<u64>) {
        let stale: Vec<_> = self
            .bodies
            .keys()
            .copied()
            .filter(|entity| !active.contains(entity))
            .collect();
        for entity in stale {
            self.remove(entity);
        }
    }

    /// Returns the current body entry for a stable runtime key.
    pub fn body(&self, entity: u64) -> Option<&BodyEntry2d> {
        self.bodies.get(&entity)
    }

    /// Returns mutable access to one body entry.
    pub fn body_mut(&mut self, entity: u64) -> Option<&mut BodyEntry2d> {
        self.bodies.get_mut(&entity)
    }

    /// Replaces one body's joint declarations and captures required rest state.
    pub fn set_joints(&mut self, entity: u64, joints: Vec<Joint2d>) {
        let Some(origin) = self.bodies.get(&entity).map(|entry| entry.pose.translation) else {
            self.joints.remove(&entity);
            return;
        };
        let constraints = joints
            .into_iter()
            .filter_map(|joint| {
                let other = joint_other(&joint);
                self.bodies.get(&other).map(|entry| JointConstraint2d {
                    fixed_offset: origin - entry.pose.translation,
                    joint,
                })
            })
            .collect();
        self.joints.insert(entity, constraints);
    }

    /// Returns current contact partners for one body in deterministic key order.
    pub fn contacts_for(&self, entity: u64) -> Vec<u64> {
        self.previous
            .keys()
            .filter_map(|(a, b)| {
                if *a == entity {
                    Some(*b)
                } else if *b == entity {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Advances dynamics and emits stable enter/stay/exit transitions.
    pub fn step(&mut self, dt: f32, gravity: Vec2) -> Vec<ContactEvent2d> {
        if !dt.is_finite() || dt <= 0.0 || !gravity.is_finite() {
            return Vec::new();
        }
        let substeps = self.required_substeps(dt);
        let sub_dt = dt / substeps as f32;
        let mut contacts = BTreeMap::new();
        for _ in 0..substeps {
            self.integrate(sub_dt, gravity);
            self.solve_joints();
            self.solve_contacts(sub_dt, &mut contacts);
        }
        self.transition_events(contacts)
    }

    fn required_substeps(&self, dt: f32) -> u32 {
        let mut count = 1_u32;
        for entry in self.bodies.values() {
            if entry.body.mode != RigidBodyMode2d::Dynamic || !entry.body.continuous {
                continue;
            }
            let speed = Vec2::from(entry.body.velocity).length();
            let extent = collider_half_extents(entry).min_element().max(0.01);
            let desired = ((speed * dt) / extent).ceil() as u32;
            count = count.max(desired.clamp(1, 16));
        }
        count
    }

    fn integrate(&mut self, dt: f32, gravity: Vec2) {
        for entry in self.bodies.values_mut() {
            if entry.body.mode != RigidBodyMode2d::Dynamic {
                continue;
            }
            let mut velocity = Vec2::from(entry.body.velocity);
            velocity += gravity * entry.body.gravity_scale * dt;
            entry.pose.translation += velocity * dt;
            entry.pose.rotation += entry.body.angular_velocity * dt;
            entry.body.velocity = velocity.to_array();
        }
    }

    fn solve_joints(&mut self) {
        let constraints: Vec<_> = self
            .joints
            .iter()
            .flat_map(|(entity, joints)| {
                joints
                    .iter()
                    .cloned()
                    .map(|joint| (*entity, joint))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (entity, constraint) in constraints {
            let other = joint_other(&constraint.joint);
            let Some(a) = self.bodies.get(&entity).cloned() else {
                continue;
            };
            let Some(b) = self.bodies.get(&other).cloned() else {
                continue;
            };
            let delta = a.pose.translation - b.pose.translation;
            let correction = match &constraint.joint {
                Joint2d::Fixed { .. } => delta - constraint.fixed_offset,
                Joint2d::Distance { distance, .. } => {
                    let target = distance.max(0.0);
                    if delta.length_squared() <= f32::EPSILON {
                        Vec2::new(-target, 0.0)
                    } else {
                        delta.normalize() * (delta.length() - target)
                    }
                }
                Joint2d::Revolute { anchor, .. } => {
                    let local_anchor = rotate(Vec2::from(*anchor) * a.pose.scale, a.pose.rotation);
                    delta + local_anchor - constraint.fixed_offset
                }
                Joint2d::Prismatic { axis, limits, .. } => {
                    let axis = rotate(Vec2::from(*axis), a.pose.rotation).normalize_or_zero();
                    if axis == Vec2::ZERO {
                        continue;
                    }
                    let perpendicular = delta - axis * delta.dot(axis);
                    let travel = delta.dot(axis);
                    let min = limits[0].min(limits[1]);
                    let max = limits[0].max(limits[1]);
                    perpendicular + axis * (travel - travel.clamp(min, max))
                }
            };
            apply_pair_correction(&mut self.bodies, entity, other, correction);
        }
    }

    fn solve_contacts(&mut self, dt: f32, contacts: &mut BTreeMap<(u64, u64), bool>) {
        let ids: Vec<_> = self.bodies.keys().copied().collect();
        for (index, &a_id) in ids.iter().enumerate() {
            for &b_id in &ids[index + 1..] {
                let a = self.bodies[&a_id].clone();
                let b = self.bodies[&b_id].clone();
                if !a.collider.filter.permits(b.collider.filter) {
                    continue;
                }
                let Some((normal, penetration)) = contact_manifold(&a, &b) else {
                    continue;
                };
                let pair = (a_id.min(b_id), a_id.max(b_id));
                let sensor = a.collider.sensor || b.collider.sensor;
                contacts.insert(pair, sensor);
                if sensor || !should_resolve_contact(&a, &b, dt) {
                    continue;
                }
                resolve_solid_contact(&mut self.bodies, a_id, b_id, normal, penetration);
            }
        }
    }

    fn transition_events(
        &mut self,
        current: BTreeMap<(u64, u64), bool>,
    ) -> Vec<ContactEvent2d> {
        let mut events = Vec::new();
        for (&(a, b), &sensor) in &current {
            events.push(ContactEvent2d {
                a,
                b,
                sensor,
                phase: if self.previous.contains_key(&(a, b)) {
                    ContactPhase2d::Stay
                } else {
                    ContactPhase2d::Enter
                },
            });
        }
        for (&(a, b), &sensor) in &self.previous {
            if !current.contains_key(&(a, b)) {
                events.push(ContactEvent2d {
                    a,
                    b,
                    sensor,
                    phase: ContactPhase2d::Exit,
                });
            }
        }
        self.previous = current;
        events
    }

    /// Returns the nearest deterministic ray hit accepted by `mask`.
    pub fn ray_cast(
        &self,
        origin: Vec2,
        direction: Vec2,
        max_distance: f32,
        mask: u32,
    ) -> Option<QueryHit2d> {
        self.cast_box(origin, Vec2::ZERO, direction, max_distance, mask)
    }

    /// Sweeps an axis-aligned box and returns the nearest deterministic hit.
    pub fn cast_box(
        &self,
        origin: Vec2,
        half_extents: Vec2,
        direction: Vec2,
        max_distance: f32,
        mask: u32,
    ) -> Option<QueryHit2d> {
        let direction = direction.normalize_or_zero();
        if direction == Vec2::ZERO || max_distance < 0.0 || !max_distance.is_finite() {
            return None;
        }
        let mut hits = Vec::new();
        for entry in self.bodies.values() {
            if entry.collider.filter.memberships & mask == 0 {
                continue;
            }
            let (min, max) = bounds(entry);
            let expanded_min = min - half_extents.abs();
            let expanded_max = max + half_extents.abs();
            if let Some(distance) =
                ray_aabb(origin, direction, expanded_min, expanded_max, max_distance)
            {
                let point = origin + direction * distance;
                let center = (expanded_min + expanded_max) * 0.5;
                let local = point - center;
                let normal = if local.x.abs() > local.y.abs() {
                    Vec2::new(local.x.signum(), 0.0)
                } else {
                    Vec2::new(0.0, local.y.signum())
                };
                hits.push(QueryHit2d {
                    entity: entry.entity,
                    point,
                    normal,
                    distance,
                });
            }
        }
        hits.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then(left.entity.cmp(&right.entity))
        });
        hits.into_iter().next()
    }

    /// Returns every collider overlapping an axis-aligned query box.
    pub fn overlap_box(&self, center: Vec2, half_extents: Vec2, mask: u32) -> Vec<u64> {
        let query_min = center - half_extents.abs();
        let query_max = center + half_extents.abs();
        self.bodies
            .values()
            .filter(|entry| entry.collider.filter.memberships & mask != 0)
            .filter_map(|entry| {
                let (min, max) = bounds(entry);
                aabb_overlap(query_min, query_max, min, max).then_some(entry.entity)
            })
            .collect()
    }
}

fn joint_other(joint: &Joint2d) -> u64 {
    match joint {
        Joint2d::Fixed { other }
        | Joint2d::Distance { other, .. }
        | Joint2d::Revolute { other, .. }
        | Joint2d::Prismatic { other, .. } => *other,
    }
}

fn rotate(value: Vec2, angle: f32) -> Vec2 {
    let (sin, cos) = angle.sin_cos();
    Vec2::new(value.x * cos - value.y * sin, value.x * sin + value.y * cos)
}

fn collider_half_extents(entry: &BodyEntry2d) -> Vec2 {
    let local = match &entry.collider.shape {
        ColliderShape2d::Box { half_extents } => Vec2::from(*half_extents),
        ColliderShape2d::Circle { radius } => Vec2::splat(*radius),
        ColliderShape2d::Capsule {
            half_height,
            radius,
        } => Vec2::new(*radius, *half_height + *radius),
        ColliderShape2d::Polygon { points } => points
            .iter()
            .map(|point| Vec2::from(*point))
            .fold(Vec2::ZERO, |extent, point| extent.max(point.abs())),
    };
    let scaled = local * entry.pose.scale.abs();
    let (sin, cos) = entry.pose.rotation.sin_cos();
    Vec2::new(
        cos.abs() * scaled.x + sin.abs() * scaled.y,
        sin.abs() * scaled.x + cos.abs() * scaled.y,
    )
}

fn bounds(entry: &BodyEntry2d) -> (Vec2, Vec2) {
    let half = collider_half_extents(entry);
    (entry.pose.translation - half, entry.pose.translation + half)
}

fn aabb_overlap(a_min: Vec2, a_max: Vec2, b_min: Vec2, b_max: Vec2) -> bool {
    a_min.x <= b_max.x
        && a_max.x >= b_min.x
        && a_min.y <= b_max.y
        && a_max.y >= b_min.y
}

fn contact_manifold(a: &BodyEntry2d, b: &BodyEntry2d) -> Option<(Vec2, f32)> {
    let (a_min, a_max) = bounds(a);
    let (b_min, b_max) = bounds(b);
    if !aabb_overlap(a_min, a_max, b_min, b_max) {
        return None;
    }
    let overlap_x = (a_max.x.min(b_max.x) - a_min.x.max(b_min.x)).max(0.0);
    let overlap_y = (a_max.y.min(b_max.y) - a_min.y.max(b_min.y)).max(0.0);
    let delta = b.pose.translation - a.pose.translation;
    if overlap_x < overlap_y {
        let direction = if delta.x >= 0.0 { 1.0 } else { -1.0 };
        Some((Vec2::new(direction, 0.0), overlap_x))
    } else {
        let direction = if delta.y >= 0.0 { 1.0 } else { -1.0 };
        Some((Vec2::new(0.0, direction), overlap_y))
    }
}

fn should_resolve_contact(a: &BodyEntry2d, b: &BodyEntry2d, dt: f32) -> bool {
    if !a.collider.one_way && !b.collider.one_way {
        return true;
    }
    if a.collider.one_way && b.collider.one_way {
        return false;
    }
    let (platform, mover) = if a.collider.one_way { (a, b) } else { (b, a) };
    if mover.body.mode != RigidBodyMode2d::Dynamic {
        return false;
    }
    let velocity = Vec2::from(mover.body.velocity);
    if velocity.y > 0.0 {
        return false;
    }
    let (_, platform_max) = bounds(platform);
    let mover_half = collider_half_extents(mover);
    let previous_center = mover.pose.translation - velocity * dt;
    previous_center.y - mover_half.y >= platform_max.y - 1.0e-3
}

fn inverse_mass(entry: &BodyEntry2d) -> f32 {
    if entry.body.mode == RigidBodyMode2d::Dynamic {
        1.0
    } else {
        0.0
    }
}

fn apply_pair_correction(
    bodies: &mut BTreeMap<u64, BodyEntry2d>,
    a_id: u64,
    b_id: u64,
    correction: Vec2,
) {
    let Some(a) = bodies.get(&a_id).cloned() else {
        return;
    };
    let Some(b) = bodies.get(&b_id).cloned() else {
        return;
    };
    let a_inverse = inverse_mass(&a);
    let b_inverse = inverse_mass(&b);
    let total = a_inverse + b_inverse;
    if total <= f32::EPSILON {
        return;
    }
    if a_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&a_id) {
            entry.pose.translation -= correction * (a_inverse / total);
        }
    }
    if b_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&b_id) {
            entry.pose.translation += correction * (b_inverse / total);
        }
    }
}

fn resolve_solid_contact(
    bodies: &mut BTreeMap<u64, BodyEntry2d>,
    a_id: u64,
    b_id: u64,
    normal: Vec2,
    penetration: f32,
) {
    let Some(a) = bodies.get(&a_id).cloned() else {
        return;
    };
    let Some(b) = bodies.get(&b_id).cloned() else {
        return;
    };
    let a_inverse = inverse_mass(&a);
    let b_inverse = inverse_mass(&b);
    let inverse_sum = a_inverse + b_inverse;
    if inverse_sum <= f32::EPSILON {
        return;
    }

    let correction = normal * penetration.max(0.0);
    if a_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&a_id) {
            entry.pose.translation -= correction * (a_inverse / inverse_sum);
        }
    }
    if b_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&b_id) {
            entry.pose.translation += correction * (b_inverse / inverse_sum);
        }
    }

    let a_velocity = Vec2::from(a.body.velocity);
    let b_velocity = Vec2::from(b.body.velocity);
    let relative = b_velocity - a_velocity;
    let normal_speed = relative.dot(normal);
    if normal_speed >= 0.0 {
        return;
    }
    let restitution = a.collider.restitution.min(b.collider.restitution).clamp(0.0, 1.0);
    let normal_impulse = -(1.0 + restitution) * normal_speed / inverse_sum;
    let mut a_result = a_velocity - normal * normal_impulse * a_inverse;
    let mut b_result = b_velocity + normal * normal_impulse * b_inverse;

    let tangent = Vec2::new(-normal.y, normal.x);
    let tangent_speed = (b_result - a_result).dot(tangent);
    let friction = (a.collider.friction.max(0.0) * b.collider.friction.max(0.0)).sqrt();
    let tangent_impulse = (-tangent_speed / inverse_sum)
        .clamp(-normal_impulse * friction, normal_impulse * friction);
    a_result -= tangent * tangent_impulse * a_inverse;
    b_result += tangent * tangent_impulse * b_inverse;

    if a_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&a_id) {
            entry.body.velocity = a_result.to_array();
        }
    }
    if b_inverse > 0.0 {
        if let Some(entry) = bodies.get_mut(&b_id) {
            entry.body.velocity = b_result.to_array();
        }
    }
}

fn ray_aabb(
    origin: Vec2,
    direction: Vec2,
    min: Vec2,
    max: Vec2,
    max_distance: f32,
) -> Option<f32> {
    let mut enter = 0.0_f32;
    let mut exit = max_distance;
    for axis in 0..2 {
        let origin_axis = origin[axis];
        let direction_axis = direction[axis];
        if direction_axis.abs() <= 1.0e-8 {
            if origin_axis < min[axis] || origin_axis > max[axis] {
                return None;
            }
            continue;
        }
        let inverse = 1.0 / direction_axis;
        let mut near = (min[axis] - origin_axis) * inverse;
        let mut far = (max[axis] - origin_axis) * inverse;
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        enter = enter.max(near);
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    (exit >= 0.0 && enter <= max_distance).then_some(enter.max(0.0))
}

/// First-release kinematic platformer controller state and configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterController2d {
    /// Axis-aligned controller half extents.
    pub half_extents: Vec2,
    /// Collision margin removed from overlap tests.
    pub skin: f32,
    /// Maximum walkable surface angle in radians.
    pub slope_limit_radians: f32,
    /// Maximum downward distance used to retain ground contact.
    pub ground_snap: f32,
    /// Collision membership mask queried by this controller.
    pub collision_mask: u32,
    /// Remaining time during which one-way platforms are ignored.
    pub drop_through_seconds: f32,
    /// Whether the latest move ended on walkable ground.
    pub grounded: bool,
    /// Latest resolved ground normal.
    pub ground_normal: Vec2,
    /// Whether the latest move was blocked horizontally.
    pub hit_wall: bool,
    /// Whether the latest move was blocked upward.
    pub hit_ceiling: bool,
}

impl Default for CharacterController2d {
    fn default() -> Self {
        Self {
            half_extents: Vec2::new(0.45, 0.9),
            skin: 0.02,
            slope_limit_radians: std::f32::consts::FRAC_PI_4,
            ground_snap: 0.1,
            collision_mask: u32::MAX,
            drop_through_seconds: 0.0,
            grounded: false,
            ground_normal: Vec2::Y,
            hit_wall: false,
            hit_ceiling: false,
        }
    }
}

impl CharacterController2d {
    /// Moves deterministically against fixed/kinematic colliders.
    ///
    /// `ignore_entity` is the controller's own collider key when that collider
    /// is also present in `world`.
    pub fn move_fixed(
        &mut self,
        world: &PhysicsWorld2d,
        start: Vec2,
        delta: Vec2,
        dt: f32,
        ignore_entity: Option<u64>,
    ) -> Vec2 {
        self.drop_through_seconds = (self.drop_through_seconds - dt.max(0.0)).max(0.0);
        self.grounded = false;
        self.ground_normal = Vec2::Y;
        self.hit_wall = false;
        self.hit_ceiling = false;
        let half_extents = (self.half_extents - Vec2::splat(self.skin.max(0.0)))
            .max(Vec2::splat(0.001));
        let mut position = start;

        let target_x = position + Vec2::new(delta.x, 0.0);
        if controller_blockers(
            world,
            target_x,
            half_extents,
            self.collision_mask,
            ignore_entity,
            self.drop_through_seconds,
            start,
            delta,
        )
        .is_empty()
        {
            position.x = target_x.x;
        } else {
            self.hit_wall = true;
        }

        let target_y = position + Vec2::new(0.0, delta.y);
        let vertical_hits = controller_blockers(
            world,
            target_y,
            half_extents,
            self.collision_mask,
            ignore_entity,
            self.drop_through_seconds,
            start,
            delta,
        );
        if vertical_hits.is_empty() {
            position.y = target_y.y;
        } else if delta.y < 0.0 {
            self.grounded = true;
        } else if delta.y > 0.0 {
            self.hit_ceiling = true;
        }

        if !self.grounded && delta.y <= 0.0 && self.ground_snap > 0.0 {
            let snap = position - Vec2::new(0.0, self.ground_snap);
            if !controller_blockers(
                world,
                snap,
                half_extents,
                self.collision_mask,
                ignore_entity,
                self.drop_through_seconds,
                position,
                Vec2::new(0.0, -self.ground_snap),
            )
            .is_empty()
            {
                self.grounded = true;
            }
        }
        position
    }

    /// Temporarily ignores one-way platforms so the controller can drop through them.
    pub fn request_drop_through(&mut self, seconds: f32) {
        self.drop_through_seconds = seconds.max(0.0);
        self.grounded = false;
    }
}

fn controller_blockers(
    world: &PhysicsWorld2d,
    center: Vec2,
    half_extents: Vec2,
    mask: u32,
    ignore_entity: Option<u64>,
    drop_through_seconds: f32,
    start: Vec2,
    delta: Vec2,
) -> Vec<u64> {
    let query_min = center - half_extents;
    let query_max = center + half_extents;
    world
        .bodies
        .values()
        .filter(|entry| Some(entry.entity) != ignore_entity)
        .filter(|entry| !entry.collider.sensor)
        .filter(|entry| entry.collider.filter.memberships & mask != 0)
        .filter(|entry| {
            let (min, max) = bounds(entry);
            aabb_overlap(query_min, query_max, min, max)
        })
        .filter(|entry| {
            if !entry.collider.one_way {
                return true;
            }
            if drop_through_seconds > 0.0 || delta.y > 0.0 {
                return false;
            }
            let (_, platform_max) = bounds(entry);
            start.y - half_extents.y >= platform_max.y - 1.0e-3
        })
        .map(|entry| entry.entity)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_body(entity: u64, mode: RigidBodyMode2d, center: Vec2) -> BodyEntry2d {
        BodyEntry2d {
            entity,
            pose: PlanarPose2d {
                translation: center,
                rotation: 0.0,
                scale: Vec2::ONE,
            },
            body: RigidBody2d {
                mode,
                ..RigidBody2d::default()
            },
            collider: Collider2d::default(),
        }
    }

    #[test]
    fn non_planar_transform_is_rejected() {
        assert_eq!(
            project_planar_transform(Mat4::from_rotation_x(0.2)),
            Err(PlanarPoseError::NonPlanarRotation)
        );
    }

    #[test]
    fn dynamic_body_falls_without_touching_any_3d_state() {
        let mut world = PhysicsWorld2d::default();
        world.upsert(box_body(1, RigidBodyMode2d::Dynamic, Vec2::new(0.0, 5.0)));
        let before = world.body(1).unwrap().pose.translation.y;
        world.step(1.0 / 60.0, Gravity2d::default().0);
        assert!(world.body(1).unwrap().pose.translation.y < before);
    }

    #[test]
    fn solid_contact_separates_dynamic_body_from_fixed_floor() {
        let mut world = PhysicsWorld2d::default();
        world.upsert(box_body(1, RigidBodyMode2d::Fixed, Vec2::ZERO));
        let mut falling = box_body(2, RigidBodyMode2d::Dynamic, Vec2::new(0.0, 0.75));
        falling.body.velocity = [0.0, -1.0];
        world.upsert(falling);
        let events = world.step(1.0 / 60.0, Vec2::ZERO);
        assert!(events.iter().any(|event| event.phase == ContactPhase2d::Enter));
        assert!(world.body(2).unwrap().pose.translation.y >= 0.99);
    }

    #[test]
    fn sensor_exit_preserves_sensor_classification() {
        let mut world = PhysicsWorld2d::default();
        let mut sensor = box_body(1, RigidBodyMode2d::Fixed, Vec2::ZERO);
        sensor.collider.sensor = true;
        world.upsert(sensor);
        world.upsert(box_body(2, RigidBodyMode2d::Fixed, Vec2::ZERO));
        let enter = world.step(1.0 / 60.0, Vec2::ZERO);
        assert!(enter.iter().any(|event| event.sensor));
        world.body_mut(2).unwrap().pose.translation = Vec2::new(5.0, 0.0);
        let exit = world.step(1.0 / 60.0, Vec2::ZERO);
        assert!(exit.iter().any(|event| {
            event.phase == ContactPhase2d::Exit && event.sensor
        }));
    }

    #[test]
    fn ray_query_is_nearest_then_entity_deterministic() {
        let mut world = PhysicsWorld2d::default();
        world.upsert(box_body(2, RigidBodyMode2d::Fixed, Vec2::new(3.0, 0.0)));
        world.upsert(box_body(1, RigidBodyMode2d::Fixed, Vec2::new(3.0, 0.0)));
        let hit = world
            .ray_cast(Vec2::ZERO, Vec2::X, 10.0, u32::MAX)
            .expect("ray must hit");
        assert_eq!(hit.entity, 1);
    }

    #[test]
    fn drop_through_ignores_one_way_platform() {
        let mut world = PhysicsWorld2d::default();
        let mut platform = box_body(1, RigidBodyMode2d::Fixed, Vec2::ZERO);
        platform.collider.one_way = true;
        world.upsert(platform);
        let mut controller = CharacterController2d::default();
        controller.request_drop_through(0.2);
        let position = controller.move_fixed(
            &world,
            Vec2::new(0.0, 1.5),
            Vec2::new(0.0, -1.0),
            1.0 / 60.0,
            None,
        );
        assert!(position.y < 1.0);
    }
}
