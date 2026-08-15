//! Skinned mesh demo (Phase 48, ADR 0043).
//!
//! Builds a two-bone arm procedurally, spawns its skeleton as entities, and
//! plays a looping bend animation through the GPU skinning pipeline.
//!
//! Run with: `cargo run --example skinned_mesh`

use engine::animation::{AnimChannel, AnimProperty, AnimationClip, Animator, Keyframe};
use engine::camera::default_camera_transform;
use engine::skeleton_asset::{compute_skeleton_identity, BoneDef, BoneId, SkeletonAsset};
use engine::skinning::{spawn_rig, JointPalette, SkinnedMesh};
use engine::{
    animation_system, foot_ik_system, publish_final_rig_pose_system, App, Assets, Camera3D,
    GlobalTransform, Handle, Material, Mesh, SkinningVertexData, Transform, Vertex,
};
use engine_authoring::id::AssetId;
use glam::{Mat4, Quat, Vec3};

const ARM_HEIGHT: f32 = 2.0;
const RINGS: usize = 9;
const HALF_WIDTH: f32 = 0.25;

fn main() {
    let mut app = App::new()
        .with_title("Skinned Mesh - Two Bone Arm")
        .with_size(1280, 720);

    let mut clips = Assets::<AnimationClip>::default();
    let clip = clips.add(bend_clip());
    app.insert_resource(clips);
    app.add_fixed_system(animation_system);
    app.add_fixed_system(foot_ik_system);
    // Pose producers only fill layers; this composes them and publishes the
    // result to the joint transforms the skinning pipeline reads (ADR 0106).
    app.add_fixed_system(publish_final_rig_pose_system);

    {
        let world = app.world_mut();

        let mesh_handle: Handle<Mesh> = world
            .get_resource_mut::<Assets<Mesh>>()
            .expect("default mesh asset storage must exist")
            .add(arm_mesh());

        // The rig owns the joint hierarchy and the playback state that drives
        // it. Two-bone arm: a root at the origin and a tip bone at y=1.
        let spawned_rig = spawn_rig(world, &arm_skeleton()).expect("two-bone rig must spawn");
        let mut animator = Animator::playing(clip);
        animator.set_looping(true);
        let rig = world
            .spawn_with(spawned_rig.skeleton)
            .expect("rig entity must spawn");
        // Animation writes pose layers, not joint transforms (ADR 0106), so a
        // rig without its pose would never move.
        world
            .add_component(rig, spawned_rig.pose)
            .expect("rig pose must be inserted");
        world
            .add_component(rig, animator)
            .expect("rig animator must be inserted");

        // The drawn mesh binds its own skin joints to that rig. A second mesh
        // could bind a different subset of the same bones (ADR 0086).
        let character = world
            .spawn_with(Transform::from_translation(Vec3::new(0.0, -1.0, 0.0)))
            .expect("character entity must spawn");
        world
            .add_component(character, GlobalTransform::default())
            .expect("character global transform must be inserted");
        world
            .add_component(character, mesh_handle)
            .expect("character mesh handle must be inserted");
        world
            .add_component(character, Material::default())
            .expect("character material must be inserted");
        world
            .add_component(
                character,
                SkinnedMesh {
                    rig,
                    joint_bones: vec![BoneId(0), BoneId(1)],
                    inverse_bind_matrices: vec![
                        Mat4::IDENTITY,
                        Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0)),
                    ],
                    skin: None,
                },
            )
            .expect("character skin binding must be inserted");
        world
            .add_component(character, JointPalette::default())
            .expect("character joint palette must be inserted");

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

    app.run().expect("event loop must run");
}

/// The two-bone rig this demo animates: a root at the origin and a tip bone
/// one unit above it.
///
/// Code-authored content builds a `SkeletonAsset` the same way import does,
/// so there is one way to describe a rig (ADR 0086 §1).
fn arm_skeleton() -> SkeletonAsset {
    let bone = |id: u32, name: &str, parent: Option<usize>, translation: Vec3| BoneDef {
        id: BoneId(id),
        name: name.to_owned(),
        parent,
        rest_translation: translation,
        rest_rotation: Quat::IDENTITY,
        rest_scale: Vec3::ONE,
    };
    let bones = vec![
        bone(0, "root_bone", None, Vec3::ZERO),
        bone(1, "tip_bone", Some(0), Vec3::new(0.0, 1.0, 0.0)),
    ];
    SkeletonAsset {
        id: AssetId::generate(),
        name: "arm".to_owned(),
        identity: compute_skeleton_identity(&bones),
        next_bone_id: bones.len() as u32,
        bones,
    }
}

/// A looping clip that bends the tip bone back and forth around Z.
fn bend_clip() -> AnimationClip {
    let bent = Quat::from_rotation_z(std::f32::consts::FRAC_PI_3);
    AnimationClip {
        duration: 2.0,
        morph_channels: Vec::new(),
        events: vec![],
        skeleton: None,
        skeleton_identity: None,
        root_bone: None,
        contacts: Vec::new(),
        channels: vec![AnimChannel {
            property: AnimProperty::Rotation,
            target_bone: Some(BoneId(1)),
            keyframes: vec![
                Keyframe {
                    time: 0.0,
                    value: Quat::IDENTITY.to_array(),
                },
                Keyframe {
                    time: 1.0,
                    value: bent.to_array(),
                },
                Keyframe {
                    time: 2.0,
                    value: Quat::IDENTITY.to_array(),
                },
            ],
        }],
    }
}

/// Builds a vertical square column with ring-based skin weights.
///
/// Vertices near the base follow the root bone; vertices above the elbow
/// blend toward the tip bone, so bending the tip is clearly visible.
fn arm_mesh() -> Mesh {
    let corners = [
        (-HALF_WIDTH, -HALF_WIDTH),
        (HALF_WIDTH, -HALF_WIDTH),
        (HALF_WIDTH, HALF_WIDTH),
        (-HALF_WIDTH, HALF_WIDTH),
    ];
    let face_normals = [
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
    ];

    let mut vertices = Vec::new();
    let mut skinning = Vec::new();
    let mut indices = Vec::new();

    for ring in 0..RINGS - 1 {
        let y0 = ARM_HEIGHT * ring as f32 / (RINGS - 1) as f32;
        let y1 = ARM_HEIGHT * (ring + 1) as f32 / (RINGS - 1) as f32;
        for face in 0..4 {
            let start = corners[face];
            let end = corners[(face + 1) % 4];
            let normal = face_normals[face];
            let base = vertices.len() as u32;
            // Quad order matches Mesh::cube: CCW viewed from outside.
            for (corner, y) in [(end, y1), (end, y0), (start, y0), (start, y1)] {
                vertices.push(Vertex {
                    position: [corner.0, y, corner.1],
                    normal,
                    color: [0.9, 0.7, 0.4],
                    uv: [0.0, 0.0],
                    outline_scale: 1.0,
                    additional_uv: [0.0; 2],
                });
                skinning.push(ring_weights(y));
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    Mesh {
        vertices,
        indices: Some(indices),
        skinning: Some(skinning),
        tangents: None,
        submeshes: Vec::new(),
    }
}

/// Blends between the root bone (0) and tip bone (1) by height.
fn ring_weights(y: f32) -> SkinningVertexData {
    let tip_weight = (y - 0.5).clamp(0.0, 1.0);
    SkinningVertexData {
        joints: [0, 1, 0, 0],
        weights: [1.0 - tip_weight, tip_weight, 0.0, 0.0],
    }
}
