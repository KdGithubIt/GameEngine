//! Common world-space and screen-space queries for gameplay and editor tools.
//!
//! These helpers keep camera unprojection and primitive intersection math behind
//! small typed APIs. Callers provide query intent and receive the closest hit.

use std::fmt;

use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::camera::{Camera3D, ViewportSize};
use crate::collision::{WorldAabb, WorldCapsule, WorldShape, WorldSphere};
use crate::input::MouseInput;
use crate::transform::GlobalTransform;

use super::core::{StaticTriangleMesh, TriangleMeshRayHit};

/// Invalid input supplied to a spatial query helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialQueryError {
    /// A ray value contained NaN or infinity.
    NonFiniteRay,
    /// A ray direction or segment had no usable length.
    ZeroDirection,
    /// A ray maximum distance was negative.
    NegativeMaximumDistance,
    /// A viewport had invalid dimensions.
    InvalidViewport,
    /// A screen position contained NaN or infinity.
    NonFiniteScreenPosition,
    /// Camera projection values or the camera transform were invalid.
    InvalidCamera,
    /// A plane point or normal was invalid.
    InvalidPlane,
}

impl fmt::Display for SpatialQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteRay => write!(formatter, "ray values must be finite"),
            Self::ZeroDirection => write!(formatter, "ray direction must be non-zero"),
            Self::NegativeMaximumDistance => {
                write!(formatter, "ray maximum distance must be non-negative")
            }
            Self::InvalidViewport => {
                write!(formatter, "viewport must have a finite positive size")
            }
            Self::NonFiniteScreenPosition => {
                write!(formatter, "screen position must be finite")
            }
            Self::InvalidCamera => {
                write!(formatter, "camera projection or transform is invalid")
            }
            Self::InvalidPlane => write!(formatter, "plane point and normal must be valid"),
        }
    }
}

impl std::error::Error for SpatialQueryError {}

/// A finite world-space ray with a normalized direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray3d {
    /// World-space starting point.
    pub origin: Vec3,
    /// Normalized world-space direction.
    pub direction: Vec3,
    /// Furthest accepted hit distance from [`Self::origin`].
    pub maximum_distance: f32,
}

impl Ray3d {
    /// Creates a validated finite ray.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when a value is non-finite, the direction
    /// is zero, or `maximum_distance` is negative.
    pub fn new(
        origin: Vec3,
        direction: Vec3,
        maximum_distance: f32,
    ) -> Result<Self, SpatialQueryError> {
        if !origin.is_finite() || !direction.is_finite() || !maximum_distance.is_finite() {
            return Err(SpatialQueryError::NonFiniteRay);
        }
        if maximum_distance < 0.0 {
            return Err(SpatialQueryError::NegativeMaximumDistance);
        }
        let direction = direction
            .try_normalize()
            .ok_or(SpatialQueryError::ZeroDirection)?;
        Ok(Self {
            origin,
            direction,
            maximum_distance,
        })
    }

    /// Creates a ray that ends exactly at `end`.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError`] when either point is non-finite or both
    /// points are equal.
    pub fn from_segment(start: Vec3, end: Vec3) -> Result<Self, SpatialQueryError> {
        if !start.is_finite() || !end.is_finite() {
            return Err(SpatialQueryError::NonFiniteRay);
        }
        let delta = end - start;
        Self::new(start, delta, delta.length())
    }

    /// Returns the world-space endpoint of this ray.
    pub fn end(self) -> Vec3 {
        self.origin + self.direction * self.maximum_distance
    }

    /// Returns a world-space point when `distance` lies on this ray.
    pub fn point_at(self, distance: f32) -> Option<Vec3> {
        let is_inside = distance.is_finite() && (0.0..=self.maximum_distance).contains(&distance);
        is_inside.then(|| self.origin + self.direction * distance)
    }
}

/// Intersection data shared by primitive, plane, and screen queries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// Distance from the ray origin.
    pub distance: f32,
    /// World-space intersection point.
    pub position: Vec3,
    /// World-space unit surface normal.
    pub normal: Vec3,
}

/// A closest ray hit associated with a caller-owned target value.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetedRayHit<T> {
    /// Target supplied alongside its world shape.
    pub target: T,
    /// Closest intersection with that target.
    pub hit: RayHit,
}

/// A top-left-origin rectangular viewport in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenViewport {
    /// Top-left viewport position in physical pixels.
    pub origin: Vec2,
    /// Width and height in physical pixels.
    pub size: Vec2,
}

