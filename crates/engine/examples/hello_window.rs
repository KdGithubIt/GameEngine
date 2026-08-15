use engine::camera::default_camera_transform;
use engine::time::Time;
use engine::{
    App, Assets, Camera3D, GlobalTransform, Handle, Input, KeyCode, Material, Mesh, MouseInput,
    Res, Transform,
};

fn main() {
    let mut app = App::new()
        .with_title("Hello Engine - Input Demo")
        .with_size(1280, 720);

    {
        let world = app.world_mut();

        let triangle_handle: Handle<Mesh> = world
            .get_resource_mut::<Assets<Mesh>>()
            .expect("default mesh asset storage must exist")
            .add(Mesh::triangle());

        let camera = world
            .spawn_with(default_camera_transform())
            .expect("camera entity must spawn");
        world
            .add_component(camera, GlobalTransform::default())
            .expect("camera global transform must be inserted");
        world
            .add_component(camera, Camera3D::default())
            .expect("camera component must be inserted");

        let triangle = world
            .spawn_with(Transform::default())
            .expect("triangle entity must spawn");
        world
            .add_component(triangle, GlobalTransform::default())
            .expect("triangle global transform must be inserted");
        world
            .add_component(triangle, triangle_handle)
            .expect("triangle mesh handle must be inserted");
        world
            .add_component(triangle, Material::default())
            .expect("triangle material must be inserted");
    }

    app.add_system(move_system);
    app.run().expect("event loop must run");
}

/// Moves and rotates transforms from keyboard and mouse input.
fn move_system(
    keyboard: Res<Input<KeyCode>>,
    mouse: Res<MouseInput>,
    time: Res<Time>,
    mut query: engine::Query<&mut Transform>,
) {
    let speed = 1.5 * time.delta_seconds;
    let rotation_speed = 2.0 * time.delta_seconds;

    for (_, transform) in &mut query {
        if keyboard.pressed(KeyCode::KeyW) {
            transform.translation.y += speed;
        }
        if keyboard.pressed(KeyCode::KeyS) {
            transform.translation.y -= speed;
        }
        if keyboard.pressed(KeyCode::KeyA) {
            transform.translation.x -= speed;
        }
        if keyboard.pressed(KeyCode::KeyD) {
            transform.translation.x += speed;
        }

        if keyboard.pressed(KeyCode::KeyQ) {
            transform.rotation =
                engine::glam::Quat::from_rotation_z(rotation_speed) * transform.rotation;
        }
        if keyboard.pressed(KeyCode::KeyE) {
            transform.rotation =
                engine::glam::Quat::from_rotation_z(-rotation_speed) * transform.rotation;
        }

        transform.translation.z -= mouse.scroll * 0.5;
    }
}
