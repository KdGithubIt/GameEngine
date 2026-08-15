//! Ambient and directional light resources for the built-in render pipeline.

use glam::Vec3;

/// A global ambient light that illuminates all surfaces uniformly.
///
/// Insert this as an ECS resource to control the ambient contribution.
/// When absent the render pipeline uses [`AmbientLight::default`].
#[derive(Debug, Clone)]
pub struct AmbientLight {
    /// RGB color of the ambient light (each component in `[0.0, 1.0]`).
    pub color: Vec3,
    /// Intensity multiplier for the ambient contribution.
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 0.1,
        }
    }
}

/// A directional light that casts parallel rays from a fixed direction.
///
/// Insert this as an ECS resource to add a sun-like light source.
/// When absent the render pipeline uses [`DirectionalLight::default`].
#[derive(Debug, Clone)]
pub struct DirectionalLight {
    /// The direction the light travels (pointing *toward* the surface).
    ///
    /// Normalization is applied by the system before upload.
    pub direction: Vec3,
    /// RGB color of the directional light.
    pub color: Vec3,
    /// Intensity multiplier for the diffuse contribution.
    pub intensity: f32,
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(-0.5, -1.0, -0.5).normalize(),
            color: Vec3::ONE,
            intensity: 1.0,
        }
    }
}

/// Procedural gradient sky drawn behind all scene geometry.
///
/// Insert this as an ECS resource to control the background. When absent
/// the render pipeline uses [`SkySettings::default`], which draws a soft
/// blue gradient; disabling it restores the previous flat clear color.
#[derive(Debug, Clone, PartialEq)]
pub struct SkySettings {
    /// Whether the gradient sky is drawn at all.
    pub enabled: bool,
    /// Linear RGB color straight up.
    pub zenith: [f32; 3],
    /// Linear RGB color at the horizon.
    pub horizon: [f32; 3],
    /// Linear RGB color straight down.
    pub ground: [f32; 3],
}

impl Default for SkySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            zenith: [0.10, 0.22, 0.45],
            horizon: [0.55, 0.65, 0.78],
            ground: [0.16, 0.15, 0.14],
        }
    }
}

/// Mirrors entity-authored light components into render resources.
///
/// When multiple light components exist, the first query result drives the
/// resource. Scene conversion emits diagnostics for multiple directional
/// lights before Play starts.
pub fn light_resource_mirror_system(
    ambient_components: engine_ecs::Query<&AmbientLight>,
    directional_components: engine_ecs::Query<&DirectionalLight>,
    mut ambient_resource: engine_ecs::ResMut<AmbientLight>,
    mut directional_resource: engine_ecs::ResMut<DirectionalLight>,
) {
    if let Some((_, ambient)) = ambient_components.iter().next() {
        *ambient_resource = ambient.clone();
    }
    if let Some((_, directional)) = directional_components.iter().next() {
        *directional_resource = directional.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::{IntoSystem, System, World};

    #[test]
    fn default_directional_light_direction_is_unit_length() {
        let light = DirectionalLight::default();
        assert!(
            (light.direction.length() - 1.0).abs() < 1e-5,
            "direction must be normalized"
        );
    }

    #[test]
    fn default_ambient_light_has_low_intensity() {
        let light = AmbientLight::default();
        assert!(light.intensity < 0.5, "ambient should be subtle by default");
    }

    #[test]
    fn light_resource_mirror_system_uses_entity_components() {
        let mut world = World::new();
        world.insert_resource(AmbientLight::default());
        world.insert_resource(DirectionalLight::default());

        let entity = world.spawn().expect("spawn must succeed");
        world
            .add_component(
                entity,
                AmbientLight {
                    color: Vec3::new(0.25, 0.5, 0.75),
                    intensity: 0.4,
                },
            )
            .expect("ambient component must insert");
        world
            .add_component(
                entity,
                DirectionalLight {
                    direction: Vec3::Y,
                    color: Vec3::new(1.0, 0.5, 0.25),
                    intensity: 2.0,
                },
            )
            .expect("directional component must insert");

        let mut system = light_resource_mirror_system
            .into_system()
            .expect("system must build");
        system.run(&mut world).expect("system must run");

        let ambient = world
            .get_resource::<AmbientLight>()
            .expect("ambient resource must exist");
        let directional = world
            .get_resource::<DirectionalLight>()
            .expect("directional resource must exist");

        assert_eq!(ambient.color, Vec3::new(0.25, 0.5, 0.75));
        assert_eq!(ambient.intensity, 0.4);
        assert_eq!(directional.direction, Vec3::Y);
        assert_eq!(directional.color, Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(directional.intensity, 2.0);
    }
}
