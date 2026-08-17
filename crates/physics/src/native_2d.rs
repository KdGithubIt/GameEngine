//! Engine-owned native 2D physics contracts and deterministic reference world (ADR 0127).
//!
//! The 2D world is deliberately independent from the existing 3D solver.

use glam::{Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 2D rigid-body mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")] pub enum RigidBodyMode2d { Fixed, Dynamic, Kinematic }
/// 2D collision filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionFilter2d { pub memberships: u32, pub mask: u32 }
impl CollisionFilter2d { pub fn permits(self, other: Self) -> bool { self.memberships & other.mask != 0 && other.memberships & self.mask != 0 } }
/// Supported first-release collider geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag="kind", rename_all="snake_case")]
pub enum ColliderShape2d { Box { half_extents: [f32;2] }, Circle { radius:f32 }, Capsule { half_height:f32, radius:f32 }, Polygon { points: Vec<[f32;2]> } }
/// Persistable rigid-body contract.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RigidBody2d { pub mode: RigidBodyMode2d, pub velocity:[f32;2], pub angular_velocity:f32, pub gravity_scale:f32, pub continuous:bool }
/// Persistable collider contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collider2d { pub shape: ColliderShape2d, pub sensor:bool, pub friction:f32, pub restitution:f32, pub filter:CollisionFilter2d, pub one_way:bool }
/// Supported backend-neutral joint contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag="kind", rename_all="snake_case")] pub enum Joint2d { Fixed { other:u64 }, Distance { other:u64, distance:f32 }, Revolute { other:u64, anchor:[f32;2] }, Prismatic { other:u64, axis:[f32;2], limits:[f32;2] } }
/// Resolved planar pose used by the 2D world.
#[derive(Debug, Clone, Copy, PartialEq)] pub struct PlanarPose2d { pub translation:Vec2, pub rotation:f32, pub scale:Vec2 }
/// Structured transform projection diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum PlanarPoseError { NonFinite, NonPlanarRotation, UnsupportedScale }

/// Projects the normal Transform world matrix into XY + Z rotation when representable.
pub fn project_planar_transform(matrix: Mat4) -> Result<PlanarPose2d, PlanarPoseError> {
    if !matrix.is_finite() { return Err(PlanarPoseError::NonFinite); }
    let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
    if !scale.is_finite() || scale.x.abs() < f32::EPSILON || scale.y.abs() < f32::EPSILON || (scale.z.abs() - 1.0).abs() > 1.0e-3 { return Err(PlanarPoseError::UnsupportedScale); }
    let z = rotation * Vec3::Z;
    if z.dot(Vec3::Z).abs() < 0.999 { return Err(PlanarPoseError::NonPlanarRotation); }
    let (_, _, angle) = rotation.to_euler(glam::EulerRot::XYZ);
    Ok(PlanarPose2d { translation: translation.truncate(), rotation: angle, scale: scale.truncate() })
}

/// One entity in the dedicated 2D world.
#[derive(Debug, Clone)] pub struct BodyEntry2d { pub entity:u64, pub pose:PlanarPose2d, pub body:RigidBody2d, pub collider:Collider2d }
/// Contact/trigger transition phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ContactPhase2d { Enter, Stay, Exit }
/// Fixed-step 2D contact event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct ContactEvent2d { pub a:u64, pub b:u64, pub sensor:bool, pub phase:ContactPhase2d }
/// Ray/shape query result.
#[derive(Debug, Clone, Copy, PartialEq)] pub struct QueryHit2d { pub entity:u64, pub point:Vec2, pub normal:Vec2, pub distance:f32 }

/// Dedicated deterministic 2D world. No 3D solver state is accepted or consulted.
#[derive(Debug, Default)] pub struct PhysicsWorld2d { bodies:BTreeMap<u64, BodyEntry2d>, previous:BTreeSet<(u64,u64)> }
impl PhysicsWorld2d {
    pub fn upsert(&mut self, entry:BodyEntry2d) { self.bodies.insert(entry.entity, entry); }
    pub fn remove(&mut self, entity:u64) { self.bodies.remove(&entity); self.previous.retain(|(a,b)| *a != entity && *b != entity); }
    pub fn body(&self, entity:u64) -> Option<&BodyEntry2d> { self.bodies.get(&entity) }
    /// Advances dynamics and emits stable enter/stay/exit transitions.
    pub fn step(&mut self, dt:f32, gravity:Vec2) -> Vec<ContactEvent2d> {
        let dt = dt.max(0.0);
        for entry in self.bodies.values_mut() { if entry.body.mode == RigidBodyMode2d::Dynamic { let mut v=Vec2::from(entry.body.velocity); v += gravity * entry.body.gravity_scale * dt; entry.pose.translation += v*dt; entry.body.velocity=v.to_array(); entry.pose.rotation += entry.body.angular_velocity*dt; } }
        let ids:Vec<_>=self.bodies.keys().copied().collect(); let mut now=BTreeSet::new(); let mut events=Vec::new();
        for (i,&a) in ids.iter().enumerate() { for &b in &ids[i+1..] { let aa=&self.bodies[&a]; let bb=&self.bodies[&b]; if aa.collider.filter.permits(bb.collider.filter) && overlap(aa,bb) { let pair=(a.min(b),a.max(b)); now.insert(pair); events.push(ContactEvent2d {a:pair.0,b:pair.1,sensor:aa.collider.sensor||bb.collider.sensor,phase:if self.previous.contains(&pair){ContactPhase2d::Stay}else{ContactPhase2d::Enter}}); } } }
        for pair in self.previous.difference(&now) { events.push(ContactEvent2d {a:pair.0,b:pair.1,sensor:false,phase:ContactPhase2d::Exit}); } self.previous=now; events
    }
    /// Nearest deterministic ray cast.
    pub fn ray_cast(&self, origin:Vec2, direction:Vec2, max_distance:f32, mask:u32) -> Option<QueryHit2d> {
        let dir=direction.normalize_or_zero(); if dir==Vec2::ZERO{return None;} let mut hits=Vec::new();
        for entry in self.bodies.values() { if entry.collider.filter.memberships & mask == 0 {continue;} let (min,max)=bounds(entry); if let Some(t)=ray_aabb(origin,dir,min,max,max_distance) { let point=origin+dir*t; let center=(min+max)*0.5; let delta=point-center; let normal=if delta.x.abs()>delta.y.abs(){Vec2::new(delta.x.signum(),0.0)}else{Vec2::new(0.0,delta.y.signum())}; hits.push(QueryHit2d{entity:entry.entity,point,normal,distance:t}); } }
        hits.sort_by(|a,b| a.distance.total_cmp(&b.distance).then(a.entity.cmp(&b.entity))); hits.into_iter().next()
    }
    /// Overlap query using an axis-aligned box.
    pub fn overlap_box(&self, center:Vec2, half_extents:Vec2, mask:u32) -> Vec<u64> { let min=center-half_extents; let max=center+half_extents; self.bodies.values().filter(|e| e.collider.filter.memberships&mask!=0).filter_map(|e| {let (a,b)=bounds(e); (min.x<=b.x&&max.x>=a.x&&min.y<=b.y&&max.y>=a.y).then_some(e.entity)}).collect() }
}

fn bounds(e:&BodyEntry2d)->(Vec2,Vec2){ let half=match &e.collider.shape { ColliderShape2d::Box{half_extents}=>Vec2::from(*half), ColliderShape2d::Circle{radius}=>Vec2::splat(*radius), ColliderShape2d::Capsule{half_height,radius}=>Vec2::new(*radius,*half_height+*radius), ColliderShape2d::Polygon{points}=>points.iter().fold(Vec2::ZERO,|m,p|m.max(Vec2::from(*p).abs())) }; let half=half*e.pose.scale.abs(); (e.pose.translation-half,e.pose.translation+half) }
fn overlap(a:&BodyEntry2d,b:&BodyEntry2d)->bool{let(amin,amax)=bounds(a);let(bmin,bmax)=bounds(b);amin.x<=bmax.x&&amax.x>=bmin.x&&amin.y<=bmax.y&&amax.y>=bmin.y}
fn ray_aabb(o:Vec2,d:Vec2,min:Vec2,max:Vec2,max_d:f32)->Option<f32>{ let inv=Vec2::new(if d.x.abs()>1e-8{1.0/d.x}else{f32::INFINITY},if d.y.abs()>1e-8{1.0/d.y}else{f32::INFINITY}); let t1=(min-o)*inv;let t2=(max-o)*inv;let lo=t1.min(t2);let hi=t1.max(t2);let enter=lo.x.max(lo.y).max(0.0);let exit=hi.x.min(hi.y);(enter<=exit&&enter<=max_d).then_some(enter)}

/// First-release kinematic platformer controller state/configuration.
#[derive(Debug, Clone, Copy, PartialEq)] pub struct CharacterController2d { pub half_extents:Vec2, pub skin:f32, pub slope_limit_radians:f32, pub ground_snap:f32, pub drop_through_seconds:f32, pub grounded:bool, pub ground_normal:Vec2, pub hit_wall:bool, pub hit_ceiling:bool }
impl Default for CharacterController2d { fn default()->Self{Self{half_extents:Vec2::new(0.45,0.9),skin:0.02,slope_limit_radians:0.7853982,ground_snap:0.1,drop_through_seconds:0.0,grounded:false,ground_normal:Vec2::Y,hit_wall:false,hit_ceiling:false}} }
impl CharacterController2d {
    /// Moves deterministically against fixed/kinematic colliders and applies shared one-way policy.
    pub fn move_fixed(&mut self, world:&PhysicsWorld2d, start:Vec2, delta:Vec2, dt:f32, mask:u32)->Vec2 {
        self.drop_through_seconds=(self.drop_through_seconds-dt).max(0.0); self.grounded=false;self.hit_wall=false;self.hit_ceiling=false; let mut pos=start;
        let target_x=pos+Vec2::new(delta.x,0.0); if world.overlap_box(target_x,self.half_extents-self.skin,mask).is_empty(){pos.x=target_x.x}else{self.hit_wall=true}
        let target_y=pos+Vec2::new(0.0,delta.y); let hits=world.overlap_box(target_y,self.half_extents-self.skin,mask); let blocked=hits.iter().any(|id| { let e=&world.bodies[id]; !(e.collider.one_way && (self.drop_through_seconds>0.0 || delta.y>0.0 || start.y-self.half_extents.y < e.pose.translation.y)) });
        if !blocked {pos.y=target_y.y} else if delta.y<0.0 {self.grounded=true;self.ground_normal=Vec2::Y}else if delta.y>0.0{self.hit_ceiling=true} pos
    }
    pub fn request_drop_through(&mut self, seconds:f32){self.drop_through_seconds=seconds.max(0.0);self.grounded=false}
}

#[cfg(test)] mod tests{use super::*;#[test]fn non_planar_is_diagnostic(){let m=Mat4::from_rotation_x(0.2);assert_eq!(project_planar_transform(m),Err(PlanarPoseError::NonPlanarRotation));}#[test]fn worlds_are_explicit(){let mut w=PhysicsWorld2d::default();assert!(w.step(1.0/60.0,Vec2::new(0.0,-9.81)).is_empty());}}