impl ScreenViewport {
    /// Creates a full-surface viewport beginning at `(0, 0)`.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError::InvalidViewport`] for non-finite or
    /// non-positive dimensions.
    pub fn from_size(size: Vec2) -> Result<Self, SpatialQueryError> {
        Self::new(Vec2::ZERO, size)
    }

    /// Creates a viewport at an explicit top-left pixel origin.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError::InvalidViewport`] for non-finite values or
    /// non-positive dimensions.
    pub fn new(origin: Vec2, size: Vec2) -> Result<Self, SpatialQueryError> {
        let viewport = Self { origin, size };
        viewport.validate()?;
        Ok(viewport)
    }

    fn validate(self) -> Result<(), SpatialQueryError> {
        let is_valid = self.origin.is_finite()
            && self.size.is_finite()
            && self.size.x > 0.0
            && self.size.y > 0.0;
        if is_valid {
            Ok(())
        } else {
            Err(SpatialQueryError::InvalidViewport)
        }
    }

    fn aspect(self) -> f32 {
        self.size.x / self.size.y
    }
}

impl TryFrom<&ViewportSize> for ScreenViewport {
    type Error = SpatialQueryError;

    fn try_from(viewport: &ViewportSize) -> Result<Self, Self::Error> {
        let size = Vec2::new(viewport.width as f32, viewport.height as f32);
        Self::from_size(size)
    }
}

/// A world-space plane represented by one point and a normalized normal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane3d {
    /// One world-space point on the plane.
    pub point: Vec3,
    /// Normalized world-space plane normal.
    pub normal: Vec3,
}

impl Plane3d {
    /// Creates a validated plane.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError::InvalidPlane`] for non-finite values or a
    /// zero normal.
    pub fn new(point: Vec3, normal: Vec3) -> Result<Self, SpatialQueryError> {
        if !point.is_finite() || !normal.is_finite() {
            return Err(SpatialQueryError::InvalidPlane);
        }
        let normal = normal
            .try_normalize()
            .ok_or(SpatialQueryError::InvalidPlane)?;
        Ok(Self { point, normal })
    }

    /// Creates a horizontal plane at world height `y`.
    ///
    /// # Errors
    ///
    /// Returns [`SpatialQueryError::InvalidPlane`] when `y` is non-finite.
    pub fn horizontal(y: f32) -> Result<Self, SpatialQueryError> {
        Self::new(Vec3::new(0.0, y, 0.0), Vec3::Y)
    }
}

/// Creates a world-space ray from one viewport pixel.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] for invalid camera, viewport, screen, or ray
/// inputs.
pub fn screen_ray(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: ScreenViewport,
    screen_position: Vec2,
    maximum_distance: f32,
) -> Result<Ray3d, SpatialQueryError> {
    viewport.validate()?;
    if !screen_position.is_finite() {
        return Err(SpatialQueryError::NonFiniteScreenPosition);
    }

    let view_projection = camera_view_projection(camera, camera_world, viewport)?;
    let inverse = view_projection.inverse();
    if !matrix_is_finite(inverse) {
        return Err(SpatialQueryError::InvalidCamera);
    }

    let local = screen_position - viewport.origin;
    let ndc_x = local.x / viewport.size.x * 2.0 - 1.0;
    let ndc_y = 1.0 - local.y / viewport.size.y * 2.0;
    let near = inverse * Vec4::new(ndc_x, ndc_y, -1.0, 1.0);
    let far = inverse * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
    if near.w.abs() <= f32::EPSILON || far.w.abs() <= f32::EPSILON {
        return Err(SpatialQueryError::InvalidCamera);
    }

    let near = near.truncate() / near.w;
    let far = far.truncate() / far.w;
    let origin = camera_world.matrix().w_axis.truncate();
    Ray3d::new(origin, far - near, maximum_distance)
}

/// Creates a world-space ray from the current mouse position.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] under the same conditions as [`screen_ray`].
pub fn mouse_ray(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: &ViewportSize,
    mouse: &MouseInput,
    maximum_distance: f32,
) -> Result<Ray3d, SpatialQueryError> {
    let screen_position = Vec2::new(mouse.position.0, mouse.position.1);
    screen_ray(
        camera,
        camera_world,
        ScreenViewport::try_from(viewport)?,
        screen_position,
        maximum_distance,
    )
}

