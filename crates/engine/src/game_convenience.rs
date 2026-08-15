//! High-level convenience APIs for common project gameplay operations.
//!
//! This module keeps frequently repeated query searches, collision handling,
//! transform math, typed asset references, request IDs, hitbox construction,
//! and physics-world queries behind intent-level methods.

use crate::advanced_geometry::{
    raycast_plane, raycast_shape, screen_ray, Plane3d, Ray3d, RayHit, ScreenViewport,
    SpatialQueryError,
};
use crate::camera::{camera_selection_key, Camera3D};
use crate::collision::{world_shapes_overlap, WorldAabb, WorldCapsule, WorldShape, WorldSphere};
use crate::game_api::Res;
use crate::game_io::GameEntityHandle;
use crate::game_module::{GameResource, GameResourceSchema};
use crate::transform::GlobalTransform;
use engine_authoring::Value;
use glam::{Mat4, Vec2, Vec3};
use std::collections::BTreeMap;

/// Stable game-resource ID used for the live physics snapshot.
pub const PHYSICS_WORLD_RESOURCE_ID: &str = "engine.physics_world";

pub use engine_scripting::game_convenience::*;

/// Physics body classification copied into the project query snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsBodyKind {
    /// Body never displaced by collision resolution.
    Static,
    /// Body controlled by game-authored kinematic motion.
    Kinematic,
    /// Body integrated by velocity and gravity.
    Dynamic,
}

/// One collider copied from the live runtime world.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsCollider {
    /// Generation-checked runtime entity handle.
    pub entity: GameEntityHandle,
    /// World-space collision shape.
    pub shape: WorldShape,
    /// Collision-layer membership.
    pub membership: u32,
    /// Collision-layer mask.
    pub mask: u32,
    /// Whether this collider is a trigger volume.
    pub is_trigger: bool,
    /// Runtime body classification.
    pub body: PhysicsBodyKind,
}

/// One camera copied from the live runtime world.
#[derive(Debug, Clone)]
pub struct PhysicsCamera {
    /// Generation-checked runtime entity handle.
    pub entity: GameEntityHandle,
    /// Perspective camera values.
    pub camera: Camera3D,
    /// Camera world transform.
    pub world: GlobalTransform,
}

/// Filter applied to ray and overlap queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicsQueryFilter {
    /// Query-layer membership used against collider masks.
    pub membership: u32,
    /// Query-layer mask used against collider membership.
    pub mask: u32,
    /// Whether trigger volumes may be returned.
    pub include_triggers: bool,
    /// Entities ignored by the query.
    pub excluded_entities: Vec<GameEntityHandle>,
    /// Optional body-class restriction.
    pub body: Option<PhysicsBodyKind>,
}

impl Default for PhysicsQueryFilter {
    fn default() -> Self {
        Self {
            membership: 1,
            mask: u32::MAX,
            include_triggers: false,
            excluded_entities: Vec::new(),
            body: None,
        }
    }
}

impl PhysicsQueryFilter {
    /// Includes trigger volumes in results.
    pub fn including_triggers(mut self) -> Self {
        self.include_triggers = true;
        self
    }

    /// Uses explicit query-layer membership and mask.
    pub fn layers(mut self, membership: u32, mask: u32) -> Self {
        self.membership = membership;
        self.mask = mask;
        self
    }

    /// Excludes one entity from results.
    pub fn excluding(mut self, entity: GameEntityHandle) -> Self {
        if !self.excluded_entities.contains(&entity) {
            self.excluded_entities.push(entity);
        }
        self
    }

    /// Restricts results to one physics body class.
    pub fn body(mut self, body: PhysicsBodyKind) -> Self {
        self.body = Some(body);
        self
    }

