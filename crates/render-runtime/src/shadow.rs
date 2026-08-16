//! Shadow mapping and environment-lighting runtime settings.

use glam::{Mat4, Vec3, Vec4};

use crate::asset::RuntimeAssetId;
use crate::camera::Camera3D;
use crate::light::AmbientLight;
use crate::postprocess::PostProcessSettings;
use crate::transform::Transform;

/// Number of directional-light cascades used by the Phase 41 MVP.
pub const SHADOW_CASCADE_COUNT: usize = 2;

/// Texture format used for the Phase 41 directional shadow map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowMapFormat {
    /// 32-bit floating point depth texture.
    Depth32Float,
}

impl ShadowMapFormat {
    /// Returns the wgpu texture format for this shadow-map format.
    pub fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Depth32Float => wgpu::TextureFormat::Depth32Float,
        }
    }
}

/// One directional-light shadow cascade in camera depth space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowCascade {
    /// Near distance for this cascade.
    pub near: f32,
    /// Far distance for this cascade.
    pub far: f32,
}

/// GPU texture contract for the directional shadow atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowMapDescriptor {
    /// Shadow atlas width in pixels.
    pub width: u32,
    /// Shadow atlas height in pixels.
    pub height: u32,
    /// Number of array layers in the atlas.
    pub layers: u32,
    /// Texture format used by the atlas.
    pub format: ShadowMapFormat,
}

impl ShadowMapDescriptor {
    /// Returns the texture extent represented by this descriptor.
    pub fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.width.max(1),
            height: self.height.max(1),
            depth_or_array_layers: self.layers.max(1),
        }
    }
}

/// Runtime settings for directional-light cascaded shadow maps.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowSettings {
    /// Whether shadow rendering is enabled.
    pub enabled: bool,
    /// Shadow-map texture resolution in pixels.
    pub resolution: u32,
    /// Shadow-map texture format.
    pub format: ShadowMapFormat,
    /// Normalized far depth for each cascade.
    pub cascade_splits: [f32; SHADOW_CASCADE_COUNT],
    /// Constant depth bias applied by the shadow pass.
    pub depth_bias: f32,
    /// Normal-based bias applied by the shadow pass.
    pub normal_bias: f32,
}

impl ShadowSettings {
    /// Creates shadow settings from ADR 0036 defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the configured cascade depth ranges for a camera range.
    pub fn cascades(
        &self,
        camera_near: f32,
        camera_far: f32,
    ) -> [ShadowCascade; SHADOW_CASCADE_COUNT] {
        let near = camera_near.max(0.0);
        let far = camera_far.max(near);
        let range = far - near;
        let first_far = near + range * self.cascade_splits[0].clamp(0.0, 1.0);
        let second_far = near + range * self.cascade_splits[1].clamp(0.0, 1.0);
        [
            ShadowCascade {
                near,
                far: first_far.max(near),
            },
            ShadowCascade {
                near: first_far.max(near),
                far: second_far.max(first_far),
            },
        ]
    }

    /// Returns the texture contract for the directional shadow atlas.
    pub fn map_descriptor(&self) -> ShadowMapDescriptor {
        ShadowMapDescriptor {
            width: self.resolution.max(1),
            height: self.resolution.max(1),
            layers: SHADOW_CASCADE_COUNT as u32,
            format: self.format,
        }
    }
}

impl Default for ShadowSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            resolution: 2048,
            format: ShadowMapFormat::Depth32Float,
            cascade_splits: [0.2, 1.0],
            depth_bias: 0.0005,
            normal_bias: 0.01,
        }
    }
}

/// Computes one light view-projection matrix per cascade (Phase 50).
///
/// Each cascade slice of the camera frustum is transformed into light space
/// and enclosed in an orthographic box, so every visible point of the slice
/// lands inside the corresponding shadow map. The near plane is pulled back
/// by the slice's own light-space depth range so casters behind the slice
/// still write shadow depth.
///
/// A zero `light_direction` degrades to straight down instead of failing.
pub fn cascade_view_projections(
    camera: &Camera3D,
    camera_transform: &Transform,
    light_direction: Vec3,
    settings: &ShadowSettings,
) -> [Mat4; SHADOW_CASCADE_COUNT] {
    let direction = light_direction.normalize_or_zero();
    let direction = if direction == Vec3::ZERO {
        Vec3::NEG_Y
    } else {
        direction
    };
    let up = if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };

    let view = Camera3D::view_matrix(camera_transform);
    let cascades = settings.cascades(camera.near, camera.far);

    cascades.map(|cascade| {
        let near = cascade.near.max(0.001);
        let far = cascade.far.max(near + 0.001);
        let slice_projection = Mat4::perspective_rh(camera.fov_y_radians, camera.aspect, near, far);
        let inverse_slice = (slice_projection * view).inverse();

        // The eight corners of the slice in world space (wgpu NDC z in 0..1).
        let mut corners = [Vec3::ZERO; 8];
        let mut corner_index = 0;
        for x in [-1.0_f32, 1.0] {
            for y in [-1.0_f32, 1.0] {
                for z in [0.0_f32, 1.0] {
                    let clip = inverse_slice * Vec4::new(x, y, z, 1.0);
                    corners[corner_index] = clip.truncate() / clip.w;
                    corner_index += 1;
                }
            }
        }

        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        let light_view = Mat4::look_to_rh(center, direction, up);

        let mut minimum = Vec3::splat(f32::MAX);
        let mut maximum = Vec3::splat(f32::MIN);
        for corner in corners {
            let light_space = light_view.transform_point3(corner);
            minimum = minimum.min(light_space);
            maximum = maximum.max(light_space);
        }

        // look_to_rh looks down -Z: nearer points have larger z. Pull the
        // near plane back so off-slice casters still occlude.
        let depth_backup = (maximum.z - minimum.z).max(1.0);
        let projection = Mat4::orthographic_rh(
            minimum.x,
            maximum.x,
            minimum.y,
            maximum.y,
            -(maximum.z + depth_backup),
            -minimum.z,
        );
        projection * light_view
    })
}