/// Intersects a ray with an axis-aligned world-space box.
pub fn raycast_aabb(ray: Ray3d, aabb: WorldAabb) -> Option<RayHit> {
    if !valid_aabb(aabb) {
        return None;
    }

    let minimum = aabb.center - aabb.half_extents;
    let maximum = aabb.center + aabb.half_extents;
    let axes = [
        (ray.origin.x, ray.direction.x, minimum.x, maximum.x, Vec3::X),
        (ray.origin.y, ray.direction.y, minimum.y, maximum.y, Vec3::Y),
        (ray.origin.z, ray.direction.z, minimum.z, maximum.z, Vec3::Z),
    ];

    let mut near_distance = 0.0;
    let mut far_distance = ray.maximum_distance;
    let mut near_normal = -ray.direction;
    for (origin, direction, minimum, maximum, axis) in axes {
        if direction.abs() <= f32::EPSILON {
            if origin < minimum || origin > maximum {
                return None;
            }
            continue;
        }

        let mut entry = ((minimum - origin) / direction, -axis);
        let mut exit = ((maximum - origin) / direction, axis);
        if entry.0 > exit.0 {
            std::mem::swap(&mut entry, &mut exit);
        }
        if entry.0 > near_distance {
            near_distance = entry.0;
            near_normal = entry.1;
        }
        far_distance = far_distance.min(exit.0);
        if near_distance > far_distance {
            return None;
        }
    }

    ray_hit(ray, near_distance, near_normal)
}

/// Intersects a ray with a world-space sphere.
pub fn raycast_sphere(ray: Ray3d, sphere: WorldSphere) -> Option<RayHit> {
    if !valid_sphere(sphere) {
        return None;
    }

    let offset = ray.origin - sphere.center;
    let radius_squared = sphere.radius * sphere.radius;
    if offset.length_squared() <= radius_squared {
        let normal = offset.try_normalize().unwrap_or(-ray.direction);
        return ray_hit(ray, 0.0, normal);
    }

    let projection = offset.dot(ray.direction);
    let discriminant = projection * projection - (offset.length_squared() - radius_squared);
    if discriminant < 0.0 {
        return None;
    }

    let distance = -projection - discriminant.sqrt();
    let position = ray.point_at(distance)?;
    let normal = (position - sphere.center).normalize_or_zero();
    ray_hit(ray, distance, normal)
}

/// Intersects a ray with a capsule defined by a core segment and radius.
pub fn raycast_capsule(ray: Ray3d, capsule: WorldCapsule) -> Option<RayHit> {
    if !valid_capsule(capsule) {
        return None;
    }

    let segment = capsule.segment_b - capsule.segment_a;
    let segment_length_squared = segment.length_squared();
    if segment_length_squared <= f32::EPSILON {
        let sphere = WorldSphere {
            center: capsule.segment_a,
            radius: capsule.radius,
        };
        return raycast_sphere(ray, sphere);
    }

    let nearest = closest_point_on_segment(ray.origin, capsule.segment_a, capsule.segment_b);
    let origin_offset = ray.origin - nearest;
    if origin_offset.length_squared() <= capsule.radius * capsule.radius {
        let normal = origin_offset.try_normalize().unwrap_or(-ray.direction);
        return ray_hit(ray, 0.0, normal);
    }

    let from_a = ray.origin - capsule.segment_a;
    let segment_dot_ray = segment.dot(ray.direction);
    let segment_dot_origin = segment.dot(from_a);
    let ray_dot_origin = ray.direction.dot(from_a);
    let a = segment_length_squared - segment_dot_ray * segment_dot_ray;
    let b = segment_length_squared * ray_dot_origin - segment_dot_origin * segment_dot_ray;
    let c = segment_length_squared * from_a.length_squared()
        - segment_dot_origin * segment_dot_origin
        - capsule.radius * capsule.radius * segment_length_squared;

    let mut closest = None;
    let discriminant = b * b - a * c;
    if a.abs() > f32::EPSILON && discriminant >= 0.0 {
        let distance = (-b - discriminant.sqrt()) / a;
        let segment_position = segment_dot_origin + distance * segment_dot_ray;
        let is_on_ray = (0.0..=ray.maximum_distance).contains(&distance);
        let is_on_segment = (0.0..=segment_length_squared).contains(&segment_position);
        if is_on_ray && is_on_segment {
            let position = ray.origin + ray.direction * distance;
            let segment_ratio = segment_position / segment_length_squared;
            let on_segment = capsule.segment_a + segment * segment_ratio;
            let normal = (position - on_segment).normalize_or_zero();
            closest = ray_hit(ray, distance, normal);
        }
    }

    for center in [capsule.segment_a, capsule.segment_b] {
        let sphere = WorldSphere {
            center,
            radius: capsule.radius,
        };
        if let Some(hit) = raycast_sphere(ray, sphere) {
            let replace = closest
                .as_ref()
                .is_none_or(|current: &RayHit| hit.distance < current.distance);
            if replace {
                closest = Some(hit);
            }
        }
    }
    closest
}