    fn accepts(&self, collider: &PhysicsCollider) -> bool {
        if !self.include_triggers && collider.is_trigger {
            return false;
        }
        if self.excluded_entities.contains(&collider.entity) {
            return false;
        }
        if self.body.is_some_and(|body| body != collider.body) {
            return false;
        }
        (self.mask & collider.membership) != 0 && (collider.mask & self.membership) != 0
    }
}

/// Closest collider hit returned by a live physics-world query.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsRayHit {
    /// Hit collider entity.
    pub entity: GameEntityHandle,
    /// Geometric hit information.
    pub hit: RayHit,
    /// Whether the collider is a trigger.
    pub is_trigger: bool,
    /// Body classification of the collider.
    pub body: PhysicsBodyKind,
}

/// Collider returned by an overlap query.
#[derive(Debug, Clone, Copy)]
pub struct PhysicsOverlap {
    /// Overlapping collider entity.
    pub entity: GameEntityHandle,
    /// Minimum push-out moving the query shape away from this collider.
    pub push_out: Vec3,
    /// Whether the collider is a trigger.
    pub is_trigger: bool,
    /// Body classification of the collider.
    pub body: PhysicsBodyKind,
}

/// Read-only snapshot used by synchronous project-side physics queries.
#[derive(Debug, Clone)]
pub struct PhysicsWorldView {
    colliders: Vec<PhysicsCollider>,
    cameras: Vec<PhysicsCamera>,
    viewport: ScreenViewport,
    mouse_position: Vec2,
}

impl Default for PhysicsWorldView {
    fn default() -> Self {
        Self {
            colliders: Vec::new(),
            cameras: Vec::new(),
            viewport: ScreenViewport::from_size(Vec2::ONE).expect("unit viewport is valid"),
            mouse_position: Vec2::ZERO,
        }
    }
}

/// System parameter that exposes synchronous live-world physics queries.
pub type PhysicsQuery = Res<PhysicsWorldView>;

impl PhysicsWorldView {
    /// Returns all copied colliders in deterministic entity order.
    pub fn colliders(&self) -> &[PhysicsCollider] {
        &self.colliders
    }

    /// Returns all copied cameras in deterministic entity order.
    pub fn cameras(&self) -> &[PhysicsCamera] {
        &self.cameras
    }

    /// Returns the active copied camera using the engine-wide selection rule.
    pub fn primary_camera(&self) -> Option<&PhysicsCamera> {
        self.cameras
            .iter()
            .filter_map(|camera| {
                camera_selection_key(
                    camera.entity.id,
                    camera.entity.generation,
                    &camera.camera,
                )
                .map(|key| (key, camera))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, camera)| camera)
    }

    /// Returns the current viewport rectangle in physical pixels.
    pub fn viewport(&self) -> ScreenViewport {
        self.viewport
    }

    /// Returns the current mouse position in physical pixels.
    pub fn mouse_position(&self) -> Vec2 {
        self.mouse_position
    }

    /// Returns the closest collider hit by a validated ray.
    pub fn raycast(&self, ray: Ray3d, filter: &PhysicsQueryFilter) -> Option<PhysicsRayHit> {
        self.raycast_all(ray, filter).into_iter().next()
    }

    /// Builds a ray from origin, direction, and length and returns its closest hit.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when the ray values are invalid.
    pub fn raycast_from(
        &self,
        origin: Vec3,
        direction: Vec3,
        maximum_distance: f32,
        filter: &PhysicsQueryFilter,
    ) -> Result<Option<PhysicsRayHit>, SpatialQueryError> {
        Ok(self.raycast(Ray3d::new(origin, direction, maximum_distance)?, filter))
    }