/// Runtime environment-lighting settings consumed by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentLighting {
    /// Optional equirectangular skybox texture used for the background and
    /// StandardLit specular image-based lighting.
    pub skybox: Option<RuntimeAssetId>,
    /// Optional equirectangular Lambert-normalized diffuse irradiance texture.
    /// When absent, the renderer derives diffuse irradiance from `skybox`.
    pub diffuse_irradiance: Option<RuntimeAssetId>,
    /// Fallback diffuse irradiance color when no texture is bound.
    pub diffuse_color: glam::Vec3,
    /// Environment-lighting intensity multiplier.
    pub intensity: f32,
    /// Whether diffuse image-based lighting is enabled. A resolved skybox can
    /// still contribute StandardLit specular IBL when this is `false`.
    pub diffuse_ibl_enabled: bool,
}

impl Default for EnvironmentLighting {
    fn default() -> Self {
        Self {
            skybox: None,
            diffuse_irradiance: None,
            diffuse_color: glam::Vec3::ONE,
            intensity: 1.0,
            diffuse_ibl_enabled: false,
        }
    }
}

impl EnvironmentLighting {
    /// Applies the color-only diffuse fallback to the ambient-light contract.
    pub fn apply_to_ambient(&self, ambient: &AmbientLight) -> AmbientLight {
        if !self.diffuse_ibl_enabled {
            return ambient.clone();
        }

        AmbientLight {
            color: self.diffuse_color,
            intensity: ambient.intensity * self.intensity.max(0.0),
        }
    }
}

