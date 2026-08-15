use engine::camera::default_camera_transform;
use engine::scene_bridge::spawn_from_authoring_scene;
use engine::{
    character_controller_system, player_character_motor_system, player_controller_system, App,
    Camera3D, Collider, GlobalTransform, KinematicCharacterController, MovePlane, PhysicsBody,
    PlayerController, PlayerMarker, Query,
};
use engine_authoring::load_scene_from_json;

/// The scene to load. Embedded at compile time so the example needs no
/// runtime asset path setup.
const SCENE_JSON: &str = include_str!("../assets/scenes/minimal.scene.json");

fn main() {
    // --- Load authoring scene from JSON ---
    let scene = load_scene_from_json(SCENE_JSON).expect("built-in scene must be valid");

    // --- Build and run the App ---
    let mut app = App::new()
        .with_title("Minimal Playable — WASD moves the blue shape")
        .with_size(1280, 720);

    {
        let world = app.world_mut();

        // Convert every authoring entity into a runtime ECS entity.
        spawn_from_authoring_scene(world, &scene).expect("built-in scene must bridge");

        // Add PlayerController to every entity with PlayerMarker.
        let player_entities: Vec<engine_ecs::Entity> = {
            let query = Query::<&PlayerMarker>::new(world);
            query.iter().map(|(entity, _)| entity).collect()
        };
        for entity in player_entities {
            world
                .add_component(
                    entity,
                    PlayerController {
                        move_speed: 1.5,
                        move_plane: MovePlane::Xy,
                        camera_relative: false,
                        ..PlayerController::default()
                    },
                )
                .expect("player controller must insert");
            world
                .add_component(entity, KinematicCharacterController::default())
                .expect("character motor must insert");
            world
                .add_component(entity, Collider::aabb(engine::glam::Vec3::splat(0.5)))
                .expect("player collider must insert");
            world
                .add_component(entity, PhysicsBody::Kinematic)
                .expect("player physics body must insert");
        }

        // Camera looking at the scene from slightly back.
        let camera = world
            .spawn_with(default_camera_transform())
            .expect("camera entity must spawn");
        world
            .add_component(camera, GlobalTransform::default())
            .expect("camera global transform must insert");
        world
            .add_component(camera, Camera3D::default())
            .expect("camera component must insert");
    }

    app.add_system(player_controller_system);
    app.ecs_mut()
        .add_fixed_system(player_character_motor_system);
    app.ecs_mut().add_fixed_system(character_controller_system);
    app.run().expect("event loop must run");
}
