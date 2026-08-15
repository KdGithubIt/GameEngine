//! Particle system demo (Phase 49, ADR 0044).
//!
//! An ember fountain orbits the origin, leaving a world-space trail, above
//! a static ground plane.
//!
//! Run with: `cargo run --example particles`

use engine::camera::default_camera_transform;
use engine::time::Time;
use engine::{
    App, Assets, Camera3D, GlobalTransform, Handle, Material, Mesh, ParticleEmitter, Query, Res,
    Transform,
};
use glam::Vec3;

/// Marks the entity moved in a circle by [`orbit_emitter_system`].
struct OrbitingEmitter;

fn main() {
    let mut app = App::new()
        .with_title("Particles - Ember Fountain")
        .with_size(1280, 720);

    {
        let world = app.world_mut();

        let (spark_mesh, ground_mesh): (Handle<Mesh>, Handle<Mesh>) = {
            let meshes = world
                .get_resource_mut::<Assets<Mesh>>()
                .expect("default mesh asset storage must exist");
            (meshes.add(Mesh::cube()), meshes.add(Mesh::plane(8.0, 8.0)))
        };

        let ground = world
            .spawn_with(Transform::from_translation(Vec3::new(0.0, -1.5, 0.0)))
            .expect("ground entity must spawn");
        world
            .add_component(ground, GlobalTransform::default())
            .expect("ground global transform must be inserted");
        world
            .add_component(ground, ground_mesh)
            .expect("ground mesh handle must be inserted");
        world
            .add_component(
                ground,
                Material {
                    color: [0.25, 0.3, 0.35, 1.0],
                    ..Material::default()
                },
            )
            .expect("ground material must be inserted");

        let emitter_entity = world
            .spawn_with(Transform::from_translation(Vec3::new(1.5, -1.0, 0.0)))
            .expect("emitter entity must spawn");
        world
            .add_component(emitter_entity, GlobalTransform::default())
            .expect("emitter global transform must be inserted");
        let mut emitter = ParticleEmitter::new(spark_mesh);
        emitter.spawn_rate = 120.0;
        emitter.lifetime = (0.6, 1.4);
        emitter.initial_speed = (2.5, 4.5);
        emitter.spread = 0.35;
        world
            .add_component(emitter_entity, emitter)
            .expect("particle emitter must be inserted");
        world
            .add_component(emitter_entity, OrbitingEmitter)
            .expect("orbit marker must be inserted");

        let camera = world
            .spawn_with(default_camera_transform())
            .expect("camera entity must spawn");
        world
            .add_component(camera, GlobalTransform::default())
            .expect("camera global transform must be inserted");
        world
            .add_component(camera, Camera3D::default())
            .expect("camera component must be inserted");
    }

    app.add_system(orbit_emitter_system);
    app.run().expect("event loop must run");
}

/// Moves marked emitters in a circle so the world-space trail is visible.
fn orbit_emitter_system(time: Res<Time>, mut emitters: Query<(&OrbitingEmitter, &mut Transform)>) {
    let angle = time.elapsed_seconds * 0.8;
    for (_, (_, transform)) in emitters.iter_mut() {
        transform.translation.x = angle.cos() * 1.5;
        transform.translation.z = angle.sin() * 1.5;
    }
}