/// Mirrors scene-owned presentation components into renderer resources.
///
/// The renderer reads these values as resources, while the editor serializes
/// them as normal scene components. Resetting to defaults when a component is
/// absent is important during scene changes: otherwise a level without custom
/// presentation settings would inherit the previous level's artistic state.
pub fn presentation_resource_mirror_system(
    shadow_components: engine_ecs::Query<&ShadowSettings>,
    environment_components: engine_ecs::Query<&EnvironmentLighting>,
    post_process_components: engine_ecs::Query<&PostProcessSettings>,
    mut shadow_resource: engine_ecs::ResMut<ShadowSettings>,
    mut environment_resource: engine_ecs::ResMut<EnvironmentLighting>,
    mut post_process_resource: engine_ecs::ResMut<PostProcessSettings>,
) {
    *shadow_resource = shadow_components
        .iter()
        .next()
        .map(|(_, settings)| settings.clone())
        .unwrap_or_default();
    *environment_resource = environment_components
        .iter()
        .next()
        .map(|(_, settings)| settings.clone())
        .unwrap_or_default();
    *post_process_resource = post_process_components
        .iter()
        .next()
        .map(|(_, settings)| *settings)
        .unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::{IntoSystem, System, World};

    #[test]
    fn presentation_resources_follow_scene_components_and_reset_when_absent() {
        let mut world = World::new();
        world.insert_resource(ShadowSettings::default());
        world.insert_resource(EnvironmentLighting::default());
        world.insert_resource(PostProcessSettings::default());

        let entity = world.spawn().expect("entity must spawn");
        world
            .add_component(
                entity,
                ShadowSettings {
                    enabled: false,
                    ..ShadowSettings::default()
                },
            )
            .expect("shadow component must insert");
        world
            .add_component(
                entity,
                EnvironmentLighting {
                    diffuse_ibl_enabled: true,
                    intensity: 2.0,
                    ..EnvironmentLighting::default()
                },
            )
            .expect("environment component must insert");
        world
            .add_component(
                entity,
                PostProcessSettings {
                    exposure: 1.5,
                    ..PostProcessSettings::default()
                },
            )
            .expect("post-process component must insert");

        let mut system = presentation_resource_mirror_system
            .into_system()
            .expect("system must build");
        system.run(&mut world).expect("system must run");
        assert!(
            !world
                .get_resource::<ShadowSettings>()
                .expect("shadow resource must exist")
                .enabled
        );
        assert_eq!(
            world
                .get_resource::<EnvironmentLighting>()
                .expect("environment resource must exist")
                .intensity,
            2.0
        );
        assert_eq!(
            world
                .get_resource::<PostProcessSettings>()
                .expect("post-process resource must exist")
                .exposure,
            1.5
        );

        world.despawn(entity).expect("entity must despawn");
        system.run(&mut world).expect("system must run again");
        assert_eq!(
            world
                .get_resource::<ShadowSettings>()
                .expect("shadow resource must exist"),
            &ShadowSettings::default()
        );
        assert_eq!(
            world
                .get_resource::<EnvironmentLighting>()
                .expect("environment resource must exist"),
            &EnvironmentLighting::default()
        );
        assert_eq!(
            world
                .get_resource::<PostProcessSettings>()
                .expect("post-process resource must exist"),
            &PostProcessSettings::default()
        );
    }

    #[test]
    fn shadow_defaults_match_adr_0036() {
        let settings = ShadowSettings::default();

        assert!(settings.enabled);
        assert_eq!(settings.resolution, 2048);
        assert_eq!(settings.format, ShadowMapFormat::Depth32Float);
        assert_eq!(settings.cascade_splits, [0.2, 1.0]);
        assert_eq!(
            settings.map_descriptor(),
            ShadowMapDescriptor {
                width: 2048,
                height: 2048,
                layers: 2,
                format: ShadowMapFormat::Depth32Float
            }
        );
    }

    #[test]
    fn cascade_ranges_use_normalized_splits() {
        let settings = ShadowSettings::default();
        let cascades = settings.cascades(1.0, 101.0);

        assert_eq!(
            cascades,
            [
                ShadowCascade {
                    near: 1.0,
                    far: 21.0
                },
                ShadowCascade {
                    near: 21.0,
                    far: 101.0
                }
            ]
        );
    }

    #[test]
    fn cascade_matrices_enclose_every_frustum_corner() {
        let camera = Camera3D::new(60.0, 16.0 / 9.0, 0.1, 100.0);
        let camera_transform =
            Transform::looking_at(Vec3::new(0.0, 5.0, 10.0), Vec3::ZERO, Vec3::Y);
        let settings = ShadowSettings::default();
        let light_direction = Vec3::new(-0.4, -1.0, -0.3);

        let matrices =
            cascade_view_projections(&camera, &camera_transform, light_direction, &settings);
        let cascades = settings.cascades(camera.near, camera.far);
        let view = Camera3D::view_matrix(&camera_transform);

        for (matrix, cascade) in matrices.iter().zip(cascades) {
            let slice_projection = Mat4::perspective_rh(
                camera.fov_y_radians,
                camera.aspect,
                cascade.near.max(0.001),
                cascade.far,
            );
            let inverse_slice = (slice_projection * view).inverse();
            for x in [-1.0_f32, 1.0] {
                for y in [-1.0_f32, 1.0] {
                    for z in [0.0_f32, 1.0] {
                        let clip = inverse_slice * Vec4::new(x, y, z, 1.0);
                        let world = clip.truncate() / clip.w;
                        let light_clip = *matrix * world.extend(1.0);
                        let ndc = light_clip.truncate() / light_clip.w;
                        assert!(
                            ndc.x.abs() <= 1.001 && ndc.y.abs() <= 1.001,
                            "corner {world:?} must fit the cascade xy bounds, got {ndc:?}"
                        );
                        assert!(
                            (-0.001..=1.001).contains(&ndc.z),
                            "corner {world:?} must fit the cascade depth range, got {ndc:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn zero_light_direction_degrades_to_downward_shadows() {
        let camera = Camera3D::default();
        let camera_transform = Transform::default();
        let settings = ShadowSettings::default();

        let matrices = cascade_view_projections(&camera, &camera_transform, Vec3::ZERO, &settings);

        for matrix in matrices {
            assert!(
                matrix.to_cols_array().iter().all(|value| value.is_finite()),
                "degenerate light direction must still produce finite matrices"
            );
        }
    }

    #[test]
    fn environment_lighting_is_opt_in_for_ambient_contract() {
        let ambient = AmbientLight {
            color: glam::Vec3::new(0.2, 0.3, 0.4),
            intensity: 0.5,
        };
        let disabled = EnvironmentLighting::default();

        let unchanged = disabled.apply_to_ambient(&ambient);
        assert_eq!(unchanged.color, ambient.color);
        assert_eq!(unchanged.intensity, ambient.intensity);

        let enabled = EnvironmentLighting {
            diffuse_ibl_enabled: true,
            diffuse_color: glam::Vec3::new(1.0, 0.8, 0.6),
            intensity: 2.0,
            ..Default::default()
        };
        let applied = enabled.apply_to_ambient(&ambient);

        assert_eq!(applied.color, glam::Vec3::new(1.0, 0.8, 0.6));
        assert_eq!(applied.intensity, 1.0);
    }
}