/// Intersects a ray with any built-in world collision shape.
pub fn raycast_shape(ray: Ray3d, shape: WorldShape) -> Option<RayHit> {
    match shape {
        WorldShape::Aabb(aabb) => raycast_aabb(ray, aabb),
        WorldShape::Sphere(sphere) => raycast_sphere(ray, sphere),
        WorldShape::CapsuleY(capsule) => raycast_capsule(ray, capsule),
    }
}

/// Returns the closest hit across caller-supplied target and shape pairs.
pub fn raycast_shapes<T>(
    ray: Ray3d,
    shapes: impl IntoIterator<Item = (T, WorldShape)>,
) -> Option<TargetedRayHit<T>> {
    let mut closest = None;
    for (target, shape) in shapes {
        let Some(hit) = raycast_shape(ray, shape) else {
            continue;
        };
        let replace = closest
            .as_ref()
            .is_none_or(|current: &TargetedRayHit<T>| hit.distance < current.hit.distance);
        if replace {
            closest = Some(TargetedRayHit { target, hit });
        }
    }
    closest
}

/// Creates a screen ray and returns the closest primitive-shape hit.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] under the same conditions as [`screen_ray`].
pub fn screen_raycast_shapes<T>(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: ScreenViewport,
    screen_position: Vec2,
    maximum_distance: f32,
    shapes: impl IntoIterator<Item = (T, WorldShape)>,
) -> Result<Option<TargetedRayHit<T>>, SpatialQueryError> {
    let ray = screen_ray(
        camera,
        camera_world,
        viewport,
        screen_position,
        maximum_distance,
    )?;
    Ok(raycast_shapes(ray, shapes))
}

/// Creates a mouse ray and returns the closest primitive-shape hit.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] under the same conditions as [`mouse_ray`].
pub fn mouse_raycast_shapes<T>(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: &ViewportSize,
    mouse: &MouseInput,
    maximum_distance: f32,
    shapes: impl IntoIterator<Item = (T, WorldShape)>,
) -> Result<Option<TargetedRayHit<T>>, SpatialQueryError> {
    let ray = mouse_ray(camera, camera_world, viewport, mouse, maximum_distance)?;
    Ok(raycast_shapes(ray, shapes))
}

/// Intersects a ray with a world-space plane.
pub fn raycast_plane(ray: Ray3d, plane: Plane3d) -> Option<RayHit> {
    let denominator = plane.normal.dot(ray.direction);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }
    let distance = (plane.point - ray.origin).dot(plane.normal) / denominator;
    ray_hit(ray, distance, plane.normal)
}

/// Converts one screen position directly into a world-space plane hit.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] under the same conditions as [`screen_ray`].
pub fn screen_point_on_plane(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: ScreenViewport,
    screen_position: Vec2,
    plane: Plane3d,
    maximum_distance: f32,
) -> Result<Option<RayHit>, SpatialQueryError> {
    let ray = screen_ray(
        camera,
        camera_world,
        viewport,
        screen_position,
        maximum_distance,
    )?;
    Ok(raycast_plane(ray, plane))
}

/// Converts the current mouse position directly into a world-space plane hit.
///
/// # Errors
///
/// Returns [`SpatialQueryError`] under the same conditions as [`mouse_ray`].
pub fn mouse_point_on_plane(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: &ViewportSize,
    mouse: &MouseInput,
    plane: Plane3d,
    maximum_distance: f32,
) -> Result<Option<RayHit>, SpatialQueryError> {
    let ray = mouse_ray(camera, camera_world, viewport, mouse, maximum_distance)?;
    Ok(raycast_plane(ray, plane))
}

/// Intersects a validated ray with one static triangle mesh.
pub fn raycast_triangle_mesh(ray: Ray3d, mesh: &StaticTriangleMesh) -> Option<TriangleMeshRayHit> {
    mesh.raycast(ray.origin, ray.direction, ray.maximum_distance)
}