    /// Returns every collider hit by a ray, sorted nearest first.
    pub fn raycast_all(&self, ray: Ray3d, filter: &PhysicsQueryFilter) -> Vec<PhysicsRayHit> {
        let mut hits = self
            .colliders
            .iter()
            .filter(|collider| filter.accepts(collider))
            .filter_map(|collider| {
                raycast_shape(ray, collider.shape).map(|hit| PhysicsRayHit {
                    entity: collider.entity,
                    hit,
                    is_trigger: collider.is_trigger,
                    body: collider.body,
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| left.hit.distance.total_cmp(&right.hit.distance));
        hits
    }

    /// Casts from one screen pixel using the primary camera.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when camera or screen values are invalid.
    pub fn screen_raycast(
        &self,
        screen_position: Vec2,
        maximum_distance: f32,
        filter: &PhysicsQueryFilter,
    ) -> Result<Option<PhysicsRayHit>, SpatialQueryError> {
        let Some(camera) = self.primary_camera() else {
            return Ok(None);
        };
        self.screen_raycast_from(camera.entity, screen_position, maximum_distance, filter)
    }

    /// Casts from one screen pixel using an explicit camera entity.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when camera or screen values are invalid.
    pub fn screen_raycast_from(
        &self,
        camera_entity: GameEntityHandle,
        screen_position: Vec2,
        maximum_distance: f32,
        filter: &PhysicsQueryFilter,
    ) -> Result<Option<PhysicsRayHit>, SpatialQueryError> {
        let Some(camera) = self
            .cameras
            .iter()
            .find(|camera| camera.entity == camera_entity)
        else {
            return Ok(None);
        };
        let ray = screen_ray(
            &camera.camera,
            &camera.world,
            self.viewport,
            screen_position,
            maximum_distance,
        )?;
        Ok(self.raycast(ray, filter))
    }

    /// Casts from the current mouse position using the primary camera.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when camera or mouse values are invalid.
    pub fn mouse_raycast(
        &self,
        maximum_distance: f32,
        filter: &PhysicsQueryFilter,
    ) -> Result<Option<PhysicsRayHit>, SpatialQueryError> {
        self.screen_raycast(self.mouse_position, maximum_distance, filter)
    }

    /// Converts one screen pixel to a world-space plane hit using the primary camera.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when camera, screen, or plane values are invalid.
    pub fn screen_point_on_plane(
        &self,
        screen_position: Vec2,
        plane: Plane3d,
        maximum_distance: f32,
    ) -> Result<Option<RayHit>, SpatialQueryError> {
        let Some(camera) = self.primary_camera() else {
            return Ok(None);
        };
        let ray = screen_ray(
            &camera.camera,
            &camera.world,
            self.viewport,
            screen_position,
            maximum_distance,
        )?;
        Ok(raycast_plane(ray, plane))
    }

    /// Converts the current mouse position to a world-space plane hit.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when camera, mouse, or plane values are invalid.
    pub fn mouse_point_on_plane(
        &self,
        plane: Plane3d,
        maximum_distance: f32,
    ) -> Result<Option<RayHit>, SpatialQueryError> {
        self.screen_point_on_plane(self.mouse_position, plane, maximum_distance)
    }

    /// Returns every collider overlapping a world-space sphere.
    pub fn overlap_sphere(
        &self,
        center: Vec3,
        radius: f32,
        filter: &PhysicsQueryFilter,
    ) -> Vec<PhysicsOverlap> {
        self.overlap_shape(WorldShape::Sphere(WorldSphere { center, radius }), filter)
    }

    /// Returns every collider overlapping an axis-aligned world-space box.
    pub fn overlap_box(
        &self,
        center: Vec3,
        half_extents: Vec3,
        filter: &PhysicsQueryFilter,
    ) -> Vec<PhysicsOverlap> {
        self.overlap_shape(
            WorldShape::Aabb(WorldAabb {
                center,
                half_extents,
            }),
            filter,
        )
    }

    /// Returns every collider overlapping a world-space capsule.
    pub fn overlap_capsule(
        &self,
        segment_a: Vec3,
        segment_b: Vec3,
        radius: f32,
        filter: &PhysicsQueryFilter,
    ) -> Vec<PhysicsOverlap> {
        self.overlap_shape(
            WorldShape::CapsuleY(WorldCapsule {
                segment_a,
                segment_b,
                radius,
            }),
            filter,
        )
    }

    /// Returns every collider overlapping an arbitrary built-in world shape.
    pub fn overlap_shape(
        &self,
        shape: WorldShape,
        filter: &PhysicsQueryFilter,
    ) -> Vec<PhysicsOverlap> {
        self.colliders
            .iter()
            .filter(|collider| filter.accepts(collider))
            .filter_map(|collider| {
                world_shapes_overlap(&shape, &collider.shape).map(|push| PhysicsOverlap {
                    entity: collider.entity,
                    push_out: push.vector,
                    is_trigger: collider.is_trigger,
                    body: collider.body,
                })
            })
            .collect()
    }
}

impl GameResource for PhysicsWorldView {
    const RESOURCE_ID: &'static str = PHYSICS_WORLD_RESOURCE_ID;
    const DISPLAY_NAME: &'static str = "Physics World";

    fn schema() -> GameResourceSchema {
        GameResourceSchema {
            id: Self::RESOURCE_ID.to_owned(),
            display_name: Self::DISPLAY_NAME.to_owned(),
            description: "Engine-owned read-only collider, camera, viewport, and mouse snapshot."
                .to_owned(),
            version: 1,
            fields: Vec::new(),
            default_value: Self::default().to_value(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        decode_physics_world(value)
    }

    fn to_value(&self) -> Value {
        Value::Object(BTreeMap::from([
            ("cameras".to_owned(), Value::Array(Vec::new())),
            ("colliders".to_owned(), Value::Array(Vec::new())),
            (
                "mouse_position".to_owned(),
                Value::Array(vec![Value::F64(0.0), Value::F64(0.0)]),
            ),
            (
                "viewport".to_owned(),
                Value::Array(vec![Value::F64(1.0), Value::F64(1.0)]),
            ),
        ]))
    }
}

fn decode_physics_world(value: &Value) -> Result<PhysicsWorldView, String> {
    let fields = object(value)?;
    let colliders = array(field(fields, "colliders")?)?
        .iter()
        .map(decode_collider)
        .collect::<Result<Vec<_>, _>>()?;
    let cameras = array(field(fields, "cameras")?)?
        .iter()
        .map(decode_camera)
        .collect::<Result<Vec<_>, _>>()?;
    let viewport_value = vec2(field(fields, "viewport")?)?;
    let viewport = ScreenViewport::from_size(viewport_value).map_err(|error| error.to_string())?;
    let mouse_position = vec2(field(fields, "mouse_position")?)?;
    Ok(PhysicsWorldView {
        colliders,
        cameras,
        viewport,
        mouse_position,
    })
}

fn decode_collider(value: &Value) -> Result<PhysicsCollider, String> {
    let fields = object(value)?;
    let shape_fields = object(field(fields, "shape")?)?;
    let shape = match string(field(shape_fields, "kind")?)? {
        "aabb" => WorldShape::Aabb(WorldAabb {
            center: vec3(field(shape_fields, "center")?)?,
            half_extents: vec3(field(shape_fields, "half_extents")?)?,
        }),
        "sphere" => WorldShape::Sphere(WorldSphere {
            center: vec3(field(shape_fields, "center")?)?,
            radius: number(field(shape_fields, "radius")?)? as f32,
        }),
        "capsule_y" => WorldShape::CapsuleY(WorldCapsule {
            segment_a: vec3(field(shape_fields, "segment_a")?)?,
            segment_b: vec3(field(shape_fields, "segment_b")?)?,
            radius: number(field(shape_fields, "radius")?)? as f32,
        }),
        kind => return Err(format!("unknown physics shape kind `{kind}`")),
    };
    let body = match string(field(fields, "body")?)? {
        "static" => PhysicsBodyKind::Static,
        "kinematic" => PhysicsBodyKind::Kinematic,
        "dynamic" => PhysicsBodyKind::Dynamic,
        body => return Err(format!("unknown physics body kind `{body}`")),
    };
    Ok(PhysicsCollider {
        entity: entity(field(fields, "entity")?)?,
        shape,
        membership: unsigned(field(fields, "membership")?)?
            .try_into()
            .map_err(|_| "physics membership exceeds u32".to_owned())?,
        mask: unsigned(field(fields, "mask")?)?
            .try_into()
            .map_err(|_| "physics mask exceeds u32".to_owned())?,
        is_trigger: boolean(field(fields, "is_trigger")?)?,
        body,
    })
}

fn decode_camera(value: &Value) -> Result<PhysicsCamera, String> {
    let fields = object(value)?;
    let camera_fields = object(field(fields, "camera")?)?;
    let matrix_values = array(field(fields, "world_matrix")?)?;
    if matrix_values.len() != 16 {
        return Err("camera world matrix must contain 16 numbers".to_owned());
    }
    let mut matrix = [0.0_f32; 16];
    for (index, value) in matrix_values.iter().enumerate() {
        matrix[index] = number(value)? as f32;
    }
    let enabled = camera_fields
        .get("enabled")
        .map(boolean)
        .transpose()?
        .unwrap_or(true);
    let priority = camera_fields
        .get("priority")
        .map(signed_i32)
        .transpose()?
        .unwrap_or(0);
    Ok(PhysicsCamera {
        entity: entity(field(fields, "entity")?)?,
        camera: Camera3D {
            enabled,
            priority,
            fov_y_radians: number(field(camera_fields, "fov_y_radians")?)? as f32,
            near: number(field(camera_fields, "near")?)? as f32,
            far: number(field(camera_fields, "far")?)? as f32,
            aspect: number(field(camera_fields, "aspect")?)? as f32,
        },
        world: GlobalTransform(Mat4::from_cols_array(&matrix)),
    })
}

fn field<'a>(fields: &'a BTreeMap<String, Value>, name: &str) -> Result<&'a Value, String> {
    fields
        .get(name)
        .ok_or_else(|| format!("required field `{name}` is missing"))
}

fn object(value: &Value) -> Result<&BTreeMap<String, Value>, String> {
    if let Value::Object(value) = value {
        Ok(value)
    } else {
        Err("expected an object".to_owned())
    }
}

fn array(value: &Value) -> Result<&[Value], String> {
    if let Value::Array(value) = value {
        Ok(value)
    } else {
        Err("expected an array".to_owned())
    }
}

fn string(value: &Value) -> Result<&str, String> {
    if let Value::String(value) = value {
        Ok(value)
    } else {
        Err("expected a string".to_owned())
    }
}

fn boolean(value: &Value) -> Result<bool, String> {
    if let Value::Bool(value) = value {
        Ok(*value)
    } else {
        Err("expected a boolean".to_owned())
    }
}

fn number(value: &Value) -> Result<f64, String> {
    match value {
        Value::F64(value) => Ok(*value),
        Value::I64(value) => Ok(*value as f64),
        Value::U64(value) => Ok(*value as f64),
        Value::String(value) => value
            .parse()
            .map_err(|_| "expected a numeric string".to_owned()),
        _ => Err("expected a number".to_owned()),
    }
}

fn unsigned(value: &Value) -> Result<u64, String> {
    match value {
        Value::U64(value) => Ok(*value),
        Value::I64(value) => (*value)
            .try_into()
            .map_err(|_| "expected a non-negative integer".to_owned()),
        Value::String(value) => value
            .parse()
            .map_err(|_| "expected a non-negative integer string".to_owned()),
        _ => Err("expected a non-negative integer".to_owned()),
    }
}

/// Decodes an exact signed 32-bit integer without fractional truncation.
fn signed_i32(value: &Value) -> Result<i32, String> {
    let signed = match value {
        Value::I64(value) => *value,
        Value::U64(value) => i64::try_from(*value)
            .map_err(|_| "expected a signed 32-bit integer".to_owned())?,
        Value::String(value) => value
            .parse::<i64>()
            .map_err(|_| "expected a signed 32-bit integer string".to_owned())?,
        _ => return Err("expected a signed 32-bit integer".to_owned()),
    };
    i32::try_from(signed).map_err(|_| "expected a signed 32-bit integer".to_owned())
}

fn vec2(value: &Value) -> Result<Vec2, String> {
    let values = array(value)?;
    if values.len() != 2 {
        return Err("expected a two-number vector".to_owned());
    }
    Ok(Vec2::new(
        number(&values[0])? as f32,
        number(&values[1])? as f32,
    ))
}

fn vec3(value: &Value) -> Result<Vec3, String> {
    let values = array(value)?;
    if values.len() != 3 {
        return Err("expected a three-number vector".to_owned());
    }
    Ok(Vec3::new(
        number(&values[0])? as f32,
        number(&values[1])? as f32,
        number(&values[2])? as f32,
    ))
}

fn entity(value: &Value) -> Result<GameEntityHandle, String> {
    let fields = object(value)?;
    Ok(GameEntityHandle {
        id: unsigned(field(fields, "id")?)?
            .try_into()
            .map_err(|_| "entity id exceeds u32".to_owned())?,
        generation: unsigned(field(fields, "generation")?)?
            .try_into()
            .map_err(|_| "entity generation exceeds u32".to_owned())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physics_camera(id: u32, enabled: bool, priority: i32) -> PhysicsCamera {
        let camera = Camera3D {
            enabled,
            priority,
            ..Camera3D::default()
        };
        PhysicsCamera {
            entity: GameEntityHandle { id, generation: 0 },
            camera,
            world: GlobalTransform::default(),
        }
    }

    #[test]
    fn primary_camera_uses_enabled_priority_and_entity_tie_breaking() {
        let world = PhysicsWorldView {
            colliders: Vec::new(),
            cameras: vec![
                physics_camera(5, true, 10),
                physics_camera(1, false, 100),
                physics_camera(2, true, 10),
                physics_camera(3, true, 0),
            ],
            viewport: ScreenViewport::from_size(Vec2::ONE).expect("viewport must be valid"),
            mouse_position: Vec2::ZERO,
        };

        assert_eq!(
            world.primary_camera().map(|camera| camera.entity.id),
            Some(2)
        );
    }

    #[test]
    fn physics_raycast_returns_nearest_filtered_collider() {
        let near = PhysicsCollider {
            entity: GameEntityHandle {
                id: 1,
                generation: 0,
            },
            shape: WorldShape::Sphere(WorldSphere {
                center: Vec3::new(0.0, 0.0, -2.0),
                radius: 0.5,
            }),
            membership: 1,
            mask: u32::MAX,
            is_trigger: false,
            body: PhysicsBodyKind::Static,
        };
        let far = PhysicsCollider {
            entity: GameEntityHandle {
                id: 2,
                generation: 0,
            },
            shape: WorldShape::Sphere(WorldSphere {
                center: Vec3::new(0.0, 0.0, -5.0),
                radius: 0.5,
            }),
            ..near
        };
        let world = PhysicsWorldView {
            colliders: vec![far, near],
            cameras: Vec::new(),
            viewport: ScreenViewport::from_size(Vec2::new(1280.0, 720.0)).unwrap(),
            mouse_position: Vec2::ZERO,
        };
        let ray = Ray3d::new(Vec3::ZERO, Vec3::NEG_Z, 10.0).unwrap();
        let hit = world
            .raycast(ray, &PhysicsQueryFilter::default())
            .expect("nearest sphere should be hit");
        assert_eq!(hit.entity, near.entity);
    }
}