fn camera_view_projection(
    camera: &Camera3D,
    camera_world: &GlobalTransform,
    viewport: ScreenViewport,
) -> Result<Mat4, SpatialQueryError> {
    let camera_is_valid = camera.fov_y_radians.is_finite()
        && camera.near.is_finite()
        && camera.far.is_finite()
        && camera.fov_y_radians > 0.0
        && camera.fov_y_radians < std::f32::consts::PI
        && camera.near > 0.0
        && camera.far > camera.near;
    if !camera_is_valid || !matrix_is_finite(camera_world.matrix()) {
        return Err(SpatialQueryError::InvalidCamera);
    }

    let projection = Mat4::perspective_rh(
        camera.fov_y_radians,
        viewport.aspect(),
        camera.near,
        camera.far,
    );
    let view_projection = projection * camera_world.matrix().inverse();
    if matrix_is_finite(view_projection) {
        Ok(view_projection)
    } else {
        Err(SpatialQueryError::InvalidCamera)
    }
}

fn ray_hit(ray: Ray3d, distance: f32, normal: Vec3) -> Option<RayHit> {
    let position = ray.point_at(distance)?;
    Some(RayHit {
        distance,
        position,
        normal,
    })
}

fn valid_aabb(aabb: WorldAabb) -> bool {
    aabb.center.is_finite()
        && aabb.half_extents.is_finite()
        && aabb.half_extents.min_element() >= 0.0
}

fn valid_sphere(sphere: WorldSphere) -> bool {
    sphere.center.is_finite() && sphere.radius.is_finite() && sphere.radius >= 0.0
}

fn valid_capsule(capsule: WorldCapsule) -> bool {
    capsule.segment_a.is_finite()
        && capsule.segment_b.is_finite()
        && capsule.radius.is_finite()
        && capsule.radius >= 0.0
}

fn matrix_is_finite(matrix: Mat4) -> bool {
    matrix.to_cols_array().into_iter().all(f32::is_finite)
}

fn closest_point_on_segment(point: Vec3, start: Vec3, end: Vec3) -> Vec3 {
    let segment = end - start;
    let length_squared = segment.length_squared();
    if length_squared <= f32::EPSILON {
        return start;
    }
    let ratio = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    start + segment * ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_world(position: Vec3) -> GlobalTransform {
        GlobalTransform(Mat4::from_translation(position))
    }

    #[test]
    fn ray_normalizes_direction_and_preserves_length() {
        let ray = Ray3d::new(Vec3::ONE, Vec3::new(0.0, 0.0, -4.0), 10.0)
            .expect("valid ray must be created");

        assert_eq!(ray.direction, Vec3::NEG_Z);
        assert_eq!(ray.end(), Vec3::new(1.0, 1.0, -9.0));
    }

    #[test]
    fn raycast_shapes_returns_closest_target() {
        let ray = Ray3d::new(Vec3::ZERO, Vec3::X, 20.0).expect("valid ray must be created");
        let shapes = [
            (
                "far",
                WorldShape::Sphere(WorldSphere {
                    center: Vec3::new(8.0, 0.0, 0.0),
                    radius: 1.0,
                }),
            ),
            (
                "near",
                WorldShape::Aabb(WorldAabb {
                    center: Vec3::new(3.0, 0.0, 0.0),
                    half_extents: Vec3::ONE,
                }),
            ),
        ];
        let hit = raycast_shapes(ray, shapes).expect("one shape must be hit");

        assert_eq!(hit.target, "near");
        assert!((hit.hit.distance - 2.0).abs() < 1.0e-5);
        assert_eq!(hit.hit.normal, Vec3::NEG_X);
    }

    #[test]
    fn center_screen_position_creates_forward_ray() {
        let camera = Camera3D::default();
        let viewport =
            ScreenViewport::from_size(Vec2::new(1280.0, 720.0)).expect("viewport must be valid");
        let ray = screen_ray(
            &camera,
            &camera_world(Vec3::ZERO),
            viewport,
            Vec2::new(640.0, 360.0),
            100.0,
        )
        .expect("screen ray must be created");

        assert!((ray.direction - Vec3::NEG_Z).length() < 1.0e-5);
    }

    #[test]
    fn screen_position_returns_world_plane_hit() {
        let camera = Camera3D::default();
        let viewport =
            ScreenViewport::from_size(Vec2::new(1280.0, 720.0)).expect("viewport must be valid");
        let plane = Plane3d::new(Vec3::new(0.0, 0.0, -5.0), Vec3::Z).expect("plane must be valid");
        let hit = screen_point_on_plane(
            &camera,
            &camera_world(Vec3::ZERO),
            viewport,
            Vec2::new(640.0, 360.0),
            plane,
            100.0,
        )
        .expect("query inputs must be valid")
        .expect("center ray must hit the plane");

        let expected = Vec3::new(0.0, 0.0, -5.0);
        assert!((hit.position - expected).length() < 1.0e-4);
    }
}
